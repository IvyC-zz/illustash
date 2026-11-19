"""WD 扩展包走原生 HTTP 与同步入口，包、图片和底座指纹均为合成数据。"""
from contextlib import closing, contextmanager
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import shutil
from pathlib import Path
import sqlite3
import struct
import subprocess
import tempfile
from threading import Thread
import unittest

from tests.folder_fixture import image
from tests.fixed_expectations import Library
from tests import test_native_lifecycle as lifecycle, test_native_sync as sync_fixture
from tests.test_read_http import free_port, wait_until
from tools.dev_preview import isolated_env
from tools.wait_limit import WAIT
from tools.ui_walk.fixture import wd_extension_spec

ROUTE = '/api/wd-extension'
FILENAME = 'wd-extension-v1.bin'


@contextmanager
def download_server(control):
    class Handler(BaseHTTPRequestHandler):
        def do_GET(self):
            data = control['bytes']
            range_header = self.headers.get('Range')
            control['ranges'].append(range_header)
            start = int(range_header.removeprefix('bytes=').removesuffix('-')) if range_header else 0
            self.send_response(206 if start else 200)
            if start:
                self.send_header('Content-Range', f'bytes {start}-{len(data)-1}/{len(data)}')
            self.send_header('Content-Length', str(len(data) - start))
            self.end_headers()
            self.wfile.write(data[start:])

        def log_message(self, *_):
            pass

    with ThreadingHTTPServer(('127.0.0.1', 0), Handler) as server:
        thread = Thread(target=server.serve_forever)
        thread.start()
        try:
            yield f'http://127.0.0.1:{server.server_port}'
        finally:
            server.shutdown()
            thread.join()


class SyntheticWdLibrary:
    setUpClass = classmethod(lifecycle.NativeLifecycleTest.setUpClass.__func__)
    start = lifecycle.NativeLifecycleTest.start
    stop = lifecycle.NativeLifecycleTest.stop
    request = lifecycle.NativeLifecycleTest.request

    def setUp(self):
        temp = tempfile.TemporaryDirectory(prefix='illustash-wd-extension-')
        self.addCleanup(temp.cleanup)
        self.root = Path(temp.name).resolve()
        checkout = self.resources
        self.resources = self.root / 'resources'
        (self.resources / 'tags').mkdir(parents=True)
        shutil.copyfile(checkout / 'tags' / 'tags_v3.csv', self.resources / 'tags' / 'tags_v3.csv')
        self.env = {**isolated_env(self.root), 'USERPROFILE': str(self.root / 'home')}
        self.new_folder = self.root / 'images'
        self.new_folder.mkdir()
        image(self.new_folder / 'first.png', 'blue')
        self.lib = self.new_library('first', self.new_folder)
        self.cli('library-scan', '--library', str(self.lib.path))
        self.port = free_port()
        self.origin = f'http://127.0.0.1:{self.port}'
        self.restart()

    def new_library(self, name, folder):
        path = self.root / (name + '.pilib')
        self.cli('library-create', '--dest', str(path), '--folder', str(folder))
        return Library(path)

    def cli(self, *arguments):
        result = subprocess.run([str(self.binary), *arguments], env=self.env, capture_output=True, timeout=WAIT)
        self.assertEqual(result.returncode, 0, result.stderr.decode(errors='replace'))

    def state(self, action=None, body=None):
        response = self.request(ROUTE + (action or ''), body)
        self.assertEqual(response['status'], 200, response['body'])
        return json.loads(response['body'])

    def finish(self, kind, *, key='task'):
        observed = None

        def ended():
            nonlocal observed
            observed = self.state()
            task = observed[key]
            return task and task['kind'] == kind and task['status'] != 'running'

        wait_until(ended)
        return observed

    def fixture(self, address, *, tags=b'synthetic tags'):
        # 两个命中让无归属图得到一个主归属及一个可见的扩展候选。
        head = [2.] + [0.] * 1023
        self.manifest = wd_extension_spec(self.root / 'download', tags, [
            ('synthetic_extension_primary', head, 0.),
            ('synthetic_extension_alternate', head, -.3),
        ], address)
        self.env['ILLUSTASH_TEST_WD_EXTENSION'] = str(self.manifest)
        return self.manifest.with_name(FILENAME)

    def restart(self):
        if hasattr(self, 'process'):
            self.stop(self.process)
        self.process, self.log = self.start(self.lib.path)
        self.token = json.loads(self.request('/api/bootstrap')['body'])['token']


