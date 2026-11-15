"""#347 测试专用索引算法，不提供模型加载、联网、CLI 或候选输出读取。

来源：1dd59da45d2773fdcb229aef948600f970b79bd9 的 tools/
{build_characters,char_patch,char_scan,index_worker}.py；此处只保留缓存合成输入路径。
merge/core/alias 的运算顺序保持历史 NumPy 定义；静态元数据、文件和业务输出独立推导。
"""
from contextlib import closing
import csv
import json
import re
import sqlite3
import time

import numpy as np
from numpy.linalg import norm
from tests.vector_math import cross

KEEP_DIST = .10
MIN_CORE, MIN_SHARE = 5, .4
SAME_DIST = .03
ALIAS_TH = .01
SOURCE = 'danbooru-patch'


def merge(embs):
    """按历史算法归一化、取均值，再还原平均模长；保留输入 dtype 和内存顺序。"""
    L = norm(embs, axis=-1)
    u = embs / L.reshape(-1, 1)
    c = u.mean(axis=0)
    return c / norm(c) * L.mean()


def split_danbooru_tag(tag):
    """拆分名字、皮肤和作品后缀。"""
    parts = re.findall('\\(([^()]+)\\)', tag)
    base = re.sub('_?\\([^()]+\\)', '', tag).strip('_')
    if not parts:
        return (base, None, None)
    copyright_ = parts[-1]
    subs = parts[:-1]
    return (base, '_'.join(subs) if subs else None, copyright_)


def fold_plain_skins(rows):
    """已有无后缀角色时，单后缀条目归为它的皮肤。"""
    rows = list(rows)
    plain = {tag: cr for (_, tag, base, _, cr) in rows if tag == base}
    return {cid: (cr, plain[base]) for (cid, tag, base, skin, cr) in rows if not skin and cr and (tag != base) and (base in plain)}


def core_of(A):
    """按邻居密度选择种子，迭代提取一致主体，保留人数和占比门槛。"""
    D = cross(A, A, check=False)
    seed = int(np.argmax((D <= KEEP_DIST).sum(1)))
    keep = np.nonzero(D[seed] <= KEEP_DIST + 0.02)[0]
    for _ in range(5):
        if len(keep) < 2:
            return None
        anchor = merge(A[keep])
        d = cross(anchor[None], A, check=False)[0]
        new = np.nonzero(d <= KEEP_DIST)[0]
        if set(new) == set(keep):
            break
        keep = new
    return keep if len(keep) >= MIN_CORE and len(keep) >= MIN_SHARE * len(A) else None


def anchor_alias(A, ids, meta):
    """直接邻居按人物质量、本体、热度和 ID 排序，再沿严格上升的映射归并。"""

    def score(c):
        m = meta.get(c) or (None,) * 9
        return ((m[6] or 0) >= 0.9, not m[4], m[5] or 0, c)
    alias = list(ids)
    for i in range(0, len(A), 1024):
        D = cross(A[i:i + 1024], A, check=False)
        for (r, j) in zip(*np.nonzero(D <= ALIAS_TH)):
            a = i + int(r)
            if score(ids[j]) > score(alias[a]):
                alias[a] = ids[j]
    step = dict(zip(ids, alias))
    for (a, c) in enumerate(alias):
        while step[c] != c:
            c = step[c]
        alias[a] = c
    return alias


def scalar_order(values, partition=False):
    """仅改变排序输入的类型，数值计算仍使用原 float32 和本机 BLAS。

    零虚部保留有限实数的比较关系；NumPy 2.5.3 的 complex64 走标量
    argintroselect/aquicksort，避开 x86 SIMD 排序（含并列值）的另一套顺序。
    依据：numpy/_core/src/npysort/{selection.cpp,quicksort.hpp} 的 dispatch 类型限制。
    调用处同时保留八组手写 tied_nearest，不能用本函数生成那些固定期望。
    """
    values = np.asarray(values)
    if values.ndim != 1 or values.dtype != np.dtype('float32') or not np.isfinite(values).all():
        raise ValueError('标量排序参考只接受有限的一维 float32 输入')
    sortable = values.astype(np.complex64)
    if partition:
        selected = np.argpartition(sortable, min(9, len(values)) - 1)[:9]
        return selected[np.argsort(sortable[selected])]
    return np.argsort(sortable)


def metadata(root):
    result = {}
    for path in sorted((root / 'char-index').glob('*.json')):
        try:
            data = json.loads(path.read_text(encoding='utf-8'))
        except ValueError:
            continue
        result[data.get('short_tag') or path.stem] = data
    return result


