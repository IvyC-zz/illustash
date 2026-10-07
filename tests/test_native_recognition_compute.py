"""Explicit native computation comparison, synthetic images/vectors only; no model discovery."""
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

import numpy as np
from PIL import Image
from tests.image_expectations import wd_input
from tests.image_expectations import preprocess
from tests.vector_math import cross

FIXED_ORDER=json.loads((Path(__file__).resolve().parent/'fixtures/recognition_anchor_order.json').read_text())


class NativePreprocessTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        binary = os.environ.get('ILLUSTASH_RECOGNITION_COMPUTE_BIN')
        if not binary:
            raise unittest.SkipTest('set ILLUSTASH_RECOGNITION_COMPUTE_BIN explicitly; no build/model discovery')
        cls.binary = Path(binary).resolve(strict=True)

    def test_generated_modes_resize_padding_and_layout(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            for width, height in ((1, 1), (7, 4), (4, 7), (31, 25), (65, 23), (16, 16)):
                y, x = np.indices((height, width))
                rgb = np.stack(((x*71+y*13)%256, (x*5+y*107)%256, (x*19+y*37)%256), axis=2).astype('uint8')
                for mode in ('RGB', 'RGBA', 'L', 'P', '1', 'I;16'):
                    with self.subTest(size=(width,height), mode=mode):
                        image = Image.fromarray(rgb)
                        if mode == 'RGBA':
                            image = image.convert(mode)
                            image.putalpha(Image.fromarray(((x*41+y*59)%256).astype('uint8')))
                        elif mode == 'I;16':
                            image = Image.fromarray((x*307+y*17).astype('uint16'))
                        else:
                            image = image.convert(mode)
                        path = root/'image.png'
                        image.save(path)
                        self.compare(root, path, 16)
            for mode in ('RGB', 'CMYK'):
                with self.subTest(format='JPEG', mode=mode):
                    image = Image.fromarray(rgb).convert(mode)
                    image.save(root/'image.jpg')
                    self.compare(root, root/'image.jpg', 16)

    def compare(self, root, path, size):
        (root/'request.json').write_text(json.dumps(dict(mode='preprocess',image=str(path),size=size)))
        subprocess.run([str(self.binary),str(root/'request.json'),str(root/'output.json')],check=True,stdout=subprocess.PIPE,stderr=subprocess.PIPE)
        native=json.loads((root/'output.json').read_text())
        with Image.open(path) as image:
            expected_wd=wd_input(image,size).reshape(-1)
            expected_ccip=preprocess(image,size).reshape(-1)
        np.testing.assert_array_equal(np.asarray(native['wd'],np.float32),expected_wd)
        np.testing.assert_array_equal(np.asarray(native['ccip'],np.float32),expected_ccip)

    def test_bad_decode_is_an_error(self):
        with tempfile.TemporaryDirectory() as tmp:
            root=Path(tmp);(root/'bad.png').write_bytes(b'invalid')
            (root/'request.json').write_text(json.dumps(dict(mode='preprocess',image=str(root/'bad.png'))))
            result=subprocess.run([str(self.binary),str(root/'request.json'),str(root/'output.json')],capture_output=True)
            self.assertNotEqual(result.returncode,0)
            self.assertFalse((root/'output.json').exists())

    def test_anchor_distance_and_nearest_order(self):
        # Basis vectors make boundary distances exact; duplicated bases expose
        # NumPy's equal-distance partition order (the producer takes only 9).
        for count in (3, 9, 16, 17, 32, 257):
            with self.subTest(count=count), tempfile.TemporaryDirectory() as tmp:
                root=Path(tmp)
                vectors=np.zeros((count,768),np.float32)
                vectors[np.arange(count),np.arange(count)%3]=1
                features=np.eye(3,768,dtype=np.float32)
                request=dict(mode='anchors',vectors=vectors.tolist(),features=features.tolist())
                (root/'request.json').write_text(json.dumps(request))
                subprocess.run([str(self.binary),str(root/'request.json'),str(root/'output.json')],check=True,capture_output=True)
                native=json.loads((root/'output.json').read_text())
                distances=cross(vectors,features,check=False)
                np.testing.assert_array_equal(native['distances'],distances)
                for j,order in enumerate(native['orders']):
                    if os.name=='nt':
                        # Approved scalar tie order (2026-10-04); Windows NumPy's SIMD argsort
                        # orders equal distances differently, so use fixed macOS arm64 NumPy output.
                        self.assertEqual(order,FIXED_ORDER['orders'][str(count)][j])
                    else:
                        self.assertEqual(order,np.argsort(distances[:,j]).tolist())
                    if sys.platform=='darwin':  # keep the Windows fixture tied to its NumPy source
                        self.assertEqual(FIXED_ORDER['orders'][str(count)][j],np.argsort(distances[:,j]).tolist())

    def test_dense_norm_dot_and_claim_boundaries(self):
        dense=np.array([1]+[2**-13]*767,np.float32)
        rng=np.random.default_rng(149)
        random=rng.standard_normal((13,768)).astype(np.float32)
        vectors=np.vstack([dense,np.eye(1,768,dtype=np.float32)[0],random])
        features=np.vstack([np.eye(1,768,dtype=np.float32)[0],dense,random[:3]])
        with tempfile.TemporaryDirectory() as tmp:
            root=Path(tmp)
            (root/'request.json').write_text(json.dumps(dict(mode='anchors',vectors=vectors.tolist(),features=features.tolist())))
            subprocess.run([str(self.binary),str(root/'request.json'),str(root/'output.json')],check=True,capture_output=True)
            got=np.asarray(json.loads((root/'output.json').read_text())['distances'],np.float32)
            expected=cross(vectors,features,check=False)
            # No epsilon in the claim decision, including the actual reviewed p90.
            np.testing.assert_array_equal(got,expected)
            for threshold in (np.float32(1e-6),expected[0,0],np.nextafter(expected[0,0],np.float32(0))):
                np.testing.assert_array_equal(got<=threshold,expected<=threshold)

    def test_first_frame_other_formats_and_unrotated_exif(self):
        with tempfile.TemporaryDirectory() as tmp:
            root=Path(tmp)
            y,x=np.indices((23,31))
            rgb=np.stack(((x*17+y*11)%256,(x*7+y*31)%256,(x*59+y*13)%256),axis=2).astype('uint8')
            image=Image.fromarray(rgb)
            for extension in ('bmp','tiff','ppm','tga','webp','gif'):
                with self.subTest(extension=extension):
                    path=root/('image.'+extension)
                    image.save(path)
                    self.compare(root,path,16)
            for extension in ('gif','webp','png'):
                with self.subTest(animated=extension):
                    path=root/('animated.'+extension)
                    image.save(path,save_all=True,append_images=[Image.new('RGB',image.size,'white')],duration=100,loop=0)
                    self.compare(root,path,16)
            exif=Image.Exif();exif[274]=6
            image.save(root/'exif.jpg',exif=exif)
            self.compare(root,root/'exif.jpg',16)

    def test_explicit_missing_or_invalid_runtime_fails_without_output(self):
        with tempfile.TemporaryDirectory() as tmp:
            root=Path(tmp)
            for corrupt in (False, True):
                with self.subTest(corrupt=corrupt):
                    runtime=root/'synthetic-invalid-runtime.dylib'
                    if corrupt:
                        runtime.write_bytes(b'not a runtime')
                    request=dict(mode='infer',runtime=str(runtime),models=str(root/'no-models'),tags=str(root/'no-tags'),images=[])
                    (root/'request.json').write_text(json.dumps(request))
                    result=subprocess.run([str(self.binary),str(root/'request.json'),str(root/'output.json')],capture_output=True)
                    self.assertNotEqual(result.returncode,0)
                    self.assertIn(runtime.name,result.stderr.decode(errors='replace'))
                    self.assertFalse((root/'output.json').exists())
