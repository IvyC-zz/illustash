#!/usr/bin/env python3
"""构建白名单内的便携产物；Python 只是构建工具，从不是基础依赖。"""
import argparse
import hashlib
import importlib.util
import json
import os
import re
from pathlib import Path
import shutil
import subprocess
import sys
import uuid
import zipfile

ROOT = Path(__file__).resolve().parents[1]
RESEARCH_OPENBLAS_IDENTITIES = json.loads((ROOT / 'crates/platform/src/research_openblas.json').read_text())

def version_errors(root=ROOT):
    """tools/version.py check：每处带版本号的地方都与 native/terms/application.json 一致（#189）。"""
    spec = importlib.util.spec_from_file_location('illustash_version', ROOT / 'tools/version.py')
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module.check(root)

def copy(source, destination):
    if source.is_symlink():
        raise ValueError(f'symlink resource rejected: {source}')
    if not source.is_file():
        # 写明文件名；Windows CopyFile2 的错误不带路径。
        raise FileNotFoundError(f'resource missing: {source}')
    destination.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy2(source, destination)

# 每个产物（便携 ZIP、基础版和完整版安装程序）都在 resources/licenses/ 下带这些声明；
# 完整版安装程序的 compute/ 随 resources/ 的其余部分一起复制它们。
NOTICES = {'licenses/LICENSE': 'LICENSE', 'licenses/THIRD_PARTY.md': 'docs/THIRD_PARTY.md',
           'licenses/pocketfft/LICENSE.md': 'native/terms/vendor/pocketfft/LICENSE.md',
           'licenses/LCMS.txt': 'native/terms/licenses/LCMS.txt',
           'licenses/rust-dependencies.txt': 'packaging/rust-licenses/rust-dependencies.txt'}
# 这些 lock 文件里每个第三方 crate 的许可文本，由 tools/gen_rust_licenses.py 生成，和 crates.json 一起提交；
# 打包暂存时只拿它们和 lock 文件核对，然后复制（#161）。
RUST_LICENSES = ROOT / 'packaging/rust-licenses'
RUST_WORKSPACES = ('native/terms', 'desktop', 'crates/iqdb')
# 模型许可的原文，与其来源和 SHA-256 一起提交（#8）。每个计算包都带着它们，不管带不带模型
# （不带时 App 在第一次使用时下载模型，#168）；没有别的来源。
MODEL_LICENSES = ROOT / 'packaging/model-licenses'
# Windows ONNX Runtime 用的 DirectML（#271）：运行库从自己的文件夹加载的 DLL，以及 NuGet 包的许可文本，
# 取自构建该运行库时明确指定的 Microsoft.AI.DirectML 包目录。
DIRECTML_FILES = {'resources/onnxruntime/DirectML.dll': 'bin/x64-win/DirectML.dll',
                  'resources/licenses/directml/LICENSE.txt': 'LICENSE.txt',
                  'resources/licenses/directml/LICENSE-CODE.txt': 'LICENSE-CODE.txt',
                  'resources/licenses/directml/ThirdPartyNotices.txt': 'ThirdPartyNotices.txt'}
# 这个运行库构建在哪个后端上跑每个模型，装在运行库旁边（native/terms recognition_compute/backend.rs
# RUNTIME_BACKENDS）：替换运行库文件夹（比如换成 Windows ML）就能改后端，不用改代码。
WINDOWS_ORT_BACKENDS = ROOT / 'packaging/onnxruntime/windows-backends.json'
# Windows 的诊断数据管道只收集登记在这个 ETW 组里的 provider（#381）。微软官方发布的 onnxruntime.dll 登记在组里；
# 从公开源码自建的（docs/BACKEND_TASKS.md）只登记全零组，不进遥测通道。按 TraceLogging 元数据里的字节序（bytes_le）。
MICROSOFT_TELEMETRY_GROUP = uuid.UUID('4f50731a-89cf-4782-b3e0-dce8c90476ba').bytes_le
# resources/models/ 下的识别模型（native/terms/models.json）：一个包要么全有，要么一个都没有。
MODEL_FILES = tuple(entry['path'] for entry in json.loads((ROOT / 'native/terms/models.json').read_text())['files'])
# 一个 worker 程序承担全部四种；宿主每次启动时传入种类（#37）。
WORKER_KINDS = ('research', 'index', 'provenance', 'recognition')

