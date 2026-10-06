"""Local verification (docs/PROVENANCE.md section 8): a candidate artist's public works against the library, picture by picture.

Only public thumbnails come in (Danbooru 180 px previews, Pixiv 150 px masters); nothing goes out. A work counts when
its 16x16 grey signature correlates >= GRID_MATCH with a library picture of the same canvas shape and the 24x24 colour
thumbnails differ by less than COLOUR_DIFF (out of 255): the same drawing, not a similar one. The thumbnails are
thrown away; only their signatures stay in `ref_sigs`, so a rerun never downloads them again.
"""
import io
import json
import re
import sqlite3
import time
from collections import deque
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

import numpy as np
from PIL import Image

from provenance import db, evidence
from provenance.net import BudgetExceeded, Stopped
from web.similar import GRID, signature

GRID_MATCH = 0.9
RATIO_SAME = 0.03
COLOUR_DIFF = 12.0
FLAT = 6.0
DANBOORU_POSTS = 200     # latest posts per artist; older ones on a later pass (lookups.cursor)
PIXIV_WORKS = 300        # latest works per account
PIXIV_PAGES = 3          # first pages of a multi-page work
NEIGHBOURS = 20          # works on each side of one known to be in the library: saves come in runs down an artist's page
NEIGHBOUR_EXTRA = 300    # at most this many works beyond the newest PIXIV_WORKS per artist
PROBE = 30               # a weak lead (OCR or related users only, nothing known in the library): works tried per site first
WEAK = ('ocr', 'pixiv_related')
REFRESH_DAYS = 30
# Which artists are verified at all (2026-09-29, the user's choice): a work list is hundreds of thumbnails, and most
# artists have one picture in the library, usually found already. Automatically: artists with MIN_PICTURES or more
# pictures here, artists whose Pixiv save runs still hold OPEN_IN_RUNS or more pictures without an artist (likely theirs),
# and popular artists (POPULAR_POSTS Danbooru posts or more). The rest wait until they qualify.
MIN_PICTURES = 3
OPEN_IN_RUNS = 2
POPULAR_POSTS = 1000
COUNT = 'danbooru_count'   # lookups backend: an artist tag's Danbooru post count (cursor), asked again after REFRESH_DAYS        # a verified artist's works lists are asked again after this long (their new works)
LOCAL = 'ref_compare'    # lookups backend: library pictures already compared against every kept signature
# Strongest leads first: one run's traffic cap covers about 60 artists (~5 MB each) out of thousands of candidates.
# Pixel hash: a Danbooru post already matched a library picture; OCR: the signature was read on one; then follows, then
# related users. Within a source, artists already tied to more library pictures go first.
SOURCE_RANK = ('pixel_hash', 'iqdb', 'ocr', 'x_following', 'pixiv_following', 'pixiv_related')
PRIORITY = f"""SELECT s.artist_id FROM seeds s
    LEFT JOIN (SELECT artist_id, count(*) n FROM image_artists GROUP BY artist_id) i ON i.artist_id = s.artist_id
    GROUP BY s.artist_id
    ORDER BY min(CASE s.source {' '.join(f"WHEN '{x}' THEN {i}" for i, x in enumerate(SOURCE_RANK))} ELSE {len(SOURCE_RANK)} END),
             coalesce(max(i.n), 0) DESC, min(s.added_at), s.artist_id"""


def _normalise(grids):
    X = np.frombuffer(b''.join(grids), np.uint8).reshape(len(grids), GRID * GRID).astype(np.float32)
    X -= X.mean(1, keepdims=True)
    spread = np.sqrt((X * X).mean(1))
    return X / (np.linalg.norm(X, axis=1, keepdims=True) + 1e-6), spread


