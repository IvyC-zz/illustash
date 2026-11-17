"""角色数据包的签名目录（#602；步骤见 docs/DEVELOPMENT.md「发布角色数据包」）。

    python3 tools/pack_catalog.py keygen ~/.tauri/illustash-packs.key
    python3 tools/pack_catalog.py entry --kind wd-extension --revision 2 --file wd-extension-v2.bin \\
        --min-app 0.3.0 --note-zh '…' --note-en '…' --output wd-extension-v2.entry.json
    python3 tools/pack_catalog.py build --previous packs.json --entry wd-extension-v2.entry.json --output packs.json
    python3 tools/pack_catalog.py sign --key ~/.tauri/illustash-packs.key packs.json
    python3 tools/pack_catalog.py verify packs.json
    python3 tools/pack_catalog.py offline --catalog packs.json --file wd-extension-v2.bin \\
        --output wd-extension-v2-offline.zip

`keygen` 和 `sign` 调用 `cargo tauri signer`（和应用更新同一种 minisign 签名，另一把专签数据包的密钥）。私钥只在
用户自己的电脑上，带密码；这个脚本从不读私钥，也不把它传给别的东西。`keygen` 把公钥写进
native/terms/pack-key.pub，之后要重新构建应用才生效。

`entry` 读包文件本身，写出一条目录条目（格式、头数、二级头数、基础模型的 SHA-256 都从包里读，不手填）。`build` 把条目
并进上一份目录、序号加一。`verify` 用 pack-key.pub 核对签名并按应用的规则检查每个字段；装了 `cryptography` 时完整
核对签名，没装时只核对密钥编号并提示。`offline` 把目录、签名和包文件打成离线包，用户在卡片上「从文件导入…」选它。
"""
import argparse
import base64
import binascii
import hashlib
import json
from pathlib import Path
import re
import subprocess
import sys
import zipfile

ROOT = Path(__file__).resolve().parents[1]
PUBKEY_FILE = ROOT / 'native/terms/pack-key.pub'
CATALOG_FORMAT = 'illustash-packs/1'
RELEASE_PREFIX = 'https://github.com/shio-vinyl/illustash/releases/download/'
CATALOG_NAME = 'packs.json'
SIGNATURE_NAME = 'packs.json.sig'
KINDS = {
    'wd-extension': ('illustash-wd-extension/1', 'illustash-wd-extension/2'),
    'ccip-longtail': ('illustash-557-ccip-offline/1',),
}
WD_DIM = 1024
CCIP_DIM = 768
SEGMENT = re.compile(r'^[A-Za-z0-9][A-Za-z0-9._-]*$')
VERSION = re.compile(r'^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(-[0-9A-Za-z.-]+)?$')
ENTRY_LIMIT = 64
FILE_LIMIT = 512 << 20


class CatalogError(Exception):
    pass


def sha256(data):
    return hashlib.sha256(data).hexdigest()


def hex64(text):
    return isinstance(text, str) and re.fullmatch(r'[0-9a-f]{64}', text) is not None


def segment(text, limit):
    return isinstance(text, str) and 0 < len(text) <= limit and SEGMENT.match(text) is not None and '..' not in text


def short(text, limit):
    return isinstance(text, str) and text.strip() != '' and len(text) <= limit and not any(c < ' ' for c in text)


# ---------- 读包文件 ----------

