#!/usr/bin/env python3
"""串行本地验收；只复用本机签名的完整 check_all 结果。"""
import argparse
from contextlib import contextmanager
try:
    import fcntl
except ImportError:
    fcntl = None
import hashlib
import hmac
import json
import os
from pathlib import Path
import platform
import re
import secrets
import signal
import shutil
import subprocess
import sys
import time

SCHEMA = 3
# 唯一豁免：纯验收叙述。其余文档、规则、脚本、锁文件一律纳入。
NOTES = 'docs/acceptance/local-summary.md'
COMMAND = ['bash', 'tools/check_all.sh']
ORT_SESSION = ':memory:.ses'
SPLASH = 'desktop/dist/splash.js'
SPLASH_SOURCE = 'web/static/splash.js'
# 锁定 Tauri 2.7/2.10 的生成位置；不豁免整个 gen 或 dist。
TAURI_METADATA = frozenset({
    'desktop/gen/schemas/acl-manifests.json',
    'desktop/gen/schemas/capabilities.json',
    'desktop/gen/schemas/desktop-schema.json',
    'desktop/gen/schemas/' + ('macOS' if sys.platform == 'darwin' else 'linux') + '-schema.json',
})
OUTPUTS = ('.venv/', 'native/terms/target/', 'crates/iqdb/target/', 'crates/core/target/', 'crates/platform/target/', 'desktop/target/')


def digest(data):
    return hashlib.sha256(data).hexdigest()


def canonical(value):
    return json.dumps(value, sort_keys=True, separators=(',', ':'), ensure_ascii=True).encode()


def git(root, *args):
    return subprocess.check_output(['git', '-C', str(root), *args])


def clean(root):
    if git(root, 'status', '--porcelain', '--untracked-files=all').strip():
        raise ValueError('候选工作区有未提交或未跟踪文件；不覆盖、不清理。')


def inputs(root):
    clean(root)
    entries = []
    for line in git(root, 'ls-tree', '-r', '-z', 'HEAD').split(b'\0'):
        if not line:
            continue
        header, name = line.split(b'\t', 1)
        mode, kind, oid = header.split()
        path = os.fsdecode(name)
        if kind != b'blob':
            raise ValueError('不支持 submodule 输入，拒绝复用。')
        if path == NOTES:
            if mode != b'100644':
                raise ValueError('验收说明必须为普通非可执行文件。')
            continue
        entries.append([path, mode.decode(), oid.decode()])
    # 不读 models/reference 等潜在外部数据；未知被忽略输入直接拒绝。
    generated = []
    for name in git(root, 'ls-files', '--others', '--ignored', '--exclude-standard', '-z').split(b'\0'):
        if not name:
            continue
        path = os.fsdecode(name)
        if path.startswith(OUTPUTS) or '__pycache__' in Path(path).parts or path.endswith('.pyc') or Path(path).name == '.DS_Store':
            continue
        if path in {SPLASH, ORT_SESSION} or path in TAURI_METADATA:
            item = root / path
            if item.is_symlink() or not item.is_file() or any(parent.is_symlink() for parent in item.parents if parent != root and root in parent.parents):
                raise ValueError('桌面生成输出必须为本工作区内普通文件。')
            continue
        if path.startswith('packaging/desktop-resources/'):
            item = root / path
            if item.is_symlink():
                raise ValueError('生成资源符号链接不支持复用。')
            generated.append([path, digest(item.read_bytes()), item.stat().st_mode])
        else:
            raise ValueError(f'未知忽略输入：{path}；拒绝复用，不读取内容。')
    return {'tracked': digest(canonical(entries)), 'generated': generated}


def generated_outputs(root, require_generated=False):
    """有效输入保持不变时可新生成；已测输出随后必须原样匹配。"""
    values = []
    if require_generated and (root / 'desktop/Cargo.toml').exists():
        if any(not (root / name).is_file() for name in {SPLASH} | TAURI_METADATA):
            raise ValueError('完整桌面检查未留下要求的 splash/Tauri 生成输出。')
    for relative in sorted({SPLASH, ORT_SESSION} | TAURI_METADATA):
        path = root / relative
        if path.is_symlink() or any(parent.is_symlink() for parent in path.parents if parent != root and root in parent.parents):
            raise ValueError('桌面生成输出不能是符号链接。')
        if not path.exists():
            continue
        data = path.read_bytes()
        if relative == SPLASH:
            source = root / SPLASH_SOURCE
            tracked = git(root, 'ls-tree', 'HEAD', '--', SPLASH_SOURCE).split()
            if not tracked or tracked[0] != b'100644' or source.is_symlink() or data != source.read_bytes():
                raise ValueError('生成 splash.js 与已跟踪 web/static/splash.js 不一致。')
        elif relative == ORT_SESSION:
            # ORT 1.30 import 的本地会话 sidecar：时间戳 + UUID，无模型/账号输入。
            if not re.fullmatch(rb'[0-9]{13}\n[0-9a-fA-F]{8}(?:-[0-9a-fA-F]{4}){3}-[0-9a-fA-F]{12}\n?', data):
                raise ValueError('ONNX Runtime 会话输出格式不匹配。')
        elif not isinstance(json.loads(data), dict):
            raise ValueError('Tauri 生成元数据必须为 JSON object。')
        values.append([relative, digest(data), path.stat().st_mode])
    return values