class Library:
    """The library's pictures as signatures: the core's `image_sigs` (read-only), computed here for the ones it lacks."""
    def __init__(self, library):
        c = sqlite3.connect(Path(library.db).resolve().as_uri() + '?mode=ro', uri=True, timeout=30)
        try:
            paths = dict(c.execute('SELECT uuid,src_path FROM assets'))
            sigs = {u: (r, g) for u, r, g in c.execute('SELECT uuid,ratio,grid FROM image_sigs WHERE ok=1') if u in paths}
        finally:
            c.close()
        for u in paths.keys() - sigs.keys():
            try:
                sigs[u] = signature(paths[u])
            except (OSError, ValueError, TypeError):
                pass
        self.paths = paths
        self.ids = np.array(list(sigs))
        self.logr = np.log(np.array([sigs[u][0] for u in self.ids], np.float32)) if sigs else np.zeros(0, np.float32)
        self.X, self.spread = _normalise([sigs[u][1] for u in self.ids]) if sigs else (np.zeros((0, GRID * GRID), np.float32), np.zeros(0))
        self._colour = {}

    def colour(self, asset):
        if asset not in self._colour:
            self._colour[asset] = _colour(self.paths[asset])
        return self._colour[asset]

    def match(self, thumb, ratio=None):
        """Library pictures that are this thumbnail's drawing."""
        return self.match_sig(*_sig(thumb, ratio))

    def match_sig(self, ratio, grid, colour):
        """The same from a kept signature (`ref_sigs`): no download needed to compare against new library pictures."""
        x, spread = _normalise([grid])
        if spread[0] < FLAT or not len(self.ids):
            return []
        lr = np.log(ratio)
        s = self.X @ x[0]
        hits = []
        colour = np.frombuffer(colour, np.uint8).reshape(24, 24, 3).astype(np.float32)
        for k in np.flatnonzero((s >= GRID_MATCH) & (np.abs(self.logr - lr) <= RATIO_SAME) & (self.spread >= FLAT)):
            asset = str(self.ids[k])
            try:
                diff = float(np.abs(colour - self.colour(asset)).mean())
            except (OSError, ValueError):
                continue
            if diff < COLOUR_DIFF:
                hits.append((asset, float(s[k]), diff))
        return hits


def _colour(file, size=24):
    with Image.open(file) as im:
        im.draft('RGB', (96, 96))
        return np.asarray(im.convert('RGB').resize((size, size), Image.BOX), np.float32)


def _links(c, artist):
    return {(r['system'], r['key']) for r in c.execute('SELECT system,key FROM artist_links WHERE artist_id=?', (artist,))}


def _record(c, artist, asset, site, post_id, url, score):
    c.execute("INSERT OR IGNORE INTO image_sources(asset,site,post_id,url,artist_id,method,similarity) VALUES (?,?,?,?,?, 'gallery_match', ?)",
              (asset, site, post_id, url, artist, round(score * 100, 1)))
    c.execute("""INSERT INTO image_artists(asset,artist_id,method,score,evidence,status) VALUES (?,?, 'gallery_match', ?, ?, 'auto')
                 ON CONFLICT(asset,artist_id) DO NOTHING""", (asset, artist, score, json.dumps({site: post_id})))


PIXIV_SOURCE = re.compile(r'pixiv\.net/(?:en/)?artworks/(\d+)|illust_id=(\d+)|/(\d+)_p(\d+)')


def _sig(thumb, ratio=None):
    """What a thumbnail is kept as: canvas ratio, 16x16 grey grid, 24x24 colour (uint8). The image itself is dropped."""
    r, grid = signature(io.BytesIO(thumb))
    return ratio or r, grid, _colour(io.BytesIO(thumb)).round().astype(np.uint8).tobytes()


