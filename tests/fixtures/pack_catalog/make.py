"""生成数据包目录（#602）的测试夹具：两把测试密钥、合成的包、签好名的几份目录和离线包。全部是合成数据。

    python3 tests/fixtures/pack_catalog/make.py

要装 `cryptography`（只在重新生成夹具时用；测试只读生成好的文件）。Ed25519 签名是确定的，ZIP 成员的时间固定，所以重跑
得到同样的字节。测试密钥的种子写在这里，谁都能用它签，只能用来测试：应用只在 ILLUSTASH_UPDATE_BASE 指向本机测试
服务时才认 ILLUSTASH_TEST_PACKS_PUBKEY 换进来的公钥。

`tauri-key.pub` 和 `packs.json.tauri.sig` 由 `cargo tauri signer` 生成（一次性的测试密钥，私钥没有留下），用来确认应用认
的格式和用户发包时用的工具写出的一致；这个脚本不重写它们，只在目录内容变了时提示重新生成。
"""
import base64
import hashlib
import json
import math
from pathlib import Path
import random
import struct
import sys
import zipfile

from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parents[2]))
from tools import pack_catalog  # noqa: E402

# 这个构建的模型身份（native/terms/models.json、resources/tags/tags_v3.csv）：目录里的包要和它对得上才算可用。
WD_MODEL = 'ae25f5f12b2d5891728fb7768fa63d65bf13a83d6d4823e3a1bf7b42875c74ff'
WD_TAGS = '298633d94d0031d2081c0893f29c82eab7f0df00b08483ba8f29d1e979441217'
CCIP_MODEL = '4ea118d16496274f4f6e08d3afc768cc592389e8f7f32f8732ce2215c228ac5f'
MIN_APP = '0.2.0'
TRUSTED = 'timestamp:1763337600\tfile:packs.json'
DATE = (2026, 11, 17, 0, 0, 0)


def sha(data):
    return hashlib.sha256(data).hexdigest()


class Key:
    def __init__(self, name):
        self.private = Ed25519PrivateKey.from_private_bytes(hashlib.sha256(f'illustash pack test key {name}'.encode()).digest())
        self.id = hashlib.sha256(f'illustash pack test key id {name}'.encode()).digest()[:8]

    def pubkey(self):
        raw = self.private.public_key().public_bytes_raw()
        text = f'untrusted comment: minisign public key: {self.id[::-1].hex().upper()}\n' \
               + base64.b64encode(b'Ed' + self.id + raw).decode() + '\n'
        return base64.b64encode(text.encode()).decode()

    def sign(self, data):
        """和 `cargo tauri signer sign` 一样：预哈希（BLAKE2b-512）的 minisign 签名，整体再 base64。"""
        signature = self.private.sign(hashlib.blake2b(data, digest_size=64).digest())
        glob = self.private.sign(signature + TRUSTED.encode())
        text = 'untrusted comment: signature from tauri secret key\n' \
               + base64.b64encode(b'ED' + self.id + signature).decode() + '\n' \
               + f'trusted comment: {TRUSTED}\n' + base64.b64encode(glob).decode() + '\n'
        return base64.b64encode(text.encode())


def wd_pack(fmt, heads):
    """合成 WD 扩展包：每个头只看向量的一维（权重 2，偏置 0），阈值 0.8。"""
    header = {'format': fmt, 'wd_model_sha256': WD_MODEL, 'wd_tags_sha256': WD_TAGS, 'labels': [h[0] for h in heads]}
    if fmt == 'illustash-wd-extension/1':
        header['threshold'] = 0.8008725047111511
    header['thresholds'] = [0.8] * len(heads)
    if fmt == 'illustash-wd-extension/2':
        header['tiers'] = [h[2] for h in heads]
    weights = b''.join(struct.pack('<1024f', *[2. if i == axis else 0. for i in range(1024)]) for _, axis, _ in heads)
    bias = struct.pack(f'<{len(heads)}f', *([0.] * len(heads)))
    return json.dumps(header).encode() + b'\n' + weights + bias


