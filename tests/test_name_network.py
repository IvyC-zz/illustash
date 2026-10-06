"""Provider retry and pacing tests; transport is simulated, never live."""
import io
import tempfile
import threading
import time
import unittest
import urllib.error
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path
from unittest.mock import patch

from tests.test_web import fixture
from web.names import BG, DB, FetchError, NameUpdater, request_json, retry_after_seconds
from web.store import Store


class NameNetworkTest(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        root = Path(self.temp.name)
        self.store = Store(fixture(root), root / 'library.sqlite', build_general=False)

    def tearDown(self):
        self.temp.cleanup()

    def test_429_retry_after_then_5xx_exponential_backoff(self):
        calls = []
        failures = [urllib.error.HTTPError(BG, 429, 'limit', {'Retry-After': '2'}, io.BytesIO()),
                    urllib.error.HTTPError(BG, 503, 'unavailable', {}, io.BytesIO())]

        def transport(*args):
            calls.append(args)
            if failures:
                raise failures.pop(0)
            return {'data': []}

        updater = NameUpdater(self.store, transport, interval=0)
        clock = [100.0]
        sleeps = []
        def sleep(seconds):
            sleeps.append(seconds)
            clock[0] += seconds
        with patch('web.names.time.monotonic', side_effect=lambda: clock[0]), patch('web.names.time.sleep', side_effect=sleep), patch('web.names.random.uniform', return_value=0):
            self.assertEqual(updater.fetch(BG + '/search/characters'), {'data': []})
        self.assertEqual(len(calls), 3)
        self.assertEqual([delay for delay in sleeps if delay > 0], [2, 1])
        self.assertEqual(updater.host_failures['api.bgm.tv'], 0)

    def test_retry_after_http_date(self):
        with patch('web.names.time.time', return_value=1_000):
            self.assertEqual(retry_after_seconds('Thu, 01 Jan 1970 00:17:10 GMT'), 30)

    def test_404_and_other_4xx_do_not_retry(self):
        for code in (400, 401, 403, 404):
            calls = []

            def transport(*args):
                calls.append(args)
                raise urllib.error.HTTPError(BG, code, 'private body', {}, io.BytesIO(b'secret'))

            updater = NameUpdater(self.store, transport, interval=0)
            with self.assertRaises(FetchError), patch('web.names.time.sleep'):
                updater.fetch(BG + '/search/characters')
            self.assertEqual(len(calls), 1)
        with patch('urllib.request.urlopen', side_effect=urllib.error.HTTPError(BG, 404, '', {}, io.BytesIO())):
            self.assertIsNone(request_json(BG + '/characters/0'))

    def test_exhausted_network_error_has_no_negative_cache(self):
        calls = []

        def broken(*args):
            calls.append(args)
            raise urllib.error.URLError(OSError('Tunnel connection failed: 503 Service Unavailable'))

        updater = NameUpdater(self.store, broken, interval=0)
        url = DB + '/wiki_pages.json?search%5Btitle%5D=test'
        with patch('web.names.time.sleep'), patch('web.names.random.uniform', return_value=0):
            with self.assertRaises(FetchError) as caught:
                updater.cached(url)
        self.assertEqual(len(calls), 3)
        self.assertIn('代理隧道 503', str(caught.exception))
        with self.store.connect() as c:
            self.assertEqual(c.execute('SELECT count(*) FROM name_cache').fetchone()[0], 0)
        updater.transport = lambda *args: []
        self.assertEqual(updater.cached(url), [])
        with self.store.connect() as c:
            self.assertEqual(c.execute('SELECT count(*) FROM name_cache').fetchone()[0], 1)

    def test_consecutive_host_failures_pause_then_resume(self):
        clock = [100.0]
        sleeps = []
        calls = [0]

        def sleep(seconds):
            sleeps.append(seconds)
            clock[0] += seconds

        def transport(*args):
            calls[0] += 1
            if calls[0] < 3:
                raise TimeoutError('private detail')
            return []

        updater = NameUpdater(self.store, transport, interval=0)
        updater.attempts, updater.failure_limit, updater.pause_seconds = 1, 2, 180
        with patch('web.names.time.monotonic', side_effect=lambda: clock[0]), patch('web.names.time.sleep', side_effect=sleep):
            for _ in range(2):
                with self.assertRaises(FetchError):
                    updater.fetch(BG + '/search/subjects')
            self.assertEqual(updater.fetch(BG + '/search/subjects'), [])
        self.assertTrue(any(delay >= 180 for delay in sleeps))
        self.assertEqual(updater.host_failures['api.bgm.tv'], 0)

    def test_two_in_flight_per_host_and_one_fetch_per_cache_key(self):
        active = [0]
        peak = [0]
        calls = [0]
        lock = threading.Lock()

        def transport(*args):
            with lock:
                calls[0] += 1
                active[0] += 1
                peak[0] = max(peak[0], active[0])
            time.sleep(0.03)
            with lock:
                active[0] -= 1
            return []

        updater = NameUpdater(self.store, transport, interval=0)
        with ThreadPoolExecutor(max_workers=4) as pool:
            self.assertEqual(list(pool.map(lambda i: updater.fetch(BG + f'/subjects/{i}'), range(4))), [[]] * 4)
        self.assertEqual((peak[0], calls[0]), (2, 4))
        calls[0] = 0
        with ThreadPoolExecutor(max_workers=2) as pool:
            self.assertEqual(list(pool.map(lambda _: updater.cached(BG + '/subjects/same'), range(2))), [[], []])
        self.assertEqual(calls[0], 1)