def tree_hash(root):
    if not root.exists():
        return None
    values = []
    for path in sorted(root.rglob('*')):
        if '__pycache__' in path.parts or path.suffix == '.pyc':
            continue
        if path.is_symlink():
            values.append([str(path.relative_to(root)), os.readlink(path)])
        elif path.is_file():
            values.append([str(path.relative_to(root)), digest(path.read_bytes()), path.stat().st_mode])
    return digest(canonical(values))


def probe(args, env, cwd):
    executable = shutil.which(args[0], path=env.get('PATH'))
    if not executable:
        raise ValueError(f'缺少环境探针：{args[0]}')
    output = subprocess.check_output(args, cwd=cwd, env=env, stderr=subprocess.STDOUT).decode().strip()
    return {'path': os.path.realpath(executable), 'binary': digest(Path(executable).read_bytes()), 'version': output}



def mac_developer_environment(root, env):
    """CLT 和完整 Xcode 是两个明确身份；未知/失败的探针不降级忽略。"""
    developer = probe(['xcode-select', '-p'], env, root)
    values = {'developer-directory': developer}
    if Path(developer['version']).name == 'CommandLineTools':
        values['developer-kind'] = 'command-line-tools'
        values['clt'] = probe(['pkgutil', '--pkg-info', 'com.apple.pkg.CLTools_Executables'], env, root)
    else:
        values['developer-kind'] = 'xcode'
        values['xcode'] = probe(['xcodebuild', '-version'], env, root)
    values['sdk'] = probe(['xcrun', '--show-sdk-version'], env, root)
    values['sdk-path'] = probe(['xcrun', '--show-sdk-path'], env, root)
    values['clang'] = probe(['xcrun', 'clang', '--version'], env, root)
    actual_clang = probe(['xcrun', '--find', 'clang'], env, root)['version']
    values['clang-resolved'] = [actual_clang, digest(Path(actual_clang).read_bytes())]
    settings = Path(values['sdk-path']['version']) / 'SDKSettings.json'
    values['sdk-settings'] = digest(settings.read_bytes())
    return values


def environment(root, env):
    versions = {'controller-python': probe([sys.executable, '--version'], env, root)}
    for args in (['rustc', '-vV'], ['cargo', '--version'], ['rustfmt', '--version'],
                 ['cargo', 'clippy', '--version'], ['node', '--version'], ['bash', '--version'],
                 [env.get('CHECK_ALL_PYTHON', 'python3.12'), '--version']):
        versions[' '.join(args)] = probe(args, env, root)
    # rustup 代理本身不是编译器；记录实际工具链文件。
    for tool in ('rustc', 'cargo', 'rustfmt', 'clippy-driver'):
        path = subprocess.check_output(['rustup', 'which', tool], env=env, cwd=root).decode().strip()
        versions[tool + '-resolved'] = [path, digest(Path(path).read_bytes())]
    if sys.platform == 'darwin':
        versions.update(mac_developer_environment(root, env))
    elif sys.platform.startswith('linux'):
        versions['tauri-libs'] = probe(['pkg-config', '--modversion', 'webkit2gtk-4.1', 'ayatana-appindicator3-0.1', 'librsvg-2.0', 'xdo'], env, root)
    else:
        raise ValueError('仅 macOS/Linux；其他平台不复用。')
    cargo_home = Path(env.get('CARGO_HOME', str(Path.home() / '.cargo')))
    configs = {'git-config': digest(git(root, 'config', '--list', '--show-origin'))}
    for path in (cargo_home / 'config', cargo_home / 'config.toml', Path.home() / '.rustup/settings.toml'):
        configs[str(path)] = digest(path.read_bytes()) if path.exists() else None
    # 不输出环境变量值；完整环境的摘要覆盖所有测试/编译开关及路径。
    return {'host': platform.node(), 'os': platform.platform(), 'arch': platform.machine(),
            'workspace': str(root.resolve()), 'env_sha256': digest(canonical(env)),
            'tools': versions, 'configs': configs,
            'python_dependencies': tree_hash(root / '.venv/check-all/python/lib'),
            'controller': digest(Path(__file__).read_bytes())}


