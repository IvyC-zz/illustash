"""图片关系判断：选中的几张图两两比较，判断是同一张图、局部（裁剪）、差分、局部编辑，还是无关。

只用 numpy + Pillow，在本机算，不用模型：
- 画布比例相同的两张图：缩到同一尺寸后逐像素比较。差异先做 3×3 模糊，去掉缩放和压缩噪声，再按 16px 的格子统计
  哪些地方变了，相邻的变化格连成区域。
    · 几乎没有变化 → 重复（同一张图，尺寸或压缩不同）；
    · 整体偏差大，但每个通道做一次线性校正后几乎一致 → 局部编辑（调色）；
    · 变化集中在局部 → 差分；如果变化区域比原图明显更平滑（打码、模糊），或者只在边缘（加字、水印、边框）→ 局部编辑；
    · 大面积都不同 → 构图相似的不同图。
- 比例不同（或同比例但对不上）：在多个缩放比例下，用 FFT 归一化互相关在大图里找小图，
  找到了就是局部重复（裁剪），再对重叠部分做同样的逐像素比较，看裁剪后有没有改动。
阈值用 tests/relate_images.py 的人造图和真实库里的副本、差分校准过（见 docs/WEB_V2.md）。
"""
from itertools import combinations

import numpy as np
from PIL import Image, ImageOps

LOAD = 1024          # decode at most this long side
MAX_IMAGES = 12      # at most 66 pairs per request
SIDE = 512           # common canvas for pixel comparison
CELL = 16            # changed-area grid, in canvas pixels
SEARCH = 256         # long side of the container during the crop search
SAME_RATIO = 0.02    # |log(aspect a / aspect b)| below this: the same canvas
PIXEL = 28           # a pixel differs when its blurred colour differs by more than this (0-255)
CELL_SHARE = 0.10    # a cell changed when this share of its pixels differ
DUPLICATE = 0.004    # changed share of the canvas below which two images are the same picture
DIFFERENT = 0.45     # above this they are different pictures
MATCH = 0.86         # normalised cross-correlation for "the smaller image is a part of the larger"
BORDER = 0.07        # edits confined to this margin read as added text / watermark / frame
REDRAWN = 0.75       # structure correlation of mostly-changed images that are still one drawing (repaint, recolour)
SIMILAR = 0.5        # ... and of images that only share a composition

SUBTLE = 0.02        # a variant changing less than this share of the picture (a mouth, glasses, a signature): 细微差分
# Relations that make two images one look-alike group (web/similar.py, Store.similar_groups).
GROUPING = ('identical', 'duplicate', 'subtle', 'crop', 'variant', 'edit')
RANK = {'identical': 0, 'duplicate': 1, 'subtle': 2, 'crop': 3, 'variant': 4, 'edit': 5, 'similar': 6, 'unrelated': 7}
LABELS = {'identical': '完全相同', 'duplicate': '重复', 'subtle': '细微差分', 'crop': '局部重复', 'variant': '差分', 'edit': '局部编辑',
          'similar': '构图相似', 'unrelated': '不同的图'}


def load(path):
    """RGB float array (0-255) of an image, long side ≤ LOAD, as it is displayed (EXIF rotation, alpha on light grey)."""
    with Image.open(path) as im:
        im.draft('RGB', (LOAD, LOAD))
        im = ImageOps.exif_transpose(im)
        size = im.size
        im.thumbnail((LOAD, LOAD))
        if 'A' in im.getbands() or im.mode == 'P':
            rgba = im.convert('RGBA')
            flat = Image.new('RGB', im.size, '#f4f4f2')
            flat.paste(rgba, mask=rgba.getchannel('A'))
            im = flat
        return np.asarray(im.convert('RGB'), np.float32), size


def _resize(x, size):
    """Resize an RGB/grey float array to (width, height)."""
    if x.ndim == 2:
        return np.asarray(Image.fromarray(x.astype(np.float32), 'F').resize(size, Image.BILINEAR), np.float32)
    return np.stack([_resize(x[..., k], size) for k in range(x.shape[2])], -1)


