"""CCIP 长尾参考包（#559）走原生 HTTP 与同步入口：导入、启用、同步后的自动检查、晚到的取消、撤回。

参考包、基础角色索引和识别结果都是合成的：同步用 test_native_sync 的合成 worker（每张图的 CCIP 向量都是全 1），
参考包经 ILLUSTASH_TEST_CCIP_LONGTAIL 换成本机合成包（tools/ui_walk/fixture.py 的 ccip_longtail_spec），
一个长尾角色的锚点正对这个方向，所以每张同步进来的图都命中它。
"""
import json
import struct
import unittest

from tests.folder_fixture import image
from tests.test_native_wd_extension import SyntheticWdLibrary
from tests import test_native_sync as sync_fixture
from tests.test_read_http import wait_until
from tools.ui_walk.fixture import ccip_longtail_spec

ROUTE = '/api/ccip-longtail'
ROLE = 'synthetic_longtail_(synthetic)'


class NativeCcipLongtailTest(SyntheticWdLibrary, unittest.TestCase):
    worker = sync_fixture.NativeSyncTest.worker

    def state(self, action=None, body=None, status=200):
        response = self.request(ROUTE + (action or ''), body)
        self.assertEqual(response['status'], status, response['body'])
        return json.loads(response['body'])

    def finish(self, kind):
        observed = None

        def ended():
            nonlocal observed
            observed = self.state()
            task = observed['task']
            return task and task['kind'] == kind and task['status'] != 'running'

        wait_until(ended)
        return observed

    def roles(self, uuid):
        detail = json.loads(self.request('/api/detail?id=' + uuid)['body'])
        return [(m['status'], m['evidence']['sources']) for m in detail['memberships']]

    def test_import_enable_auto_late_cancel_and_withdraw(self):
        self.worker()
        self.assertEqual(self.request('/api/sync', {})['status'], 200)
        wait_until(lambda: json.loads(self.request('/api/sync')['body'])['state']['status'] == 'done')
        anchor = struct.pack('<768f', *([1.] * 768))
        manifest = ccip_longtail_spec(self.root / 'download', self.root / 'shared', [(ROLE, anchor)], 'http://127.0.0.1:1')
        self.env['ILLUSTASH_TEST_CCIP_LONGTAIL'] = str(manifest)
        self.restart()

        # 没导入：包没就位、本图库没启用，启用回 409。
        before = self.state()
        self.assertEqual((before['package']['status'], before['package']['heads']), ('missing', 1))
        self.assertEqual((before['library']['enabled'], before['library']['vectors']), (False, 1))
        self.state('/enable', {}, status=409)

        # 导入：只收下包，不启用。
        imported = self.state('/package', {'action': 'import', 'paths': [str(manifest.with_name('A-longtail-p75.zip'))]})
        wait_until(lambda: self.state()['package']['status'] == 'ready')
        self.assertFalse(imported['library']['enabled'])

        # 启用：唯一的图补上长尾角色的机器归属。
        self.state('/enable', {})
        enabled = self.finish('enable')
        self.assertEqual(enabled['task']['status'], 'done', enabled)
        self.assertEqual(enabled['task']['result']['changed'], 1)
        self.assertEqual((enabled['library']['enabled'], enabled['library']['images'], enabled['library']['assigned']), (True, 1, 1))
        first = json.loads(self.request('/api/assets?offset=0&limit=10')['body'])['items'][0]['uuid']
        self.assertEqual(self.roles(first), [('machine', ['CCIP_LONGTAIL'])])

        # 取消晚到（任务已经结束）：什么都不做，回结束后的状态，不当错误。
        late = self.state('/cancel', {'id': enabled['task']['id']})
        self.assertEqual(late['task']['status'], 'done')

        # 同步后的自动检查：新识别的图也补上，任务种类是 auto。
        image(self.new_folder / 'second.png', 'orange')
        self.assertEqual(self.request('/api/sync', {})['status'], 200)
        auto = self.finish('auto')
        self.assertEqual(auto['task']['status'], 'done', auto)
        self.assertEqual(auto['task']['result']['changed'], 1)
        self.assertEqual((auto['library']['images'], auto['library']['assigned']), (2, 2))

        # 撤回：两张图都回到没有角色。
        self.state('/withdraw', {})
        withdrawn = self.finish('withdraw')
        self.assertEqual(withdrawn['task']['status'], 'done', withdrawn)
        self.assertEqual(withdrawn['task']['result']['changed'], 2)
        self.assertEqual((withdrawn['library']['enabled'], withdrawn['library']['images'], withdrawn['library']['assigned']),
                         (False, 0, 0))
        self.assertEqual(self.roles(first), [])
