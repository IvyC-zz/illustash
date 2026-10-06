"""角色、皮肤、作品的固定等价契约：旧形态仅用来布置真实启动输入，被测操作全部通过核心。

原行为断言、回滚断言和独立旧 SQL 投影核对保留；分步同步仍单独验收旧库的收编与拒绝。
"""
import json
import sqlite3
import tempfile
import unittest
from pathlib import Path

from tests import terms_seed
from tests.test_web_v2 import assert_memberships_current
from web import names, terms_core, works
from web.store import Store

# 兼容表：影子收编（S2）生成 shadow_<名字>，与旧表逐行逐列比对。
COMPAT = ('entities', 'links', 'work_review', 'assertions', 'candidates',
          'identity_relations', 'identity_evidence', 'identity_decisions', 'identity_decision_evidence')
PROJECTIONS = ('family_evidence', 'single_assets', 'lead_assets', 'memberships', 'tag_memberships', 'duplicates')


def term_rows(path):
    """核心各表的行数：再同步一次不能新增。"""
    with sqlite3.connect(path) as c:
        tables = [r[0] for r in c.execute("SELECT name FROM sqlite_master WHERE type='table' AND (name LIKE 'term%' OR name LIKE 'shadow_%')")]
        return {t: c.execute(f'SELECT count(*) FROM {t}').fetchone()[0] for t in tables}


def refreshed_projections(path, swap):
    """库的一份拷贝：swap 时先把影子兼容表换上真名（旧表丢掉），再用 Python 原来的 SQL 全量重算投影。"""
    from web import effects  # noqa: F401  (Store 的静态方法只用连接)
    copy = Path(str(path) + ('.swap' if swap else '.plain'))
    with sqlite3.connect(path) as src, sqlite3.connect(copy) as dst:
        src.backup(dst)
    c = sqlite3.connect(copy)
    c.row_factory = sqlite3.Row
    names.register(c)
    if swap:
        c.execute('PRAGMA legacy_alter_table=ON')
        shadows = {r[0] for r in c.execute("SELECT name FROM sqlite_master WHERE type='table' AND name LIKE 'shadow_%'")}
        for t in COMPAT:
            if 'shadow_' + t in shadows:
                for (trigger,) in c.execute("SELECT name FROM sqlite_master WHERE type='trigger' AND tbl_name=?", ('shadow_' + t,)).fetchall():
                    c.execute(f'DROP TRIGGER {trigger}')
                c.execute(f'DROP TABLE {t}')
                c.execute(f'ALTER TABLE shadow_{t} RENAME TO {t}')
        c.execute('PRAGMA legacy_alter_table=OFF')
    # 只在抛弃式副本中授权原 SQL 参考重算；被比较的核心库绝不修复或解除写保护。
    c.execute("INSERT OR REPLACE INTO meta VALUES ('terms_writer','1')")
    Store._refresh_evidence(c)
    Store._refresh_single(c)
    Store._refresh_memberships(c)
    c.execute("DELETE FROM meta WHERE key='terms_writer'")
    out = {t: sorted(map(tuple, c.execute(f'SELECT * FROM {t}'))) for t in PROJECTIONS}
    for view in ('family_confirmed', 'effective_assertions', 'identity_nodes', 'identity_assignments', 'ambiguous_claims'):
        if c.execute("SELECT 1 FROM sqlite_master WHERE type='view' AND name=?", (view,)).fetchone():
            out[view] = sorted(map(tuple, c.execute(f'SELECT * FROM {view}')))
    c.close()
    copy.unlink()
    return out


