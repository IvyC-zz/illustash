"""Windows narrow BLAS parity using only fixed synthetic matrices and an explicit candidate.

The reference runs in the explicitly selected existing NumPy environment. No binary builds,
DLL discovery, real models, user installations or libraries are performed by this suite.
"""
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

import numpy as np


@unittest.skipUnless(sys.platform == 'win32', 'Windows pinned OpenBLAS backend only')
class WindowsResearchMathTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        configured = os.environ.get('ILLUSTASH_RESEARCH_COMPUTE_BIN')
        dll = os.environ.get('ILLUSTASH_RESEARCH_OPENBLAS_DLL')
        if not configured or not dll:
            raise unittest.SkipTest('explicit candidate math example and matching reference DLL required')
        cls.binary = Path(configured).resolve()
        if not cls.binary.is_file() or not Path(dll).is_file():
            raise AssertionError('configured candidate or matching reference DLL missing')

    def probe(self, cases, *, env=None):
        with tempfile.TemporaryDirectory(prefix='illustash-math-synthetic-') as temporary:
            request = Path(temporary) / 'matrix.json'
            request.write_text(json.dumps(cases), encoding='utf-8')
            # The candidate cannot search PATH for DLLs or delegate numerical work to Python.
            return subprocess.run([str(self.binary), '--math', str(request)], env=env or (os.environ | {'PATH': ''}),
                                  capture_output=True, timeout=30)

    @staticmethod
    def case(a, b, same=False):
        return dict(dtype='f64' if a.dtype == np.float64 else 'f32', a=a.ravel().tolist(), b=[] if same else b.ravel().tolist(), same=same,
                    m=a.shape[0], p=b.shape[0], dim=a.shape[1])

    def test_numpy_float64_shape_dispatch_exact_including_same_buffer_syrk(self):
        self.check_shape_dispatch(np.float64, 'u8')

    def test_numpy_shape_dispatch_exact_including_same_buffer_syrk(self):
        self.check_shape_dispatch(np.float32, 'u4')

    def check_shape_dispatch(self, dtype, bits):
        rng = np.random.default_rng(151104)
        cases, expected = [], []
        for dim in (1, 7, 59, 65, 768):
            for m, p in ((1, 1), (1, 6), (6, 1), (6, 7), (7, 6), (6, 6)):
                a = rng.normal(size=(m, dim)).astype(dtype)
                b = rng.normal(size=(p, dim)).astype(dtype)
                cases.append(self.case(a, b))
                expected.append((a @ b.T).ravel().view(bits).tolist())
            a = rng.normal(size=(6, dim)).astype(dtype)
            cases.append(self.case(a, a, same=True))
            expected.append((a @ a.T).ravel().view(bits).tolist())
        result = self.probe(cases)
        self.assertEqual(result.returncode, 0, result.stderr.decode(errors='replace'))
        actual = json.loads(result.stdout)
        self.assertIn('USE64BITINT', actual['identity'])
        self.assertEqual(actual['bits'], expected)

    def test_numpy_large_shapes_exact_across_threaded_blas_dispatch(self):
        # Large GEMM/SYRK inputs exercise OpenBLAS's threaded path when the test
        # environment selects multiple threads; each process uses the same fixed
        # DLL and CPU/thread configuration as its independent NumPy reference.
        rng = np.random.default_rng(151151)
        for dtype, bits in ((np.float32, 'u4'), (np.float64, 'u8')):
            cases, expected = [], []
            for m, p, dim, same in ((1, 257, 768, False), (257, 1, 768, False),
                                    (129, 257, 59, False), (257, 129, 768, False),
                                    (129, 129, 768, True)):
                a = rng.normal(size=(m, dim)).astype(dtype)
                b = a if same else rng.normal(size=(p, dim)).astype(dtype)
                cases.append(self.case(a, b, same=same))
                expected.append((a @ b.T).ravel().view(bits).tolist())
            result = self.probe(cases)
            self.assertEqual(result.returncode, 0, result.stderr.decode(errors='replace'))
            actual = json.loads(result.stdout)
            self.assertIn('USE64BITINT', actual['identity'])
            self.assertEqual(actual['bits'], expected)

    def test_installed_root_default_and_explicit_override_share_fixed_identity(self):
        import shutil
        with tempfile.TemporaryDirectory(prefix='illustash-installed-openblas-') as temporary:
            root = Path(temporary).resolve() / 'relocated resources with spaces'
            library = root / 'openblas/openblas.dll'
            library.parent.mkdir(parents=True)
            shutil.copy2(Path(os.environ['ILLUSTASH_RESEARCH_OPENBLAS_DLL']), library)
            env = os.environ | {'PATH': '', 'ILLUSTASH_ROOT': str(root)}
            env.pop('ILLUSTASH_RESEARCH_OPENBLAS_DLL', None)
            a = np.array([[1., 2., 3.], [4., 5., 6.]], dtype=np.float32)
            result = self.probe([self.case(a, a, same=True)], env=env)
            self.assertEqual(result.returncode, 0, result.stderr.decode(errors='replace'))
            self.assertEqual(json.loads(result.stdout)['bits'], [(a @ a.T).ravel().view('u4').tolist()])
            # An explicitly bad override must not silently switch to the good installed resource.
            result = self.probe([self.case(a, a)], env=env | {'ILLUSTASH_RESEARCH_OPENBLAS_DLL': 'relative.dll'})
            self.assertNotEqual(result.returncode, 0)
            self.assertIn(b'absolute DLL path', result.stderr)
            library.unlink()
            result = self.probe([self.case(a, a)], env=env)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn(b'absolute DLL path', result.stderr)

    def test_missing_relative_unpinned_dll_fail_without_portable_fallback(self):
        a = np.array([[1, 2]], dtype=np.float32)
        cases = [self.case(a, a)]
        env = os.environ | {'PATH': ''}
        env.pop('ILLUSTASH_RESEARCH_OPENBLAS_DLL', None)
        env.pop('ILLUSTASH_ROOT', None)
        result = self.probe(cases, env=env)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn(b'ILLUSTASH_RESEARCH_OPENBLAS_DLL', result.stderr)
        result = self.probe(cases, env=env | {'ILLUSTASH_RESEARCH_OPENBLAS_DLL': 'relative.dll'})
        self.assertNotEqual(result.returncode, 0)
        self.assertIn(b'absolute DLL path', result.stderr)
        with tempfile.TemporaryDirectory(prefix='illustash-unpinned-dll-') as temporary:
            dll = Path(temporary) / 'not-openblas.dll'
            dll.write_bytes(b'owned synthetic invalid DLL; must reject before LoadLibraryExW')
            result = self.probe(cases, env=env | {'ILLUSTASH_RESEARCH_OPENBLAS_DLL': str(dll)})
            self.assertNotEqual(result.returncode, 0)
            self.assertIn(b'fingerprint', result.stderr)


from tests import test_native_research_compute as compute
from tests import test_native_research_production as production


@unittest.skipUnless(sys.platform == 'win32', 'explicit Windows native development calculator only')
class WindowsNativeResearchComputeTest(compute.NativeResearchComputeTest):
    """Algorithm-only comparison through the example; installed routing is tested separately."""



@unittest.skipUnless(sys.platform == 'win32', 'explicit Windows development App publication only')
class WindowsNativeResearchPublicationTest(production.NativeResearchPublicationTest):

    @classmethod
    def setUpClass(cls):
        super().setUpClass()
        configured = os.environ.get('ILLUSTASH_RESEARCH_COMPUTE_BIN')
        if not configured:
            raise unittest.SkipTest('explicit candidate research example required')
        cls.candidate_worker = Path(configured).resolve()
        if not cls.candidate_worker.is_file():
            raise AssertionError('candidate development calculator missing')
        # Existing #146 timing fixtures run the actual native example response.
        # This verifies App publication; it is explicitly NOT native installed routing.
