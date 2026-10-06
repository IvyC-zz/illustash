"""provenance.net.KeepAlive：复用连接、跟随跳转、错误状态、服务器断开闲置连接后重连、经 HTTP 代理。只用本机假服务器。"""
import http.server
import threading
import unittest
from unittest import mock

from provenance import net as pnet
from provenance.net import KeepAlive, Net, Stopped


class Site(http.server.BaseHTTPRequestHandler):
    protocol_version = 'HTTP/1.1'             # keep-alive
    connections, lines, drop_after = set(), [], None

    def log_message(self, *_):
        pass

    def do_GET(self):
        cls = type(self)
        cls.connections.add(self.client_address)
        cls.lines.append(self.requestline)
        path = self.path.split('://', 1)[-1].split('/', 1)[-1]
        code, body, extra = {
            'ok': (200, b'{"ok": 1}', {}),
            'moved': (302, b'', {'Location': '/ok'}),
            'gone': (404, b'{}', {}),
            'denied': (403, b'<title>Just a moment...</title>', {}),
        }.get(path.split('?')[0], (500, b'', {}))
        # decide before answering: once the client has the body, the test may already have reset drop_after
        drop = cls.drop_after is not None and len(cls.lines) >= cls.drop_after
        self.send_response(code)
        for k, v in extra.items():
            self.send_header(k, v)
        self.send_header('Content-Length', str(len(body)))
        self.end_headers()
        self.wfile.write(body)
        if drop:
            self.close_connection = True       # hang up without saying so: the client's idle connection goes stale


class KeepAliveTest(unittest.TestCase):
    def setUp(self):
        Site.connections, Site.lines, Site.drop_after = set(), [], None
        self.server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Site)
        threading.Thread(target=self.server.serve_forever, daemon=True).start()
        self.addCleanup(self.server.server_close)
        self.addCleanup(self.server.shutdown)
        self.base = f'http://127.0.0.1:{self.server.server_port}'
        self.net = Net(1)                     # the default opener: KeepAlive (127.0.0.1 bypasses any proxy)
        self.net._wait = lambda host: None
        self.addCleanup(self.net.open.close)

    def test_one_connection_for_many_requests(self):
        for _ in range(5):
            self.assertEqual(self.net.get(self.base + '/ok'), {'ok': 1})
        self.assertEqual(len(Site.connections), 1)
        self.assertEqual(self.net.used, 5 * 9)

    def test_redirects_and_answers(self):
        self.assertEqual(self.net.get(self.base + '/moved'), {'ok': 1})
        self.assertIsNone(self.net.get(self.base + '/gone'))
        with self.assertRaises(Stopped):
            self.net.get(self.base + '/denied')
        self.assertEqual(len(Site.connections), 1)   # error answers keep the connection too

    def test_stale_connection_is_replaced(self):
        Site.drop_after = 1
        self.assertEqual(self.net.get(self.base + '/ok'), {'ok': 1})
        Site.drop_after = None
        self.assertEqual(self.net.get(self.base + '/ok'), {'ok': 1})   # no error, no retry delay
        self.assertEqual(len(Site.connections), 2)

    def test_plain_http_through_a_proxy_uses_the_absolute_url(self):
        with mock.patch.object(pnet.urllib.request, 'getproxies', return_value={'http': self.base}), \
             mock.patch.object(pnet.urllib.request, 'proxy_bypass', return_value=False):
            opener = KeepAlive()
        self.addCleanup(opener.close)
        net = Net(1, opener=opener)
        net._wait = lambda host: None
        self.assertEqual(net.get('http://example.invalid/ok?x=1'), {'ok': 1})
        self.assertEqual(Site.lines[-1], 'GET http://example.invalid/ok?x=1 HTTP/1.1')


if __name__ == '__main__':
    unittest.main()
