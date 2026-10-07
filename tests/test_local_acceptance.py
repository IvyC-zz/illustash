"""验收工具的临时合成仓库测试；不构建、不访问真实图库。"""
import importlib.util
import io
import json
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest import mock
import os
import sys

spec = importlib.util.spec_from_file_location('local_acceptance', Path(__file__).resolve().parents[1] / 'tools/local_acceptance.py')
acceptance = importlib.util.module_from_spec(spec)
spec.loader.exec_module(acceptance)


@unittest.skipUnless(os.name == 'posix', 'POSIX local acceptance controller only')
class LocalAcceptanceTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name) / 'repo'
        self.root.mkdir()
        self.state = Path(self.temporary.name) / 'state'
        self.state.mkdir()
        self.git('init', '-q')
        self.git('config', 'user.email', 'synthetic@example.invalid')
        self.git('config', 'user.name', 'Synthetic')
        self.git('config', 'commit.gpgsign', 'false')
        hooks = Path(self.temporary.name) / 'empty-hooks'
        hooks.mkdir()
        self.git('config', 'core.hooksPath', str(hooks))
        self.write('src/code.rs', 'code')
        self.write('tests/test.py', 'test')
        self.write('Cargo.lock', 'lock')
        self.write('config.json', '{}')
        self.write('docs/contract.md', 'consumed contract')
        self.write('.gitignore', 'packaging/desktop-resources/\nmodels/\n.venv/\ndesktop/dist/splash.js\ndesktop/gen/\n:memory:*\n')
        self.write(acceptance.NOTES, '纯验收说明')
        self.commit()
        self.environment = {'host': 'synthetic', 'os': 'synthetic', 'arch': 'test',
                            'tools': {'rust': '1'}, 'dependencies': '1', 'env_sha256': '1', 'python_dependencies': '1'}
        self.payload = {'schema': acceptance.SCHEMA, 'command': acceptance.COMMAND, 'exit': 0,
                        'started': 1, 'finished': 2, 'commit': self.head(),
                        'tree': self.git('rev-parse', 'HEAD^{tree}').strip(),
                        'inputs': acceptance.inputs(self.root), 'environment': self.environment,
                        'log_sha256': acceptance.digest(b'complete synthetic log'), 'generated_outputs': []}
        (self.state / 'check-all.log').write_bytes(b'complete synthetic log')
        acceptance.write_record(self.state, self.payload)

    def git(self, *args):
        return subprocess.check_output(['git', '-C', str(self.root), *args], stderr=subprocess.DEVNULL).decode()

    def write(self, name, value):
        path = self.root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(value)

    def commit(self):
        self.git('add', '.')
        self.git('commit', '-qm', 'synthetic')

    def head(self):
        return self.git('rev-parse', 'HEAD').strip()

    def verify(self, environment=None):
        return acceptance.verified_record(self.state, self.root, acceptance.inputs(self.root),
                                          self.environment if environment is None else environment)

    def test_same_inputs_and_notes_only_reuse_original_commit(self):
        measured = self.head()
        self.assertEqual(self.verify()['commit'], measured)
        self.write(acceptance.NOTES, '新增验收叙述')
        self.commit()
        self.assertNotEqual(self.head(), measured)
        self.assertEqual(self.verify()['commit'], measured)

    def test_every_other_tracked_input_invalidates(self):
        for name in ('src/code.rs', 'tests/test.py', 'Cargo.lock', 'config.json', 'docs/contract.md',
                     'tools/check_all.sh', 'generated/resource.png', 'AGENTS.md'):
            with self.subTest(name=name):
                self.git('checkout', '--detach', self.payload['commit'])
                self.write(name, 'changed')
                self.commit()
                with self.assertRaises(ValueError):
                    self.verify()

    def test_code_change_reverted_still_invalidates(self):
        self.write('src/code.rs', 'changed')
        self.commit()
        self.write('src/code.rs', 'code')
        self.commit()
        self.assertEqual(acceptance.inputs(self.root), self.payload['inputs'])
        with self.assertRaisesRegex(ValueError, '提交变化'):
            self.verify()

    def test_generated_ignored_resource_invalidates(self):
        self.write('packaging/desktop-resources/resource', 'generated')
        with self.assertRaises(ValueError):
            self.verify()

    def test_generated_desktop_outputs_do_not_change_effective_inputs(self):
        self.write(acceptance.SPLASH_SOURCE, 'tracked splash')
        self.commit()
        before = acceptance.inputs(self.root)
        self.write(acceptance.SPLASH, 'tracked splash')
        for name in acceptance.TAURI_METADATA:
            self.write(name, '{}')
        self.assertEqual(acceptance.inputs(self.root), before)
        self.assertEqual(len(acceptance.generated_outputs(self.root)), 5)
        self.write(acceptance.SPLASH, 'stale splash')
        with self.assertRaisesRegex(ValueError, '不一致'):
            acceptance.generated_outputs(self.root)

    def test_splash_source_change_invalidates_and_requires_new_copy(self):
        self.write(acceptance.SPLASH_SOURCE, 'old source')
        self.commit()
        self.write(acceptance.SPLASH, 'old source')
        old_inputs = acceptance.inputs(self.root)
        self.write(acceptance.SPLASH_SOURCE, 'new source')
        self.commit()
        self.assertNotEqual(acceptance.inputs(self.root), old_inputs)
        with self.assertRaisesRegex(ValueError, '不一致'):
            acceptance.generated_outputs(self.root)
        self.write(acceptance.SPLASH, 'new source')
        acceptance.generated_outputs(self.root)

    def test_desktop_unknown_generated_files_and_symlinks_rejected(self):
        for name in ('desktop/gen/schemas/unknown.json', 'desktop/gen/other.bin'):
            with self.subTest(name=name):
                self.write(name, '{}')
                with self.assertRaisesRegex(ValueError, '未知忽略输入'):
                    acceptance.inputs(self.root)
                (self.root / name).unlink()
        self.write('.gitignore', (self.root / '.gitignore').read_text() + 'desktop/dist/unrelated.js\n')
        self.commit()
        self.write('desktop/dist/unrelated.js', 'not splash')
        with self.assertRaisesRegex(ValueError, '未知忽略输入'):
            acceptance.inputs(self.root)
        (self.root / 'desktop/dist/unrelated.js').unlink()
        self.write(acceptance.SPLASH_SOURCE, 'tracked splash')
        self.commit()
        path = self.root / acceptance.SPLASH
        path.parent.mkdir(parents=True, exist_ok=True)
        path.symlink_to(self.root / acceptance.SPLASH_SOURCE)
        with self.assertRaisesRegex(ValueError, '普通文件'):
            acceptance.inputs(self.root)

    def test_generated_desktop_full_fixture_records_and_reuses(self):
        self.write(acceptance.SPLASH_SOURCE, 'tracked splash')
        self.write('desktop/Cargo.toml', '# synthetic descriptor')
        generate = '#!/bin/bash\nmkdir -p desktop/dist desktop/gen/schemas\ncp web/static/splash.js desktop/dist/splash.js\n'
        generate += ''.join("printf '{}' > " + name + '\n' for name in sorted(acceptance.TAURI_METADATA))
        self.write('tools/check_all.sh', generate)
        self.commit()
        self.run_fixture()
        state = self.root / '.venv/local-acceptance'
        payload = json.loads((state / 'result.json').read_text())['payload']
        self.assertEqual(len(payload['generated_outputs']), 5)
        self.write(acceptance.NOTES, 'notes after synthetic generation')
        self.commit()
        self.run_fixture('result')
        generated_root = state / 'worktree'
        for name in sorted(acceptance.TAURI_METADATA):
            path = generated_root / name
            original = path.read_text()
            path.write_text('{"tampered":true}')
            with self.subTest(name=name), self.assertRaisesRegex(ValueError, '输出缺失或改变'):
                self.run_fixture('result')
            path.write_text(original)
        (generated_root / acceptance.SPLASH).write_text('tampered splash')
        with self.assertRaisesRegex(ValueError, '不一致'):
            self.run_fixture('result')
        # 完整入口重新复制候选源，可修复旧/篡改 splash；不删除缓存。
        self.run_fixture()
        self.run_fixture('result')

    def test_incomplete_desktop_generation_cannot_record_success(self):
        self.write('desktop/Cargo.toml', '# synthetic descriptor')
        self.write('tools/check_all.sh', '#!/bin/bash\nexit 0\n')
        self.commit()
        with self.assertRaisesRegex(ValueError, '生成输出'):
            self.run_fixture()
        self.assertFalse((self.root / '.venv/local-acceptance/result.json').exists())

    def test_onnxruntime_exact_session_output_is_signed_not_broadly_ignored(self):
        before = acceptance.inputs(self.root)
        marker = '1791000000000\n01234567-89ab-cdef-0123-456789abcdef\n'
        self.write(acceptance.ORT_SESSION, marker)
        self.assertEqual(acceptance.inputs(self.root), before)
        outputs = acceptance.generated_outputs(self.root)
        self.assertEqual(len(outputs), 1)
        payload = dict(self.payload, generated_outputs=outputs)
        acceptance.write_record(self.state, payload)
        self.verify()
        self.write(acceptance.ORT_SESSION, marker.replace('0000000', '0000001'))
        with self.assertRaisesRegex(ValueError, '输出缺失或改变'):
            self.verify()
        self.write(acceptance.ORT_SESSION, 'invalid external input')
        with self.assertRaisesRegex(ValueError, '格式'):
            acceptance.generated_outputs(self.root)
        (self.root / acceptance.ORT_SESSION).unlink()
        self.write(':memory:unknown', 'unknown')
        with self.assertRaisesRegex(ValueError, '未知忽略输入'):
            acceptance.inputs(self.root)

    def test_clt_or_xcode_identity_is_explicit_and_probe_failures_reject(self):
        sdk = Path(self.temporary.name) / 'sdk'
        sdk.mkdir()
        (sdk / 'SDKSettings.json').write_text('{}')
        clang = Path(self.temporary.name) / 'clang'
        clang.write_text('synthetic clang')
        commands = []
        developer = str(Path(self.temporary.name) / 'CommandLineTools')
        fail = None
        def fake_probe(args, env, cwd):
            commands.append(args)
            if args == fail:
                raise subprocess.CalledProcessError(1, args)
            versions = {('xcode-select', '-p'): developer,
                        ('xcrun', '--show-sdk-path'): str(sdk),
                        ('xcrun', '--find', 'clang'): str(clang)}
            return {'path': 'synthetic', 'binary': 'synthetic', 'version': versions.get(tuple(args), 'synthetic-version')}
        with mock.patch.object(acceptance, 'probe', side_effect=fake_probe):
            values = acceptance.mac_developer_environment(self.root, {})
            self.assertEqual(values['developer-kind'], 'command-line-tools')
            self.assertNotIn(['xcodebuild', '-version'], commands)
            self.assertIn(['pkgutil', '--pkg-info', 'com.apple.pkg.CLTools_Executables'], commands)
            (sdk / 'SDKSettings.json').write_text('{"changed":true}')
            self.assertNotEqual(acceptance.mac_developer_environment(self.root, {})['sdk-settings'], values['sdk-settings'])
            fail = ['pkgutil', '--pkg-info', 'com.apple.pkg.CLTools_Executables']
            with self.assertRaises(subprocess.CalledProcessError):
                acceptance.mac_developer_environment(self.root, {})
            fail = None
            developer = str(Path(self.temporary.name) / 'Xcode.app/Contents/Developer')
            self.assertEqual(acceptance.mac_developer_environment(self.root, {})['developer-kind'], 'xcode')
            fail = ['xcodebuild', '-version']
            with self.assertRaises(subprocess.CalledProcessError):
                acceptance.mac_developer_environment(self.root, {})

    def test_targeted_shell_phase_uses_candidate_builds_before_discover(self):
        script = (Path(acceptance.__file__).parent / 'check_all.sh').read_text()
        start = script.index('run cargo build --release --locked --manifest-path native/terms/Cargo.toml\n')
        targeted = script.index('if [ -n "${CHECK_ALL_TARGETED_TESTS:-}" ]')
        end = script.index('log="$cache/unittest.log"')
        self.assertLess(start, targeted)
        self.assertLess(targeted, script.index('run "$python" -m unittest discover -s tests -v -b'))
        # 执行真实脚本的构建至定向片段；只将 cargo 变成合成写入。
        for name in ('native/terms/target/release/illustash-terms', 'crates/iqdb/target/release/illustash-iqdb',
                     'native/terms/target/release/examples/read-http'):
            self.write(name, 'old target')
        self.write('tests/__init__.py', '')
        self.write('tests/candidate_helper.py', 'EXPECTED=\"candidate\"')
        self.write('tests/test_candidate.py', "from tests.candidate_helper import EXPECTED\nimport os, pathlib, unittest\nclass Candidate(unittest.TestCase):\n def test_candidate(self):\n  root=pathlib.Path(os.environ['ILLUSTASH_NATIVE_ROOT'])\n  self.assertEqual(pathlib.Path(os.environ['ILLUSTASH_TERMS_BIN']).read_text(),'candidate')\n  self.assertEqual(pathlib.Path(os.environ['ILLUSTASH_IQDB_BINARY']).read_text(),'candidate')\n  self.assertEqual((root/'native/terms/target/release/examples/read-http').read_text(),'candidate')\n")
        prefix = """set -eu
root=$PWD
cache=$root/.venv/check-all
python=$SYNTHETIC_PYTHON
head=$SYNTHETIC_HEAD
config() { echo "$head"; }
rustc() { echo synthetic-rust; }
run() {
  if [ "$1" = cargo ]; then
    printf candidate > native/terms/target/release/illustash-terms
    printf candidate > crates/iqdb/target/release/illustash-iqdb
    printf candidate > native/terms/target/release/examples/read-http
  else
    "$@"
  fi
}
"""
        env = dict(os.environ, SYNTHETIC_PYTHON=sys.executable, SYNTHETIC_HEAD=self.head(),
                   CHECK_ALL_TARGETED_TESTS='tests.test_candidate')
        result = subprocess.run(['bash', '-c', prefix + script[start:end]], cwd=self.root, env=env, capture_output=True)
        self.assertEqual(result.returncode, 0, result.stderr.decode())
        self.assertIn(self.head(), result.stdout.decode())
        # 旧前置变量 fail-closed，不静默使用旧 target。
        old_start = script.index('if [ -n "${CHECK_ALL_PREFLIGHT_TESTS:-}" ]')
        old_end = script.index('run cargo fmt', old_start)
        result = subprocess.run(['bash', '-c', script[old_start:old_end]],
                                env=dict(os.environ, CHECK_ALL_PREFLIGHT_TESTS='test_candidate'), capture_output=True)
        self.assertEqual(result.returncode, 2)

    def test_unknown_ignored_input_rejected_without_reading(self):
        self.write('models/no-read', 'external')
        with self.assertRaisesRegex(ValueError, '未知忽略输入'):
            acceptance.inputs(self.root)

    def test_dirty_and_untracked_workspaces_rejected(self):
        self.write('src/code.rs', 'dirty')
        with self.assertRaises(ValueError):
            acceptance.inputs(self.root)
        self.git('checkout', '--', 'src/code.rs')
        self.write('untracked', 'synthetic')
        with self.assertRaises(ValueError):
            acceptance.inputs(self.root)

    def test_platform_tools_dependencies_environment_invalidates(self):
        for name in self.environment:
            with self.subTest(name=name):
                changed = dict(self.environment, **{name: 'changed'})
                with self.assertRaises(ValueError):
                    self.verify(changed)

    def test_tampered_record_and_log_rejected(self):
        path = self.state / 'result.json'
        wrapper = json.loads(path.read_text())
        wrapper['payload']['exit'] = 1
        path.write_text(json.dumps(wrapper))
        with self.assertRaisesRegex(ValueError, '签名'):
            self.verify()
        acceptance.write_record(self.state, self.payload)
        (self.state / 'check-all.log').write_bytes(b'changed')
        with self.assertRaisesRegex(ValueError, '日志'):
            self.verify()

    def test_signed_failure_incomplete_partial_wrong_tree_rejected(self):
        variants = [dict(self.payload, schema=1), dict(self.payload, schema=2), dict(self.payload, exit=1), dict(self.payload, command=['targeted']),
                    dict(self.payload, tree='0' * 40), dict(self.payload, finished=0),
                    {k: v for k, v in self.payload.items() if k != 'exit'}]
        for payload in variants:
            with self.subTest(payload=payload):
                acceptance.write_record(self.state, payload)
                with self.assertRaises(ValueError):
                    self.verify()

    def test_lock_excludes_second_process_and_releases(self):
        code = "import importlib.util, pathlib, sys; s=importlib.util.spec_from_file_location('a',sys.argv[1]); a=importlib.util.module_from_spec(s); s.loader.exec_module(a); c=a.locked(pathlib.Path(sys.argv[2])); c.__enter__()"
        with acceptance.locked(self.state):
            result = subprocess.run([sys.executable, '-c', code, str(Path(acceptance.__file__)), str(self.state)], capture_output=True)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn('验收窗口已占用', result.stderr.decode())
        with acceptance.locked(self.state):
            pass

    def test_child_inherits_lock_if_controller_exits(self):
        read_fd, write_fd = os.pipe()
        code = "import importlib.util,pathlib,sys,subprocess,os; s=importlib.util.spec_from_file_location('a',sys.argv[1]); a=importlib.util.module_from_spec(s); s.loader.exec_module(a); c=a.locked(pathlib.Path(sys.argv[2])); fd=c.__enter__(); subprocess.Popen([sys.executable,'-c',\"import os; print('ready',flush=True); os.read(0,1)\"],stdin=int(sys.argv[3]),pass_fds=(fd,)); os._exit(0)"
        owner = subprocess.Popen([sys.executable, '-c', code, str(Path(acceptance.__file__)), str(self.state), str(read_fd)],
                                 pass_fds=(read_fd,), stdout=subprocess.PIPE)
        os.close(read_fd)
        try:
            owner.wait(timeout=10)
            self.assertEqual(owner.stdout.readline(), b'ready\n')
            with self.assertRaisesRegex(ValueError, '已占用'):
                with acceptance.locked(self.state):
                    pass
        finally:
            os.write(write_fd, b'x')
            os.close(write_fd)
            owner.stdout.read()
            owner.stdout.close()
        with acceptance.locked(self.state):
            pass

    def run_fixture(self, action='run'):
        previous = Path.cwd()
        try:
            os.chdir(self.root)
            with mock.patch('sys.argv', ['local_acceptance.py', action, self.head()]), \
                 mock.patch.object(acceptance, 'environment', return_value=self.environment):
                acceptance.main()
        finally:
            os.chdir(previous)

    def test_full_command_fixture_success_reuse_and_failure(self):
        self.write('tools/check_all.sh', '#!/bin/bash\necho synthetic-full-command\nexit 0\n')
        self.commit()
        self.run_fixture()
        state = self.root / '.venv/local-acceptance'
        measured = json.loads((state / 'result.json').read_text())['payload']['commit']
        self.write(acceptance.NOTES, 'closeout')
        self.commit()
        self.run_fixture('result')
        self.assertEqual(json.loads((state / 'result.json').read_text())['payload']['commit'], measured)
        self.write('tools/check_all.sh', '#!/bin/bash\necho synthetic-failure\nexit 9\n')
        self.commit()
        with self.assertRaisesRegex(ValueError, 'exit 9'):
            self.run_fixture()
        self.assertFalse((state / 'result.json').exists())

    def test_text_stdout_capture_preserves_complete_raw_log(self):
        text = '合成完整输出\n'
        self.write('tools/check_all.sh', "#!/bin/bash\nprintf '合成完整输出\\n'\nexit 0\n")
        self.commit()
        with mock.patch('sys.stdout', new=io.StringIO()) as output:
            self.run_fixture()
        self.assertIn(text, output.getvalue())
        state = self.root / '.venv/local-acceptance'
        self.assertEqual((state / 'check-all.log').read_bytes(), text.encode())
        payload = json.loads((state / 'result.json').read_text())['payload']
        self.assertEqual(payload['exit'], 0)
        self.assertEqual(payload['log_sha256'], acceptance.digest(text.encode()))

    def test_command_changes_input_cannot_record_success(self):
        self.write('tools/check_all.sh', '#!/bin/bash\necho changed > src/code.rs\nexit 0\n')
        self.commit()
        with self.assertRaises(ValueError):
            self.run_fixture()
        self.assertFalse((self.root / '.venv/local-acceptance/result.json').exists())

    def test_unknown_existing_workspace_not_adopted(self):
        self.git('worktree', 'add', '--detach', str(self.state / 'worktree'), self.head())
        with self.assertRaisesRegex(ValueError, '归属'):
            acceptance.prepare(self.root, self.state, self.head())

    def test_fixed_workspace_detached_preserves_cache_and_refuses_dirty(self):
        workspace = acceptance.prepare(self.root, self.state, self.head())
        self.write(acceptance.NOTES, 'next candidate')
        self.commit()
        cache = workspace / '.venv/preserved'
        cache.parent.mkdir()
        cache.write_text('reusable')
        acceptance.prepare(self.root, self.state, self.head())
        self.assertEqual(cache.read_text(), 'reusable')
        (workspace / 'src/code.rs').write_text('user edits')
        with self.assertRaises(ValueError):
            acceptance.prepare(self.root, self.state, self.payload['commit'])
        self.assertEqual((workspace / 'src/code.rs').read_text(), 'user edits')


if __name__ == '__main__':
    unittest.main()
