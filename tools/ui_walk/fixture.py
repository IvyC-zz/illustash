"""走查用的合成图库（#453）：全部是脚本生成的图和数据，不用真实图库、不用识别模型、不联网。

每次运行重新生成一份（十几秒），每条流程开始前恢复成刚生成的样子，所以流程之间互不影响。生成是确定的：同样的代码
生成同样的图、同样的识别结果和归属；只有 uuid 每次不同，走查的快照里一律换成文件名（Fixture.names）。

数据只经产品自己的入口写进图库，和真实使用时走同一条路：
1. `library-create` 建库、`library-scan` 同步；
2. `recognition-import` 导入一个合成的识别结果包（WD 评级与标签、WD/CCIP 向量，格式见 docs/RESULT_PACKAGES.md）。
   模型文件和标签表是假的，只有它们的 SHA-256 写进清单，宿主据此接受这个包，从不加载它们；
3. `import` 导入一个合成的画师证据包（docs/EVIDENCE_PACKAGES.md）；
4. 起宿主（带研究 worker 时）重新分组、找相似，再经 HTTP 接口建角色、确认归属、跳过和排除，和页面上点按钮一样。

内容（SPEC）：人工确认的角色；WD 认出的角色和「也可能是」；几组 CCIP 相近、没有角色的图（按组整理）；
缩放、局部修改的副本（相似的图）和字节相同的副本（重复）；R18 评级；画面标签；画师（机器认定和「也许是」）；
已跳过、已排除的图。随机抽查和身份整理的题在新建图库里没有写入方，造不出来（#453 的说明）。
"""
from __future__ import annotations

import hashlib
import http.cookiejar
import json
import math
import os
import random
import shutil
import sqlite3
import struct
import subprocess
import time
import urllib.error
import urllib.request
import zipfile
import zlib
from dataclasses import dataclass, field
from pathlib import Path

from tools import dev_preview
from tools.ui_walk.host import Host

# ---------- 内容 ----------
# 名字都是造词，不会和真实角色、画师重名；中英文界面里显示一样。
HUMAN = [('Aster Vale', 'aster', 12), ('Briar Quill', 'briar', 9), ('Cobalt Wren', 'cobalt', 6)]
# WD 认出的角色：(标签, 文件名前缀, 张数)。第二个只作为「也可能是」出现在前一个的几张图上。
MACHINE = [('dahlia_moor', 'dahlia', 8)]
ALSO = ('ember_lark', 3)
GROUPS = [7, 5, 4, 3]            # 没有角色、彼此相近的几组（按组整理）
PLAIN = 48                       # 没有角色、也不成组的图
SIMILAR = [('scene-a', ['small', 'edit']), ('scene-b', ['edit'])]   # 原图和它的变体（相似的图）
COPIES = ['plain-001', 'plain-002']                                  # 字节相同的副本（重复）
R18 = {'plain-040': 'explicit', 'plain-041': 'questionable', 'plain-042': 'explicit', 'aster-12': 'questionable'}
GENERAL = {'blue_hair': ('aster', 'group-1'), 'smile': ('briar', 'plain-00'), 'outdoors': ('scene-',)}
ARTISTS = [('synthetic_painter', 'Synthetic Painter', ('aster', 'briar-0')), ('mock_atelier', 'Mock Atelier', ('plain-01', 'scene-'))]
MAYBE_ARTIST = ('quiet_brush', 'Quiet Brush', ('cobalt',))   # 只有 OCR 一类的弱证据：显示为「也许是」
SKIPPED = ['plain-045', 'plain-046']
EXCLUDED = ['plain-047', 'plain-048']

CSV = ['name,category', 'general,9', 'sensitive,9', 'questionable,9', 'explicit,9', 'solo,0', 'blue_hair,0', 'smile,0',
       'outdoors,0', 'dahlia_moor,4', 'ember_lark,4']
WD_DIM, CCIP_DIM = 1024, 768