def resources(destination):
    for path in sorted((ROOT / 'web/static').rglob('*')):
        if path.is_file():
            copy(path, destination / path.relative_to(ROOT))
    copy(ROOT / 'tags/tags_v3.csv', destination / 'tags/tags_v3.csv')
    for installed, source in NOTICES.items():
        copy(ROOT / source, destination / installed)

def launcher_environment(windows=False):
    """启用已安装的适配器，启动时不运行诊断或任务。"""
    if windows:
        lines = ['if not defined ILLUSTASH_ROOT set "ILLUSTASH_ROOT=%~dp0resources"',
                 'if not defined ILLUSTASH_RESEARCH_OPENBLAS_DLL set "ILLUSTASH_RESEARCH_OPENBLAS_DLL=%ILLUSTASH_ROOT%\\openblas\\openblas.dll"',
                 'if not defined ILLUSTASH_MODELS set "ILLUSTASH_MODELS=%ILLUSTASH_ROOT%\\models"',
                 'if not defined ILLUSTASH_SHARED if defined LOCALAPPDATA set "ILLUSTASH_SHARED=%LOCALAPPDATA%\\Illustash\\Shared"',
                 'if not defined ILLUSTASH_SHARED (echo Cannot locate writable shared resources; configure ILLUSTASH_SHARED. & exit /b 1)']
        for kind in WORKER_KINDS:
            lines.append(f'if not defined ILLUSTASH_{kind.upper()}_WORKER set "ILLUSTASH_{kind.upper()}_WORKER=%~dp0bin\\illustash-worker.exe"')
        return '\r\n'.join(lines) + '\r\n'
    lines = [': "${ILLUSTASH_ROOT:=$ROOT/resources}"',
             ': "${ILLUSTASH_MODELS:=$ILLUSTASH_ROOT/models}"',
             ': "${ILLUSTASH_SHARED:=${HOME:?HOME required}/Library/Application Support/Illustash/Shared}"',
             'export ILLUSTASH_ROOT ILLUSTASH_MODELS ILLUSTASH_SHARED']
    for kind in WORKER_KINDS:
        key = f'ILLUSTASH_{kind.upper()}_WORKER'
        lines.extend([f': "${{{key}:=$ROOT/bin/illustash-worker}}"', f'export {key}'])
    return '\n'.join(lines) + '\n'

def model_licenses(source=None):
    """已提交的模型声明的 [(installed path, file)]，已与 sources.json 核对。"""
    source = source or MODEL_LICENSES
    registry = source / 'sources.json'
    if not registry.is_file():
        raise ValueError(f'model license registry missing: {registry}')
    originals = json.loads(registry.read_text())['files']
    for name, entry in originals.items():
        path = source / name
        if path.is_symlink() or not path.is_file():
            raise ValueError(f'model license text missing: {name}')
        if hashlib.sha256(path.read_bytes()).hexdigest() != entry['sha256']:
            raise ValueError(f'model license text differs from its registered original: {name}')
    expected = {'NOTICE.txt', 'sources.json', *originals}
    present = {path.relative_to(source).as_posix() for path in source.rglob('*')
               if not path.is_dir() and path.name != '.DS_Store'}
    if present != expected:
        raise ValueError(f'model license directory does not match sources.json: '
                         f'missing {sorted(expected - present)}, unregistered {sorted(present - expected)}')
    return [(f'resources/licenses/models/{name}', source / name) for name in sorted(expected)]

def third_party_crates(lockfile):
    """Cargo 生成的 lock 文件里每个第三方包的 'name version'。

    我们自己的 crate 是名为 illustash-* 的路径包；vendored 的路径包算第三方。"""
    crates = set()
    for block in lockfile.read_text(encoding='utf-8').replace('\r\n', '\n').split('\n[[package]]\n')[1:]:
        fields = dict(re.findall(r'^(name|version|source) = "([^"]*)"$', block.split('\n[', 1)[0], re.M))
        if 'source' in fields or not fields['name'].startswith('illustash-'):
            crates.add(f"{fields['name']} {fields['version']}")
    return crates

