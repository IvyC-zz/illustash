"""C3 的旧形态输入夹具：删掉旧写方法之后，仍能从固定输入验收真实启动切换。

这些 SQL 在删除旧写路径之前保存，只用于测试临时目录。结果快照仍在 tests/golden，不能由夹具覆盖或重录。
加载后用公开启动切换函数收编，随后被测写入必须走核心。
"""
import contextlib
import io
import json
import sqlite3
import tempfile
from pathlib import Path

from web import terms_core
from web.store import Store

ROOT = Path(__file__).resolve().parent / 'fixtures' / 'terms_c3'


def key(test):
    return test.id().removeprefix('tests.').replace('.', '_')


def dump(path, root):
    with sqlite3.connect(path) as c:
        sql = '\n'.join(c.iterdump())
    return sql.replace(str(root.resolve()), '<临时目录>').replace(str(root), '<临时目录>')


def load(name, root, *, db=None, source=None, provenance=None):
    """恢复固定旧库输入。legacy=True 专门用于这一真实启动切换夹具，不作为测试默认形态。"""
    temporary = (Path(tempfile.gettempdir()).resolve(), Path('/private/tmp').resolve())
    if not any(root.resolve().is_relative_to(t) for t in temporary):
        raise ValueError('固定输入夹具仅允许恢复到临时目录')
    record = json.loads((ROOT / f'{name}.json').read_text(encoding='utf-8'))
    # 源夹具的相册映射是源数据库之外的输入；恢复固定 SQL 之前先重建同一个公共源夹具侧文件。
    from tests.test_web import fixture
    original_source = root / 'source.db'
    if original_source.exists():
        original_source.unlink()
    fixture(root)
    db, source = Path(db or root / 'app.sqlite'), Path(source or root / 'source.db')
    for path, field in ((source, 'source'), (db, 'library'), (provenance, 'provenance')):
        if path is None:
            continue
        path = Path(path)
        if not path.resolve().is_relative_to(root.resolve()):
            raise ValueError('固定输入夹具文件不得越出临时根目录')
        path.parent.mkdir(parents=True, exist_ok=True)
        if path.exists():
            path.unlink()  # 仅调用者新建的临时夹具文件，不能用于用户图库
        with sqlite3.connect(path) as c:
            c.executescript(record[field].replace('<临时目录>', str(root)))
            c.execute('PRAGMA journal_mode=WAL')
    store = Store(source, db, initialize=False, legacy=True)
    store.album_titles = store._album_titles()
    return store, record['attrs']


def switch(store, *, artists=True, names=True):
    """与服务端一致：标签、角色、名字；没有溯源输入的合成库收编空画师状态。"""
    from web.server import adopt_characters, adopt_labels, adopt_names
    output = io.StringIO()
    with contextlib.redirect_stdout(output):
        if artists:
            terms_core.run('migrate', '--db', store.path)
            terms_core.cutover_artists(store.path)
        if not adopt_labels(store) or not adopt_characters(store) or names and not adopt_names(store):
            raise AssertionError('合成旧库的启动切换失败：' + output.getvalue()[-2000:])
    return store


def resume_ids(test, name):
    """启动备份可以消耗 UUID；被测调用恢复到旧路径实际录下的 ID 序列位置。"""
    from tests import golden_writes
    from unittest import mock
    cursor = json.loads((ROOT / f'{name}_ids.json').read_text(encoding='utf-8'))['next']
    patch = mock.patch('uuid.uuid4', side_effect=golden_writes.Ids(cursor))
    patch.start()
    test.addCleanup(patch.stop)