def _blur(x):
    """3×3 box blur (edges replicated)."""
    p = np.pad(x, [(1, 1), (1, 1)] + [(0, 0)] * (x.ndim - 2), mode='edge')
    h, w = x.shape[:2]
    return sum(p[i:i + h, j:j + w] for i in range(3) for j in range(3)) / 9.0


def _difference(fa, fb, reach=1):
    """Per-pixel colour difference that forgives a shift of up to `reach` pixels (resampling, sub-pixel offsets):
    each pixel of a against the closest of b's pixels around it, and the other way round."""
    h, w = fa.shape[:2]
    pad = lambda x: np.pad(x, [(reach, reach), (reach, reach)] + [(0, 0)] * (x.ndim - 2), mode='edge')
    pa, pb = pad(fa), pad(fb)
    shifts = [(dy, dx) for dy in range(2 * reach + 1) for dx in range(2 * reach + 1)]
    ab = np.min([np.abs(fa - pb[dy:dy + h, dx:dx + w]).max(-1) for dy, dx in shifts], 0)
    ba = np.min([np.abs(fb - pa[dy:dy + h, dx:dx + w]).max(-1) for dy, dx in shifts], 0)
    return np.maximum(ab, ba)


def _grey(x):
    return x @ np.array([0.299, 0.587, 0.114], np.float32)


def _canvas(ratio):
    return (SIDE, max(8, round(SIDE / ratio))) if ratio >= 1 else (max(8, round(SIDE * ratio)), SIDE)


def _regions(cells):
    """Connected changed cells → boxes (x, y, w, h) in cell units, largest first."""
    seen, boxes = np.zeros_like(cells), []
    for y0, x0 in zip(*np.nonzero(cells)):
        if seen[y0, x0]:
            continue
        stack, ys, xs = [(y0, x0)], [], []
        seen[y0, x0] = True
        while stack:
            y, x = stack.pop()
            ys.append(y)
            xs.append(x)
            for dy, dx in ((1, 0), (-1, 0), (0, 1), (0, -1)):
                v, u = y + dy, x + dx
                if 0 <= v < cells.shape[0] and 0 <= u < cells.shape[1] and cells[v, u] and not seen[v, u]:
                    seen[v, u] = True
                    stack.append((v, u))
        boxes.append((min(xs), min(ys), max(xs) - min(xs) + 1, max(ys) - min(ys) + 1, len(ys)))
    return sorted(boxes, key=lambda b: -b[4])


def _covered(x, box, changed):
    """How a region was hidden, if it plainly was: 'fill' (one flat colour: a block, a bar, a sticker's face)
    or 'mosaic' (a grid of flat blocks); else None.

    Anything subtler (a sticker with a drawing, a painted-over patch) cannot be told from a redrawn detail by pixels
    alone: glasses added and glasses painted out look the same, so those stay variants.
    """
    g = _grey(x[box])
    if min(g.shape) < 12:
        return None
    # Fill: the pixels that changed are one flat colour on this side (judged on them, not on the whole box).
    inner = changed[box][1:-1, 1:-1]
    lap = np.abs(4 * g[1:-1, 1:-1] - g[:-2, 1:-1] - g[2:, 1:-1] - g[1:-1, :-2] - g[1:-1, 2:])
    if inner.sum() > 200 and (lap[inner] < 1.5).mean() > 0.9 and x[box][1:-1, 1:-1][inner].std(0).mean() < 6:
        return 'fill'
    # Mosaic: its block edges run straight through the whole region, several rows and columns of them.
    dx, dy = np.abs(np.diff(g, axis=1)) > 6, np.abs(np.diff(g, axis=0)) > 6
    cols, rows = dx.mean(0) > 0.6, dy.mean(1) > 0.6
    # Resampling spreads each block edge over two lines: count the neighbours of a straight edge as part of it.
    near = lambda m: m | np.r_[m[1:], False] | np.r_[False, m[:-1]]
    if (cols.sum() >= 3 and rows.sum() >= 3 and dx[:, near(cols)].sum() > 0.6 * dx.sum()
            and dy[near(rows)].sum() > 0.6 * dy.sum()):
        return 'mosaic'
    return None