def rust_licenses(source=None, root=ROOT):
    """已提交的 Rust 许可文本，已与 crates.json 和当前的 lock 文件核对。"""
    source = source or RUST_LICENSES
    text, registry = source / 'rust-dependencies.txt', source / 'crates.json'
    regenerate = 'rerun tools/gen_rust_licenses.py (docs/THIRD_PARTY.md#rust-依赖)'
    if not (text.is_file() and registry.is_file()):
        raise ValueError(f'Rust license texts missing: {text}; {regenerate}')
    registry = json.loads(registry.read_text(encoding='utf-8'))
    if hashlib.sha256(text.read_bytes()).hexdigest() != registry['text_sha256']:
        raise ValueError(f'Rust license texts differ from crates.json; {regenerate}')
    if sorted(registry['workspaces']) != sorted(RUST_WORKSPACES):
        raise ValueError(f'Rust license texts cover the wrong workspaces; {regenerate}')
    for workspace in RUST_WORKSPACES:
        locked = third_party_crates(root / workspace / 'Cargo.lock')
        covered, unbuilt = (set(registry['workspaces'][workspace][key]) for key in ('crates', 'not_built'))
        if covered & unbuilt or covered | unbuilt != locked:
            raise ValueError(f'Rust license texts are out of date with {workspace}/Cargo.lock: '
                             f'unlisted {sorted(locked - covered - unbuilt)}, '
                             f'no longer locked {sorted((covered | unbuilt) - locked)}; {regenerate}')
    return text

def research_openblas(site):
    """从明确指定的、装有 NumPy wheel 的 site-packages 里取固定的 DLL 和完整的随附声明。"""
    registry = RESEARCH_OPENBLAS_IDENTITIES
    candidates = []
    for dll in sorted((site / 'numpy.libs').glob('*.dll')):
        if dll.is_symlink():
            raise ValueError('symlink OpenBLAS resource rejected')
        digest = hashlib.sha256(dll.read_bytes()).hexdigest()
        if digest in registry:
            identity = registry[digest]
            dist = site / f"numpy-{identity['numpy']}.dist-info"
            # PEP 639 的 licenses/ 目录树就是 wheel 原本随附的声明。
            notices = dist / 'licenses'
            if not (notices / 'LICENSE.txt').is_file():
                raise ValueError('fixed NumPy OpenBLAS resource requires original wheel licenses/LICENSE.txt')
            candidates.append((dll, notices, digest))
    if len(candidates) != 1:
        raise ValueError('expected exactly one fixed NumPy OpenBLAS DLL in the explicit site-packages')
    return candidates[0]

def check_windows_ort_telemetry(dll):
    """#381：不打包登记在微软遥测组里的 ONNX Runtime（比如换成了官方发布的那份）。"""
    if MICROSOFT_TELEMETRY_GROUP in Path(dll).read_bytes():
        raise ValueError(f'{dll}: this ONNX Runtime registers its ETW provider in the Microsoft telemetry group '
                         '(an official Microsoft build); use the runtime built from source (docs/BACKEND_TASKS.md, #381)')