def inspect_wd(data):
    """WD 扩展包：第一行是清单，后面是 heads × (1024 + 1) 个 float32。"""
    end = data.find(b'\n')
    if end < 0:
        raise CatalogError('不是 WD 扩展包：缺少清单行')
    manifest = json.loads(data[:end])
    labels = manifest.get('labels') or []
    fmt = manifest.get('format')
    if fmt not in KINDS['wd-extension']:
        raise CatalogError(f'WD 扩展包格式 {fmt} 不是应用认识的')
    if len(data) - end - 1 != len(labels) * (WD_DIM + 1) * 4:
        raise CatalogError('WD 扩展包的系数长度和头数对不上')
    tiers = manifest.get('tiers')
    if fmt == 'illustash-wd-extension/1' and tiers is not None:
        raise CatalogError('旧格式的包不能带级别；分级的包用 illustash-wd-extension/2')
    if fmt == 'illustash-wd-extension/2' and (tiers is None or manifest.get('thresholds') is None):
        raise CatalogError('illustash-wd-extension/2 的包必须带逐头的 thresholds 和 tiers')
    for key in ('thresholds', 'tiers'):
        if manifest.get(key) is not None and len(manifest[key]) != len(labels):
            raise CatalogError(f'{key} 的个数和头数对不上')
    if tiers is not None and any(t not in (1, 2) for t in tiers):
        raise CatalogError('级别只能是 1 或 2')
    return {
        'format': fmt,
        'heads': len(labels),
        'candidate_only': sum(1 for t in tiers or [] if t == 2),
        'base': {'wd_model_sha256': manifest['wd_model_sha256'], 'wd_tags_sha256': manifest['wd_tags_sha256']},
    }


def inspect_ccip(path):
    """CCIP 长尾参考包：ZIP 里的溯源、元数据和锚点。"""
    with zipfile.ZipFile(path) as archive:
        provenance = json.loads(archive.read('A-provenance.json'))
        metadata = json.loads(archive.read('A-metadata.json'))
        anchors = archive.read('A-anchors.f32')
    if provenance.get('format') not in KINDS['ccip-longtail']:
        raise CatalogError(f'参考包格式 {provenance.get("format")} 不是应用认识的')
    if len(anchors) != len(metadata) * CCIP_DIM * 4:
        raise CatalogError('参考包的锚点个数和角色数对不上')
    return {
        'format': provenance['format'],
        'heads': len(metadata),
        'candidate_only': 0,
        'base': {'ccip_model_sha256': provenance['ccip_sha256']},
    }


def entry(*, kind, revision, path, min_app, license_text, note_zh=None, note_en=None, version=None, tag=None,
          name=None):
    """从包文件写出一条目录条目。版本名默认 `<种类>-v<修订号>`，Release 的 tag 默认和版本名相同，文件名默认是
    `<版本名>` 加原来的扩展名（CCIP 的参考包照旧叫 A-longtail-p75.zip 也可以，用 --name 指定）。"""
    path = Path(path)
    if kind not in KINDS:
        raise CatalogError(f'未知的种类 {kind}')
    data = path.read_bytes()
    info = inspect_wd(data) if kind == 'wd-extension' else inspect_ccip(path)
    version = version or f'{kind}-v{revision}'
    tag = tag or version
    name = name or version + path.suffix
    result = {
        'kind': kind,
        'revision': revision,
        'version': version,
        'format': info['format'],
        'heads': info['heads'],
        'candidate_only': info['candidate_only'],
        'file': {'name': name, 'size': len(data), 'sha256': sha256(data), 'url': f'{RELEASE_PREFIX}{tag}/{name}'},
        'base': info['base'],
        'min_app': min_app,
        'license': license_text,
    }
    if note_zh or note_en:
        if not (note_zh and note_en):
            raise CatalogError('说明要中英文都写')
        result['note'] = {'zh-CN': note_zh, 'en': note_en}
    check_entry(result)
    return result


# ---------- 目录的规则（和 native/terms/src/app_packs.rs 的 Catalog::parse 一致）----------