class NativeWdExtensionTest(SyntheticWdLibrary, unittest.TestCase):
    def test_download_resume_import_enable_and_withdraw(self):
        control = {'ranges': []}
        with download_server(control) as address:
            package = self.fixture(address)
            raw = package.read_bytes()
            self.restart()
            state = self.state()
            self.assertEqual((state['package']['status'], state['library']['blocked']), ('missing', 'no_vectors'))
            self.assertEqual(control['ranges'], [])
            self.assertEqual(self.request(ROUTE + '/enable', {})['status'], 409)
            vector = struct.pack('<1024f', 1., *([0.] * 1023))
            with sqlite3.connect(self.lib.source) as source:
                source.execute('CREATE TABLE IF NOT EXISTS embeddings(uuid TEXT,model TEXT,vec BLOB,PRIMARY KEY(uuid,model))')
                source.execute("INSERT OR REPLACE INTO embeddings SELECT uuid,'wd',? FROM assets WHERE in_scope=1", (vector,))
            with sqlite3.connect(self.lib.db) as app:
                ids = [u for u, in app.execute('SELECT uuid FROM assets ORDER BY uuid')]
            detail = lambda u: self.request('/api/detail?id=' + u)['body']
            before = {u: detail(u) for u in ids}
            created = self.request('/api/entities', {'name': 'Synthetic extension manual'})
            self.assertEqual(created['status'], 200, created['body'])
            manual_id = json.loads(created['body'])['id']

            def search_roles():
                result = self.request('/api/entities?q=synthetic%20extension')
                self.assertEqual(result['status'], 200, result['body'])
                return {item['id']: item['count'] for item in json.loads(result['body'])}

            self.assertEqual(search_roles(), {manual_id: 0})
            expected = self.state()['library']['vectors']
            self.assertGreater(expected, 0)
            self.assertEqual(self.request(ROUTE + '/enable', {})['status'], 409)
            self.assertEqual(self.request(ROUTE + '/enable', {'unknown': 1})['status'], 400)
            self.assertEqual(self.request(ROUTE + '/package', {'action': 'download'}, token='invalid')['status'], 403)

            bad = bytes([raw[0] ^ 1]) + raw[1:]
            control['bytes'] = bad
            self.state('/package', {'action': 'download'})
            failed = self.finish('download', key='download')
            self.assertEqual(failed['download']['status'], 'failed', failed)
            self.assertEqual(failed['package']['status'], 'missing')
            self.assertTrue(failed['download']['failures'])
            self.assertTrue(failed['download']['diagnostic'])
            control['bytes'] = raw
            partial = Path(state['package']['location'] + '.partial')
            partial.mkdir(exist_ok=True, parents=True)
            part_bytes = 53
            (partial / (FILENAME + '.part')).write_bytes(raw[:part_bytes])
            self.assertEqual(self.state()['package']['have'], part_bytes)
            self.state('/package', {'action': 'download'})
            self.assertEqual(self.finish('download', key='download')['package']['status'], 'ready')
            self.assertEqual(control['ranges'][-1], f'bytes={part_bytes}-')

            self.state('/package', {'action': 'delete'})
            renamed = self.root / 'renamed-asset.data'
            renamed.write_bytes(bad)
            self.state('/package', {'action': 'import', 'paths': [str(renamed)]})
            imported = self.finish('import', key='download')
            self.assertEqual(imported['download']['results'][0]['result'], 'mismatch', imported)
            renamed.write_bytes(raw)
            self.state('/package', {'action': 'import', 'paths': [str(renamed)]})
            imported = self.finish('import', key='download')
            self.assertEqual(imported['download']['results'][0]['result'], 'accepted', imported)

            self.assertEqual(self.state('/enable', {})['task']['status'], 'running')
            enabled = self.finish('enable')
            self.assertEqual(enabled['task']['status'], 'done', enabled)
            self.assertGreater(enabled['task']['result']['changed'], 0)
            self.assertEqual(enabled['library']['scored'], expected)
            self.assertRegex(enabled['library']['checked_at'], r'^\d{4}-\d\d-\d\d \d\d:\d\d:\d\d$')
            self.assertTrue(enabled['library']['enabled'])
            searchable = search_roles()
            self.assertEqual(len(searchable), 3)
            self.assertEqual(searchable[manual_id], 0)
            self.assertIn(0, [count for role, count in searchable.items() if role != manual_id])
            candidates = [
                alt['evidence'] for u in ids for alt in json.loads(detail(u))['alternatives']
                if alt['evidence']['sources'] == ['WD_EXT']
            ]
            self.assertGreater(len(candidates), 0)
            self.assertTrue(all(c['wd'] is None and c['ccip'] is None for c in candidates))
            second_folder = self.root / 'second-images'
            second_folder.mkdir()
            second = self.new_library('second', second_folder)
            self.assertEqual(self.request('/api/libraries/open', {'path': str(second.path)})['status'], 200)
            self.token = json.loads(self.request('/api/bootstrap')['body'])['token']
            self.assertIsNone(self.state()['task'])
            self.assertFalse(self.state()['library']['enabled'])
            self.assertEqual(self.request('/api/libraries/open', {'path': str(self.lib.path)})['status'], 200)
            self.token = json.loads(self.request('/api/bootstrap')['body'])['token']
            self.assertEqual(self.state()['task']['id'], enabled['task']['id'])
            self.state('/package', {'action': 'delete'})
            self.assertTrue(self.state()['library']['enabled'])
            self.restart()
            self.assertEqual(self.state()['library'], enabled['library'])
            self.assertIsNone(self.state()['task'])
            self.state('/withdraw', {})
            reverted = self.finish('withdraw')
            self.assertEqual(reverted['task']['status'], 'done', reverted)
            self.assertFalse(reverted['library']['enabled'])
            self.assertEqual({u: detail(u) for u in ids}, before)
            self.assertEqual(search_roles(), {manual_id: 0})
            with sqlite3.connect(self.lib.db) as app:
                for role in searchable:
                    self.assertIsNotNone(app.execute('SELECT 1 FROM term_records WHERE key=?', (role,)).fetchone())
            self.restart()
            self.assertEqual(search_roles(), {manual_id: 0})
            self.state('/withdraw', {})
            self.assertEqual(self.finish('withdraw')['task']['result']['changed'], 0)