def stage(destination, binaries, research=False, desktop=None, iqdb=None, models=None, openblas_site=None,
          ort_dylib=None, ort_licenses=None, directml=None):
    destination = destination.resolve()
    if destination.exists():
        raise ValueError('output already exists; choose a new directory')
    if (models or openblas_site or ort_dylib or ort_licenses) and not research:
        raise ValueError('models/runtime require --research')
    # 在创建输出之前检查：缺模型许可的计算包从不暂存，Rust 许可文本落后于 lock 文件的包也一样。
    model_notices = model_licenses() if research else []
    rust_licenses()
    if research and sys.platform not in ('darwin', 'win32'):
        # Linux 已冻结：不在那里发布计算包（README）。
        raise ValueError('computation packages require macOS or Windows')
    if bool(ort_dylib) != bool(ort_licenses):
        raise ValueError('ORT runtime requires its LICENSE and ThirdPartyNotices.txt directory')
    if directml and (sys.platform != 'win32' or not ort_dylib):
        raise ValueError('DirectML goes with the Windows ORT runtime')
    if directml:
        for source in DIRECTML_FILES.values():
            if not (directml / source).is_file():
                raise FileNotFoundError(f'DirectML package file missing: {directml / source}')
    if ort_dylib and sys.platform == 'win32':
        check_windows_ort_telemetry(ort_dylib)
    openblas = None
    if openblas_site:
        if sys.platform != 'win32':
            raise ValueError('the fixed OpenBLAS resource is Windows only')
        openblas = research_openblas(openblas_site)
    destination.mkdir(parents=True)
    suffix = '.exe' if sys.platform == 'win32' else ''
    shell_setup = launcher_environment() if research else ''
    cmd_setup = launcher_environment(windows=True) if research else ''
    copy(binaries / ('illustash-terms' + suffix), destination / 'bin' / ('illustash-terms' + suffix))
    resources(destination / 'resources')
    copy(ROOT / 'native/terms/Cargo.lock', destination / 'dependencies/core-Cargo.lock')
    if desktop:
        copy(desktop, destination / 'bin' / ('illustash-desktop' + suffix))
        copy(ROOT / 'desktop/Cargo.lock', destination / 'dependencies/desktop-Cargo.lock')
        (destination / 'desktop.sh').write_text('#!/bin/sh\nset -eu\nROOT=$(CDPATH= cd -- "${0%/*}" && pwd)\n' + shell_setup + 'exec "$ROOT/bin/illustash-desktop" --root "${ILLUSTASH_ROOT:-$ROOT/resources}" "$@"\n', newline='')
        (destination / 'desktop.sh').chmod(0o755)
        (destination / 'desktop.cmd').write_text('@echo off\r\nsetlocal\r\n' + cmd_setup + '"%~dp0bin\\illustash-desktop.exe" --root "%ILLUSTASH_ROOT%" %*\r\n' if research else '@echo off\r\n"%~dp0bin\\illustash-desktop.exe" --root "%~dp0resources" %*\r\n', newline='')
    (destination / 'start.sh').write_text('#!/bin/sh\nset -eu\nROOT=$(CDPATH= cd -- "${0%/*}" && pwd)\n' + shell_setup + 'exec "$ROOT/bin/illustash-terms" http --native --root "${ILLUSTASH_ROOT:-$ROOT/resources}" "$@"\n', newline='')
    (destination / 'start.sh').chmod(0o755)
    (destination / 'start.cmd').write_text('@echo off\r\nsetlocal\r\n' + cmd_setup + '"%~dp0bin\\illustash-terms.exe" http --native --root "%ILLUSTASH_ROOT%" %*\r\n' if research else '@echo off\r\n"%~dp0bin\\illustash-terms.exe" http --native --root "%~dp0resources" %*\r\n', newline='')
    if research:
        iqdb = iqdb or target(ROOT / 'crates/iqdb') / 'release' / ('illustash-iqdb' + suffix)
        copy(iqdb, destination / 'bin' / ('illustash-iqdb' + suffix))
        copy(ROOT / 'crates/iqdb/Cargo.lock', destination / 'dependencies/iqdb-Cargo.lock')
        copy(binaries / ('illustash-worker' + suffix), destination / 'bin' / ('illustash-worker' + suffix))
        if models:
            for name in MODEL_FILES:
                copy(models / name, destination / 'resources/models' / name)
        for installed, path in model_notices:
            copy(path, destination / installed)
        if ort_dylib:
            library = 'onnxruntime.dll' if sys.platform == 'win32' else 'libonnxruntime.dylib'
            copy(ort_dylib, destination / 'resources/onnxruntime' / library)
            for name in ('LICENSE', 'ThirdPartyNotices.txt'):
                copy(ort_licenses / name, destination / 'resources/licenses/onnxruntime' / name)
        if directml:
            for installed, source in DIRECTML_FILES.items():
                copy(directml / source, destination / installed)
            copy(WINDOWS_ORT_BACKENDS, destination / 'resources/onnxruntime/backends.json')
        if openblas:
            dll, notices, digest = openblas
            installed = destination / 'resources/openblas/openblas.dll'
            copy(dll, installed)
            if hashlib.sha256(installed.read_bytes()).hexdigest() != digest:
                raise ValueError('OpenBLAS resource changed while staging')
            for path in sorted(notices.rglob('*')):
                if path.is_file():
                    copy(path, destination / 'resources/licenses/openblas' / path.relative_to(notices))
        for name in ('research.cmd', 'first-start.cmd'):
            text = (ROOT / 'packaging' / name).read_text()
            # 两个快捷方式和普通启动用同样的设置，保留已有的覆盖值。
            body = text.replace('@echo off\nsetlocal\n', '').replace('\n', '\r\n')
            (destination / name).write_text('@echo off\r\nsetlocal\r\n' + cmd_setup + body, newline='')
        copy(ROOT / 'packaging/RESEARCH_README.txt', destination / 'START-HERE.txt')
    copy(ROOT / 'docs/BACKEND_TASKS.md', destination / 'INSTALL.md')
    files = {p.relative_to(destination).as_posix(): hashlib.sha256(p.read_bytes()).hexdigest()
             for p in sorted(destination.rglob('*')) if p.is_file()}
    (destination / 'manifest.json').write_text(json.dumps({'format': 1, 'variant': 'research' if research else 'base', 'platform': sys.platform, 'files': files}, indent=2) + '\n')



