"""固定旧路径标准答案的核心验收。已有 tests/golden 始终只读，不提供重录入口。

输入恢复到临时旧库，再真实切换四项核心；全部被测写入与连续撤销只走核心。
"""
import contextlib
import io
import json
import sqlite3
import tempfile
import uuid
from collections import Counter
from pathlib import Path
from unittest import mock

from tests.test_web import fixture
from web import library, terms_core
from web.store import Store

GOLDEN = Path(__file__).resolve().parent / 'golden'
POLICIES = (None, 'hybrid')
MODES = ('core', 'names')

TABLES = ('entities', 'links', 'names', 'display_names', 'name_aliases', 'work_review', 'assertions', 'deferred', 'asked',
          'reviews', 'identity_relations', 'identity_decisions', 'identity_uncertain', 'identity_anchor_decisions',
          'memberships', 'tag_memberships', 'asset_tags', 'prefs')


class Ids:
    """uuid.uuid4 的替身：固定序列。"""
    def __init__(self, start):
        self.n = start

    def __call__(self):
        self.n += 1
        return uuid.UUID(int=self.n)


def build(root, policy):
    """固定旧形态输入；实际被测写入由 run 按模式切换后执行，不再调用旧写方法布置场景。"""
    from provenance import db
    from tests import terms_seed
    with mock.patch('uuid.uuid4', side_effect=Ids(0x1000)):
        lib = library.create(root / 'Golden.pilib')
    store, ids = terms_seed.load('golden_' + (policy or 'none'), root,
                                 db=Path(lib.path) / 'library.sqlite', provenance=db.path_of(lib))
    return store, lib, ids


def switch(store, lib, mode):
    from web.server import adopt_artists, adopt_characters, adopt_labels, adopt_names
    if mode not in MODES:
        raise ValueError('黄金验收只支持完整核心形态')
    with contextlib.redirect_stdout(io.StringIO()):
        assert adopt_artists(store, lib) and adopt_labels(store) and adopt_characters(store) and adopt_names(store)


def copyright_of(tag):
    return 'series_x' if tag.endswith('_(series_x)') else ''