def check_entry(e, strict=True):
    """结构的规则和应用一致（不合法时应用整份目录不用）。`strict` 另外拒绝应用会跳过的条目（不认识的种类或格式、
    旧格式带二级头）：发布时不该写进去，测试夹具才用。"""
    f = e.get('file') or {}
    url = f.get('url', '')
    rest = url[len(RELEASE_PREFIX):] if url.startswith(RELEASE_PREFIX) else ''
    tag, _, tail = rest.partition('/')
    ok = (segment(e.get('kind'), 32) and segment(e.get('version'), 64)
          and isinstance(e.get('revision'), int) and e['revision'] >= 1
          and isinstance(e.get('format'), str) and 0 < len(e['format']) <= 64
          and isinstance(e.get('heads'), int) and 1 <= e['heads'] <= 1_000_000
          and isinstance(e.get('candidate_only', 0), int) and 0 <= e.get('candidate_only', 0) <= e['heads']
          and segment(f.get('name'), 128) and isinstance(f.get('size'), int) and 1 <= f['size'] <= FILE_LIMIT
          and hex64(f.get('sha256')) and segment(tag, 64) and tail == f.get('name')
          and isinstance(e.get('min_app'), str) and VERSION.match(e['min_app'])
          and short(e.get('license'), 200)
          and ('note' not in e or (short(e['note'].get('zh-CN'), 200) and short(e['note'].get('en'), 200))))
    if not ok:
        raise CatalogError(f'条目 {e.get("version")} 的字段不合法')
    base = e.get('base') or {}
    if e['kind'] == 'wd-extension' and not (hex64(base.get('wd_model_sha256')) and hex64(base.get('wd_tags_sha256'))):
        raise CatalogError(f'条目 {e["version"]} 缺少 WD 模型或标签表的 SHA-256')
    if e['kind'] == 'ccip-longtail' and not hex64(base.get('ccip_model_sha256')):
        raise CatalogError(f'条目 {e["version"]} 缺少 CCIP 模型的 SHA-256')
    if not strict:
        return
    if e['kind'] not in KINDS or e['format'] not in KINDS[e['kind']]:
        raise CatalogError(f'条目 {e["version"]} 的种类或格式 {e["format"]} 不是应用认识的')
    if e['format'] == 'illustash-wd-extension/1' and e.get('candidate_only', 0):
        raise CatalogError(f'条目 {e["version"]}：旧格式的包全部是一级头')


def check_catalog(catalog, strict=True):
    if catalog.get('format') != CATALOG_FORMAT:
        raise CatalogError('目录格式不对')
    if not isinstance(catalog.get('serial'), int) or catalog['serial'] < 1:
        raise CatalogError('序号须为正整数')
    packs = catalog.get('packs')
    if not isinstance(packs, list) or len(packs) > ENTRY_LIMIT:
        raise CatalogError('条目列表不对')
    seen = set()
    for e in packs:
        check_entry(e, strict)
        keys = {('version', e['version']), ('sha', e['file']['sha256']), ('revision', e['kind'], e['revision'])}
        if keys & seen:
            raise CatalogError(f'条目 {e["version"]} 的版本、修订号或 SHA-256 重复')
        seen |= keys


def build(entries, previous=None, strict=True):
    """把条目并进上一份目录（同版本名的替换），序号加一。"""
    packs = list(previous['packs']) if previous else []
    for e in entries:
        packs = [p for p in packs if p['version'] != e['version']] + [e]
    packs.sort(key=lambda p: (p['kind'], p['revision']))
    catalog = {'format': CATALOG_FORMAT, 'serial': (previous['serial'] + 1) if previous else 1, 'packs': packs}
    check_catalog(catalog, strict)
    return catalog


def dumps(catalog):
    return (json.dumps(catalog, ensure_ascii=False, indent=2) + '\n').encode('utf-8')


# ---------- 签名 ----------

def _decoded_lines(text, what):
    """Tauri 的公钥和签名都是 minisign 文本整体再 base64 一次。"""
    try:
        return base64.b64decode(text.strip(), validate=True).decode('utf-8').splitlines()
    except (binascii.Error, UnicodeDecodeError) as error:
        raise CatalogError(f'{what}不是 Tauri 写的格式') from error


def configured_pubkey(path=PUBKEY_FILE):
    lines = [line.strip() for line in Path(path).read_text(encoding='utf-8').splitlines()]
    keys = [line for line in lines if line and not line.startswith('#')]
    return keys[0] if keys else None


def parse_pubkey(text):
    lines = _decoded_lines(text, '公钥')
    raw = base64.b64decode(lines[1]) if len(lines) >= 2 else b''
    if len(raw) != 42 or raw[:2] not in (b'Ed', b'ED'):
        raise CatalogError('公钥不是 minisign 格式')
    return raw[2:10], raw[10:]