def check_windows_crt(source):
    """发布安装程序之前，拒绝对 VC++ 运行库的导入，包括延迟导入。"""
    for directory in ('bin', 'resources'):
        for path in sorted((source / directory).rglob('*')):
            if path.suffix.lower() not in ('.exe', '.dll') or path.name == 'illustash-desktop.exe':
                continue
            result = subprocess.run(['dumpbin', '/nologo', '/dependents', str(path)],
                                    check=True, capture_output=True, text=True)
            dependencies = re.findall(r'^\s+((?:vcruntime|msvcp|concrt|vcomp|msvcr)[\w.-]*\.dll)\s*$',
                                      result.stdout, re.IGNORECASE | re.MULTILINE)
            if dependencies:
                raise ValueError(f'Windows computation component requires dynamic VC++ runtime: '
                                 f'{path.relative_to(source)}: {", ".join(dependencies)}')


def bundled_models(present):
    """模型文件要么全有，要么全无（#168）：没有时 App 在第一次使用时下载。"""
    found = [name for name in MODEL_FILES if present(f'resources/models/{name}')]
    if found and len(found) != len(MODEL_FILES):
        missing = [name for name in MODEL_FILES if name not in found]
        raise ValueError(f'models must be all present or all absent; missing resources/models/{missing[0]}')
    return bool(found)


def desktop_compute(source, destination):
    """只给安装程序用的布局，现有的 worker 适配器原样复用。模型可有可无（#168）。"""
    if sys.platform != 'win32':
        raise ValueError('full desktop installers require Windows')
    bundled_models(lambda relative: (source / relative).is_file())
    for required in ('bin/illustash-worker.exe', *(f'resources/{name}' for name in NOTICES),
                     'resources/openblas/openblas.dll',
                     'resources/licenses/openblas/LICENSE.txt', 'resources/onnxruntime/onnxruntime.dll',
                     'resources/licenses/onnxruntime/LICENSE', 'resources/licenses/onnxruntime/ThirdPartyNotices.txt',
                     *DIRECTML_FILES, 'resources/onnxruntime/backends.json',
                     *(installed for installed, _ in model_licenses())):
        if not (source / required).is_file():
            raise ValueError(f'full installer component missing: {required}')
    openblas_digest = hashlib.sha256((source / 'resources/openblas/openblas.dll').read_bytes()).hexdigest()
    if openblas_digest not in RESEARCH_OPENBLAS_IDENTITIES:
        raise ValueError('desktop OpenBLAS resource is not a fixed reference identity')
    if destination.exists():
        raise ValueError('desktop compute output already exists')
    check_windows_crt(source)
    destination.mkdir(parents=True)
    for directory in ('bin', 'resources', 'dependencies'):
        for path in sorted((source / directory).rglob('*')):
            if path.is_file() and path.name not in ('illustash-desktop.exe',):
                copy(path, destination / path.relative_to(source))
    if hashlib.sha256((destination / 'resources/openblas/openblas.dll').read_bytes()).hexdigest() != openblas_digest:
        raise ValueError('OpenBLAS resource changed while staging desktop compute')
    files = {p.relative_to(destination).as_posix(): hashlib.sha256(p.read_bytes()).hexdigest()
             for p in sorted(destination.rglob('*')) if p.is_file()}
    (destination / 'manifest.json').write_text(json.dumps({'format': 1, 'variant': 'research',
        'platform': 'win32', 'files': files}, indent=2) + '\n')


