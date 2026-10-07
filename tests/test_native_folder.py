"""M3 folder scans: only owned temporary synthetic folders and packages.

The former Python scanner's and importer's results on the same inputs were recorded through
tests/reference_replay.py (#9; ``ILLUSTASH_REFERENCE=record`` re-runs them). Generated files get
content-derived modification times and native keys are reproducible, so the recorded states hold.
"""
from tests.native_worker_fixture import command as worker_command, source as worker_source, exists as worker_exists

import json
import os
import shutil
import sqlite3
import subprocess
import threading
import time
import unittest
from pathlib import Path
from unittest.mock import patch

from PIL import Image
from tests.folder_fixture import FolderSource, image, settle
from tests.reference_replay import RECORD, Recorder, unclocked
from tests.test_native_human_labels import snapshot
from tests import test_native_lifecycle as lifecycle
from tests.test_native_lifecycle import NativeLifecycleTest


class NativeFolderScanTest(unittest.TestCase):
    tearDown = FolderSource.tearDown
    rows = FolderSource.rows

    def setUp(self):
        FolderSource.setUp(self)
        self.rec = Recorder(self)
        self.addCleanup(self.rec.finish, self)
        self.rec.alias(self.root, '@ROOT@')
        # The package as first built (its asset keys are random): recorded, restored when replaying.
        self.rec.package('package', self.lib.path)
        self.env = {**os.environ, 'ILLUSTASH_TEST_KEY_COUNTER': str(self.root / 'key-counter')}

    @classmethod
    def setUpClass(cls):
        path = os.environ.get('ILLUSTASH_TERMS_BIN')
        if not path:
            raise unittest.SkipTest('explicit native binary required')
        cls.binary = Path(path).resolve()

    def scan_pair(self):
        settle(self.pics)
        old = self.root / 'old.sqlite'
        with sqlite3.connect(self.lib.source) as src, sqlite3.connect(old) as dest:
            src.backup(dest)
        before = set(u for u, in self.rows('SELECT uuid FROM assets'))
        result = subprocess.run([str(self.binary), 'folder-scan', '--db', str(self.lib.source)],
                                input=json.dumps(self.source).encode(), capture_output=True, timeout=15, env=self.env)
        self.assertEqual(result.returncode, 0, result.stderr.decode())
        new = json.loads(result.stdout)
        ids = [u for u, in self.rows('SELECT uuid FROM assets ORDER BY rowid') if u not in before]
        def old_scan():
            from web import folder
            with patch.object(folder.uuid, 'uuid4', side_effect=ids):
                return {'result': folder.scan(self.source, old), 'source': snapshot(old)}
        expected = self.rec.value('old scan', old_scan)
        self.assertEqual(new, expected['result'])
        self.assertEqual(snapshot(self.lib.source), expected['source'])
        return new

    def test_complete_source_typed_rowid_ddl_comparison(self):
        shutil.copyfile(self.pics / 'a.png', self.pics / 'copy.png')
        (self.pics / 'bad.gif').write_bytes(b'synthetic corrupt image')
        image(self.pics / '.hide' / 'invisible.png', 'red')
        image(self.pics / 'skip.pilib' / 'invisible.png', 'red')
        r = self.scan_pair()
        self.assertEqual(r['new'], 4)
        self.assertEqual(self.rows("SELECT width,height FROM assets WHERE filename='bad.gif'"), [(0, 0)])
        self.assertEqual(self.scan_pair()['changed'], 0)
        with sqlite3.connect(self.lib.source) as c:
            c.execute("INSERT INTO labels(uuid,dim,value,conf) SELECT uuid,'rating','general',1 FROM assets")
            c.execute("INSERT INTO embeddings(uuid,model,vec) SELECT uuid,'synthetic',x'0102' FROM assets")
        (self.pics / 'a.png').rename(self.pics / 'sub' / 'moved.png')
        image(self.pics / 'copy.png', 'yellow', (50, 20))
        image(self.pics / 'z-new.webp', 'blue')
        r = self.scan_pair()
        self.assertEqual((r['new'], r['changed'], r['moved']), (1, 1, 1))
        (self.pics / 'sub' / 'b.jpg').rename(self.root / 'saved.jpg')
        self.assertEqual(self.scan_pair()['gone'], 1)
        self.assertEqual(self.scan_pair()['gone'], 1)  # existing Python statistics count vanished paths again
        (self.root / 'saved.jpg').rename(self.pics / 'returned.jpg')
        r = self.scan_pair()
        self.assertEqual((r['returned'], r['moved']), (1, 1))
        p = self.pics / 'returned.jpg'
        os.utime(p, ns=(-1234567890, -1234567890))
        self.assertEqual(self.scan_pair()['changed'], 1)

    def test_source_commit_failure_rolls_back_and_retry(self):
        before = snapshot(self.lib.source)
        with sqlite3.connect(self.lib.source) as c:
            c.execute("CREATE TRIGGER fail_file BEFORE INSERT ON files BEGIN SELECT RAISE(ABORT,'synthetic source failure'); END")
        failed = snapshot(self.lib.source)
        result = subprocess.run([str(self.binary), 'folder-scan', '--db', str(self.lib.source)],
                                input=json.dumps(self.source).encode(), capture_output=True)
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(snapshot(self.lib.source), failed)
        with sqlite3.connect(self.lib.source) as c:
            c.execute('DROP TRIGGER fail_file')
        self.assertEqual(snapshot(self.lib.source), before)
        self.assertEqual(self.scan_pair()['new'], 2)


