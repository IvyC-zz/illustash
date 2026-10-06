"""绑定的账号：Pixiv 登录状态的格式和校验、SauceNAO key、解绑、旧明文文件迁移。用文件存储代替钥匙串，不联网。"""
import os
import stat
import tempfile
import unittest
from pathlib import Path
from unittest import mock

from provenance import accounts


class Net:
    def __init__(self, ok=True):
        self.ok = ok

    def get(self, url, params=None, extra=None, **_):
        assert extra['Cookie'].startswith('PHPSESSID=')
        if 'user/extra' in url:
            return {'error': False, 'body': {'following': 82}} if self.ok else None   # a 401 answers None
        return {'body': {'name': 'me'}}


class AccountsTest(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        for p in (mock.patch.object(accounts, 'APP_SUPPORT', self.root / 'support'),
                  mock.patch.dict(os.environ, {'ILLUSTASH_NO_KEYCHAIN': '1'})):
            p.start()
            self.addCleanup(p.stop)

    def tearDown(self):
        self.temp.cleanup()

    def test_pixiv_value_forms(self):
        self.assertEqual(accounts.clean_pixiv(' 123_AbC '), '123_AbC')
        self.assertEqual(accounts.clean_pixiv('first=1; PHPSESSID=123_AbC; p_ab_id=4'), '123_AbC')
        with self.assertRaises(accounts.AccountError):
            accounts.clean_pixiv('mkdir -p ~/.config && read -rs k')      # the command pasted by mistake

    def test_bind_check_and_unbind(self):
        with self.assertRaises(accounts.AccountError):
            accounts.bind_pixiv('123_AbC', Net(ok=False))
        self.assertFalse(accounts.status()['pixiv']['bound'])
        who = accounts.bind_pixiv('123_AbC', Net())
        self.assertEqual((who['user_id'], who['name'], who['following']), ('123', 'me', 82))
        st = accounts.status()['pixiv']
        self.assertTrue(st['bound'])
        self.assertEqual(st['name'], 'me')
        self.assertEqual(stat.S_IMODE((self.root / 'support' / 'accounts' / 'pixiv').stat().st_mode), 0o600)
        self.assertEqual(accounts.headers()['www.pixiv.net']['Cookie'], 'PHPSESSID=123_AbC')
        accounts.remove('pixiv')
        self.assertFalse(accounts.status()['pixiv']['bound'])
        self.assertNotIn('www.pixiv.net', accounts.headers())

    def test_saucenao_key(self):
        with self.assertRaises(accounts.AccountError):
            accounts.bind_saucenao('not a key')
        accounts.bind_saucenao('a' * 40)
        self.assertEqual(accounts.get('saucenao'), 'a' * 40)

    def test_legacy_files_move_and_are_deleted(self):
        legacy = {'pixiv': self.root / 'pixiv.cookie', 'saucenao': self.root / 'saucenao.key'}
        legacy['pixiv'].write_text('9_x\n')
        with mock.patch.object(accounts, 'LEGACY', legacy):
            self.assertEqual(accounts.migrate_legacy(), ['pixiv'])
        self.assertEqual(accounts.get('pixiv'), '9_x')
        self.assertFalse(legacy['pixiv'].exists())


if __name__ == '__main__':
    unittest.main()


class ExpiredSessionTest(unittest.TestCase):
    """登录过期：不能悄悄退回未登录状态（那样查不到 R18，还会被缓存成「没找到」）。"""
    setUp, tearDown = AccountsTest.setUp, AccountsTest.tearDown

    def test_recheck_marks_expired_and_rebinding_clears_it(self):
        self.assertIsNone(accounts.recheck_pixiv(Net()))
        accounts.bind_pixiv('123_AbC', Net())
        self.assertFalse(accounts.recheck_pixiv(Net(ok=False)))
        st = accounts.status()['pixiv']
        self.assertTrue(st['bound'])
        self.assertEqual(st['expired'], accounts.PIXIV_EXPIRED)
        self.assertEqual(st['name'], 'me')                      # who it was stays visible
        self.assertTrue(accounts.recheck_pixiv(Net()))
        self.assertNotIn('expired', accounts.status()['pixiv'])
        accounts.recheck_pixiv(Net(ok=False))
        accounts.bind_pixiv('123_AbC', Net())
        self.assertNotIn('expired', accounts.status()['pixiv'])

    def test_a_401_with_the_session_stops_pixiv(self):
        import io
        import urllib.error
        from provenance.net import LoginRejected, Net as RealNet

        def opener(request, timeout):
            raise urllib.error.HTTPError(request.full_url, 401, 'Unauthorized', {}, io.BytesIO(b''))
        net = RealNet(1, {'www.pixiv.net': {'Cookie': 'PHPSESSID=1_a'}}, opener=opener)
        self.assertIsNone(net.get('https://danbooru.donmai.us/x.json'))      # no session sent: just an answer
        with self.assertRaises(LoginRejected):
            net.get('https://www.pixiv.net/ajax/user/1/profile/all')
        self.assertIn('www.pixiv.net', net.rejected)
        self.assertIn('www.pixiv.net', net.stopped)
        self.assertNotIn('danbooru.donmai.us', net.stopped)
        with self.assertRaises(accounts.AccountError):             # binding reports it as a bad login
            accounts.check_pixiv('1_a', RealNet(1, opener=opener))

    def test_run_stops_pixiv_when_the_login_expired(self):
        from provenance import run
        from provenance.net import Net as RealNet, Stopped
        accounts.bind_pixiv('123_AbC', Net())
        net, logs = RealNet(1), []
        with mock.patch.object(accounts, 'recheck_pixiv', return_value=False):
            run._check_pixiv(net, logs.append, "full")
        self.assertEqual(net.stopped['www.pixiv.net'], accounts.PIXIV_EXPIRED)
        with self.assertRaises(Stopped):
            net.get('https://www.pixiv.net/ajax/user/1/profile/all')
        net = RealNet(1)
        with mock.patch.object(accounts, 'recheck_pixiv', side_effect=ConnectionError('offline')):
            run._check_pixiv(net, logs.append, "full")                    # can't tell: carry on, say so
        self.assertEqual(net.stopped, {})
        self.assertIn('没能检查 Pixiv 登录', logs[-1])
