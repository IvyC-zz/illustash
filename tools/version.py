"""版本号只有一个来源：native/terms/application.json（#189）。

其他带版本号的地方（tauri.conf.json、每个自有 crate 的 Cargo.toml，以及这些 crate 在根 Cargo.lock 里的条目）
都从它改写；两边不一致时 packaging/build.py 拒绝打包。

    python3 tools/version.py              # 只检查；不一致时退出码为 1 并列出每一处
    python3 tools/version.py --sync       # 按 application.json 改写其他所有地方
    python3 tools/version.py --set 0.2.0  # 修改 application.json，然后同步
"""
import argparse
import json
from pathlib import Path
import re
import sys

ROOT = Path(__file__).resolve().parents[1]
SOURCE = 'native/terms/application.json'
TAURI = 'desktop/tauri.conf.json'
CRATES = ('native/terms', 'desktop', 'crates/core', 'crates/platform', 'crates/iqdb', 'crates/relation-fft')
LOCK = 'Cargo.lock'
# 不带构建元数据的 SemVer 2.0.0：允许预发布后缀（0.2.0-beta.1），不允许 `+build`。
SEMVER = re.compile(r'(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)'
                    r'(?:-((?:0|[1-9]\d*|\d*[A-Za-z-][0-9A-Za-z-]*)(?:\.(?:0|[1-9]\d*|\d*[A-Za-z-][0-9A-Za-z-]*))*))?')
# [package] 表直到下一个表头为止；其中第一行 `version = "…"` 是 crate 自己的版本。
PACKAGE_VERSION = re.compile(r'(^\[package\]\n(?:(?!\[).*\n)*?version = ")([^"]*)(")', re.M)
TAURI_VERSION = re.compile(r'(^  "version": ")([^"]*)(")', re.M)
# 本项目某个 crate 的 lock 条目：本地路径包没有 `source = …` 这一行。
LOCK_ENTRY = re.compile(r'(^\[\[package\]\]\nname = "(illustash-[^"]+)"\nversion = ")([^"]*)("\n(?!source = ))', re.M)


def valid(version):
    return isinstance(version, str) and SEMVER.fullmatch(version) is not None


def read(root, relative):
    # 逐字节原样读写：这些文件是 LF（.gitattributes），在 Windows 上也必须保持 LF。
    return (root / relative).read_bytes().decode('utf-8')


def write(root, relative, text):
    (root / relative).write_bytes(text.encode('utf-8'))


def source_version(root=ROOT):
    return json.loads(read(root, SOURCE))['version']


def places(root=ROOT):
    """每处必须与 application.json 相同的地方，给出 (relative path, label, version found)。"""
    found = []
    tauri = TAURI_VERSION.findall(read(root, TAURI))
    found.append((TAURI, 'version', tauri[0][1] if len(tauri) == 1 else None))
    for crate in CRATES:
        manifest = f'{crate}/Cargo.toml'
        package = PACKAGE_VERSION.search(read(root, manifest))
        found.append((manifest, '[package] version', package.group(2) if package else None))
    entries = LOCK_ENTRY.findall(read(root, LOCK))
    if not entries:
        found.append((LOCK, 'illustash-*', None))
    found.extend((LOCK, name, version) for _, name, version, _ in entries)
    return found


def check(root=ROOT):
    """每处不一致的消息；全部一致时为空。"""
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
    """按 application.json 改写每一处（给了 `version` 时先把 application.json 设为它）。"""
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
    rewrites.append((LOCK, LOCK_ENTRY, lambda m: m.group(1) + version + m.group(4)))
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
        print(f'版本号 {version} 已同步到 {TAURI}、{len(CRATES)} 个 Cargo.toml 和根 Cargo.lock')
    errors = check()
    for message in errors:
        print(message, file=sys.stderr)
    if not errors:
        print(f'版本号一致：{source_version()}')
    return int(bool(errors))


if __name__ == '__main__':
    raise SystemExit(main())