class Progress:
    """Two levels for the settings page: artists done out of all, and the current artist's works (downloaded, signed
    before, or skipped alike, so an artist whose works are all known still moves). Text and numbers at most every
    EVERY seconds; `found` counts matches as they come."""
    EVERY = 5

    def __init__(self, log, net, artists):
        self.log, self.net, self.artists = log, net, artists
        self.report = getattr(log, 'progress', None) or (lambda **numbers: None)
        self.done = self.found = self.works = self.planned = self.fetched = 0
        self.last = 0.0

    def start(self):                  # a new artist
        self.works = self.planned = 0
        self.show(force=True)

    def plan(self, n):                # this many more works for the current artist
        self.planned += n

    def work(self, found=0):          # one work handled
        self.works += 1
        self.found += found
        self.show()

    def finish(self):
        self.done += 1
        self.show(force=True)

    def show(self, force=False):
        if not force and time.monotonic() - self.last < self.EVERY:
            return
        self.last = time.monotonic()
        mb = self.net.used / 2 ** 20
        self.log(f'本地比对：第 {min(self.done + 1, self.artists)}/{self.artists} 位画师，作品 {self.works}/{self.planned}，'
                 f'已下载小图 {self.fetched} 张，比对上 {self.found} 张，流量 {mb:.0f} MB')
        self.report(label='画师', done=self.done, total=self.artists, found=self.found, mb=round(mb, 1),
                    sub_label='作品', sub_done=self.works, sub_total=self.planned)


class _Quiet(Progress):
    def __init__(self):
        pass

    def plan(self, n):
        pass

    def work(self, found=0):
        pass

    fetched = 0


def _reference(c, net, artist, site, post_id, page, url, ratio, progress, extra=None):
    """One public work's signature: from `ref_sigs`, else downloaded once and kept (committed at once: a run that stops
    mid-artist keeps what it paid for). None when it cannot be had."""
    row = c.execute('SELECT ratio,grid,colour FROM ref_sigs WHERE site=? AND post_id=? AND page=? AND colour IS NOT NULL',
                    (site, post_id, page)).fetchone()
    if row:
        return row['ratio'], row['grid'], row['colour']
    thumb = net.get(url, raw=True, extra=extra)
    progress.fetched += 1
    if not thumb:
        return None
    try:
        sig = _sig(thumb, ratio)
    except (OSError, ValueError):
        return None
    with c:
        c.execute('INSERT OR REPLACE INTO ref_sigs(site,post_id,page,artist_id,ratio,grid,colour,url) VALUES (?,?,?,?,?,?,?,?)',
                  (site, post_id, page, artist, *sig, url))
    return sig


def _matched(c, lib, artist, sig, site, post_id, url):
    found = 0
    with c:
        for asset, score, _ in lib.match_sig(*sig):
            _record(c, artist, asset, site, post_id, url, score)
            found += 1
    return found


def danbooru_posts(c, net, artist, tag):
    """The artist's latest Danbooru posts; also learns the Pixiv account the artist page registers."""
    posts = net.get('https://danbooru.donmai.us/posts.json',
                    {'tags': tag, 'limit': DANBOORU_POSTS, 'only': 'id,image_width,image_height,preview_file_url,source'}) or []
    page = net.get('https://danbooru.donmai.us/artists.json', {'search[name]': tag, 'only': 'name,urls'}) or []
    with c:
        for u in (page[0].get('urls', []) if page else []):
            if (m := re.search(r'pixiv\.net/(?:en/)?(?:users/|member\.php\?id=)(\d+)', u.get('url', ''))):
                c.execute("INSERT OR IGNORE INTO artist_links(system,key,artist_id,url,source) VALUES ('pixiv',?,?,?, 'danbooru_artist')",
                          (m.group(1), artist, f'https://www.pixiv.net/users/{m.group(1)}'))
    return posts


def _pixiv_source(post):
    """(work id, page) of the Pixiv work a Danbooru post says it came from, or None."""
    if (m := PIXIV_SOURCE.search(post.get('source') or '')):
        return m.group(1) or m.group(2) or m.group(3), int(m.group(4) or 0)
    return None


def danbooru(c, lib, net, artist, posts, known, covered, progress=None, probe=None):
    """Match the posts' previews. Skipped: posts the pixel hash step already tied to a library picture, and posts whose
    Pixiv source this run already looked at on Pixiv (the same drawing twice). With `probe`, a weak lead: the rest of
    the posts only when one of the first `probe` matched."""
    found, progress = 0, progress or _Quiet()
    for n, p in enumerate(posts):
        if probe and n == probe and not found:
            for _ in posts[n:]:
                progress.work()
            break
        post_id, here = str(p['id']), 0
        skip = (not p.get('preview_file_url') or not p.get('image_width') or post_id in known
                or ((src := _pixiv_source(p)) and src[0] in covered and src[1] < PIXIV_PAGES))
        if not skip:
            sig = _reference(c, net, artist, 'danbooru', post_id, 0, p['preview_file_url'], p['image_width'] / p['image_height'], progress)
            if sig:
                here = _matched(c, lib, artist, sig, 'danbooru', post_id, f'https://danbooru.donmai.us/posts/{post_id}')
        found += here
        progress.work(here)
    return found