def parse_signature(text):
    lines = _decoded_lines(text, '签名')
    if len(lines) < 4 or not lines[2].startswith('trusted comment: '):
        raise CatalogError('签名不是 minisign 格式')
    sig = base64.b64decode(lines[1])
    glob = base64.b64decode(lines[3])
    if len(sig) != 74 or len(glob) != 64 or sig[:2] != b'ED':
        raise CatalogError('签名不是预哈希的 minisign 格式（cargo tauri signer 写的）')
    return sig[2:10], sig[10:], lines[2][len('trusted comment: '):], glob


def verify(data, signature_text, pubkey_text):
    """返回 True：签名完整核对通过；None：没装 cryptography，只核对了密钥编号。对不上时抛出异常。"""
    key_id, key = parse_pubkey(pubkey_text)
    sig_id, sig, trusted, glob = parse_signature(signature_text)
    if sig_id != key_id:
        raise CatalogError('签名不是用 native/terms/pack-key.pub 那把公钥对应的私钥签的')
    try:
        from cryptography.exceptions import InvalidSignature
        from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PublicKey
    except ImportError:
        return None
    public = Ed25519PublicKey.from_public_bytes(key)
    try:
        public.verify(sig, hashlib.blake2b(data, digest_size=64).digest())
        public.verify(glob, sig + trusted.encode('utf-8'))
    except InvalidSignature as error:
        raise CatalogError('签名对不上这份目录') from error
    return True


def check_signed(catalog_path, signature_path=None, pubkey=None):
    catalog_path = Path(catalog_path)
    signature_path = Path(signature_path or str(catalog_path) + '.sig')
    pubkey = pubkey or configured_pubkey()
    if not pubkey:
        raise CatalogError('native/terms/pack-key.pub 还是占位，先运行 keygen')
    data = catalog_path.read_bytes()
    check_catalog(json.loads(data))
    return verify(data, signature_path.read_text(encoding='utf-8'), pubkey)


def offline(catalog_path, package_path, output, signature_path=None):
    """离线包：根上是 packs.json、packs.json.sig 和包文件（名字照目录里的）。"""
    catalog_path, package_path = Path(catalog_path), Path(package_path)
    signature_path = Path(signature_path or str(catalog_path) + '.sig')
    catalog = json.loads(catalog_path.read_bytes())
    data = package_path.read_bytes()
    found = [p for p in catalog['packs'] if p['file']['sha256'] == sha256(data)]
    if not found:
        raise CatalogError(f'{package_path.name} 不在这份目录里')
    with zipfile.ZipFile(output, 'w', zipfile.ZIP_DEFLATED) as archive:
        for name, body in ((CATALOG_NAME, catalog_path.read_bytes()), (SIGNATURE_NAME, signature_path.read_bytes()),
                           (found[0]['file']['name'], data)):
            archive.writestr(zipfile.ZipInfo(name, date_time=(2026, 1, 1, 0, 0, 0)), body)


