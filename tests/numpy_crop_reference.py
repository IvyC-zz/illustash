"""#347 测试专用裁剪参考，保留历史 NumPy 运算形状和顺序。

来源：c6762ff65ecd06ca98a0b266bb49f9e04eeaa118 的 web/relate.py。
只摘取像素解码与 locate 的完整纯计算依赖；没有旧应用、联网或候选程序入口。
"""
import numpy as np
from PIL import Image, ImageOps

LOAD = 1024
SEARCH = 256


def load(path):
    """按显示方向解码，透明部分铺浅灰，返回 RGB float32 和原尺寸。"""
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
        return (np.asarray(im.convert('RGB'), np.float32), size)


def _resize(x, size):
    """按 Pillow 双线性缩放每个浮点通道。"""
    if x.ndim == 2:
        return np.asarray(Image.fromarray(x.astype(np.float32), 'F').resize(size, Image.BILINEAR), np.float32)
    return np.stack([_resize(x[..., k], size) for k in range(x.shape[2])], -1)


def _blur(x):
    """边缘复制的 3×3 均值模糊。"""
    p = np.pad(x, [(1, 1), (1, 1)] + [(0, 0)] * (x.ndim - 2), mode='edge')
    (h, w) = x.shape[:2]
    return sum((p[i:i + h, j:j + w] for i in range(3) for j in range(3))) / 9.0


def _grey(x):
    return x @ np.array([0.299, 0.587, 0.114], np.float32)


def _ncc(big, small):
    """用 FFT、积分图和局部方差计算所有有效位置的归一化互相关。"""
    (H, W) = big.shape
    (h, w) = small.shape
    t = small - small.mean()
    norm_t = np.sqrt((t * t).sum())
    if norm_t < 0.001 * h * w:
        return None
    shape = (H + h, W + w)
    corr = np.fft.irfft2(np.fft.rfft2(big, shape) * np.conj(np.fft.rfft2(t, shape)), shape)[:H - h + 1, :W - w + 1]
    s1 = np.pad(big, ((1, 0), (1, 0))).cumsum(0).cumsum(1)
    s2 = np.pad(big * big, ((1, 0), (1, 0))).cumsum(0).cumsum(1)
    window = lambda s: s[h:, w:] - s[:-h, w:] - s[h:, :-w] + s[:-h, :-w]
    var = window(s2) - window(s1) ** 2 / (h * w)
    return np.where(var > h * w, corr / (np.sqrt(np.maximum(var, 1e-06)) * norm_t), -1.0)


def _search(G, gs, factors, box=None):
    """按给定尺度序列选择第一个最高分位置。"""
    (H, W) = G.shape
    (x0, y0, x1, y1) = box or (0, 0, W, H)
    region = G[y0:y1, x0:x1]
    (hs, ws) = gs.shape
    best = None
    for f in factors:
        (w, h) = (round(ws * f), round(hs * f))
        if w < 24 or h < 24 or w > region.shape[1] or (h > region.shape[0]):
            continue
        score = _ncc(region, _blur(_resize(gs, (w, h))))
        if score is None:
            continue
        i = int(np.argmax(score))
        (y, x) = divmod(i, score.shape[1])
        if best is None or score.flat[i] > best[0]:
            best = (float(score.flat[i]), x0 + x, y0 + y, w, h, f)
    return best


def _grey_at(x, side):
    g = _grey(x)
    scale = side / max(g.shape)
    return _blur(_resize(g, (max(8, round(g.shape[1] * scale)), max(8, round(g.shape[0] * scale)))))


def locate(big, small, least=0.1):
    """粗搜索后做两轮局部细搜索，返回分数和归一化坐标。"""
    G = _grey_at(big, SEARCH)
    (H, W) = G.shape
    gs = _grey(small)
    (hs, ws) = gs.shape
    fit = min(W / ws, H / hs)
    factors = [f for f in np.geomspace(fit, fit * 0.2, 28) if ws * f * (hs * f) <= 0.97 * W * H and ws * f * (hs * f) >= least * W * H]
    best = _search(G, gs, factors)
    if not best:
        return None
    F = _grey_at(big, SEARCH * 2)
    (Hf, Wf) = F.shape
    k = Wf / W
    (score, x, y, w, h, f) = best
    best = (score, x * k, y * k, w * k, h * k, f * k)
    for (spread, steps) in ((1.08, 17), (1.012, 9)):
        (_, x, y, w, h, f) = best
        margin = round(max(w, h) * 0.1) + 4
        box = (max(0, round(x) - margin), max(0, round(y) - margin), min(Wf, round(x + w) + margin), min(Hf, round(y + h) + margin))
        best = _search(F, gs, np.geomspace(f / spread, f * spread, steps), box) or best
    (score, x, y, w, h, _) = best
    return (score, x / Wf, y / Hf, w / Wf, h / Hf)
