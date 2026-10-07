"""#109: authenticated synthetic desktop dialogs, no real filesystem picker or library."""
import http.server
import json
import threading
import unittest
from concurrent.futures import ThreadPoolExecutor
from tests import test_native_lifecycle as lifecycle


class Shell(http.server.BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def do_POST(self):
        self.server.calls.append((self.path, self.headers.get('X-Illustash-Token')))
        self.server.entered.set()
        self.server.release.wait(8)
        data = json.dumps(self.server.reply).encode()
        self.send_response(200)
        self.send_header('Content-Length', str(len(data)))
        self.end_headers()
        try:
            self.wfile.write(data)
        except BrokenPipeError:  # host exit while a synthetic picker is waiting
            pass


class NativePlatformPickerTest(unittest.TestCase):
    setUpClass = classmethod(lifecycle.NativeLifecycleTest.setUpClass.__func__)
    package = lifecycle.NativeLifecycleTest.package
    stop = lifecycle.NativeLifecycleTest.stop
    start = lifecycle.NativeLifecycleTest.start
    request = lifecycle.NativeLifecycleTest.request
    def setUp(self):
        lifecycle.NativeLifecycleTest.setUp(self)
        self.stop(self.process)
        self.shell = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Shell)
        self.shell.calls = []
        self.shell.reply = {'paths': []}
        self.shell.entered = threading.Event()
        self.shell.release = threading.Event()
        self.shell.release.set()
        threading.Thread(target=self.shell.serve_forever, daemon=True).start()
        self.addCleanup(self.shell.server_close)
        self.addCleanup(self.shell.shutdown)
        self.addCleanup(self.shell.release.set)
        self.env.update(ILLUSTASH_DESKTOP_BRIDGE=f'http://127.0.0.1:{self.shell.server_port}',
                        ILLUSTASH_DESKTOP_TOKEN='synthetic-picker-secret')
        self.process, self.log = self.start(self.lib.path)
        self.token = json.loads(self.request('/api/bootstrap')['body'])['token']

    def test_picker_selection_cancel_multi_and_no_guess(self):
        paths = [str(self.root / 'Pixiv'), str(self.root / '雪 X')]
        self.shell.reply = {'paths': paths}
        response = self.request('/api/libraries/choose', {'kind': 'folder', 'multiple': True})
        self.assertEqual(response['status'], 200, response)
        self.assertEqual(json.loads(response['body']), {'path': paths[0], 'name': 'Pixiv', 'paths': paths})
        self.assertEqual(self.shell.calls, [('/platform/choose?kind=folder&multiple=1', 'synthetic-picker-secret')])
        self.shell.reply = {'paths': [paths[0]]}
        self.assertEqual(json.loads(self.request('/api/libraries/choose', {'kind': 'folder'})['body']),
                         {'path': paths[0], 'preset': 'other', 'name': 'Pixiv'})
        self.shell.reply = {'paths': []}
        self.assertEqual(json.loads(self.request('/api/libraries/choose', {'kind': 'folder', 'multiple': True})['body']),
                         {'path': None, 'paths': []})

    def test_picker_security_and_invalid_replies(self):
        for body in ({'kind': 'invalid'}, {'kind': 'zip', 'multiple': True}):
            self.assertEqual(self.request('/api/libraries/choose', body)['status'], 400)
        self.assertEqual(self.request('/api/libraries/choose', {'kind': 'folder'}, token='old')['status'], 403)
        self.assertEqual(self.shell.calls, [])
        for reply in ({'paths': [None]}, {'paths': 'bad'}, {'paths': ['a', 'b']}, {'paths': ['']}):
            self.shell.reply = reply
            self.assertEqual(self.request('/api/libraries/choose', {'kind': 'folder'})['status'], 400)

    def test_picker_old_result_cannot_enter_replacement_library(self):
        other, _ = self.package('replacement')
        self.shell.reply = {'paths': [str(self.root / 'old-selected')]}
        self.shell.release.clear()
        with ThreadPoolExecutor() as pool:
            pending = pool.submit(self.request, '/api/libraries/choose', {'kind': 'folder'})
            self.assertTrue(self.shell.entered.wait(4))
            response = self.request('/api/libraries/open', {'path': str(other.path)})
            self.assertEqual(response['status'], 200, response)
            self.shell.release.set()
            old = pending.result(5)
        self.assertEqual(old['status'], 403, old)
        self.assertNotIn(b'old-selected', old['body'])
        new_token = json.loads(self.request('/api/bootstrap')['body'])['token']
        self.assertNotEqual(new_token, self.token)

    def test_exit_does_not_wait_for_pending_picker(self):
        self.shell.release.clear()
        with ThreadPoolExecutor() as pool:
            pending = pool.submit(self.request, '/api/libraries/choose', {'kind': 'folder'})
            self.assertTrue(self.shell.entered.wait(4))
            self.stop(self.process)
            self.assertIsNotNone(self.process.poll())
            self.shell.release.set()
            try:
                pending.result(5)
            except (OSError, TimeoutError):
                pass
