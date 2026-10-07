"""R0 HTTP transport and strict comparison tests. All state is disposable synthetic data."""
import gzip
import concurrent.futures
import http.client
import json
import os
from pathlib import Path
import sqlite3
import signal
import socket
import subprocess
import tempfile
import threading
import time
import unittest
from unittest.mock import patch

from PIL import Image
from tests import web_http_fixture as existing
from tools import verify_read_http as verify
from web import library, terms_core
from tests.reference import host

# Windows 没有 SIGINT：宿主放在独立进程组里启动，用 CTRL_BREAK 走它的控制台退出处理。
SPAWN = {'creationflags': subprocess.CREATE_NEW_PROCESS_GROUP} if os.name == 'nt' else {}
INTERRUPT = signal.CTRL_BREAK_EVENT if os.name == 'nt' else signal.SIGINT
from web.server import Handler, Server, publish_http_library, invalidate_http_library

ROOT = Path(__file__).resolve().parents[1]


def free_port():
    with socket.socket() as s:
        s.bind(('127.0.0.1', 0))
        return s.getsockname()[1]


def wait_until(predicate, timeout=15):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        try:
            value = predicate()
            if value:
                return value
        except (OSError, http.client.HTTPException, ValueError):
            pass
        time.sleep(.03)
    raise AssertionError('temporary HTTP service did not reach expected state')


def stop_process(process):
    if process.poll() is None:
        process.send_signal(signal.SIGINT)
        try:
            process.wait(12)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait(3)
    for stream in (process.stdout, process.stderr):
        if stream:
            stream.close()


