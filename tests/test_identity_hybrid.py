"""Hybrid identity policy: the relation layer is on, machine results stay formal memberships."""
import hashlib
import tempfile
import unittest
from pathlib import Path

from tests import terms_seed
from tests.test_web_v2 import assert_memberships_current
from web import identity
from web.store import Store


class HybridTest(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        self.store = terms_seed.base(self.root)
        self.source = self.store.source
        with self.store.connect() as c:
            ids = dict(c.execute("SELECT key,entity_id FROM links WHERE system='ccip'"))
        self.a, self.b = ids['alice'], ids['bob']
        if self._testMethodName == 'test_canonical_without_a_name_borrows_its_duplicate_s':
            # 公开 create_linked 会保存自动名字，此用例必须保持“只有链接、没有名字”的真实旧输入。
            with self.store.connect() as c:
                c.execute("INSERT INTO entities(id,name,work,kind) VALUES ('canon','','','character')")
                c.execute("INSERT INTO links(system,key,entity_id,source) VALUES ('danbooru','zed_(canonical)','canon','import')")
                c.execute("INSERT INTO names VALUES (?,'zh','爱丽丝','bangumi','display',0,0)", (self.a,))
        if self._testMethodName == 'test_close_same_tier_runner_up_is_flagged_not_withdrawn':
            # 原来的手工机器证据放进启动输入，完整切换后仍执行全部原断言。
            import sqlite3
            with sqlite3.connect(self.source) as c:
                c.executemany('INSERT INTO labels VALUES (?,?,?,?)', [('b','char_ext','bob',.96), ('b','general','solo',1)])
            with self.store.connect() as c:
                c.execute("INSERT INTO candidates VALUES ('b',?,'CCIP',0.96)", (self.b,))
                c.execute("INSERT INTO general_hits VALUES ('b','solo')")
                self.store._refresh_single(c)
                self.store._refresh_evidence(c)
                self.store._refresh_memberships(c)
        self.digest = hashlib.sha256(self.source.read_bytes()).hexdigest()
        with self.store.connect() as c:
            self.before = self.members(c)
        self.assertEqual(self.store.activate_identity(mode='hybrid')['policy'], 'hybrid')
        terms_seed.switch(self.store)

    def tearDown(self):
        self.assertEqual(hashlib.sha256(self.source.read_bytes()).hexdigest(), self.digest)
        assert_memberships_current(self, self.store)

    @staticmethod
    def members(c):
        return set(map(tuple, c.execute('SELECT uuid,entity,status FROM memberships')))

    def test_machine_memberships_survive_activation_and_refresh(self):
        self.assertTrue(any(s == 'machine' for _, _, s in self.before))
        with self.store.connect() as c:
            self.assertEqual(self.members(c), self.before)
        self.store.refresh()
        with self.store.connect() as c:
            self.assertEqual(self.members(c), self.before)

    def test_duplicate_regroups_machine_evidence_and_undo_restores(self):
        with self.store.connect() as c:
            raw = sorted(map(tuple, c.execute('SELECT * FROM candidates')))
        action = self.store.decide_identity(a=self.b, b=self.a, kind='duplicate_of', status='accepted', reason='test')
        self.assertIn(('b', self.b, 'machine'), self.before)
        with self.store.connect() as c:
            self.assertFalse(any(e == self.b for _, e, _ in self.members(c)))   # bob's evidence on b now counts for alice
            self.assertIn(('b', self.a, 'machine'), self.members(c))
            self.assertEqual(sorted(map(tuple, c.execute('SELECT * FROM candidates'))), raw)   # raw rows never move
        self.store.undo(action['id'])
        with self.store.connect() as c:
            self.assertEqual(self.members(c), self.before)

    def test_canonical_without_a_name_borrows_its_duplicate_s(self):
        # Danbooru's canonical tag (liv_(punishing:_gray_raven)) had no Chinese name; 丽芙 sat on liv_(pgr).
        canon = 'canon'
        with self.store.connect() as c:
            self.assertEqual(c.execute('SELECT entity_name(?)', (canon,)).fetchone()[0], 'Zed')
        self.store.decide_identity(a=self.a, b=canon, kind='duplicate_of', status='accepted', reason='test')
        with self.store.connect() as c:
            self.assertEqual(c.execute('SELECT entity_name(?)', (canon,)).fetchone()[0], '爱丽丝')

    def test_review_queue_skip_undo_and_decision(self):
        with self.store.connect() as c:
            c.execute("INSERT INTO identity_pairs VALUES (?,?,0.01,NULL,'anchor_unverified','',?)",
                      (self.a, self.b, '{"relation": "form_of", "subject": "bob", "object": "alice"}'))
        queue = self.store.identity_queue()
        self.assertEqual(queue['total'], 1)
        item = queue['items'][0]
        self.assertEqual({item['a']['id'], item['b']['id']}, {self.a, self.b})
        self.assertEqual(item['hint']['kind'], 'form_of')
        self.assertTrue(item['a']['samples'] and item['b']['samples'])
        skip = self.store.skip_identity_pair(self.a, self.b)
        self.assertEqual(self.store.identity_queue()['total'], 0)
        self.store.undo(skip['id'])
        self.assertEqual(self.store.identity_queue()['total'], 1)
        self.store.decide_identity(a=self.a, b=self.b, kind='different_from', status='accepted', reason='test')
        self.assertEqual(self.store.identity_queue()['total'], 0)

    def test_batch_accept_leaves_contradictions_proposed(self):
        form = self.store.create_entity('Form')['id']
        first = self.store._core(dict(op='propose',a=form,b=self.a,kind='form_of',evidence={'t':1},source='danbooru'))['relation_id']
        second = self.store._core(dict(op='propose',a=form,b=self.b,kind='form_of',evidence={'t':2},source='danbooru'))['relation_id']
        result = self.store._core(dict(op='accept-many',ids=[first,second],reason='test'))
        with self.store.connect() as c:
            status = dict(c.execute('SELECT id,status FROM identity_relations WHERE id IN (?,?)', (first, second)))
        self.assertEqual(result['accepted'], [first])
        self.assertEqual([s['relation_id'] for s in result['skipped']], [second])
        self.assertEqual(status, {first: 'accepted', second: 'proposed'})

    def test_rejecting_a_variant_keeps_the_person(self):
        form = self.store.create_entity('Form')['id']
        self.store.decide_identity(a=form, b=self.a, kind='form_of', status='accepted', reason='test')
        self.store.change(['b'], 'remove_variant', form)
        with self.store.connect() as c:
            self.assertIn(('b', self.a, 'machine'), self.members(c))

    def test_close_same_tier_runner_up_is_flagged_not_withdrawn(self):
        with self.store.connect() as c:
            flagged = c.execute('SELECT uuid,entity,rival FROM ambiguous_claims').fetchall()
        self.assertEqual([tuple(r) for r in flagged], [('b', self.a, self.b)])
        self.assertEqual([r['uuid'] for r in self.store.assets(entity=self.a, status='ambiguous')['items']], ['b'])
        self.assertIn('b', [r['uuid'] for r in self.store.assets(entity=self.a)['items']])
        self.assertEqual(self.store.identity_status()['ambiguous'], 1)


if __name__ == '__main__':
    unittest.main()
