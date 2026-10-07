"""Read-only endpoint contracts and thumbnail scheduling against disposable libraries."""
from concurrent.futures import ThreadPoolExecutor
from contextlib import contextmanager
import gzip
import json
import threading
import time
import unittest
from unittest.mock import patch
from PIL import Image
from tests import web_http_fixture


class DeliveryPerformanceTest(unittest.TestCase):
    setUp = web_http_fixture.HTTPTest.setUp
    tearDown = web_http_fixture.HTTPTest.tearDown
    request = web_http_fixture.HTTPTest.request

    def test_character_summary_http_contract(self):
        status, body, _ = self.request('/api/characters?min=1&summary=1')
        self.assertEqual(status, 200)
        self.assertEqual(json.loads(body), self.store.characters(minimum=1, summary=True))
        self.assertTrue(all(row['summary'] for row in json.loads(body)))
        status, body, _ = self.request('/api/characters?min=1')
        self.assertEqual(status, 200)
        self.assertTrue(all('summary' not in row for row in json.loads(body)))

    def test_gzip_negotiation_preserves_json_and_security_headers(self):
        payload = [{'id': n, 'name': '角色名字' * 20} for n in range(50)]
        with patch.object(self.store, 'characters', return_value=payload):
            for accept, encoded in [('gzip', True), ('br, gzip;q=0.5', True), ('gzip ; q=0.5', True), ('gzip;q=0, *;q=1', False), ('*', True), ('gzip;q=bad', False), ('identity', False)]:
                status, body, headers = self.request('/api/characters', headers={'Accept-Encoding': accept})
                self.assertEqual(status, 200)
                self.assertEqual(headers.get('Content-Encoding') == 'gzip', encoded)
                self.assertEqual(int(headers['Content-Length']), len(body))
                self.assertEqual(headers['Vary'], 'Accept-Encoding')
                self.assertEqual(headers['Cache-Control'], 'no-store')
                self.assertEqual(json.loads(gzip.decompress(body) if encoded else body), payload)
        self.assertIsNone(self.request('/api/identity/count', headers={'Accept-Encoding': 'gzip'})[2].get('Content-Encoding'))

    def test_count_only_and_countless_page_contract(self):
        for query in ('', '?status=confirmed', '?nsfw=hide', '?q=Alice', '?pool=unmatched'):
            full = json.loads(self.request('/api/assets' + query)[1])
            with patch.object(self.store, '_display_reader', side_effect=AssertionError('count rendered items')):
                status, body, _ = self.request('/api/assets/count' + query)
            self.assertEqual(status, 200)
            self.assertEqual(json.loads(body), {'total': full['total']})
        queries = []
        connect = self.store.connect
        @contextmanager
        def traced():
            with connect() as c:
                c.set_trace_callback(queries.append)
                yield c
        with patch.object(self.store, 'connect', traced):
            page = json.loads(self.request('/api/assets?count=0')[1])
        self.assertIsNone(page['total'])
        self.assertEqual(page['items'], self.store.assets()['items'])
        self.assertFalse(any('SELECT count(*) FROM assets a' in q for q in queries))
        self.assertEqual(self.request('/api/assets/count?unrecognized=x')[0], 400)

    def test_identity_count_preserves_queue_filter_skip_and_undo(self):
        chars = self.store.characters(minimum=1)
        a, b = chars[0]['id'], chars[1]['id']
        with self.store.connect() as c:
            c.execute("INSERT INTO identity_pairs VALUES (?,?,0.01,NULL,'anchor_unverified','','{}')", (a, b))
        def check():
            for nsfw in ('', 'hide', 'only'):
                expected = self.store.identity_queue(nsfw=nsfw)['total']
                with patch('web.names.View.entity', side_effect=AssertionError('count rendered names')):
                    result = self.store.identity_queue(nsfw=nsfw, count_only=True)
                self.assertEqual(result, {'total': expected})
                self.assertEqual(json.loads(self.request('/api/identity/count?nsfw=' + nsfw)[1]), result)
        self.store.decide_identity(a=a, b=b, kind='different_from', status='proposed', reason='test')
        self.assertEqual(self.store.identity_queue()['total'], 1)
        check()
        action = self.store.skip_identity_pair(a, b)
        check()
        self.assertEqual(self.store.identity_queue(count_only=True)['total'], 0)
        self.store.undo(action['id'])
        check()
        self.assertEqual(self.store.identity_queue(count_only=True)['total'], 1)
        self.store.decide_identity(a=a, b=b, kind='different_from', status='accepted', reason='test')
        check()
        self.assertEqual(self.store.identity_queue(count_only=True)['total'], 0)

    def test_identity_queue_never_pairs_the_shared_oc_mark(self):
        a = self.store.characters(minimum=1)[0]['id']
        oc = self.store.shared_oc()['id']
        self.store.change(['b'], 'confirmed', oc)
        with self.store.connect() as c:
            c.execute("INSERT INTO identity_pairs VALUES (?,?,0.01,NULL,'anchor_unverified','','{}')", tuple(sorted((a, oc))))
        self.assertEqual(self.store.identity_queue()['total'], 0)
        self.assertEqual(self.store.identity_queue(count_only=True)['total'], 0)

    def test_cached_thumbnail_does_not_wait_for_decoder_slots(self):
        first = self.request('/image?id=a')[1]
        for _ in range(4):
            self.server.image_slots.acquire()
        try:
            with ThreadPoolExecutor() as pool:
                future = pool.submit(self.request, '/image?id=a')
                try:
                    status, body, _ = future.result(timeout=2)
                finally:
                    for _ in range(4):
                        self.server.image_slots.release()
            self.assertEqual(status, 200)
            self.assertEqual(body, first)
        finally:
            self.assertEqual(self.server.image_locks, {})

    def test_same_cold_thumbnail_is_decoded_once(self):
        original = Image.open
        barrier = threading.Barrier(6)
        calls = []
        def opening(*args, **kwargs):
            calls.append(args[0])
            time.sleep(.15)
            return original(*args, **kwargs)
        def request():
            barrier.wait(timeout=3)
            return self.request('/image?id=a')
        with patch('web.server.Image.open', side_effect=opening), ThreadPoolExecutor(max_workers=6) as pool:
            results = list(pool.map(lambda _: request(), range(6)))
        self.assertTrue(all(r[0] == 200 for r in results))
        self.assertEqual(len({r[1] for r in results}), 1)
        self.assertEqual(len(calls), 1)
        self.assertEqual(self.server.image_locks, {})
        self.assertFalse(list(self.server.cache.glob('*.tmp')))

    def test_source_change_invalidates_cached_thumbnail(self):
        first = self.request('/image?id=a')[1]
        source = self.store.image_path('a')
        Image.new('RGB', (80, 100), '#e00000').save(source)
        status, second, _ = self.request('/image?id=a')
        self.assertEqual(status, 200)
        self.assertNotEqual(first, second)

    def test_generation_failure_releases_locks_and_allows_retry(self):
        with patch('web.server.Image.Image.save', side_effect=OSError('test')):
            self.assertEqual(self.request('/image?id=a')[0], 404)
        self.assertEqual(self.server.image_locks, {})
        self.assertFalse(list(self.server.cache.glob('*.tmp')))
        self.assertEqual(self.request('/image?id=a')[0], 200)