class FrontDoorHTTPTest(existing.HTTPTest):
    """Run the *existing* complete HTTP/security test bodies through the native front door."""
    def setUp(self):
        super().setUp()
        self.direct = self.url
        self.url = f'http://127.0.0.1:{free_port()}'
        self.server.origin = self.url
        self.server.host_header = self.url.split('://')[1]
        self.log = tempfile.TemporaryFile()
        self.proxy = subprocess.Popen([str(host.binary()), 'http', '--port', self.url.rsplit(':', 1)[1],
                                       '--upstream', str(self.server.server_port), '--route', 'GET:/api/fold=forward'], stdout=self.log, stderr=self.log)
        self.addCleanup(self.log.close)
        self.addCleanup(stop_process, self.proxy)
        wait_until(lambda: verify.fetch(self.url, {'url': '/api/fold'}, self.url)['status'] == 200)

    def tearDown(self):
        stop_process(self.proxy)
        super().tearDown()

    def pair(self, path, method='GET', body=None, headers=None):
        item = {'url': path, 'method': method, 'headers': headers or {}}
        if body is not None:
            item['body'] = json.dumps(body)
            item['headers'] = {'Content-Type': 'application/json', 'X-CSRF-Token': self.server.token, **item['headers']}
        a, b = verify.fetch(self.direct, item, self.url), verify.fetch(self.url, item, self.url)
        self.assertIsNone(verify.difference(a, b), (path, verify.difference(a, b)))
        self.assertEqual(b['route'], 'forward')
        for header in ('Content-Type', 'Content-Encoding', 'Vary', 'Cache-Control', 'X-Content-Type-Options',
                       'Referrer-Policy', 'Content-Security-Policy', 'Content-Length'):
            self.assertEqual(a['headers'].get(header), b['headers'].get(header), header)
        return a, b

    def test_raw_corpus_same_database_and_core_gate(self):
        requests = verify.corpus(self.store.path, sample=1, query_limit=12)
        # Persist and reload the corpus, just as later R1–R4 runs do.
        path = Path(self.temp.name) / 'requests.json'
        path.write_text(json.dumps(requests, ensure_ascii=False))
        requests = json.loads(path.read_text())
        report = verify.compare(self.direct, self.url, self.url, requests)
        self.assertEqual(report['differences'], [], report['differences'])
        self.assertEqual(report['compared'], report['forward_equal'])
        self.assertEqual(report['core_equal'], 0)
        self.assertEqual(set(report['stages']), {'R1', 'R2', 'R3', 'R4'})
        report = verify.compare(self.direct, self.url, self.url, [requests[0]], require_core=['R1'])
        self.assertIn('routing', report['differences'][0])
        self.assertTrue(verify.compare(self.direct, self.url, self.url, [], ['R1'])['differences'])

    def test_single_response_byte_mutation_is_detected_over_http(self):
        original = Handler.respond
        def mutated(handler, payload, *args, **kwargs):
            if handler.path == '/api/fold' and handler.request_version == 'HTTP/1.0':
                payload = json.dumps(payload, ensure_ascii=False).encode()
                offset = payload.index(b' ')
                payload = payload[:offset] + b'\t' + payload[offset + 1:]
            return original(handler, payload, *args, **kwargs)
        with patch.object(Handler, 'respond', mutated):
            report = verify.compare(self.direct, self.url, self.url, [{'stage': 'R3', 'url': '/api/fold'}])
        self.assertEqual(len(report['differences']), 1)
        diff = report['differences'][0]
        self.assertEqual(diff['offset'], 9)
        self.assertTrue(diff['diagnostic_only_json_equal'])
        self.assertEqual(report['forward_equal'], 0)
        self.pair('/api/fold')  # restored, no production database or golden file changed

    def test_raw_security_errors_token_readonly_and_unknown_route(self):
        for headers in ({'Host': 'evil.example'}, {'Origin': 'https://evil.example'}):
            self.assertEqual(self.pair('/api/bootstrap', headers=headers)[0]['status'], 403)
        self.assertEqual(self.pair('/api/change', 'POST', {'action': 'unknown', 'ids': ['a']},
                                   {'X-CSRF-Token': 'bad'})[0]['status'], 403)
        self.store.refuse_writes('合成库只读')
        self.assertEqual(self.pair('/api/change', 'POST', {'action': 'unknown', 'ids': ['a']})[0]['status'], 409)
        a, _ = self.pair('/api/bootstrap')
        self.assertEqual(json.loads(a['body'])['token'], self.server.token)
        self.pair('/api/assets?limit=bad')
        self.pair('/missing-route')
        self.pair('/')
        self.pair('/app.js')
        self.pair('/image?id=a')
        self.pair('/api/fold', 'PUT')

    def test_post_framing_rejections_are_preserved(self):
        for side, base in (('python', self.direct), ('front', self.url)):
          with self.subTest(side=side):
            # The server rejects on the headers and closes without reading the body: the client may still be writing
            # (broken pipe), or lose the answer to the reset. Read the answer anyway; resend only if it was lost.
            for attempt in range(5):
                conn = http.client.HTTPConnection('127.0.0.1', int(base.rsplit(':', 1)[1]), timeout=3)
                try:
                    try:
                        conn.request('POST', '/api/fold', body=iter([b'{"hidden":true}']), encode_chunked=True,
                                     headers={'Host': self.server.host_header, 'Origin': self.url,
                                              'Content-Type': 'application/json', 'X-CSRF-Token': self.server.token})
                    except (BrokenPipeError, ConnectionResetError):
                        pass
                    response = conn.getresponse()
                    status, body = response.status, response.read()
                    break
                except (ConnectionResetError, http.client.RemoteDisconnected):
                    if attempt == 4:
                        raise
                finally:
                    conn.close()
            self.assertEqual(status, 400)
            self.assertEqual(json.loads(body), {'error': '请求格式不正确'})
        for attempt in range(5):   # the same early rejection, on Content-Length: resend when the reset ate the answer
            try:
                large = self.pair('/api/fold', 'POST', {'large': 'x' * 100001})
                break
            except (BrokenPipeError, ConnectionResetError, http.client.RemoteDisconnected):
                if attempt == 4:
                    raise
        self.assertEqual(large[0]['status'], 400)

    def test_gzip_response_is_not_decoded_by_proxy(self):
        def get(base):
            conn = http.client.HTTPConnection('127.0.0.1', int(base.rsplit(':', 1)[1]))
            conn.request('GET', '/api/characters', headers={'Host': self.server.host_header, 'Accept-Encoding': 'gzip'})
            response = conn.getresponse()
            out = response.getheader('Content-Encoding'), response.read()
            conn.close()
            return out
        with patch.object(self.store, 'characters', return_value=[{'name': '合成' * 1000}]):
            a, b = get(self.direct), get(self.url)
        self.assertEqual(a[0], 'gzip')
        self.assertEqual(b[0], 'gzip')
        self.assertEqual(gzip.decompress(a[1]), gzip.decompress(b[1]))

    def test_image_forward_does_not_block_json(self):
        entered, release = threading.Event(), threading.Event()
        original = Handler.image
        def slow(handler, *args):
            entered.set()
            if not release.wait(5):
                raise AssertionError('JSON never ran concurrently')
            return original(handler, *args)
        with patch.object(Handler, 'image', slow):
            with concurrent.futures.ThreadPoolExecutor() as pool:
                future = pool.submit(verify.fetch, self.url, {'url': '/image?id=a'}, self.url)
                self.assertTrue(entered.wait(3))
                try:
                    self.assertEqual(verify.fetch(self.url, {'url': '/api/fold'}, self.url, timeout=2)['status'], 200)
                    self.assertFalse(future.done())
                finally:
                    release.set()
                self.assertEqual(future.result(3)['status'], 200)

    def test_python_restart_recovers_same_internal_port(self):
        port = self.server.server_port
        self.server.shutdown()
        self.thread.join()
        self.server.server_close()
        unavailable = verify.fetch(self.url, {'url': '/api/fold'}, self.url)
        self.assertEqual(unavailable['status'], 502)
        self.assertEqual(unavailable['route'], 'unavailable')
        self.assertEqual(json.loads(unavailable['body']), {'error': 'Python 服务暂不可用'})
        self.server = Server(('127.0.0.1', port), self.store, Path(self.temp.name) / 'cache', public_origin=self.url)
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()
        a, _ = self.pair('/api/bootstrap')
        self.assertEqual(json.loads(a['body'])['token'], self.server.token)