class NativeWdExtensionSyncTest(SyntheticWdLibrary, unittest.TestCase):
    worker = sync_fixture.NativeSyncTest.worker

    def test_sync_scores_each_batch_and_preserves_committed_results_after_cancellation(self):
        def sync_state():
            return json.loads(self.request('/api/sync')['body'])['state']

        def candidates():
            with closing(sqlite3.connect(self.lib.db)) as db:
                return db.execute(
                    "SELECT uuid,entity,score FROM candidates WHERE source='WD_EXT' ORDER BY uuid,entity"
                ).fetchall()

        self.worker()
        self.assertEqual(self.request('/api/sync', {})['status'], 200)
        wait_until(lambda: sync_state()['status'] == 'done')
        package = self.fixture('http://127.0.0.1:1', tags=(self.resources / 'tags' / 'tags_v3.csv').read_bytes())
        self.restart()
        self.state('/package', {'action': 'import', 'paths': [str(package)]})
        self.assertEqual(self.finish('import', key='download')['package']['status'], 'ready')
        self.state('/enable', {})
        manual = self.finish('enable')
        self.assertEqual(manual['task']['status'], 'done', manual)
        self.assertEqual(manual['library']['scored'], 1)
        before = candidates()

        for i, color in enumerate(('orange', 'green', 'purple')):
            image(self.new_folder / f'auto-{i}.png', color)
        self.worker('stream-gated')
        (self.root / 'last-request').unlink()
        self.assertEqual(self.request('/api/sync', {})['status'], 200)
        request = self.root / 'last-request'
        wait_until(request.exists)
        ids = json.loads(request.read_text())
        self.assertEqual(len(ids), 3)
        for i in range(2):
            (self.root / f'release-batch-{i}').touch()
            wait_until(lambda: sync_state().get('import_done') == i + 1)
            self.assertEqual(sync_state()['status'], 'recognising')
            self.assertEqual(self.state()['library']['scored'], manual['library']['scored'] + i + 1)
            self.assertEqual({u for u, _, _ in candidates()}, {u for u, _, _ in before} | set(ids[:i + 1]))
        committed = candidates()

        # 第三批没有交付，停止宿主，确认重新打开后已交付的候选和计数都保留。
        self.worker('stream')
        self.assertEqual(candidates(), committed)
        self.assertEqual(self.state()['library']['scored'], 3)
        request.unlink()
        self.assertEqual(self.request('/api/sync', {})['status'], 200)
        wait_until(lambda: sync_state()['status'] == 'done')
        self.assertEqual(json.loads(request.read_text()), ids[2:])
        self.assertEqual(self.state()['library']['scored'], 4)
        self.assertEqual(self.state()['library']['checked_at'], manual['library']['checked_at'])

        # 用同一包重新检查全部图，批次路径和原来整库路径得出同样的角色和分数。
        streamed = candidates()
        self.state('/enable', {})
        self.assertEqual(self.finish('enable')['task']['status'], 'done')
        self.assertEqual(candidates(), streamed)