def _prepare(c, lib, artist, batch, known, progress):
    """Up to 48 works: those with kept signatures matched now, known ones skipped. Returns ({work: matches}, to list)."""
    found, todo = {}, []
    for w in batch:
        rows = c.execute("SELECT ratio,grid,colour FROM ref_sigs WHERE site='pixiv' AND post_id=? AND colour IS NOT NULL", (w,)).fetchall()
        if rows:
            found[w] = sum(_matched(c, lib, artist, tuple(r), 'pixiv', w, f'https://www.pixiv.net/artworks/{w}') for r in rows)
            progress.work(found[w])
        elif w in known:
            progress.work()
        else:
            todo.append(w)
    return found, todo


def _listing(net, uid, todo):
    """The works' thumbnail paths and page counts (one www.pixiv.net request; run ahead in a thread, no database)."""
    q = [('ids[]', w) for w in todo] + [('work_category', 'illustManga'), ('is_first_page', '0')]
    works = ((net.get(f'https://www.pixiv.net/ajax/user/{uid}/profile/illusts', q) or {}).get('body') or {}).get('works') or {}
    return list(works.values() if isinstance(works, dict) else works)


def _download(c, lib, net, artist, todo, listed, progress):
    found = {}
    for w in listed:
        m, here = re.search(r'/img/(\d{4}/\d\d/\d\d/\d\d/\d\d/\d\d)/', w.get('url', '')), 0
        for page in range(min(int(w.get('pageCount', 1)), PIXIV_PAGES) if m else 0):
            url = f"https://i.pximg.net/c/150x150_80/img-master/img/{m.group(1)}/{w['id']}_p{page}_master1200.jpg"
            sig = _reference(c, net, artist, 'pixiv', str(w['id']), page, url, None, progress, {'Referer': 'https://www.pixiv.net/'})
            if sig:
                here += _matched(c, lib, artist, sig, 'pixiv', str(w['id']), f"https://www.pixiv.net/artworks/{w['id']}")
        found[str(w['id'])] = here
        progress.work(here)
    for _ in range(len(todo) - len(listed)):   # asked for but not listed (deleted, hidden)
        progress.work()
    return found


def pixiv(c, lib, net, artist, uid, known=frozenset(), progress=None, covered=None, probe=None):
    """The account's works (R18 only with the bound login): first around the ones already known to be in the library,
    then the newest PIXIV_WORKS, widening around every new match. The person saves an artist's page in runs, often old
    works far below the newest (2026-09-29), so a match says its neighbours are likely saved too.
    Works already signed in `ref_sigs` need no request; known ones are skipped. `covered` collects the ids looked at.
    With `probe` (a weak lead), only the newest `probe` works unless one of them matches.
    The next batch's listing (www.pixiv.net, 2 s apart) is fetched while this batch's thumbnails (i.pximg.net) come in."""
    found, progress = 0, progress or _Quiet()
    body = (net.get(f'https://www.pixiv.net/ajax/user/{uid}/profile/all') or {}).get('body') or {}
    everything = sorted([*(body.get('illusts') or {}), *(body.get('manga') or {})], key=int, reverse=True)
    pos = {w: i for i, w in enumerate(everything)}
    newest = set(everything[:PIXIV_WORKS])
    plan, seen, extra = [], set(), NEIGHBOUR_EXTRA

    def add(works):
        nonlocal extra
        for w in works:
            if w in seen or (w not in newest and extra <= 0):
                continue
            extra -= w not in newest
            seen.add(w)
            plan.append(w)
            progress.plan(1)

    def around(w):
        i = pos[w]
        return everything[max(0, i - NEIGHBOURS):i + NEIGHBOURS + 1]

    def matched(hits):
        nonlocal found
        for w, here in hits.items():
            found += here
            if here and w in pos:
                add(around(w))

    for (w,) in c.execute("SELECT DISTINCT post_id FROM image_sources WHERE site='pixiv' AND artist_id=?", (artist,)).fetchall():
        if w in pos:
            add(around(w))
    add(everything[:probe or PIXIV_WORKS])
    i, ahead, widened = 0, deque(), not probe
    with ThreadPoolExecutor(max_workers=1) as pool:
        while True:
            while len(ahead) < 2 and i < len(plan):     # keep the next listing in flight
                batch, i = plan[i:i + 48], i + 48
                cached, todo = _prepare(c, lib, artist, batch, known, progress)
                matched(cached)
                if todo:
                    ahead.append((todo, pool.submit(_listing, net, uid, todo)))
            if not ahead:
                if not widened and found:                # a weak lead that matched: the full newest list after all
                    widened = True
                    add(everything[:PIXIV_WORKS])
                    continue
                break
            todo, listing = ahead.popleft()
            matched(_download(c, lib, net, artist, todo, listing.result(), progress))
    if covered is not None:
        covered.update(seen)
    return found


