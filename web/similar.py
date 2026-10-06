"""全库找相似：近似重复、细微差分、差分、裁剪、局部编辑，在后台慢慢做，结果成组显示。

1. 签名：每张图解码一次（JPEG 用 draft 快速缩小），存画布比例和 16×16 灰度缩略图（`image_sigs`）。
2. 候选：
   - 比例相同、缩略图几乎一样（相关系数 ≥ GRID_MATCH）的图对 → 近似重复、差分、局部编辑；
   - CCIP 特征很近（距离 < CCIP_NEAR）的图对 → 再加上裁剪（比例不同，缩略图对不上）。
   已是字节相同副本（`duplicates` 同组）的、已比较过的、已排除的图不再比。
3. 比较：候选逐对交给 web/relate.py（约 0.5 秒/对），结果写进 `relations`，所有结论都存，下次不重比。
4. 分组：`Store.refresh_similar()` 把「不是不同的图」的关系连成组（`similar_groups`）。

图片只在本机读，不外发。只有服务进程能读 Photos 图库，所以在服务里跑。
"""
import threading
import time
from collections import OrderedDict

import numpy as np
from PIL import Image, ImageOps, UnidentifiedImageError

from web import relate
from web.features import pair_block_rows

GRID = 16
GRID_MATCH = 0.9       # correlation of the 16×16 grey thumbnails for "maybe the same picture"
RATIO_SAME = 0.03      # |log aspect ratio difference| for the thumbnail comparison to make sense
CCIP_NEAR = 0.006      # CCIP distance of candidates that may be crops (the same drawing, whatever the framing)
MAX_NEIGHBOURS = 12    # per image, so a pile of near-blank images does not explode into all pairs
FLAT = 6.0             # thumbnails with less contrast than this (grey levels) say nothing


def signature(path):
    """(aspect ratio, 16×16 grey thumbnail bytes) of an image as displayed."""
    with Image.open(path) as im:
        im.draft('RGB', (128, 128))
        im = ImageOps.exif_transpose(im)
        if 'A' in im.getbands() or im.mode == 'P':
            rgba = im.convert('RGBA')
            flat = Image.new('RGB', im.size, '#f4f4f2')
            flat.paste(rgba, mask=rgba.getchannel('A'))
            im = flat
        grey = im.convert('L')
        return grey.width / grey.height, grey.resize((GRID, GRID), Image.BOX).tobytes()


def grid_pairs(uuids, ratios, grids, query=None):
    """Pairs whose thumbnails nearly match and whose canvases have the same shape.

    `query`: only look for the neighbours of these uuids (new images) against all of them."""
    X = np.frombuffer(b''.join(grids), np.uint8).reshape(len(grids), GRID * GRID).astype(np.float32)
    X -= X.mean(1, keepdims=True)
    spread = np.sqrt((X * X).mean(1))
    keep = np.nonzero(spread >= FLAT)[0]
    X = X[keep] / (np.linalg.norm(X[keep], axis=1, keepdims=True) + 1e-6)
    logr = np.log(np.asarray(ratios, np.float32)[keep])
    keys = np.asarray(uuids)[keep]
    rows = np.arange(len(keep)) if query is None else np.flatnonzero(np.isin(keys, list(query)))
    pairs = set()
    block = pair_block_rows(len(keep))
    for start in range(0, len(rows), block):
        idx = rows[start:start + block]
        S = X[idx] @ X.T
        S[np.arange(len(idx)), idx] = -1
        for i, r in enumerate(idx):
            # Filter before sorting: only above-threshold neighbours can become pairs.
            valid = np.flatnonzero((S[i] >= GRID_MATCH) & (np.abs(logr[r] - logr) <= RATIO_SAME))
            row = valid
            if len(valid) > MAX_NEIGHBOURS:
                # Float32 dot products can differ by a few ulps across block sizes.
                # Break near-equal ranking scores by UUID, not BLAS rounding noise.
                row = valid[np.lexsort((keys[valid], -np.round(S[i, valid], 6)))[:MAX_NEIGHBOURS]]
            for j in row:
                if S[i, j] < GRID_MATCH:
                    break
                a, b = uuids[keep[r]], uuids[keep[j]]
                pairs.add((a, b) if a < b else (b, a))
    return pairs


def ccip_pairs(features, uuids, query=None):
    known, X = features.matrix(uuids)
    rows = np.arange(len(known)) if query is None else np.flatnonzero(np.isin(np.asarray(known), list(query)))
    pairs = set()
    block = pair_block_rows(len(known))
    for start in range(0, len(rows), block):
        idx = rows[start:start + block]
        S = X[idx] @ X.T
        S[np.arange(len(idx)), idx] = -1
        for i, r in enumerate(idx):
            for j in np.flatnonzero(S[i] > 1 - 2 * CCIP_NEAR):
                a, b = known[r], known[j]
                pairs.add((a, b) if a < b else (b, a))
    return pairs