def clean(root, selected):
    """对缓存补充角色完整执行留原始向量、清洗、已有角色去重与补充角色去重。"""
    idx, patch = root / 'char-index', root / 'char-patch'
    raw, rejected = patch / 'raw', patch / 'rejected'
    raw.mkdir(exist_ok=True)
    rejected.mkdir(exist_ok=True)
    known_ids, known = [], []
    for cid, meta in metadata(root).items():
        path = idx / f'{cid}.npy'
        if cid in selected or not path.exists():
            continue
        vectors = np.load(path)
        if vectors.ndim == 2 and len(vectors) >= 3:
            known_ids.append(cid)
            known.append(merge(vectors))
    known = np.stack(known) if known else np.empty((0, 768), np.float32)
    accepted = []
    plan = json.loads((patch / 'plan.json').read_text())
    for row in sorted(plan, key=lambda r: -r['post_count']):
        cid = row['short_tag']
        meta_path, feature_path = idx / f'{cid}.json', idx / f'{cid}.npy'
        if cid not in selected or not meta_path.exists():
            continue
        meta = json.loads(meta_path.read_text())
        if meta.get('source') != SOURCE:
            continue
        original = raw / f'{cid}.npy'
        if not original.exists():
            feature_path.replace(original)
        vectors = np.load(original)
        keep = core_of(vectors)
        posts = meta.get('posts_all') or meta['posts']
        meta['posts_all'] = posts
        reason = None if keep is not None else '参考图没有一致的主体'
        if keep is not None:
            anchor = merge(vectors[keep])
            distances = cross(anchor[None], known)[0]
            if distances.size and distances.min() < SAME_DIST:
                reason = '与已有角色 %s 几乎相同（距离 %.3f）：多为其伴生物或皮肤' % (
                    known_ids[int(distances.argmin())], distances.min())
            elif accepted:
                distances = cross(anchor[None], np.stack([a for _, a in accepted]))[0]
                if distances.min() < SAME_DIST:
                    twin = accepted[int(distances.argmin())][0]
                    reason = '与补充角色 %s 是同一人（距离 %.3f）：变身前后、本体与皮肤等，并入其别名' % (twin, distances.min())
                    meta['same_as'] = twin
        if reason:
            meta['rejected'] = reason
            feature_path.unlink(missing_ok=True)
            (rejected / f'{cid}.json').write_text(json.dumps(meta, ensure_ascii=False, indent=1), encoding='utf-8')
            meta_path.unlink()
        else:
            accepted.append((cid, anchor))
            meta.update(posts=[posts[i] for i in keep], refs_total=len(vectors), refs_kept=len(keep))
            np.save(feature_path, vectors[keep].astype(np.float32))
            meta_path.write_text(json.dumps(meta, ensure_ascii=False, indent=1), encoding='utf-8')


def game_names(root):
    games = {}
    for path in sorted((root / 'game-chars').glob('*_index.json')):
        data = json.loads(path.read_text(encoding='utf-8'))
        names = {}
        for row in data['data']:
            record = {lang: row.get(key) or [] for lang, key in [('en', 'ennames'), ('ja', 'jpnames'), ('zh', 'cnnames')]}
            for name in record['en']:
                names.setdefault(name.strip().lower(), record)
                for part in name.split():
                    names.setdefault(part.strip().lower(), record)
        games[path.name.removesuffix('_index.json')] = names
    return games


GAME_DIR = {
    'blue_archive': 'bluearchive', 'arknights': 'arknights', 'azur_lane': 'azurlane',
    'fate_grand_order': 'fgo', 'fate/grand_order': 'fgo', 'genshin_impact': 'genshin',
    'girls_frontline': 'girlsfrontline', 'goddess_of_victory:_nikke': 'nikke',
    'nikke': 'nikke', 'path_to_nowhere': 'pathtonowhere', 'honkai:_star_rail': 'starrail',
    'neural_cloud': 'neuralcloud',
}