def local_run(library, net=None, limit=None, log=print, chunk=4096):
    """New library pictures against every kept signature (`ref_sigs`), no network: after a sync, instead of asking every
    artist's works again. Each (picture, signature) pair is compared once: a signature meets the whole library when it
    is downloaded, a picture meets every signature here, then is marked in lookups (LOCAL)."""
    counts = {'pictures': 0, 'matches': 0, 'stopped': ''}
    lib = Library(library)
    c = db.connect(library)
    try:
        seen = {r[0] for r in c.execute('SELECT target FROM lookups WHERE backend=?', (LOCAL,))}
        new = np.array([i for i, u in enumerate(lib.ids) if str(u) not in seen], dtype=np.int64)[:limit]
        refs = c.execute('SELECT count(*) FROM ref_sigs WHERE colour IS NOT NULL').fetchone()[0]
        if not len(new):
            return counts
        log(f'本地重比新图：{len(new)} 张新图 × {refs} 个已存签名')
        X, logr, spread = lib.X[new], lib.logr[new], lib.spread[new]
        report = getattr(log, 'progress', None) or (lambda **numbers: None)
        rows, done = c.execute('SELECT site,post_id,artist_id,ratio,grid,colour FROM ref_sigs WHERE colour IS NOT NULL'), 0
        while batch := rows.fetchmany(chunk):
            R, rspread = _normalise([r['grid'] for r in batch])
            rr = np.log(np.array([r['ratio'] for r in batch], np.float32))
            for a in range(0, len(new), chunk):
                S = R @ X[a:a + chunk].T
                ok = ((S >= GRID_MATCH) & (np.abs(rr[:, None] - logr[None, a:a + chunk]) <= RATIO_SAME)
                      & (rspread[:, None] >= FLAT) & (spread[None, a:a + chunk] >= FLAT))
                for i, j in zip(*np.nonzero(ok)):
                    r, asset = batch[i], str(lib.ids[new[a + j]])
                    try:
                        diff = float(np.abs(np.frombuffer(r['colour'], np.uint8).reshape(24, 24, 3).astype(np.float32) - lib.colour(asset)).mean())
                    except (OSError, ValueError):
                        continue
                    if diff < COLOUR_DIFF and r['artist_id']:
                        url = (f"https://danbooru.donmai.us/posts/{r['post_id']}" if r['site'] == 'danbooru'
                               else f"https://www.pixiv.net/artworks/{r['post_id']}")
                        before = c.total_changes
                        with c:
                            _record(c, r['artist_id'], asset, r['site'], r['post_id'], url, float(S[i, j]))
                        counts['matches'] += c.total_changes > before   # only what was not recorded already
            done += len(batch)
            log(f"本地重比新图：签名 {done}/{refs}，比对上 {counts['matches']} 张")
            report(label='签名', done=done, total=refs, found=counts['matches'])
        with c:
            c.executemany("""INSERT INTO lookups(target,backend,attempts,last_at,last_result) VALUES (?,?,1,datetime('now'),'miss')
                             ON CONFLICT(target,backend) DO NOTHING""", [(str(lib.ids[k]), LOCAL) for k in new])
        counts['pictures'] = len(new)
    finally:
        c.close()
    return counts


