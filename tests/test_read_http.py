"""HTTP 测试辅助函数，以及严格比较工具自身的测试。所有状态都是用完即弃的合成数据。"""
import http.client
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
    """停止用 ``SPAWN`` 启动的宿主（在 Windows 上，中断信号发给它的进程组）。"""
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
            # 原 Python Store 建出来的合成应用数据库（录制）。
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

    def test_fetch_closes_with_reset_after_reading_the_whole_response(self):
        # #140：每个请求仍是一条新连接，读完响应后以 RST 关闭，本机不留 TIME_WAIT；服务端据此看到复位而非 EOF。
        listener = socket.create_server(('127.0.0.1', 0))
        self.addCleanup(listener.close)
        listener.settimeout(10)
        seen = []

        def serve(count):
            for index in range(count):
                conn, (_, port) = listener.accept()
                with conn:
                    conn.settimeout(10)
                    data = b''
                    while b'\r\n\r\n' not in data:
                        data += conn.recv(4096)
                    head, body = data.split(b'\r\n\r\n', 1)
                    length = next((int(line.split(b':')[1]) for line in head.split(b'\r\n')
                                   if line.lower().startswith(b'content-length:')), 0)
                    while len(body) < length:
                        body += conn.recv(4096)
                    reply = head.split(b' ', 1)[0] + b' ' + body
                    close = b'Connection: close\r\n' if index == count - 1 else b''
                    conn.sendall(b'HTTP/1.1 200 OK\r\nContent-Length: %d\r\n%s\r\n%s' % (len(reply), close, reply))
                    try:
                        ending = 'eof' if conn.recv(1) == b'' else 'data'
                    except ConnectionResetError:
                        ending = 'reset'
                    seen.append((port, ending))

        server = threading.Thread(target=serve, args=(3,), daemon=True)
        server.start()
        origin = f'http://127.0.0.1:{listener.getsockname()[1]}'
        bodies = [verify.fetch(origin, item, origin)['body'] for item in (
            {'url': '/'}, {'url': '/', 'method': 'POST', 'body': b'{"x":1}'}, {'url': '/last'})]
        server.join(10)
        self.assertEqual(bodies, [b'GET ', b'POST {"x":1}', b'GET '])
        self.assertEqual([ending for _, ending in seen], ['reset'] * 3)
        self.assertEqual(len({port for port, _ in seen}), 3)

    def test_non_temporary_database_refused_before_open(self):
        with patch.object(sqlite3, 'connect', side_effect=AssertionError('opened DB')):
            with self.assertRaises(ValueError):
                verify.corpus(Path('/__illustash_synthetic_non_temporary__') / 'do-not-read.sqlite')