def ccip_pack(tags):
    rnd = random.Random('pack-catalog-ccip')
    anchors = b''
    for _ in tags:
        v = [rnd.gauss(0, 1) for _ in range(768)]
        norm = math.sqrt(sum(x * x for x in v))
        anchors += struct.pack('<768f', *(x / norm for x in v))
    metadata = json.dumps([{'id': t, 'tag': t, 'skin': False, 'post_count': 20, 'gender_conf': 0.5,
                            'p50': 0.1, 'p75': 0.2, 'p90': 0.3} for t in tags]).encode()
    provenance = json.dumps({'format': 'illustash-557-ccip-offline/1', 'ccip_sha256': CCIP_MODEL,
                             'files': {'A-metadata.json': sha(metadata), 'A-anchors.f32': sha(anchors)}}).encode()
    path = HERE / 'ccip-longtail-v2.zip'
    with zipfile.ZipFile(path, 'w', zipfile.ZIP_STORED) as z:
        for member, body in (('A-provenance.json', provenance), ('A-metadata.json', metadata), ('A-anchors.f32', anchors)):
            z.writestr(zipfile.ZipInfo(member, date_time=DATE), body)
    return path


def placeholder(kind, revision, fmt, base, min_app=MIN_APP):
    """不提供的条目：结构合法，但种类、格式、模型或版本不对。没有对应的文件。"""
    version = f'{kind}-v{revision}'
    name = version + '.bin'
    return {'kind': kind, 'revision': revision, 'version': version, 'format': fmt, 'heads': 1, 'candidate_only': 0,
            'file': {'name': name, 'size': 8, 'sha256': sha(version.encode()),
                     'url': f'{pack_catalog.RELEASE_PREFIX}{version}/{name}'},
            'base': base, 'min_app': min_app, 'license': 'Apache-2.0'}


def write(name, data):
    (HERE / name).write_bytes(data)


def main():
    a, b = Key('A'), Key('B')
    write('key-a.pub', (a.pubkey() + '\n').encode())
    write('key-b.pub', (b.pubkey() + '\n').encode())
    write('wd-extension-v2.bin', wd_pack('illustash-wd-extension/1',
                                         [('packone_(synthetic)', 0, 1), ('packtwo_(synthetic)', 1, 1)]))
    write('wd-extension-v3.bin', wd_pack('illustash-wd-extension/2', [('packone_(synthetic)', 0, 1),
                                                                     ('packtwo_(synthetic)', 1, 1),
                                                                     ('packthree_(synthetic)', 2, 2)]))
    ccip = ccip_pack(['longpack_(synthetic)', 'longpacktwo_(synthetic)'])
    wd_base = {'wd_model_sha256': WD_MODEL, 'wd_tags_sha256': WD_TAGS}
    note = lambda n: {'note_zh': f'合成测试包 {n}', 'note_en': f'Synthetic test pack {n}'}
    good = [
        pack_catalog.entry(kind='wd-extension', revision=2, path=HERE / 'wd-extension-v2.bin', min_app=MIN_APP,
                           license_text='Apache-2.0', **note(2)),
        pack_catalog.entry(kind='wd-extension', revision=3, path=HERE / 'wd-extension-v3.bin', min_app=MIN_APP,
                           license_text='Apache-2.0', **note(3)),
        pack_catalog.entry(kind='ccip-longtail', revision=2, path=ccip, min_app=MIN_APP, license_text='MIT'),
    ]
    rejected = [
        placeholder('wd-extension', 4, 'illustash-wd-extension/1', wd_base, min_app='99.0.0'),
        placeholder('wd-extension', 5, 'illustash-wd-extension/9', wd_base),
        placeholder('wd-extension', 6, 'illustash-wd-extension/1', {**wd_base, 'wd_tags_sha256': sha(b'other tags')}),
        placeholder('future-pack', 1, 'illustash-future/1', {}),
    ]
    catalogs = {
        'packs.json': pack_catalog.build(good + rejected, strict=False),
        'packs-2.json': {**pack_catalog.build(good + rejected[:3], strict=False), 'serial': 2},
        'packs-1-alt.json': pack_catalog.build(good + rejected[:2], strict=False),
    }
    for name, catalog in catalogs.items():
        data = pack_catalog.dumps(catalog)
        write(name, data)
        write(name + '.sig', a.sign(data))
    write('packs.json.sig-b', b.sign((HERE / 'packs.json').read_bytes()))
    for kind, package in (('wd-extension-v2', HERE / 'wd-extension-v2.bin'), ('ccip-longtail-v2', ccip)):
        pack_catalog.offline(HERE / 'packs.json', package, HERE / f'{kind}-offline.zip')
    tauri = HERE / 'packs.json.tauri.sig'
    if tauri.exists():
        try:
            pack_catalog.verify((HERE / 'packs.json').read_bytes(), tauri.read_text(), (HERE / 'tauri-key.pub').read_text())
        except pack_catalog.CatalogError:
            print('packs.json 变了：用 cargo tauri signer 重新生成 tauri-key.pub 和 packs.json.tauri.sig（见文件开头）')


if __name__ == '__main__':
    main()
