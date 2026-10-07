#!/usr/bin/env python3
"""Build allowlisted portable artifacts; Python is a build tool, never a base dependency."""
import argparse
import hashlib
import json
import os
import re
from pathlib import Path
import shutil
import subprocess
import sys
import zipfile

ROOT = Path(__file__).resolve().parents[1]
RESEARCH_OPENBLAS_IDENTITIES = json.loads((ROOT / 'crates/platform/src/research_openblas.json').read_text())

def copy(source, destination):
    if source.is_symlink():
        raise ValueError(f'symlink resource rejected: {source}')
    destination.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy2(source, destination)

def resources(destination):
    for path in sorted((ROOT / 'web/static').rglob('*')):
        if path.is_file():
            copy(path, destination / path.relative_to(ROOT))
    copy(ROOT / 'tags/tags_v3.csv', destination / 'tags/tags_v3.csv')
    copy(ROOT / 'native/terms/vendor/pocketfft/LICENSE.md', destination / 'licenses/pocketfft/LICENSE.md')
    copy(ROOT / 'native/terms/licenses/LCMS.txt', destination / 'licenses/LCMS.txt')

def launcher_environment(windows=False):
    """Opt in to installed adapters without running diagnostics or a task at startup."""
    if windows:
        lines = ['if not defined ILLUSTASH_ROOT set "ILLUSTASH_ROOT=%~dp0resources"',
                 'if not defined ILLUSTASH_RESEARCH_OPENBLAS_DLL set "ILLUSTASH_RESEARCH_OPENBLAS_DLL=%ILLUSTASH_ROOT%\\openblas\\openblas.dll"',
                 'if not defined ILLUSTASH_MODELS set "ILLUSTASH_MODELS=%ILLUSTASH_ROOT%\\models"',
                 'if not defined ILLUSTASH_SHARED if defined LOCALAPPDATA set "ILLUSTASH_SHARED=%LOCALAPPDATA%\\Illustash\\Shared"',
                 'if not defined ILLUSTASH_SHARED (echo Cannot locate writable shared resources; configure ILLUSTASH_SHARED. & exit /b 1)']
        for kind in ('research', 'index', 'provenance', 'recognition'):
            lines.append(f'if not defined ILLUSTASH_{kind.upper()}_WORKER set "ILLUSTASH_{kind.upper()}_WORKER=%~dp0bin\\illustash-{kind}.exe"')
        return '\r\n'.join(lines) + '\r\n'
    lines = [': "${ILLUSTASH_ROOT:=$ROOT/resources}"',
             ': "${ILLUSTASH_MODELS:=$ILLUSTASH_ROOT/models}"',
             ': "${ILLUSTASH_SHARED:=${HOME:?HOME required}/Library/Application Support/Illustash/Shared}"',
             'export ILLUSTASH_ROOT ILLUSTASH_MODELS ILLUSTASH_SHARED']
    for kind in ('research', 'index', 'provenance', 'recognition'):
        key = f'ILLUSTASH_{kind.upper()}_WORKER'
        lines.extend([f': "${{{key}:=$ROOT/bin/illustash-{kind}}}"', f'export {key}'])
    return '\n'.join(lines) + '\n'

def research_openblas(site):
    """Fixed DLL + complete bundled notices from an explicit site-packages holding the NumPy wheel."""
    registry = RESEARCH_OPENBLAS_IDENTITIES
    candidates = []
    for dll in sorted((site / 'numpy.libs').glob('*.dll')):
        if dll.is_symlink():
            raise ValueError('symlink OpenBLAS resource rejected')
        digest = hashlib.sha256(dll.read_bytes()).hexdigest()
        if digest in registry:
            identity = registry[digest]
            dist = site / f"numpy-{identity['numpy']}.dist-info"
            # PEP 639 licenses/ tree is the wheel's original bundled notices.
            notices = dist / 'licenses'
            if not (notices / 'LICENSE.txt').is_file():
                raise ValueError('fixed NumPy OpenBLAS resource requires original wheel licenses/LICENSE.txt')
            candidates.append((dll, notices, digest))
    if len(candidates) != 1:
        raise ValueError('expected exactly one fixed NumPy OpenBLAS DLL in the explicit site-packages')
    return candidates[0]