def compare(a, b, fine=None):
    """Two arrays showing the same canvas (already the same size): what differs, and how.

    `fine`: the same pair at full decoded resolution, for the mosaic check (resampling smears the block edges).
    Returns kind, the changed share, boxes normalised to the canvas, and for an edit which side was edited ('a'/'b').
    """
    h, w = a.shape[:2]
    fa, fb = _blur(a), _blur(b)
    raw = _difference(fa, fb)
    # A global tone change: per channel, one curve (any monotone-ish mapping of levels) turns a into b almost everywhere.
    fitted = np.empty_like(fa)
    for k in range(3):
        levels = np.clip(fa[..., k], 0, 255).astype(np.int32) // 4
        count = np.bincount(levels.ravel(), minlength=64)
        mean = np.bincount(levels.ravel(), fb[..., k].ravel(), minlength=64) / np.maximum(count, 1)
        used = count > 0
        fitted[..., k] = np.interp(fa[..., k], np.arange(64)[used] * 4 + 1.5, mean[used])
    tuned = _difference(fitted, fb)
    share = lambda d: _cells(d, h, w)
    cells, tone_cells = share(raw), share(tuned)
    changed, after_tone = cells.mean(), tone_cells.mean()
    result = {'changed': round(float(changed), 4), 'boxes': [], 'edited': None, 'how': None}
    if changed < DUPLICATE:
        result['kind'] = 'duplicate'
        return result
    if after_tone < DUPLICATE:
        result.update(kind='edit', how='tone', changed=round(float(changed), 4))
        return result
    if changed > DIFFERENT:
        # Changed almost everywhere: still the same drawing (a repaint, another colouring) when the structure lines up.
        structure = _correlation(_grey(fa), _grey(fb))
        result['structure'] = round(structure, 3)
        result['kind'] = 'variant' if structure >= REDRAWN else 'similar' if structure >= SIMILAR else 'unrelated'
        return result
    rows, cols = cells.shape
    # Nearby changes are one change (an eye and its eyebrow): join cells up to one cell apart before boxing them.
    grown = np.pad(cells, 1)
    grown = np.logical_or.reduce([grown[1 + dy:1 + dy + rows, 1 + dx:1 + dx + cols] for dy in (-1, 0, 1) for dx in (-1, 0, 1)])
    boxes = _regions(grown)
    result['boxes'] = [{'x': x / cols, 'y': y / rows, 'w': bw / cols, 'h': bh / rows} for x, y, bw, bh, _ in boxes[:12]]
    # Only near the edges (text band, watermark, frame): an edit on top of the same picture.
    margin_x, margin_y = max(1, round(cols * BORDER)), max(1, round(rows * BORDER))
    inner = cells[margin_y:rows - margin_y, margin_x:cols - margin_x]
    if not inner.any():
        result.update(kind='edit', how='border')
        return result
    # Plainly hidden on one side (a flat block or a mosaic over the biggest change): an edit, not a variant.
    fa_, fb_ = fine or (a, b)
    H, W = fa_.shape[:2]
    x, y, bw, bh, _ = boxes[0]
    # The grown box reaches one cell past the change; step back in so the untouched surroundings do not count.
    x0, y0 = (x + (x > 0)) * CELL / w, (y + (y > 0)) * CELL / h
    x1, y1 = min(x + bw - (x + bw < cols), cols) * CELL / w, min(y + bh - (y + bh < rows), rows) * CELL / h
    box = (slice(round(y0 * H), round(y1 * H)), slice(round(x0 * W), round(x1 * W)))
    changed_px = _difference(_blur(fa_), _blur(fb_), 2) > PIXEL
    for side, arr in (('a', fa_), ('b', fb_)):
        how = _covered(arr, box, changed_px)
        if how and not _covered(fb_ if side == 'a' else fa_, box, changed_px):
            result.update(kind='edit', how=how, edited=side)
            return result
    result['kind'] = 'variant'
    return result


