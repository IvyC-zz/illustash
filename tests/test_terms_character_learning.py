"""C3 学习验收：固定旧路径状态与返回值，对照公开发布器和直接核心发布器。

全部学习、裁决与撤销在完整核心形态上运行；人工边界输入先恢复旧库，再执行真实启动切换。
"""
import copy
import sqlite3
import unittest
from pathlib import Path
from unittest import mock

from tests import test_learn as fixtures, terms_seed
from tests.terms_c3_snapshots import Checkpoints, deterministic
from tests.test_terms_core import core
from tests.test_terms_characters_contract import COMPAT, PROJECTIONS
from web import learn, terms_core
from web.store import Store

LEARNING = ('prototypes', 'learn_state', 'proto_claims', 'weak_claims')


def snapshot(store, tables):
    with store.connect() as c:
        return {t: sorted((tuple(r) for r in c.execute(f'SELECT * FROM {t}')), key=repr) for t in tables
                if c.execute('SELECT 1 FROM sqlite_master WHERE name=?', (t,)).fetchone()}


def dump(store):
    with sqlite3.connect(store.path) as c:
        return '\n'.join(c.iterdump())


def rebuilt_snapshot(store, tables):
    cloned = copy.copy(store)
    cloned.path = Path(str(store.path) + '.rebuild')
    with sqlite3.connect(store.path) as source, sqlite3.connect(cloned.path) as target:
        source.backup(target)
    try:
        terms_core.run('reproject-characters', '--db', cloned.path)
        return snapshot(cloned, tables)
    finally:
        cloned.path.unlink()


