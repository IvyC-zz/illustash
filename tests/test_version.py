"""tools/version.py：application.json 是版本号的唯一来源（#189）。"""
import contextlib
import importlib.util
import io
import json
from pathlib import Path
import shutil
import tempfile
import tomllib
import unittest
from unittest import mock

from tools import version

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location('illustash_build', ROOT / 'packaging/build.py')
build = importlib.util.module_from_spec(spec)
spec.loader.exec_module(build)


class VersionTests(unittest.TestCase):
    def setUp(self):
        self.root = Path(tempfile.mkdtemp(prefix='illustash-version-'))
        self.addCleanup(shutil.rmtree, self.root)
        files = [version.SOURCE, version.TAURI, version.LOCK]
        for crate in version.CRATES:
            files.append(f'{crate}/Cargo.toml')
        for relative in files:
            (self.root / relative).parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(ROOT / relative, self.root / relative)

    def edit(self, relative, old, new):
        # 按字节读写，保持文件原有的 LF：write_text 在 Windows 上会把 \n 写成 \r\n（#322）。
        path = self.root / relative
        text = path.read_bytes().decode('utf-8')
        self.assertIn(old, text)
        path.write_bytes(text.replace(old, new, 1).encode('utf-8'))

    def test_repository_is_consistent(self):
        self.assertEqual(version.check(ROOT), [])
        self.assertTrue(version.valid(version.source_version(ROOT)))

    def test_every_shipped_crate_and_lock_is_covered(self):
        paths = {path for path, _, _ in version.places(ROOT)}
        self.assertIn(version.TAURI, paths)
        for crate in version.CRATES:
            self.assertIn(f'{crate}/Cargo.toml', paths)
        self.assertIn('Cargo.lock', paths)
        # 根 lock 列出全部本项目 crate，从不列 registry crate。
        names = {(path, name) for path, name, _ in version.places(ROOT) if path.endswith('.lock')}
        self.assertIn(('Cargo.lock', 'illustash-terms'), names)
        self.assertIn(('Cargo.lock', 'illustash-iqdb'), names)
        self.assertIn(('Cargo.lock', 'illustash-relation-fft'), names)
        self.assertTrue(all(name.startswith('illustash-') for _, name in names))

    def test_set_rewrites_every_place_and_keeps_files_valid(self):
        before = {p: (self.root / p).read_bytes() for p, _, _ in version.places(self.root)}
        self.assertEqual(version.sync(self.root, '0.2.0-beta.1'), '0.2.0-beta.1')
        self.assertEqual(version.check(self.root), [])
        self.assertEqual({found for _, _, found in version.places(self.root)}, {'0.2.0-beta.1'})
        metadata = json.loads((self.root / version.SOURCE).read_text(encoding='utf-8'))
        self.assertEqual(metadata, {'name': 'Illustash', 'version': '0.2.0-beta.1', 'channel': '预览版'})
        self.assertEqual(json.loads((self.root / version.TAURI).read_text())['version'], '0.2.0-beta.1')
        for crate in version.CRATES:
            manifest = tomllib.loads((self.root / crate / 'Cargo.toml').read_text())
            self.assertEqual(manifest['package']['version'], '0.2.0-beta.1')
        lock = tomllib.loads((self.root / 'Cargo.lock').read_text())
        for package in lock['package']:
            if package['name'].startswith('illustash-'):
                self.assertEqual(package['version'], '0.2.0-beta.1')
                self.assertNotIn('source', package)
        # 只改了本项目的版本字符串：每处一个，别的不动（恰好也是 0.1.0 的 registry crate 仍是 0.1.0）。
        counts = {}
        for path, _, _ in version.places(self.root):
            counts[path] = counts.get(path, 0) + 1
        for path, old in before.items():
            new = (self.root / path).read_bytes()
            self.assertEqual(new.count(b'0.2.0-beta.1'), counts[path] + (path == version.SOURCE), path)
            self.assertEqual(new.replace(b'0.2.0-beta.1', b'0.1.0'), old, path)

    def test_sync_restores_a_hand_edited_place(self):
        self.edit('crates/core/Cargo.toml', 'version = "0.1.0"', 'version = "0.1.1"')
        self.edit('Cargo.lock', 'name = "illustash-terms"\nversion = "0.1.0"', 'name = "illustash-terms"\nversion = "9.9.9"')
        self.assertEqual(version.sync(self.root), '0.1.0')
        self.assertEqual(version.check(self.root), [])
        for path, _, _ in version.places(self.root):
            self.assertEqual((self.root / path).read_bytes(), (ROOT / path).read_bytes(), path)

    def test_check_names_each_mismatch(self):
        self.edit(version.TAURI, '"version": "0.1.0"', '"version": "0.1.1"')
        self.edit('crates/iqdb/Cargo.toml', 'version = "0.1.0"', 'version = "0.0.9"')
        self.edit('Cargo.lock', 'name = "illustash-core"\nversion = "0.1.0"', 'name = "illustash-core"\nversion = "0.1.2"')
        errors = version.check(self.root)
        self.assertEqual(len(errors), 3, errors)
        self.assertTrue(errors[0].startswith('desktop/tauri.conf.json: version 是 0.1.1'))
        self.assertTrue(any(e.startswith('Cargo.lock: illustash-core 是 0.1.2') for e in errors))
        self.assertTrue(any(e.startswith('crates/iqdb/Cargo.toml: [package] version 是 0.0.9') for e in errors))

    def test_dependency_version_is_not_the_package_version(self):
        # 依赖表里的 `version = …` 不能顶替缺失的 [package] version。
        self.edit('crates/core/Cargo.toml', 'version = "0.1.0"\n', '')
        errors = version.check(self.root)
        self.assertIn('crates/core/Cargo.toml: 找不到 [package] version', errors)
        with self.assertRaisesRegex(ValueError, 'crates/core/Cargo.toml'):
            version.sync(self.root)

    def test_invalid_versions_are_refused(self):
        for bad in ('0.1', 'v0.1.0', '01.0.0', '0.1.0+build.5', '0.1.0-', '0.1.0-01', '0.1.0 '):
            self.assertFalse(version.valid(bad), bad)
            with self.assertRaises(ValueError):
                version.sync(self.root, bad)
        for good in ('0.1.0', '1.10.0', '0.2.0-beta.1', '0.2.0-rc.0', '0.2.0-alpha-2'):
            self.assertTrue(version.valid(good), good)
        self.assertEqual(version.check(self.root), [])  # 被拒绝的版本设置没有改动任何东西
        self.edit(version.SOURCE, '"version":"0.1.0"', '"version":"0.1"')
        self.assertEqual(len(version.check(self.root)), 1)
        self.assertIn('不是 SemVer', version.check(self.root)[0])

    def test_packaging_refuses_mismatched_versions_before_building(self):
        self.edit('desktop/Cargo.toml', 'version = "0.1.0"', 'version = "0.1.1"')
        self.assertEqual(build.version_errors(self.root), version.check(self.root))
        self.assertEqual(len(build.version_errors(self.root)), 1)
        stderr = io.StringIO()
        output = self.root / 'out'
        with mock.patch.object(build, 'version_errors', lambda: version.check(self.root)), \
                mock.patch.object(build.subprocess, 'run') as run, \
                mock.patch('sys.argv', ['build.py', '--output', str(output)]), \
                contextlib.redirect_stderr(stderr), self.assertRaises(SystemExit) as raised:
            build.main()
        self.assertEqual(raised.exception.code, 2)
        self.assertIn('版本号不一致', stderr.getvalue())
        self.assertIn('desktop/Cargo.toml: [package] version 是 0.1.1', stderr.getvalue())
        run.assert_not_called()
        self.assertFalse(output.exists())


if __name__ == '__main__':
    unittest.main()
