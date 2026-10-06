"""共享路径的合成验收；不读取本机 review 或真实图库。"""
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

from web import library


class ResourceLayoutTest(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name).resolve()

    def test_shared_relative_absolute_default_and_legacy_ambiguity(self):
        cwd = self.root / 'cwd'
        resource = cwd / 'resources'
        resource.mkdir(parents=True)
        self.assertEqual(library.shared_root('resources', cwd=cwd), resource / 'review')
        self.assertEqual(library.shared_root(resource, 'shared', cwd=cwd), resource / 'shared')
        old = cwd / 'shared'
        old.mkdir()
        self.assertEqual(library.shared_root(resource, 'shared', cwd=cwd), resource / 'shared')
        (old / 'char-index').mkdir()
        with self.assertRaisesRegex(library.LibraryError, '绝对路径'):
            library.shared_root(resource, 'shared', cwd=cwd)
        self.assertFalse((resource / 'shared').exists())
        self.assertEqual(library.shared_root(resource, old, cwd=cwd), old)
        self.assertEqual(library.shared_root(cwd, 'shared', cwd=cwd), old)
        if os.name != 'nt':
            (resource / 'shared').symlink_to(old, target_is_directory=True)
            self.assertEqual(library.shared_root(resource, 'shared', cwd=cwd), old)
            (resource / 'shared').unlink()
            (old / 'char-index').rmdir()
            (old / 'char-index').symlink_to(old / 'missing')
            with self.assertRaises(library.LibraryError):
                library.shared_root(resource, 'shared', cwd=cwd)

    def test_python_worker_import_uses_resource_root_from_different_cwd(self):
        resource = self.root / 'resources'
        cwd = self.root / 'worker cwd'
        cwd.mkdir()
        env = dict(os.environ, PYTHONPATH=str(library.ROOT), ILLUSTASH_ROOT=str(resource), ILLUSTASH_SHARED='shared')
        result = subprocess.run([sys.executable, '-c', 'from web.library import SHARED; print(SHARED)'],
                                cwd=cwd, env=env, text=True, capture_output=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.strip(), str(resource / 'shared'))
