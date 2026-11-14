"""CI 分流与汇总的回归：直接运行 workflow 中的 shell，不启动构建或访问网络。"""
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[1]


def job_script(job):
    """提取指定 job 的最后一个多行 run 块，保持测试与实际执行的脚本一致。"""
    source = (ROOT / '.github/workflows/ci.yml').read_text(encoding='utf-8')
    body = re.search(rf'^  {job}:\n(.*?)(?=^  \w+:|\Z)', source, re.M | re.S).group(1)
    blocks = re.findall(r'^        run: \|\n((?:^          .*\n|^\n)+)', body, re.M)
    return ''.join(line[10:] if line.startswith('          ') else line
                   for line in blocks[-1].splitlines(keepends=True))


@unittest.skipUnless(os.name == 'posix' and shutil.which('bash') and shutil.which('git') and shutil.which('jq'),
                     'POSIX CI shell with git and jq only')
class CiWorkflowTest(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix='illustash-ci-')
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.output = self.root / 'output'

    def shell(self, script, **environment):
        return subprocess.run(['bash', '--noprofile', '--norc', '-e', '-o', 'pipefail', '-c', script],
                              cwd=self.root, env={**os.environ, 'RUNNER_TEMP': str(self.root),
                                                 'GITHUB_OUTPUT': str(self.output), **environment},
                              capture_output=True, text=True, timeout=10)

    def git(self, *args):
        result = subprocess.run(['git', *args], cwd=self.root, capture_output=True, text=True, timeout=10)
        self.assertEqual(result.returncode, 0, result.stderr)
        return result.stdout.strip()

    def test_scope_uses_merge_diff_and_keeps_distributed_documents_in_checks(self):
        self.git('init', '-b', 'main')
        self.git('config', 'user.name', 'Synthetic CI')
        self.git('config', 'user.email', 'synthetic@users.noreply.github.com')
        self.git('commit', '--allow-empty', '-m', 'synthetic base')
        base = self.git('rev-parse', 'HEAD')
        # main 自己的代码改动不能使纯文档 PR 误跑构建。
        (self.root / 'main.rs').write_text('// 合成代码\n')
        self.git('add', 'main.rs')
        self.git('commit', '-m', 'synthetic main change')
        main = self.git('rev-parse', 'HEAD')
        cases = [
            (['README.md'], False),
            (['AGENTS.md', 'docs/nested/guide.md'], False),
            (['docs/THIRD_PARTY.md'], True),
            (['docs/BACKEND_TASKS.md'], True),
            (['docs/assets/demo.png'], True),
            (['vendor/LICENSE.md'], True),
            (['README.md', 'native/terms/src/lib.rs'], True),
            (['.github/workflows/ci.yml'], True),
        ]
        for paths, code in cases:
            with self.subTest(paths=paths):
                self.git('checkout', '-B', 'synthetic-pr', base)
                for name in paths:
                    path = self.root / name
                    path.parent.mkdir(parents=True, exist_ok=True)
                    path.write_text('合成改动\n', encoding='utf-8')
                self.git('add', *paths)
                self.git('commit', '-m', 'synthetic PR change')
                self.git('checkout', '-B', 'main', main)
                self.git('merge', '--no-ff', '--no-edit', 'synthetic-pr')
                self.output.unlink(missing_ok=True)
                result = self.shell(job_script('changes'), GITHUB_EVENT_NAME='pull_request')
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(self.output.read_text().strip(), f'code={str(code).lower()}')

    def test_push_and_manual_dispatch_always_run_checks(self):
        # 不在 git 仓库里运行：这两种事件不依赖 PR 检出或文件列表。
        for event in ('push', 'workflow_dispatch'):
            with self.subTest(event=event):
                self.output.unlink(missing_ok=True)
                result = self.shell(job_script('changes'), GITHUB_EVENT_NAME=event)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(self.output.read_text().strip(), 'code=true')

    def test_moving_code_into_documents_still_checks_the_removed_code(self):
        self.git('init', '-b', 'main')
        self.git('config', 'user.name', 'Synthetic CI')
        self.git('config', 'user.email', 'synthetic@users.noreply.github.com')
        (self.root / 'source.rs').write_text('// 合成代码\n', encoding='utf-8')
        self.git('add', 'source.rs')
        self.git('commit', '-m', 'synthetic source')
        (self.root / 'docs').mkdir()
        self.git('mv', 'source.rs', 'docs/moved.md')
        self.git('commit', '-m', 'synthetic move')
        result = self.shell(job_script('changes'), GITHUB_EVENT_NAME='pull_request')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.output.read_text().strip(), 'code=true')

    def needs(self, code='true', result='success', changes='success'):
        jobs = dict.fromkeys(('core', 'python', 'js', 'desktop', 'contracts', 'windows'), {'result': result})
        return {**jobs, 'changes': {'result': changes, 'outputs': {'code': code}}}

    def check_result(self, needs, success):
        result = self.shell(job_script('ok'), NEEDS=json.dumps(needs))
        self.assertEqual(result.returncode == 0, success, result.stdout + result.stderr)

    def test_summary_accepts_complete_code_checks_and_document_skips(self):
        self.check_result(self.needs(), True)
        self.check_result(self.needs(code='false', result='skipped'), True)

    def test_summary_reports_windows_without_making_it_required_for_code(self):
        for status in ('failure', 'cancelled', 'skipped'):
            with self.subTest(status=status):
                needs = self.needs()
                needs['windows'] = {'result': status}
                result = self.shell(job_script('ok'), NEEDS=json.dumps(needs))
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                self.assertIn(f'windows: {status}', result.stdout)
        needs = self.needs(code='false', result='skipped')
        needs['windows'] = {'result': 'success'}
        self.check_result(needs, False)

    def test_summary_rejects_failed_cancelled_and_unexpected_skipped_checks(self):
        for job in ('core', 'python', 'js', 'desktop', 'contracts'):
            for status in ('failure', 'cancelled', 'skipped'):
                with self.subTest(job=job, status=status):
                    needs = self.needs()
                    needs[job] = {'result': status}
                    self.check_result(needs, False)
        # 文档分流不能掩盖失败、取消或不一致的构建结果。
        for status in ('failure', 'cancelled', 'success'):
            with self.subTest(document_result=status):
                needs = self.needs(code='false', result='skipped')
                needs['core'] = {'result': status}
                self.check_result(needs, False)

    def test_summary_rejects_fork_or_missing_scope_output(self):
        for changes in ('failure', 'cancelled', 'skipped'):
            with self.subTest(changes=changes):
                self.check_result(self.needs(changes=changes), False)
        for code in ('', 'unknown', None):
            with self.subTest(code=code):
                self.check_result(self.needs(code=code), False)


if __name__ == '__main__':
    unittest.main()
