"""HTTP test helpers and the strict comparator tool's own tests. All state is disposable synthetic data."""
import http.client
import os
from pathlib import Path
import sqlite3
import signal
import socket
import subprocess
import tempfile
import time
import unittest
from unittest.mock import patch

from tests.fixed_expectations import Expectations
from tools import verify_read_http as verify

# Windows 没有 SIGINT：宿主放在独立进程组里启动，用 CTRL_BREAK 走它的控制台退出处理。
SPAWN = {'creationflags': subprocess.CREATE_NEW_PROCESS_GROUP} if os.name == 'nt' else {}
INTERRUPT = signal.CTRL_BREAK_EVENT if os.name == 'nt' else signal.SIGINT


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
    """Stop a host started with ``SPAWN`` (on Windows the interrupt goes to its process group)."""
    if process.poll() is None:
        process.send_signal(INTERRUPT)
        try:
            process.wait(12)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait(3)
    for stream in (process.stdout, process.stderr):
        if stream:
            stream.close()


class ComparatorTest(unittest.TestCase):
    def test_deterministic_corpus_excludes_random_ask(self):
        with tempfile.TemporaryDirectory() as tmp:
            # The synthetic application database as the former Python Store built it (recorded).
            root, rec = Path(tmp).resolve(), Expectations(self)
            rec.alias(root, '@ROOT@')
            rec.package('store', root, library=False)
            requests = verify.corpus(root / 'app.sqlite', sample=1, query_limit=0)
            rec.finish(self)
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