class ComparatorTest(unittest.TestCase):
    def test_deterministic_corpus_excludes_random_ask(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            store = existing.Store(existing.fixture(root), root / 'app.sqlite')
            store.refresh()
            requests = verify.corpus(store.path, sample=1, query_limit=0)
            self.assertTrue(requests)
            self.assertFalse(any(item['url'].split('?', 1)[0] == '/api/ask' for item in requests))
            self.assertEqual({item['stage'] for item in requests}, {'R1', 'R2', 'R3', 'R4'})

    def test_custom_random_ask_is_explicit_failure_not_raw_comparison(self):
        for url in ('/api/ask', '/api/ask?nsfw=hide&skip=alice'):
            with self.subTest(url=url), patch.object(verify, 'fetch') as fetcher:
                report = verify.compare('http://127.0.0.1:41001', 'http://127.0.0.1:41002',
                                        'http://127.0.0.1:41002', [{'stage': 'R4', 'url': url}])
                self.assertEqual(report['differ'], 1)
                self.assertIn('候选范围与约束', report['differences'][0]['error'])
                self.assertEqual(report['core_equal'], 0)
                self.assertEqual(report['forward_equal'], 0)
                fetcher.assert_not_called()

    def test_every_raw_json_spelling_is_significant(self):
        for a, b in ((b'{"x":1}', b'{"x": 1}'), (b'{"a":1,"b":2}', b'{"b":2,"a":1}'),
                     (b'{"x":1.0}', b'{"x":1}'), (b'[]', b'[]\n'), (b'[]', b'[')):
            self.assertIsNotNone(verify.difference({'status': 200, 'body': a}, {'status': 200, 'body': b}))
        self.assertIsNotNone(verify.difference({'status': 200, 'body': b'[]'}, {'status': 400, 'body': b'[]'}))

    def test_non_temporary_database_refused_before_open(self):
        with patch.object(sqlite3, 'connect', side_effect=AssertionError('opened DB')):
            with self.assertRaises(ValueError):
                verify.corpus(Path('/__illustash_synthetic_non_temporary__') / 'do-not-read.sqlite')

    def test_private_state_replaced_and_removed(self):
        with tempfile.TemporaryDirectory() as root:
            state = Path(root) / 'state.json'
            class Context:
                library = None
                token = 'synthetic-token'
                store = type('Store', (), {'path': Path(root) / 'one.sqlite', 'source': Path(root) / 'source.sqlite', 'read_only': None})()
            with patch.dict(os.environ, {'ILLUSTASH_HTTP_STATE': str(state)}):
                publish_http_library(Context)
                a = json.loads(state.read_text())
                self.assertEqual(state.stat().st_mode & 0o777, 0o600)
                Context.store.path = Path(root) / 'two.sqlite'
                publish_http_library(Context)
                b = json.loads(state.read_text())
                self.assertNotEqual(a['generation'], b['generation'])
                self.assertNotEqual(a['db'], b['db'])
                invalidate_http_library()
                self.assertFalse(state.exists())


class LifecycleTest(unittest.TestCase):
    def test_loopback_bind_does_not_resolve_hostname(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            store = existing.Store(existing.fixture(root), root / 'app.sqlite')
            store.refresh()
            with patch.object(socket, 'getfqdn', side_effect=AssertionError('loopback DNS must not block readiness')) as lookup:
                server = Server(('127.0.0.1', 0), store, root / 'cache')
                try:
                    self.assertEqual(server.server_name, '127.0.0.1')
                    self.assertEqual(server.server_port, server.socket.getsockname()[1])
                    self.assertGreater(server.server_port, 0)
                    lookup.assert_not_called()
                finally:
                    server.server_close()

    def test_cli_launch_library_switch_token_restart_and_clean_exit(self):
        from web.store import Store
        from tests.synthetic_source import source_fixture as fixture
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            packages = []
            for name, count in (('one', 3), ('two', 4)):
                package = library.create(root / (name + '.pilib'), name=name, test={'synthetic': True})
                source = fixture(package.path)
                source.rename(package.source)
                if count == 4:
                    import sqlite3
                    with sqlite3.connect(package.source) as c:
                        c.execute("INSERT INTO assets VALUES ('extra','extra.png','',120,150,'2026-09-22','orig',1,0)")
                store = Store(package.source, package.db)
                store.refresh()
                store.tag(['a'], name)
                packages.append(package)
            port = free_port()
            origin = f'http://127.0.0.1:{port}'
            env = {**os.environ, 'ILLUSTASH_APP_SUPPORT': str(root / 'app-support'),
                   'ILLUSTASH_SHARED': str(root / 'shared'), 'ILLUSTASH_TERMS_BIN': str(terms_core.binary()),
                   'ILLUSTASH_HTTP_STATE': str(root / 'unused-external-state')}
            with tempfile.TemporaryFile() as log:
                process = subprocess.Popen([str(host.binary()), 'http', '--root', str(ROOT),
                    '--python', os.sys.executable, f'--port={port}', '--library', str(packages[0].path),
                    '--no-open', '--no-names', '--no-sync', '--no-similar', '--no-index-check'],
                    stdout=log, stderr=log, env=env)
                try:
                    def ready():
                        response = verify.fetch(origin, {'url': '/api/bootstrap'}, origin)
                        return response if response['status'] == 200 else None
                    first = wait_until(ready)
                    first_data = json.loads(first['body'])
                    self.assertEqual(first_data['total'], 3)
                    first_tags=verify.fetch(origin, {'url':'/api/user-tags'},origin)
                    self.assertEqual(first_tags['route'],'core')
                    self.assertEqual(json.loads(first_tags['body']),[{'tag':'one','count':1}])
                    # --backup remains usable while this front door owns the public port.
                    backup = subprocess.run([str(host.binary()), 'http', '--root', str(ROOT),
                        '--python', os.sys.executable, '--port', str(port), '--library', str(packages[0].path),
                        '--backup'], env=env, capture_output=True, timeout=15)
                    self.assertEqual(backup.returncode, 0, backup.stderr)
                    old_token = first_data['token']
                    switched = verify.fetch(origin, {'method': 'POST', 'url': '/api/libraries/open',
                        'body': json.dumps({'path': str(packages[1].path)}),
                        'headers': {'Content-Type': 'application/json', 'X-CSRF-Token': old_token}}, origin)
                    self.assertEqual(switched['status'], 200, switched['body'])
                    def new_library():
                        response = verify.fetch(origin, {'url': '/api/bootstrap'}, origin)
                        if response['status'] == 200:
                            data = json.loads(response['body'])
                            return data if data['total'] == 4 else None
                    second = wait_until(new_library)
                    self.assertNotEqual(second['token'], old_token)
                    second_tags=verify.fetch(origin, {'url':'/api/user-tags'},origin)
                    self.assertEqual(second_tags['route'],'core')
                    self.assertEqual(json.loads(second_tags['body']),[{'tag':'two','count':1}])
                    expired = verify.fetch(origin, {'method': 'POST', 'url': '/api/change',
                        'body': '{"action":"unknown","ids":["extra"]}',
                        'headers': {'Content-Type': 'application/json', 'X-CSRF-Token': old_token}}, origin)
                    self.assertEqual(expired['status'], 403)
                    self.assertEqual(json.loads(expired['body']), {'error': '会话已过期，请刷新页面'})
                    fresh = verify.fetch(origin, {'method': 'POST', 'url': '/api/change',
                        'body': '{"action":"unknown","ids":["extra"]}',
                        'headers': {'Content-Type': 'application/json', 'X-CSRF-Token': second['token']}}, origin)
                    self.assertEqual(fresh['status'], 200, fresh['body'])
                    self.assertTrue(json.loads(verify.fetch(origin, {'url': '/api/detail?id=extra'}, origin)['body'])['unknown'])
                except Exception:
                    log.seek(0)
                    print(log.read().decode(errors='replace')[-4000:])
                    raise
                finally:
                    stop_process(process)
                self.assertEqual(process.returncode, 0)
                with socket.socket() as probe:
                    probe.bind(('127.0.0.1', port))

    def test_occupied_port_does_not_start_python_or_kill_owner(self):
        with socket.socket() as listener:
            listener.bind(('127.0.0.1', 0))
            listener.listen()
            done = subprocess.run([str(host.binary()), 'http', '--port', str(listener.getsockname()[1]),
                                   '--python', '/definitely/not/a/python'], capture_output=True, timeout=3)
            self.assertNotEqual(done.returncode, 0)
            self.assertNotIn(b'definitely', done.stderr)

    def test_missing_python_exits_and_releases_public_port(self):
        port = free_port()
        done = subprocess.run([str(host.binary()), 'http', '--port', str(port),
                               '--python', '/definitely/not/a/python'], capture_output=True, timeout=3)
        self.assertNotEqual(done.returncode, 0)
        with socket.socket() as probe:
            probe.bind(('127.0.0.1', port))

    def test_route_core_is_rejected_without_false_migration_claim(self):
        done = subprocess.run([str(host.binary()), 'http', '--route', 'GET:/api/local-links=core'],
                              capture_output=True, timeout=3)
        self.assertEqual(done.returncode, 2)
        self.assertIn('尚未迁移'.encode(), done.stderr)

    @unittest.skipUnless(os.name == 'posix', 'desktop parent watch uses the existing Unix lifecycle')
    def test_desktop_parent_loss_stops_core_and_python(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            worker = root / 'synthetic-python'
            stopped, ready, pidfile = root / 'stopped', root / 'ready', root / 'core-pid'
            worker.write_text(f'''#!{os.sys.executable}
import signal, time
from pathlib import Path
signal.signal(signal.SIGINT, lambda *_: (Path({str(stopped)!r}).write_text('stopped'), exit(0)))
Path({str(ready)!r}).write_text('ready')
while True: time.sleep(.1)
''')
            worker.chmod(0o700)
            port = free_port()
            args = [str(host.binary()), 'http', '--root', str(root), '--python', str(worker), '--port', str(port)]
            env = {**os.environ, 'ILLUSTASH_DESKTOP_BRIDGE': 'http://127.0.0.1:1'}
            wrapper = f'''import subprocess
from pathlib import Path
child = subprocess.Popen({args!r})
Path({str(pidfile)!r}).write_text(str(child.pid))
child.wait()
'''
            with tempfile.TemporaryFile() as log:
                parent = subprocess.Popen([os.sys.executable, '-c', wrapper], env=env, stdout=log, stderr=log)
                core_pid = None
                try:
                    wait_until(lambda: ready.exists() and pidfile.exists())
                    core_pid = int(pidfile.read_text())
                    parent.kill()
                    parent.wait(3)
                    wait_until(stopped.exists)
                    def port_closed():
                        with socket.socket() as connection:
                            return connection.connect_ex(('127.0.0.1', port)) != 0
                    wait_until(port_closed)
                finally:
                    stop_process(parent)
                    if core_pid:
                        try:
                            os.kill(core_pid, signal.SIGINT)
                        except ProcessLookupError:
                            pass