class SimilarScanner:
    """Background thread: signatures, candidates, pairwise relations, groups. `state` is shown on the duplicates page."""
    def __init__(self, store, features=None, slots=None):
        self.store, self.features = store, features
        self.slots = slots or threading.BoundedSemaphore(2)
        self.state = {'status': 'idle'}
        self.wake = threading.Event()
        self.thread = threading.Thread(target=self.run, name='similar', daemon=True)
        self.cache = OrderedDict()

    def start(self):
        self.wake.set()
        self.thread.start()

    def poke(self):
        self.wake.set()

    def run(self):
        while True:
            self.wake.wait()
            self.wake.clear()
            try:
                self.scan()
            except Exception as exc:
                self.state = {'status': 'error', 'message': f'{type(exc).__name__}: {exc}'[:300]}

    def _library(self):
        with self.store.connect() as c:
            rows = c.execute('''SELECT a.uuid,a.src_path FROM assets a WHERE NOT EXISTS(SELECT 1 FROM excluded x WHERE x.uuid=a.uuid)''').fetchall()
            # Unreadable images (still in iCloud) are read again next scan; opening a missing file is cheap.
            done = {r[0] for r in c.execute('SELECT uuid FROM image_sigs WHERE ok=1')}
        return {r[0]: r[1] for r in rows}, done

    def _signatures(self, paths, done):
        todo = [u for u in paths if u not in done]
        self.state = {'status': 'reading', 'done': 0, 'total': len(todo)}
        batch = []
        for n, u in enumerate(todo, 1):
            try:
                with self.slots:
                    ratio, grid = signature(paths[u])
                batch.append((u, ratio, grid, 1))
            except (OSError, UnidentifiedImageError, Image.DecompressionBombError, ValueError, TypeError):
                batch.append((u, None, None, 0))
            if len(batch) >= 200 or n == len(todo):
                with self.store.connect() as c:
                    c.executemany('INSERT OR REPLACE INTO image_sigs VALUES (?,?,?,?)', batch)
                batch = []
                self.state = {'status': 'reading', 'done': n, 'total': len(todo)}

    def candidates(self, paths):
        """Pairs still to compare. Only images never paired before look for neighbours (against the whole
        library): re-pairing all 48k images found nothing new yet took 67 s every launch (2026-09-26).
        Pairs that could not be read last time are retried from similar_retry."""
        with self.store.connect() as c:
            c.execute('CREATE TABLE IF NOT EXISTS similar_paired(uuid TEXT PRIMARY KEY)')
            c.execute('CREATE TABLE IF NOT EXISTS similar_retry(a TEXT NOT NULL,b TEXT NOT NULL,PRIMARY KEY(a,b))')
            paired = {r[0] for r in c.execute('SELECT uuid FROM similar_paired')}
            sigs = [r for r in c.execute('SELECT uuid,ratio,grid FROM image_sigs WHERE ok=1') if r[0] in paths]
            checked = {(a, b) for a, b in c.execute('SELECT a,b FROM relations')}
            retry = set(map(tuple, c.execute('SELECT a,b FROM similar_retry')))
            copies = {}
            for u, grp in c.execute('SELECT uuid,grp FROM duplicates'):
                copies[u] = grp
        new = [u for u in paths if u not in paired]
        pairs = set(retry)
        if new:
            pairs |= grid_pairs([r[0] for r in sigs], [r[1] for r in sigs], [r[2] for r in sigs], set(new)) if sigs else set()
            if self.features is not None:
                self.features.load()
                pairs |= ccip_pairs(self.features, list(paths), set(new))
        # An image counts as paired only once it could be read; until then it keeps looking for neighbours.
        readable = {r[0] for r in sigs}
        self.new = [u for u in new if u in readable]
        return sorted(p for p in pairs if p not in checked and p[0] in paths and p[1] in paths
                      and not (copies.get(p[0]) and copies.get(p[0]) == copies.get(p[1])))

    def _load(self, uuid, path):
        if uuid in self.cache:
            self.cache.move_to_end(uuid)
            return self.cache[uuid]
        with self.slots:
            image = relate.load(path)
        self.cache[uuid] = image
        if len(self.cache) > 48:
            self.cache.popitem(last=False)
        return image

    def scan(self):
        started = time.time()
        paths, done = self._library()
        self._signatures(paths, done)
        self.state = {'status': 'pairing'}
        todo = self.candidates(paths)
        self.state = {'status': 'comparing', 'done': 0, 'total': len(todo)}
        rows, failed, found = [], [], 0
        for n, (a, b) in enumerate(todo, 1):
            try:
                r = relate.relate(self._load(a, paths[a]), self._load(b, paths[b]))
                rows.append((a, b, r['kind'], r.get('changed'), r.get('note') or ''))
                found += r['kind'] in relate.GROUPING
            except (OSError, UnidentifiedImageError, Image.DecompressionBombError, ValueError):
                failed.append((a, b))    # unreadable now (still in iCloud?): tried again next scan
            if len(rows) >= 25 or n == len(todo):
                with self.store.connect() as c:
                    c.executemany('INSERT OR REPLACE INTO relations(a,b,kind,changed,note) VALUES (?,?,?,?,?)', rows)
                    c.executemany('DELETE FROM similar_retry WHERE a=? AND b=?', [r[:2] for r in rows])
                    c.executemany('INSERT OR IGNORE INTO similar_retry VALUES (?,?)', failed)
                rows, failed = [], []
                self.state = {'status': 'comparing', 'done': n, 'total': len(todo), 'found': found}
            if n % 500 == 0:
                self.store.refresh_similar()
        with self.store.connect() as c:
            c.executemany('INSERT OR IGNORE INTO similar_paired VALUES (?)', [(u,) for u in self.new])
        self.store.refresh_similar()
        self.state = {'status': 'idle', 'compared': len(todo), 'found': found, 'seconds': round(time.time() - started),
                      'at': time.strftime('%Y-%m-%d %H:%M:%S')}