class LearningContract(unittest.TestCase):
    hybrid = False

    @classmethod
    def setUpClass(cls):
        if core() is None:
            raise unittest.SkipTest('no terms core binary and no cargo')

    def setUp(self):
        deterministic(self)
        self.checkpoints = Checkpoints(self)
        name = terms_seed.key(self)
        fixtures.LearnTest.setUp(self, seed=name)
        # 此用例仍验收发布器首次登记图片。启动时尚未出现 close，切换后只布置外围 assets 输入。
        incoming = None
        if self._testMethodName == 'test_first_prototype_claim_registers_the_image_and_mirrors_all_owners':
            with self.store.connect() as c:
                incoming = tuple(c.execute("SELECT * FROM assets WHERE uuid='close'").fetchone())
                c.execute("DELETE FROM assets WHERE uuid='close'")
        terms_seed.switch(self.store)
        if incoming is not None:
            with self.store.connect() as c:
                c.execute('INSERT INTO assets VALUES (' + ','.join('?' for _ in incoming) + ')', incoming)
        terms_seed.resume_ids(self, name)
        self.api = copy.copy(self.store)
        self.api.path = Path(self.temp.name) / 'api.sqlite'
        with sqlite3.connect(self.store.path) as src, sqlite3.connect(self.api.path) as dst:
            src.backup(dst)
            dst.execute('PRAGMA journal_mode=WAL')
        self.publisher = terms_core.LearningPublisher(self.store.path)

        # 把原来在 both 之外读取旧状态、执行旧发布器的断言也先固化，不能在删除旧写路径时漏掉。
        self.extra = Checkpoints(self, suffix='_extra')
        original_snapshot, original_learn = snapshot, learn.learn
        def observed_snapshot(store, tables):
            result = original_snapshot(store, tables)
            if store is self.api:
                self.extra.capture(store, tables)
            return result
        def observed_learn(store, *args, **kwargs):
            result = original_learn(store, *args, **kwargs)
            if store is self.api:
                self.extra.capture_value('学习返回值', {k: result[k] for k in ('relearned', 'changed')})
            return result
        patch = mock.patch(__name__ + '.snapshot', side_effect=observed_snapshot)
        patch.start()
        self.addCleanup(patch.stop)
        patch = mock.patch.object(learn, 'learn', side_effect=observed_learn)
        patch.start()
        self.addCleanup(patch.stop)

    def tearDown(self):
        self.temp.cleanup()

    def both(self, **kwargs):
        with mock.patch.object(learn.time, 'strftime', return_value='2026-09-29 00:00:00'):
            old = learn.learn(self.api, self.features, **kwargs)
            new = learn.learn(self.store, self.features, publisher=self.publisher, **kwargs)
        self.checkpoints.capture(self.api, LEARNING + COMPAT + PROJECTIONS)
        self.assertEqual({k: new[k] for k in ('relearned', 'changed')}, {k: old[k] for k in ('relearned', 'changed')})
        self.assertEqual(snapshot(self.store, LEARNING + COMPAT + PROJECTIONS),
                         snapshot(self.api, LEARNING + COMPAT + PROJECTIONS))
        # Rebuild in a copy: validation must never repair the database being compared.
        self.assertEqual(rebuilt_snapshot(self.store, PROJECTIONS), snapshot(self.store, PROJECTIONS))
        return new

    def batch(self):
        capture = mock.Mock()
        capture.snapshot.return_value = self.publisher.snapshot()
        capture.publish.side_effect = lambda value: value
        return learn.learn(self.store, self.features, publisher=capture, force=True, reclaim=True)

    def test_learning_changes_and_continuous_old_undo_double_run(self):
        self.both()
        self.both()  # no dirty work: neither side publishes
        steps = []
        for uuids, status, entity in [(['s0'], 'rejected', self.alice),
                                      (['u0', 'u1'], 'confirmed', self.bob),
                                      (['close'], 'rejected', self.alice)]:
            pair = [s.change(uuids, status, entity)['id'] for s in (self.store, self.api)]
            steps.append(pair)
            self.both()
        for pair in reversed(steps):
            for s, action in zip((self.store, self.api), pair):
                s.undo(action)  # 固定旧历史文案、状态与连续撤销结果仍逐项核对。
            self.both()
        for s in (self.store, self.api):
            with s.connect() as c:
                action = c.execute('SELECT max(id) FROM actions WHERE undone=0').fetchone()[0]
            s.undo(action)
        self.both()

    def test_first_prototype_claim_registers_the_image_and_mirrors_all_owners(self):
        with self.store.connect() as c:
            self.assertIsNone(c.execute("SELECT id FROM term_assets WHERE namespace='library' AND key='close'").fetchone())
        self.both()
        with self.store.connect() as c:
            expected = set(map(tuple, c.execute('SELECT uuid,entity,status,score,sources FROM memberships UNION SELECT uuid,entity,status,score,sources FROM tag_memberships')))
            actual = set(map(tuple, c.execute("""SELECT a.key,t.key,p.state,p.score,p.sources FROM term_projection p
                JOIN term_assets a ON a.id=p.asset_id JOIN terms t ON t.id=p.term_id JOIN term_tree r ON r.term_id=t.id""")))
        self.assertEqual(actual, expected)
        self.assertTrue(any(row[0] == 'close' for row in actual))
        self.assertEqual(snapshot(self.store, ('term_projection',)), rebuilt_snapshot(self.store, ('term_projection',)))

    def test_more_than_ten_changed_images_uses_the_same_sql_scope(self):
        with self.store.connect() as c:
            uuids = [r[0] for r in c.execute('SELECT uuid FROM assets')]
        self.assertGreater(len(uuids), 10)
        claims = {(u, self.alice): .99 for u in uuids}
        with mock.patch.object(learn, '_scan', side_effect=[claims, {}, claims, {}]):
            self.assertEqual(self.both(force=True)['changed'], len(uuids))

    def test_successful_empty_batch_clears_old_claims(self):
        self.both()
        for s in (self.api, self.store):
            s.change([f's{i}' for i in range(6)], 'rejected', self.alice)
        self.both()
        self.assertEqual(snapshot(self.store, ('proto_claims',))['proto_claims'], [])

    def test_human_change_during_training_rejects_and_next_run_retries(self):
        self.both()
        original = learn.prototypes_for
        changed = []
        def during_training(seeds, negatives):
            if not changed:
                changed.append(self.store.change(['s0'], 'rejected', self.alice))
            return original(seeds, negatives)
        before = snapshot(self.store, LEARNING)
        with mock.patch.object(learn, 'prototypes_for', side_effect=during_training):
            result = learn.learn(self.store, self.features, force=True, publisher=self.publisher)
        self.assertEqual(result, {'status': 'stale', 'relearned': 0, 'changed': 0})
        self.assertEqual(snapshot(self.store, LEARNING), before)
        self.assertNotEqual(snapshot(self.store, ('assertions',)), snapshot(self.api, ('assertions',)))
        self.api.change(['s0'], 'rejected', self.alice)
        self.assertEqual(self.both()['status'], 'published')

    def test_undo_to_same_verdict_still_invalidates_inflight_batch(self):
        batch = self.batch()
        action = self.store.change(['s0'], 'rejected', self.alice)
        self.store.undo(action['id'])
        before = dump(self.store)
        self.assertEqual(self.publisher.publish(batch)['status'], 'stale')
        self.assertEqual(dump(self.store), before)

    def test_two_workers_cannot_overwrite_each_other(self):
        first = self.batch()
        second = copy.deepcopy(first)
        self.assertEqual(self.publisher.publish(first)['status'], 'published')
        before = dump(self.store)
        self.assertEqual(self.publisher.publish(second)['status'], 'stale')
        self.assertEqual(dump(self.store), before)

    def test_failed_input_and_mid_transaction_failure_leave_nothing(self):
        batch = self.batch()
        invalid = copy.deepcopy(batch)
        invalid['prototypes'][0] = list(invalid['prototypes'][0])
        invalid['prototypes'][0][2] = 'not hex'
        before = dump(self.store)
        with self.assertRaises(terms_core.Failed):
            self.publisher.publish(invalid)
        self.assertEqual(dump(self.store), before)
        # Failure after all four learning tables were replaced, while refreshing ownership.
        with self.store.connect() as c:
            c.execute("CREATE TRIGGER fail_learning BEFORE INSERT ON family_evidence BEGIN SELECT RAISE(ABORT,'injected'); END")
        before = dump(self.store)
        with self.assertRaisesRegex(RuntimeError, 'injected'):
            self.publisher.publish(batch)
        self.assertEqual(dump(self.store), before)
        with self.store.connect() as c:
            self.assertIsNone(c.execute("SELECT 1 FROM meta WHERE key='terms_writer'").fetchone())
            c.execute('DROP TRIGGER fail_learning')
        self.assertEqual(self.publisher.publish(batch)['status'], 'published')

    def test_asset_and_identity_mark_changes_invalidate(self):
        for change in (lambda: self.store.set_mascot(self.alice, True),
                       lambda: self.store.import_changes(removed=['s0'])):
            batch = self.batch()
            change()
            before = dump(self.store)
            self.assertEqual(self.publisher.publish(batch)['status'], 'stale')
            self.assertEqual(dump(self.store), before)

    def test_sub_epsilon_double_run_is_exact_and_ownership_rebuild_is_bounded(self):
        from tests.terms_projection_compare import assert_rebuilt_equal
        self.both()
        with self.api.connect() as c:
            claims = {(u, e): score for u, e, score in c.execute('SELECT * FROM proto_claims')}
            weak = {(u, e): score for u, e, source, score in c.execute('SELECT * FROM weak_claims') if source == 'PROTO'}
        key = ('close', self.alice)
        self.assertIn(key, claims)
        claims[key] += 5e-7
        with mock.patch.object(learn, '_scan', side_effect=[claims, weak, claims, weak]), \
             mock.patch.object(learn.time, 'strftime', return_value='2026-09-29 00:00:00'):
            old = learn.learn(self.api, self.features, force=True)
            new = learn.learn(self.store, self.features, force=True, publisher=self.publisher)
        self.assertEqual(old['changed'], 0)
        self.assertEqual(new['changed'], 0)
        # No exception on the reference comparison: every stored column is identical.
        self.assertEqual(snapshot(self.store, LEARNING + COMPAT + PROJECTIONS),
                         snapshot(self.api, LEARNING + COMPAT + PROJECTIONS))
        def ownership(store):
            with store.connect() as c:
                out = {t: [dict(r) for r in c.execute(f'SELECT * FROM {t} ORDER BY uuid,entity')]
                       for t in ('memberships', 'tag_memberships')}
                out['term_projection'] = [dict(r) for r in c.execute("""SELECT a.key AS uuid,t.key AS entity,p.*
                    FROM term_projection p JOIN term_assets a ON a.id=p.asset_id JOIN terms t ON t.id=p.term_id
                    ORDER BY a.key,t.key""")]
                return out
        before = ownership(self.store)
        clone = copy.copy(self.store)
        clone.path = Path(self.temp.name) / 'epsilon-rebuild.sqlite'
        with sqlite3.connect(self.store.path) as src, sqlite3.connect(clone.path) as dst:
            src.backup(dst)
        terms_core.run('reproject-characters', '--db', clone.path)
        after = ownership(clone)
        self.assertNotEqual(before, after)
        assert_rebuilt_equal(self, before, after, command='publish-learning', suppressed_pairs={key})
        with self.assertRaises(AssertionError):
            assert_rebuilt_equal(self, before, after, command='legacy-double-run', suppressed_pairs={key})
        # family_evidence.ccip also changes when rebuilt. It is outside the approved
        # ownership exception and must continue to be reported, never normalized away.
        with self.assertRaises(AssertionError):
            assert_rebuilt_equal(self, snapshot(self.store, ('family_evidence',)),
                                 snapshot(clone, ('family_evidence',)), command='publish-learning', suppressed_pairs={key})

    def test_sub_epsilon_rank_flip_is_reported_not_tolerated(self):
        from tests.terms_projection_compare import assert_rebuilt_equal
        # 原来的人工排名边界已在旧写路径尚在时录成输入，恢复后真实切换，不解除核心保护。
        root = Path(self.temp.name)
        name = 'mid_claims_' + terms_seed.key(self)
        self.store, _ = terms_seed.load(name, root, db=root / 'rank-core.sqlite')
        # 原人工 general_hits 输入来自 solo 证据；完整核心由源 WD 证据投影，补齐相同的源输入。
        with sqlite3.connect(self.store.source) as c:
            c.execute("INSERT INTO labels(uuid,value,conf,dim) VALUES ('close','solo',1,'general')")
        terms_seed.switch(self.store)
        terms_seed.resume_ids(self, name)
        self.api = copy.copy(self.store)
        self.api.path = root / 'rank-api.sqlite'
        with sqlite3.connect(self.store.path) as src, sqlite3.connect(self.api.path) as dst:
            src.backup(dst)
            dst.execute('PRAGMA journal_mode=WAL')
        self.publisher = terms_core.LearningPublisher(self.store.path)
        terms_core.run('reproject-characters', '--db', self.store.path)
        claims = {('close', self.alice): .9500003, ('close', self.bob): .95}
        with mock.patch.object(learn, '_scan', side_effect=[claims, {}, claims, {}]), \
             mock.patch.object(learn.time, 'strftime', return_value='2026-09-29 00:00:00'):
            old = learn.learn(self.api, self.features, force=True)
            new = learn.learn(self.store, self.features, force=True, publisher=self.publisher)
        self.assertEqual(old['changed'], 0)
        self.assertEqual(new['changed'], 0)
        self.assertEqual(snapshot(self.store, LEARNING + COMPAT + PROJECTIONS),
                         snapshot(self.api, LEARNING + COMPAT + PROJECTIONS))
        with self.store.connect() as c:
            before = {'memberships': [dict(r) for r in c.execute("SELECT * FROM memberships WHERE uuid='close'")]}
        self.assertEqual([r['entity'] for r in before['memberships']], [self.bob])
        terms_core.run('reproject-characters', '--db', self.store.path)
        with self.store.connect() as c:
            after = {'memberships': [dict(r) for r in c.execute("SELECT * FROM memberships WHERE uuid='close'")]}
        self.assertEqual([r['entity'] for r in after['memberships']], [self.alice])
        with self.assertRaises(AssertionError):
            assert_rebuilt_equal(self, before, after, command='publish-learning',
                                 suppressed_pairs={('close', self.alice), ('close', self.bob)})

    def test_score_change_epsilon_matches_python(self):
        self.both()
        batch = self.batch()
        self.assertTrue(batch['proto_claims'])
        row = list(batch['proto_claims'][0])
        batch['proto_claims'][0] = row
        row[2] += 0.0000005
        self.assertEqual(self.publisher.publish(batch)['changed'], 0)
        # Match _claim: sub-epsilon claims are stored without refreshing ownership.
        # A later >epsilon difference is included in the changed image set.
        batch['token'] = self.publisher.snapshot()
        row[2] += 0.000002
        self.assertEqual(self.publisher.publish(batch)['changed'], 1)