def scenario(ids, policy):
    """(入口, 参数) 的序列：每个写入口的合法与非法调用。hybrid 与未设置策略各跑一遍（同一份序列）。"""
    a, b, sk, w, l1, l2 = ids['alice'], ids['bob'], ids['skin'], ids['work'], ids['local'], ids['local2']
    calls = []
    add = lambda method, **kw: calls.append((method, kw))
    # 标签与画师
    add('tag', ids=['a', 'b'], tag='夏天')
    add('tag', ids=['a'], tag='夏天', add=False)
    add('tag', ids=[], tag='夏天')
    add('tag', ids=['a'], tag='')
    add('judge_artist', ids=['a'], artist=ids['artist'], verdict='yes', name='甲画师')
    add('judge_artist', ids=['c'], artist=ids['artist2'], verdict='no', name='')
    add('judge_artist', ids=['a'], artist=ids['artist'], verdict=None)
    add('judge_artist', ids=['a'], artist=ids['artist'], verdict='maybe')
    # 人工结论
    for action in ('confirmed', 'rejected', 'unknown', 'resume', 'add_variant', 'remove_variant', 'bad'):
        for entity in (a, sk, 'nope', None):
            add('change', ids=['b'], action=action, entity=entity)
    add('change', ids=['a'], action='confirmed', entity=None, entities=[a, b])
    add('change', ids=['a'], action='confirmed', entity=None, entities=(a,))
    add('change', ids=None, action='confirmed', entity=b, filters={'entity': a})
    add('change', ids=None, action='confirmed', entity=b, filters={'status': 'bad'})
    add('change', ids=['a'], action='confirmed', entity=b, exclude=['b'])
    add('change', ids=['missing'], action='confirmed', entity=b)
    for answers in (dict(yes=['a']), dict(no=['b']), dict(later=['c']), dict(yes=['a'], no=['b']), dict(yes='a'), dict()):
        add('answer', entity=a, kind='seed', **answers)
    add('answer', entity=a, kind='ambiguous', no=['a'], switch=b)
    add('answer', entity=sk, kind='seed', yes=['a'])
    add('answer', entity=a, kind='bad', yes=['a'])
    add('answer_audit', yes=['a'])
    add('answer_audit', yes=['c'])
    add('answer_audit', yes=['a', 'a'])
    add('reassign', ids=['b'], old_entity=a, new_entity=b)
    add('reassign', ids=['a'], old_entity=b, new_entity=[a, b])
    add('reassign', ids=['c'], old_entity=a, new_entity=b)
    add('reassign', ids=['b'], old_entity=sk, new_entity=b)
    add('reassign', ids=['b'], old_entity=a, new_entity=[])
    add('sweep', entity=b)
    add('sweep', entity=sk)
    add('sweep', entity='nope')
    add('uncertain_identity', ids=['a'], options=[a, b], reason='看不清')
    add('uncertain_identity', ids=['a'], options=[a], reason='看不清')
    add('uncertain_identity', ids=['a'], options=[a, b], reason=' ')
    # 锚点修正的入口 2026-09-30 随 v2 删除，不在场景里。
    add('decide_identity', a=a, b=b, kind='different_from', status='accepted', reason='不是同一人')
    add('decide_identity', a=a, b=b, kind='x', status='accepted', reason='r')
    add('decide_identity', relation_id=0, status='accepted', reason='r')
    # 资料、结构
    add('set_profile', entity=a, name='爱丽丝改', aliases=['甲', '甲', ' 乙 '], copyright_of=copyright_of)
    add('set_profile', entity=a, name='', copyright_of=copyright_of)
    add('set_profile', entity=l1, name='', copyright_of=copyright_of)
    add('set_profile', entity=sk, name='x', copyright_of=copyright_of)
    add('set_profile', entity=l1, tag='hero_(series_x)', copyright_of=copyright_of)
    add('set_profile', entity=l2, tag='bob_tag', copyright_of=copyright_of)
    add('set_profile', entity=l2, work=w, copyright_of=copyright_of)
    add('set_profile', entity=l2, work=5, copyright_of=copyright_of)
    add('set_profile', entity=l2, name='x' * 121, copyright_of=copyright_of)
    add('link_local', items=[{'id': l2, 'tag': 'solo_(series_x)'}], copyright_of=copyright_of)
    add('link_local', items=[], copyright_of=copyright_of)
    add('create_entity', name='新角色甲')
    add('create_entity', name='新角色甲')
    add('create_entity', name=None, kind='oc', artist='某画师')
    add('create_entity', name='', kind='bad')
    add('create_linked', tag='kaito_(series_x)', typed='Kaito', copyright_of=copyright_of)
    add('create_linked', tag='kaito_(series_x)', typed='', copyright_of=copyright_of)
    add('create_linked', tag='a b', copyright_of=copyright_of)
    add('shared_oc')
    add('shared_oc')
    add('set_kind', entity=b, kind='oc', artist='某画师')
    add('set_kind', entity=b, kind='', artist='')
    add('set_kind', entity=sk, kind='oc', artist='')
    add('set_mascot', entity=b, mascot=True)
    add('set_mascot', entity=b, mascot=False)
    add('set_mascot', entity=b, mascot=1)
    add('set_work', entity=b, work=w)
    add('set_work', entity=b, work='nope')
    add('set_work', entity=sk, work=w)
    add('edit_work', action='create', name='作品乙', parent=w)
    add('edit_work', action='create', name='')
    add('edit_work', action='from_tag', tag='new_copyright')
    add('edit_work', action='from_tag', tag='new_copyright')
    add('edit_work', action='edit', ident=w, name='作品甲改')
    add('edit_work', action='move', ident=w, parent=w)
    add('apply_work_evidence', evidence={
        'tags': [{'name': t, 'category': 3, 'is_deprecated': False}
                 for t in ('series_x', 'franchise_x', 'watched_y', 'fate/grand_order', 'fate_(series)')],
        'implications': [{'antecedent_name': 'series_x', 'consequent_name': 'franchise_x', 'status': 'active'},
                         {'antecedent_name': 'fate/grand_order', 'consequent_name': 'fate_(series)', 'status': 'active'}],
        'watch_tags': ['watched_y', 'fate/grand_order']})
    add('merge', source=a, target=a)
    add('merge', source=l1, target=b)
    return calls