def assert_characters_sync(test, store):
    """S2 验收：影子收编提交成功、各兼容表与旧表零差异；再跑一次不新增行；兼容表足以用原 SQL 算出同样的投影。"""
    try:
        terms_core.run('migrate', '--db', store.path)
        command = 'verify-characters' if store.characters_in_core() else 'sync-characters'
        report = terms_core.run(command, '--db', store.path)
    except terms_core.Failed as exc:
        test.fail(str(exc))
    test.assertTrue(report['committed'])
    for table, diff in report['tables'].items():
        test.assertEqual((diff['missing'], diff['extra']), (0, 0), table)
    rows = term_rows(store.path)
    again = terms_core.run(command, '--db', store.path)
    test.assertTrue(again['committed'])
    test.assertEqual(term_rows(store.path), rows)
    test.assertEqual(refreshed_projections(store.path, True), refreshed_projections(store.path, False))
    if store.characters_in_core():
        with store.connect() as c:
            stored = {t: sorted(map(tuple, c.execute(f'SELECT * FROM {t}'))) for t in PROJECTIONS}
        rebuilt = refreshed_projections(store.path, False)
        test.assertEqual(stored, {t: rebuilt[t] for t in PROJECTIONS})
    return report


class Contract(unittest.TestCase):
    policy = None   # None：新图库默认（未设置身份策略）；'hybrid'：生产据称使用的策略
    sync_check = True   # 每个场景结束后做一次影子收编核对（S2）；切换后改为从 term_* 全量重建核对

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        self.store = terms_seed.base(self.root)
        self.source = self.store.source
        with self.store.connect() as c:
            ids = dict(c.execute("SELECT key,entity_id FROM links WHERE system='ccip'"))
        self.alice, self.bob = ids['alice'], ids['bob']

    def tearDown(self):
        assert_memberships_current(self, self.store)
        if self.sync_check and self.store.characters_in_core():
            try:
                terms_core.run('verify-characters', '--db', self.store.path)
            except terms_core.Failed as exc:
                self.fail(str(exc))
        elif self.sync_check:
            assert_characters_sync(self, self.store)

    def activate(self, mode=None):
        mode = mode or self.policy
        if mode:
            if self.store.characters_in_core():
                self.store = terms_seed.startup_input(self.store)
            return self.store.activate_identity(mode=mode)

    def switch(self):
        """旧表准备段结束后，四模块真实切换；后续再次调用只核对完成状态。"""
        terms_seed.switch(self.store)
        self.assertTrue(self.store.characters_in_core())

    def rows(self, sql, *args):
        with self.store.connect() as c:
            return [tuple(r) for r in c.execute(sql, args)]

    def one(self, sql, *args):
        found = self.rows(sql, *args)
        return found[0] if found else None

    def assertion(self, uuid, entity):
        return self.one('SELECT status,origin FROM assertions WHERE uuid=? AND entity=?', uuid, entity)

    def state(self):
        """人工结论与复核流程的全部状态，用来核对「被拒绝的操作什么都没改」。"""
        return {t: sorted(self.rows('SELECT * FROM ' + t)) for t in ('assertions', 'deferred', 'reviews', 'asked', 'memberships')} | \
            {'actions': self.rows('SELECT id,undone FROM actions ORDER BY id')}

    def action(self, before, label='夹具步骤'):
        with self.store.connect() as c:
            return c.execute('INSERT INTO actions(label,before_json) VALUES (?,?)', (label, json.dumps(before))).lastrowid

    def add_source(self, sql, *args):
        with sqlite3.connect(self.source) as src:
            src.execute(sql, args)