def build(root, tags_path):
    """从元数据与原向量构造整行角色、别名、分位阈值和持久化锚点。"""
    meta = metadata(root)
    with tags_path.open(encoding='utf-8') as handle:
        wd = {r['name'] for r in csv.DictReader(handle) if r['category'] == '4'}
    games = game_names(root)
    albums = json.loads((root / 'album-char-map.json').read_text(encoding='utf-8'))['map']
    tags = {cid: data.get('tag') or cid for cid, data in meta.items()}
    parts = {cid: split_danbooru_tag(tag) for cid, tag in tags.items()}
    folded = fold_plain_skins((cid, tags[cid], base, skin, cr or meta[cid].get('copyright'))
                              for cid, (base, skin, cr) in parts.items())
    rows, aliases, anchors, ids = [], [], [], []
    for cid, data in sorted(meta.items()):
        tag = tags[cid]
        base, skin, copyright = parts[cid]
        skin, copyright = folded.get(cid, (skin, copyright))
        copyright = copyright or data.get('copyright')
        names = games.get(GAME_DIR.get(copyright or '', ''), {}).get(base.lower(), {})
        stats = dict(n_ref=None, anchor_p50=None, anchor_p75=None, anchor_p90=None, anchor_row=None)
        path = root / 'char-index' / f'{cid}.npy'
        if path.exists():
            vectors = np.load(path)
            if vectors.ndim == 2 and len(vectors) >= 3:
                anchor = merge(vectors)
                distances = (1 - (vectors / norm(vectors, axis=1, keepdims=True)) @ (anchor / norm(anchor))) / 2
                stats.update({f'anchor_p{q}': float(np.percentile(distances, q)) for q in (50, 75, 90)})
                stats.update(n_ref=len(vectors), anchor_row=len(anchors))
                anchors.append(anchor)
                ids.append(cid)
        gender, confidence = None, None
        genders = data.get('gender') or {}
        if genders and max(genders.values()) > 0:
            gender = max(genders, key=genders.get)
            confidence = float(genders[gender])
        rows.append(dict(char_id=cid, danbooru_tag=tag, wd_tag=tag if tag in wd else None,
                         copyright=copyright, base_tag=base, skin=skin, post_count=data.get('post_count'),
                         gender=gender, gender_conf=confidence, core_tags=json.dumps(data.get('core_tags') or [], ensure_ascii=False),
                         **stats, **{f'name_{lang}': (names.get(lang) or [None])[0] for lang in ('en', 'ja', 'zh')}))
        aliases.extend([(cid, cid, 'short'), (cid, tag, 'danbooru')])
        if tag in wd:
            aliases.append((cid, tag, 'wd'))
        if base:
            aliases.append((cid, base, 'base'))
        for lang in ('en', 'ja', 'zh'):
            aliases.extend((cid, name, lang) for name in names.get(lang, []))
    for album, info in albums.items():
        aliases.extend((cid, album, 'album') for cid in [info['char_id'], *info.get('also', [])])
    with closing(sqlite3.connect(root / 'pi.db')) as db, db:
        db.executescript('''
            DROP TABLE IF EXISTS characters;
            DROP TABLE IF EXISTS character_aliases;
            CREATE TABLE characters (
                char_id TEXT PRIMARY KEY, danbooru_tag TEXT, wd_tag TEXT, copyright TEXT,
                base_tag TEXT, skin TEXT, post_count INTEGER, gender TEXT, gender_conf REAL,
                core_tags TEXT, n_ref INTEGER, anchor_p50 REAL, anchor_p75 REAL, anchor_p90 REAL,
                anchor_row INTEGER, name_en TEXT, name_ja TEXT, name_zh TEXT);
            CREATE TABLE character_aliases (
                char_id TEXT NOT NULL, alias TEXT NOT NULL, kind TEXT NOT NULL,
                PRIMARY KEY(char_id,alias,kind));
        ''')
        columns = [r[1] for r in db.execute('PRAGMA table_info(characters)')]
        db.executemany('INSERT INTO characters VALUES (' + ','.join('?' * len(columns)) + ')',
                       [[row[key] for key in columns] for row in rows])
        db.executemany('INSERT OR IGNORE INTO character_aliases VALUES (?,?,?)', aliases)
        db.executescript('''
            CREATE INDEX idx_char_wd ON characters(wd_tag);
            CREATE INDEX idx_char_cr ON characters(copyright);
            CREATE INDEX idx_char_base ON characters(base_tag);
            CREATE INDEX idx_alias ON character_aliases(alias);
        ''')
    np.save(root / 'char-anchors.npy', np.stack(anchors).astype(np.float32))
    (root / 'char-anchors.txt').write_text('\n'.join(ids), encoding='utf-8')


def anchor_index(anchors, ids, database):
    with closing(sqlite3.connect(database)) as db:
        meta = {r[0]: r[1:] for r in db.execute('''SELECT char_id,anchor_p90,copyright,wd_tag,base_tag,
            skin,post_count,gender_conf,anchor_p50,anchor_p75 FROM characters''')}
    thresholds = [np.array([(meta.get(cid) or (None,) * 9)[column] or 0 for cid in ids], np.float32)
                  for column in (0, 7, 8)]
    return meta, *thresholds, anchor_alias(anchors, ids, meta)