def macos_desktop_compute(source, destination):
    """把完整的便携资源布局复制进 macOS App 的 compute 包。模型可有可无（#168）。"""
    if sys.platform != 'darwin':
        raise ValueError('macOS desktop computation requires a macOS artifact')
    required = (
        'resources/web/static/index.html', 'resources/tags/tags_v3.csv',
        'resources/onnxruntime/libonnxruntime.dylib',
        'resources/licenses/onnxruntime/LICENSE',
        'resources/licenses/onnxruntime/ThirdPartyNotices.txt',
        *(f'resources/{name}' for name in NOTICES),
        *(installed for installed, _ in model_licenses()),
        'bin/illustash-terms', 'bin/illustash-iqdb', 'bin/illustash-worker',
        'dependencies/core-Cargo.lock', 'dependencies/iqdb-Cargo.lock',
    )
    manifest = json.loads((source / 'manifest.json').read_text())
    if (manifest.get('format'), manifest.get('variant'), manifest.get('platform')) != (1, 'research', 'darwin'):
        raise ValueError('expected a staged macOS research manifest')
    bundled_models(lambda relative: relative in manifest['files'])
    for relative in required:
        path = source / relative
        if path.is_symlink() or not path.is_file() or relative not in manifest['files']:
            raise ValueError(f'full macOS component missing or unmanifested: {relative}')
    if os.path.lexists(destination):
        raise ValueError('desktop compute output already exists')
    # 创建输出之前先校验；输入被改动过时绝不发布新的 manifest。
    paths = []
    for directory in ('bin', 'resources', 'dependencies'):
        for path in sorted((source / directory).rglob('*')):
            if path.is_file() and path.name != 'illustash-desktop':
                relative = path.relative_to(source).as_posix()
                if path.is_symlink() or manifest['files'].get(relative) != hashlib.sha256(path.read_bytes()).hexdigest():
                    raise ValueError(f'staged resource does not match manifest: {relative}')
                paths.append((relative, path))
    expected = {name for name in manifest['files']
                if name.split('/')[0] in ('bin', 'resources', 'dependencies')
                and Path(name).name != 'illustash-desktop'}
    if {name for name, _ in paths} != expected:
        raise ValueError('staged resources missing from source manifest file set')
    destination.mkdir(parents=True)
    for relative, path in paths:
        copy(path, destination / relative)
    files = {relative: hashlib.sha256((destination / relative).read_bytes()).hexdigest()
             for relative, _ in paths}
    if any(files[name] != manifest['files'][name] for name in files):
        raise ValueError('staged resources changed while copying')
    (destination / 'manifest.json').write_text(json.dumps({'format': 1, 'variant': 'research',
        'platform': 'darwin', 'files': files}, indent=2) + '\n')

def archive(directory, output):
    if output.exists():
        raise ValueError('archive already exists; choose a new path')
    with zipfile.ZipFile(output, 'x', compression=zipfile.ZIP_DEFLATED, compresslevel=9) as bundle:
        for path in sorted(directory.rglob('*')):
            if path.is_file():
                info = zipfile.ZipInfo((Path(directory.name) / path.relative_to(directory)).as_posix(), (1980, 1, 1, 0, 0, 0))
                info.compress_type = zipfile.ZIP_DEFLATED
                info.external_attr = (path.stat().st_mode & 0xffff) << 16
                bundle.writestr(info, path.read_bytes())

