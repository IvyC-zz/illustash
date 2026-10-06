"""桌面 app 的 Pixiv 登录窗口：Python 端的通道客户端、Net 经窗口发请求、账号的「在窗口里登录」模式、过期时弹窗。
用本机假通道和假的窗口会话代替真正的外壳，不联网。"""
import http.server
import json
import os
import tempfile
import threading
import unittest
from pathlib import Path
from unittest import mock

from provenance import accounts, desktop, run
from provenance.net import LoginRejected, Net, Stopped


class FakeShell(http.server.BaseHTTPRequestHandler):
    """Answers like desktop/src/bridge.rs; records what it was asked."""
    calls = []
    replies = {}

    def log_message(self, *_):
        pass

    def _answer(self):
        type(self).calls.append((self.command, self.path, self.headers.get('X-Illustash-Token')))
        if self.headers.get('X-Illustash-Token') != 'secret':
            code, body = 403, {'error': '口令不对'}
        else:
            code, body = type(self).replies.get(self.path.split('?')[0], (502, {'error': '未知路径'}))
        data = json.dumps(body).encode()
        self.send_response(code)
        self.send_header('Content-Length', str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    do_GET = do_POST = _answer


class ChannelTest(unittest.TestCase):
    def setUp(self):
        FakeShell.calls, FakeShell.replies = [], {}
        self.server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), FakeShell)
        threading.Thread(target=self.server.serve_forever, daemon=True).start()
        self.addCleanup(self.server.server_close)
        self.addCleanup(self.server.shutdown)
        env = {'ILLUSTASH_DESKTOP_BRIDGE': f'http://127.0.0.1:{self.server.server_port}', 'ILLUSTASH_DESKTOP_TOKEN': 'secret'}
        patch = mock.patch.dict(os.environ, env)
        patch.start()
        self.addCleanup(patch.stop)

    def test_fetch_sends_path_and_token(self):
        FakeShell.replies['/pixiv/fetch'] = (200, {'status': 200, 'body': '{"error":false,"body":{"following":3}}'})
        status, body = desktop.fetch('https://www.pixiv.net/ajax/user/1/following?offset=0&limit=100')
        self.assertEqual((status, json.loads(body)['body']['following']), (200, 3))
        method, path, token = FakeShell.calls[-1]
        self.assertEqual((method, token), ('GET', 'secret'))
        self.assertIn('path=%2Fajax%2Fuser%2F1%2Ffollowing%3Foffset%3D0%26limit%3D100', path)

    def test_only_pixiv_goes_through_the_window(self):
        with self.assertRaises(ValueError):
            desktop.fetch('https://i.pximg.net/c/150x150_80/img-master/img/x.jpg')
        self.assertEqual(FakeShell.calls, [])

    def test_shell_errors_are_connection_errors(self):
        FakeShell.replies['/pixiv/fetch'] = (502, {'error': '用户正在 Pixiv 窗口里登录'})
        with self.assertRaisesRegex(ConnectionError, '正在 Pixiv 窗口里登录'):
            desktop.fetch('https://www.pixiv.net/ajax/user/extra')
        FakeShell.replies['/pixiv/fetch'] = (200, {'status': 0, 'error': 'TypeError: Load failed'})
        with self.assertRaisesRegex(ConnectionError, 'Load failed'):
            desktop.fetch('https://www.pixiv.net/ajax/user/extra')
        with mock.patch.dict(os.environ, {'ILLUSTASH_DESKTOP_TOKEN': 'wrong'}):
            with self.assertRaisesRegex(ConnectionError, '口令不对'):
                desktop.session()

    def test_login_and_logout_are_posts(self):
        FakeShell.replies.update({'/pixiv/login': (200, {'ok': True}), '/pixiv/logout': (200, {'ok': True})})
        desktop.login()
        desktop.logout()
        self.assertEqual([(m, p) for m, p, _ in FakeShell.calls], [('POST', '/pixiv/login'), ('POST', '/pixiv/logout')])

    def test_force_login_is_explicit(self):
        FakeShell.replies['/pixiv/login'] = (200, {'ok': True})
        desktop.login(force=True)
        self.assertEqual(FakeShell.calls[-1][:2], ('POST', '/pixiv/login?force=1'))

    def test_not_started_by_the_app(self):
        with mock.patch.dict(os.environ, {'ILLUSTASH_DESKTOP_BRIDGE': ''}):
            self.assertFalse(desktop.available())
            self.assertEqual(desktop.via(), {})
            with self.assertRaises(ConnectionError):
                desktop.session()