class HybridLearningContract(LearningContract):
    hybrid = True

    def test_creation_refreshes_existing_sub_epsilon_claims_exactly(self):
        self.test_sub_epsilon_double_run_is_exact_and_ownership_rebuild_is_bounded()
        made = self.api.create_entity('学习后新人物')
        terms_core.character_command(self.store.path, {'op': 'create_entity', 'id': made['id'],
                                                       'name': '学习后新人物', 'lang': 'zh', 'kind': '', 'artist': ''})
        self.assertEqual(snapshot(self.store, LEARNING + COMPAT + PROJECTIONS),
                         snapshot(self.api, LEARNING + COMPAT + PROJECTIONS))
        self.assertEqual(rebuilt_snapshot(self.store, PROJECTIONS), snapshot(self.store, PROJECTIONS))

    def test_profile_undo_refreshes_sub_epsilon_claims_exactly(self):
        self.test_sub_epsilon_double_run_is_exact_and_ownership_rebuild_is_bounded()
        self.api.set_profile(self.alice, name='撤销前名字')
        with self.api.connect() as c:
            step = c.execute('SELECT max(id) FROM actions WHERE undone=0').fetchone()[0]
        terms_core.character_command(self.store.path, {'op': 'set_profile', 'entity': self.alice,
                                                       'name': '撤销前名字', 'name_lang': 'zh'})
        self.api.undo(step)
        terms_core.character_command(self.store.path, {'op': 'undo', 'action': step})
        self.assertEqual(snapshot(self.store, LEARNING + COMPAT + PROJECTIONS),
                         snapshot(self.api, LEARNING + COMPAT + PROJECTIONS))
        self.assertEqual(rebuilt_snapshot(self.store, PROJECTIONS), snapshot(self.store, PROJECTIONS))