def stage(destination, binaries, research=False, desktop=None, iqdb=None, models=None, openblas_site=None,
          ort_dylib=None, ort_licenses=None):
    destination = destination.resolve()
    if destination.exists():
        raise ValueError('output already exists; choose a new directory')
    if (models or openblas_site or ort_dylib or ort_licenses) and not research:
        raise ValueError('models/runtime require --research')
    if research and sys.platform not in ('darwin', 'win32'):
        # Linux is frozen: no computation package ships there (README).
        raise ValueError('computation packages require macOS or Windows')
    if bool(ort_dylib) != bool(ort_licenses):
        raise ValueError('ORT runtime requires its LICENSE and ThirdPartyNotices.txt directory')
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
        for kind in ('research', 'index', 'provenance', 'recognition'):
            copy(binaries / ('illustash-worker' + suffix), destination / 'bin' / ('illustash-' + kind + suffix))
        if models:
            for name in ('wd_swinv2_v3_emb.onnx', 'ccip/model_feat.onnx', 'ccip/model_metrics.onnx', 'ccip/metrics.json'):
                copy(models / name, destination / 'resources/models' / name)
        if ort_dylib:
            library = 'onnxruntime.dll' if sys.platform == 'win32' else 'libonnxruntime.dylib'
            copy(ort_dylib, destination / 'resources/onnxruntime' / library)
            for name in ('LICENSE', 'ThirdPartyNotices.txt'):
                copy(ort_licenses / name, destination / 'resources/licenses/onnxruntime' / name)
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
            # Both shortcuts use the same override-preserving setup as ordinary startup.
            body = text.replace('@echo off\nsetlocal\n', '').replace('\n', '\r\n')
            (destination / name).write_text('@echo off\r\nsetlocal\r\n' + cmd_setup + body, newline='')
        copy(ROOT / 'packaging/RESEARCH_README.txt', destination / 'START-HERE.txt')
    copy(ROOT / 'docs/BACKEND_TASKS.md', destination / 'INSTALL.md')
    files = {p.relative_to(destination).as_posix(): hashlib.sha256(p.read_bytes()).hexdigest()
             for p in sorted(destination.rglob('*')) if p.is_file()}
    (destination / 'manifest.json').write_text(json.dumps({'format': 1, 'variant': 'research' if research else 'base', 'platform': sys.platform, 'files': files}, indent=2) + '\n')



def check_windows_crt(source):
    """Reject VC++ runtime imports, including delay imports, before publishing an installer."""
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


