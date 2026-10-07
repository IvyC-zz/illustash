"""M2 user-label/undo acceptance, on independent disposable package clones only.

Explicit ILLUSTASH_TERMS_BIN is required; missing release is not acceptance. The native host is
compared with the former Python reference handler through fixed recordings (tests/reference_replay.py,
#9): the seed packages, every reference response and the reference library state after each step
were recorded while the reference existed; ``ILLUSTASH_REFERENCE=record`` re-runs it. Neither side
starts research workers. All app/source/provenance tables, raw DDL, typed cells and rowids are
compared after each HTTP step (as digests), including history, undo and durable events. Responses
are compared byte for byte; only session tokens, loopback origins and temporary roots are
placeholders. The recorded seed fixed its timestamp DEFAULTs and preseeded user terms, so native
results are deterministic. Snapshots are individually transactional, not cross-database atomic.
"""
from contextlib import closing
import datetime
import http.client
import json
import os
from pathlib import Path
import re
import shutil
import signal
import sqlite3
import subprocess
import tempfile
import threading
import time
import unittest
from unittest.mock import patch

from tests.reference_replay import RECORD, Library, Recorder, library_digests, unclocked
from tests.test_read_http import INTERRUPT, SPAWN, free_port, wait_until
from tests.synthetic_source import source_fixture as fixture
from tools import verify_read_http
from tools.verify_read_http import fetch, temporary_path

HEADERS = ('Content-Type', 'Content-Length', 'Content-Encoding', 'Cache-Control', 'Vary',
           'X-Content-Type-Options', 'Referrer-Policy', 'Content-Security-Policy')


def quote(name):
    return '"' + name.replace('"', '""') + '"'


def difference(left, right):
    """Byte difference of two responses; clock-like text is compared as `<clock>`."""
    masked = [dict(r, body=unclocked(r['body'])) if 'body' in r else r for r in (left, right)]
    return verify_read_http.difference(*masked)


def snapshot(path):
    """Schema and every table (typed cells and physical rowids), as per-table digests."""
    from tests.reference_replay import table_digests
    return table_digests(temporary_path(path))


def states(lib):
    return library_digests(lib)


def freeze_seed_defaults(path):
    """Only a freshly constructed temporary seed: alter DEFAULTs once, never compared output."""
    path = temporary_path(path)
    with closing(sqlite3.connect(path)) as c:
        current, storage = c.execute('SELECT CURRENT_TIMESTAMP,typeof(CURRENT_TIMESTAMP)').fetchone()
        assert storage == 'text' and re.fullmatch(r'\d{4}-\d{2}-\d{2} \d{2}:\d{2}:\d{2}', current)
        stamp = datetime.datetime.strptime(current, '%Y-%m-%d %H:%M:%S').replace(tzinfo=datetime.timezone.utc)
        assert abs((datetime.datetime.now(datetime.timezone.utc) - stamp).total_seconds()) < 2
        version = c.execute('PRAGMA schema_version').fetchone()[0]
        rows = c.execute("SELECT name,sql FROM sqlite_schema WHERE type='table' AND sql IS NOT NULL").fetchall()
        c.execute('PRAGMA writable_schema=ON')
        for name, sql in rows:
            fixed = re.sub(r'\bDEFAULT\s+CURRENT_TIMESTAMP\b', "DEFAULT '2026-10-01 00:00:00'", sql, flags=re.I)
            if fixed != sql:
                c.execute("UPDATE sqlite_schema SET sql=? WHERE type='table' AND name=?", (fixed, name))
        c.execute('PRAGMA writable_schema=OFF')
        c.execute(f'PRAGMA schema_version={version + 1}')
        c.commit()
    with closing(sqlite3.connect(path)) as c:
        assert c.execute('PRAGMA integrity_check').fetchone()[0] == 'ok'


