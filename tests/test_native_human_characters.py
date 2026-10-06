"""#35 native character HTTP acceptance: synthetic packages, raw bytes and typed three-DB snapshots."""
from contextlib import closing
import http.client
import json
import sqlite3
import unittest

from tests import test_native_human_labels as labels
from tests import test_native_human_curation as curation
from tests import test_native_human_identity as identity
from tests.test_native_human_labels import states
from tools.verify_read_http import fetch
from web import effects, terms_core


class NativeHumanCharactersTest(unittest.TestCase):
    setUpClass = classmethod(labels.NativeHumanLabelsTest.setUpClass.__func__)
    setUp = labels.NativeHumanLabelsTest.setUp
    clone = labels.NativeHumanLabelsTest.clone
    start = labels.NativeHumanLabelsTest.start
    stop = labels.NativeHumanLabelsTest.stop
    seed_both = labels.NativeHumanLabelsTest.seed_both
    undo_latest = labels.NativeHumanLabelsTest.undo_latest
    pair = curation.NativeHumanCurationTest.pair

    def test_change_all_actions_multi_copy_filter_and_variants(self):
        skin = 'synthetic-skin'
        def variant(store):
            with store.connect() as c:
                c.execute("INSERT OR REPLACE INTO meta VALUES ('terms_writer','1')")
                c.execute("INSERT INTO entities(id,name,kind,parent_id,variant) VALUES (?,'Synthetic summer','skin',?,'summer')", (skin, self.alice))
                c.execute("DELETE FROM meta WHERE key='terms_writer'")
            store.set_profile(self.alice, name='Alice')
        self.seed_both(variant)
        calls = [({'ids': ['a', 'a'], 'action': 'confirmed', 'entity': self.alice}),
                 ({'ids': ['c'], 'action': 'confirmed', 'entities': [self.alice, self.bob, self.alice]}),
                 ({'ids': ['c'], 'action': 'add_variant', 'entity': skin}),
                 ({'ids': ['c'], 'action': 'remove_variant', 'entity': skin}),
                 ({'ids': ['a'], 'action': 'rejected', 'entity': self.alice}),
                 ({'ids': ['a'], 'action': 'resume'}),
                 ({'filter': {}, 'exclude': ['c'], 'action': 'unknown'}),
                 ({'ids': ['c'] * 501, 'confirm': True, 'action': 'confirmed', 'entity': self.alice})]
        for body in calls:
            self.pair('/api/change', body)
        for _ in calls:
            self.undo_latest()

    def test_answer_yes_no_later_switch_and_kinds(self):
        for kind in ('seed', 'ambiguous', 'sample', 'audit', 'hint'):
            self.pair('/api/ask/answer', {'entity': self.alice, 'yes': ['a'], 'no': ['c'], 'kind': kind})
            self.undo_latest()
        self.pair('/api/ask/answer', {'entity': self.alice, 'no': ['a'], 'switch': self.bob, 'later': ['c']})
        self.undo_latest()
        prior = self.store.overview()['undo']
        result = self.pair('/api/ask/answer', {'entity': self.alice, 'later': ['missing', 'c'], 'yes': False, 'no': ''})
        self.assertEqual(result, {'undo': None})
        self.assertEqual(self.store.overview()['undo'], prior)

    def test_audit_correct_reassign_sweep_and_mixed_latest_undo(self):
        def audit(store):
            with store.connect() as c:
                c.execute('DELETE FROM audit_sample')
                c.executemany("INSERT INTO audit_sample(uuid,entity,stratum,solo) VALUES (?,?,'synthetic',1)", [('a', self.alice), ('c', self.bob)])
        self.seed_both(audit)
        self.pair('/api/audit/answer', {'yes': ['a'], 'no': ['c']})
        self.pair('/api/reassign', {'ids': ['a'], 'old': self.alice, 'new': self.bob})
        self.pair('/api/reassign', {'filter': {'entity': self.bob}, 'exclude': ['c'], 'old': self.bob, 'new': [self.alice]})
        self.pair('/api/sweep', {'entity': self.alice})
        self.pair('/api/sweep', {'entity': self.alice})  # empty machine set still records one action
        self.pair('/api/tag', {'ids': ['c'], 'tag': '第二标签'})
        self.pair('/api/artist/verdict', {'ids': ['b'], 'artist': self.artist, 'verdict': 'yes'})
        self.pair('/api/curate', {'ids': ['c'], 'action': 'exclude'})
        self.pair('/api/change', {'ids': ['c'], 'action': 'resume'})
        for _ in range(9):
            self.undo_latest()

    def test_merged_entity_and_multi_replacement_preserve_other_memberships(self):
        def identities(store):
            self.third = store.create_entity('Synthetic third')['id']
            self.merged = store.create_entity('Synthetic merged')['id']
            store.merge(self.merged, self.alice)
        self.seed_both(identities)
        self.pair('/api/change', {'ids': ['c'], 'action': 'confirmed', 'entity': self.merged})
        self.pair('/api/reassign', {'ids': ['c'], 'old': self.merged, 'new': [self.bob, self.third, self.bob]})
        self.pair('/api/reassign', {'ids': ['c'], 'old': self.bob, 'new': self.alice})
        with self.store.connect() as c:
            self.assertIsNotNone(c.execute('SELECT 1 FROM memberships WHERE uuid=? AND entity=?', ('c', self.third)).fetchone())
        for _ in range(3): self.undo_latest()

    def test_hybrid_verdict_scope_and_undo(self):
        def hybrid(store):
            with store.connect() as c:
                c.execute("INSERT OR REPLACE INTO meta VALUES ('identity_policy','hybrid')")
            terms_core.character_command(store.path, {'op': 'rebuild'})
        self.seed_both(hybrid)
        self.pair('/api/change', {'ids': ['a'], 'action': 'confirmed', 'entity': self.alice})
        self.pair('/api/ask/answer', {'entity': self.alice, 'no': ['a'], 'switch': self.bob})
        self.pair('/api/reassign', {'ids': ['a'], 'old': self.bob, 'new': self.alice})
        self.pair('/api/sweep', {'entity': self.bob})
        for _ in range(4): self.undo_latest()

    def test_role_and_identity_commands_share_recent_undo(self):
        def hybrid(store):
            self.third = store.create_entity('Synthetic third')['id']
            with store.connect() as c:
                c.execute("INSERT OR REPLACE INTO meta VALUES ('identity_policy','hybrid')")
            terms_core.character_command(store.path, {'op': 'rebuild'})
        self.seed_both(hybrid)
        pair = lambda url, body: identity.NativeHumanIdentityTest.pair(self, url, body)
        pair('/api/change', {'ids': ['c'], 'action': 'confirmed', 'entity': self.alice})
        pair('/api/identity/decision', {'a': self.alice, 'b': self.third, 'kind': 'duplicate_of',
                                      'status': 'rejected', 'reason': 'Synthetic refusal'})
        pair('/api/ask/answer', {'entity': self.alice, 'no': ['c'], 'switch': self.bob})
        pair('/api/change', {'ids': ['c'], 'action': 'resume'})
        pair('/api/merge', {'source': self.third, 'target': self.bob})
        for _ in range(5): self.undo_latest()

    def test_unused_extended_json_and_truthy_non_string_numbers(self):
        self.pair('/api/change', {'ids': ['c'], 'action': 'confirmed', 'entity': self.alice,
                                 'unused': [10 ** 120, float('nan'), float('inf')]})
        self.undo_latest()
        self.pair('/api/ask/answer', {'entity': self.alice, 'later': ['c'], 'yes': {}, 'no': 0.0})
        for invalid in (10 ** 120, float('nan'), float('inf')):
            self.pair('/api/ask/answer', {'entity': self.alice, 'later': ['c'], 'yes': invalid}, expected=400)

    def test_validation_errors_have_no_partial_writes(self):
        cases = [('/api/change', b) for b in (
            {}, {'action': 'bad'}, {'action': 'confirmed', 'entities': []},
            {'action': 'confirmed', 'entity': self.alice, 'entities': [self.bob]},
            {'action': 'unknown', 'ids': []}, {'action': 'unknown', 'ids': [1]},
            {'action': 'unknown', 'ids': ['missing']}, {'action': 'unknown', 'filter': []},
            {'action': 'unknown', 'filter': {'unknown': ''}},
            {'action': 'unknown', 'ids': ['a'], 'filter': {}},
            {'action': 'unknown', 'ids': ['a'], 'exclude': ['c']},
            {'action': 'unknown', 'ids': ['a'], 'exclude': [1]},
            {'action': 'unknown', 'ids': ['a'] * 501, 'confirm': 1},
            {'action': 'add_variant', 'entity': self.alice, 'ids': ['a']})]
        cases += [('/api/ask/answer', b) for b in (
            {}, {'entity': self.alice, 'yes': [1]}, {'entity': self.alice, 'yes': ['a'], 'no': ['a']},
            {'entity': self.alice, 'yes': ['a'], 'switch': self.bob},
            {'entity': self.alice, 'yes': ['a'], 'kind': None},
            {'entity': self.alice, 'yes': ['a'], 'kind': 'bad'},
            {'entity': self.alice, 'yes': ['missing']},
            {'entity': self.alice, 'no': ['a'], 'switch': self.alice})]
        cases += [('/api/audit/answer', {}), ('/api/audit/answer', {'yes': ['missing']}),
                  ('/api/reassign', {'ids': ['a'], 'old': self.alice, 'new': []}),
                  ('/api/reassign', {'ids': ['a'], 'old': self.alice, 'new': ['missing']}),
                  ('/api/sweep', {'entity': 'missing'})]
        for url, body in cases:
            with self.subTest(url=url, body=body):
                self.pair(url, body, expected=400)

    def test_transaction_faults_rollback(self):
        def audit(store):
            with store.connect() as c:
                c.execute("INSERT OR REPLACE INTO audit_sample(uuid,entity,stratum,solo) VALUES (?,?,'synthetic',1)", ('a', self.alice))
        self.seed_both(audit)
        commands = [('/api/change', {'ids': ['a'], 'action': 'confirmed', 'entity': self.alice}),
                    ('/api/ask/answer', {'entity': self.alice, 'yes': ['a']}),
                    ('/api/reassign', {'ids': ['a'], 'old': self.alice, 'new': self.bob}),
                    ('/api/sweep', {'entity': self.alice})]
        commands.append(('/api/audit/answer', {'yes': ['a']}))
        for table in ('actions', 'term_verdicts', 'backend_effects', 'assertions'):
            for lib in (self.left, self.right):
                with closing(sqlite3.connect(lib.db)) as c:
                    c.execute(f"CREATE TRIGGER fail_character BEFORE INSERT ON {table} BEGIN SELECT RAISE(ABORT,'synthetic failure'); END"); c.commit()
            for url, body in commands:
                self.pair(url, body, expected=500)
            for lib in (self.left, self.right):
                with closing(sqlite3.connect(lib.db)) as c:
                    c.execute('DROP TRIGGER fail_character'); c.commit()

    def test_postcommit_event_failure_survives_and_recovers(self):
        for lib in (self.left, self.right):
            with closing(sqlite3.connect(lib.db)) as c:
                c.execute("CREATE TRIGGER fail_ack BEFORE UPDATE ON backend_effects BEGIN SELECT RAISE(ABORT,'synthetic ack failure'); END"); c.commit()
        responses = [fetch(base, {'method': 'POST', 'url': '/api/change',
                      'body': json.dumps({'ids': ['c'], 'action': 'confirmed', 'entity': self.alice}),
                      'headers': {'Content-Type': 'application/json', 'X-CSRF-Token': self.token}}, self.origin)
                     for base in (self.direct, self.origin)]
        self.assertEqual([r['status'] for r in responses], [500, 500])
        self.assertEqual(states(self.left), states(self.right))
        with self.store.connect() as c:
            self.assertEqual(c.execute("SELECT status FROM backend_effects ORDER BY id DESC LIMIT 1").fetchone()[0], 'pending')
        for lib in (self.left, self.right):
            with closing(sqlite3.connect(lib.db)) as c:
                c.execute('DROP TRIGGER fail_ack'); c.commit()
        self.stop(self.process)
        effects.drain(self.store, self.reference._after_operation)
        self.process = self.start(self.right)
        self.token = json.loads(fetch(self.origin, {'url': '/api/bootstrap'}, self.origin)['body'])['token']
        self.reference.token = self.token
        self.assertEqual(states(self.left), states(self.right))
        self.undo_latest()

    def test_security_and_inflight_request_switch_isolation(self):
        commands = [('/api/change', {'ids': ['a'], 'action': 'unknown'}),
                    ('/api/ask/answer', {'entity': self.alice, 'later': ['a']}),
                    ('/api/audit/answer', {'yes': ['a']}),
                    ('/api/reassign', {'ids': ['a'], 'old': self.alice, 'new': self.bob}),
                    ('/api/sweep', {'entity': self.alice})]
        for url, body in commands:
            self.pair(url, body, expected=403, headers={'X-CSRF-Token': 'bad'})
        newer = self.clone(self.right, 'switched')
        for url, body in commands:
            token = self.token
            before_old, before_new = states(self.right), states(newer)
            encoded = json.dumps(body).encode()
            conn = http.client.HTTPConnection('127.0.0.1', self.port, timeout=5)
            conn.putrequest('POST', url, skip_host=True)
            for key, value in {'Host': self.origin.split('://')[1], 'Origin': self.origin,
                               'Content-Type': 'application/json', 'X-CSRF-Token': token,
                               'Content-Length': str(len(encoded))}.items(): conn.putheader(key, value)
            conn.endheaders(); conn.send(encoded[:1])
            switched = fetch(self.origin, {'method': 'POST', 'url': '/api/libraries/open',
                         'body': json.dumps({'path': str(newer.path)}),
                         'headers': {'Content-Type': 'application/json', 'X-CSRF-Token': token}}, self.origin)
            self.assertEqual(switched['status'], 200)
            conn.send(encoded[1:]); response = conn.getresponse(); response.read(); conn.close()
            self.assertEqual(response.status, 403)
            self.assertEqual(states(self.right), before_old); self.assertEqual(states(newer), before_new)
            self.token = json.loads(fetch(self.origin, {'url': '/api/bootstrap'}, self.origin)['body'])['token']
            restored = fetch(self.origin, {'method': 'POST', 'url': '/api/libraries/open',
                         'body': json.dumps({'path': str(self.right.path)}),
                         'headers': {'Content-Type': 'application/json', 'X-CSRF-Token': self.token}}, self.origin)
            self.assertEqual(restored['status'], 200)
            self.token = json.loads(fetch(self.origin, {'url': '/api/bootstrap'}, self.origin)['body'])['token']
            self.reference.token = self.token
