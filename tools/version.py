"""The version number has one source: native/terms/application.json (#189).

Every other place that carries it (tauri.conf.json, the Cargo.toml of each shipped crate, and those crates'
entries in every Cargo.lock) is rewritten from it; packaging/build.py refuses to package when they disagree.

    python3 tools/version.py              # check only; exit 1 and list each mismatch
    python3 tools/version.py --sync       # rewrite every other place from application.json
    python3 tools/version.py --set 0.2.0  # change application.json, then sync
"""
import argparse
import json
from pathlib import Path
import re
import sys

ROOT = Path(__file__).resolve().parents[1]
SOURCE = 'native/terms/application.json'
TAURI = 'desktop/tauri.conf.json'
CRATES = ('native/terms', 'desktop', 'crates/core', 'crates/platform', 'crates/iqdb')
# SemVer 2.0.0 without build metadata: a prerelease suffix (0.2.0-beta.1) is allowed, `+build` is not.
SEMVER = re.compile(r'(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)'
                    r'(?:-((?:0|[1-9]\d*|\d*[A-Za-z-][0-9A-Za-z-]*)(?:\.(?:0|[1-9]\d*|\d*[A-Za-z-][0-9A-Za-z-]*))*))?')
# The [package] table up to the next table header; its first `version = "…"` line is the crate's own.
PACKAGE_VERSION = re.compile(r'(^\[package\]\n(?:(?!\[).*\n)*?version = ")([^"]*)(")', re.M)
TAURI_VERSION = re.compile(r'(^  "version": ")([^"]*)(")', re.M)
# A lock entry for one of this project's crates: local path packages carry no `source = …` line.
LOCK_ENTRY = re.compile(r'(^\[\[package\]\]\nname = "(illustash-[^"]+)"\nversion = ")([^"]*)("\n(?!source = ))', re.M)


def valid(version):
    return isinstance(version, str) and SEMVER.fullmatch(version) is not None


def read(root, relative):
    # Byte-exact round trip: the files are LF (.gitattributes) and must stay so on Windows too.
    return (root / relative).read_bytes().decode('utf-8')


def write(root, relative, text):
    (root / relative).write_bytes(text.encode('utf-8'))


def source_version(root=ROOT):
    return json.loads(read(root, SOURCE))['version']


def places(root=ROOT):
    """(relative path, label, version found) for every place that must equal application.json."""
    found = []
    tauri = TAURI_VERSION.findall(read(root, TAURI))
    found.append((TAURI, 'version', tauri[0][1] if len(tauri) == 1 else None))
    for crate in CRATES:
        manifest = f'{crate}/Cargo.toml'
        package = PACKAGE_VERSION.search(read(root, manifest))
        found.append((manifest, '[package] version', package.group(2) if package else None))
        lock = f'{crate}/Cargo.lock'
        entries = LOCK_ENTRY.findall(read(root, lock))
        if not entries:
            found.append((lock, 'illustash-*', None))
        found.extend((lock, name, version) for _, name, version, _ in entries)
    return found


def check(root=ROOT):
    """Messages for every disagreement; empty when consistent."""
    try:
        version = source_version(root)
    except (OSError, ValueError, KeyError, TypeError) as error:
        return [f'{SOURCE}: 读不到版本号：{error}']
    if not valid(version):
        return [f'{SOURCE}: 版本号 {version!r} 不是 SemVer（例：0.2.0、0.2.0-beta.1）']
    errors = []
    for path, label, found in places(root):
        if found is None:
            errors.append(f'{path}: 找不到 {label}')
        elif found != version:
            errors.append(f'{path}: {label} 是 {found}，应与 {SOURCE} 的 {version} 一致')
    return errors


def sync(root=ROOT, version=None):
    """Rewrite every place from application.json (after setting it to `version` when given)."""
    if version is not None:
        if not valid(version):
            raise ValueError(f'版本号 {version!r} 不是 SemVer（例：0.2.0、0.2.0-beta.1）')
        metadata = json.loads(read(root, SOURCE))
        metadata['version'] = version
        write(root, SOURCE, json.dumps(metadata, ensure_ascii=False, separators=(',', ':')) + '\n')
    version = source_version(root)
    if not valid(version):
        raise ValueError(f'{SOURCE}: 版本号 {version!r} 不是 SemVer')
    put = lambda match: match.group(1) + version + match.group(3)
    rewrites = [(TAURI, TAURI_VERSION, put)]
    for crate in CRATES:
        rewrites.append((f'{crate}/Cargo.toml', PACKAGE_VERSION, put))
        rewrites.append((f'{crate}/Cargo.lock', LOCK_ENTRY, lambda m: m.group(1) + version + m.group(4)))
    for relative, pattern, replace in rewrites:
        text = read(root, relative)
        updated, count = pattern.subn(replace, text, count=0 if relative.endswith('.lock') else 1)
        if not count:
            raise ValueError(f'{relative}: 找不到版本号所在行')
        if updated != text:
            write(root, relative, updated)
    return version


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    group = parser.add_mutually_exclusive_group()
    group.add_argument('--sync', action='store_true', help='rewrite every other place from application.json')
    group.add_argument('--set', metavar='VERSION', help='set application.json to VERSION, then sync')
    args = parser.parse_args(argv)
    if args.sync or args.set:
        try:
            version = sync(version=args.set)
        except (OSError, ValueError, KeyError) as error:
            print(error, file=sys.stderr)
            return 1
        print(f'版本号 {version} 已同步到 {TAURI}、{len(CRATES)} 个 Cargo.toml 和对应的 Cargo.lock')
    errors = check()
    for message in errors:
        print(message, file=sys.stderr)
    if not errors:
        print(f'版本号一致：{source_version()}')
    return int(bool(errors))


if __name__ == '__main__':
    raise SystemExit(main())