class Shared:
    """两种身份策略下都一样的行为。"""

    def setUp(self):
        super().setUp()
        self.activate()

    # ---- 相册结论没有 action，撤销要能回到它 ----

    def test_album_confirmation_returns_when_a_later_rejection_is_undone(self):
        self.switch()
        self.assertEqual(self.assertion('a', self.alice), ('confirmed', 'album'))
        rejected = self.store.change(['a'], 'rejected', self.alice)
        self.assertEqual(self.assertion('a', self.alice), ('rejected', 'review'))
        self.assertEqual(self.store.undo(rejected['id']), None)
        self.assertEqual(self.assertion('a', self.alice), ('confirmed', 'album'))
        self.assertIn(('a', self.alice, 'confirmed'), set(self.rows('SELECT uuid,entity,status FROM memberships')))

    def test_album_import_after_an_undone_step_only_fills_an_empty_verdict(self):
        self.switch()
        first = self.store.change(['c'], 'rejected', self.alice)
        self.store.undo(first['id'])
        self.assertIsNone(self.assertion('c', self.alice))
        # 之后同步时 c 才出现在相册里：没有有效结论，相册导入写入一次。
        self.add_source("INSERT INTO decisions VALUES ('c','character','alice_album','album:Alice album pk=1')")
        self.store.refresh()
        self.assertEqual(self.assertion('c', self.alice), ('confirmed', 'album'))
        second = self.store.change(['c'], 'rejected', self.alice)
        self.store.undo(second['id'])
        self.assertEqual(self.assertion('c', self.alice), ('confirmed', 'album'))
        # 已导入过的相册成员不再写：人工否定之后再同步，否定保留。
        self.store.change(['c'], 'rejected', self.alice)
        self.store.refresh()
        self.assertEqual(self.assertion('c', self.alice), ('rejected', 'review'))
        self.assertEqual(self.rows("SELECT uuid FROM imported WHERE value='alice_album' ORDER BY uuid"), [('a',), ('c',)])

    def test_first_album_import_never_overwrites_a_human_verdict(self):
        self.switch()
        rejected = self.store.change(['c'], 'rejected', self.alice)
        self.add_source("INSERT INTO decisions VALUES ('c','character','alice_album','album:Alice album pk=1')")
        self.store.refresh()
        self.assertEqual(self.assertion('c', self.alice), ('rejected', 'review'))
        self.assertIn(('c', 'alice_album'), self.rows('SELECT * FROM imported'))
        # 撤掉那次否定：导入时没写相册结论，所以回到没有结论。
        self.store.undo(rejected['id'])
        self.assertIsNone(self.assertion('c', self.alice))

    # ---- 撤销的形状 ----

    def test_zero_member_sweep_is_a_step_with_reviews(self):
        self.switch()
        nobody = self.store.create_entity('Nobody')['id']
        self.switch()
        before = self.state()
        step = self.store.sweep(nobody)
        # 快照是空的；切换后新步骤写成信封 [{"terms": {"v": 2, ...}}]（方案第 6.3 节），里面的快照同样是空的。
        self.assertEqual(terms_core.legacy_before(json.loads(self.one('SELECT before_json FROM actions WHERE id=?', step['id'])[0])), [])
        self.assertEqual(self.rows('SELECT entity,action_id FROM reviews'), [(nobody, step['id'])])
        self.store.undo(step['id'])
        self.assertEqual(self.rows('SELECT * FROM reviews'), [])
        self.assertEqual(self.one('SELECT undone FROM actions WHERE id=?', step['id']), (1,))
        after = self.state()
        self.assertEqual({k: v for k, v in after.items() if k != 'actions'}, {k: v for k, v in before.items() if k != 'actions'})

    def test_unknown_and_mixed_steps_are_refused_and_change_nothing(self):
        self.switch()
        for before in ([{'unknown': 1}],
                       # 前一项是能撤的快照，后一项不认识：整步拒绝，前一项也不能生效。
                       [{'uuid': 'a', 'entity': self.alice, 'assertion': None, 'deferred': False}, {'unknown': 1}],
                       [{'uuid': 'a', 'entity': self.alice, 'assertion': None, 'deferred': False}, {'terms': 'verdicts'}]):
            with self.subTest(before=before):
                step = self.action(before)
                state = self.state()
                with self.assertRaises(Exception):
                    self.store.undo(step)
                self.assertEqual(self.state(), state)
                with self.store.connect() as c:
                    c.execute('DELETE FROM actions WHERE id=?', (step,))

    # ---- 机器证据：成功的空结果、索引放弃角色 ----

    def test_a_successful_empty_rescan_clears_the_image_s_candidates(self):
        self.switch()
        self.store.change(['b'], 'confirmed', self.bob)
        self.switch()
        self.assertTrue(self.rows("SELECT 1 FROM candidates WHERE uuid='b'"))
        with sqlite3.connect(self.source) as src:
            src.execute("DELETE FROM labels WHERE uuid='b' AND dim IN ('character','char_ext')")
        self.assertEqual(self.store.reindex_characters(['b']), 1)
        self.assertEqual(self.rows("SELECT * FROM candidates WHERE uuid='b'"), [])
        self.assertEqual(self.rows("SELECT entity,status FROM memberships WHERE uuid='b'"), [(self.bob, 'confirmed')])
        self.assertEqual(self.assertion('b', self.bob), ('confirmed', 'review'))

    def test_a_dropped_index_character_loses_its_claims_but_keeps_its_identity(self):
        self.switch()
        self.add_source("INSERT INTO characters VALUES ('carol','Carol',NULL,NULL,'Example',NULL)")
        self.add_source("INSERT INTO labels VALUES ('c','char_ext','carol',0.97)")
        self.store.reindex_characters(['c'])
        carol = self.one("SELECT entity_id FROM links WHERE system='ccip' AND key='carol'")[0]
        self.switch()
        self.assertIn((carol, 'CCIP', 0.97), self.rows("SELECT entity,source,score FROM candidates WHERE uuid='c'"))
        with sqlite3.connect(self.source) as src:
            src.execute("DELETE FROM characters WHERE char_id='carol'")
            src.execute("DELETE FROM labels WHERE value='carol'")
        self.store.reindex_characters(['c'])
        self.assertEqual(self.rows("SELECT * FROM candidates WHERE entity=?", carol), [])
        self.assertFalse(any(e == carol for _, e, _ in self.rows('SELECT uuid,entity,status FROM memberships')))
        self.assertEqual(self.one('SELECT kind FROM entities WHERE id=?', carol), ('character',))
        self.assertEqual(self.one("SELECT entity_id FROM links WHERE system='ccip' AND key='carol'"), (carol,))

    # ---- 作品树：状态，由处理顺序决定 ----

    def work(self, tag, locked=False):
        self.switch()
        ident = self.store.edit_work('create', name=tag.upper(), tag=tag)['id']
        if not locked:
            self.store = terms_seed.startup_input(self.store)
            with self.store.connect() as c:
                c.execute('UPDATE work_review SET parent_locked=0 WHERE entity_id=?', (ident,))
        return ident

    def apply_work_evidence(self, evidence):
        """原 tools/reconcile_works.py --apply 的入口（工具已于 #186 删除）。"""
        return self.store.apply_work_evidence(evidence)

    @staticmethod
    def evidence(tags, edges):
        return {'tags': [{'name': t, 'category': 3} for t in tags],
                'implications': [{'antecedent_name': a, 'consequent_name': b, 'status': 'active'} for a, b in edges]}

    def parent(self, ident):
        return self.one('SELECT parent_id FROM entities WHERE id=?', ident)[0]

    def review(self, ident):
        return self.one('SELECT status,note,parent_locked FROM work_review WHERE entity_id=?', ident)

    def test_new_works_are_locked_with_or_without_a_parent(self):
        self.switch()
        top = self.store.edit_work('create', name='顶层')['id']
        child = self.store.edit_work('create', name='子作品', parent=top)['id']
        self.assertEqual(self.review(top), ('pending', '', 1))
        self.assertEqual(self.review(child), ('pending', '', 1))
        self.assertEqual(self.parent(child), top)

    def test_work_with_a_parent_keeps_it_when_evidence_offers_two(self):
        a, b, c = self.work('a_work'), self.work('b_work'), self.work('c_work')
        with self.store.connect() as conn:
            conn.execute('UPDATE entities SET parent_id=? WHERE id=?', (b, a))   # 早先由证据挂上，未上锁
        self.switch()
        report = self.apply_work_evidence(self.evidence(['a_work', 'b_work', 'c_work'], [('a_work', 'b_work'), ('a_work', 'c_work')]))
        self.assertEqual(self.parent(a), b)
        self.assertEqual(self.review(a), ('confirmed', '多个作品上级，需人工选择：b_work, c_work', 0))
        self.assertEqual(report['tree_edges'], [])
        self.assertEqual([p['id'] for p in report['pending']], [a])
        self.assertIsNone(self.parent(c))

    def test_two_way_implication_accepts_the_edge_processed_first(self):
        a, b = self.work('a_work'), self.work('b_work')
        self.switch()
        report = self.apply_work_evidence(self.evidence(['a_work', 'b_work'], [('b_work', 'a_work'), ('a_work', 'b_work')]))
        self.assertEqual((self.parent(a), self.parent(b)), (b, None))
        self.assertEqual(report['tree_edges'], [['a_work', 'b_work']])
        self.assertEqual(self.review(b), ('confirmed', '蕴含关系成环，需人工确认', 0))
        # 再跑一次结果不变。
        self.apply_work_evidence(self.evidence(['a_work', 'b_work'], [('b_work', 'a_work'), ('a_work', 'b_work')]))
        self.assertEqual((self.parent(a), self.parent(b)), (b, None))

    def test_locked_work_is_left_alone_and_move_locks(self):
        a, b, c = self.work('a_work'), self.work('b_work'), self.work('c_work')
        self.switch()
        self.store.edit_work('move', a, parent=c)
        self.assertEqual(self.review(a)[2], 1)
        self.apply_work_evidence(self.evidence(['a_work', 'b_work'], [('a_work', 'b_work')]))
        self.assertEqual(self.parent(a), c)
        with self.assertRaises(ValueError):
            self.store.edit_work('move', c, parent=a)
        self.assertIsNone(self.parent(c))

    # ---- 名字：带 qualifier 的作品仍从词表选名 ----

    def test_qualifier_work_reads_the_vocabulary_name(self):
        with self.store.connect() as c:
            c.execute("INSERT INTO entities(id,name,kind) VALUES ('w-legacy','','work')")
            c.execute("INSERT INTO work_review(entity_id) VALUES ('w-legacy')")
            Store.link(c, 'w-legacy', 'qualifier', 'my_series')
            c.execute("INSERT INTO display_names(kind,key,lang,source,value) VALUES ('work','my_series','zh','index','旧词表名')")
            # names 里来源更高的新名字：带 qualifier 的作品不读它。
            names.put_name(c, 'w-legacy', 'zh', 'bangumi', '新名字')
        self.switch()
        shown = {r['id']: r['name'] for r in self.store.works()}
        self.assertEqual(shown['w-legacy'], '旧词表名')
        self.store.edit_work('rename', 'w-legacy', name='我起的名字')
        self.assertEqual({r['id']: r['name'] for r in self.store.works()}['w-legacy'], '我起的名字')


