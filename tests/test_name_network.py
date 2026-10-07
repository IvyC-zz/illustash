"""Provider retry and pacing tests; transport is simulated, never live."""
import tempfile
import threading
import time
import unittest
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

from tests.synthetic_source import source_fixture as fixture
from web.names import BG, NameUpdater
from web.store import Store


class NameNetworkTest(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        root = Path(self.temp.name)
        self.store = Store(fixture(root), root / 'library.sqlite', build_general=False)

    def tearDown(self):
        self.temp.cleanup()

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