def _worth(c, library):
    """{artist: (library pictures, pictures without an artist in their Pixiv save runs)} for every seeded artist."""
    from provenance import assets
    from provenance.sources.iqdb import _save_runs
    pictures = dict(c.execute("""SELECT artist_id, count(DISTINCT asset) FROM image_artists
                                 WHERE status IN ('auto','confirmed') GROUP BY artist_id"""))
    owners = {}
    for asset, artist in c.execute("SELECT asset, artist_id FROM image_artists WHERE status IN ('auto','confirmed')"):
        owners.setdefault(asset, set()).add(artist)
    with assets.open_source(library) as s:
        added = dict(s.execute('SELECT uuid, added_at FROM assets'))
    open_ = {}
    for run in _save_runs([dict(p, added_at=added.get(p['asset'])) for p in assets.pictures(library, ('pixiv',))]):
        unresolved = sum(p['asset'] not in owners for p in run)
        for artist in {a for p in run for a in owners.get(p['asset'], ())}:
            open_[artist] = open_.get(artist, 0) + unresolved
    return {a: (pictures.get(a, 0), open_.get(a, 0)) for (a,) in c.execute('SELECT DISTINCT artist_id FROM seeds')}


def popularity(c, net, artists, log=print):
    """Danbooru post counts of these artists' tags, 50 names per request, kept for REFRESH_DAYS. {artist: posts}."""
    tags = {}
    for artist, key in c.execute("SELECT artist_id, key FROM artist_links WHERE system='danbooru'"):
        if artist in artists:
            tags.setdefault(key, set()).add(artist)
    ask = [t for t in tags if evidence.due(c, 'tag:' + t, COUNT)]
    if ask:
        log(f'本地比对：查 {len(ask)} 位画师在 Danbooru 的帖子数（判断是否热门）')
    for i in range(0, len(ask), 50):
        batch = ask[i:i + 50]
        answer = net.get('https://danbooru.donmai.us/tags.json',
                         {'search[name_comma]': ','.join(batch), 'only': 'name,post_count', 'limit': 100}) or []
        counts = {t['name']: int(t.get('post_count') or 0) for t in answer if isinstance(t, dict)}
        with c:
            for t in batch:
                c.execute("""INSERT INTO lookups(target,backend,attempts,last_at,last_result,next_due_at,cursor)
                             VALUES (?,?,1,datetime('now'),'hit',datetime('now',?),?) ON CONFLICT(target,backend) DO UPDATE SET
                             attempts=attempts+1,last_at=excluded.last_at,next_due_at=excluded.next_due_at,cursor=excluded.cursor""",
                          ('tag:' + t, COUNT, f'+{REFRESH_DAYS} days', str(counts.get(t, 0))))
    out = {}
    for target, cursor in c.execute('SELECT target, cursor FROM lookups WHERE backend=?', (COUNT,)):
        for artist in tags.get(target[4:], ()):
            out[artist] = max(out.get(artist, 0), int(cursor or 0))
    return out


def chosen(c, library, net, candidates, log=print):
    """The candidates worth verifying, best first: most pictures here, then open save runs, then Danbooru posts."""
    worth = _worth(c, library)
    strong = {a for a in candidates if worth.get(a, (0, 0))[0] >= MIN_PICTURES or worth.get(a, (0, 0))[1] >= OPEN_IN_RUNS}
    try:
        posts = popularity(c, net, set(candidates) - strong, log)
    except (Stopped, BudgetExceeded, ConnectionError) as exc:
        log(f'本地比对：没能查画师热度（{exc}），只比对图多的画师')
        posts = {}
    keep = [a for a in candidates if a in strong or posts.get(a, 0) >= POPULAR_POSTS]
    keep.sort(key=lambda a: (-worth.get(a, (0, 0))[0], -worth.get(a, (0, 0))[1], -posts.get(a, 0)))
    return keep, len(candidates) - len(keep)