def target(crate):
    """cargo 的产物目录；遵守 CARGO_TARGET_DIR。"""
    configured = os.environ.get('CARGO_TARGET_DIR')
    return Path(configured).resolve() if configured else crate / 'target'

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--archive', type=Path, help='also write a sorted ZIP with fixed timestamps')
    parser.add_argument('--research', action='store_true')
    parser.add_argument('--models', type=Path, help='explicit model directory (allowlisted WD/CCIP files only); their licenses come from packaging/model-licenses/')
    parser.add_argument('--openblas-site', type=Path, help='Windows: site-packages holding the fixed NumPy wheel (numpy.libs OpenBLAS DLL and its licenses)')
    parser.add_argument('--ort-dylib', type=Path, help='explicit existing CPU ORT dynamic library; installed under resources/onnxruntime')
    parser.add_argument('--ort-licenses', type=Path, help='directory containing this ORT distribution LICENSE and ThirdPartyNotices.txt')
    parser.add_argument('--directml', type=Path, help='Windows: the Microsoft.AI.DirectML NuGet package directory the ORT runtime was built with (DirectML.dll and its licenses)')
    parser.add_argument('--profile', choices=('release', 'dev'), default='release')
    parser.add_argument('--binaries', type=Path, help='use already built release binaries')
    parser.add_argument('--iqdb', type=Path, help='built optional IQDB executable')
    parser.add_argument('--desktop', type=Path, help='optional native desktop executable (portable layout)')
    parser.add_argument('--desktop-resources', type=Path, help='also stage resources for Tauri bundling')
    parser.add_argument('--desktop-compute', type=Path, help='stage Windows installer computation resources (without --models: the release package; the app downloads the models)')
    parser.add_argument('--macos-desktop-compute', type=Path, help='stage macOS App computation resources (without --models: the release package; the app downloads the models)')
    args = parser.parse_args()
    if (args.models or args.openblas_site or args.ort_dylib or args.ort_licenses) and not args.research:
        parser.error('models/runtime require --research')
    if bool(args.ort_dylib) != bool(args.ort_licenses):
        parser.error('--ort-dylib and --ort-licenses must be supplied together')
    if args.directml and not (sys.platform == 'win32' and args.ort_dylib):
        parser.error('--directml requires Windows and --ort-dylib')
    if args.desktop_compute and not (args.research and args.openblas_site and args.ort_dylib and args.ort_licenses
                                     and args.directml):
        parser.error('--desktop-compute requires --research, --openblas-site, --ort-dylib, --ort-licenses and --directml')
    if args.macos_desktop_compute and not (sys.platform == 'darwin' and args.research
                                         and args.ort_dylib and args.ort_licenses):
        parser.error('--macos-desktop-compute requires macOS, --research, --ort-dylib and --ort-licenses')
    for destination in (args.output, args.archive, args.desktop_resources, args.desktop_compute, args.macos_desktop_compute):
        if destination is not None and destination.exists():
            parser.error(f'output already exists: {destination}; choose a fresh path')
    if errors := version_errors():
        parser.error('版本号不一致，先运行 python3 tools/version.py --sync：\n' + '\n'.join(errors))
    try:
        rust_licenses()
        if args.research:
            model_licenses()
    except ValueError as error:
        parser.error(str(error))
    binaries = args.binaries
    if binaries is None:
        subprocess.run(['cargo', 'build', '--locked', '--profile', args.profile, '--manifest-path', str(ROOT / 'native/terms/Cargo.toml'), '--bins'], check=True, cwd=ROOT)
        binaries = target(ROOT / 'native/terms') / ('debug' if args.profile == 'dev' else 'release')
    if args.research and args.iqdb is None:
        subprocess.run(['cargo', 'build', '--locked', '--release', '--manifest-path', str(ROOT / 'crates/iqdb/Cargo.toml')], check=True, cwd=ROOT)
    stage(args.output, binaries, args.research, args.desktop, args.iqdb, args.models, args.openblas_site,
          args.ort_dylib, args.ort_licenses, args.directml)
    if args.desktop_compute:
        desktop_compute(args.output, args.desktop_compute)
    if args.macos_desktop_compute:
        macos_desktop_compute(args.output, args.macos_desktop_compute)
    if args.archive:
        archive(args.output.resolve(), args.archive.resolve())
    if args.desktop_resources:
        resources(args.desktop_resources)

if __name__ == '__main__':
    main()