class UnsetPolicy(Shared, Contract):
    policy = None

    def test_destructive_merge_undo_restores_everything(self):
        self.switch()
        self.store.change(['c'], 'confirmed', self.bob)
        self.switch()
        merge = self.store.merge(self.bob, self.alice)
        self.assertEqual(self.one('SELECT merged_into FROM entities WHERE id=?', self.bob), (self.alice,))
        self.assertEqual(self.assertion('c', self.alice), ('confirmed', 'review'))
        self.assertIsNone(self.assertion('c', self.bob))
        self.store.undo(merge['id'])
        self.assertEqual(self.one('SELECT merged_into FROM entities WHERE id=?', self.bob), (None,))
        self.assertEqual(self.assertion('c', self.bob), ('confirmed', 'review'))
        self.assertIsNone(self.assertion('c', self.alice))


class HybridPolicy(Shared, Contract):
    policy = 'hybrid'


class HybridGraph(Contract):
    """身份图（hybrid）：先等价后层级、b 为代表、不限制同层合并。"""

    def skin(self, ident, parent, variant):
        with self.store.connect() as c:
            c.execute("INSERT INTO entities(id,name,kind,parent_id,variant) VALUES (?,'','skin',?,?)", (ident, parent, variant))

    def node(self, ident):
        return self.one('SELECT parent_id,variant,kind,merged_into FROM entities WHERE id=?', ident)

    def relation(self, a, b, kind):
        return self.one('SELECT id,status FROM identity_relations WHERE a=? AND b=? AND kind=?', a, b, kind)

    def decisions(self, relation):
        with self.store.connect() as c:
            return [(r['previous_status'], r['status'], r['reason'],
                     sorted(x[0] for x in c.execute('SELECT fingerprint FROM identity_decision_evidence WHERE decision_id=?', (r['id'],))))
                    for r in c.execute('SELECT * FROM identity_decisions WHERE relation_id=? ORDER BY id', (relation,))]

    def fingerprints(self, relation):
        return sorted(r[0] for r in self.rows('SELECT fingerprint FROM identity_evidence WHERE relation_id=?', relation))

    def test_an_independent_person_s_alias_joins_an_existing_skin_group(self):
        self.skin('summer', self.alice, 'alice_(summer)')
        self.activate('hybrid')
        self.switch()
        self.store.decide_identity(a=self.bob, b='summer', kind='duplicate_of', status='accepted', reason='同一套皮肤')
        self.assertEqual(self.node(self.bob), (self.alice, 'bob', 'skin', 'summer'))
        self.assertEqual(self.node('summer'), (self.alice, 'alice_(summer)', 'skin', None))
        self.assertEqual(self.rows('SELECT entity,canonical FROM identity_canonical WHERE entity IN (?,?) ORDER BY entity', self.bob, 'summer'),
                         sorted([(self.bob, 'summer'), ('summer', 'summer')]))

    def test_representative_is_b_and_form_applies_to_the_whole_group(self):
        self.switch()
        person = self.store.create_entity('Person')['id']
        self.activate('hybrid')
        self.switch()
        self.store.decide_identity(a=self.alice, b=self.bob, kind='duplicate_of', status='accepted', reason='同一人')
        self.store.decide_identity(a=self.alice, b=person, kind='form_of', status='accepted', reason='是变体')
        self.assertEqual(self.node(self.alice)[::3], (person, self.bob))
        self.assertEqual(self.node(self.bob)[::3], (person, None))

    def test_undoing_a_relation_decision_records_what_it_reviewed(self):
        self.skin('summer', self.alice, 'alice_(summer)')
        self.activate('hybrid')
        relation, status = self.relation('summer', self.alice, 'form_of')
        self.switch()
        evidence = self.fingerprints(relation)
        self.assertEqual(status, 'accepted')
        self.assertEqual(self.store.identity_status()['legacy_hierarchy_pending_people'], 1)
        # 人工退回 proposed：不回落成机器关系，皮肤不再挂在人物下。
        back = self.store.decide_identity(relation_id=relation, status='proposed', reason='再看看')
        self.assertEqual(self.relation('summer', self.alice, 'form_of')[1], 'proposed')
        self.assertEqual(self.node('summer')[0], None)
        self.assertEqual(self.store.identity_status()['legacy_hierarchy_pending_people'], 0)
        # 撤销 = 追加一条回到原状态的决议，并记下当时关系上的全部证据：继承层级因此算「已复核」。
        self.store.undo(back['id'])
        self.assertEqual(self.relation('summer', self.alice, 'form_of')[1], 'accepted')
        self.assertEqual(self.node('summer')[0], self.alice)
        self.assertEqual(self.store.identity_status()['legacy_hierarchy_pending_people'], 0)
        self.assertEqual(self.decisions(relation), [
            ('proposed', 'accepted', '保留既有层级并使其可撤销；不代表新增人工确认', []),
            ('accepted', 'proposed', '再看看', evidence),
            ('proposed', 'accepted', '撤销人工关系决议', evidence)])

    def test_undoing_a_merge_made_before_activation(self):
        self.switch()
        self.store.change(['c'], 'confirmed', self.bob)
        merge = self.store.merge(self.bob, self.alice)          # 策略未设置时的破坏式合并
        self.activate('hybrid')
        relation, status = self.relation(self.bob, self.alice, 'duplicate_of')
        self.switch()
        self.assertEqual(status, 'accepted')
        self.assertEqual(self.node(self.bob)[3], self.alice)
        self.store.undo(merge['id'])
        self.assertEqual(self.relation(self.bob, self.alice, 'duplicate_of')[1], 'proposed')
        self.assertEqual(self.decisions(relation)[-1][:3], ('accepted', 'proposed', '撤销历史合并；原归属无法区分的认领待确认'))
        self.assertEqual(self.decisions(relation)[-1][3], self.fingerprints(relation))
        self.assertEqual(self.node(self.bob)[3], None)
        # 结论不搬回去：合并时搬到代表上的结论留在代表上，拆开的组挂起待确认。
        self.assertEqual(self.assertion('c', self.alice), ('confirmed', 'review'))
        self.assertIsNone(self.assertion('c', self.bob))
        self.assertEqual(sorted(self.rows('SELECT uuid,entity,reason FROM identity_suspended')),
                         [('a', self.alice, 'equivalence_split'), ('c', self.alice, 'equivalence_split')])
        self.assertEqual(self.store.identity_assignments('c')['assignments'], [])

    def test_undoing_an_old_merge_without_its_migrated_relation_is_refused(self):
        self.switch()
        merge = self.store.merge(self.bob, self.alice)
        self.activate('hybrid')
        with self.store.connect() as c:
            c.execute("UPDATE identity_relations SET kind='different_from',a=min(a,b),b=max(a,b) WHERE kind='duplicate_of'")
        self.switch()
        state = self.state()
        with self.assertRaises(ValueError):
            self.store.undo(merge['id'])
        self.assertEqual(self.state(), state)