class SnapshotTest(unittest.TestCase):
    def test_detects_typed_cells_rowids_ddl_history_events_and_auxiliary_changes(self):
        with tempfile.TemporaryDirectory() as tmp:
            a, b = Path(tmp) / 'a.sqlite', Path(tmp) / 'b.sqlite'
            with closing(sqlite3.connect(a)) as c:
                c.executescript("CREATE TABLE data(value); INSERT INTO data VALUES (1); "
                                "CREATE TABLE actions(id INTEGER PRIMARY KEY,before_json TEXT);"
                                "INSERT INTO actions VALUES (1,'[]');"
                                "CREATE TABLE backend_effects(id INTEGER PRIMARY KEY,status TEXT);"
                                "INSERT INTO backend_effects VALUES (1,'pending');")
            for sql, table in (("UPDATE data SET value=1.0", 'data'),
                               ("UPDATE data SET rowid=3", 'data'),
                               ("ALTER TABLE data ADD COLUMN extra", 'schema'),
                               ("UPDATE actions SET before_json='[1]'", 'actions'),
                               ("UPDATE backend_effects SET status='done'", 'backend_effects')):
                shutil.copyfile(a, b)
                self.assertEqual(snapshot(a), snapshot(b))
                with closing(sqlite3.connect(b)) as c:
                    c.execute(sql); c.commit()
                self.assertNotEqual(snapshot(a)[table], snapshot(b)[table], sql)
            # The same snapshot function is used for named source/provenance DBs, not only app.
            self.assertIsNotNone(difference({'status': 200, 'body': b'{"x": 1}'},
                                            {'status': 200, 'body': b'{"x":1}'}))


    def test_http_pair_rejects_same_failed_partial_commit_and_aux_mutation(self):
        # Exercise the real pair gate without a native release or a live HTTP server.
        from types import SimpleNamespace
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            left, right = SimpleNamespace(path=root / 'left'), SimpleNamespace(path=root / 'right')
            for lib in (left, right):
                lib.path.mkdir()
                for filename in ('library.sqlite', 'source.sqlite', 'provenance.sqlite'):
                    with closing(sqlite3.connect(lib.path / filename)) as c:
                        c.executescript('CREATE TABLE data(value); INSERT INTO data VALUES (1);')
            harness = NativeHumanLabelsTest('test_tag_add_remove_filter_duplicates_and_literal_false')
            harness.left, harness.right = left, right
            harness.direct, harness.origin = 'left', 'right'
            harness.token, harness.latency, harness.sentinel = 'token', [], root / 'sentinel'
            harness.rec = Live()
            def partial(base, item, origin):
                lib = left if base == 'left' else right
                with closing(sqlite3.connect(lib.path / 'library.sqlite')) as c:
                    c.execute('UPDATE data SET value=2'); c.commit()
                return {'status': 500, 'body': b'{"error":"same"}', 'route': 'core', 'headers': {}, 'ms': 1}
            with patch(__name__ + '.fetch', partial), self.assertRaisesRegex(AssertionError, 'failed POST partially committed'):
                harness.pair('/api/tag', {'ids': ['a'], 'tag': 'synthetic'}, expected=500)
            for filename in ('source.sqlite', 'provenance.sqlite'):
                shutil.copyfile(left.path / 'source.sqlite', right.path / 'source.sqlite')
                shutil.copyfile(left.path / 'provenance.sqlite', right.path / 'provenance.sqlite')
                self.assertEqual(states(left), states(right))
                def auxiliary(base, item, origin):
                    if base == 'right':
                        with closing(sqlite3.connect(right.path / filename)) as c:
                            c.execute('UPDATE data SET value=3'); c.commit()
                    return {'status': 200, 'body': b'{}', 'route': 'core', 'headers': {}, 'ms': 1}
                with patch(__name__ + '.fetch', auxiliary), self.assertRaises(AssertionError):
                    harness.pair('/api/tag', {'ids': ['a'], 'tag': 'synthetic'})


class Live:
    """Recorder stand-in for harness self-tests: every reference value is computed live."""
    def value(self, label, compute):
        return compute()

    def state(self, label, lib):
        return states(lib)


class NativeHumanLabelsTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        configured = os.environ.get('ILLUSTASH_TERMS_BIN')
        if not configured or not Path(configured).is_file():
            raise unittest.SkipTest('set ILLUSTASH_TERMS_BIN explicitly to the M2 release; skip is not acceptance')
        cls.binary = Path(configured).resolve()
        cls.resources = Path(os.environ.get('ILLUSTASH_NATIVE_ROOT') or cls.binary.parents[4]).resolve()

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='illustash-m2-human-')
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name).resolve()
        self.rec = Recorder(self)
        self.addCleanup(self.rec.finish, self)
        self.rec.alias(self.root, '@ROOT@')
        support, shared = self.root / 'support', self.root / 'shared'
        shared.mkdir()
        if RECORD:
            from web import library
            for name, value in (('APP_SUPPORT', support), ('SHARED', shared),
                                ('HOME', self.root / 'home' / 'Pictures' / 'Illustash')):
                scoped = patch.object(library, name, value)
                scoped.start(); self.addCleanup(scoped.stop)
        sentinel_bin = self.root / 'bin'; sentinel_bin.mkdir()
        self.sentinel = self.root / 'python-was-launched'
        for command in ('python', 'python3', 'python3.12'):
            executable = sentinel_bin / command
            executable.write_text('#!/bin/sh\n/bin/echo forbidden > "$ILLUSTASH_PYTHON_SENTINEL"\nexit 97\n')
            executable.chmod(0o755)
        self.env = {**os.environ, 'PATH': str(sentinel_bin), 'HOME': str(self.root / 'home'), 'USERPROFILE': str(self.root / 'home'),
                    'ILLUSTASH_APP_SUPPORT': str(support), 'ILLUSTASH_SHARED': str(shared),
                    'ILLUSTASH_PYTHON_SENTINEL': str(self.sentinel),
                    # Reproducible native keys, so recorded expectations hold (store::new_key test hook).
                    'ILLUSTASH_TEST_KEY_COUNTER': str(self.root / 'key-counter')}
        if RECORD:
            self.build_reference_seed()
        for name in ('pristine', 'left', 'right'):
            lib = self.rec.package(name, self.root / f'{name}.pilib')
            if not RECORD:
                setattr(self, name, lib)
        self.alice, self.bob, self.artist = self.rec.value('ids', lambda: (self.alice, self.bob, self.artist))
        self.port = free_port(); self.origin = f'http://127.0.0.1:{self.port}'
        self.rec.alias(self.origin, '@ORIGIN@')
        if RECORD:
            self.start_reference()
        self.process = self.start(self.right)
        self.token = json.loads(fetch(self.origin, {'url': '/api/bootstrap'}, self.origin)['body'])['token']
        self.rec.alias(self.token, '@TOKEN@')
        if RECORD:
            self.reference.token = self.token
        self.assertEqual(self.rec.state('start', self.left), states(self.right))
        self.latency = []

    def build_reference_seed(self):
        """Recording only: the original seed, built by the former Python application."""
        from provenance import db as provenance_db, export
        from web import library, terms_core
        from web.store import Store
        Store = getattr(self, 'store_factory', None) or Store
        seed = library.create(self.root / 'original.pilib', 'Synthetic')
        fixture(seed.path).rename(seed.source)
        with closing(sqlite3.connect(seed.source)) as c:
            c.execute('ALTER TABLE assets ADD COLUMN stable_hash TEXT')
            c.execute("UPDATE assets SET stable_hash='same' WHERE uuid IN ('a','b')")
            c.commit()
        with closing(provenance_db.connect(seed)) as c:
            self.artist = provenance_db.artist_for(c, [('x', 'synthetic_artist', '')], 'test')
            c.executemany("INSERT INTO image_artists(asset,artist_id,method,status) VALUES (?,?,'pixel_hash','auto')",
                          [('a', self.artist), ('b', self.artist)])
            c.commit()
        store = Store(seed.source, seed.db); store.refresh()
        seed.config['id'] = store.library_id(); seed.save()
        terms_core.import_package(seed.db, store.library_id(), export.write(seed, self.root / 'artist-evidence'))
        freeze_seed_defaults(seed.db)
        self.pristine = self.clone(seed, 'pristine')
        # Each identity is born once before cloning. The tested calls use real tag/undo commands.
        for tag in ('合成 标签', '第二标签'):
            store.undo(store.tag(['c'], tag)['id'])
        self.alice, self.bob = store.entities('Alice')[0]['id'], store.entities('Bob')[0]['id']
        self.left, self.right = self.clone(seed, 'left'), self.clone(seed, 'right')

    def start_reference(self):
        """Recording only: the former Python reference handler serving the left package."""
        from web import artists, library
        from web.server import Server
        from web.store import Store
        self.left = library.open_library(self.left.path)
        self.store = Store.open(self.left.source, self.left.db)
        self.store.artist_assets = lambda artist: artists.assets_of(self.left, artist, rejected=self.store.artist_verdicts())
        self.reference = Server(('127.0.0.1', 0), self.store, self.left.thumbnails, public_origin=self.origin)
        self.reference.library = self.left
        self.reference.forced_off = {'index_check', 'names', 'provenance', 'similar', 'sync'}
        self.direct = f'http://127.0.0.1:{self.reference.server_port}'
        self.thread = threading.Thread(target=self.reference.serve_forever, daemon=True); self.thread.start()
        self.addCleanup(self.reference.server_close)
        self.addCleanup(self.thread.join)
        self.addCleanup(self.reference.shutdown)

    def clone(self, source, name):
        path = self.root / (name + '.pilib'); path.mkdir()
        shutil.copyfile(source.path / 'library.json', path / 'library.json')
        shutil.copyfile(source.path / 'album-char-map.json', path / 'album-char-map.json')
        for filename in ('library.sqlite', 'source.sqlite', 'provenance.sqlite'):
            with closing(sqlite3.connect(source.path / filename)) as src, closing(sqlite3.connect(path / filename)) as dst:
                src.backup(dst)
        for directory in ('thumbnails', 'features', 'backups'):
            (path / directory).mkdir()
        return Library(path)

    def stop(self, process):
        if process.poll() is None:
            process.send_signal(INTERRUPT)
            try:
                process.wait(5)
            except subprocess.TimeoutExpired:
                process.kill(); process.wait(3)

    def start(self, lib):
        log = tempfile.TemporaryFile(); self.addCleanup(log.close)
        process = subprocess.Popen([str(self.binary), 'http', '--library', str(lib.path),
                                    '--port', str(self.port), '--root', str(self.resources), '--no-names', '--no-index-check'],
                                   env=self.env, stdout=log, stderr=log, **SPAWN)
        self.addCleanup(self.stop, process)
        try:
            wait_until(lambda: fetch(self.origin, {'url': '/api/bootstrap'}, self.origin)['status'] == 200, timeout=5)
        except AssertionError:
            log.seek(0); self.fail(f'native startup failed: {log.read().decode(errors="replace")}')
        return process

    def reference_response(self, label, item):
        """The former reference's response to the same request (recorded)."""
        return self.rec.value(label, lambda: fetch(self.direct, item, self.origin))

    def pair(self, url, body, *, expected=200, headers=None):
        item = {'url': url, 'method': 'POST', 'body': json.dumps(body, ensure_ascii=False).encode('utf-8'),
                'headers': {'Content-Type': 'application/json', 'X-CSRF-Token': self.token, **(headers or {})}}
        before = [self.rec.state('before', self.left), states(self.right)]
        responses = [self.reference_response('response', item), fetch(self.origin, item, self.origin)]
        after = [self.rec.state('after', self.left), states(self.right)]
        # Even two identically failed/partially committed commands must fail acceptance.
        for old, new, response in zip(before, after, responses):
            if response['status'] >= 400:
                self.assertEqual(old, new, ('failed POST partially committed', url, body))
        self.assertIsNone(difference(*responses), (url, body, difference(*responses)))
        self.assertEqual(responses[1]['status'], expected, (url, body, responses[1]['body']))
        self.assertEqual(responses[1]['route'], 'core')
        for header in HEADERS:
            self.assertEqual(responses[0]['headers'].get(header), responses[1]['headers'].get(header), header)
        self.assertEqual(after[0], after[1], (url, body))
        self.latency.append((url, responses[0]['ms'], responses[1]['ms']))
        self.assertFalse(self.sentinel.exists())
        return json.loads(responses[1]['body'])

    def latest_action(self):
        """Latest live action; both sides hold the same history (checked after every step)."""
        with closing(sqlite3.connect(self.right.db)) as c:
            action = c.execute('SELECT id FROM actions WHERE undone=0 ORDER BY id DESC LIMIT 1').fetchone()
        return action[0] if action else None

    def undo_latest(self):
        action = self.latest_action()
        self.assertIsNotNone(action)
        return self.pair('/api/undo', {'id': action})

    def seed_both(self, operation):
        # Seed history through actual old commands once (recorded), then clone the complete committed input.
        self.stop(self.process)
        if RECORD:
            operation(self.store)
        self.rec.package('seeded', self.left.path)
        for filename in ('library.sqlite', 'source.sqlite', 'provenance.sqlite'):
            with closing(sqlite3.connect(self.left.path / filename)) as src, closing(sqlite3.connect(self.right.path / filename)) as dst:
                src.backup(dst)
        self.process = self.start(self.right)
        self.token = json.loads(fetch(self.origin, {'url': '/api/bootstrap'}, self.origin)['body'])['token']
        self.rec.alias(self.token, '@TOKEN@')
        if RECORD:
            self.reference.token = self.token
        self.assertEqual(self.rec.state('seeded', self.left), states(self.right))

    def test_tag_add_remove_filter_duplicates_and_literal_false(self):
        first = self.pair('/api/tag', {'ids': ['a', 'a'], 'tag': ' 合成\t标签\n'})['undo']
        self.assertEqual(first['label'], '已给 1 张加上标签 合成 标签')
        for value in (0, None, 'false', False, True):
            self.pair('/api/tag', {'ids': ['a'], 'tag': '合成 标签', 'add': value})
        self.pair('/api/tag', {'filter': {}, 'exclude': ['b'], 'tag': '第二标签'})
        self.pair('/api/tag', {'filter': {'artist': self.artist}, 'tag': '第二标签'})
        self.pair('/api/tag', {'ids': ['c'] * 501, 'tag': '第二标签', 'confirm': True})
        for _ in range(9):
            self.undo_latest()

    def test_validation_error_order_and_no_partial_commit(self):
        cases = [({}, 400), ({'tag': None, 'ids': ['missing']}, 400),
                 ({'tag': 'x' * 61, 'ids': []}, 400), ({'tag': ' ' * 61}, 400)]
        base = {'tag': '合成 标签'}
        cases += [(base | body, 400) for body in (
            {'ids': None}, {'ids': []}, {'ids': 'a'}, {'ids': [1]}, {'ids': ['a', 'missing']},
            {'ids': ['a'], 'exclude': 'b'}, {'ids': ['a'], 'exclude': [1]},
            {'ids': ['a'], 'exclude': ['b']}, {'ids': ['a'], 'filter': {}},
            {'filter': []}, {'filter': {'unknown': ''}}, {'filter': {'entity': 1}},
            {'filter': {'q': 'missing-synthetic'}}, {'filter': {}, 'exclude': ['a', 'b', 'c']},
            {'ids': ['a'] * 501}, {'ids': ['a'] * 501, 'confirm': 1})]
        for body, status in cases:
            with self.subTest(body=body):
                self.pair('/api/tag', body, expected=status)
        for value in (None, 999999, 0, '999999', 1.25, [], {}):
            with self.subTest(undo=value):
                self.pair('/api/undo', {'id': value}, expected=400 if value not in ([], {}) else 500)

    def test_cross_type_latest_undo_and_committed_effects(self):
        def stack(store):
            store.skip_identity_pair(self.alice, self.bob)
            store.curate(['c'], 'exclude')
            store.judge_artist(['b'], self.artist, 'no', '@synthetic_artist')
            store.change(['b'], 'confirmed', self.bob)
        self.seed_both(stack)
        self.pair('/api/tag', {'ids': ['c'], 'tag': '第二标签'})
        with closing(sqlite3.connect(self.right.db)) as c:
            oldest = c.execute('SELECT min(id) FROM actions WHERE undone=0').fetchone()[0]
        self.pair('/api/undo', {'id': oldest}, expected=400)
        for _ in range(5):
            self.undo_latest()
        with closing(sqlite3.connect(self.right.db)) as c:
            self.assertEqual(c.execute("SELECT kind,status,error FROM backend_effects").fetchall(),
                             [('verdict', 'done', '')] * 4)
        self.pair('/api/undo', {'id': oldest}, expected=400)

    def test_injected_writes_and_unknown_history_are_atomic(self):
        for table in ('actions', 'term_verdicts'):
            for lib in (self.left, self.right):
                with closing(sqlite3.connect(lib.db)) as c:
                    c.execute(f"CREATE TRIGGER fail_write BEFORE INSERT ON {table} BEGIN SELECT RAISE(ABORT,'synthetic write failure'); END")
                    c.commit()
            self.pair('/api/tag', {'ids': ['a'], 'tag': '合成 标签'}, expected=500)
            for lib in (self.left, self.right):
                with closing(sqlite3.connect(lib.db)) as c:
                    c.execute('DROP TRIGGER fail_write'); c.commit()
        for owner in ('character', 'curation'):
            def seed(store):
                if owner == 'character':
                    store.change(['c'], 'confirmed', self.alice)
                else:
                    store.curate(['c'], 'exclude')
            self.seed_both(seed)
            for lib in (self.left, self.right):
                with closing(sqlite3.connect(lib.db)) as c:
                    c.execute("CREATE TRIGGER fail_event BEFORE INSERT ON backend_effects BEGIN SELECT RAISE(ABORT,'synthetic event failure'); END")
                    c.commit()
            with closing(sqlite3.connect(self.right.db)) as c:
                action = c.execute('SELECT max(id) FROM actions WHERE undone=0').fetchone()[0]
            self.pair('/api/undo', {'id': action}, expected=500)
            for lib in (self.left, self.right):
                with closing(sqlite3.connect(lib.db)) as c:
                    c.execute('DROP TRIGGER fail_event'); c.commit()
            self.undo_latest()
        for lib in (self.left, self.right):
            with closing(sqlite3.connect(lib.db)) as c:
                c.execute("INSERT INTO actions(label,before_json) VALUES ('unknown synthetic','[{\"unknown\":true}]')")
                c.commit()
        self.pair('/api/undo', {'id': self.rec.value('reference undo id', lambda: self.store.overview()['undo']['id'])}, expected=500)

    def test_csrf_host_and_origin_refuse_without_database_writes(self):
        for headers in ({'X-CSRF-Token': 'bad'}, {'Host': 'evil.example'}, {'Origin': 'https://evil.example'}):
            self.pair('/api/tag', {'ids': ['a'], 'tag': '合成 标签'}, expected=403, headers=headers)
        self.pair('/api/undo', {'id': 1}, expected=403, headers={'X-CSRF-Token': 'bad'})

    def test_new_concept_failure_is_fully_rolled_back_native(self):
        for lib in (self.left, self.right):
            with closing(sqlite3.connect(lib.db)) as c:
                c.execute("CREATE TRIGGER fail_action BEFORE INSERT ON actions BEGIN SELECT RAISE(ABORT,'synthetic action failure'); END")
                c.commit()
        # The injected schema itself is part of the common input, not an output exception.
        before = [self.rec.state('before', self.left), states(self.right)]
        item = {'method': 'POST', 'url': '/api/tag', 'body': json.dumps({'ids': ['a'], 'tag': '全新故障标签'}),
                'headers': {'Content-Type': 'application/json', 'X-CSRF-Token': self.token}}
        old = self.reference_response('response', item)
        native = fetch(self.origin, item, self.origin)
        self.assertIsNone(difference(old, native), difference(old, native))
        self.assertEqual(native['status'], 500)
        self.assertEqual(states(self.right), before[1])
        # The old CLI creates the concept outside its verdict transaction. Record the real
        # legacy partial commit separately; this exception never weakens normal pair().
        after_old = self.rec.state('after old', self.left)
        self.assertNotEqual(after_old, before[0], 'the legacy partial-create bug must be evidenced')
        self.assertEqual(after_old['source'], before[0]['source'])
        self.assertEqual(after_old['provenance'], before[0]['provenance'])
        self.assertEqual(after_old['app']['actions'], before[0]['app']['actions'])
        self.assertEqual(after_old['app']['backend_effects'], before[0]['app']['backend_effects'])
        def old_concepts():
            with closing(sqlite3.connect(self.left.db)) as c:
                return c.execute("SELECT count(*) FROM terms t JOIN term_names n ON n.term_id=t.id WHERE t.kind='concept' AND n.value='全新故障标签'").fetchone()[0]
        self.assertEqual(self.rec.value('old concepts', old_concepts), 1)
        with closing(sqlite3.connect(self.right.db)) as c:
            self.assertEqual(c.execute("SELECT count(*) FROM terms t JOIN term_names n ON n.term_id=t.id WHERE t.kind='concept' AND n.value='全新故障标签'").fetchone()[0], 0)
        # Successful birth is native-only: the two engines generate independent UUIDs.
        # Do not map them or claim full two-side state equality for this separate phase.
        with closing(sqlite3.connect(self.right.db)) as c:
            c.execute('DROP TRIGGER fail_action'); c.commit()
        retry_before = states(self.right)
        created = fetch(self.origin, item, self.origin)
        self.assertEqual(created['status'], 200, created['body'])
        self.assertEqual(created['route'], 'core')
        action = json.loads(created['body'])['undo']
        self.assertEqual(action['label'], '已给 1 张加上标签 全新故障标签')
        def reference_tags():
            from web.store import Store
            return Store.open(self.right.source, self.right.db).user_tags()
        expected_tags = self.rec.value('reference reading of the native tags', reference_tags)
        tags = fetch(self.origin, {'url': '/api/user-tags'}, self.origin)
        self.assertEqual(tags['status'], 200)
        self.assertEqual(tags['body'], json.dumps(expected_tags, ensure_ascii=False).encode())
        self.assertEqual([entry['tag'] for entry in expected_tags], ['全新故障标签'])
        with closing(sqlite3.connect(self.right.db)) as c:
            self.assertEqual(c.execute('SELECT uuid,tag FROM asset_tags ORDER BY uuid,tag').fetchall(),
                             [('a', '全新故障标签'), ('b', '全新故障标签')])
            history = c.execute('SELECT id,label,ts,before_json,undone FROM actions WHERE id=?', (action['id'],)).fetchone()
            self.assertEqual(history[:3], (action['id'], action['label'], action['ts']))
            self.assertEqual(json.loads(history[3]), [{'terms': 'verdicts'}])
            self.assertEqual(history[4], 0)
            self.assertEqual(c.execute('SELECT max(id) FROM actions WHERE undone=0').fetchone()[0], action['id'])
            self.assertEqual(c.execute('PRAGMA foreign_key_check').fetchall(), [])
        for name in ('source', 'provenance'):
            self.assertEqual(states(self.right)[name], retry_before[name])
        undone = fetch(self.origin, {'method': 'POST', 'url': '/api/undo', 'body': json.dumps({'id': action['id']}).encode(),
                                     'headers': {'Content-Type': 'application/json', 'X-CSRF-Token': self.token}}, self.origin)
        self.assertEqual(undone['status'], 200, undone['body'])
        self.assertEqual(undone['route'], 'core')
        self.assertEqual(undone['body'], b'{"undo": null}')
        self.assertEqual(fetch(self.origin, {'url': '/api/user-tags'}, self.origin)['body'], b'[]')
        with closing(sqlite3.connect(self.right.db)) as c:
            self.assertEqual(c.execute('SELECT uuid,tag FROM asset_tags').fetchall(), [])
            self.assertEqual(c.execute('SELECT undone FROM actions WHERE id=?', (action['id'],)).fetchone()[0], 1)
            self.assertEqual(c.execute('SELECT max(id) FROM actions WHERE undone=0').fetchone()[0], None)
            self.assertEqual(c.execute('PRAGMA foreign_key_check').fetchall(), [])
            # Undo removes the verdict, while the newly created identity and complete history persist.
            self.assertEqual(c.execute("SELECT count(*) FROM terms t JOIN term_names n ON n.term_id=t.id WHERE t.kind='concept' AND n.value='全新故障标签'").fetchone()[0], 1)
        retry_after = states(self.right)
        for name in ('source', 'provenance'):
            self.assertEqual(retry_after[name], retry_before[name])
        self.assertEqual(retry_after['app']['schema'], retry_before['app']['schema'])
        self.assertEqual(retry_after['app']['backend_effects'], retry_before['app']['backend_effects'])
        self.assertFalse(self.sentinel.exists())

    def test_undo_input_types_keep_owner_specific_legacy_contract(self):
        for owner in ('tag', 'artist', 'character', 'curation', 'identity_skip'):
            for value in (True, 1.0, '1'):
                with self.subTest(owner=owner, id=value):
                    def seed(store):
                        # Each owner's actual first action is produced from the untouched,
                        # complete no-history seed. No dangling/rebound action foreign keys.
                        for filename in ('library.sqlite', 'source.sqlite', 'provenance.sqlite'):
                            with closing(sqlite3.connect(self.pristine.path / filename)) as src, closing(sqlite3.connect(self.left.path / filename)) as dst:
                                src.backup(dst)
                        if owner == 'tag':
                            action = store.tag(['c'], '第二标签')
                        elif owner == 'artist':
                            action = store.judge_artist(['c'], self.artist, 'no', '合成画师')
                        elif owner == 'character':
                            action = store.change(['c'], 'confirmed', self.alice)
                        elif owner == 'curation':
                            action = store.curate(['c'], 'exclude')
                        else:
                            action = store.skip_identity_pair(self.alice, self.bob)
                        self.assertEqual(action['id'], 1)
                        with store.connect() as c:
                            self.assertEqual(c.execute('PRAGMA foreign_key_check').fetchall(), [])
                    self.seed_both(seed)
                    rejected = owner == 'character' or owner in ('curation', 'identity_skip') and isinstance(value, str)
                    self.pair('/api/undo', {'id': value}, expected=400 if rejected else 200)

    def test_write_lock_contention_preserves_exact_error_and_all_data(self):
        with closing(sqlite3.connect(self.left.db)) as left, closing(sqlite3.connect(self.right.db)) as right:
            left.execute('BEGIN IMMEDIATE'); right.execute('BEGIN IMMEDIATE')
            self.pair('/api/tag', {'ids': ['a'], 'tag': '合成 标签'}, expected=500)
            left.rollback(); right.rollback()
        self.pair('/api/tag', {'ids': ['a'], 'tag': '合成 标签'})
        self.undo_latest()

    def test_inflight_old_token_cannot_write_after_library_switch(self):
        self.stop(self.process)
        newer = self.clone(self.right, 'new-session')
        self.process = self.start(self.right)
        old_token = json.loads(fetch(self.origin, {'url': '/api/bootstrap'}, self.origin)['body'])['token']
        before_old, before_new = states(self.right), states(newer)
        body = json.dumps({'ids': ['a'], 'tag': '合成 标签'}).encode()
        conn = http.client.HTTPConnection('127.0.0.1', self.port, timeout=5)
        self.addCleanup(conn.close)
        conn.putrequest('POST', '/api/tag', skip_host=True)
        for name, value in {'Host': self.origin.split('://')[1], 'Origin': self.origin,
                            'Content-Type': 'application/json', 'X-CSRF-Token': old_token,
                            'Content-Length': str(len(body))}.items():
            conn.putheader(name, value)
        conn.endheaders(); conn.send(body[:1])
        # A second connection switches while the old request is still receiving its body.
        response = fetch(self.origin, {'method': 'POST', 'url': '/api/libraries/open',
                                      'body': json.dumps({'path': str(newer.path)}),
                                      'headers': {'Content-Type': 'application/json', 'X-CSRF-Token': old_token}}, self.origin)
        self.assertEqual(response['status'], 200, response['body'])
        current_token = json.loads(fetch(self.origin, {'url': '/api/bootstrap'}, self.origin)['body'])['token']
        self.assertNotEqual(current_token, old_token)
        conn.send(body[1:]); stale = conn.getresponse()
        self.assertEqual(stale.status, 403, stale.read())
        self.assertEqual(states(self.right), before_old)
        self.assertEqual(states(newer), before_new)
        stale_again = fetch(self.origin, {'method': 'POST', 'url': '/api/tag', 'body': body,
                                         'headers': {'Content-Type': 'application/json', 'X-CSRF-Token': old_token}}, self.origin)
        self.assertEqual(stale_again['status'], 403)
        self.assertEqual(states(newer), before_new)
        self.assertFalse(self.sentinel.exists())

    def test_pending_verdict_startup_and_failed_ack_resume_without_http_trigger(self):
        # A dispatcher acknowledgement failure happens AFTER the human transaction has
        # committed. Its exact old HTTP500 does not mean the undo itself was rolled back.
        # Keep this explicit committed phase separate from pair()'s precommit fault gate.
        for owner in ('character', 'curation'):
            def seed(store):
                if owner == 'character':
                    store.change(['c'], 'confirmed', self.alice)
                else:
                    store.curate(['c'], 'exclude')
            self.seed_both(seed)
            for lib in (self.left, self.right):
                with closing(sqlite3.connect(lib.db)) as c:
                    c.execute("CREATE TRIGGER fail_ack BEFORE UPDATE ON backend_effects WHEN OLD.kind='verdict' "
                              "BEGIN SELECT RAISE(ABORT,'synthetic acknowledgement failure'); END")
                    c.commit()
            with closing(sqlite3.connect(self.right.db)) as c:
                action = c.execute('SELECT max(id) FROM actions WHERE undone=0').fetchone()[0]
            before = [self.rec.state('before', self.left), states(self.right)]
            item = {'method': 'POST', 'url': '/api/undo', 'body': json.dumps({'id': action}).encode(),
                    'headers': {'Content-Type': 'application/json', 'X-CSRF-Token': self.token}}
            old, native = self.reference_response('response', item), fetch(self.origin, item, self.origin)
            self.assertIsNone(difference(old, native), difference(old, native))
            self.assertEqual(native['status'], 500)
            self.assertEqual(native['body'], json.dumps({'error': '保存失败，未提交的操作已回滚；请查看启动终端'}, ensure_ascii=False).encode())
            after = [self.rec.state('after', self.left), states(self.right)]
            self.assertEqual(after[0], after[1])
            def committed(lib):
                with closing(sqlite3.connect(lib.db)) as c:
                    return (c.execute('SELECT undone FROM actions WHERE id=?', (action,)).fetchone()[0],
                            c.execute("SELECT kind,status,error FROM backend_effects ORDER BY id DESC LIMIT 1").fetchone())
            for lib, previous, current in zip((self.left, self.right), before, after):
                self.assertEqual(previous['source'], current['source'])
                self.assertEqual(previous['provenance'], current['provenance'])
            for seen in (self.rec.value('old side committed', lambda: committed(self.left)), committed(self.right)):
                self.assertEqual(tuple(seen[0:1]) + (tuple(seen[1]),), (1, ('verdict', 'pending', '')))
            for lib in (self.left, self.right):
                with closing(sqlite3.connect(lib.db)) as c:
                    c.execute('DROP TRIGGER fail_ack'); c.commit()
            if RECORD:
                from web import effects
                effects.drain(self.store, self.reference._after_operation)
            drained = self.rec.state('drained', self.left)
            # No HTTP request: the native loop must recover its committed pending event.
            wait_until(lambda: states(self.right) == drained, timeout=5)
            self.assertFalse(self.sentinel.exists())
        self.stop(self.process)
        with closing(sqlite3.connect(self.right.db)) as c:
            existing = c.execute('SELECT kind,status,error FROM backend_effects ORDER BY id').fetchall()
            c.execute("INSERT INTO backend_effects(kind) VALUES ('verdict')")
            c.execute("INSERT INTO backend_effects(kind) VALUES ('unknown-synthetic')")
            c.execute("CREATE TRIGGER fail_ack BEFORE UPDATE ON backend_effects WHEN OLD.kind='verdict' "
                      "BEGIN SELECT RAISE(ABORT,'synthetic acknowledgement failure'); END")
            c.commit()
        self.process = self.start(self.right)
        with closing(sqlite3.connect(self.right.db)) as c:
            self.assertEqual(c.execute('SELECT kind,status,error FROM backend_effects ORDER BY id').fetchall(),
                             existing + [('verdict', 'pending', ''), ('unknown-synthetic', 'pending', '')])
            c.execute('DROP TRIGGER fail_ack'); c.commit()
        def consumed():
            with closing(sqlite3.connect(self.right.db)) as c:
                return c.execute("SELECT count(*) FROM backend_effects WHERE kind='verdict' AND status='pending'").fetchone()[0] == 0
        wait_until(consumed, timeout=5)
        with closing(sqlite3.connect(self.right.db)) as c:
            self.assertEqual(c.execute('SELECT kind,status,error FROM backend_effects ORDER BY id').fetchall(),
                             existing + [('verdict', 'done', ''), ('unknown-synthetic', 'pending', '')])
        # A second already-published pending verdict must also be consumed on restart.
        self.stop(self.process)
        with closing(sqlite3.connect(self.right.db)) as c:
            c.execute("INSERT INTO backend_effects(kind) VALUES ('verdict')"); c.commit()
        self.process = self.start(self.right)
        self.assertTrue(consumed())