class LearnerRetry(unittest.TestCase):
    def test_success_keeps_the_existing_public_idle_state(self):
        worker = learn.Learner(mock.Mock(), mock.Mock(), publisher=mock.Mock())
        worker.anchors = {}
        class Stop(BaseException):
            pass
        with mock.patch.object(worker.wake, 'wait', return_value=False), \
             mock.patch.object(learn, 'learn', return_value={'status': 'published', 'changed': 2, 'relearned': 1}), \
             mock.patch.object(learn.time, 'sleep', side_effect=Stop):
            with self.assertRaises(Stop):
                worker.run()
        self.assertEqual(worker.state['status'], 'idle')
        self.assertEqual((worker.state['changed'], worker.state['relearned']), (2, 1))

    def test_rejection_keeps_regroup_pending_and_skips_cluster(self):
        worker = learn.Learner(mock.Mock(), mock.Mock(), publisher=mock.Mock())
        worker.anchors = {}
        worker.regroup.set()
        class Stop(BaseException):
            pass
        with mock.patch.object(worker.wake, 'wait', return_value=False), \
             mock.patch.object(learn, 'learn', return_value={'status': 'stale', 'changed': 0, 'relearned': 0}), \
             mock.patch.object(learn, 'cluster') as cluster, \
             mock.patch.object(learn.time, 'sleep', side_effect=Stop):
            with self.assertRaises(Stop):
                worker.run()
        cluster.assert_not_called()
        self.assertTrue(worker.regroup.is_set())
        self.assertTrue(worker.wake.is_set())
        self.assertEqual(worker.state, {'status': 'idle'})