def desktop_compute(source, destination):
    """Installer-only layout, reused by the existing worker adapters unchanged."""
    if sys.platform != 'win32':
        raise ValueError('full desktop installers require Windows')
    for required in ('resources/models/wd_swinv2_v3_emb.onnx',
                     'resources/models/ccip/model_feat.onnx', 'resources/models/ccip/model_metrics.onnx',
                     'resources/models/ccip/metrics.json', 'resources/openblas/openblas.dll',
                     'resources/licenses/openblas/LICENSE.txt', 'resources/onnxruntime/onnxruntime.dll',
                     'resources/licenses/onnxruntime/LICENSE', 'resources/licenses/onnxruntime/ThirdPartyNotices.txt'):
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
    """Copy a complete portable resource layout into the macOS App compute bundle."""
    if sys.platform != 'darwin':
        raise ValueError('macOS desktop computation requires a macOS artifact')
    required = (
        'resources/web/static/index.html', 'resources/tags/tags_v3.csv',
        'resources/models/wd_swinv2_v3_emb.onnx', 'resources/models/ccip/model_feat.onnx',
        'resources/models/ccip/model_metrics.onnx', 'resources/models/ccip/metrics.json',
        'resources/onnxruntime/libonnxruntime.dylib',
        'resources/licenses/onnxruntime/LICENSE',
        'resources/licenses/onnxruntime/ThirdPartyNotices.txt',
        'resources/licenses/pocketfft/LICENSE.md',
        'resources/licenses/LCMS.txt', 'bin/illustash-terms', 'bin/illustash-iqdb',
        *(f'bin/illustash-{kind}' for kind in ('recognition', 'index', 'research', 'provenance')),
        'dependencies/core-Cargo.lock', 'dependencies/iqdb-Cargo.lock',
    )
    manifest = json.loads((source / 'manifest.json').read_text())
    if (manifest.get('format'), manifest.get('variant'), manifest.get('platform')) != (1, 'research', 'darwin'):
        raise ValueError('expected a staged macOS research manifest')
    for relative in required:
        path = source / relative
        if path.is_symlink() or not path.is_file() or relative not in manifest['files']:
            raise ValueError(f'full macOS component missing or unmanifested: {relative}')
    if os.path.lexists(destination):
        raise ValueError('desktop compute output already exists')
    # Validate before creating output; never publish a new manifest for altered input.
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
    parser.add_argument('--models', type=Path, help='explicit model directory (allowlisted WD/CCIP files only)')
    parser.add_argument('--openblas-site', type=Path, help='Windows: site-packages holding the fixed NumPy wheel (numpy.libs OpenBLAS DLL and its licenses)')
    parser.add_argument('--ort-dylib', type=Path, help='explicit existing CPU ORT dynamic library; installed under resources/onnxruntime')
    parser.add_argument('--ort-licenses', type=Path, help='directory containing this ORT distribution LICENSE and ThirdPartyNotices.txt')
    parser.add_argument('--profile', choices=('release', 'dev'), default='release')
    parser.add_argument('--binaries', type=Path, help='use already built release binaries')
    parser.add_argument('--iqdb', type=Path, help='built optional IQDB executable')
    parser.add_argument('--desktop', type=Path, help='optional native desktop executable (portable layout)')
    parser.add_argument('--desktop-resources', type=Path, help='also stage resources for Tauri bundling')
    parser.add_argument('--desktop-compute', type=Path, help='stage full Windows installer computation resources')
    parser.add_argument('--macos-desktop-compute', type=Path, help='stage complete macOS App computation resources')
    args = parser.parse_args()
    if (args.models or args.openblas_site or args.ort_dylib or args.ort_licenses) and not args.research:
        parser.error('models/runtime require --research')
    if bool(args.ort_dylib) != bool(args.ort_licenses):
        parser.error('--ort-dylib and --ort-licenses must be supplied together')
    if args.desktop_compute and not (args.research and args.models and args.openblas_site
                                     and args.ort_dylib and args.ort_licenses):
        parser.error('--desktop-compute requires --research, --models, --openblas-site, --ort-dylib and --ort-licenses')
    if args.macos_desktop_compute and not (sys.platform == 'darwin' and args.research and args.models
                                         and args.ort_dylib and args.ort_licenses):
        parser.error('--macos-desktop-compute requires macOS, --research, --models, --ort-dylib and --ort-licenses')
    for destination in (args.output, args.archive, args.desktop_resources, args.desktop_compute, args.macos_desktop_compute):
        if destination is not None and destination.exists():
            parser.error(f'output already exists: {destination}; choose a fresh path')
    binaries = args.binaries
    if binaries is None:
        subprocess.run(['cargo', 'build', '--locked', '--profile', args.profile, '--manifest-path', str(ROOT / 'native/terms/Cargo.toml'), '--bins'], check=True, cwd=ROOT)
        binaries = target(ROOT / 'native/terms') / ('debug' if args.profile == 'dev' else 'release')
    if args.research and args.iqdb is None:
        subprocess.run(['cargo', 'build', '--locked', '--release', '--manifest-path', str(ROOT / 'crates/iqdb/Cargo.toml')], check=True, cwd=ROOT)
    stage(args.output, binaries, args.research, args.desktop, args.iqdb, args.models, args.openblas_site,
          args.ort_dylib, args.ort_licenses)
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