class NativeFolderHTTPTest(unittest.TestCase):
    setUpClass = classmethod(NativeLifecycleTest.setUpClass.__func__)
    _fixture_setup = NativeLifecycleTest.setUp

    def setUp(self):
        self._fixture_setup()
        if self._testMethodName.startswith('test_native_library_new'):
            return  # The factory now validates the complete synthetic reference before publication.
        with sqlite3.connect(self.lib.source) as c:
            for column in ('hidden INTEGER', 'stable_hash TEXT', 'orig_size INTEGER'):
                c.execute('ALTER TABLE assets ADD COLUMN ' + column)
            c.execute('CREATE TABLE embeddings(uuid TEXT NOT NULL,model TEXT NOT NULL,vec BLOB NOT NULL,PRIMARY KEY(uuid,model))')

    package = NativeLifecycleTest.package
    stop = NativeLifecycleTest.stop
    start = NativeLifecycleTest.start
    request = NativeLifecycleTest.request

    def finish(self):
        for _ in range(150):
            response = self.request('/api/sync')
            self.assertEqual(response['status'], 200)
            state = json.loads(response['body'])
            if state['state']['status'] not in ('scanning', 'importing'):
                return state
            time.sleep(.02)
        self.fail('sync did not finish')

    def sync(self):
        settle(self.new_folder, self.root / 'second-source')
        response = self.request('/api/sync', {})
        self.assertEqual(response['status'], 200, response['body'])
        self.assertEqual(response['body'], b'{"started": true}')
        return self.finish()

    def test_native_library_new_folder_inventory_paths_verdict_undo(self):
        settings = json.loads(self.request('/api/settings')['body'])
        cleared = json.loads(self.request('/api/cache', {'action': 'clear'})['body'])
        self.assertEqual(cleared.pop('removed'), 0)
        self.assertEqual(cleared, settings)
        self.assertNotIn('sync', settings['forced_off'])
        folder_path = self.new_folder
        image(folder_path / 'a.png', 'red')
        shutil.copyfile(folder_path / 'a.png', folder_path / 'copy.png')
        state = self.sync()
        self.assertEqual(state['state']['status'], 'needs_recognition', state)
        self.assertEqual(state['state']['total'], 3)
        with sqlite3.connect(self.lib.db) as c:
            self.assertEqual(c.execute('SELECT count(*) FROM assets').fetchone()[0], 3)
            self.assertEqual(c.execute('SELECT count(*) FROM duplicates').fetchone()[0], 2)
            self.assertEqual(c.execute("SELECT kind,status FROM backend_effects").fetchall(), [('imported', 'pending')])
            asset = c.execute("SELECT uuid FROM assets WHERE filename='a.png'").fetchone()[0]
        tagged = self.request('/api/tag', {'ids': [asset], 'tag': '合成标签'})
        self.assertEqual(tagged['status'], 200, tagged['body'])
        undo = json.loads(tagged['body'])['undo']
        before = snapshot(self.lib.db)['actions']
        (folder_path / 'a.png').rename(folder_path / 'moved.png')
        self.sync()
        with sqlite3.connect(self.lib.db) as c:
            self.assertEqual(c.execute('SELECT src_path FROM assets WHERE uuid=?', (asset,)).fetchone()[0], str(folder_path / 'moved.png'))
            self.assertEqual(c.execute('SELECT tag FROM asset_tags WHERE uuid=?', (asset,)).fetchall(), [('合成标签',)])
        self.assertEqual(snapshot(self.lib.db)['actions'], before)
        detail = self.request('/api/detail?id=' + asset)
        self.assertEqual(detail['status'], 200)
        # The asset's added time comes from the file's birth time, which only macOS lets settle() fix.
        self.assertEqual(unclocked(detail['body']), unclocked(lifecycle.NativeLifecycleTest.reference_response(self, '/api/detail?id=' + asset)['body']))
        self.assertEqual(self.request('/api/undo', {'id': undo['id']})['status'], 200)
        (folder_path / 'moved.png').rename(self.root / 'saved.png')
        state = self.sync()
        self.assertEqual(state['state']['removed'], 1)
        (self.root / 'saved.png').rename(folder_path / 'back.png')
        self.sync()
        with sqlite3.connect(self.lib.db) as c:
            self.assertEqual(c.execute("SELECT uuid FROM assets WHERE filename='back.png'").fetchone()[0], asset)
        self.assertFalse(self.sentinel.exists())

    def test_native_library_new_folder_application_failure_retry_and_changed_evidence(self):
        image(self.new_folder / 'a.png', 'red')
        state=self.sync()
        self.assertNotEqual(state['state']['status'],'error',state)
        with sqlite3.connect(self.lib.db) as c:
            asset = c.execute("SELECT uuid FROM assets WHERE filename='a.png'").fetchone()[0]
        with sqlite3.connect(self.lib.source) as c:
            c.execute("INSERT INTO labels(uuid,dim,value,conf) VALUES (?,'rating','general',1)", (asset,))
        # Deliver existing synthetic recognition through the same native evidence path.
        result = subprocess.run([str(self.binary), 'library-scan', '--library', str(self.lib.path)], env=self.env, capture_output=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        with sqlite3.connect(self.lib.db) as c:
            c.execute("CREATE TRIGGER fail_import BEFORE INSERT ON assets BEGIN SELECT RAISE(ABORT,'synthetic app failure'); END")
        image(self.new_folder / 'a.png', 'yellow', (51, 31))
        state = self.sync()
        self.assertEqual(state['state']['status'], 'error')
        with sqlite3.connect(self.lib.source) as c:
            self.assertEqual(c.execute('SELECT width FROM assets WHERE uuid=?', (asset,)).fetchone()[0], 51)
            self.assertEqual(c.execute('SELECT count(*) FROM labels WHERE uuid=?', (asset,)).fetchone()[0], 0)
        with sqlite3.connect(self.lib.db) as c:
            self.assertEqual(c.execute('SELECT width FROM assets WHERE uuid=?', (asset,)).fetchone()[0], 40)
            c.execute('DROP TRIGGER fail_import')
        state = self.sync()
        self.assertEqual(state['state']['status'], 'needs_recognition', state)
        with sqlite3.connect(self.lib.db) as c:
            self.assertEqual(c.execute('SELECT width FROM assets WHERE uuid=?', (asset,)).fetchone()[0], 51)
            self.assertEqual(c.execute('SELECT count(*) FROM asset_rating WHERE uuid=?', (asset,)).fetchone()[0], 0)
            self.assertGreater(c.execute("SELECT count(*) FROM term_evidence e JOIN terms t ON t.id=e.term_id WHERE t.kind='rating'").fetchone()[0], 0)
            self.assertGreater(c.execute("SELECT count(*) FROM meta WHERE key LIKE 'folder_stale_wd:%'").fetchone()[0], 0)
        # Same-output recognition is a new successful delivery; immutable old evidence can be reused.
        with sqlite3.connect(self.lib.source) as c:
            c.execute("INSERT INTO labels(uuid,dim,value,conf) VALUES (?,'rating','general',1)", (asset,))
        self.sync()
        with sqlite3.connect(self.lib.db) as c:
            self.assertEqual(c.execute('SELECT rating FROM asset_rating WHERE uuid=?', (asset,)).fetchall(), [('general',)])
        self.assertFalse(self.lib.source.with_suffix('.scan-pending.json').exists())
        self.assertFalse(self.sentinel.exists())

    def test_native_library_new_folder_full_application_typed_rowid_projection_comparison(self):
        clone = self.root / 'reference-clone.pilib'
        clone.mkdir()
        (clone / 'library.json').write_bytes((self.lib.path / 'library.json').read_bytes())
        for name in ('source.sqlite', 'library.sqlite'):
            with sqlite3.connect(self.lib.path / name) as src, sqlite3.connect(clone / name) as dest:
                src.backup(dest)
        image(self.new_folder / 'a.png', 'red')
        image(self.new_folder / 'b.webp', 'blue')
        # Run both independently writing consumers within one real clock second (when recording).
        while RECORD and time.time() % 1 > .1:
            time.sleep(.005)
        state=self.sync()
        self.assertEqual(state['state']['status'],'needs_recognition',state)
        with sqlite3.connect(self.lib.source) as c:
            ids = [u for u, in c.execute('SELECT uuid FROM assets ORDER BY rowid')]
        expected = self.rec.value('old import', lambda: self.old_import(clone, ids, state['history'][0]))
        self.assertEqual(snapshot(self.lib.source),expected['source'])
        # Exact DDL, storage types, physical rowids and every view, including formal projections.
        left,right=snapshot(self.lib.db),expected['db']
        self.assertEqual(set(left),set(right))
        for table in left:
            self.assertEqual(left[table],right[table],table)
        for url in ('/api/assets','/api/ids','/api/home','/api/assets/count'):
            self.assertEqual(self.request(url)['body'],lifecycle.NativeLifecycleTest.reference_response(self, url)['body'])
        self.assertFalse(self.sentinel.exists())

    def old_import(self, clone, ids, run):
        """Recording only: the former Python scanner and importer on a clone of the same input."""
        from web.store import Store
        from web import folder, library, terms_characters
        import hashlib
        old_lib = library.Library(clone, self.lib.config)
        old_store = Store(old_lib.source, old_lib.db)
        with patch.object(folder.uuid,'uuid4',side_effect=ids):
            for source in self.lib.config['sources']:
                folder.scan(source,old_lib.source)
        source = snapshot(old_lib.source)
        with sqlite3.connect(old_lib.db) as c:
            c.execute("INSERT INTO sync_runs(id,status,started_at) VALUES (?,'running',?)",(run['id'],run['started_at']))
        with patch.object(terms_characters,'_now',return_value=run['finished_at']):
            old_store.import_changes(ids,run_id=run['id'])
        with sqlite3.connect(old_lib.source) as src, sqlite3.connect(old_lib.db) as c:
            for u,h in src.execute('SELECT uuid,stable_hash FROM assets'):
                c.execute('INSERT OR REPLACE INTO meta VALUES (?,?)',('folder_labels:'+u,hashlib.sha256(b'[]').hexdigest()))
                c.execute('INSERT OR REPLACE INTO meta VALUES (?,?)',('folder_hash:'+u,h))
            c.execute("UPDATE sync_runs SET status=?,finished_at=?,found=?,removed=?,imported=?,failed=?,sources=?,message=? WHERE id=?",
                      (run['status'],run['finished_at'],run['found'],run['removed'],run['imported'],run['failed'],
                       json.dumps(run['sources'],ensure_ascii=False),run['message'],run['id']))
        return {'source': source, 'db': snapshot(old_lib.db)}

    def test_native_library_new_folder_multiple_sources_keep_configuration_order(self):
        self.stop(self.process)
        self.lib.config['sources'][0]['id']='z'
        second=self.root / 'second-source'
        image(second / 'copy.png','red')
        self.lib.config['sources'].append({'id':'a','kind':'folder','path':str(second),'preset':'other'})
        (self.lib.path / 'library.json').write_text(json.dumps(self.lib.config,ensure_ascii=False))
        self.process,self.log=self.start(self.lib.path)
        self.token=json.loads(self.request('/api/bootstrap')['body'])['token']
        self.test_native_library_new_folder_full_application_typed_rowid_projection_comparison()
        state=json.loads(self.request('/api/sync')['body'])
        self.assertEqual(list(state['history'][0]['sources']),['z','a'])
        self.assertEqual((state['state']['scanned'],state['state']['scan_total']),(4,4))

    def test_native_library_new_folder_shutdown_completed_and_restart_retries(self):
        # Commit-before-close is explicit. Pre-commit cancellation/rollback and join are
        # gated in app_sync::tests::scan_boundary_fixture on the same application service.
        image(self.new_folder / 'a.png', 'red')
        self.assertEqual(self.sync()['state']['status'], 'needs_recognition')
        source, app = snapshot(self.lib.source), snapshot(self.lib.db)
        self.stop(self.process)
        self.assertEqual(snapshot(self.lib.source), source)
        self.assertEqual(snapshot(self.lib.db), app)
        self.process, self.log = self.start(self.lib.path)
        self.token = json.loads(self.request('/api/bootstrap')['body'])['token']
        self.assertEqual(self.sync()['state']['status'], 'needs_recognition')
        self.assertFalse(self.sentinel.exists())

    def test_native_library_new_folder_security_concurrent_switch_and_shutdown(self):
        # A file's size cannot order scan completion against package creation/switch.
        # The pre-commit concurrent cancellation and duplicate start are gated in the
        # actual run_with task owned by Application (app_sync::tests::scan_boundary_fixture).
        image(self.new_folder / 'a.png', 'red')
        self.assertEqual(self.request('/api/sync', {}, token='wrong')['status'], 403)
        self.assertEqual(self.request('/api/sync', {})['status'], 200)
        self.assertEqual(self.finish()['state']['status'], 'needs_recognition')
        source, app = snapshot(self.lib.source), snapshot(self.lib.db)
        old = self.lib
        other, _ = self.package('replacement')
        response = self.request('/api/libraries/open', {'path': str(other.path)})
        self.assertEqual(response['status'], 200, response['body'])
        self.token = json.loads(self.request('/api/bootstrap')['body'])['token']
        state = json.loads(self.request('/api/sync')['body'])
        self.assertEqual(state['state']['status'], 'off')
        self.assertEqual(self.request('/api/sync', {})['status'], 400)
        before=snapshot(other.db)
        refused=subprocess.run([str(self.binary),'library-scan','--library',str(other.path)],env=self.env,capture_output=True)
        self.assertNotEqual(refused.returncode,0)
        self.assertEqual(snapshot(other.db),before)
        # A completed scan retains its exact committed data and terminal history.
        self.assertEqual(snapshot(old.db), app)
        self.assertEqual(snapshot(old.source), source)
        self.assertFalse(self.sentinel.exists())

# Reuse fixtures without rediscovering their unrelated tests.
del NativeLifecycleTest