@dataclass
class Fixture:
    root: Path                                                 # images/（共用、只读）、live/（流程用）、pristine/（存档）
    names: dict[str, str] = field(default_factory=dict)        # uuid → 文件名（不带扩展名）
    chars: list[dict] = field(default_factory=list)            # 人工确认的角色 [{name, id, images}]
    machine: list[dict] = field(default_factory=list)          # WD 认出的角色 [{name, id, images}]
    groups: list[dict] = field(default_factory=list)            # 未识别分组 [{id, images（文件名）}]
    similar: list[dict] = field(default_factory=list)           # 相似组 [{id, images（封面的文件名）}]
    summary: str = ''

    @property
    def live(self) -> Path:
        return self.root / 'live'

    @property
    def library(self) -> Path:
        return self.live / '合成图库.pilib'

    def reset(self) -> Path:
        """把 live/ 恢复成刚生成的样子（图库、隔离的 HOME 和应用数据），返回图库路径。路径每次一样，快照里的路径也一样。"""
        shutil.rmtree(self.live, ignore_errors=True)
        shutil.copytree(self.root / 'pristine', self.live, symlinks=True)
        return self.library

    def saved(self) -> dict:
        return {'names': self.names, 'chars': self.chars, 'machine': self.machine, 'groups': self.groups,
                'similar': self.similar, 'summary': self.summary}

    @classmethod
    def load(cls, root: Path) -> 'Fixture':
        data = json.loads((root / 'fixture.json').read_text(encoding='utf-8'))
        return cls(root, **data)

    def contract(self) -> dict:
        """交给走查流程的夹具说明。"""
        return {'chars': self.chars, 'machine': self.machine, 'groups': self.groups,
                'charIds': {c['name']: c['id'] for c in self.chars + self.machine},
                'similar': self.similar, 'copies': COPIES, 'r18': sorted(R18),
                'skipped': SKIPPED, 'excluded': EXCLUDED}


# ---------- 图 ----------

def scene(path: Path, width: int, height: int, seed: int, edit: bool = False) -> None:
    """一张合成图：两色渐变底加几个色块和圆，形状按比例摆放（同一个 seed 换尺寸画出来是同一张图的缩放）。
    edit 时在右下角多一块，做「局部修改过」的版本。"""
    rnd = random.Random(seed)
    a = [rnd.randrange(40, 230) for _ in range(3)]
    b = [rnd.randrange(40, 230) for _ in range(3)]
    rows = [bytearray() for _ in range(height)]
    for y in range(height):
        t = y / max(1, height - 1)
        rows[y] = bytearray(bytes(int(a[c] + (b[c] - a[c]) * t) for c in range(3)) * width)
    shapes = []
    for _ in range(rnd.randrange(3, 6)):
        color = bytes(rnd.randrange(0, 256) for _ in range(3))
        x0, y0 = rnd.random() * 0.8, rnd.random() * 0.8
        shapes.append((rnd.choice('rc'), x0, y0, x0 + 0.1 + rnd.random() * 0.3, y0 + 0.1 + rnd.random() * 0.3, color))
    if edit:
        shapes.append(('r', 0.78, 0.78, 0.95, 0.95, b'\xff\x20\x60'))
    for kind, x0, y0, x1, y1, color in shapes:
        px0, py0, px1, py1 = int(x0 * width), int(y0 * height), min(width, int(x1 * width)), min(height, int(y1 * height))
        cx, cy, rx, ry = (px0 + px1) / 2, (py0 + py1) / 2, max(1, (px1 - px0) / 2), max(1, (py1 - py0) / 2)
        for y in range(py0, py1):
            if kind == 'r':
                lo, hi = px0, px1
            else:
                d = 1 - ((y + 0.5 - cy) / ry) ** 2
                if d <= 0:
                    continue
                half = rx * math.sqrt(d)
                lo, hi = max(0, int(cx - half)), min(width, int(cx + half))
            rows[y][lo * 3:hi * 3] = color * (hi - lo)
    raw = b''.join(b'\x00' + bytes(r) for r in rows)

    def chunk(kind: bytes, data: bytes) -> bytes:
        return struct.pack('>I', len(data)) + kind + data + struct.pack('>I', zlib.crc32(kind + data))

    header = struct.pack('>IIBBBBB', width, height, 8, 2, 0, 0, 0)
    path.write_bytes(b'\x89PNG\r\n\x1a\n' + chunk(b'IHDR', header) + chunk(b'IDAT', zlib.compress(raw, 6)) + chunk(b'IEND', b''))


@dataclass
class Item:
    name: str
    width: int
    height: int
    seed: int
    vector: str                      # CCIP 向量的「方向」：同一方向的图彼此相近
    edit: bool = False
    copy_of: str = ''


