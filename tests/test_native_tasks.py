"""Shared task consumer acceptance using owned synthetic packages only."""
import json
import sqlite3
import time
import unittest
from web import library
from tests.test_native_folder import NativeFolderHTTPTest


class NativeTasksHTTPTest(unittest.TestCase):
    setUpClass = classmethod(NativeFolderHTTPTest.setUpClass.__func__)
    _fixture_setup = NativeFolderHTTPTest._fixture_setup
    setUp = NativeFolderHTTPTest.setUp
    package = NativeFolderHTTPTest.package
    stop = NativeFolderHTTPTest.stop
    start = NativeFolderHTTPTest.start
    request = NativeFolderHTTPTest.request
    finish = NativeFolderHTTPTest.finish
    sync = NativeFolderHTTPTest.sync

    def running(self):
        until = time.monotonic() + 3
        while time.monotonic() < until:
            with sqlite3.connect(self.lib.db) as c:
                if c.execute("SELECT count(*) FROM sync_runs WHERE status='running'").fetchone()[0]:
                    return
            time.sleep(.001)
        self.fail('task did not persist running record')

    def test_native_library_new_tasks_same_id_different_package(self):
        replacement = self.root / 'same-id.pilib'
        replacement.mkdir()
        config = dict(self.lib.config)
        folder = self.root / 'new-source'; folder.mkdir()
        config['sources'] = [dict(config['sources'][0], path=str(folder))]
        (replacement / 'library.json').write_text(json.dumps(config))
        for name in ('source.sqlite', 'library.sqlite'):
            with sqlite3.connect(self.lib.path / name) as src, sqlite3.connect(replacement / name) as dest:
                src.backup(dest)
        other = library.Library(replacement, config)
        self.assertEqual(other.config["id"], self.lib.config["id"])
        large = self.new_folder / 'large.png'
        with large.open('wb') as f: f.truncate(256 * 1024 * 1024)
        self.assertEqual(self.request('/api/sync', {})['status'], 200)
        self.running()
        old_token = self.token
        self.assertEqual(self.request('/api/libraries/open', {'path': str(replacement)})['status'], 200)
        self.token = json.loads(self.request('/api/bootstrap')['body'])['token']
        self.assertEqual(json.loads(self.request('/api/sync')['body'])['state'], {'status': 'idle'})
        self.assertEqual(self.request('/api/sync', {}, token=old_token)['status'], 403)
        with sqlite3.connect(self.lib.db) as c:
            self.assertEqual(c.execute('SELECT status FROM sync_runs ORDER BY id DESC LIMIT 1').fetchone()[0], 'interrupted')
        with sqlite3.connect(other.db) as c:
            self.assertEqual(c.execute('SELECT count(*) FROM sync_runs').fetchone()[0], 0)
            self.assertEqual(c.execute('SELECT count(*) FROM assets').fetchone()[0], 0)
        self.assertEqual(self.sync()['state']['status'], 'done')
        self.assertFalse(self.sentinel.exists())

    def test_native_library_new_tasks_hard_crash_recovery(self):
        large = self.new_folder / 'large.png'
        with large.open('wb') as f: f.truncate(256 * 1024 * 1024)
        self.assertEqual(self.request('/api/sync', {})['status'], 200)
        self.running()
        self.process.kill(); self.process.wait(3)
        with sqlite3.connect(self.lib.db) as c:
            self.assertEqual(c.execute('SELECT status FROM sync_runs ORDER BY id DESC LIMIT 1').fetchone()[0], 'running')
        large.unlink()
        self.process, self.log = self.start(self.lib.path)
        self.token = json.loads(self.request('/api/bootstrap')['body'])['token']
        self.assertEqual(self.sync()['state']['status'], 'needs_recognition')
        with sqlite3.connect(self.lib.db) as c:
            self.assertEqual(c.execute('SELECT status FROM sync_runs ORDER BY id').fetchall(), [('interrupted',), ('needs_recognition',)])
        self.assertFalse(self.lib.source.with_suffix('.scan-pending.json').exists())
        self.assertFalse(self.sentinel.exists())


del NativeFolderHTTPTest
