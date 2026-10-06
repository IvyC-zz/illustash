"""拆脚手架阶段 C2（用户决定 2026-09-30）：已有的旧形态图库启动时切到词条核心，切换失败就只读打开，提示原因。

旧表不再被写：Store 的写方法给出原因；Python 连接上对图库文件的任何改动被 SQLite 授权回调拒绝（读用到的临时表照常）；
核心的写命令拒绝这个库；HTTP 上改这个图库的接口返回 409 和原因，切换图库、账号、缓存、备份照常。切换前的备份留着，
下次启动再试一次。"""
import contextlib
import io
import json
import sqlite3
import tempfile
import threading
import unittest
import urllib.error
import urllib.request
from pathlib import Path
from unittest import mock

from tests.test_terms_core import core
from tests import terms_seed
from web import terms_core
from web.server import Server, adopt_characters, adopt_labels, adopt_terms
from web.store import Invalid, Store


class ReadOnly(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        if core() is None:
            raise unittest.SkipTest('no terms core binary and no cargo')

    def setUp(self):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.root = Path(temp.name)
        self.store = terms_seed.base(self.root)
        self.alice = self.store.entities('Alice')[0]['id']
        with contextlib.redirect_stdout(io.StringIO()):
            self.assertTrue(adopt_labels(self.store))
            with mock.patch.object(terms_core, 'cutover_characters', side_effect=terms_core.Failed('模拟：核对没有通过')):
                self.assertFalse(adopt_characters(self.store))

    def dump(self):
        with sqlite3.connect(self.store.path) as c:
            return '\n'.join(c.iterdump())

    def test_a_failed_switch_opens_the_library_read_only(self):
        reason = self.store.read_only
        self.assertIn('模拟：核对没有通过', reason)
        self.assertIn('只读', reason)
        before = self.dump()
        with self.assertRaisesRegex(Invalid, '只读'):
            self.store.change(['b'], 'confirmed', self.alice)
        with self.assertRaisesRegex(Invalid, '只读'):
            self.store.set_kind(self.alice, 'oc', 'x')
        with self.assertRaises(sqlite3.DatabaseError):          # 没有专门检查的写入口：授权回调兜底
            self.store.curate(['a'], 'exclude')
        with self.assertRaises(sqlite3.DatabaseError), self.store.connect() as c:
            c.execute("UPDATE entities SET artist='x'")
        with self.assertRaisesRegex(terms_core.Failed, '只读'):
            terms_core.character_command(self.store.path, {'op': 'sweep', 'entity': self.alice})
        self.assertEqual(self.dump(), before)

    def test_reads_keep_working(self):
        self.assertTrue(self.store.characters())
        self.assertTrue(self.store.entities('Alice'))
        self.assertTrue(self.store.assets(query='Alice')['items'])    # 搜索用临时表
        self.assertTrue(self.store.detail('a'))

    def test_no_further_switch_is_tried(self):
        with mock.patch('web.server.adopt_labels') as labels:
            adopt_terms(self.store)
        labels.assert_not_called()

    def test_http_refuses_changes_to_the_library_and_says_why(self):
        server = Server(('127.0.0.1', 0), self.store, self.root / 'cache')
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        self.addCleanup(lambda: (server.shutdown(), thread.join(), server.server_close()))

        def call(path, body=None):
            headers = {'Content-Type': 'application/json', 'Origin': server.origin, 'X-CSRF-Token': server.token} if body is not None else {}
            request = urllib.request.Request(server.origin + path, data=json.dumps(body).encode() if body is not None else None, headers=headers)
            try:
                with urllib.request.urlopen(request, timeout=5) as r:
                    return r.status, json.loads(r.read())
            except urllib.error.HTTPError as e:
                return e.code, json.loads(e.read())

        status, boot = call('/api/bootstrap')
        self.assertEqual((status, boot['read_only']), (200, self.store.read_only))
        status, body = call('/api/change', {'ids': ['b'], 'action': 'confirmed', 'entity': self.alice})
        self.assertEqual((status, body['error']), (409, self.store.read_only))
        self.assertEqual(call('/api/cache', {'action': 'clear'})[0], 200)   # 不改这个图库的操作照常


if __name__ == '__main__':
    unittest.main()