@contextmanager
def locked(state):
    if state.is_symlink() or state.parent.is_symlink():
        raise ValueError('验收状态目录不能是符号链接。')
    if fcntl is None:
        raise ValueError('验收窗口需要 POSIX 文件锁。')
    state.mkdir(parents=True, exist_ok=True)
    with (state / 'lock').open('a+') as stream:
        try:
            fcntl.flock(stream, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            raise ValueError('验收窗口已占用；不切 head、不写 target、不杀进程。') from None
        try:
            yield stream.fileno()
        finally:
            fcntl.flock(stream, fcntl.LOCK_UN)


def key(state):
    path = state / 'key'
    try:
        fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    except FileExistsError:
        pass
    else:
        with os.fdopen(fd, 'wb') as stream:
            stream.write(secrets.token_bytes(32))
    if path.is_symlink() or path.stat().st_mode & 0o077:
        raise ValueError('签名密钥权限异常。')
    value = path.read_bytes()
    if len(value) != 32:
        raise ValueError('签名密钥损坏。')
    return value


def write_record(state, payload):
    secret = key(state)
    wrapper = {'payload': payload, 'signature': hmac.new(secret, canonical(payload), hashlib.sha256).hexdigest()}
    temporary = state / 'result.tmp'
    temporary.write_bytes(canonical(wrapper))
    temporary.replace(state / 'result.json')


def verified_record(state, root, current_inputs, current_environment):
    wrapper = json.loads((state / 'result.json').read_text())
    payload = wrapper['payload']
    if not hmac.compare_digest(wrapper['signature'], hmac.new(key(state), canonical(payload), hashlib.sha256).hexdigest()):
        raise ValueError('验收记录签名不匹配。')
    required = {'schema', 'command', 'exit', 'started', 'finished', 'commit', 'tree', 'inputs', 'environment', 'log_sha256', 'generated_outputs'}
    if set(payload) != required or payload['schema'] != SCHEMA or payload['command'] != COMMAND or type(payload['exit']) is not int or payload['exit'] != 0:
        raise ValueError('不是完整入口退出 0 的记录。')
    if any(not isinstance(payload[name], str) or not re.fullmatch(r'[0-9a-f]{40}', payload[name]) for name in ('commit', 'tree')):
        raise ValueError('commit/tree 必须为完整 Git SHA。')
    if not isinstance(payload['started'], int) or not isinstance(payload['finished'], int) or payload['finished'] < payload['started']:
        raise ValueError('记录时间无效。')
    if git(root, 'rev-parse', payload['commit'] + '^{tree}').decode().strip() != payload['tree']:
        raise ValueError('已测 commit/tree 关联不匹配。')
    if payload['inputs'] != current_inputs or payload['environment'] != current_environment:
        raise ValueError('输入或机器/平台/工具链/依赖/环境改变。')
    if payload['generated_outputs'] != generated_outputs(root):
        raise ValueError('已测生成输出缺失或改变。')
    if digest((state / 'check-all.log').read_bytes()) != payload['log_sha256']:
        raise ValueError('完整运行日志缺失或改变。')
    # 可验证祖先与变更白名单；不将别的分支相同树冒称为此次验收。
    subprocess.run(['git', '-C', str(root), 'merge-base', '--is-ancestor', payload['commit'], 'HEAD'], check=True)
    for line in git(root, 'rev-list', '--parents', payload['commit'] + '..HEAD').splitlines():
        commits = line.decode().split()
        if len(commits) != 2:
            raise ValueError('验收叙述收尾只允许线性提交，不复用 merge。')
        changed = git(root, 'diff-tree', '--no-commit-id', '--name-only', '-r', '-z', commits[1], commits[0]).split(b'\0')
        if any(os.fsdecode(path) != NOTES for path in changed if path):
            raise ValueError('非验收说明的提交变化不得复用。')
    return payload


def prepare(source, state, commit):
    workspace = state / 'worktree'
    marker = state / 'workspace.json'
    owner = {'common': str((source / os.fsdecode(git(source, 'rev-parse', '--git-common-dir').strip())).resolve()),
             'workspace': str(workspace.resolve())}
    if workspace.is_symlink():
        raise ValueError('固定工作区不能是符号链接。')
    if workspace.exists():
        if not marker.exists() or json.loads(marker.read_text()) != owner:
            raise ValueError('现有目录没有本工具归属记录；不接管、不覆盖。')
        if git(workspace, 'rev-parse', '--git-common-dir').strip() != git(source, 'rev-parse', '--git-common-dir').strip():
            # 相对 common-dir 的输出不同；比较规范路径。
            def common(root):
                return (root / os.fsdecode(git(root, 'rev-parse', '--git-common-dir').strip())).resolve()
            if common(workspace) != common(source):
                raise ValueError('固定路径属于其他仓库。')
        clean(workspace)
        # 禁止分支 checkout；只有工具建立的 detached 工作区可移动。
        if subprocess.run(['git', '-C', str(workspace), 'symbolic-ref', '-q', 'HEAD'], stdout=subprocess.DEVNULL).returncode == 0:
            raise ValueError('固定验收区不是 detached HEAD，拒绝切换。')
        git(workspace, 'checkout', '--detach', commit)
    else:
        git(source, 'worktree', 'add', '--detach', str(workspace), commit)
        marker.write_bytes(canonical(owner))
    return workspace


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('action', choices=['run', 'result'])
    parser.add_argument('candidate', help='已提交的本地 ref；不 fetch、不 push')
    args = parser.parse_args()
    source = Path(git(Path.cwd(), 'rev-parse', '--show-toplevel').decode().strip())
    common = (source / git(source, 'rev-parse', '--git-common-dir').decode().strip()).resolve()
    state = common.parent / '.venv/local-acceptance'
    commit = git(source, 'rev-parse', args.candidate + '^{commit}').decode().strip()
    with locked(state) as lock_fd:
        root = prepare(source, state, commit)
        env = dict(os.environ)
        for name in ('_', 'OLDPWD', 'SHLVL', 'CARGO_TARGET_DIR'):
            env.pop(name, None)
        env['PWD'] = str(root)
        before = inputs(root)
        current = environment(root, env)
        try:
            record = verified_record(state, root, before, current)
        except (ValueError, KeyError, TypeError, OSError, subprocess.CalledProcessError) as error:
            if args.action == 'result':
                raise ValueError(f'无可复用全量结果：{error}') from None
            print(f'不复用，重新完整验收：{error}', flush=True)
        else:
            print(json.dumps({'candidate': commit, 'candidate_tree': git(root, 'rev-parse', 'HEAD^{tree}').decode().strip(),
                              'reuse': True, 'measured': record, 'state': str(state)}, ensure_ascii=False, indent=2))
            return
        # 删旧成功记录，失败/中断绝不能留下本次成功的错觉。
        (state / 'result.json').unlink(missing_ok=True)
        started = time.time_ns()
        with (state / 'check-all.log').open('wb') as log:
            process = subprocess.Popen(COMMAND, cwd=root, env=env, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, pass_fds=(lock_fd,))
            try:
                for chunk in iter(lambda: process.stdout.read1(65536), b''):
                    log.write(chunk)
                    output = getattr(sys.stdout, 'buffer', None)
                    if output is None:
                        # unittest -b 等捕获器只提供文本流；原始完整日志仍写入 log。
                        sys.stdout.write(chunk.decode('utf-8', errors='replace'))
                    else:
                        output.write(chunk)
                    sys.stdout.flush()
                status = process.wait()
            except BaseException:
                # 不杀任何进程；保持锁直到自己的子进程自然结束。
                for chunk in iter(lambda: process.stdout.read1(65536), b''):
                    log.write(chunk)
                process.wait()
                raise
            finally:
                process.stdout.close()
        if status:
            raise ValueError(f'完整验收失败（exit {status}）；没有成功记录。')
        if inputs(root) != before or git(root, 'rev-parse', 'HEAD').decode().strip() != commit:
            raise ValueError('验收期间输入/head 改变。')
        after = environment(root, env)
        # check_all 首次安装 Python 依赖是预期操作；其他环境变化不接受。
        comparable = dict(current, python_dependencies=after['python_dependencies'])
        if comparable != after:
            raise ValueError('验收期间环境/工具链改变。')
        record = {'schema': SCHEMA, 'command': COMMAND, 'exit': 0, 'started': started,
                  'finished': time.time_ns(), 'commit': commit,
                  'tree': git(root, 'rev-parse', 'HEAD^{tree}').decode().strip(),
                  'inputs': before, 'environment': after,
                  'log_sha256': digest((state / 'check-all.log').read_bytes()),
                  'generated_outputs': generated_outputs(root, require_generated=True)}
        write_record(state, record)
        print(f'完整验收记录：{state / "result.json"}；锁已随命令结束释放。')


if __name__ == '__main__':
    def interrupted(signum, frame):
        raise InterruptedError(f'信号 {signum} 中断；不写成功记录。')
    signal.signal(signal.SIGTERM, interrupted)
    try:
        main()
    except (ValueError, OSError, subprocess.CalledProcessError) as exc:
        print(str(exc), file=sys.stderr)
        sys.exit(1)