def outcome(result=None, exc=None):
    if exc is not None:
        return ['ValueError' if isinstance(exc, ValueError) else type(exc).__name__, str(exc)]
    return ['ok', clean(result)]


def clean(value):
    """返回值：去掉历史步骤的时间，JSON 往返。"""
    if isinstance(value, dict):
        return {k: clean(v) for k, v in sorted(value.items()) if k != 'ts'}
    if isinstance(value, (list, tuple)):
        return [clean(v) for v in value]
    return json.loads(json.dumps(value, ensure_ascii=False, default=str))


def state(store):
    """{表: [行…]}：列按名字排序，去掉时间列（ts、*_at、created_at），行排序。"""
    out = {}
    with sqlite3.connect(store.path) as c:
        for table in TABLES:
            try:
                cols = sorted(r[1] for r in c.execute(f'PRAGMA table_info({table})')
                              if r[1] != 'ts' and not r[1].endswith('_at'))
            except sqlite3.OperationalError:
                cols = []
            if not cols:
                out[table] = None
                continue
            rows = c.execute(f'SELECT {",".join(cols)} FROM {table}').fetchall()
            out[table] = [cols] + sorted((list(r) for r in rows), key=lambda r: json.dumps(r, ensure_ascii=False))
        out['actions'] = [['id', 'label', 'undone']] + [list(r) for r in c.execute('SELECT id,label,undone FROM actions ORDER BY id')]
    verdicts = store.artist_verdicts()
    out['artist_verdicts'] = [['artist', 'uuid', 'verdict']] + sorted([a, u, v or ''] for a, m in verdicts.items() for u, v in m.items())
    return json.loads(json.dumps(out, ensure_ascii=False))


def diff(before, after):
    """每张表：去掉的行、加上的行（多重集）。"""
    out = {}
    for table in after:
        a, b = before.get(table), after[table]
        if a == b:
            continue
        if a is None or b is None or a[0] != b[0]:
            out[table] = {'replaced': b}
            continue
        key = lambda r: json.dumps(r, ensure_ascii=False)
        old, new = Counter(map(key, a[1:])), Counter(map(key, b[1:]))
        out[table] = {'-': [json.loads(k) for k in sorted((old - new).elements())],
                      '+': [json.loads(k) for k in sorted((new - old).elements())]}
    return out


def shown(kwargs):
    return {k: ('<copyright_of>' if callable(v) else v) for k, v in kwargs.items()}


def run(policy, mode):
    """在一个新夹具上按 mode 切换，执行全部调用，再连续撤销回起点。返回可以写进快照的记录。"""
    with tempfile.TemporaryDirectory() as temp:
        store, lib, ids = build(Path(temp), policy)
        switch(store, lib, mode)
        first = state(store)
        with sqlite3.connect(store.path) as c:
            mark = c.execute('SELECT coalesce(max(id),0) FROM actions').fetchone()[0]
        record = {'policy': policy, 'ids': ids, 'initial': first, 'steps': [], 'undo': []}
        current = first
        fresh = Ids(0x2000)
        for method, kwargs in scenario(ids, policy):
            try:
                with mock.patch('uuid.uuid4', side_effect=fresh), contextlib.redirect_stdout(io.StringIO()):
                    got = outcome(getattr(store, method)(**kwargs))
            except Exception as exc:  # noqa: BLE001 - 拒绝的类型和文案都要录
                got = outcome(exc=exc)
            after = state(store)
            record['steps'].append({'call': [method, shown(kwargs)], 'outcome': got, 'diff': diff(current, after)})
            current = after
        while True:
            with sqlite3.connect(store.path) as c:
                row = c.execute('SELECT id,label FROM actions WHERE undone=0 AND id>? ORDER BY id DESC LIMIT 1', (mark,)).fetchone()
            if not row:
                break
            try:
                with contextlib.redirect_stdout(io.StringIO()):
                    got = outcome(store.undo(row[0]))
            except Exception as exc:  # noqa: BLE001
                got = outcome(exc=exc)
                record['undo'].append({'action': row[0], 'label': row[1], 'outcome': got, 'diff': {}})
                break
            after = state(store)
            record['undo'].append({'action': row[0], 'label': row[1], 'outcome': got, 'diff': diff(current, after)})
            current = after
        record['final'] = diff(first, current)          # 撤销到底之后与起点的差别（结构编辑不进历史，会留下）
        return record