# 既有切换验收类名保留；同一套断言现在均经过完整核心。
class SwitchedUnsetPolicy(UnsetPolicy):
    pass


class SwitchedHybridPolicy(HybridPolicy):
    pass


class SwitchedHybridGraph(HybridGraph):
    pass


if __name__ == '__main__':
    unittest.main()


class CharactersSync(Contract):
    """S2 影子收编本身：分步同步、回滚、预检、灵敏度。"""
    sync_check = False

    def sync(self):
        return assert_characters_sync(self, self.store)

    def refused(self):
        rows = term_rows(self.store.path)
        with self.assertRaises(terms_core.Failed) as caught:
            terms_core.run('sync-characters', '--db', self.store.path)
        self.assertEqual(term_rows(self.store.path), rows, '没通过就什么都不留')
        message = str(caught.exception)
        return json.loads(message[message.index('{'):])

    def steps(self):
        """一串旧操作：每一步之后同步都要零差异（旧代码在收编之后继续写，下一次同步补上）。"""
        s, a, b = self.store, self.alice, self.bob
        yield lambda: s.change(['a'], 'rejected', a)
        undo = []
        yield lambda: undo.append(s.change(['b'], 'confirmed', b))
        yield lambda: s.undo(undo[0]['id'])
        yield lambda: s.sweep(a)
        yield lambda: s.change(['b', 'c'], 'unknown')
        yield lambda: s.reassign(['b'], a, b) if any(m['id'] == a for m in s.detail('b')['assertions']) else None
        yield lambda: self.add_source("INSERT INTO decisions VALUES ('c','character','alice_album','album:Alice album pk=1')")
        yield lambda: s.refresh()
        yield lambda: s.edit_work('create', name='系列', tag='series')
        yield lambda: s.set_work(a, s.works()[0]['id'])
        with sqlite3.connect(self.source) as src:
            src.execute("UPDATE assets SET trashed=1 WHERE uuid='a'")
        yield lambda: s.import_changes(removed=['a'])

    def run_steps(self):
        terms_core.run('migrate', '--db', self.store.path)
        self.sync()
        self.switch()
        for step in self.steps():
            step()
            self.sync()

    def test_sync_after_every_step_unset(self):
        self.run_steps()

    def test_sync_after_every_step_hybrid(self):
        self.activate('hybrid')
        self.run_steps()

    def test_merge_undone_under_each_policy(self):
        self.switch()
        self.store.change(['c'], 'confirmed', self.bob)
        merge = self.store.merge(self.bob, self.alice)
        self.sync()
        self.store.undo(merge['id'])
        self.sync()
        self.store.change(['b'], 'confirmed', self.bob)   # 第二次合并搬的比第一次多
        merge = self.store.merge(self.bob, self.alice)
        self.activate('hybrid')
        self.sync()
        self.switch()
        self.store.undo(merge['id'])
        self.sync()

    def test_tool_acceptance_after_a_human_decision(self):
        """原 tools/identity_rollout.py（#186 删除）的 accept_many 写的决议没有 action：排在同一关系上前一条决议之后。"""
        from web import identity
        with self.store.connect() as c:
            c.execute("INSERT INTO entities(id,name,kind,parent_id,variant) VALUES ('summer','','skin',?,'alice_(summer)')", (self.alice,))
        self.activate('hybrid')
        terms_core.run('migrate', '--db', self.store.path)
        self.sync()
        relation = self.one("SELECT id FROM identity_relations WHERE kind='form_of'")[0]
        self.switch()
        self.store.decide_identity(relation_id=relation, status='proposed', reason='再看看')
        self.sync()
        self.assertEqual(self.store._core({'op': 'accept-many', 'ids': [relation], 'reason': '自动接受'})['accepted'], [relation])
        self.assertEqual(self.one('SELECT status FROM identity_relations WHERE id=?', relation), ('accepted',))
        self.sync()

    def test_unexplained_history_rolls_everything_back(self):
        self.switch()
        self.store.change(['c'], 'confirmed', self.alice)
        self.store = terms_seed.startup_input(self.store)
        terms_core.run('migrate', '--db', self.store.path)
        with self.store.connect() as c:   # 绕过历史改了表：这一步写的应是 review
            c.execute("UPDATE assertions SET origin='album' WHERE uuid='c'")
            Store._refresh_memberships(c)
        report = self.refused()
        self.assertEqual(report['unexplained_count'], 1)
        self.assertEqual(report['unexplained'][0]['uuid'], 'c')

    def test_tampering_after_a_sync_is_reported(self):
        self.switch()
        self.store.change(['c'], 'confirmed', self.alice)
        self.store = terms_seed.startup_input(self.store)
        terms_core.run('migrate', '--db', self.store.path)
        self.sync()
        with self.store.connect() as c:
            c.execute("UPDATE assertions SET origin='sweep' WHERE uuid='c'")
            Store._refresh_memberships(c)
        report = self.refused()
        self.assertEqual(report['unexplained'][0]['core'], ['yes', 'review'])
        with self.store.connect() as c:
            c.execute("UPDATE assertions SET origin='review' WHERE uuid='c'")
            Store._refresh_memberships(c)
        self.sync()

    def test_preflight_failures_roll_everything_back(self):
        terms_core.run('migrate', '--db', self.store.path)
        with self.store.connect() as c:
            c.execute("UPDATE entities SET name='旧名字' WHERE id=?", (self.alice,))
            c.execute("INSERT INTO links(system,key,entity_id,source) VALUES ('wd_character','x',?,'test')", (self.bob,))
        report = self.refused()
        self.assertEqual({p['check'] for p in report['preflight']},
                         {'entities.name 不为空（旧迁移应已清空）', 'links 里已有 wd_character（与角色 WD 链接改名冲突）'})

    def test_v2_policy_is_refused(self):
        """v2 已放弃（Store 启动时会把 v2 的库转成 hybrid）；核心仍拒绝切换 v2 的库，兜底。"""
        self.store.activate_identity()
        with self.store.connect() as c:
            c.execute("UPDATE meta SET value='v2' WHERE key='identity_policy'")
        terms_core.run('migrate', '--db', self.store.path)
        report = self.refused()
        self.assertEqual(report['preflight'], [{'check': '只支持身份策略未设置或 hybrid', 'policy': 'v2'}])

    def test_shadow_tables_are_written_only_by_the_core(self):
        terms_core.run('migrate', '--db', self.store.path)
        self.sync()
        with self.store.connect() as c:
            for sql in ("UPDATE shadow_entities SET work='x'", 'DELETE FROM shadow_assertions', 'DELETE FROM term_tree'):
                with self.subTest(sql=sql), self.assertRaises(sqlite3.IntegrityError):
                    c.execute(sql)