def write_pubkey(pubkey, path=PUBKEY_FILE):
    parse_pubkey(pubkey)
    lines = [line for line in Path(path).read_text(encoding='utf-8').splitlines() if line.startswith('#')]
    lines = [line for line in lines if '占位' not in line and '只有说明行' not in line]
    Path(path).write_text('\n'.join(lines + [pubkey.strip()]) + '\n', encoding='utf-8')


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    sub = parser.add_subparsers(dest='command', required=True)
    keygen = sub.add_parser('keygen', help='生成专签数据包的密钥对，公钥写进 native/terms/pack-key.pub')
    keygen.add_argument('key', type=Path, help='私钥写到哪里（只留在本机，例如 ~/.tauri/illustash-packs.key）')
    make = sub.add_parser('entry', help='从包文件写出一条目录条目')
    make.add_argument('--kind', required=True, choices=sorted(KINDS))
    make.add_argument('--revision', required=True, type=int)
    make.add_argument('--file', required=True, type=Path)
    make.add_argument('--min-app', required=True)
    make.add_argument('--license', default=None, help='默认：WD 扩展 Apache-2.0，CCIP 长尾 MIT')
    make.add_argument('--note-zh')
    make.add_argument('--note-en')
    make.add_argument('--version')
    make.add_argument('--tag')
    make.add_argument('--name')
    make.add_argument('--output', type=Path, required=True)
    put = sub.add_parser('build', help='把条目并进目录，序号加一')
    put.add_argument('--previous', type=Path, help='上一份目录（第一次发布时省略）')
    put.add_argument('--entry', type=Path, action='append', required=True)
    put.add_argument('--output', type=Path, required=True)
    sign = sub.add_parser('sign', help='用 cargo tauri signer 签名并核对')
    sign.add_argument('--key', type=Path, required=True)
    sign.add_argument('catalog', type=Path)
    check = sub.add_parser('verify', help='核对目录的签名和字段')
    check.add_argument('catalog', type=Path)
    check.add_argument('--signature', type=Path)
    check.add_argument('--pubkey', help='默认读 native/terms/pack-key.pub')
    bundle = sub.add_parser('offline', help='把目录、签名和包文件打成离线包')
    bundle.add_argument('--catalog', type=Path, required=True)
    bundle.add_argument('--file', type=Path, required=True)
    bundle.add_argument('--output', type=Path, required=True)
    args = parser.parse_args(argv)
    try:
        if args.command == 'keygen':
            if args.key.exists():
                raise CatalogError(f'{args.key} 已经存在；换个位置，或确认不再需要它之后自己删掉')
            # 交互地问密码；私钥由 tauri 写到用户指定的位置，这里不读它。
            subprocess.run(['cargo', 'tauri', 'signer', 'generate', '-w', str(args.key)], check=True,
                           cwd=ROOT / 'desktop')
            pubkey = Path(str(args.key) + '.pub').read_text(encoding='utf-8').strip()
            write_pubkey(pubkey)
            print(f'公钥已写进 {PUBKEY_FILE.relative_to(ROOT)}；私钥在 {args.key}，只留在这台电脑上。')
        elif args.command == 'entry':
            license_text = args.license or ('Apache-2.0' if args.kind == 'wd-extension' else 'MIT')
            result = entry(kind=args.kind, revision=args.revision, path=args.file, min_app=args.min_app,
                           license_text=license_text, note_zh=args.note_zh, note_en=args.note_en,
                           version=args.version, tag=args.tag, name=args.name)
            args.output.write_text(json.dumps(result, ensure_ascii=False, indent=2) + '\n', encoding='utf-8')
            print(f'已写出 {args.output}：{result["version"]}，{result["heads"]} 个头，SHA-256 {result["file"]["sha256"]}')
        elif args.command == 'build':
            previous = json.loads(args.previous.read_bytes()) if args.previous else None
            entries = [json.loads(path.read_bytes()) for path in args.entry]
            catalog = build(entries, previous)
            args.output.write_bytes(dumps(catalog))
            print(f'已写出 {args.output}：序号 {catalog["serial"]}，{len(catalog["packs"])} 个条目')
        elif args.command == 'sign':
            check_catalog(json.loads(args.catalog.read_bytes()))
            subprocess.run(['cargo', 'tauri', 'signer', 'sign', '-f', str(args.key.resolve()),
                            str(args.catalog.resolve())], check=True, cwd=ROOT / 'desktop')
            result = check_signed(args.catalog)
            print(f'已写出 {args.catalog}.sig' + ('，签名核对通过' if result else '；没装 cryptography，只核对了密钥编号'))
        elif args.command == 'verify':
            result = check_signed(args.catalog, args.signature, args.pubkey)
            print('目录和签名核对通过' if result else '目录核对通过；没装 cryptography，签名只核对了密钥编号')
        else:
            if not check_signed(args.catalog):
                print('提示：没装 cryptography，签名只核对了密钥编号', file=sys.stderr)
            offline(args.catalog, args.file, args.output)
            print(f'已写出 {args.output}')
        return 0
    except (CatalogError, OSError, ValueError, KeyError, subprocess.CalledProcessError) as error:
        print(f'pack_catalog: {error}', file=sys.stderr)
        return 1


if __name__ == '__main__':
    sys.exit(main())