class ViaTest(unittest.TestCase):
    """Net sends www.pixiv.net through the window and treats its answers exactly like direct ones."""

    def net(self, *answers):
        replies = list(answers)
        def fetch(url):
            self.urls.append(url)
            reply = replies.pop(0)
            if isinstance(reply, Exception):
                raise reply
            return reply
        self.urls = []
        net = Net(1, via={'www.pixiv.net': fetch}, opener=lambda *a, **k: self.fail('went around the window'))
        net._wait = lambda host: None
        return net

    def test_answers(self):
        net = self.net((200, b'{"body": 1}'))
        self.assertEqual(net.get('https://www.pixiv.net/ajax/user/1', {'lang': 'ja'}), {'body': 1})
        self.assertEqual(self.urls, ['https://www.pixiv.net/ajax/user/1?lang=ja'])
        self.assertEqual(net.used, 11)
        self.assertIsNone(self.net((404, b'{}')).get('https://www.pixiv.net/ajax/user/9'))

    def test_bot_check_stops_the_host(self):
        net = self.net((403, b'<title>Just a moment...</title>'))
        with self.assertRaises(Stopped):
            net.get('https://www.pixiv.net/ajax/user/extra')
        with self.assertRaises(Stopped):                 # no second try for the rest of the run
            net.get('https://www.pixiv.net/ajax/user/1')
        self.assertEqual(len(self.urls), 1)

    def test_401_is_an_expired_login(self):
        net = self.net((401, b'{}'))
        with self.assertRaises(LoginRejected):
            net.get('https://www.pixiv.net/ajax/user/1/profile/all')
        self.assertIn('www.pixiv.net', net.rejected)

    def test_channel_trouble_is_retried_then_reported(self):
        with mock.patch('provenance.net.time.sleep'):
            self.assertEqual(self.net(ConnectionError('busy'), (200, b'[]')).get('https://www.pixiv.net/ajax/x'), [])
            with self.assertRaises(ConnectionError):
                self.net(*[ConnectionError('gone')] * 3).get('https://www.pixiv.net/ajax/x')


class Window:
    """Stands in for provenance.desktop: a Pixiv window that may or may not hold a login."""

    def __init__(self, user='123', answer=True):
        self.user, self.answer, self.calls = user, answer, []

    def install(self, test):
        for name in ('available', 'session', 'login', 'logout', 'fetch'):
            patch = mock.patch.object(desktop, name, getattr(self, name))
            patch.start()
            test.addCleanup(patch.stop)

    def available(self):
        return True

    def session(self):
        return {'loggedIn': bool(self.user), 'userId': self.user, 'loginOpen': False}

    def login(self, force=False):
        self.calls.append('login')
        if force:
            self.calls.append('force-login')

    def logout(self):
        self.calls.append('logout')
        self.user = None

    def fetch(self, url):
        self.calls.append(url)
        if not self.answer:
            return 200, b'{"error": true, "message": "", "body": []}'
        if 'user/extra' in url:
            return 200, b'{"error": false, "body": {"following": 82}}'
        return 200, json.dumps({'error': False, 'body': {'name': 'me'}}).encode()


