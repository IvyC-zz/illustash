"""#153：固定的 LCMS/标量 TIFF 候选等价测试；显式指定二进制，从不自动构建。

旧参照：未改动的 provenance.sources.pixel_hash 和 Pillow/Cms2.19。
必须分别给出按路径和按字节的 TIFF 预期。输入只用合成数据。
"""
import hashlib
import io
import os
from pathlib import Path
import tempfile
import unittest
from PIL import Image, ImageCms, features
from tests.image_expectations import pixel_hash as pixel_module
pixel_hash = pixel_module.pixel_hash
from tests import test_native_provenance_compute as driver
from tests.provenance_image_fixtures import lab_profile, gamma_profile, jpeg_icc_variant, raw_u16_tiff, rgb_image, scalar_image, scalar_tiff, scalar_tiffs, srgb_profile


class IndependentICCAndScalarInputs(unittest.TestCase):
    def test_old_mm_deflate32_reinterpretation_and_handwritten_pam(self):
        import struct
        for mode, values, old_values, gray in [
                ('i32', [-2147483648, -1, 0, 127, 256, 2147483647],
                 [128, -1, 0, 2130706432, 65536, -129], [128, 0, 0, 255, 255, 0]),
                ('f32', [1., 2., 3., 4., 100., 255.],
                 [32831, 64, 16448, 32832, 51266, 32579], [0] * 6)]:
            data = scalar_tiff(mode=mode, endian='>', values=values, compression=8)
            with Image.open(io.BytesIO(data)) as image:
                self.assertEqual(image.tile[0].codec_name, 'libtiff')
                self.assertEqual(image.tile[0].args[0], 'I;32BS' if mode == 'i32' else 'F;32BF')
                image.load()
                actual = list(image.get_flattened_data())
            if mode == 'f32':
                actual = [struct.unpack('<I', struct.pack('<f', v))[0] for v in actual]
            self.assertEqual(actual, old_values)
            header = b'P7\nWIDTH 3\nHEIGHT 2\nDEPTH 4\nMAXVAL 255\nTUPLTYPE RGB_ALPHA\nENDHDR\n'
            expected = hashlib.md5(header + bytes(c for v in gray for c in (v, v, v, 255))).hexdigest()
            self.assertEqual(pixel_hash(data), expected)
            with tempfile.TemporaryDirectory() as tmp:
                path = Path(tmp) / 'mm-deflate.tiff'
                path.write_bytes(data)
                self.assertEqual(pixel_hash(path), expected)
            with Image.open(io.BytesIO(scalar_tiff(mode=mode, endian='>', values=values))) as image:
                self.assertEqual(image.tile[0].codec_name, 'raw')
                image.load()
                self.assertEqual(list(image.get_flattened_data()), values)

    def test_review_multistrip_mmap_qualification_against_old_reference(self):
        with tempfile.TemporaryDirectory() as tmp:
            for endian in ('<', '>'):
                for rows in (1, 2):
                    for orientation in range(1, 9):
                        path = Path(tmp) / f'strips-{ord(endian)}-{rows}-{orientation}.tiff'
                        path.write_bytes(raw_u16_tiff(endian=endian, rows=rows, orientation=orientation))
                        with Image.open(path) as image:
                            self.assertEqual(len(image.tile), 2 if rows == 1 else 1)
                            image.load()
                            self.assertEqual(bool(image.map), rows == 2)
                            self.assertEqual(image.size, ((3, 2) if rows == 2 or orientation < 5 else (2, 3)))
                            # Image.__exit__ 关闭 fp，但不关闭留下来的 mmap/core。
                            image.close()
                        self.assertEqual(pixel_hash(path) != pixel_hash(path.read_bytes()),
                                         rows == 2 and orientation >= 5)
                        if rows == 1 and orientation == 5:
                            self.assertEqual(pixel_hash(path), 'e047539afcdeb139b9623c13a11e59a9')

    def test_review_fillorder2_literal_bits_before_clamp_and_no_mmap(self):
        header = b'P7\nWIDTH 1\nHEIGHT 1\nDEPTH 4\nMAXVAL 255\nTUPLTYPE RGB_ALPHA\nENDHDR\n'
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / 'bits.tiff'
            path.write_bytes(raw_u16_tiff(width=1, height=1, values=[128], fill_order=2))
            expected = hashlib.md5(header + bytes([1, 1, 1, 255])).hexdigest()
            self.assertEqual(expected, '4d074b03fdaa96bf3add2057013e2048')
            self.assertEqual(pixel_hash(path), expected)
            self.assertEqual(pixel_hash(path.read_bytes()), expected)
            for rows in (1, 2):
                for orientation in range(1, 9):
                    path.write_bytes(raw_u16_tiff(rows=rows, orientation=orientation, fill_order=2))
                    with Image.open(path) as image:
                        image.load()
                        self.assertFalse(image.map)
                        self.assertEqual(image.size, (2, 3) if orientation >= 5 else (3, 2))
                    self.assertEqual(pixel_hash(path), pixel_hash(path.read_bytes()))
            path.write_bytes(raw_u16_tiff(width=1, height=1, values=[128], fill_order=2, compression=8))
            with Image.open(path) as image:
                image.load()
                self.assertEqual(image.getpixel((0, 0)), 128)  # libtiff 会把压缩字节反转。

    def test_build_override_isolation_keeps_parent_environment(self):
        from tools.build_provenance_probe import build_environment
        parent = dict(PATH='/synthetic/bin', LCMS2_LIB_DIR='/synthetic/lcms217',
                      LCMS2_INCLUDE_DIR='/synthetic/include', LCMS2_STATIC='1',
                      CARGO_TARGET_DIR='/synthetic/fixed-acceptance', KEEP='unchanged')
        before = parent.copy()
        self.assertEqual(build_environment(parent), dict(PATH='/synthetic/bin', KEEP='unchanged'))
        self.assertEqual(parent, before)

    def test_old_reference_engine_version_and_gamma_nonidentity(self):
        self.assertEqual(features.version_module('littlecms2'), '2.19')
        original = rgb_image()
        for padded in (False, True):
            transformed = ImageCms.profileToProfile(original, ImageCms.ImageCmsProfile(io.BytesIO(gamma_profile(padded))),
                                                   ImageCms.createProfile('sRGB'), outputMode='RGB')
            self.assertNotEqual(transformed.tobytes(), original.tobytes())

    def test_handwritten_scalar_clamp(self):
        for mode, values in [('I;16', [0, 1, 127, 255, 255, 255]),
                             ('I;16B', [0, 1, 127, 255, 255, 255]),
                             ('I', [0, 0, 0, 1, 255, 255]),
                             ('F', [0, 0, 255, 0, 0, 127])]:
            rgba = bytes(component for v in values for component in (v, v, v, 255))
            header = b'P7\nWIDTH 3\nHEIGHT 2\nDEPTH 4\nMAXVAL 255\nTUPLTYPE RGB_ALPHA\nENDHDR\n'
            with tempfile.TemporaryDirectory() as tmp:
                path = Path(tmp) / 'scalar.tiff'
                scalar_image(mode).save(path)
                self.assertEqual(pixel_hash(path), hashlib.md5(header + rgba).hexdigest(), mode)
                self.assertEqual(pixel_hash(path.read_bytes()), hashlib.md5(header + rgba).hexdigest(), mode)

    def test_old_jpeg_fragment_order_count_and_legacy_sequence_handling(self):
        plain, profiled = io.BytesIO(), io.BytesIO()
        image = rgb_image()
        image.save(plain, format='JPEG')
        image.save(profiled, format='JPEG', icc_profile=gamma_profile(True))
        self.assertNotEqual(pixel_hash(plain.getvalue()), pixel_hash(profiled.getvalue()))
        for variant in ('reverse', 'zero-first-sequence'):
            self.assertEqual(pixel_hash(jpeg_icc_variant(profiled.getvalue(), variant)), pixel_hash(profiled.getvalue()))
        for variant in ('bad-total', 'missing'):
            self.assertEqual(pixel_hash(jpeg_icc_variant(profiled.getvalue(), variant)), pixel_hash(plain.getvalue()))

    def test_tiff_path_bytes_old_difference_is_fixed_independently(self):
        differences = []
        with tempfile.TemporaryDirectory() as tmp:
            for mode, compression, orientation, path in scalar_tiffs(tmp):
                if pixel_hash(path) != pixel_hash(path.read_bytes()):
                    differences.append((mode, compression, orientation))
                if (mode, compression, orientation) == ('I;16', 'raw', 5):
                    self.assertEqual(pixel_hash(path), '8b63fd4c70147424891a484d08122dcf')
                    self.assertEqual(pixel_hash(path.read_bytes()), 'e047539afcdeb139b9623c13a11e59a9')
        self.assertEqual(differences, [(m, 'raw', o) for m in ('I;16', 'I;16B') for o in range(5, 9)])


class NativeICCAndScalarTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        driver.NativeProvenanceComputeTest.setUpClass.__func__(cls)

    def setUp(self):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.root = Path(temp.name)

    def probe(self, request, success=True):
        return driver.NativeProvenanceComputeTest.probe(self, request, success)

    def compare(self, path):
        for op, old_input in [('hash', path), ('hash_bytes', path.read_bytes())]:
            with self.subTest(path=path.name, op=op):
                self.assertEqual(self.probe(dict(op=op, path=str(path))), pixel_hash(old_input))

    def test_rgb8_tiff_orientation_compression_and_icc_against_old_reference(self):
        for compression in ('raw', 'tiff_adobe_deflate', 'tiff_lzw', 'packbits'):
            for orientation in range(1, 9):
                for profile in (None, gamma_profile(), b'synthetic invalid ICC'):
                    path = self.root / f'rgb-{compression}-{orientation}-{len(profile or b"")}.tiff'
                    kwargs = dict(compression=compression, tiffinfo={274: orientation})
                    if profile is not None:
                        kwargs['icc_profile'] = profile
                    rgb_image().save(path, **kwargs)
                    self.compare(path)
        # 不悄悄放宽到 alpha、planar 或 tiled 的取值范围。
        path = self.root / 'planar.tiff'
        rgb_image().save(path, tiffinfo={284: 2})
        self.probe(dict(op='hash', path=str(path)), success=False)

    def test_final_linked_cmm_is_2190(self):
        self.assertEqual(self.probe(dict(op='lcms_version')), 2190)

    def test_mm_deflate32_and_unchanged_raw_le_u16_matrix(self):
        for mode in ('i32', 'f32', 'u16'):
            for endian in ('<', '>'):
                for compression in (1, 8):
                    for rows in (1, 2):
                        for orientation in range(1, 10):
                            path = self.root / f'scalar-{mode}-{ord(endian)}-{compression}-{rows}-{orientation}.tiff'
                            path.write_bytes(scalar_tiff(mode=mode, endian=endian, compression=compression,
                                                       rows=rows, orientation=orientation))
                            self.compare(path)

    def test_mm_deflate32_icc_after_legacy_rgb_conversion(self):
        for mode in ('i32', 'f32'):
            for profile in (b'synthetic invalid ICC', gamma_profile()):
                path = self.root / f'mm-{mode}-icc.tiff'
                path.write_bytes(scalar_tiff(mode=mode, endian='>', compression=8, icc=profile))
                self.compare(path)

    def test_unverified_mm_compression_and_planar_layout_are_gated(self):
        for mode in ('i32', 'f32'):
            for label, kwargs in [('deflate32946', dict(compression=32946)),
                                  ('planar2', dict(compression=8, planar=2))]:
                path = self.root / f'mm-{mode}-{label}.tiff'
                path.write_bytes(scalar_tiff(mode=mode, endian='>', **kwargs))
                for op in ('hash', 'hash_bytes'):
                    with self.subTest(mode=mode, label=label, op=op):
                        self.assertIn('compressed scalar layout remains unaccepted',
                                      self.probe(dict(op=op, path=str(path)), success=False))

    def test_review_raw_u16_multistrip_orientations(self):
        for endian in ('<', '>'):
            for rows in (1, 2):
                for orientation in range(1, 9):
                    path = self.root / f'strips-{ord(endian)}-{rows}-{orientation}.tiff'
                    path.write_bytes(raw_u16_tiff(endian=endian, rows=rows, orientation=orientation))
                    self.compare(path)

    def test_review_raw_u16_fillorder2_bits_and_orientations(self):
        path = self.root / 'literal-bits.tiff'
        path.write_bytes(raw_u16_tiff(width=1, height=1, values=[128], fill_order=2))
        self.compare(path)
        for rows in (1, 2):
            for orientation in range(1, 9):
                path = self.root / f'bits-{rows}-{orientation}.tiff'
                path.write_bytes(raw_u16_tiff(rows=rows, orientation=orientation, fill_order=2))
                self.compare(path)

    def test_unaccepted_fillorder_modes_are_explicitly_gated(self):
        for label, kwargs in [('big-endian', dict(endian='>')), ('compressed', dict(compression=8))]:
            path = self.root / f'bits-{label}.tiff'
            path.write_bytes(raw_u16_tiff(fill_order=2, **kwargs))
            for op in ('hash', 'hash_bytes'):
                with self.subTest(label=label, op=op):
                    self.assertIn('FillOrder mode remains unaccepted',
                                  self.probe(dict(op=op, path=str(path)), success=False))

    def test_icc_profile_gamma_alpha_invalid_and_incompatible(self):
        profiles = [('sRGB', srgb_profile()), ('gamma', gamma_profile()),
                    ('bad', b'invalid synthetic ICC'),
                    ('LAB', lab_profile())]
        for mode in ('RGB', 'RGBA', 'L', 'LA', 'P'):
            for label, profile in profiles:
                path = self.root / f'{mode}-{label}.png'
                kwargs = dict(icc_profile=profile)
                if mode == 'P':
                    kwargs['transparency'] = 1
                rgb_image(mode).save(path, **kwargs)
                self.compare(path)
        for mode in ('RGB', 'CMYK'):
            for label, profile in profiles + [('multipart-gamma', gamma_profile(True))]:
                path = self.root / f'{mode}-{label}.jpg'
                rgb_image(mode).save(path, icc_profile=profile)
                self.compare(path)

    def test_scalar_tiff_modes_orientations_compressions_path_and_bytes(self):
        for _, _, _, path in scalar_tiffs(self.root):
            self.compare(path)

    def test_jpeg_fragment_order_count_and_legacy_sequence_handling(self):
        output = io.BytesIO()
        rgb_image().save(output, format='JPEG', icc_profile=gamma_profile(True))
        for variant in ('reverse', 'zero-first-sequence', 'bad-total', 'missing'):
            path = self.root / f'fragments-{variant}.jpg'
            path.write_bytes(jpeg_icc_variant(output.getvalue(), variant))
            self.compare(path)

    def test_scalar_tiff_and_png_icc_clamp_before_colour_conversion(self):
        for mode in ('I;16', 'I;16B', 'I', 'F'):
            for label, profile in [('gamma', gamma_profile()), ('bad', b'invalid ICC')]:
                path = self.root / f'scalar-{mode}-{label}.tiff'
                scalar_image(mode).save(path, icc_profile=profile)
                self.compare(path)
        for label, profile in [('gamma', gamma_profile()), ('bad', b'invalid ICC')]:
            path = self.root / f'gray16-{label}.png'
            scalar_image('I;16').save(path, icc_profile=profile)
            self.compare(path)


if __name__ == '__main__':
    unittest.main()