def items() -> list[Item]:
    out: list[Item] = []
    seed = 100

    def add(name: str, vector: str, **kw) -> None:
        nonlocal seed
        seed += 1
        i = len(out)
        out.append(Item(name, 240 + (i % 3) * 60, 180 + (i % 4) * 45, seed, vector, **kw))

    for _, slug, n in HUMAN:
        for i in range(n):
            add(f'{slug}-{i + 1:02d}', slug)
    for tag, slug, n in MACHINE:
        for i in range(n):
            add(f'{slug}-{i + 1:02d}', slug)
    for g, n in enumerate(GROUPS, 1):
        for i in range(n):
            add(f'group-{g}-{i + 1:02d}', f'group-{g}')
    for i in range(PLAIN):
        add(f'plain-{i + 1:03d}', f'plain-{i + 1:03d}')
    for base, variants in SIMILAR:
        add(base, base)
        source = out[-1]
        for v in variants:
            scale = 0.75 if v == 'small' else 1
            out.append(Item(f'{base}-{v}', int(source.width * scale), int(source.height * scale), source.seed, base,
                            edit=v == 'edit'))
    for name in COPIES:
        source = next(x for x in out if x.name == name)
        out.append(Item(f'{name}-copy', source.width, source.height, source.seed, source.vector, copy_of=name))
    return out


def vectors(names: list[str], directions: list[str], dim: int, seed: int) -> bytes:
    """每张图一个单位向量：同一方向的图在同一个随机基向量附近（余弦约 0.97），不同方向几乎正交。返回 .npy 字节。"""
    rnd = random.Random(seed)
    bases: dict[str, list[float]] = {}
    close = {d for n, d in zip(names, directions) if n.endswith(('-small', '-edit', '-copy'))}
    rows = []
    for name, direction in zip(names, directions):
        if direction not in bases:
            bases[direction] = [rnd.gauss(0, 1) for _ in range(dim)]
        noise = random.Random(f'{seed}-{name}')
        # 原图和它的变体、副本几乎一样（余弦 > 0.99），组内的图噪声大一些（约 0.97）。
        level = 0.03 if direction in close else 0.25
        v = [x + noise.gauss(0, level) for x in bases[direction]]
        norm = math.sqrt(sum(x * x for x in v))
        rows.append(struct.pack(f'<{dim}f', *(x / norm for x in v)))
    header = "{'descr': '<f4', 'fortran_order': False, 'shape': (%d, %d), }" % (len(names), dim)
    header += ' ' * (63 - (len(header) + 10) % 64) + '\n'
    return b'\x93NUMPY\x01\x00' + struct.pack('<H', len(header)) + header.encode() + b''.join(rows)


