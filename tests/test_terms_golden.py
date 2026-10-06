"""写路径固定快照：完整核心形态逐步核对删除旧 Python 写路径前录下的行为。

保留每个入口、拒绝原文、返回值、状态差分、历史与连续撤销的全部原断言；移除显式 old 模式。
"""
import json
import unittest

from tests import golden_writes
from tests.test_terms_core import core


def first_difference(want, got):
    if want['initial'] != got['initial']:
        tables = [t for t in want['initial'] if want['initial'][t] != got['initial'].get(t)]
        return f'夹具的初始状态不同：{tables}'
    if want['ids'] != got['ids']:
        return f"夹具的 id 不同：{want['ids']} / {got['ids']}"
    for i, (a, b) in enumerate(zip(want['steps'], got['steps'])):
        if a != b:
            return (f"第 {i + 1} 步 {json.dumps(a['call'], ensure_ascii=False)}：\n快照 {json.dumps(a, ensure_ascii=False)[:1500]}\n"
                    f"实际 {json.dumps(b, ensure_ascii=False)[:1500]}")
    if len(want['steps']) != len(got['steps']):
        return f"步数不同：{len(want['steps'])} / {len(got['steps'])}"
    for i, (a, b) in enumerate(zip(want['undo'], got['undo'])):
        if a != b:
            return f"撤销第 {i + 1} 步：\n快照 {json.dumps(a, ensure_ascii=False)[:1500]}\n实际 {json.dumps(b, ensure_ascii=False)[:1500]}"
    if len(want['undo']) != len(got['undo']):
        return f"撤销步数不同：{len(want['undo'])} / {len(got['undo'])}"
    if want['final'] != got['final']:
        return '撤销到底之后的状态不同'
    return None


class GoldenWrites(unittest.TestCase):
    policy = None

    @classmethod
    def setUpClass(cls):
        if core() is None:
            raise unittest.SkipTest('no terms core binary and no cargo')
        cls.golden = golden_writes.load(cls.policy)

    def check(self, mode):
        got = json.loads(json.dumps(golden_writes.run(self.policy, mode), ensure_ascii=False))
        self.assertIsNone(first_difference(self.golden, got))

    def test_core_writes_match_the_snapshot(self):
        self.check('core')

    def test_core_writes_with_names_in_core_match_the_snapshot(self):
        self.check('names')

    def test_the_snapshot_covers_every_write_entry(self):
        methods = {s['call'][0] for s in self.golden['steps']}
        self.assertLessEqual({'tag', 'judge_artist', 'change', 'answer', 'answer_audit', 'reassign', 'sweep',
                              'uncertain_identity', 'decide_identity', 'set_profile',
                              'link_local', 'create_entity', 'create_linked', 'shared_oc', 'set_kind', 'set_mascot',
                              'set_work', 'edit_work', 'merge', 'apply_work_evidence'}, methods)
        for method in methods:          # 每个入口都至少有一次成功、一次拒绝（策略未设置时身份类入口只有拒绝）
            kinds = {s['outcome'][0] for s in self.golden['steps'] if s['call'][0] == method}
            self.assertIn('ValueError', kinds | ({'ValueError'} if method in ('shared_oc', 'create_linked', 'apply_work_evidence') else set()), method)
        self.assertTrue(self.golden['undo'])
        self.assertTrue(all(u['outcome'][0] == 'ok' for u in self.golden['undo']))


class GoldenWritesHybrid(GoldenWrites):
    policy = 'hybrid'


class GoldenNameWrites(unittest.TestCase):
    """NameUpdater 写名字（不进历史）：核心写入与旧路径固定快照逐步相等。"""
    @classmethod
    def setUpClass(cls):
        if core() is None:
            raise unittest.SkipTest('no terms core binary and no cargo')
        cls.golden = json.loads(golden_writes.names_path().read_text(encoding='utf-8'))

    def check(self, mode):
        got = json.loads(json.dumps(golden_writes.run_names(mode), ensure_ascii=False))
        self.assertEqual(got['initial'], self.golden['initial'])
        for want, step in zip(self.golden['steps'], got['steps']):
            self.assertEqual(step, want, want['call'])
        self.assertEqual(len(got['steps']), len(self.golden['steps']))

    def test_core_name_writes_match_the_snapshot(self):
        self.check('names')

    def test_the_snapshot_writes_something(self):
        self.assertGreaterEqual(sum(bool(s['diff']) for s in self.golden['steps']), 8)   # 没有主人的 tag、非法来源不写
        self.assertIn('ValueError', {s['outcome'][0] for s in self.golden['steps']})


if __name__ == '__main__':
    unittest.main()