class AppLoginTest(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        for patch in (mock.patch.object(accounts, 'APP_SUPPORT', Path(self.temp.name)),
                      mock.patch.dict(os.environ, {'ILLUSTASH_NO_KEYCHAIN': '1'}),
                      mock.patch('provenance.net.Net._wait', lambda self, host: None),
                      mock.patch.object(accounts, 'RECHECK_AFTER', 0)):
            patch.start()
            self.addCleanup(patch.stop)
        self.window = Window()
        self.window.install(self)

    def connect(self):
        return accounts.connect_pixiv(accounts.make_net(5, app=True))

    def test_connect_records_who_and_uses_the_window(self):
        self.window.user = None
        self.assertIsNone(self.connect())                       # still waiting for the person to log in
        self.assertFalse(accounts.status()['pixiv']['bound'])
        self.window.user = '123'
        who = self.connect()
        self.assertEqual((who['user_id'], who['name'], who['following'], who['via']), ('123', 'me', 82, 'app'))
        self.assertTrue(accounts.status()['pixiv']['bound'])
        self.assertTrue(accounts.pixiv_bound())
        self.assertNotIn('www.pixiv.net', accounts.headers())    # no cookie: the window carries the login
        self.assertIn('www.pixiv.net', accounts.make_net(5).via)
        self.assertTrue(all('www.pixiv.net/ajax/' in c for c in self.window.calls))

    def test_settings_forces_login_only_after_expiry(self):
        from web.server import account_action
        self.connect()
        with mock.patch.object(desktop, 'login') as login:
            account_action(None, 'pixiv/login', {})
            login.assert_called_once_with(force=False)
            login.reset_mock()
            accounts.mark_pixiv(problem=accounts.PIXIV_EXPIRED)
            account_action(None, 'pixiv/login', {})
            login.assert_called_once_with(force=True)

    def test_connect_replaces_a_pasted_session(self):
        accounts.put('pixiv', '9_old')
        self.connect()
        self.assertIsNone(accounts.get('pixiv'))
        self.assertTrue(accounts.pixiv_in_app())

    def test_mid_login_is_waiting_not_an_error(self):
        """Cookie set, window still on accounts.pixiv.net: the shell won't take the page away, so try again later."""
        busy = mock.Mock(side_effect=ConnectionError('桌面 app：用户正在 Pixiv 窗口里登录'))
        with mock.patch.object(desktop, 'fetch', busy), mock.patch('provenance.net.time.sleep'):
            with mock.patch.object(desktop, 'session', lambda: {'loggedIn': True, 'userId': '123', 'loginOpen': True}):
                self.assertIsNone(self.connect())
            with self.assertRaises(ConnectionError):             # window closed: a real failure, say so
                self.connect()
        self.assertFalse(accounts.status()['pixiv']['bound'])
        self.assertIsNotNone(self.connect())

    def test_refused_login_is_not_recorded(self):
        self.window.answer = False
        with self.assertRaises(accounts.AccountError):
            self.connect()
        self.assertFalse(accounts.status()['pixiv']['bound'])

    def test_recheck_and_expiry(self):
        self.connect()
        self.assertTrue(accounts.recheck_pixiv(accounts.make_net(5)))
        self.window.user = None                                  # logged out in the window
        self.assertFalse(accounts.recheck_pixiv(accounts.make_net(5)))
        self.assertEqual(accounts.status()['pixiv']['expired'], accounts.PIXIV_EXPIRED)
        self.window.user, self.window.answer = '123', False       # a login Pixiv no longer accepts
        self.assertFalse(accounts.recheck_pixiv(accounts.make_net(5)))
        self.assertIn('Pixiv 返回 error', accounts.status()['pixiv']['expired_detail'])   # what Pixiv said is kept
        self.window.answer = True
        self.assertTrue(accounts.recheck_pixiv(accounts.make_net(5)))
        self.assertNotIn('expired', accounts.status()['pixiv'])

    def test_one_bad_answer_is_asked_again_before_calling_it_expired(self):
        self.connect()
        answers = iter([False, True])                            # right after launch: refused once, then fine
        real = self.window.fetch
        def flaky(url):
            if 'user/extra' in url:
                self.window.answer = next(answers)
            return real(url)
        with mock.patch.object(desktop, 'fetch', flaky):
            self.assertTrue(accounts.recheck_pixiv(accounts.make_net(5)))
        self.assertNotIn('expired', accounts.status()['pixiv'])

    def test_without_the_app_pixiv_is_stopped_not_logged_out(self):
        self.connect()
        with mock.patch.object(desktop, 'available', lambda: False):
            net = accounts.make_net(5)
            with self.assertRaisesRegex(Stopped, '桌面 app'):
                net.get('https://www.pixiv.net/ajax/user/1/profile/all')
            self.assertIn('i.pximg.net', net.headers)             # thumbnails still come straight from Python
            with self.assertRaises(ConnectionError):
                accounts.recheck_pixiv(net)
        self.assertNotIn('expired', accounts.status()['pixiv'])  # nothing learned about the login

    def test_unbind_clears_the_window(self):
        self.connect()
        accounts.remove('pixiv')
        self.assertIn('logout', self.window.calls)
        self.assertFalse(accounts.status()['pixiv']['bound'])

    def test_expired_login_pops_up_the_window_on_manual_runs_only(self):
        self.connect()
        logs = []
        with mock.patch.object(accounts, 'recheck_pixiv', return_value=False):
            run._check_pixiv(accounts.make_net(5), logs.append, 'new')
            self.assertNotIn('login', self.window.calls)          # after a sync: no window out of nowhere
            self.assertIn('不弹出 Pixiv 登录窗口（运行方式 new', logs[-1])   # and the log says why
            net = accounts.make_net(5)
            run._check_pixiv(net, logs.append, 'full')
        self.assertIn('login', self.window.calls)
        self.assertIn('force-login', self.window.calls)
        self.assertEqual(net.stopped['www.pixiv.net'], accounts.PIXIV_EXPIRED)
        self.assertIn('Pixiv 登录窗口', logs[-1])


if __name__ == '__main__':
    unittest.main()
