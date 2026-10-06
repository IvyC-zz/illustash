"""边标边学与相似分组：人工结论 → 原型 → 认领；未识别池分组。只用临时库与人造特征。"""
import tempfile
import unittest
from pathlib import Path

import numpy as np

from web import learn
from web.features import Features
from web.store import Store
from tests import terms_seed
from tests.test_web_v2 import assert_memberships_current


def unit(v):
    v = np.asarray(v, np.float32)
    return v / np.linalg.norm(v)


class LearnTest(unittest.TestCase):
    def setUp(self, *, seed=None):
        self.temp = tempfile.TemporaryDirectory()
        root = Path(self.temp.name)
        default_seed = seed is None
        seed = seed or 'test_terms_character_learning_LearningContract_test_learning_changes_and_continuous_old_undo_double_run'
        self.store, attrs = terms_seed.load(seed, root)
        self.alice, self.bob = attrs['alice'], attrs['bob']
        if default_seed:
            if self._testMethodName == 'test_rejected_skin_is_not_a_parent_negative':
                with self.store.connect() as c:
                    c.execute("INSERT INTO entities(id,name,kind,parent_id,variant) VALUES ('summer','','skin',?,'summer')", (self.alice,))
            terms_seed.switch(self.store)
        rng = np.random.default_rng(0)
        A, B = unit([1, 0, 0, 0, 0, 0, 0, 0]), unit([0, 0, 0, 0, 1, 0, 0, 0])
        near = lambda centre, spread: unit(centre + rng.normal(0, spread, 8))
        vectors = {f's{i}': near(A, 0.08) for i in range(6)}                  # alice's confirmed images
        vectors |= {'close': near(A, 0.05), 'edge': unit(A + 0.35 * unit([0, 1, 0, 0, 0, 0, 0, 0])),
                    'far': unit([0, 0, 1, 0, 0, 0, 0, 0])}
        vectors |= {f'u{i}': near(B, 0.05) for i in range(4)}                 # a look-alike group, nobody yet
        vectors |= {'ring': unit([0, 0, 0, 0, 0, 0, 0, 1]), 'hintb': unit([0, 0, 0, 0, 0, 0, 1, 1])}      # placed per test
        vectors |= {'a': unit([0, 0, 0, 1, 0, 0, 0, 0]), 'b': unit([0, 0, 0, 0, 0, 1, 0, 0]), 'c': unit([0, 0, 0, 0, 0, 0, 1, 0])}
        self.features = Features(self.store.source, np.stack(list(vectors.values())), list(vectors))

    def tearDown(self):
        assert_memberships_current(self, self.store)
        self.temp.cleanup()

    def owners(self, uuid):
        return [m['id'] for m in self.store.detail(uuid)['memberships']]

    def learned_snapshot(self):
        with self.store.connect() as c:
            return {table: [tuple(r) for r in c.execute(f'SELECT * FROM {table} ORDER BY 1,2')]
                    for table in ('prototypes', 'proto_claims')}

    def test_shared_oc_is_a_mark_never_learned_and_splits_on_demand(self):
        from web.store import Invalid
        oc = self.store.shared_oc()
        self.assertEqual(self.store.shared_oc()['id'], oc['id'])
        self.assertEqual((oc['name'], oc['kind']), ('原创角色（未细分）', 'oc'))
        self.store.change([f'u{i}' for i in range(4)] + ['far'], 'confirmed', oc['id'])
        learn.learn(self.store, self.features)
        with self.store.connect() as c:
            self.assertEqual(c.execute('SELECT count(*) FROM prototypes WHERE entity=?', (oc['id'],)).fetchone()[0], 0)
            self.assertGreater(c.execute('SELECT count(*) FROM prototypes WHERE entity=?', (self.alice,)).fetchone()[0], 0)
        # Images that look like the OC's members are not claimed by the mark.
        with self.store.connect() as c:
            self.assertEqual(c.execute('SELECT count(*) FROM weak_claims WHERE entity=?', (oc['id'],)).fetchone()[0], 0)
        groups = learn.oc_groups(self.store, self.features, oc['id'])
        self.assertEqual(groups['total'], 5)
        self.assertEqual([sorted(g) for g in groups['groups']], [[f'u{i}' for i in range(4)]])
        # A model learned before an entity became a mark is dropped on the next pass.
        self.store.set_mascot(self.alice, True)
        learn.learn(self.store, self.features)
        with self.store.connect() as c:
            self.assertEqual(c.execute('SELECT count(*) FROM prototypes WHERE entity=?', (self.alice,)).fetchone()[0], 0)
        self.store.set_mascot(self.alice, False)
        with self.assertRaises(Invalid): self.store.set_kind(oc['id'], '')

    def test_undo_relearns_even_when_sqlite_reuses_rowid(self):
        learn.learn(self.store, self.features)
        expected = self.learned_snapshot()
        action = self.store.change(['s0'], 'rejected', self.alice)
        learn.learn(self.store, self.features)
        self.store.undo(action['id'])
        self.assertGreater(learn.learn(self.store, self.features)['relearned'], 0)
        self.assertEqual(self.learned_snapshot(), expected)
        self.assertEqual(learn.learn(self.store, self.features)['relearned'], 0)

    def test_rejected_skin_is_not_a_parent_negative(self):
        learn.learn(self.store, self.features)
        expected = self.learned_snapshot()
        self.store.change(['s0'], 'remove_variant', 'summer')
        learn.learn(self.store, self.features)
        self.assertEqual(self.learned_snapshot(), expected)
        self.assertIn(self.alice, self.owners('close'))

    def test_removed_seed_images_invalidate_learning(self):
        learn.learn(self.store, self.features)
        self.store.import_changes(removed=['s0', 's1', 's2'])
        self.assertGreater(learn.learn(self.store, self.features, reclaim=True)['relearned'], 0)
        self.assertNotIn(self.alice, self.owners('close'))

    def test_reembedded_seed_invalidates_learning_without_new_verdict(self):
        learn.learn(self.store, self.features)
        X = self.features.X.copy()
        X[self.features.row['s0']] = unit([0,0,0,0,0,0,0,1])
        self.features._set(self.features.uuids, X)
        self.assertGreater(learn.learn(self.store, self.features)['relearned'], 0)

    def test_prototypes_claim_close_images_and_respect_removals(self):
        result = learn.learn(self.store, self.features)
        self.assertGreaterEqual(result['relearned'], 1)
        self.assertEqual(self.owners('close'), [self.alice])
        self.assertEqual(self.owners('far'), [])
        membership = self.store.detail('close')['memberships'][0]
        self.assertEqual((membership['status'], membership['evidence']['sources']), ('machine', ['PROTO']))
        # Nothing changed: nothing is refitted.
        self.assertEqual(learn.learn(self.store, self.features)['relearned'], 0)
        # Removing the claimed image makes it a negative: the prototype tightens inside it.
        self.store.change(['close'], 'rejected', self.alice)
        learn.learn(self.store, self.features)
        self.assertEqual(self.owners('close'), [])
        self.assertEqual(self.owners('edge'), [])

    def test_outer_ring_and_patched_middle_tier_are_only_leads(self):
        learn.learn(self.store, self.features)
        with self.store.connect() as c:
            vec, threshold, hint = c.execute('SELECT vec,threshold,hint FROM prototypes WHERE entity=?', (self.alice,)).fetchone()
        centre = np.frombuffer(vec, np.float32)
        # An image halfway between the claim limit and the hint limit: (1 - cos) / 2 = d.
        d = (threshold + hint) / 2
        side = unit(np.eye(8)[7] - centre * centre[7])
        vectors = dict(zip(self.features.uuids, self.features.X))
        vectors['ring'] = unit(centre * (1 - 2 * d) + side * np.sqrt(1 - (1 - 2 * d) ** 2))
        features = Features(self.store.source, np.stack(list(vectors.values())), list(vectors))
        anchors = {'entities': [self.bob], 'vectors': features.matrix(['hintb'])[1], 'lo': np.array([-1.0], np.float32),
                   'hi': np.array([0.01], np.float32)}
        learn.learn(self.store, features, reclaim=True, anchors=anchors)
        for uuid, entity in (('ring', self.alice), ('hintb', self.bob)):
            self.assertNotIn(entity, self.owners(uuid))
            alternatives = self.store.detail(uuid)['alternatives']
            self.assertEqual([(a['id'], a['weak']) for a in alternatives if a['id'] == entity], [(entity, True)])
            self.assertIn(uuid, self.store.ids({'entity': entity, 'status': 'maybe'}))
        self.assertIn(self.alice, [r['id'] for r in self.store.suggest(['ring'])])
        # Removing the image for that character drops the lead.
        self.store.change(['ring'], 'rejected', self.alice)
        self.assertNotIn(self.alice, [a['id'] for a in self.store.detail('ring')['alternatives']])
        self.assertNotIn('ring', self.store.ids({'entity': self.alice, 'status': 'maybe'}))

    def test_too_few_confirmations_learn_nothing(self):
        self.store.change(['s0', 's1', 's2'], 'rejected', self.alice)
        learn.learn(self.store, self.features)
        with self.store.connect() as c:
            self.assertEqual(c.execute('SELECT count(*) FROM prototypes WHERE entity=?', (self.alice,)).fetchone()[0], 0)
        self.assertEqual(self.owners('close'), [])

    def test_unmatched_look_alikes_form_a_group_that_shrinks_when_named(self):
        learn.learn(self.store, self.features)
        self.assertEqual(learn.cluster(self.store, self.features), 1)
        group = self.store.clusters()[0]
        self.assertEqual(group['size'], 4)
        members = sorted(r['uuid'] for r in self.store.assets(pool='cluster', cluster=str(group['cid']))['items'])
        self.assertEqual(members, ['u0', 'u1', 'u2', 'u3'])
        # Name part of the group: those leave it; with fewer than two left it disappears from the list.
        self.store.change(None, 'confirmed', self.bob, filters={'pool': 'cluster', 'cluster': str(group['cid'])}, exclude=['u3'])
        self.assertEqual(self.store.clusters(), [])
        self.assertEqual(self.store.ids({'pool': 'cluster', 'cluster': str(group['cid'])}), ['u3'])

    def test_hair_colour_splits_a_group_and_traits_describe_it(self):
        C = unit([0, 0, 0, 0, 0, 1, 1, 0])
        rng = np.random.default_rng(1)
        extra = {f'h{i}': unit(C + rng.normal(0, 0.03, 8)) for i in range(6)}
        with terms_seed.staged(self.store) as self.store, self.store.connect() as c:
            c.executemany('INSERT INTO assets(uuid,filename) VALUES (?,?)', [(u, u) for u in extra])
            hits = [(f'h{i}', 'blonde_hair' if i < 3 else 'black_hair') for i in range(6)]
            hits += [(f'h{i}', 'halo') for i in range(6)] + [(f'u{i}', 'rabbit_ears') for i in range(4)]
            c.executemany('INSERT INTO general_hits VALUES (?,?)', hits)
            # 题材来自同一份机器输入；启动时交证据包不能将手工投影当成源标签。
            import sqlite3
            with sqlite3.connect(self.store.source) as src:
                src.executemany("INSERT INTO labels VALUES (?,'general',?,1)", hits)
            self.store._refresh_memberships(c, list(extra))
        vectors = dict(zip(self.features.uuids, self.features.X)) | extra
        features = Features(self.store.source, np.stack(list(vectors.values())), list(vectors))
        learn.learn(self.store, features)
        self.assertEqual(learn.cluster(self.store, features), 3)
        groups = {frozenset(self.store.ids({'pool': 'cluster', 'cluster': str(g['cid'])})): g for g in self.store.clusters()}
        self.assertIn(frozenset({'h0', 'h1', 'h2'}), groups)
        self.assertIn(frozenset({'h3', 'h4', 'h5'}), groups)
        blonde, black = groups[frozenset({'h0', 'h1', 'h2'})], groups[frozenset({'h3', 'h4', 'h5'})]
        self.assertIn('blonde hair', blonde['traits'])
        self.assertIn('halo', blonde['traits'])
        self.assertIn('rabbit ears', groups[frozenset({'u0', 'u1', 'u2', 'u3'})]['traits'])
        # Same face, different hair colour: never suggested as one person.
        self.assertNotEqual(blonde['near'], black['cid'])
        self.assertNotEqual(black['near'], blonde['cid'])


if __name__ == '__main__':
    unittest.main()
