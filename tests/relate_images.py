"""图片关系测试用的类插画合成图片（原 tests/test_relate.py 的辅助函数）。"""
import io

import numpy as np
from PIL import Image, ImageDraw


def picture(seed, size=(900, 1200)):
    """一张类似插画的图片：柔和的色块、带边缘的形状、细密的纹理。"""
    rng = np.random.default_rng(seed)
    base = Image.fromarray(rng.integers(0, 255, (size[1] // 60, size[0] // 60, 3), dtype=np.uint8)).resize(size, Image.BICUBIC)
    d = ImageDraw.Draw(base)
    for _ in range(40):
        x, y = rng.integers(0, size[0]), rng.integers(0, size[1])
        r = int(rng.integers(20, 160))
        colour = tuple(int(v) for v in rng.integers(0, 255, 3))
        (d.ellipse if rng.random() < 0.5 else d.rectangle)([x - r, y - r, x + r, y + r], fill=colour, outline=(0, 0, 0), width=4)
    noise = rng.normal(0, 6, (size[1], size[0], 3))
    return Image.fromarray(np.clip(np.asarray(base, np.float32) + noise, 0, 255).astype(np.uint8))


def jpeg(im, quality=80):
    buf = io.BytesIO()
    im.save(buf, 'JPEG', quality=quality)
    return Image.open(io.BytesIO(buf.getvalue())).convert('RGB')