def claim_thresholds(root, ids, th, th75):
    patched = {cid for cid, row in metadata(root).items() if row.get('source') == SOURCE}
    return np.array([th75[i] if cid in patched else th[i] for i, cid in enumerate(ids)], np.float32)


def scan(root, task_id):
    anchors = np.load(root / 'char-anchors.npy')
    ids = (root / 'char-anchors.txt').read_text().splitlines()
    features = np.load(root / 'full-features.npy')
    with (root / 'full-manifest.csv').open() as handle:
        assets = [r['uuid'] for r in csv.DictReader(handle)]
    have = {asset.strip().upper() for asset in assets}
    with closing(sqlite3.connect(root / 'pi.db')) as db:
        extra = [(asset, vector) for asset, vector in db.execute("SELECT uuid,vec FROM embeddings WHERE model='ccip'")
                 if asset.upper() not in have]
    if extra:
        features = np.concatenate([features, np.stack([np.frombuffer(v, np.float32) for _, v in extra])])
        assets.extend(asset for asset, _ in extra)
    _, th, th50, th75, entities = anchor_index(anchors, ids, root / 'pi.db')
    limit = claim_thresholds(root, ids, th, th75)
    with (root / 'char-scan.csv').open('w', newline='', encoding='utf-8') as handle:
        writer = csv.writer(handle)
        writer.writerow(['uuid', 'char_id', 'entity', 'dist', 'thresh', 'tier', 'rank', 'within'])
        for start in range(0, len(features), 4000):
            distances = cross(anchors, features[start:start + 4000])
            for j in range(distances.shape[1]):
                seen, rank = set(), 0
                for a in scalar_order(distances[:, j], partition=True):
                    entity = entities[a]
                    if entity in seen:
                        continue
                    seen.add(entity)
                    distance, threshold = float(distances[a, j]), float(limit[a])
                    within = int(threshold > 0 and distance <= threshold)
                    tier = ('p50' if distance <= th50[a] else 'p75' if distance <= th75[a] else 'p90') if within else '-'
                    writer.writerow([assets[start + j].strip(), ids[a], entity, '%.4f' % distance,
                                     '%.4f' % threshold, tier, rank, within])
                    rank += 1
    with (root / 'char-scan.csv').open(encoding='utf-8') as handle:
        rows = [(r['uuid'], r['entity'], 1 - float(r['dist'])) for r in csv.DictReader(handle) if r['within'] == '1']
    with closing(sqlite3.connect(root / 'pi.db')) as db, db:
        db.execute("DELETE FROM labels WHERE dim='char_ext'")
        db.executemany("""INSERT OR REPLACE INTO labels(uuid,dim,value,conf,source,model_ver,run_id)
            VALUES (?,'char_ext',?,?,'char_index',?,?)""",
                       [(*row, 'character_index@2025-09-25+danbooru@' + time.strftime('%Y-%m-%d'),
                         'char-scan-' + task_id) for row in rows])


def cached_apply(root, tags_path, request_path):
    """限定已批准的缓存 apply 路径；缺少缓存就失败，不能寻找模型或替换参考。"""
    request = json.loads(request_path.read_text())
    if request['action'] != 'apply':
        raise ValueError('测试参考只支持缓存 apply')
    selected = {row['short_tag'] for row in request['rows']}
    for cid in selected:
        if not (root / 'char-index' / f'{cid}.npy').is_file():
            raise ValueError(f'缺少测试参考的缓存向量：{cid}')
    clean(root, selected)
    build(root, tags_path)
    scan(root, request['identity']['task_id'])
    with closing(sqlite3.connect(root / 'pi.db')) as db:
        db.row_factory = sqlite3.Row
        return dict(format=request['format'], identity=request['identity'], before=request['before'],
                    characters=[dict(row) for row in db.execute('SELECT * FROM characters')],
                    aliases=[list(row) for row in db.execute('SELECT char_id,alias,kind FROM character_aliases')],
                    labels=[list(row) for row in db.execute("SELECT uuid,value,conf FROM labels WHERE dim='char_ext'")])


def boundary_edges(root, edges):
    """同一固定输入在本机 .01 硬阈值两侧的归属；不沿用另一后端的布尔结果。"""
    anchors = np.load(root / 'char-anchors.npy')
    distances = cross(anchors, anchors)
    result = [(i, parent, bool(distances[i, parent] <= ALIAS_TH)) for i, parent, _ in edges]
    if {within for _, _, within in result} != {False, True}:
        raise AssertionError('合成边界输入必须仍覆盖 .01 的两侧')
    return result
