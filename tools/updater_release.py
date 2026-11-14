"""应用内更新的发布文件（#279；步骤见 docs/DEVELOPMENT.md「发布」）。

    python3 tools/updater_release.py pack-mac <Illustash.app> <Illustash_X.Y.Z_aarch64.app.tar.gz>
    python3 tools/updater_release.py manifest --mac <tar.gz> --windows <setup.exe> --output latest.json

`pack-mac` 把 .app 打成更新插件要的 tar.gz（第一层是 `Illustash.app/`，不带 macOS 的扩展属性和 `._` 文件）。
`manifest` 读两个包旁边的 `.sig`（`cargo tauri signer sign --app-version X.Y.Z` 写的），写出放进同一个 Release
的 latest.json。它核对每个签名：用的是 desktop/tauri.conf.json 里那把公钥、绑定的版本是 application.json 里的版本、
签的就是这个文件名。签名本身只由用户在自己的机器上用私钥做，这里不碰私钥。
"""
import argparse
import base64
import binascii
import datetime
import json
from pathlib import Path
import sys
import tarfile

ROOT = Path(__file__).resolve().parents[1]
REPOSITORY = 'shio-vinyl/illustash'
# latest.json 里的平台名：更新插件先找 `{os}-{arch}-{installer}`，再找 `{os}-{arch}`。
PLATFORMS = {'mac': 'darwin-aarch64', 'windows': 'windows-x86_64'}


class ReleaseError(Exception):
    pass


def application_version(root=ROOT):
    return json.loads((root / 'native/terms/application.json').read_text(encoding='utf-8'))['version']


def configured_pubkey(root=ROOT):
    config = json.loads((root / 'desktop/tauri.conf.json').read_text(encoding='utf-8'))
    return config.get('plugins', {}).get('updater', {}).get('pubkey', '')


def _lines(text, what):
    """Tauri 的公钥和签名都是 minisign 文本整体再 base64 一次。"""
    try:
        return base64.b64decode(text.strip(), validate=True).decode('utf-8').splitlines()
    except (binascii.Error, UnicodeDecodeError) as error:
        raise ReleaseError(f'{what}不是 Tauri 写的格式') from error


def _key_id(line, what):
    try:
        raw = base64.b64decode(line.strip(), validate=True)
    except binascii.Error as error:
        raise ReleaseError(f'{what}不是 minisign 格式') from error
    if len(raw) < 10:
        raise ReleaseError(f'{what}不是 minisign 格式')
    return raw[2:10]


def pubkey_id(pubkey):
    lines = _lines(pubkey, '公钥')
    if len(lines) < 2:
        raise ReleaseError('公钥不是 Tauri 写的格式')
    return _key_id(lines[1], '公钥')


def check_signature(signature, *, key_id, version, name):
    """签名用的是这把公钥对应的私钥、绑定了这个版本、签的是这个文件名；返回签名原文（写进 latest.json）。"""
    lines = _lines(signature, f'{name} 的签名')
    trusted = next((line[len('trusted comment: '):] for line in lines if line.startswith('trusted comment: ')), None)
    if len(lines) < 4 or trusted is None:
        raise ReleaseError(f'{name} 的签名不是 Tauri 写的格式')
    if _key_id(lines[1], f'{name} 的签名') != key_id:
        raise ReleaseError(f'{name} 不是用 desktop/tauri.conf.json 里那把公钥对应的私钥签的')
    fields = dict(part.split(':', 1) for part in trusted.split('\t') if ':' in part)
    if fields.get('version') != version:
        raise ReleaseError(f'{name} 的签名绑定的版本是 {fields.get("version") or "（没有）"}，不是 {version}；'
                           f'签名时要加 --app-version {version}')
    if fields.get('file') != name:
        raise ReleaseError(f'{name} 的签名是给 {fields.get("file")} 的；先改好文件名再签名')
    return signature.strip()


def pack_mac(app, output):
    """更新插件解开时跳过第一层目录，所以第一层就是 .app 本身。tarfile 不读扩展属性，不会写出 `._` 文件。"""
    app, output = Path(app), Path(output)
    if app.suffix != '.app' or not (app / 'Contents/Info.plist').is_file():
        raise ReleaseError(f'{app} 不是 .app')
    if not output.name.endswith('.app.tar.gz'):
        raise ReleaseError('输出文件名要以 .app.tar.gz 结尾')

    def plain(info):
        info.uid = info.gid = 0
        info.uname = info.gname = ''
        return info

    with tarfile.open(output, 'w:gz', format=tarfile.PAX_FORMAT) as archive:
        archive.add(app, arcname=app.name, filter=plain)


def manifest(*, version, packages, pubkey, notes='', now=None):
    """`packages` 是 {'mac' 或 'windows': (文件名, 签名)}。下载地址按版本号拼出，指向这个版本自己的 Release。"""
    if not pubkey.strip():
        raise ReleaseError('desktop/tauri.conf.json 里还没有更新公钥（plugins.updater.pubkey）')
    key_id = pubkey_id(pubkey)
    base = f'https://github.com/{REPOSITORY}/releases/download/v{version}/'
    platforms = {}
    for kind, (name, signature) in sorted(packages.items()):
        platforms[PLATFORMS[kind]] = {
            'signature': check_signature(signature, key_id=key_id, version=version, name=name),
            'url': base + name,
        }
    now = now or datetime.datetime.now(datetime.timezone.utc)
    return {
        'version': version,
        'notes': notes,
        'pub_date': now.replace(microsecond=0).isoformat().replace('+00:00', 'Z'),
        'platforms': platforms,
    }


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    sub = parser.add_subparsers(dest='command', required=True)
    pack = sub.add_parser('pack-mac', help='把 .app 打成更新用的 tar.gz')
    pack.add_argument('app')
    pack.add_argument('output')
    make = sub.add_parser('manifest', help='写出 latest.json')
    make.add_argument('--mac', type=Path, help='macOS 更新包（.app.tar.gz），旁边要有 .sig')
    make.add_argument('--windows', type=Path, help='Windows 安装包（-setup.exe），旁边要有 .sig')
    make.add_argument('--notes', default='', help='一句更新说明（完整说明写在 Release 正文）')
    make.add_argument('--output', type=Path, required=True)
    args = parser.parse_args(argv)
    try:
        if args.command == 'pack-mac':
            pack_mac(args.app, args.output)
            print(f'已写出 {args.output}')
            return 0
        packages = {}
        for kind in ('mac', 'windows'):
            path = getattr(args, kind)
            if path is None:
                continue
            sig = path.with_name(path.name + '.sig')
            if not path.is_file() or not sig.is_file():
                raise ReleaseError(f'找不到 {path} 或 {sig.name}')
            packages[kind] = (path.name, sig.read_text(encoding='utf-8'))
        if not packages:
            raise ReleaseError('至少给一个平台的包')
        version = application_version()
        value = manifest(version=version, packages=packages, pubkey=configured_pubkey(), notes=args.notes)
        args.output.write_text(json.dumps(value, ensure_ascii=False, indent=2) + '\n', encoding='utf-8')
        print(f'已写出 {args.output}：{version}，{"、".join(value["platforms"])}')
        return 0
    except ReleaseError as error:
        print(error, file=sys.stderr)
        return 1


if __name__ == '__main__':
    sys.exit(main())
