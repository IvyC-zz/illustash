"""Product invariants: curated state survives refresh, undo and restart."""
import hashlib
import sqlite3
import tempfile
import unittest
from pathlib import Path
from web.store import DISPLAY, Invalid, Store

# C3：普通测试与新图库都默认建成完整词条核心形态。

from tests.synthetic_source import source_fixture as fixture


class StoreTest(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        self.source = fixture(self.root)
        self.digest = hashlib.sha256(self.source.read_bytes()).hexdigest()
        if self._testMethodName == 'test_variants_have_distinct_display_names':
            # 原用例验证旧名字列迁移：先布置固定旧输入，再真实启动切换，被测读取全部在核心形态。
            from tests import terms_seed
            self.source.unlink()
            self.store, attrs = terms_seed.load('refusals_none', self.root)
            with self.store.connect() as c:
                c.execute("INSERT INTO entities(id,name,parent_id,variant,kind) VALUES ('bob-swim','Bob swim',?,'swimsuit','skin')", (attrs['bob'],))
                Store._names_from_tables(c)
            terms_seed.switch(self.store)
            self.digest = hashlib.sha256(self.source.read_bytes()).hexdigest()
        else:
            self.store = Store(self.source, self.root/'app.sqlite')
            self.store.refresh()
        self.alice = self.store.entities('Alice')[0]['id']
        self.bob = self.store.entities('Bob')[0]['id']

    def tearDown(self):
        self.assertEqual(hashlib.sha256(self.source.read_bytes()).hexdigest(), self.digest)
        from tests.test_web_v2 import assert_memberships_current
        assert_memberships_current(self, self.store)
        self.temp.cleanup()

    def test_import_scope_and_candidate_deduplication(self):
        self.assertEqual(self.store.overview()['total'],3)
        self.assertEqual(self.store.overview()['confirmed'],1)
        tasks={t['id']:t['unchecked'] for t in self.store.characters(minimum=1)}
        self.assertEqual(tasks, {self.alice:1,self.bob:1})
        self.assertEqual(len(self.store.detail('a')['tags']),1)
        # The album title is a search alias; display uses the readable name.
        self.assertEqual(self.store.entities('Alice album')[0]['name'],'Alice')
        self.assertEqual(self.store.detail('a')['assertions'][0]['name'],'Alice')
        self.assertEqual(self.store.create_entity('Alice album')['id'],self.alice)

    def test_confirm_multi_character_and_persistent_undo(self):
        self.store.change(['b'],'confirmed',self.alice)
        action=self.store.change(['b'],'confirmed',self.bob)
        self.assertEqual(len(self.store.detail('b')['assertions']),2)
        reopened=Store(self.source,self.store.path)
        reopened.undo(action['id'])
        self.assertEqual([s['id'] for s in reopened.detail('b')['assertions']],[self.alice])
        self.assertEqual(reopened.assets(status='confirmed')['total'],2)

    def test_reject_imported_identity_refresh_does_not_restore(self):
        action=self.store.change(['a'],'rejected',self.alice)
        self.store.refresh()
        self.assertEqual(self.store.detail('a')['assertions'][0]['status'],'rejected')
        self.assertEqual(self.store.assets(entity=self.alice)['total'],1)
        self.store.undo(action['id'])
        self.assertEqual(self.store.detail('a')['assertions'][0]['origin'],'album')

    def test_unknown_preserves_confirmed_and_undo(self):
        action=self.store.change(['a','b'],'unknown')
        self.assertEqual(len(self.store.detail('a')['assertions']),1)
        self.assertEqual(self.store.assets(entity=self.alice)['total'],2)
        self.assertEqual(self.store.assets(status='unknown')['total'],2)
        self.store.undo(action['id'])
        self.assertEqual(self.store.assets(status='unknown')['total'],0)

    def test_confirmation_resumes_and_undo_restores_unknown(self):
        self.store.change(['b'],'unknown')
        action=self.store.change(['b'],'confirmed',self.alice)
        self.assertFalse(self.store.detail('b')['unknown'])
        self.store.undo(action['id'])
        self.assertTrue(self.store.detail('b')['unknown'])
        self.assertEqual(self.store.detail('b')['assertions'],[])

    def test_batch_is_atomic_and_invalid_ids_rejected(self):
        with self.assertRaises(Invalid):
            self.store.change(['a','missing'],'rejected',self.alice)
        self.assertEqual(self.store.detail('a')['assertions'][0]['status'],'confirmed')
        self.assertIsNone(self.store.overview()['undo'])
        for ids in [[], ['a']*501, 'a', [123]]:
            with self.assertRaises(Invalid):
                self.store.change(ids,'unknown')
        with self.assertRaises(Invalid):
            self.store.change(['a'],'confirmed','missing')

    def test_undo_requires_latest_action_and_supports_stack(self):
        first=self.store.change(['b'],'confirmed',self.alice)
        second=self.store.change(['b'],'rejected',self.alice)
        with self.assertRaises(Invalid):self.store.undo(first['id'])
        self.assertEqual(self.store.undo(second['id'])['id'],first['id'])
        self.assertIsNone(self.store.undo(first['id']))
        self.assertEqual(self.store.detail('b')['assertions'],[])

    def test_alias_search_filter_and_pagination(self):
        self.assertEqual(self.store.entities('爱丽丝')[0]['id'],self.alice)
        self.assertEqual(self.store.assets(query='爱丽丝')['total'],2)
        self.assertEqual(self.store.assets(query='a_li_ce')['total'],2)
        self.assertEqual(self.store.assets(query='%')['total'],0)
        self.assertEqual(self.store.assets(query="' OR 1=1 --")['total'],0)
        self.assertEqual(self.store.assets(entity=self.alice,status='confirmed')['total'],1)
        self.assertEqual(self.store.assets(entity=self.bob,status='confirmed')['total'],0)
        self.assertEqual(self.store.assets(entity=self.alice,status='machine')['total'],1)
        self.assertEqual(len(self.store.assets(offset=1,limit=1)['items']),1)

    def test_local_entity_and_assertion_survive_refresh(self):
        char=self.store.create_entity('New OC')
        self.store.change(['c'],'confirmed',char['id'])
        self.store.refresh()
        self.assertEqual(self.store.entities('New OC')[0]['id'],char['id'])
        self.assertEqual(self.store.assets(entity=char['id'],status='confirmed')['total'],1)
        self.assertEqual(self.store.create_entity('new oc')['id'],char['id'])

    def test_resume_and_unknown_are_explicit(self):
        self.store.change(['b'],'unknown')
        self.store.change(['b'],'resume')
        self.assertEqual(len(self.store.characters(minimum=1)),2)

    def test_backup_includes_live_curated_state(self):
        self.store.change(['c'],'confirmed',self.bob)
        path=self.store.backup()
        with sqlite3.connect(path) as c:
            self.assertEqual(c.execute("SELECT status FROM assertions WHERE uuid='c'").fetchone()[0],'confirmed')
            self.assertEqual(c.execute('PRAGMA quick_check').fetchone()[0],'ok')

    def test_variants_have_distinct_display_names(self):
        self.assertEqual(self.store.detail('b')['memberships'][0]['name'],'Alice')
        with self.store.connect() as c:
            self.assertEqual(c.execute(f"SELECT {DISPLAY} FROM entities e WHERE id='bob-swim'").fetchone()[0],'Bob swim')
        self.assertEqual(self.store.characters()[0]['name'],'Alice')

    def test_general_tags_are_filters_not_confirmations(self):
        self.assertIn('blue_hair',self.store.general_tags('blue_hair'))
        self.assertEqual(self.store.assets(tag='blue_hair')['total'],1)
        self.assertEqual(self.store.assets(tag='blue_hair',status='confirmed')['total'],1)
        with self.assertRaises(Invalid):self.store.assets(tag='not_a_real_tag')
        self.assertEqual(self.store.detail('a')['assertions'][0]['status'],'confirmed')

    def test_review_pools_and_correction_undo(self):
        pools={p['kind']:p['count'] for p in self.store.review_pools()}
        self.assertEqual(pools['unmatched'],1)
        self.assertEqual(self.store.assets(pool='unmatched')['items'][0]['uuid'],'c')
        self.assertNotIn('conflicts',pools)
        self.store.change(['b'],'confirmed',self.alice)
        with self.assertRaises(Invalid): self.store.assets(pool='conflicts')
        self.store.change(['b'],'unknown')
        self.assertEqual(self.store.assets(status='unknown')['total'],1)
        self.store.undo(self.store.overview()['undo']['id'])
        action=self.store.correct('a',self.alice,self.bob)
        assertions={r['id']:r['status'] for r in self.store.detail('a')['assertions']}
        self.assertEqual(assertions,{self.alice:'rejected',self.bob:'confirmed'})
        self.store.refresh()
        self.assertEqual({r['id']:r['status'] for r in self.store.detail('a')['assertions']},assertions)
        self.store.undo(action['id'])
        self.assertEqual(self.store.detail('a')['assertions'][0]['origin'],'album')
        with self.assertRaises(Invalid):self.store.correct('a',self.alice,self.alice)
        with self.assertRaises(Invalid):self.store.correct('c',self.alice,self.bob)

    def test_reassign_candidate_batch_is_atomic_and_undoable(self):
        other=self.store.create_entity('Other role')
        with self.assertRaises(Invalid):self.store.reassign(['b','c'],self.alice,other['id'])
        self.assertEqual(self.store.detail('b')['assertions'],[])
        action=self.store.reassign(['b'],self.alice,other['id'])
        self.assertEqual({r['id']:r['status'] for r in self.store.detail('b')['assertions']},
                         {self.alice:'rejected',other['id']:'confirmed'})
        self.assertEqual(self.store.assets(entity=self.alice,status='machine')['total'],0)
        self.store.undo(action['id'])
        self.assertEqual(self.store.detail('b')['assertions'],[])
        self.assertEqual(self.store.assets(entity=self.alice,status='machine')['total'],1)

    def test_reassign_to_several_characters_in_one_undoable_step(self):
        one,two=self.store.create_entity('Role one'),self.store.create_entity('Role two')
        action=self.store.reassign(['b'],self.alice,[one['id'],two['id']])
        self.assertEqual({r['id']:r['status'] for r in self.store.detail('b')['assertions']},
                         {self.alice:'rejected',one['id']:'confirmed',two['id']:'confirmed'})
        self.assertIn('Role one、Role two',action['label'])
        self.store.undo(action['id'])
        self.assertEqual(self.store.detail('b')['assertions'],[])
        with self.assertRaises(Invalid):self.store.reassign(['b'],self.alice,[])
        with self.assertRaises(Invalid):self.store.reassign(['b'],self.alice,[one['id'],self.alice])

    def test_source_cannot_be_used_as_application_db(self):
        with self.assertRaises(Invalid):Store(self.source,self.source)


if __name__=='__main__':unittest.main()
