"""C3 身份命令：固定旧输入经启动切换后，对照旧路径检查点与两种核心调用形式。

保留原始历史、时间、逐行状态、基线、全量重建与事务回滚断言。历史合并输入从已保存的旧库加载，
不再调用旧 Python 写路径制造历史；新关系与批量接受都由核心完成。
"""
import copy
import json
import sqlite3
import tempfile
import time
import unittest
from pathlib import Path

from tests import terms_seed
from tests.terms_c3_snapshots import Checkpoints, deterministic
from tests.test_terms_core import core
from tests.test_terms_character_learning import snapshot, dump
from tests.test_terms_characters_contract import COMPAT, PROJECTIONS, refreshed_projections
from web import identity, terms_core

PRIVATE = ('identity_canonical', 'identity_scopes', 'identity_suspended', 'identity_uncertain',
           'identity_anchor_decisions', 'identity_anchor_history', 'learn_state',
           'asked', 'reviews', 'deferred', 'backend_effects')


class IdentityCommands(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        if core() is None:
            raise unittest.SkipTest('no terms core binary and no cargo')

    def setUp(self):
        deterministic(self)
        self.checkpoints = Checkpoints(self)
        self.boot()

    def boot(self, legacy_merge=False):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        root = Path(temp.name)
        self._boots = getattr(self, '_boots', 0) + 1
        name = terms_seed.key(self) + f'_boot{self._boots}'
        self.api, attrs = terms_seed.load(name, root, db=root / 'api.sqlite')
        for key, value in attrs.items():
            setattr(self, key, value)
        terms_seed.switch(self.api)
        terms_seed.resume_ids(self, name)
        self.new = copy.copy(self.api)
        self.new.path = root / 'core.sqlite'
        with sqlite3.connect(self.api.path) as src, sqlite3.connect(self.new.path) as dst:
            src.backup(dst)
        self.same()

    def scope_input(self):
        """原测试中人工布置的认领范围：旧输入先固定，再执行真实启动切换；不解除核心写保护。"""
        root = self.api.path.parent
        name = 'mid_scopes_' + terms_seed.key(self)
        self.api, _ = terms_seed.load(name, root, db=root / 'scoped-api.sqlite')
        terms_seed.switch(self.api)
        terms_seed.resume_ids(self, name)
        self.new = copy.copy(self.api)
        self.new.path = root / 'scoped-core.sqlite'
        with sqlite3.connect(self.api.path) as src, sqlite3.connect(self.new.path) as dst:
            src.backup(dst)

    @staticmethod
    def aligned():
        fraction = time.time() % 1
        if fraction > .6:
            time.sleep(1 - fraction + .01)

    def command(self, **args):
        return terms_core.run('identity-command', '--db', self.new.path, stdin=json.dumps(args, ensure_ascii=False))

    def same(self, *, rebuild=True):
        self.checkpoints.capture(self.api, COMPAT + PROJECTIONS + PRIVATE + ('identity_baseline', 'actions'))
        self.assertEqual(snapshot(self.api, COMPAT + PROJECTIONS + PRIVATE),
                         snapshot(self.new, COMPAT + PROJECTIONS + PRIVATE))
        with self.api.connect() as a, self.new.connect() as b:
            query = "SELECT key,value FROM meta WHERE key LIKE 'identity_%' ORDER BY key"
            self.assertEqual(list(map(tuple, a.execute(query))), list(map(tuple, b.execute(query))))
            self.assertEqual(list(map(tuple, a.execute('SELECT * FROM identity_baseline ORDER BY rowid'))),
                             list(map(tuple, b.execute('SELECT id,parent_id,variant,kind,merged_into FROM term_identity_baseline ORDER BY seq'))))
            # New history has a versioned envelope; the underlying snapshot is identical.
            def actions(c):
                result = []
                for r in c.execute('SELECT id,label,before_json,undone,ts FROM actions ORDER BY id'):
                    before = json.loads(r['before_json'])
                    if before and 'terms' in before[0] and isinstance(before[0]['terms'], dict) and before[0]['terms'].get('v') == 2:
                        before = before[0]['terms']['before']
                    result.append((r['id'],r['label'],before,r['undone'],r['ts']))
                return result
            self.assertEqual(actions(a), actions(b))
        self.assertTrue(terms_core.run('verify-characters', '--db', self.new.path)['committed'])
        if rebuild:
            self.rebuilt_equal()

    def rebuilt_equal(self):
        full = refreshed_projections(self.new.path, True)
        self.assertEqual({t: sorted(full[t], key=repr) for t in PROJECTIONS}, snapshot(self.new, PROJECTIONS))

    def decide(self, **args):
        self.aligned()
        old = self.api.decide_identity(**args)
        result = self.command(op='decide', effect='verdict', **args)
        self.assertEqual(old['id'], result['action'])
        self.same()
        return old['id']

    def undo(self, action):
        self.aligned()
        self.api.undo(action)
        self.command(op='undo', action=action, effect='verdict')
        self.same()

    def propose(self, a, b, kind, evidence=None, source='manual'):
        self.aligned()
        result = self.api._core(dict(op='propose',a=a,b=b,kind=kind,evidence=evidence,source=source))['relation_id']
        actual = self.command(op='propose',a=a,b=b,kind=kind,evidence=evidence,source=source)
        self.assertEqual(result,actual['relation_id'])
        self.same()
        return result

    def test_cross_layer_duplicate_preserves_b_representative_and_variant_fallback(self):
        action = self.decide(a=self.bob,b=self.skin,kind='duplicate_of',status='accepted',reason='同一套皮肤')
        with self.new.connect() as c:
            self.assertEqual(tuple(c.execute('SELECT parent_id,variant,kind,merged_into FROM entities WHERE id=?',(self.bob,)).fetchone()),
                             (self.alice,'bob','skin',self.skin))
        self.undo(action)

    def test_representative_b_then_hierarchy_applies_to_the_whole_component(self):
        a = self.decide(a=self.alice,b=self.bob,kind='duplicate_of',status='accepted',reason='同一人')
        b = self.decide(a=self.alice,b=self.person,kind='form_of',status='accepted',reason='是变体')
        self.undo(b)
        self.undo(a)

    def test_clear_and_undo_review_all_evidence_and_preserve_pending_hash(self):
        with self.api.connect() as c:
            relation = c.execute("SELECT id FROM identity_relations WHERE a=? AND kind='form_of'",(self.skin,)).fetchone()[0]
            self.assertEqual(len(identity.pending_hierarchy_people(c)),1)
        step=self.decide(relation_id=relation,status='proposed',reason='再看看')
        # Evidence arriving after the decision is also reviewed by undo.
        self.propose(self.skin,self.alice,'form_of',{'note':'new\t证据','n':1e-5},'legacy-hierarchy')
        self.undo(step)
        with self.new.connect() as c:
            self.assertEqual(identity.pending_hierarchy_people(c),set())

    def test_relation_fingerprint_filters_metadata_and_matches_python_float_format(self):
        values={'中文🙂':[1e-7,5.72e-5,1e16,-0.0,True,None], 'response_ids':['a'],
                'nested':{'fetched_at':'today','response_id':'x','keep':'yes'}}
        relation=self.propose(self.bob,self.person,'duplicate_of',values)
        values['response_ids']=['b'];values['nested']['fetched_at']='tomorrow'
        self.assertEqual(self.propose(self.person,self.bob,'duplicate_of',values),relation)
        with self.new.connect() as c:
            self.assertEqual(c.execute('SELECT count(*) FROM identity_evidence WHERE relation_id=?',(relation,)).fetchone()[0],1)
        # Different-from uses lexical strings, independently of numeric term IDs.
        self.propose(self.person,self.bob,'different_from',{'n':.0000572})

    def test_contradiction_and_mid_transaction_error_leave_everything_unchanged(self):
        before=dump(self.new)
        with self.assertRaisesRegex(terms_core.Failed,'变体不能与其人物重复'):
            self.command(op='decide',a=self.alice,b=self.skin,kind='duplicate_of',status='accepted',reason='bad',effect='verdict')
        self.assertEqual(dump(self.new),before)
        with self.new.connect() as c:
            c.execute("CREATE TRIGGER identity_failure BEFORE INSERT ON identity_canonical BEGIN SELECT RAISE(ABORT,'injected graph'); END")
        before=dump(self.new)
        with self.assertRaisesRegex(RuntimeError,'injected graph'):
            self.command(op='decide',a=self.alice,b=self.bob,kind='duplicate_of',status='accepted',reason='fails',effect='verdict')
        self.assertEqual(dump(self.new),before)

    def test_accept_many_checks_each_edge_and_preserves_untrimmed_reason(self):
        a=self.propose(self.bob,self.person,'form_of',{'a':1})
        b=self.propose(self.person,self.bob,'form_of',{'b':2})
        d=self.propose(self.bob,self.person,'different_from',{'c':3})
        self.aligned()
        old=self.api._core(dict(op='accept-many',ids=[a,b,d,999],reason='  自动接受  '))
        new=self.command(op='accept-many',ids=[a,b,d,999],reason='  自动接受  ')
        self.assertEqual(old,new)
        self.assertEqual(new['accepted'],[a])
        self.assertEqual(len(new['skipped']),2)
        self.same()

    def test_legacy_hybrid_merge_undo_keeps_migration_verdicts_in_place(self):
        self.boot(legacy_merge=True)
        self.undo(self.merge_step)
        with self.new.connect() as c:
            self.assertIsNone(c.execute("SELECT 1 FROM assertions WHERE uuid='c' AND entity=?",(self.bob,)).fetchone())
            self.assertEqual(tuple(c.execute("SELECT status,origin FROM assertions WHERE uuid='c' AND entity=?",(self.alice,)).fetchone()),('confirmed','review'))
            self.assertTrue(c.execute("SELECT 1 FROM identity_suspended WHERE uuid='c'").fetchone())

    def test_unknown_mixed_history_and_wrong_recent_id_are_atomic(self):
        step=self.decide(a=self.bob,b=self.person,kind='duplicate_of',status='accepted',reason='ok')
        before=dump(self.new)
        with self.assertRaises(terms_core.Failed):
            self.command(op='undo',action=step+1)
        self.assertEqual(dump(self.new),before)
        with self.new.connect() as c:
            items=json.loads(c.execute('SELECT before_json FROM actions WHERE id=?',(step,)).fetchone()[0])
            items.append({'unknown':1})
            c.execute('UPDATE actions SET before_json=? WHERE id=?',(json.dumps(items),step))
        before=dump(self.new)
        with self.assertRaises(terms_core.Failed):
            self.command(op='undo',action=step)
        self.assertEqual(dump(self.new),before)

    def test_split_scopes_are_preserved_exactly(self):
        step = self.decide(a=self.bob, b=self.person, kind='duplicate_of', status='accepted', reason='合组')
        self.scope_input()
        self.aligned()
        self.api.undo(step)
        self.command(op='undo', action=step, effect='verdict')
        self.same(rebuild=False)
        # 旧代码在 identity_claims 拆分时写入未定却不刷新归属，增量与全量曾因此不同（被钉住的旧缺陷）；那张表只有 v2
        # 才有内容，随 v2 删除之后增量等于全量。
        self.rebuilt_equal()
        with self.new.connect() as c:
            self.assertEqual(c.execute('SELECT reason FROM identity_suspended').fetchone()[0], 'equivalence_split')

    def test_creation_after_split_keeps_legacy_delayed_refresh_semantics(self):
        self.test_split_scopes_are_preserved_exactly()
        made = self.api.create_entity('拆分后新人物')
        terms_core.character_command(self.new.path, {'op': 'create_entity', 'id': made['id'],
                                                     'name': '拆分后新人物', 'lang': 'zh', 'kind': '', 'artist': ''})
        self.same()

    def test_profile_undo_after_split_refreshes_legacy_delayed_results(self):
        self.test_split_scopes_are_preserved_exactly()
        self.aligned()
        self.api.set_profile(self.alice, name='撤销前名字')
        with self.api.connect() as c:
            step = c.execute('SELECT max(id) FROM actions WHERE undone=0').fetchone()[0]
        terms_core.character_command(self.new.path, {'op': 'set_profile', 'entity': self.alice,
                                                     'name': '撤销前名字', 'name_lang': 'zh', 'effect': 'verdict'})
        self.same(rebuild=False)
        self.undo(step)

    def test_removing_redundant_edge_keeps_equivalence(self):
        self.decide(a=self.alice, b=self.bob, kind='duplicate_of', status='accepted', reason='一')
        self.decide(a=self.person, b=self.bob, kind='duplicate_of', status='accepted', reason='二')
        step = self.decide(a=self.alice, b=self.person, kind='duplicate_of', status='accepted', reason='冗余')
        self.scope_input()
        self.undo(step)
        with self.new.connect() as c:
            self.assertEqual(c.execute('SELECT count(*) FROM identity_suspended').fetchone()[0], 0)