class OriginalSql(unittest.TestCase):
    def test_checked_in_sql_is_the_python_original(self):
        class Capture:
            def __init__(self, hybrid=False):
                self.hybrid, self.statements = hybrid, []
            def execute(self, sql, args=()):
                if sql != "SELECT value FROM meta WHERE key='identity_policy'":
                    self.statements.append((sql, args))
                return self
            def fetchone(self):
                return ('hybrid',) if self.hybrid else None
        sql_dir = terms_core.ROOT / 'crates/core/src/character_sql'
        for hybrid in (False, True):
            c = Capture(hybrid)
            Store._refresh_evidence(c)
            name = 'evidence_hybrid' if hybrid else 'evidence_default'
            self.assertEqual((sql_dir / f'{name}.sql').read_text().rstrip(), c.statements[-1][0].rstrip())
        c = Capture()
        Store._refresh_memberships(c)
        Store._refresh_single(c)
        for table in ('memberships', 'tag_memberships', 'single_assets', 'lead_assets'):
            expected = next(sql for sql, args in c.statements if sql.startswith(f'INSERT INTO {table} '))
            self.assertEqual((sql_dir / f'{table}.sql').read_text().rstrip(), expected.rstrip())
        expected = next(sql for sql, args in c.statements if sql.startswith('UPDATE duplicates'))
        self.assertEqual((sql_dir / 'fold.sql').read_text().rstrip(), expected.rstrip())


