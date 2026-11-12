"""原生的缓存清理，只用新建的合成包和资源。"""
from concurrent.futures import ThreadPoolExecutor
import http.client
import json
import os
import socket
import threading
import time
import unittest

from PIL import Image
from tests import test_native_lifecycle as lifecycle
from tests.test_native_lifecycle import NativeLifecycleTest, dump
from tests.test_native_files import asset, key
from tools.verify_read_http import cookie


class NativeCacheTest(unittest.TestCase):
    setUpClass = classmethod(NativeLifecycleTest.setUpClass.__func__)
    setUp = NativeLifecycleTest.setUp
    package = NativeLifecycleTest.package
    stop = NativeLifecycleTest.stop
    start = NativeLifecycleTest.start
    request = NativeLifecycleTest.request
    pair = NativeLifecycleTest.pair

    def seed(self):
        cache = self.lib.thumbnails
        for name in ('one.jpg', '.hidden.jpg', '.jpg'):
            (cache / name).write_bytes(b'synthetic cached bytes')
        for name in ('keep.JPG', 'keep.png', 'keep.tmp'):
            (cache / name).write_bytes(b'keep')
        (cache / 'nested').mkdir(exist_ok=True)
        (cache / 'nested/keep.jpg').write_bytes(b'keep nested')

    def test_clear_exact_response_scope_count_and_database_unchanged(self):
        before = dump(self.lib.db)
        database_bytes = self.lib.db.read_bytes()
        self.seed()
        old = lifecycle.NativeLifecycleTest.reference_response(self, '/api/cache', {'action': 'clear'})
        self.seed()
        new = self.request('/api/cache', {'action': 'clear'})
        self.assertEqual(old['status'], 200)
        self.assertEqual(new['status'], 200)
        # 参考结果录自 POSIX；Windows 文件名不区分大小写，所以也会删掉 keep.JPG。
        recorded, payload = json.loads(old['body']), json.loads(new['body'])
        if os.name == 'nt':
            recorded['removed'] = payload['removed']
            self.assertEqual(payload, recorded)
        else:
            self.assertEqual(new['body'], old['body'])
        self.assertEqual(new['route'], 'core')
        self.assertEqual(payload['removed'], 4 if os.name == 'nt' else 3)
        self.assertEqual(payload['thumbnails']['files'], 0)
        self.assertEqual(payload['thumbnails']['bytes'], 0)
        for name in ('keep.png', 'keep.tmp', 'nested/keep.jpg', *(('keep.JPG',) if os.name != 'nt' else ())):
            self.assertTrue((self.lib.thumbnails / name).is_file())
        self.assertEqual(self.pair('/api/cache', {'action': 'clear'})['status'], 200)
        self.assertEqual(before, dump(self.lib.db))
        self.assertEqual(database_bytes, self.lib.db.read_bytes())
        self.assertFalse(self.sentinel.exists())

    def test_missing_cache_errors_and_security_status_only(self):
        self.lib.thumbnails.rmdir()
        self.assertEqual(self.pair('/api/cache', {'action': 'clear'})['status'], 200)
        self.assertFalse(self.lib.thumbnails.exists())
        for body in ({}, {'action': 'bad'}, {'action': None}, {'action': ['clear']}, {'action': 1}):
            self.assertEqual(self.request('/api/cache', body)['status'],
                             lifecycle.NativeLifecycleTest.reference_response(self, '/api/cache', body)['status'])
        for headers in ({'Host': 'evil.example'}, {'Origin': 'https://evil.example'}):
            self.assertEqual(self.request('/api/cache', {'action': 'clear'}, headers=headers)['status'],
                             lifecycle.NativeLifecycleTest.reference_response(self, '/api/cache', {'action': 'clear'}, headers=headers)['status'])
        self.assertEqual(self.request('/api/cache', {'action': 'clear'}, token='bad')['status'], 403)
        self.lib.thumbnails.mkdir()
        (self.lib.thumbnails / 'cannot-unlink.jpg').mkdir()
        self.assertEqual(self.request('/api/cache', {'action': 'clear'})['status'],
                         lifecycle.NativeLifecycleTest.reference_response(self, '/api/cache', {'action': 'clear'})['status'])
        self.assertEqual(self.request('/api/cache', {'action': 'clear'})['status'], 500)
        self.assertTrue((self.lib.thumbnails / 'cannot-unlink.jpg').is_dir())
        self.assertFalse(self.sentinel.exists())

    def test_clear_and_concurrent_images_regenerate_without_temps(self):
        source = asset(self, 'concurrent-clear', Image.new('RGB', (1601, 801), '#407080'))
        url = '/image?id=concurrent-clear'
        target = self.lib.thumbnails / key(source, 'concurrent-clear', 'thumb')
        self.assertEqual(self.request(url)['status'], 200)
        self.assertEqual(json.loads(self.request('/api/cache', {'action': 'clear'})['body'])['removed'], 1)
        self.assertFalse(target.exists())
        barrier = threading.Barrier(12)
        def operation(i):
            barrier.wait()
            return self.request('/api/cache', {'action': 'clear'}) if i % 3 == 0 else self.request(url)
        with ThreadPoolExecutor(max_workers=12) as pool:
            results = list(pool.map(operation, range(12)))
        self.assertTrue(all(r['status'] == 200 and r['route'] == 'core' for r in results))
        self.assertEqual(self.request('/api/cache', {'action': 'clear'})['status'], 200)
        self.assertFalse(target.exists())
        regenerated = self.request(url)
        self.assertEqual(regenerated['status'], 200)
        self.assertEqual(regenerated['body'], target.read_bytes())
        self.assertFalse(list(self.lib.thumbnails.glob('*.tmp')))
        self.assertFalse(self.sentinel.exists())

    def test_slow_image_clear_and_switch_keep_old_bytes_and_new_cache(self):
        second = self.package('second')
        source = asset(self, 'slow', Image.new('RGB', (12, 8), 'red'))
        target = self.lib.thumbnails / key(source, 'slow', 'thumb')
        payload = b'synthetic old cache' * 300_000
        target.write_bytes(payload)
        (second.thumbnails / 'new.jpg').write_bytes(b'new package cache')
        sock = socket.socket(); self.addCleanup(sock.close)
        sock.setsockopt(socket.SOL_SOCKET, socket.SO_RCVBUF, 4096); sock.settimeout(10)
        sock.connect(('127.0.0.1', self.port))
        sock.sendall((f'GET /image?id=slow HTTP/1.1\r\nHost: 127.0.0.1:{self.port}\r\nCookie: {cookie(self.origin)}\r\n'
                      'Connection: close\r\n\r\n').encode())
        response = http.client.HTTPResponse(sock); self.addCleanup(response.close); response.begin()
        self.assertEqual(response.status, 200)
        clear = self.request('/api/cache', {'action': 'clear'})
        self.assertEqual(clear['status'], 200)
        self.assertEqual(json.loads(clear['body'])['removed'], 1)
        self.assertFalse(target.exists())
        old_token = self.token
        self.assertEqual(self.request('/api/libraries/open', {'path': str(second.path)})['status'], 200)
        sock.setsockopt(socket.SOL_SOCKET, socket.SO_RCVBUF, 1024 * 1024)
        self.assertEqual(response.read(), payload)
        self.assertEqual((second.thumbnails / 'new.jpg').read_bytes(), b'new package cache')
        self.assertEqual(self.request('/api/cache', {'action': 'clear'}, token=old_token)['status'], 403)
        self.token = json.loads(self.request('/api/bootstrap')['body'])['token']
        self.assertEqual(json.loads(self.request('/api/cache', {'action': 'clear'})['body'])['removed'], 1)
        self.assertFalse(self.sentinel.exists())

    def test_inflight_clear_switch_returns_old_package_settings(self):
        second = self.package('second')
        # 包的副本可能 ID 相同；存储的归属按路径/会话确定。
        config = json.loads((second.path / 'library.json').read_text())
        config['id'] = json.loads((self.lib.path / 'library.json').read_text())['id']
        (second.path / 'library.json').write_text(json.dumps(config))
        (second.thumbnails / 'keep.jpg').write_bytes(b'new cache')
        for i in range(8000):
            (self.lib.thumbnails / f'{i:05}.jpg').write_bytes(b'synthetic')
        with ThreadPoolExecutor(max_workers=1) as pool:
            future = pool.submit(self.request, '/api/cache', {'action': 'clear'})
            deadline = time.monotonic() + 5
            while len(list(self.lib.thumbnails.iterdir())) == 8000:
                self.assertLess(time.monotonic(), deadline)
                time.sleep(.001)
            self.assertEqual(self.request('/api/libraries/open', {'path': str(second.path)})['status'], 200)
            clear = future.result(timeout=10)
        self.assertEqual(clear['status'], 200)
        payload = json.loads(clear['body'])
        self.assertEqual(payload['removed'], 8000)
        self.assertEqual(payload['thumbnails']['path'], str(self.lib.thumbnails))
        self.assertEqual(payload['thumbnails']['files'], 0)
        self.assertEqual((second.thumbnails / 'keep.jpg').read_bytes(), b'new cache')
        self.assertFalse(self.sentinel.exists())

    def test_partial_body_from_old_session_cannot_clear_new_package(self):
        second = self.package('second')
        (second.thumbnails / 'keep.jpg').write_bytes(b'new cache')
        sock = socket.socket(); self.addCleanup(sock.close); sock.settimeout(5)
        sock.connect(('127.0.0.1', self.port))
        body = b'{"action":"clear"}'
        headers = (f'POST /api/cache HTTP/1.1\r\nHost: 127.0.0.1:{self.port}\r\nOrigin: {self.origin}\r\n'
                   f'Cookie: {cookie(self.origin)}\r\n'
                   f'X-CSRF-Token: {self.token}\r\nContent-Type: application/json\r\nContent-Length: {len(body)}\r\n'
                   'Connection: close\r\n\r\n').encode()
        sock.sendall(headers + body[:1])
        self.assertEqual(self.request('/api/libraries/open', {'path': str(second.path)})['status'], 200)
        sock.sendall(body[1:])
        response = http.client.HTTPResponse(sock); self.addCleanup(response.close); response.begin()
        self.assertEqual(response.status, 403)
        self.assertEqual((second.thumbnails / 'keep.jpg').read_bytes(), b'new cache')
        self.assertFalse(self.sentinel.exists())


del NativeLifecycleTest