def sha(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


# ---------- 包 ----------

def recognition_package(dest: Path, resources: Path, library_id: str, assets: list[tuple[str, str, Path]], direction: dict) -> Path:
    """合成识别结果包：每张图一条 rating，部分图带画面标签和 WD 角色标签。assets 是 (uuid, 文件名, 路径)。"""
    dest.mkdir(parents=True)
    labels = []
    for uuid, name, _ in assets:
        labels.append({'asset': uuid, 'dim': 'rating', 'value': R18.get(name, 'general'), 'confidence': 0.9})
        if not name.startswith(('group-', 'plain-', 'scene-')):
            labels.append({'asset': uuid, 'dim': 'general', 'value': 'solo', 'confidence': 0.95})
        for tag, prefixes in GENERAL.items():
            if name.startswith(prefixes):
                labels.append({'asset': uuid, 'dim': 'general', 'value': tag, 'confidence': 0.7})
        for tag, slug, _ in MACHINE:
            if name.startswith(slug + '-'):
                labels.append({'asset': uuid, 'dim': 'character', 'value': tag, 'confidence': 0.82})
                if int(name.rsplit('-', 1)[1]) <= ALSO[1]:
                    labels.append({'asset': uuid, 'dim': 'character', 'value': ALSO[0], 'confidence': 0.5})
    (dest / 'labels.jsonl').write_text('\n'.join(json.dumps(x) for x in labels), newline='\n')
    names = [n for _, n, _ in assets]
    dirs = [direction[n] for n in names]
    (dest / 'wd.npy').write_bytes(vectors(names, dirs, WD_DIM, 1))
    (dest / 'ccip.npy').write_bytes(vectors(names, dirs, CCIP_DIM, 2))
    tags = (resources / 'tags' / 'tags_v3.csv').read_bytes()
    models = {'wd': {'version': 'swinv2_448', 'dimension': WD_DIM, 'sha256': sha(b'synthetic WD'), 'tags_sha256': sha(tags)},
              'ccip': {'version': 'ccip-model_feat', 'dimension': CCIP_DIM, 'sha256': sha(b'synthetic CCIP')}}
    files = {f: {'bytes': (dest / f).stat().st_size, 'sha256': sha((dest / f).read_bytes())} for f in ('labels.jsonl', 'wd.npy', 'ccip.npy')}
    manifest = {'format': 'illustash-recognition/2', 'library_id': library_id, 'assets': [u for u, _, _ in assets],
                'models': models, 'inputs': {u: sha(p.read_bytes()) for u, _, p in assets},
                'preprocessing': 'illustash-wd-ccip/1', 'files': files}
    (dest / 'manifest.json').write_text(json.dumps(manifest), newline='\n')
    return dest


def wd_extension_spec(directory: Path, tags: bytes, heads: list[tuple[str, list[float], float]], origin: str) -> Path:
    """把合成线性头和对应的下载清单写到隔离目录。"""
    path = 'wd-extension-v1.bin'
    format_name = 'illustash-wd-extension/1'
    base = sha(b'synthetic WD')
    header = {'format': format_name, 'wd_model_sha256': base, 'wd_tags_sha256': sha(tags),
              'threshold': 0.8008725047111511, 'labels': [label for label, _, _ in heads]}
    weights = b''.join(struct.pack(f'<{WD_DIM}f', *weights) for _, weights, _ in heads)
    bias = struct.pack(f'<{len(heads)}f', *(bias for _, _, bias in heads))
    contents = json.dumps(header).encode() + b'\n' + weights + bias
    directory.mkdir(parents=True, exist_ok=True)
    (directory / path).write_bytes(contents)
    spec = {'revision': 1, 'format': format_name, 'heads': len(heads), 'license': 'Apache-2.0',
            'base_sha256': base, 'tags_sha256': sha(tags),
            'file': {'path': path, 'size': len(contents), 'sha256': sha(contents), 'url': f'{origin}/{path}'}}
    manifest = directory / 'wd-extension.json'
    manifest.write_text(json.dumps(spec), encoding='utf-8')
    return manifest


def wd_extension_walk(fixture: Fixture, directory: Path, origin: str) -> Path:
    """三个合成头分别命中无归属、已有 WD 归属、已有人工归属的图片。"""
    names = ['group-1-01', 'dahlia-01', 'aster-01']
    start = struct.unpack_from('<H', (data := (fixture.root / 'recognition' / 'wd.npy').read_bytes()), 8)[0] + 10
    order = {name: i for i, name in enumerate(fixture.names.values())}
    heads = []
    for label, photo in zip(('extone_(synthetic)', 'exttwo_(synthetic)', 'extthree_(synthetic)'), names):
        vector = struct.unpack_from(f'<{WD_DIM}f', data, start + order[photo] * WD_DIM * 4)
        heads.append((label, [10 * x for x in vector], -8))
    tags = (fixture.root / 'resources' / 'tags' / 'tags_v3.csv').read_bytes()
    return wd_extension_spec(directory, tags, heads, origin)


def pack_catalog_walk(fixture: Fixture, directory: Path, ccip: Path) -> tuple[dict[str, bytes], str]:
    """走查用的签名包目录（#602）：角色资料包的新版本 `character-pack-v2`。WD 部分在内置合成包的三个头之外多一个一级头
    （group-3）和一个只给候选的二级头（group-4）；CCIP 部分原样用内置的合成长尾包（`ccip`）。用测试密钥 A 签名
    （tests/fixtures/pack_catalog/make.py，要装 cryptography）。返回替身服务的「路径 → 字节」和测试公钥。"""
    import sys
    sys.path.insert(0, str(dev_preview.ROOT / 'tests' / 'fixtures' / 'pack_catalog'))
    import make  # noqa: E402（测试夹具的签名工具）
    from tools import pack_catalog
    start = struct.unpack_from('<H', (data := (fixture.root / 'recognition' / 'wd.npy').read_bytes()), 8)[0] + 10
    order = {name: i for i, name in enumerate(fixture.names.values())}
    vector = lambda photo: [10 * x for x in struct.unpack_from(f'<{WD_DIM}f', data, start + order[photo] * WD_DIM * 4)]
    heads = [('extone_(synthetic)', 'group-1-01', 1), ('exttwo_(synthetic)', 'dahlia-01', 1), ('extthree_(synthetic)', 'aster-01', 1),
             ('extfour_(synthetic)', 'group-3-01', 1), ('extfive_(synthetic)', 'group-4-01', 2)]
    tags = (fixture.root / 'resources' / 'tags' / 'tags_v3.csv').read_bytes()
    header = {'format': 'illustash-wd-extension/2', 'wd_model_sha256': sha(b'synthetic WD'), 'wd_tags_sha256': sha(tags),
              'labels': [h[0] for h in heads], 'thresholds': [0.8008725047111511] * len(heads), 'tiers': [h[2] for h in heads]}
    weights = b''.join(struct.pack(f'<{WD_DIM}f', *vector(photo)) for _, photo, _ in heads)
    contents = json.dumps(header).encode() + b'\n' + weights + struct.pack(f'<{len(heads)}f', *([-8.] * len(heads)))
    directory.mkdir(parents=True, exist_ok=True)
    package = directory / 'wd-extension-v2.bin'
    package.write_bytes(contents)
    entry = pack_catalog.entry(revision=2, wd=package, ccip=ccip, ccip_tag='ccip-longtail-v1', min_app='0.2.0',
                               note_zh='合成走查包：多两个角色，其中一个只给候选',
                               note_en='Synthetic walk pack: two more characters, one of them suggestion-only')
    catalog = pack_catalog.dumps(pack_catalog.build([entry]))
    key = make.Key('A')
    prefix = '/shio-vinyl/illustash/releases/download/'
    files = {prefix + 'packs/packs.json': catalog, prefix + 'packs/packs.json.sig': key.sign(catalog),
             prefix + 'character-pack-v2/wd-extension-v2.bin': contents}
    return files, key.pubkey()


def npy_rows(data: bytes) -> tuple[int, bytes]:
    """.npy 的头长和数据段（只认本文件写出的小端 float32、C 顺序）。"""
    start = struct.unpack_from('<H', data, 8)[0] + 10
    return start, data[start:]


def ccip_longtail_spec(directory: Path, shared: Path, roles: list[tuple[str, bytes]], origin: str) -> Path:
    """合成 CCIP 长尾参考包（#559）和它的下载清单；包的格式照 native/terms/src/app_ccip_longtail.rs 的 Package::load。

    roles 是（角色 tag，768 维小端 float32 锚点）。阈值 p75 = 0.2（余弦距离）。CCIP 模型摘要用合成识别结果里的
    sha256(b'synthetic CCIP')。长尾包要叠在基础角色索引上，合成图库没有，所以在共享目录写一个只有一个锚点的基础索引；
    它没有元数据，阈值为零，不认领任何图。"""
    anchors = b''.join(vector for _, vector in roles)
    metadata = json.dumps([{'id': tag, 'tag': tag, 'skin': False, 'post_count': 20, 'gender_conf': 0.5,
                            'p50': 0.1, 'p75': 0.2, 'p90': 0.3} for tag, _ in roles]).encode()
    model = sha(b'synthetic CCIP')
    provenance = json.dumps({'format': 'illustash-557-ccip-offline/1', 'ccip_sha256': model,
                             'files': {'A-metadata.json': sha(metadata), 'A-anchors.f32': sha(anchors)}}).encode()
    directory.mkdir(parents=True, exist_ok=True)
    path = directory / 'A-longtail-p75.zip'
    with zipfile.ZipFile(path, 'w', zipfile.ZIP_STORED) as z:
        for member, body in (('A-provenance.json', provenance), ('A-metadata.json', metadata), ('A-anchors.f32', anchors)):
            z.writestr(zipfile.ZipInfo(member, date_time=(2026, 11, 17, 0, 0, 0)), body)
    contents = path.read_bytes()
    rnd = random.Random('ccip-longtail-base')
    base = [rnd.gauss(0, 1) for _ in range(CCIP_DIM)]
    norm = math.sqrt(sum(x * x for x in base))
    header = "{'descr': '<f4', 'fortran_order': False, 'shape': (1, %d), }" % CCIP_DIM
    header += ' ' * (63 - (len(header) + 10) % 64) + '\n'
    shared.mkdir(parents=True, exist_ok=True)
    (shared / 'char-anchors.npy').write_bytes(b'\x93NUMPY\x01\x00' + struct.pack('<H', len(header)) + header.encode()
                                              + struct.pack(f'<{CCIP_DIM}f', *(x / norm for x in base)))
    (shared / 'char-anchors.txt').write_text('base_synthetic\n', encoding='utf-8')
    spec = {'sha256': sha(contents), 'size': len(contents), 'ccip_sha256': model, 'roles': len(roles),
            'url': f'{origin}/{path.name}'}
    manifest = directory / 'ccip-longtail.json'
    manifest.write_text(json.dumps(spec), encoding='utf-8')
    return manifest


def ccip_longtail_walk(fixture: Fixture, directory: Path, shared: Path, origin: str) -> Path:
    """走查图库用的长尾包：三个长尾角色的锚点取自三张图的 CCIP 向量，分别命中没有角色的一组相近图（补上归属）、
    WD 认出的角色、人工确认的角色（后两种只多出候选）。组内的图都在阈值内，别的组远在阈值外。"""
    names = [('longone_(synthetic)', 'group-2-01'), ('longtwo_(synthetic)', 'dahlia-02'), ('longthree_(synthetic)', 'aster-02')]
    _, data = npy_rows((fixture.root / 'recognition' / 'ccip.npy').read_bytes())
    order = {name: i for i, name in enumerate(fixture.names.values())}
    row = CCIP_DIM * 4
    roles = [(tag, data[order[photo] * row:(order[photo] + 1) * row]) for tag, photo in names]
    return ccip_longtail_spec(directory, shared, roles, origin)


def fake_resources(dest: Path) -> Path:
    """识别结果包要求的模型文件和标签表：内容是假的，宿主只核对 SHA-256，不加载。"""
    (dest / 'models' / 'ccip').mkdir(parents=True)
    (dest / 'models' / 'wd_swinv2_v3_emb.onnx').write_bytes(b'synthetic WD')
    (dest / 'models' / 'ccip' / 'model_feat.onnx').write_bytes(b'synthetic CCIP')
    (dest / 'tags').mkdir()
    (dest / 'tags' / 'tags_v3.csv').write_text('\n'.join(CSV) + '\n', newline='\n')
    return dest


def artist_package(dest: Path, library_id: str, by_name: dict[str, str]) -> Path:
    dest.mkdir(parents=True)
    rows, assets = [], []
    for (key, name, prefixes), method in [(a, 'pixel_hash') for a in ARTISTS] + [(MAYBE_ARTIST, 'ocr')]:
        for file in sorted(by_name):
            if not file.startswith(prefixes) or file.endswith(('-copy', '-small', '-edit')):
                continue
            uuid = by_name[file]
            assets.append(uuid)
            rows.append({'id': f'{file}-{key}', 'asset': {'namespace': 'library', 'id': uuid},
                         'term': {'kind': 'artist', 'link': {'system': 'provenance', 'key': key}, 'name': name},
                         'source': 'provenance', 'method': method, 'version': f'{method}/1',
                         'score': {'value': 0 if method == 'pixel_hash' else 0.6,
                                   'metric': 'hamming_distance' if method == 'pixel_hash' else 'ocr_confidence'},
                         'observed_at': '2026-11-01T00:00:00Z', 'data': {}})
    (dest / 'evidence.jsonl').write_text('\n'.join(json.dumps(r, ensure_ascii=False) for r in rows), newline='\n')
    data = (dest / 'evidence.jsonl').read_bytes()
    manifest = {'format': 'illustash-evidence/1', 'library_id': library_id,
                'producer': {'name': 'provenance', 'version': 'walk-fixture'}, 'created_at': '2026-11-01T08:00:00+08:00',
                'assets': [{'asset': {'namespace': 'library', 'id': u}, 'status': 'completed', 'error': None}
                           for u in dict.fromkeys(assets)],
                'files': {'evidence.jsonl': {'bytes': len(data), 'sha256': sha(data)}}}
    (dest / 'manifest.json').write_text(json.dumps(manifest), newline='\n')
    return dest


# ---------- 宿主 ----------

class Client:
    """宿主的 HTTP 客户端：用一次性码换会话 cookie，写入带 CSRF token 和 Origin，和页面一样。"""

    def __init__(self, url: str):
        self.base = url.split('/?')[0]
        self.opener = urllib.request.build_opener(urllib.request.HTTPCookieProcessor(http.cookiejar.CookieJar()))
        self.opener.open(url, timeout=20).read()
        self.token = self.get('/api/bootstrap')['token']

    def get(self, path: str) -> dict:
        with self.opener.open(self.base + path, timeout=30) as r:
            return json.loads(r.read())

    def post(self, path: str, body: dict) -> dict:
        req = urllib.request.Request(self.base + path, data=json.dumps(body).encode(), method='POST',
                                     headers={'Content-Type': 'application/json', 'X-CSRF-Token': self.token,
                                              'Origin': self.base})
        try:
            with self.opener.open(req, timeout=60) as r:
                return json.loads(r.read())
        except urllib.error.HTTPError as e:
            raise RuntimeError(f'{path} 失败：{e.code} {e.read().decode(errors="replace")}') from None

    def wait(self, what: str, done, timeout: float = 120) -> None:
        end = time.monotonic() + timeout
        while time.monotonic() < end:
            if done():
                return
            time.sleep(0.3)
        raise RuntimeError(f'建夹具时等{what}超时')


def worker() -> Path | None:
    """研究 worker（分组、相似、学习）：环境变量 ILLUSTASH_RESEARCH_WORKER，否则和宿主同目录的 illustash-worker。"""
    if configured := os.environ.get('ILLUSTASH_RESEARCH_WORKER'):
        return Path(configured)
    path = dev_preview.binary().with_name('illustash-worker')
    return path if os.access(path, os.X_OK) else None


def build(root: Path, core: Path) -> Fixture:
    root.mkdir(parents=True)
    images = root / 'images'
    images.mkdir()
    plan = items()
    for i, it in enumerate(plan):
        if it.copy_of:
            shutil.copyfile(images / f'{it.copy_of}.png', images / f'{it.name}.png')
        else:
            scene(images / f'{it.name}.png', it.width, it.height, it.seed, it.edit)
        # 文件时间固定（2026-11-01 起每张隔一小时）：「最新」排序和按天的图条每次建出来都一样。
        stamp = 1793462400 + i * 3600
        os.utime(images / f'{it.name}.png', (stamp, stamp))
    fixture = Fixture(root)
    work = fixture.live
    work.mkdir()
    env = dev_preview.isolated_env(work)
    run = lambda *a: subprocess.run([str(core), *a], env=env, check=True, capture_output=True, text=True)
    run('library-create', '--dest', str(fixture.library), '--folder', str(images), '--name', '合成图库')
    run('library-scan', '--library', str(fixture.library))
    library_id = json.loads((fixture.library / 'library.json').read_text())['id']
    with sqlite3.connect(fixture.library / 'source.sqlite') as c:
        rows = [(u, Path(p).stem, Path(p)) for u, p in c.execute('SELECT uuid, src_path FROM assets ORDER BY src_path')]
    fixture.names = {u: n for u, n, _ in rows}
    by_name = {n: u for u, n, _ in rows}
    direction = {it.name: it.vector for it in plan}
    resources = fake_resources(root / 'resources')
    package = recognition_package(root / 'recognition', resources, library_id, rows, direction)
    run('recognition-import', '--library', str(fixture.library), '--root', str(resources), '--package', str(package))
    evidence = artist_package(root / 'artists', library_id, by_name)
    run('import', '--db', str(fixture.library / 'library.sqlite'), '--library-id', library_id, '--package', str(evidence))

    research = worker()
    with Host(core, dev_preview.ROOT, fixture.library, work,
              env={'ILLUSTASH_RESEARCH_WORKER': str(research)} if research else None) as host:
        api = Client(host.url)
        for name, slug, n in HUMAN:
            entity = api.post('/api/entities', {'name': name})
            members = [f'{slug}-{i + 1:02d}' for i in range(n)]
            api.post('/api/change', {'ids': [by_name[m] for m in members], 'action': 'confirmed', 'entity': entity['id']})
            fixture.chars.append({'name': name, 'id': entity['id'], 'images': members})
        api.post('/api/change', {'ids': [by_name[n] for n in SKIPPED], 'action': 'unknown'})
        api.post('/api/curate', {'ids': [by_name[n] for n in EXCLUDED], 'action': 'exclude'})
        if research:
            # 研究 worker 在导入后自己会分组、找相似；这里再要一次，等它们和学习都停下来。
            idle = lambda: (api.get('/api/clusters').get('learner') or {}).get('status') in (None, 'idle', 'off')
            api.post('/api/regroup', {})
            api.wait('分组', lambda: api.get('/api/clusters').get('clusters') and idle())
            api.post('/api/similar', {})
            api.wait('找相似', lambda: api.get('/api/similar?kind=&nsfw=').get('state', {}).get('status') == 'idle' and idle(),
                     timeout=180)
        clusters = api.get('/api/clusters').get('clusters') or []
        fixture.groups = [{'id': g['cid'], 'images': sorted(fixture.names.get(u, u) for u in g['members'])} for g in clusters]
        fixture.similar = [{'id': g['gid'], 'images': sorted(fixture.names.get(u, u) for u in g['covers'])}
                           for g in api.get('/api/similar?kind=&nsfw=').get('groups') or []]
        for tag, slug, n in MACHINE:
            found = api.get('/api/entities?q=' + urllib.request.quote(tag.split('_')[0]))
            hit = next((e for e in found if isinstance(e, dict) and slug in str(e.get('name', '')).lower()), None)
            fixture.machine.append({'name': hit['name'] if hit else tag, 'id': hit['id'] if hit else '',
                                    'images': [f'{slug}-{i + 1:02d}' for i in range(n)]})
        # 引导：标成跳过并收起清单；只问一次的提示卡记成问过（应用级的放在隔离的应用数据里，随 pristine 一起复制）。
        api.post('/api/onboarding', {'action': 'skip'})
        api.post('/api/onboarding', {'action': 'hide', 'hidden': True})
        for key in ('names', 'works'):
            api.post('/api/onboarding', {'action': 'seen', 'key': key})
        api.post('/api/updates', {'action': 'auto', 'allow': False})
    shutil.copytree(work, root / 'pristine', symlinks=True)
    fixture.summary = (f"{len(rows)} 张合成图：人工确认的角色 {len(HUMAN)} 个，WD 认出的 {len(MACHINE)} 个，"
                       f"未识别分组 {len(fixture.groups)} 组，相似 {len(fixture.similar)} 组，副本 {len(COPIES)} 对，"
                       f"R18 {len(R18)} 张，画师 {len(ARTISTS) + 1} 个；研究 worker {'有' if research else '没有（分组、相似页为空）'}")
    (root / 'fixture.json').write_text(json.dumps(fixture.saved(), ensure_ascii=False), encoding='utf-8')
    return fixture


def cache_key(core: Path) -> str:
    """夹具的输入：本文件、宿主程序和研究 worker。三者都没变时上一次建好的夹具可以直接用。"""
    h = hashlib.sha256(Path(__file__).read_bytes())
    for path in (core, worker()):
        if path:
            st = path.stat()
            h.update(f'{path.name}:{st.st_size}:{st.st_mtime_ns}'.encode())
    return h.hexdigest()[:16]


def cached(base: Path, core: Path) -> Fixture:
    """取缓存里的夹具，没有就建一份。缓存放在 base/<键>，键见 cache_key；旧的键目录删掉。"""
    key = cache_key(core)
    root = base / key
    if (root / 'fixture.json').is_file() and (root / 'pristine').is_dir():
        return Fixture.load(root)
    for old in base.glob('*') if base.is_dir() else []:
        shutil.rmtree(old, ignore_errors=True)
    print('建合成图库（第一次或输入变了，约一两分钟）…', flush=True)
    tmp = base / (key + '.building')
    shutil.rmtree(tmp, ignore_errors=True)
    shutil.rmtree(root, ignore_errors=True)
    # 图库里存的是绝对路径（来源目录、图片），所以直接在最终位置建，建完才写 fixture.json 表示可用。
    return build(root, core)


def _unused_similar_running(api: Client) -> bool:
    try:
        tasks = api.get('/api/tasks')
    except Exception:  # noqa: BLE001（读不到就当还在跑，交给超时）
        return True
    text = json.dumps(tasks)
    return '"similar"' in text and '"running"' in text
