"""macOS App 图标（#293）：docs/assets/icon-transparent.svg 的图形放在白色底板上。

macOS 26 会把不是满幅底板的图标放到灰色底板上，所以 App 包用自己的图标：Apple 的 1024 网格（位于 100 px 处、
824 px 的连续圆角底板），白色，下面一层柔和的阴影，图形居中放在上面。Windows 继续用 icon.ico，欢迎页继续用透明的
icon.png。

    python3 tools/mac_icon.py    # 只能在 macOS 上运行：写出 docs/assets/icon-macos.svg 和 desktop/icons/icon.icns

SVG 由 macOS（sips）渲染；CoreSVG 忽略 SVG 滤镜，所以阴影在这里加到位图上。
"""
import math
import subprocess
import tempfile
from pathlib import Path

from PIL import Image, ImageFilter

ROOT = Path(__file__).resolve().parent.parent
GLYPH = ROOT / 'docs/assets/icon-transparent.svg'
SVG = ROOT / 'docs/assets/icon-macos.svg'
ICNS = ROOT / 'desktop/icons/icon.icns'
# 图形在它 1254 的 viewBox 里实际绘制的边界，以及它在 824 px 底板上的高度（约 70 %）。
BOUNDS, SCALE = (355, 302, 901, 931), .92
SHADOW = {'dy': 12, 'blur': 14, 'opacity': .22}


def tile_path(n=5, a=412, c=512, steps=360):
    """超椭圆 |x|^n + |y|^n = 1，接近 Apple 的连续圆角。"""
    pts = []
    for i in range(steps):
        t = 2 * math.pi * i / steps
        ct, st = math.cos(t), math.sin(t)
        pts.append((c + a * math.copysign(abs(ct) ** (2 / n), ct), c + a * math.copysign(abs(st) ** (2 / n), st)))
    return 'M' + ' L'.join(f'{x:.2f} {y:.2f}' for x, y in pts) + ' Z'


def svg():
    glyph = GLYPH.read_text()
    inner = glyph[glyph.index('<defs>') + len('<defs>'):glyph.rindex('</svg>')]
    defs, body = inner.split('</defs>', 1)
    x0, y0, x1, y1 = BOUNDS
    tile = tile_path()
    return f'''<svg xmlns="http://www.w3.org/2000/svg" width="1024" height="1024" viewBox="0 0 1024 1024" fill="none">
  <title>Illustash — macOS app icon</title>
  <desc>Made by tools/mac_icon.py from icon-transparent.svg (#293): the glyph on a white tile on Apple's icon grid. The shadow is added to the bitmap.</desc>
  <defs>
    <linearGradient id="macTile" x1="512" y1="100" x2="512" y2="924" gradientUnits="userSpaceOnUse"><stop stop-color="#ffffff"/><stop offset="1" stop-color="#f7f7fb"/></linearGradient>{defs}</defs>
  <path d="{tile}" fill="url(#macTile)"/>
  <path d="{tile}" stroke="#000" stroke-opacity=".05" stroke-width="2"/>
  <g transform="translate(512 512) scale({SCALE}) translate({-(x0 + x1) / 2} {-(y0 + y1) / 2})">{body}</g>
</svg>
'''


def with_shadow(icon):
    alpha = icon.getchannel('A').point(lambda v: round(v * SHADOW['opacity']))
    shadow = Image.new('RGBA', icon.size, (0, 0, 0, 0))
    shadow.putalpha(alpha.filter(ImageFilter.GaussianBlur(SHADOW['blur'])).transform(icon.size, Image.AFFINE, (1, 0, 0, 0, 1, -SHADOW['dy'])))
    return Image.alpha_composite(shadow, icon)


def main():
    SVG.write_text(svg())
    with tempfile.TemporaryDirectory() as tmp:
        tmp = Path(tmp)
        subprocess.run(['sips', '-s', 'format', 'png', str(SVG), '--out', str(tmp / 'tile.png')], check=True, capture_output=True)
        icon = with_shadow(Image.open(tmp / 'tile.png').convert('RGBA'))
        iconset = tmp / 'icon.iconset'
        iconset.mkdir()
        for size in (16, 32, 128, 256, 512):
            for scale, suffix in ((1, ''), (2, '@2x')):
                icon.resize((size * scale,) * 2, Image.LANCZOS).save(iconset / f'icon_{size}x{size}{suffix}.png')
        subprocess.run(['iconutil', '-c', 'icns', str(iconset), '-o', str(ICNS)], check=True)
    print(f'{SVG.relative_to(ROOT)}, {ICNS.relative_to(ROOT)}')


if __name__ == '__main__':
    main()