def run(library, net, limit=None, log=print, artists=None):
    """Verify every seeded artist not verified yet (or `artists`). Stops cleanly on budget or refusal."""
    counts = {'artists': 0, 'matched_artists': 0, 'matches': 0, 'cold': 0, 'stopped': ''}
    lib = Library(library)
    c = db.connect(library)
    try:
        # New library pictures no longer undo this: local_run compares them with the kept signatures, offline.
        done = {r['target'][7:] for r in c.execute("""SELECT target FROM lookups WHERE backend='verify' AND last_result IN ('hit','miss')
                                                       AND last_at >= datetime('now', ?)""", (f'-{REFRESH_DAYS} days',))}
        # Named artists (「找 TA 的其他图」) are verified again even when done: kept signatures make it cheap.
        todo = list(artists) if artists else [r['artist_id'] for r in c.execute(PRIORITY) if r['artist_id'] not in done]
        if not artists:
            todo, counts['cold'] = chosen(c, library, net, todo, log)
        todo = todo[:limit]
        if todo:
            log(f"本地比对：{len(todo)} 位画师待比对（图多或热门；另有 {counts['cold']} 位冷门画师不自动查）")
        known = {site: {r[0] for r in c.execute('SELECT DISTINCT post_id FROM image_sources WHERE site=?', (site,))}
                 for site in ('danbooru', 'pixiv')}
        progress = Progress(log, net, len(todo))
        weak = {r[0] for r in c.execute(f"""SELECT s.artist_id FROM seeds s GROUP BY s.artist_id
                    HAVING sum(s.source NOT IN ({','.join('?' * len(WEAK))})) = 0
                    AND NOT EXISTS (SELECT 1 FROM image_sources x WHERE x.artist_id = s.artist_id)""", WEAK)}
        for artist in todo:
            found, skipped, covered = 0, False, set()
            probe = PROBE if artist in weak else None
            progress.start()
            try:
                posts = {key: danbooru_posts(c, net, artist, key) for system, key in sorted(_links(c, artist)) if system == 'danbooru'}
                progress.plan(sum(len(items) for items in posts.values()))
                for system, key in sorted(_links(c, artist)):   # Danbooru may have added a Pixiv account: Pixiv first
                    if system == 'pixiv':
                        try:
                            found += pixiv(c, lib, net, artist, key, known['pixiv'], progress, covered, probe)
                        except Stopped as exc:          # Pixiv refused: go on with Danbooru alone, retry Pixiv next run
                            skipped, counts['stopped'] = True, str(exc)
                for key, items in posts.items():
                    found += danbooru(c, lib, net, artist, items, known['danbooru'], covered, progress, probe)
                # Pixiv refused: matches found so far stay, but the artist is not done; Pixiv (R18 included) comes next run.
                if not skipped:
                    with c:
                        c.execute("""INSERT INTO lookups(target,backend,attempts,last_at,last_result) VALUES (?, 'verify', 1, datetime('now'), ?)
                                     ON CONFLICT(target,backend) DO UPDATE SET attempts=attempts+1,last_at=excluded.last_at,last_result=excluded.last_result""",
                                  ('artist:' + artist, 'hit' if found else 'miss'))
                        # 'probe': a weak lead looked at only in its newest works; a later pass may look at all of them
                        c.execute("UPDATE lookups SET cursor=? WHERE target=? AND backend='verify'",
                                  ('probe' if probe and not found else None, 'artist:' + artist))
            except (Stopped, BudgetExceeded) as exc:
                counts['stopped'] = str(exc)
                break
            except ConnectionError:
                continue
            counts['artists'] += 1
            counts['matched_artists'] += bool(found)
            counts['matches'] += found
            progress.finish()
    finally:
        c.close()
    return counts
