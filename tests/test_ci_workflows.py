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


def job_body(job, workflow='ci.yml'):
    source = (ROOT / '.github/workflows' / workflow).read_text(encoding='utf-8')
    return re.search(rf'^  {job}:\n(.*?)(?=^  \w+:|\Z)', source, re.M | re.S).group(1)


def job_script(job):
    """提取指定 job 的最后一个多行 run 块，保持测试与实际执行的脚本一致。"""
    body = job_body(job)
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

    def test_manual_dispatch_always_runs_checks(self):
        # 不在 git 仓库里运行：手动触发不依赖检出或文件列表。
        result = self.shell(job_script('changes'), GITHUB_EVENT_NAME='workflow_dispatch')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.output.read_text().strip(), 'code=true')

    def repository(self):
        self.git('init', '-b', 'main')
        self.git('config', 'user.name', 'Synthetic CI')
        self.git('config', 'user.email', 'synthetic@users.noreply.github.com')
        (self.root / 'source.rs').write_text('// 合成代码\n', encoding='utf-8')
        self.git('add', 'source.rs')
        self.git('commit', '-m', 'synthetic base')
        return self.git('rev-parse', 'HEAD')

    def commit(self, changes, message='synthetic change'):
        """`changes`：路径到内容，None 表示删除；返回新提交。"""
        for name, text in changes.items():
            path = self.root / name
            if text is None:
                self.git('rm', '-q', name)
                continue
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(text, encoding='utf-8')
            self.git('add', name)
        self.git('commit', '-m', message)
        return self.git('rev-parse', 'HEAD')

    def push_scope(self, before):
        self.output.unlink(missing_ok=True)
        result = self.shell(job_script('changes'), GITHUB_EVENT_NAME='push', PUSH_BEFORE=before)
        self.assertEqual(result.returncode, 0, result.stderr)
        return self.output.read_text().strip(), result.stdout

    def test_push_compares_trees_before_and_after_the_push(self):
        # #501：main push 按推送前后的实际差异分流；纯文档 push（含合并提交）跳过重型 job。
        base = self.repository()
        self.git('checkout', '-q', '-b', 'docs-pr')
        self.commit({'README.md': '合成文档\n', 'docs/guide.md': '合成说明\n'})
        self.git('checkout', '-q', 'main')
        self.git('merge', '-q', '--no-ff', '--no-edit', 'docs-pr')
        self.assertEqual(self.push_scope(base)[0], 'code=false')
        # 随包分发的 Markdown 按代码算。
        before = self.git('rev-parse', 'HEAD')
        self.commit({'docs/THIRD_PARTY.md': '合成许可\n'})
        self.assertEqual(self.push_scope(before)[0], 'code=true')
        # 多提交 push：只要其中有代码就进代码分支，即使最后一个提交只改文档。
        before = self.git('rev-parse', 'HEAD')
        self.commit({'native/terms/src/lib.rs': '// 合成代码\n'})
        self.commit({'README.md': '再改文档\n'})
        self.assertEqual(self.push_scope(before)[0], 'code=true')
        # 把代码改名进 docs/ 时，删掉的旧路径仍算代码。
        before = self.git('rev-parse', 'HEAD')
        self.git('mv', 'source.rs', 'docs/moved.md')
        self.git('commit', '-m', 'synthetic move')
        self.assertEqual(self.push_scope(before)[0], 'code=true')

    def test_push_without_a_usable_before_commit_runs_checks(self):
        self.repository()
        self.git('remote', 'add', 'origin', str(self.root / 'absent-remote'))
        for before in ('', '0' * 40, 'not-a-commit', 'f' * 40):
            with self.subTest(before=before):
                scope, stdout = self.push_scope(before)
                self.assertEqual(scope, 'code=true')
                self.assertIn('按代码改动处理', stdout)

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
        jobs = dict.fromkeys(('core', 'python', 'js', 'desktop', 'contracts'), {'result': result})
        return {**jobs, 'changes': {'result': changes, 'outputs': {'code': code}}}

    def check_result(self, needs, success):
        result = self.shell(job_script('ok'), NEEDS=json.dumps(needs))
        self.assertEqual(result.returncode == 0, success, result.stdout + result.stderr)

    def test_summary_accepts_complete_code_checks_and_document_skips(self):
        self.check_result(self.needs(), True)
        self.check_result(self.needs(code='false', result='skipped'), True)

    def test_push_runs_do_not_cancel_each_other_but_heavy_jobs_replace_older_ones(self):
        # #501：纯文档 push 不能取消上一轮还在写缓存的代码运行。workflow 级的组每次 push 各不相同，
        # 重型 job 各自的组只在 push 时共用，文档 push 跳过这些 job，不会进组。
        source = (ROOT / '.github/workflows/ci.yml').read_text(encoding='utf-8')
        top = re.search(r'^concurrency:\n  group: (.*)$', source, re.M).group(1)
        self.assertIn("github.event_name == 'push' && format('push-{0}', github.run_id)", top)
        for job in ('core', 'python', 'desktop', 'contracts', 'windows'):
            with self.subTest(job=job):
                body = job_body(job)
                self.assertIn("if: needs.changes.outputs.code == 'true'", body)
                group = re.search(r'^    concurrency:\n      group: (.*)\n      cancel-in-progress: true$', body, re.M).group(1)
                self.assertEqual(group, f"ci-{job}-${{{{ github.event_name == 'push' && github.ref || github.run_id }}}}")

    def test_windows_quick_tests_name_existing_classes(self):
        tests = re.search(r'^      QUICK_TESTS: (.*)$', job_body('windows'), re.M).group(1).split()
        self.assertTrue(tests)
        for name in tests:
            with self.subTest(name=name):
                module, cls = name.split('.')
                self.assertRegex((ROOT / 'tests' / f'{module}.py').read_text(encoding='utf-8'), rf'(?m)^class {cls}\(')

    def test_summary_does_not_wait_for_windows(self):
        # #501：Windows 试运行不计入通过判定，也不在 needs 里，汇总不用等它跑完。
        needs = re.search(r'^    needs: \[(.*)\]$', job_body('ok'), re.M).group(1).split(', ')
        self.assertEqual(needs, ['changes', 'core', 'python', 'js', 'desktop', 'contracts'])
        self.assertNotIn('windows', job_script('ok'))

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