def _correlation(a, b):
    """Pearson correlation of two (blurred) grey images: ~0.9 for one drawing recoloured, ~0 for unrelated pages."""
    x, y = a.ravel() - a.mean(), b.ravel() - b.mean()
    return float((x @ y) / max(np.sqrt((x @ x) * (y @ y)), 1e-6))


def _cells(d, h, w):
    rows, cols = max(1, h // CELL), max(1, w // CELL)
    grid = (d[:rows * CELL, :cols * CELL] > PIXEL).reshape(rows, CELL, cols, CELL).mean((1, 3))
    return grid > CELL_SHARE


def _ncc(big, small):
    """Normalised cross-correlation of `small` at every position inside `big` (valid positions only)."""
    H, W = big.shape
    h, w = small.shape
    t = small - small.mean()
    norm_t = np.sqrt((t * t).sum())
    if norm_t < 1e-3 * h * w:
        return None
    shape = (H + h, W + w)
    corr = np.fft.irfft2(np.fft.rfft2(big, shape) * np.conj(np.fft.rfft2(t, shape)), shape)[:H - h + 1, :W - w + 1]
    s1 = np.pad(big, ((1, 0), (1, 0))).cumsum(0).cumsum(1)
    s2 = np.pad(big * big, ((1, 0), (1, 0))).cumsum(0).cumsum(1)
    window = lambda s: s[h:, w:] - s[:-h, w:] - s[h:, :-w] + s[:-h, :-w]
    var = window(s2) - window(s1) ** 2 / (h * w)
    # A flat window (no texture) matches nothing; without this its tiny variance blows the score up.
    return np.where(var > h * w, corr / (np.sqrt(np.maximum(var, 1e-6)) * norm_t), -1.0)


def _search(G, gs, factors, box=None):
    """Best NCC of the grey template `gs` inside G over template sizes `factors` × its own size; within `box` if given."""
    H, W = G.shape
    x0, y0, x1, y1 = box or (0, 0, W, H)
    region = G[y0:y1, x0:x1]
    hs, ws = gs.shape
    best = None
    for f in factors:
        w, h = round(ws * f), round(hs * f)
        if w < 24 or h < 24 or w > region.shape[1] or h > region.shape[0]:
            continue
        score = _ncc(region, _blur(_resize(gs, (w, h))))
        if score is None:
            continue
        i = int(np.argmax(score))
        y, x = divmod(i, score.shape[1])
        if best is None or score.flat[i] > best[0]:
            best = (float(score.flat[i]), x0 + x, y0 + y, w, h, f)
    return best


def _grey_at(x, side):
    g = _grey(x)
    scale = side / max(g.shape)
    return _blur(_resize(g, (max(8, round(g.shape[1] * scale)), max(8, round(g.shape[0] * scale)))))


def locate(big, small, least=0.1):
    """Where `small` appears inside `big`: (score, x, y, w, h) normalised to `big`, or None.

    A coarse search over sizes at SEARCH px, then two finer passes near the best at twice the resolution (sizes 1%
    and 0.25% apart): a few percent of scale error misaligns the edges enough to look like a change.
    `least`: the smallest share of `big` the part may cover (a line of text matches almost anywhere).
    """
    G = _grey_at(big, SEARCH)
    H, W = G.shape
    gs = _grey(small)
    hs, ws = gs.shape
    fit = min(W / ws, H / hs)                        # the largest template that still fits
    factors = [f for f in np.geomspace(fit, fit * 0.2, 28) if (ws * f) * (hs * f) <= 0.97 * W * H and (ws * f) * (hs * f) >= least * W * H]
    best = _search(G, gs, factors)
    if not best:
        return None
    F = _grey_at(big, SEARCH * 2)
    Hf, Wf = F.shape
    k = Wf / W
    score, x, y, w, h, f = best
    best = (score, x * k, y * k, w * k, h * k, f * k)          # in F's pixels from here on
    for spread, steps in ((1.08, 17), (1.012, 9)):
        _, x, y, w, h, f = best
        margin = round(max(w, h) * 0.1) + 4
        box = (max(0, round(x) - margin), max(0, round(y) - margin), min(Wf, round(x + w) + margin), min(Hf, round(y + h) + margin))
        best = _search(F, gs, np.geomspace(f / spread, f * spread, steps), box) or best
    score, x, y, w, h, _ = best
    return score, x / Wf, y / Hf, w / Wf, h / Hf


INSET = 0.06         # alignment looks for the middle of one image inside the other


def _align(xa, xb):
    """The overlapping parts of two nearly-registered images (a shift or a slight zoom apart), or None."""
    hb, wb = xb.shape[:2]
    core = xb[round(hb * INSET):round(hb * (1 - INSET)), round(wb * INSET):round(wb * (1 - INSET))]
    found = locate(xa, core)
    if not found or found[0] < MATCH:
        return None
    _, x, y, w, h = found
    k = 1 - 2 * INSET
    # b's whole frame in a's normalised coordinates, then clipped to a.
    fx, fy, fw, fh = x - w * INSET / k, y - h * INSET / k, w / k, h / k
    x0, y0, x1, y1 = max(fx, 0), max(fy, 0), min(fx + fw, 1), min(fy + fh, 1)
    if x1 - x0 < 0.5 or y1 - y0 < 0.5:
        return None
    ha, wa = xa.shape[:2]
    part_a = xa[round(y0 * ha):round(y1 * ha), round(x0 * wa):round(x1 * wa)]
    bx0, by0, bx1, by1 = (x0 - fx) / fw, (y0 - fy) / fh, (x1 - fx) / fw, (y1 - fy) / fh
    part_b = xb[round(by0 * hb):round(by1 * hb), round(bx0 * wb):round(bx1 * wb)]
    return part_a, part_b, (x1 - x0) * (y1 - y0), (bx1 - bx0) * (by1 - by0)


def relate(a, b):
    """Relation of image a to image b (arrays from load() plus their original sizes)."""
    (xa, size_a), (xb, size_b) = a, b
    ra, rb = xa.shape[1] / xa.shape[0], xb.shape[1] / xb.shape[0]
    out = {'boxes_a': [], 'boxes_b': []}
    if abs(np.log(ra / rb)) < SAME_RATIO:
        canvas = _canvas(ra)
        if size_a == size_b and xa.shape == xb.shape and np.array_equal(xa, xb):
            return out | {'kind': 'identical', 'changed': 0.0, 'note': '像素完全一致'}
        big = max(xa.shape[:2], xb.shape[:2])
        fine = (_resize(xa, (big[1], big[0])), _resize(xb, (big[1], big[0])))
        r = compare(_resize(xa, canvas), _resize(xb, canvas), fine)
        if r['kind'] != 'duplicate':
            # Maybe just a few pixels off (a re-export, a slightly different crop): compare what overlaps once aligned.
            aligned = _align(xa, xb)
            if aligned and min(aligned[2], aligned[3]) > 0.9:
                pa, pb = aligned[:2]
                size = (pb.shape[1], pb.shape[0])
                again = compare(_resize(pa, _canvas(size[0] / size[1])), _resize(pb, _canvas(size[0] / size[1])), (_resize(pa, size), pb))
                if again['changed'] < r['changed']:
                    r = again
        if r['kind'] not in ('similar', 'unrelated'):
            note = {'duplicate': '同一张图' + ('（尺寸不同）' if size_a != size_b else '（重新压缩过）'),
                    'variant': f"局部不同，约占画面 {r['changed']:.0%}" if r['changed'] <= DIFFERENT else '线稿相同，大面积重新上色或修改',
                    'edit': {'tone': '整体调色或亮度不同', 'border': '边缘加了文字、水印或边框', 'mosaic': '局部打了马赛克', 'fill': '局部被色块遮住'}.get(r['how'], '')}[r['kind']]
            if r['kind'] == 'variant' and r['changed'] < SUBTLE:
                r['kind'], note = 'subtle', '只有很小的地方不同（表情、签名、小物件等）'
            if r['kind'] == 'edit' and r['edited']:
                note += '（改动在' + ('左图' if r['edited'] == 'a' else '右图') + '）'
            return out | {'kind': r['kind'], 'changed': r['changed'], 'note': note, 'boxes_a': r['boxes'], 'boxes_b': r['boxes'], 'edited': r['edited']}
        fallback = r
    else:
        fallback = None
    # One may be a part of the other: look for the smaller picture inside the larger.
    # Either may be the part: two crops of one original each contain most of the other.
    tries = [(locate(xa, xb), xa, xb, False), (locate(xb, xa), xb, xa, True)]
    found, big, small, flip = max(tries, key=lambda t: t[0][0] if t[0] else -1)
    if found and found[0] >= MATCH:
        score, x, y, w, h = found
        H, W = big.shape[:2]
        part = big[round(y * H):round((y + h) * H), round(x * W):round((x + w) * W)]
        canvas = _canvas(part.shape[1] / part.shape[0])
        r = compare(_resize(part, canvas), _resize(small, canvas))
        where = {'x': x, 'y': y, 'w': w, 'h': h}
        inside = [{'x': x + b['x'] * w, 'y': y + b['y'] * h, 'w': b['w'] * w, 'h': b['h'] * h} for b in r['boxes']]
        note = f"{'右图' if not flip else '左图'}是{'左图' if not flip else '右图'}的一部分（约占 {w * h:.0%}）"
        if r['kind'] in ('variant', 'edit') and r['changed'] >= 0.02:     # resampling alone leaves a little
            note += '，重叠处也有改动'
        boxes_big, boxes_small = [where | {'match': True}] + inside, r['boxes']
        return out | {'kind': 'crop', 'changed': r['changed'], 'score': round(score, 3), 'note': note,
                      'boxes_a': boxes_small if flip else boxes_big, 'boxes_b': boxes_big if flip else boxes_small}
    if fallback:
        return out | {'kind': fallback['kind'], 'changed': fallback['changed'],
                      'note': '构图接近，但画的不是同一张' if fallback['kind'] == 'similar' else ''}
    return out | {'kind': 'unrelated', 'note': ''}


def relate_all(images):
    """All pairs of {uuid: load() result}; plus groups of the same picture (identical / duplicate)."""
    ids = list(images)
    pairs = []
    for i, j in combinations(range(len(ids)), 2):
        r = relate(images[ids[i]], images[ids[j]])
        pairs.append({'a': ids[i], 'b': ids[j], 'label': LABELS[r['kind']], **r})
    pairs.sort(key=lambda p: (RANK[p['kind']], ids.index(p['a']), ids.index(p['b'])))
    parent = {u: u for u in ids}
    find = lambda u: u if parent[u] == u else find(parent[u])
    for p in pairs:
        if p['kind'] in ('identical', 'duplicate'):
            parent[find(p['b'])] = find(p['a'])
    groups = {}
    for u in ids:
        groups.setdefault(find(u), []).append(u)
    return {'pairs': pairs, 'same': [g for g in groups.values() if len(g) > 1],
            'sizes': {u: {'width': images[u][1][0], 'height': images[u][1][1]} for u in ids}}
