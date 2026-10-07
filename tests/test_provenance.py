"""溯源模块：像素哈希算法、原帖解析、查询流程（假的网络）、画师链接。只用临时目录，不联网。"""
import json
import tempfile
import unittest
from pathlib import Path
from unittest import mock


from web import library
from provenance import db


class FakeNet:
    def __init__(self, answers):
        self.answers, self.used, self.calls = answers, 0, []

    def get(self, url, params=None, **_):
        hashes = [v for k, v in params if k == 'search[pixel_hash][]']
        self.calls.append(hashes)
        out = []
        for h in hashes:
            answer = self.answers(h)
            if isinstance(answer, Exception):
                raise answer
            out += [dict(a, pixel_hash=h) for a in answer]
        return out


class AccountSourceTest(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name).resolve()
        self.lib = library.create(self.root / 'A.pilib')

    def tearDown(self):
        self.temp.cleanup()

    def archive(self):
        data = self.root / 'twitter-2026' / 'data'
        data.mkdir(parents=True)
        (data / 'following.js').write_text('window.YTD.following.part0 = ' + json.dumps(
            [{'following': {'accountId': '111', 'userLink': 'https://twitter.com/intent/user?user_id=111'}},
             {'following': {'accountId': '222', 'userLink': 'https://twitter.com/intent/user?user_id=222'}}]))
        (data / 'direct-messages.js').write_text('never read')
        return data.parent

    def test_pixiv_related_expands_verified_artists_only(self):
        from provenance.sources import pixiv_related
        with db.connect(self.lib) as c:
            verified = db.artist_for(c, [('pixiv', '10', '')], 'test')
            unverified = db.artist_for(c, [('pixiv', '20', '')], 'test')
            c.execute("INSERT INTO lookups(target,backend,last_result) VALUES (?, 'verify', 'hit')", ('artist:' + verified,))
        class Net:
            used = 0
            headers = {'www.pixiv.net': {'Cookie': 'PHPSESSID=1_x'}}
            def get(self, url, params=None, **_):
                assert '/user/10/' in url
                return {'body': {'recommendUsers': [{'userId': '10'}, {'userId': '30'}, {'userId': '20'}]}}
        self.assertEqual(pixiv_related.run(self.lib, Net(), log=lambda m: None)['candidates'], 2)
        with db.connect(self.lib) as c:
            self.assertEqual(sorted(r[0] for r in c.execute("SELECT l.key FROM seeds s JOIN artist_links l ON l.artist_id=s.artist_id WHERE s.source='pixiv_related'")),
                             ['20', '30'])
        self.assertEqual(pixiv_related.run(self.lib, Net(), log=lambda m: None)['expanded'], 0)   # one hop per artist
        class NoSession(Net):
            headers = {}
        self.assertIn('Pixiv', pixiv_related.run(self.lib, NoSession(), log=lambda m: None)['stopped'])
        class AppWindow(NoSession):      # login in the desktop app's Pixiv window: no Cookie header, routed via the window
            via = {'www.pixiv.net': object()}
        from provenance import accounts
        with mock.patch.object(accounts, '_meta', return_value={'pixiv': {'user_id': '1', 'via': 'app'}}):
            self.assertEqual(pixiv_related.run(self.lib, AppWindow(), log=lambda m: None)['stopped'], '')



if __name__ == '__main__':
    unittest.main()
