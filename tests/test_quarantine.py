"""隔离名单的 Rust 入口（#501）：用合成的 cargo 核对跳过参数、单独运行和退出码，不编译真实项目。"""
import json
import os
from pathlib import Path
import sys
import tempfile
import unittest
from unittest import mock

from tools import quarantine

# 合成 cargo：--list 列出两项；--exact 单名时只跑那一项（flaky 失败）；其余为常规运行，记下参数。
FAKE = r'''
import json, sys
from pathlib import Path
args = sys.argv[sys.argv.index('--') + 1:] if '--' in sys.argv else []
log = Path(sys.argv[0]).with_suffix('.calls')
with log.open('a') as calls: calls.write(json.dumps(args) + '\n')
if args == ['--list']:
    print('app::tests::steady: test\napp::tests::flaky: test\n2 tests, 0 benchmarks')
elif len(args) == 2 and args[0] == '--exact':
    failed = args[1] == 'app::tests::flaky'
    print(f'test result: {"FAILED" if failed else "ok"}. {0 if failed else 1} passed; {1 if failed else 0} failed; 0 ignored')
    sys.exit(101 if failed else 0)
else:
    mode = Path(sys.argv[0]).with_suffix('.mode').read_text()
    print(f'test result: {"FAILED" if mode == "fail" else "ok"}. 1 passed; {1 if mode == "fail" else 0} failed;')
    sys.exit(101 if mode == 'fail' else 0)
'''


class CargoQuarantineTest(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.fake = self.root / 'cargo.py'
        self.fake.write_text(FAKE)
        self.fake.with_suffix('.mode').write_text('ok')
        self.list = self.root / 'quarantine.json'
        patches = [mock.patch.object(quarantine, 'CARGO', [sys.executable, str(self.fake)]),
                   mock.patch.dict(os.environ, {'ILLUSTASH_QUARANTINE': str(self.list),
                                                'ILLUSTASH_QUARANTINE_TODAY': '2026-11-20'})]
        for patch in patches:
            patch.start()
            self.addCleanup(patch.stop)
        os.environ.pop('GITHUB_STEP_SUMMARY', None)

    def entries(self, *names, platforms=None):
        self.list.write_text(json.dumps({'entries': [
            {'kind': 'rust', 'crate': 'desktop', 'id': name, 'platforms': platforms or [quarantine.platform()],
             'issue': 1, 'added': '2026-11-15', 'expires': '2026-11-29'} for name in names]}))

    def calls(self):
        path = self.fake.with_suffix('.calls')
        return [json.loads(line) for line in path.read_text().splitlines()] if path.exists() else []

    def run_cargo(self):
        with mock.patch('sys.stdout') as stdout, mock.patch('sys.stderr') as stderr:
            status = quarantine.cargo_test(str(quarantine.ROOT / 'desktop/Cargo.toml'), [])
        text = ''.join(str(call.args[0]) for call in stdout.write.call_args_list + stderr.write.call_args_list)
        return status, text

    def test_quarantined_test_is_skipped_then_run_alone_and_only_reported(self):
        self.entries('app::tests::flaky')
        status, text = self.run_cargo()
        self.assertEqual(status, 0)
        self.assertEqual(self.calls(), [['--list'], ['--exact', '--skip', 'app::tests::flaky'],
                                        ['--exact', 'app::tests::flaky']])
        self.assertIn('隔离名单中的失败（不计入结果）', text)
        self.assertIn('app::tests::flaky（#1', text)

    def test_ordinary_failure_still_fails(self):
        self.entries('app::tests::flaky')
        self.fake.with_suffix('.mode').write_text('fail')
        self.assertNotEqual(self.run_cargo()[0], 0)

    def test_missing_expired_and_other_platform_entries(self):
        self.entries('app::tests::gone')
        status, text = self.run_cargo()
        self.assertEqual(status, 1)
        self.assertIn('找不到', text)
        # 别的平台的条目在这里不核对、不跳过：测试可能只在那个平台上编译，这里照常参与常规运行。
        self.fake.with_suffix('.calls').unlink()
        other = 'windows' if quarantine.platform() == 'macos' else 'macos'
        self.entries('app::tests::gone', platforms=[other])
        self.assertEqual(self.run_cargo()[0], 0)
        self.assertEqual(self.calls(), [[]])
        self.list.write_text(self.list.read_text().replace('2026-11-29', '2026-11-19'))
        with self.assertRaises(quarantine.QuarantineError):
            quarantine.load()

    def test_crate_without_entries_runs_cargo_test_unchanged(self):
        self.list.write_text(json.dumps({'entries': []}))
        self.assertEqual(self.run_cargo()[0], 0)
        self.assertEqual(self.calls(), [[]])


if __name__ == '__main__':
    unittest.main()
