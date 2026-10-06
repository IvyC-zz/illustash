"""图片关系判断：用人造的带纹理图片覆盖 重复 / 差分 / 局部编辑 / 裁剪 / 无关。"""
import io
import tempfile
import unittest
from pathlib import Path

import numpy as np
from PIL import Image, ImageDraw, ImageFilter

from web import relate


def picture(seed, size=(900, 1200)):
    """An illustration-like image: soft colour fields, shapes with edges, fine texture."""
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


class RelateTest(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.base = picture(1)

    def tearDown(self):
        self.temp.cleanup()

    def load(self, im, name):
        path = Path(self.temp.name) / (name + '.png')
        im.save(path)
        return relate.load(path)

    def relation(self, other, name='b'):
        return relate.relate(self.load(self.base, 'a'), self.load(other, name))

    def test_identical_and_resized_recompressed_copy(self):
        self.assertEqual(self.relation(self.base.copy())['kind'], 'identical')
        copy = jpeg(self.base.resize((600, 800), Image.LANCZOS), 70)
        r = self.relation(copy)
        self.assertEqual(r['kind'], 'duplicate', r)
        self.assertIn('尺寸不同', r['note'])

    def test_variant_changes_a_local_area(self):
        other = self.base.copy()
        patch = picture(7, (260, 220))
        other.paste(patch, (330, 420))                       # a different face / expression, inside the picture
        r = self.relation(jpeg(other, 90))
        self.assertEqual(r['kind'], 'variant', r)
        box = r['boxes_b'][0]
        self.assertAlmostEqual(box['x'] + box['w'] / 2, (330 + 130) / 900, delta=0.05)
        self.assertAlmostEqual(box['y'] + box['h'] / 2, (420 + 110) / 1200, delta=0.05)

    def test_a_tiny_change_is_a_subtle_variant(self):
        other = self.base.copy()
        other.paste(picture(8, (90, 90)), (400, 500))                  # ~0.75% of the picture: a mouth, a pair of glasses
        r = self.relation(jpeg(other, 90))
        self.assertEqual(r['kind'], 'subtle', r)

    def test_edits_mosaic_border_text_and_tone(self):
        mosaic = self.base.copy()
        region = mosaic.crop((300, 400, 600, 700)).resize((10, 10), Image.NEAREST).resize((300, 300), Image.NEAREST)
        mosaic.paste(region, (300, 400))
        r = self.relation(mosaic)
        self.assertEqual((r['kind'], r['edited']), ('edit', 'b'), r)
        self.assertIn('马赛克', r['note'])
        text = self.base.copy()
        ImageDraw.Draw(text).rectangle([0, 1140, 900, 1200], fill=(255, 255, 255))
        ImageDraw.Draw(text).text((20, 1150), '@artist  do not repost', fill=(0, 0, 0))
        self.assertEqual(self.relation(text)['note'], '边缘加了文字、水印或边框')
        tone = Image.fromarray(np.clip(np.asarray(self.base, np.float32) * 0.8 + 30, 0, 255).astype(np.uint8))
        r = self.relation(tone)
        self.assertEqual((r['kind'], r['note']), ('edit', '整体调色或亮度不同'), r)

    def test_crop_is_found_inside_the_larger_picture(self):
        crop = jpeg(self.base.crop((180, 300, 720, 900)).resize((360, 400), Image.LANCZOS), 85)
        r = self.relation(crop)
        self.assertEqual(r['kind'], 'crop', r)
        where = r['boxes_a'][0]
        self.assertTrue(where['match'])
        for got, want in ((where['x'], 180 / 900), (where['y'], 300 / 1200), (where['w'], 540 / 900), (where['h'], 600 / 1200)):
            self.assertAlmostEqual(got, want, delta=0.03)
        self.assertIn('右图是左图的一部分', r['note'])

    def test_unrelated_pictures_and_groups(self):
        self.assertEqual(self.relation(picture(2))['kind'], 'unrelated')
        images = {'a': self.load(self.base, 'a'), 'b': self.load(jpeg(self.base, 60), 'b'), 'c': self.load(picture(3), 'c')}
        result = relate.relate_all(images)
        self.assertEqual(result['same'], [['a', 'b']])
        self.assertEqual([(p['a'], p['b'], p['kind']) for p in result['pairs']][0], ('a', 'b', 'duplicate'))


if __name__ == '__main__':
    unittest.main()
