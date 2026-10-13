"""完整测试入口的并行调度：用合成 tests 包验证汇总、跳过行、退役导入屏蔽与子进程失败。"""
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile
import unittest

RUNNER = Path(__file__).with_name('run_fixed_suite.py')

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
        self.write('test_alpha.py', PASSING)

    def tearDown(self):
        self.temporary.cleanup()

    def write(self, name, text):
        path = self.root / 'tests' / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text)

    def retired_package(self):
        (self.root / 'web').mkdir()
        (self.root / 'web/__init__.py').write_text('')

    def run_suite(self, *args):
        return subprocess.run([sys.executable, '-m', 'tests.run_fixed_suite', '-v', '-b', *args], cwd=self.root,
                              capture_output=True, text=True, timeout=120)

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

    def test_parallel_mode_rejects_unsupported_arguments(self):
        run = self.run_suite('--jobs', '2', '-k', 'one')
        self.assertNotEqual(run.returncode, 0)
        self.assertIn('--jobs 1', run.stderr)


if __name__ == '__main__':
    unittest.main()