class ProjectionTolerance(unittest.TestCase):
    def rows(self, score):
        with sqlite3.connect(':memory:') as c:
            skey, = c.execute("SELECT printf('%d%d%d%020.12f%d',0,0,0,?,1)", (score,)).fetchone()
        return {'memberships': [{'uuid': 'close', 'entity': 'alice', 'status': 'machine', 'origin': 'machine',
                                 'sources': 'PROTO', 'score': score, 'source_count': 1, 'ccip_only': 1, 'skey': skey}]}

    def compare(self, a, b, **kwargs):
        from tests.terms_projection_compare import assert_rebuilt_equal
        assert_rebuilt_equal(self, a, b, command=kwargs.pop('command', 'publish-learning'),
                             suppressed_pairs=kwargs.pop('suppressed_pairs', {('close', 'alice')}), **kwargs)

    def test_only_the_approved_score_and_formatted_skey_difference_pass(self):
        a, b = self.rows(0.9956405460834503), self.rows(0.9956410460834503)
        self.assertEqual(a['memberships'][0]['skey'], '0000000000.9956405460831')
        self.assertEqual(b['memberships'][0]['skey'], '0000000000.9956410460831')
        self.compare(a, b)
        for table in ('tag_memberships', 'term_projection'):
            x, y = {table: copy.deepcopy(a['memberships'])}, {table: copy.deepcopy(b['memberships'])}
            if table == 'term_projection':
                for rows in (x[table], y[table]):
                    for row in rows:
                        row['state'] = row.pop('status')
                        for col in ('skey', 'source_count', 'ccip_only', 'origin'):
                            del row[col]
            self.compare(x, y)

    def test_out_of_scope_changes_fail(self):
        a, b = self.rows(0.9956405460834503), self.rows(0.9956410460834503)
        mutations = {
            'too-large': lambda r: r.update(score=a['memberships'][0]['score'] + 1.1e-6),
            'uuid': lambda r: r.update(uuid='different-image'),
            'entity': lambda r: r.update(entity='bob'),
            'status': lambda r: r.update(status='confirmed'),
            'origin': lambda r: r.update(origin='review'),
            'sources': lambda r: r.update(sources='CCIP'),
            'source_count': lambda r: r.update(source_count=2),
            'ccip_only': lambda r: r.update(ccip_only=0),
            'invalid-skey': lambda r: r.update(skey='wrong'),
            'unrelated-skey-bit': lambda r: r.update(skey=r['skey'][:-1]+'0'),
            'nan': lambda r: r.update(score=float('nan')),
            'null': lambda r: r.update(score=None),
        }
        for name, mutate in mutations.items():
            with self.subTest(name=name):
                bad = copy.deepcopy(b)
                mutate(bad['memberships'][0])
                with self.assertRaises(AssertionError):
                    self.compare(a, bad)
        for bad in ({'memberships': []}, {'memberships': b['memberships'] * 2}):
            with self.assertRaises(AssertionError):
                self.compare(a, bad)
        with self.assertRaises(AssertionError):
            self.compare(a, b, suppressed_pairs=set())
        for command in ('undo', 'verdict', 'legacy-double-run'):
            with self.subTest(command=command), self.assertRaises(AssertionError):
                self.compare(a, b, command=command)
        # The exception does not silently spread to evidence components/ranks.
        with self.assertRaises(AssertionError):
            self.compare({'family_evidence': [{'uuid': 'close', 'entity': 'alice', 'ccip': .9956405}]},
                         {'family_evidence': [{'uuid': 'close', 'entity': 'alice', 'ccip': .9956410}]})
