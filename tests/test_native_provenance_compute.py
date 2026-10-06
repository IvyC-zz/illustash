"""#153 synthetic parity against unchanged Python algorithms and handwritten invariants.

No build/model/network/real library discovery. Candidate execution requires
ILLUSTASH_PROVENANCE_COMPUTE_BIN pointing at an explicitly built example binary.
"""
import hashlib
import json
import os
from pathlib import Path
import struct
import subprocess
import tempfile
import unittest

from PIL import Image
from provenance.sources import pixel_hash


class IndependentProvenanceInputs(unittest.TestCase):
    def test_review_decode_counterexamples_against_old_reference(self):
        gif = bytes.fromhex('47494638396102000100800000ff000000ff002c01000000010001000002024c01003b')
        self.assertEqual(pixel_hash.pixel_hash(gif), '8e58a0d07c7dfd772cd78dc4f0fb91d9')
        for magic, payload in [(b'P6', b'\x01\x01\x01'), (b'P3', b'1 1 1\n')]:
            self.assertEqual(pixel_hash.pixel_hash(magic + b'\n1 1\n6\n' + payload),
                             '1d985b5ca1e25181496f9834a45e0b6f')

    def test_literal_pam_hash_and_hidden_transparent_colour(self):
        literal = b'P7\nWIDTH 1\nHEIGHT 1\nDEPTH 4\nMAXVAL 255\nTUPLTYPE RGB_ALPHA\nENDHDR\n'
        self.assertEqual(hashlib.md5(literal + bytes([12, 34, 56, 0])).hexdigest(),
                         '67222c4833b8bfd91eb0f1f2ce68ce67')
        with tempfile.TemporaryDirectory() as tmp:
            image = Path(tmp) / 'transparent.png'
            Image.new('RGBA', (1, 1), (12, 34, 56, 0)).save(image)
            self.assertEqual(pixel_hash.pixel_hash(image), '67222c4833b8bfd91eb0f1f2ce68ce67')


class NativeProvenanceComputeTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        configured = os.environ.get('ILLUSTASH_PROVENANCE_COMPUTE_BIN')
        if not configured:
            raise unittest.SkipTest('explicit candidate provenance probe required; no automatic build')
        cls.binary = Path(configured).resolve()
        if not cls.binary.is_file():
            raise AssertionError('configured provenance probe missing')

    def setUp(self):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.root = Path(temp.name)

    def probe(self, request, success=True):
        incoming, output = self.root / 'request.json', self.root / 'output.json'
        incoming.write_text(json.dumps(request), encoding='utf-8')
        output.unlink(missing_ok=True)
        run = subprocess.run([str(self.binary), str(incoming), str(output)],
                             env=os.environ | {'PATH': ''}, capture_output=True,
                             timeout=30, start_new_session=os.name != 'nt')
        if success:
            self.assertEqual(run.returncode, 0, run.stderr.decode())
            return json.loads(output.read_text())
        self.assertNotEqual(run.returncode, 0)
        self.assertFalse(output.exists())
        return run.stderr.decode()

    def test_pam_exact_dimensions_straight_alpha_and_invalid_buffer(self):
        for width, height, pixels in [(1, 1, [12, 34, 56, 0]),
                                      (2, 1, [10, 20, 30, 255, 250, 240, 230, 128]),
                                      (1, 2, [10, 20, 30, 255, 250, 240, 230, 128])]:
            header = f'P7\nWIDTH {width}\nHEIGHT {height}\nDEPTH 4\nMAXVAL 255\nTUPLTYPE RGB_ALPHA\nENDHDR\n'.encode()
            expected = hashlib.md5(header + bytes(pixels)).hexdigest()
            self.assertEqual(self.probe(dict(op='rgba', width=width, height=height, pixels=pixels)), expected)
        for width, height, pixels in [(0, 1, []), (1, 0, []), (1, 1, [1, 2, 3])]:
            self.probe(dict(op='rgba', width=width, height=height, pixels=pixels), success=False)

    def test_candidate_decode_exact_for_synthetic_modes_paths_and_bytes(self):
        rgb = Image.new('RGB', (23, 17))
        rgb.putdata([(x * 13 % 256, x * 47 % 256, x * 59 % 256) for x in range(23 * 17)])
        cases = [(mode, rgb.convert(mode), 'PNG', {}) for mode in ('RGB', 'RGBA', 'L', 'LA', 'P', '1')]
        alpha = rgb.convert('RGBA')
        alpha.putalpha(Image.frombytes('L', alpha.size, bytes(x * 23 % 256 for x in range(23 * 17))))
        cases.append(('alpha', alpha, 'PNG', {}))
        palette = rgb.convert('P')
        cases.append(('palette-alpha', palette, 'PNG', dict(transparency=1)))
        for fmt in ('JPEG', 'WEBP', 'BMP', 'PPM'):
            cases.append((fmt, rgb, fmt, {}))
        cases.append(('CMYK', rgb.convert('CMYK'), 'JPEG', {}))
        exif = Image.Exif()
        exif[274] = 6
        cases.append(('orientation', rgb, 'JPEG', dict(exif=exif)))
        for name, image, fmt, kwargs in cases:
            with self.subTest(name=name):
                path = self.root / (name + '.' + fmt.lower())
                image.save(path, format=fmt, **kwargs)
                expected = pixel_hash.pixel_hash(path)
                self.assertEqual(self.probe(dict(op='hash', path=str(path))), expected)
                self.assertEqual(self.probe(dict(op='hash_bytes', path=str(path))), expected)
        corrupt = self.root / 'corrupt.png'
        corrupt.write_bytes(b'not an image')
        self.probe(dict(op='hash', path=str(corrupt)), success=False)
        self.probe(dict(op='hash', path=str(self.root / 'missing.png')), success=False)

    def test_unaccepted_alpha_tiff_never_returns_a_hash(self):
        path = self.root / 'synthetic.tiff'
        Image.new('RGBA', (7, 9), (255, 0, 0, 123)).save(path)
        self.assertIn('TIFF', self.probe(dict(op='hash', path=str(path)), success=False))

    def test_review_gif_palette_canvas_and_pnm_rounding(self):
        raw = bytes.fromhex('47494638396102000100800000ff000000ff002c01000000010001000002024c01003b')
        cases = [('partial.gif', raw)]
        # Explicit transparent red/green palette entries retain their hidden RGB.
        for index in (0, 1):
            cases.append((f'partial-alpha-{index}.gif', raw[:19] +
                          bytes.fromhex('21f904010000') + bytes([index, 0]) + raw[19:]))
        for magic, plain, bands in [(b'P6', False, 3), (b'P3', True, 3),
                                    (b'P5', False, 1), (b'P2', True, 1)]:
            for maximum in (6, 10, 255, 256, 65535):
                values = [1, maximum // 2, maximum] * bands
                payload = (' '.join(map(str, values)) + '\n').encode() if plain else (
                    bytes(values) if maximum <= 255 else struct.pack('>' + 'H' * len(values), *values))
                cases.append((f'{magic.decode()}-{maximum}.pnm', magic +
                              f'\n3 1\n{maximum}\n'.encode() + payload))
        for name, data in cases:
            with self.subTest(name=name):
                path = self.root / name
                path.write_bytes(data)
                for op, reference in [('hash', path), ('hash_bytes', data)]:
                    self.assertEqual(self.probe(dict(op=op, path=str(path))), pixel_hash.pixel_hash(reference))