def name_calls():
    """NameUpdater 写名字：(说明, 调用 updater 的函数, 固定的 Danbooru 别名或 None)。"""
    return [
        ('replace 人物 bangumi', lambda u: u.replace('character', 'prio_(series_q)', 'bangumi', [('zh', '新 b 名'), ('ja', '新 j 名')]), None),
        ('replace 成功的空结果', lambda u: u.replace('character', 'prio_(series_q)', 'model', []), None),
        ('replace 作品', lambda u: u.replace('work', 'series_q', 'index', [('ja', '新シリーズ'), ('zh', '  ')]), None),
        ('replace 皮肤', lambda u: u.replace('skin', 'x_style', 'bangumi', [('zh', '新样式')]), None),
        ('replace 没有主人的 tag', lambda u: u.replace('character', 'no_such_tag', 'bangumi', [('zh', '无主')]), None),
        ('replace 非法来源', lambda u: u.replace('character', 'prio_(series_q)', 'bogus', [('zh', 'x')]), None),
        ('aliases 人物', lambda u: u.aliases('character', 'prio_(series_q)', fetch=True), ['alias a', 'alias b']),
        ('aliases 作品', lambda u: u.aliases('work', 'series_q', fetch=True), ['Q 系列', 'Q2']),
        ('aliases 皮肤', lambda u: u.aliases('skin', 'swimsuit', fetch=True), ['swim']),
        ('seed 索引种子', lambda u: seed(u), None),
    ]


def seed(updater):
    from web import names
    with updater.store.connect() as c:
        rows = names.scope(c)
    return updater.seed(list(rows.values()) if isinstance(rows, dict) else rows)


NAME_TABLES = ('names', 'display_names', 'name_aliases')


def name_state(store):
    """与 state 同一种形状：每张表第一行是列名。"""
    out = {}
    with sqlite3.connect(store.path) as c:
        for t in NAME_TABLES:
            cols = sorted(r[1] for r in c.execute(f'PRAGMA table_info({t})'))
            rows = c.execute(f'SELECT {",".join(cols)} FROM {t}').fetchall()
            out[t] = [cols] + sorted((list(r) for r in rows), key=lambda r: json.dumps(r, ensure_ascii=False))
    return json.loads(json.dumps(out, ensure_ascii=False))


def run_names(mode):
    """NameUpdater 在完整核心形态上核对原有固定快照。"""
    from tests import terms_seed
    from web import names
    from web.server import adopt_characters, adopt_labels, adopt_names
    with tempfile.TemporaryDirectory() as temp:
        store, _ = terms_seed.load('golden_names', Path(temp))
        if mode != 'names':
            raise ValueError('名字黄金验收只支持核心形态')
        terms_seed.switch(store)
        updater = names.NameUpdater(store, enabled=False)
        current = name_state(store)
        record = {'initial': current, 'steps': []}
        for label, call, wiki in name_calls():
            try:
                patch = mock.patch.object(names, 'danbooru_names', return_value=wiki) if wiki is not None else contextlib.nullcontext()
                with patch, mock.patch.object(updater, 'wiki', return_value={'other_names': wiki or []}):
                    got = outcome(call(updater))
            except Exception as exc:  # noqa: BLE001
                got = outcome(exc=exc)
            after = name_state(store)
            record['steps'].append({'call': label, 'outcome': got, 'diff': diff(current, after)})
            current = after
        return record


def names_path():
    return GOLDEN / 'terms_name_writes.json'


def path(policy):
    return GOLDEN / f'terms_writes_{policy or "none"}.json'


def load(policy):
    return json.loads(path(policy).read_text(encoding='utf-8'))



if __name__ == '__main__':
    print(__doc__)
