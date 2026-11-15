"""完整测试入口的并行调度：用合成 tests 包验证汇总、跳过行、退役导入屏蔽与子进程失败。"""
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile
import unittest
from tools.wait_limit import WAIT

from tools import quarantine

RUNNER = Path(__file__).with_name('run_fixed_suite.py')
QUARANTINE = Path(quarantine.__file__)
LAUNCH = ("import runpy,sys;sys.path.insert(0,sys.argv.pop(1));"
          "runpy.run_module('tests.run_fixed_suite',run_name='__main__',alter_sys=True)")

PASSING = '''import unittest
class Alpha(unittest.TestCase):
    def test_one(self): pass
    def test_two(self): pass
    @unittest.skip('POSIX only synthetic')
    def test_skipped(self): pass
class Beta(unittest.TestCase):
    def test_three(self): pass
'''


class FixedSuiteRunnerTest(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name)
        (self.root / 'tests').mkdir()
        (self.root / 'tests/__init__.py').write_text('')
        shutil.copy(RUNNER, self.root / 'tests/run_fixed_suite.py')
        (self.root / 'tools').mkdir()
        (self.root / 'tools/__init__.py').write_text('')
        shutil.copy(QUARANTINE, self.root / 'tools/quarantine.py')
        self.isolate()
        self.write('test_alpha.py', PASSING)

    def tearDown(self):
        self.temporary.cleanup()

    def write(self, name, text):
        path = self.root / 'tests' / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text)

    def isolate(self, *entries):
        self.write('quarantine.json', json.dumps({'entries': list(entries)}))

    def entry(self, test_id, platforms=None, expires='2026-11-29'):
        return {'kind': 'python', 'id': test_id, 'platforms': platforms or [quarantine.platform()], 'issue': 1,
                'added': '2026-11-15', 'expires': expires}

    def retired_package(self):
        (self.root / 'web').mkdir()
        (self.root / 'web/__init__.py').write_text('')

    def run_suite(self, *args, flags=()):
        # 按 Windows 验收机的方式启动：自行插入仓库根后 runpy，不依赖 `-m` 把 cwd 放进 sys.path（嵌入式 Python 不放）。
        # 沿用本进程的解释器选项并固定 -X utf8，输出按 UTF-8 解码，与本进程的编码设置无关。
        command = [sys.executable, *subprocess._args_from_interpreter_flags(), '-X', 'utf8', *flags,
                   '-c', LAUNCH, str(self.root), '-v', '-b', *args]
        environment = {key: value for key, value in os.environ.items() if key != 'GITHUB_STEP_SUMMARY'}
        environment['ILLUSTASH_QUARANTINE_TODAY'] = '2026-11-20'
        return subprocess.run(command, cwd=self.root, capture_output=True, text=True, encoding='utf-8', timeout=WAIT * 2,
                              env=environment)

    def test_parallel_summary_matches_serial_counts_and_keeps_skip_lines(self):
        for jobs in ('1', '3'):
            with self.subTest(jobs=jobs):
                run = self.run_suite('--jobs', jobs)
                self.assertEqual(run.returncode, 0, run.stderr)
                self.assertIn('Ran 4 tests in ', run.stderr)
                self.assertIn('OK (skipped=1)', run.stderr)
                skipped = [line for line in run.stderr.splitlines() if ' ... skipped ' in line]
                self.assertEqual(len(skipped), 1)
                self.assertIn("'POSIX only synthetic'", skipped[0])
                self.assertIn('退役参考导入尝试：0', run.stdout)

    def test_parallel_output_is_not_interleaved_and_failures_set_exit_status(self):
        self.write('test_gamma.py', 'import unittest\nclass Gamma(unittest.TestCase):\n'
                                    '    def test_fails(self):\n        print("captured"); self.fail("synthetic")\n'
                                    '    def test_errors(self): raise RuntimeError("boom")\n')
        run = self.run_suite('--jobs', '2')
        self.assertEqual(run.returncode, 1)
        self.assertIn('FAILED (failures=1, errors=1, skipped=1)', run.stderr)
        self.assertIn('Ran 6 tests in ', run.stderr)
        # 每个测试类的逐项结果与失败详情在其标题之后连续出现。
        blocks = re.split(r'(?m)^(?=\[\d+/\d+\] )', run.stderr)
        gamma, = [block for block in blocks if block.startswith(('[1/3] test_gamma.Gamma', '[2/3] test_gamma.Gamma',
                                                                  '[3/3] test_gamma.Gamma'))]
        for text in ('test_fails', 'test_errors', 'FAIL: test_fails', 'ERROR: test_errors', 'captured'):
            self.assertIn(text, gamma)
        self.assertNotIn('test_alpha', gamma)

    def test_retired_import_inside_worker_fails_the_run(self):
        self.retired_package()
        self.write('test_retired.py', 'import unittest\nclass Retired(unittest.TestCase):\n'
                                      '    def test_import(self):\n        import web\n')
        run = self.run_suite('--jobs', '2')
        self.assertEqual(run.returncode, 1)
        self.assertIn('已退役的参考模块不可导入：web', run.stderr)
        self.assertIn('退役参考导入尝试：1', run.stdout)

    def test_retired_import_at_discovery_fails_the_run(self):
        self.retired_package()
        self.write('test_retired.py', 'import web\n')
        run = self.run_suite('--jobs', '2')
        self.assertEqual(run.returncode, 1)
        self.assertIn('errors=1', run.stderr)
        self.assertIn('退役参考导入尝试：1', run.stdout)

    def test_worker_crash_is_reported_and_fails_the_run(self):
        self.write('test_crash.py', 'import os, unittest\nclass Crash(unittest.TestCase):\n'
                                    '    def test_exit(self): os._exit(3)\n')
        run = self.run_suite('--jobs', '2')
        self.assertEqual(run.returncode, 1)
        self.assertIn('并行执行失败：test_crash.Crash：子进程退出码 3', run.stderr)
        self.assertIn('未运行的测试 1 项：test_crash.Crash.test_exit', run.stderr)

    def test_class_with_rewritten_module_is_loaded_from_the_module_that_holds_it(self):
        self.write('test_alias.py', 'import unittest\nclass Alias(unittest.TestCase):\n'
                                    "    __module__ = 'test_alpha'\n    def test_alias(self): pass\n")
        run = self.run_suite('--jobs', '2')
        self.assertEqual(run.returncode, 0, run.stderr)
        self.assertIn('Ran 5 tests in ', run.stderr)
        self.assertIn('test_alias (test_alpha.Alias.test_alias) ... ok', run.stderr)

    def test_serial_classes_run_alone_after_the_pool(self):
        runner = self.root / 'tests/run_fixed_suite.py'
        source = runner.read_text()
        start = source.index('SERIAL=frozenset(')
        runner.write_text(source[:start] + "SERIAL=frozenset({'test_alpha.Beta','test_missing.Gone'})" +
                          source[source.index('\n', start):])
        run = self.run_suite('--jobs', '3')
        self.assertEqual(run.returncode, 0, run.stderr)
        self.assertIn('串行名单中的测试类未被发现：test_missing.Gone', run.stderr)
        headers = re.findall(r'(?m)^\[\d+/2\] (\S+)（', run.stderr)
        self.assertEqual(headers, ['test_alpha.Alpha', 'test_alpha.Beta'])
        self.assertRegex(run.stderr, r'\[2/2\] test_alpha\.Beta（1 项，[0-9.]+s，单独运行）')
        self.assertIn('Ran 4 tests in ', run.stderr)

    def test_parallel_workers_do_not_rely_on_cwd_in_sys_path(self):
        # 模拟带 ._pth 的嵌入式 Python：-I 不把 cwd 或脚本目录放进 sys.path，也忽略 PYTHONPATH。
        # 子进程须沿用父进程的 -I/-X utf8，并在隔离下自行找到 tests 包。
        self.write('test_isolated.py', 'import sys, unittest\nclass Isolated(unittest.TestCase):\n'
                                       '    def test_flags(self):\n'
                                       '        self.assertTrue(sys.flags.isolated)\n'
                                       '        self.assertTrue(sys.flags.utf8_mode)\n'
                                       "        self.assertNotIn('', sys.path)\n")
        run = self.run_suite('--jobs', '2', flags=('-I',))
        self.assertEqual(run.returncode, 0, run.stderr)
        self.assertNotIn('ModuleNotFoundError', run.stderr)
        self.assertIn('Ran 5 tests in ', run.stderr)
        self.assertIn('test_flags (test_isolated.Isolated.test_flags) ... ok', run.stderr)
        self.assertIn('退役参考导入尝试：0', run.stdout)

    def test_parallel_mode_rejects_unsupported_arguments(self):
        run = self.run_suite('--jobs', '2', '-k', 'one')
        self.assertNotEqual(run.returncode, 0)
        self.assertIn('--jobs 1', run.stderr)

    def test_select_runs_only_matching_classes_and_checks_them_for_omissions(self):
        # #501：定向复跑只跑选中的类和方法；前缀按「.」边界匹配，Alpha 不会选中 AlphaBeta。
        self.write('test_gamma.py', 'import unittest\nclass AlphaBeta(unittest.TestCase):\n    def test_four(self): pass\n')
        run = self.run_suite('--jobs', '2', '--select', 'test_alpha.Alpha', '--select=test_alpha.Beta.test_three')
        self.assertEqual(run.returncode, 0, run.stderr)
        self.assertIn('定向选择：test_alpha.Alpha, test_alpha.Beta.test_three；选中 4 项', run.stderr)
        self.assertIn('Ran 4 tests in ', run.stderr)
        self.assertNotIn('test_four', run.stderr)

    def test_select_that_matches_nothing_fails_and_import_errors_still_count(self):
        run = self.run_suite('--jobs', '2', '--select', 'test_alpha.Alpha', '--select', 'test_alpha.Missing')
        self.assertNotEqual(run.returncode, 0)
        self.assertIn('定向选择没有匹配任何测试：test_alpha.Missing', run.stderr)
        # 导入失败的测试文件不因不在选择范围内而被略过。
        self.write('test_broken.py', 'raise ImportError("synthetic broken module")\n')
        run = self.run_suite('--jobs', '2', '--select', 'test_alpha.Alpha')
        self.assertNotEqual(run.returncode, 0)
        self.assertIn('synthetic broken module', run.stderr)
        for args in (('--jobs', '1', '--select', 'test_alpha.Alpha'), ('--jobs', '2', '--select=')):
            with self.subTest(args=args):
                self.assertNotEqual(self.run_suite(*args).returncode, 0)


    def test_quarantined_failures_run_but_do_not_fail_and_are_listed(self):
        # #501：名单里的测试照跑；本平台的失败（含 subTest）不计入结果，单列一节。
        self.write('test_flaky.py', 'import unittest\nclass Flaky(unittest.TestCase):\n'
                                    '    def test_sometimes(self):\n        for i in range(2):\n'
                                    '            with self.subTest(i=i): self.fail("synthetic flake")\n'
                                    '    def test_steady(self): pass\n')
        self.isolate(self.entry('test_flaky.Flaky.test_sometimes'))
        for jobs in ('1', '2'):
            with self.subTest(jobs=jobs):
                run = self.run_suite('--jobs', jobs)
                self.assertEqual(run.returncode, 0, run.stdout + run.stderr)
                self.assertIn('隔离名单中的失败（不计入结果）', run.stdout)
                self.assertIn('test_flaky.Flaky.test_sometimes（#1', run.stdout)
                self.assertIn('2 处失败', run.stdout)
                self.assertIn('synthetic flake', run.stderr)
                # 定向复现时照样计入。
                self.assertNotEqual(self.run_suite('--jobs', jobs, '--count-quarantined').returncode, 0)

    def test_quarantine_does_not_cover_other_platforms_or_other_tests(self):
        self.write('test_flaky.py', 'import unittest\nclass Flaky(unittest.TestCase):\n'
                                    '    def test_sometimes(self): self.fail("synthetic flake")\n'
                                    '    def test_other(self): self.fail("real failure")\n')
        other = 'windows' if quarantine.platform() == 'macos' else 'macos'
        self.isolate(self.entry('test_flaky.Flaky.test_sometimes', [other]))
        self.assertNotEqual(self.run_suite('--jobs', '2').returncode, 0)
        self.isolate(self.entry('test_flaky.Flaky.test_sometimes'))
        run = self.run_suite('--jobs', '2')
        self.assertNotEqual(run.returncode, 0)
        self.assertIn('real failure', run.stderr)

    def test_expired_missing_or_malformed_entries_fail_before_running(self):
        cases = [(self.entry('test_alpha.Alpha.test_one', expires='2026-11-19'), '已于 2026-11-19 到期'),
                 (self.entry('test_alpha.Alpha.test_gone'), '找不到'),
                 ({**self.entry('test_alpha.Alpha.test_one'), 'issue': 0}, 'issue 须是正整数'),
                 ({**self.entry('test_alpha.Alpha.test_one'), 'platforms': ['linux']}, 'platforms')]
        for entry, message in cases:
            for jobs in ('1', '2'):
                with self.subTest(message=message, jobs=jobs):
                    self.isolate(entry)
                    run = self.run_suite('--jobs', jobs)
                    self.assertNotEqual(run.returncode, 0)
                    self.assertIn(message, run.stderr)
                    self.assertNotIn('test_one (test_alpha.Alpha', run.stderr)


if __name__ == '__main__':
    unittest.main()