def base(root, *, db=None):
    """基础旧库启动输入：从删除前保存的旧答案夹具中取原始 a/b/c 场景，去掉额外边界输入。

    仅布置旧表后切换的测试使用；不复制或调用已经删除的旧 Python 写方法。
    """
    store, attrs = load('refusals_none', root, db=db)
    alice, bob = attrs['alice'], attrs['bob']
    with store.connect() as c:
        for table in ('actions', 'asked', 'audit_sample', 'reviews', 'deferred', 'prefs', 'curation',
                      'prototypes', 'proto_claims', 'weak_claims', 'learn_state', 'work_review', 'name_aliases'):
            c.execute(f'DELETE FROM {table}')
        c.execute("DELETE FROM sqlite_sequence WHERE name='actions'")
        for table in ('identity_relations', 'identity_evidence', 'identity_decisions', 'identity_decision_evidence',
                      'identity_baseline', 'identity_canonical', 'identity_scopes', 'identity_suspended',
                      'identity_uncertain', 'identity_anchor_decisions', 'identity_anchor_history'):
            if c.execute("SELECT 1 FROM sqlite_master WHERE type='table' AND name=?", (table,)).fetchone():
                c.execute(f'DELETE FROM {table}')
        c.execute("DELETE FROM meta WHERE key LIKE 'identity_%'")
        for table, column in (('names', 'entity_id'), ('links', 'entity_id'), ('entities', 'id')):
            c.execute(f'DELETE FROM {table} WHERE {column} NOT IN (?,?)', (alice, bob))
        c.execute("DELETE FROM display_names WHERE source NOT IN ('builtin','index')")
        c.execute("DELETE FROM assets WHERE uuid NOT IN ('a','b','c')")
        c.execute("DELETE FROM candidates WHERE uuid NOT IN ('a','b','c') OR entity NOT IN (?,?)", (alice, bob))
        c.execute('DELETE FROM assertions')
        c.execute("INSERT INTO assertions VALUES ('a',?,'confirmed','album')", (alice,))
        c.execute('DELETE FROM imported')
        c.execute("INSERT INTO imported VALUES ('a','alice_album')")
        c.execute('DELETE FROM general_hits')
        c.execute("INSERT INTO general_hits VALUES ('a','blue_hair')")
        c.execute('DELETE FROM asset_rating')
        c.execute('DELETE FROM asset_tags')
        with store.original() as src:
            store._refresh_duplicates(c, src)
            Store._refresh_evidence(c)
            Store._refresh_single(c)
            Store._refresh_memberships(c)
            Store._refresh_rules(c, Store._realistic(src, 'main'))
            Store._refresh_excluded(c)
    return store


def startup_input(store, *, db=None):
    """将已由核心布置的测试状态作为另一份旧库的启动输入。

    专用于公开入口无法表达的旧表、旧版本或旧历史边界：只复制兼容表及外围表，不复制核心表、
    写授权标记或保护触发器。修改输入后必须再次 switch；原核心库不解除保护，标准答案也不重写。
    """
    temporary = (Path(tempfile.gettempdir()).resolve(), Path('/private/tmp').resolve())
    for path in (store.path, Path(store.source)):
        if not any(path.resolve().is_relative_to(t) for t in temporary):
            raise ValueError('启动输入只能取自临时测试库')
    root = Path(tempfile.mkdtemp(prefix='启动输入-', dir=store.path.parent))
    result, _ = load('refusals_none', root)
    # 保留原测试源路径；被测同步仍读同一份输入，源只读断言继续约束它。
    result.source = store.source
    with store.connect() as src, result.connect() as dst:
        tables = [r[0] for r in dst.execute("SELECT name FROM sqlite_master WHERE type='table' AND name<>'sqlite_sequence'")]
        for table in tables:
            if not src.execute("SELECT 1 FROM sqlite_master WHERE name=? AND type IN ('table','view')", (table,)).fetchone():
                dst.execute(f'DELETE FROM "{table}"')
                continue
            columns = [r[1] for r in dst.execute(f'PRAGMA table_info("{table}")')]
            available = {r[1] for r in src.execute(f'PRAGMA table_info("{table}")')}
            if not set(columns).issubset(available):
                raise AssertionError(f'启动输入的兼容列不一致：{table}')
            cols = ','.join(f'"{c}"' for c in columns)
            rows = [tuple(r) for r in src.execute(f'SELECT {cols} FROM "{table}"')]
            if table == 'meta':
                rows = [r for r in rows if not r[0].startswith('terms_')]
            if table == 'actions':
                action_columns = columns.index('before_json')
                rows = [r[:action_columns] + (json.dumps(terms_core.legacy_before(json.loads(r[action_columns])), ensure_ascii=False),)
                        + r[action_columns + 1:] for r in rows]
            dst.execute(f'DELETE FROM "{table}"')
            if rows:
                dst.executemany(f'INSERT INTO "{table}" ({cols}) VALUES ({",".join("?" for _ in columns)})', rows)
        # 完整旧库的身份只读视图由原启动迁移安装；hybrid 的学习签名也需要它们。
        from web import identity
        identity.install(dst)
        if identity.enabled(dst):
            identity.install_views(dst)
    if db is not None:
        db = Path(db)
        if not any(db.resolve().is_relative_to(t) for t in temporary) or db.exists():
            raise ValueError('启动输入的目标必须是尚未建立的临时库')
        db.parent.mkdir(parents=True, exist_ok=True)
        with result.connect() as src, sqlite3.connect(db) as dst:
            src.backup(dst)
        result = Store(store.source, db, initialize=False, legacy=True)
    result.album_titles = result._album_titles()
    return result


@contextlib.contextmanager
def staged(store):
    """布置一份旧库启动边界；退出准备段立即切到完整核心，原库保持写保护。"""
    old = startup_input(store)
    yield old
    switch(old)
