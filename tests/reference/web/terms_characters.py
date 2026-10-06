"""角色切到词条核心之后，同步、刷新、重扫由核心在一个事务里提交（native/terms/src/character_import.rs）。

这里在 Store 的写锁内、用只读连接准备交给核心的输入，查询和顺序与 Store.refresh / reindex_characters /
_import_changes 原来的一样：索引的角色行、源标签、相册决定、重复组的全量分组、realistic 列表、时间戳。
"""
import time

from .store import CANDIDATE_LABELS, Store


def _index(src, new_ids=None):
    columns = {r[1] for r in src.execute('PRAGMA table_info(characters)')}
    tag = "coalesce(nullif(danbooru_tag,''),char_id)" if 'danbooru_tag' in columns else 'char_id'
    return {'characters': [dict(r) for r in src.execute('SELECT * FROM characters')],
            'aliases': [list(r) for r in src.execute('SELECT char_id,alias FROM character_aliases')],
            'tags': [list(r) for r in src.execute(f'SELECT char_id,{tag} FROM characters')],
            'new_ids': new_ids or {}}


def _chunks(src, sql, ids):
    return [list(r) for part in range(0, len(ids), 500) for r in src.execute(
        sql.format(','.join('?' for _ in ids[part:part + 500])), ids[part:part + 500])]


def _now():
    return time.strftime('%Y-%m-%d %H:%M:%S')


def refresh(store, src, c, new_ids=None):
    assets = [list(r) for r in src.execute(
        "SELECT uuid,filename,src_path,width,height,added_at,src_kind FROM assets WHERE in_scope=1 AND COALESCE(trashed,0)=0")]
    shape = [(r[0], r[5] or '', r[3] or 0, r[4] or 0) for r in assets]
    return {'op': 'refresh', 'index': _index(src, new_ids), 'assets': assets,
            'duplicates': [list(r) for r in Store._duplicate_rows(c, src, shape)],
            'labels': [list(r) for r in src.execute(CANDIDATE_LABELS)],
            'decisions': [list(r) for r in src.execute("SELECT uuid,value FROM decisions WHERE dim='character'")],
            'album_map': store._album_map(), 'indexed': _now()}


def reindex(store, src, c, uuids, new_ids=None):
    known = {r[0] for r in c.execute('SELECT uuid FROM assets')}
    uuids = [u for u in dict.fromkeys(uuids) if u in known]
    return {'op': 'reindex', 'index': _index(src, new_ids), 'uuids': uuids,
            'labels': _chunks(src, CANDIDATE_LABELS + ' AND uuid IN ({})', uuids),
            'realistic': store._realistic(src, 'main'), 'indexed': _now()}


def import_changes(store, src, c, added, removed, run_id=None, new_ids=None):
    added, removed = list(dict.fromkeys(added)), list(dict.fromkeys(removed))
    rows = _chunks(src, """SELECT uuid,filename,src_path,width,height,added_at,src_kind FROM assets
        WHERE in_scope=1 AND COALESCE(trashed,0)=0 AND uuid IN ({})""", added)
    added = [r[0] for r in rows]
    gone = set(added) | set(removed)
    shape = [tuple(r) for r in c.execute("SELECT uuid,coalesce(added_at,''),coalesce(width,0),coalesce(height,0) FROM assets")
             if r[0] not in gone]
    shape += [(r[0], r[5] or '', r[3] or 0, r[4] or 0) for r in rows]
    return {'op': 'import', 'rows': rows, 'removed': removed, 'run_id': run_id, 'new_ids': new_ids or {},
            'wd_tags': [list(r) for r in src.execute('SELECT char_id,wd_tag FROM characters')],
            'labels': _chunks(src, CANDIDATE_LABELS + ' AND uuid IN ({})', added),
            'decisions': _chunks(src, "SELECT uuid,value FROM decisions WHERE dim='character' AND uuid IN ({})", added),
            'album_map': store._album_map(), 'duplicates': [list(r) for r in Store._duplicate_rows(c, src, shape)],
            'realistic': store._realistic(src, 'main'), 'indexed': _now()}


def rederive(store, obs_realistic):
    return {'op': 'rederive', 'realistic': obs_realistic, 'rating_indexed': _now(), 'general_indexed': _now()}
