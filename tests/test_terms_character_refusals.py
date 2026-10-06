"""C3 拒绝固定快照：旧写路径先录制，核心公开入口再逐步核对。

从保存的旧形态输入执行真实启动切换。每次校验输入、拒绝类型及原文、返回值、完整状态差分、版权查询序列；
最终校验全部状态与原始历史文案，另由核心全量核对。已有快照不可重录。
"""
import itertools
import json
import random
import tempfile
import unittest
import uuid
from pathlib import Path
from unittest import mock

from tests.test_terms_core import core
from tests import golden_writes, terms_seed
from web import terms_core


def ids():
    n = iter(range(10**6))
    return lambda: uuid.UUID(int=next(n) + 1)


def outcome(call, fresh):
    """是否拒绝、拒绝的文案。核心的拒绝经 Store._core 转成 Invalid（ValueError 的子类），旧代码有的抛 ValueError：
    HTTP 层（tests/reference/web/server.py）对两者一样处理（400 + 原文），所以按 ValueError 归类，文案逐字比较；其他异常类型原样比较。"""
    try:
        with mock.patch('uuid.uuid4', side_effect=fresh):      # 两边的新 id 是同一串
            result = call()
    except ValueError as exc:
        return 'ValueError', str(exc)
    except Exception as exc:  # noqa: BLE001 - 类型和文案都要比
        return type(exc).__name__, str(exc)
    if isinstance(result, dict):
        # 历史步骤的时间：两边可能跨过整秒（步骤本身，或返回值里的 undo）。
        result = {k: {x: y for x, y in v.items() if x != 'ts'} if k == 'undo' and isinstance(v, dict) else v
                  for k, v in result.items() if k != 'ts'}
    return 'ok', json.dumps(result, ensure_ascii=False, sort_keys=True, default=str) if isinstance(result, (dict, list)) else ''


class Refusals(unittest.TestCase):
    policy = 'hybrid'
    names_in_core = False

    @classmethod
    def setUpClass(cls):
        if core() is None:
            raise unittest.SkipTest('no terms core binary and no cargo')

    def setUp(self):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        root = Path(temp.name)
        self.new, attrs = terms_seed.load('refusals_' + (self.policy or 'none'), root)
        for name, value in attrs.items():
            setattr(self, name, value)
        terms_seed.switch(self.new)
        self.fresh = ids()
        self.mark = self.last_id(self.new)
        key = f"terms_refusals_{self.policy or 'none'}_{self._testMethodName}.json"
        self.expected = json.loads((golden_writes.GOLDEN / key).read_text(encoding='utf-8'))
        self.current = golden_writes.state(self.new)
        self.assertEqual(self.current, self.expected['initial'], key)
        self.step = 0
        query_path = golden_writes.GOLDEN / f"terms_refusal_queries_{self.policy or 'none'}_{self._testMethodName}.json"
        self.query_expected = json.loads(query_path.read_text(encoding='utf-8')) if query_path.exists() else []
        self.queries = {r['step']: r['queries'] for r in self.query_expected}

    def tearDown(self):
        self.assertEqual(self.step, len(self.expected['steps']))
        self.assert_final()

    def assert_final(self):
        self.assertEqual(golden_writes.state(self.new), self.expected['final'])

    @staticmethod
    def last_id(store):
        with store.connect() as c:
            return c.execute('SELECT coalesce(max(id),0) FROM actions').fetchone()[0]

    def history(self, store):
        """这次测试写下的历史：原始文案（核心写的，不经 action_label 重新渲染）。"""
        with store.connect() as c:
            return [r[0] for r in c.execute('SELECT label FROM actions WHERE id>? ORDER BY id', (self.mark,))]

    def settled(self):
        """原始历史文案与全部状态对照旧路径固定答案；核心全量核对仍须零差异。"""
        self.assertEqual(self.history(self.new), [r[1] for r in self.expected['final']['actions'][1:] if r[0] > self.mark])
        self.assert_final()
        terms_core.run('verify-characters', '--db', self.new.path)
        terms_core.run('verify-names', '--db', self.new.path)

    def same(self, method, /, **kwargs):
        self.assertLess(self.step, len(self.expected['steps']))
        want = self.expected['steps'][self.step]
        call = json.loads(json.dumps([method, golden_writes.shown(kwargs)], ensure_ascii=False))
        self.assertEqual(call, want['call'])
        actual, queries = dict(kwargs), []
        if callable(kwargs.get('copyright_of')):
            def copyright_of(tag):
                result = kwargs['copyright_of'](tag)
                queries.append([tag, result])
                return result
            actual['copyright_of'] = copyright_of
        got = outcome(lambda: getattr(self.new, method)(**actual), self.fresh)
        self.assertEqual(list(got), want['outcome'], kwargs)
        self.assertEqual(queries, self.queries.get(self.step, []), kwargs)
        after = golden_writes.state(self.new)
        self.assertEqual(golden_writes.diff(self.current, after), want['diff'], kwargs)
        self.current = after
        self.step += 1
        return got

    def test_decide_identity(self):
        a, b, w = self.alice, self.bob, self.work
        ids = [None, 0, -1, True, 1.5, 'x', 999999]
        ends = [dict(a=a, b=b, kind='duplicate_of'), dict(a=a, b=a, kind='duplicate_of'), dict(a=a, b=b, kind='x'),
                dict(a=a, b='nope', kind='form_of'), dict(a=a, b=w, kind='different_from'), dict(a=None, b=b, kind='form_of'),
                dict(a=a, b=b, kind=None)]
        statuses = ['accepted', 'foo', None, 5]
        reasons = ['', '  ', 'ok', None, 7]
        messages = {}
        for rid, status, reason in itertools.product(ids, statuses, reasons):
            for end in (ends if rid is None else [{}]):
                args = dict(relation_id=rid, status=status, reason=reason, **end)
                kind, text = self.same('decide_identity', **args)
                messages[text] = messages.get(text, 0) + 1
        self.assertEqual(sum(messages.values()), 260)
        self.assertGreaterEqual(len(messages), 2 if self.policy is None else 5, messages)   # 各种拒绝都走到了
        # 合法的调用，以及合法之后对同一条关系的再决议、不存在的关系。
        for args in (dict(a=a, b=b, kind='different_from', status='accepted', reason='不是同一人'),
                     dict(a=b, b=a, kind='different_from', status='rejected', reason='再看一次'),
                     dict(a=a, b=b, kind='duplicate_of', reason='只提议'),
                     dict(relation_id=1, status='proposed', reason='退回'),
                     dict(relation_id=999999, status='accepted', reason='不存在')):
            self.same('decide_identity', **args)
        self.assert_final()

    def test_merge(self):
        """hybrid 下合并不再预演（= 一条 duplicate_of 决议）；策略未设置时照旧预演。两种都比。"""
        a, b, w = self.alice, self.bob, self.work
        with self.new.connect() as c:
            skin = c.execute("SELECT id FROM entities WHERE parent_id IS NOT NULL LIMIT 1").fetchone()
        for source, target in ((a, a), (a, 'nope'), ('nope', b), (a, w), (w, a), (None, b), (a, None), (3, b),
                               *(((skin[0], a), (a, skin[0])) if skin else ())):
            self.same('merge', source=source, target=target)
        self.same('merge', source=a, target=b)
        self.same('merge', source=a, target=b)                       # 再合并一次
        self.assert_final()
        self.settled()

    def test_create_entity(self):
        for name, kind, artist in itertools.product(['', '  ', None, 'x' * 121, 5, 'Alice', 'alice_tag', '爱丽丝', '新角色甲'],
                                                    ['', 'oc', 'bad', None], ['', 'someone', 'y' * 121, 3]):
            self.same('create_entity', name=name, kind=kind, artist=artist)
        self.same('create_entity', name='新角色甲')                  # 刚建的同名角色：返回已有的
        self.same('create_entity', name=None, kind='oc')              # 原创角色 N 接着编号
        self.assert_final()

    def test_create_linked(self):
        asked = []
        def copyright_of(tag):
            asked.append(tag)
            return 'vocaloid' if tag.endswith('_(vocaloid)') else 'series_x' if tag.endswith('_(series_x)') else ''
        for tag, typed in itertools.product(['', 'a b', None, 7, 'alice_tag', 'Alice_Tag'], ['', 'Kaito', None]):
            self.same('create_linked', tag=tag, typed=typed, copyright_of=copyright_of)
        # 每种输入的名字都配新的 tag，才会真的新建：别名与 tag 相同时不存、过长截断、已有作品 / 新建作品 / 没有作品。
        for i, typed in enumerate(['', 'Kaito', None, 'z' * 130, 7]):
            for tag in (f'kaito{i}_(vocaloid)', f'zed{i}_(series_x)', f'lone{i}', f'kaito{i}_(vocaloid)'):
                self.same('create_linked', tag=tag, typed=typed, copyright_of=copyright_of)
            self.same('create_linked', tag=f'hero{i}_(series_x)', typed=f'Hero{i} (series x)', copyright_of=copyright_of)
            self.same('create_linked', tag=f'hero{i}b', typed=f'hero{i}b', copyright_of=copyright_of)
        self.assert_final()
        self.assertEqual(asked, [query[0] for row in self.query_expected for query in row['queries']])                            # 两边查版权的次数一样


    # ---- 名字 N5：其余写入口 ----

    def test_change(self):
        a, b, sk, w = self.alice, self.bob, self.skin, self.work
        for action, entity, ids in itertools.product(
                ['confirmed', 'rejected', 'unknown', 'resume', 'add_variant', 'remove_variant', 'bad', None],
                [a, sk, 'nope', None, w, 5], [['a'], ['b', 'c'], [], None, 'a', ['missing'], [1]]):
            self.same('change', ids=ids, action=action, entity=entity)
        for kwargs in (dict(entities=[a, b]), dict(entities=(a,)), dict(entities=[]), dict(entities=[a] * 21),
                       dict(entities=[a], entity=a), dict(entities=[a, 'nope']), dict(entities=[b, sk])):
            for action in ('confirmed', 'rejected'):
                self.same('change', ids=['b'], action=action, **kwargs)
        for kwargs in (dict(ids=['a'], filters={}), dict(ids=None, filters={'status': 'bad'}),
                       dict(ids=None, filters={'q': 'a'}), dict(ids=None, filters={'entity': a}, exclude=['a']),
                       dict(ids=['a'], exclude=['b']), dict(ids=None, filters={}, exclude='x'), dict(ids=None, filters=5)):
            for action, entity in (('confirmed', b), ('bad', b), ('confirmed', 'nope')):
                self.same('change', action=action, entity=entity, **kwargs)
        self.settled()

    def test_answer(self):
        a, b, sk = self.alice, self.bob, self.skin
        lists = [dict(yes=['a']), dict(no=['b']), dict(later=['c']), dict(yes=['a'], no=['b']), dict(),
                 dict(yes='a'), dict(yes=('c',)), dict(yes=['a', 'a']), dict(no=['missing']), dict(yes=[1])]
        for entity, kind, switch, answers in itertools.product([a, sk, 'nope', None], ['seed', 'audit', 'bad'],
                                                               [None, b, a, sk, 'nope'], lists):
            self.same('answer', entity=entity, kind=kind, switch=switch, **answers)
        self.settled()

    def test_answer_audit(self):
        for yes, no in itertools.product([['a'], [], 'a', ['b'], ['a', 'a'], [1], ('a',)], [['a'], [], ['c'], None]):
            self.same('answer_audit', yes=yes, no=no if no is not None else ())
        self.settled()

    def test_reassign(self):
        a, b, sk = self.alice, self.bob, self.skin
        for ids, old, new in itertools.product([['b'], ['a'], ['c'], None, [], ['missing']], [a, b, sk, 'nope', None],
                                               [b, a, [b, a], [], [b] * 21, 'nope', (b,), sk, [a, a]]):
            self.same('reassign', ids=ids, old_entity=old, new_entity=new)
        self.same('reassign', ids=None, filters={'entity': a}, old_entity=a, new_entity=b)
        self.same('reassign', ids=None, filters={'entity': a}, exclude=[5], old_entity=a, new_entity=b)
        self.settled()

    def test_sweep(self):
        for entity in (self.alice, self.bob, self.skin, 'nope', None, self.work, 5, self.alice):
            self.same('sweep', entity=entity)
        self.settled()

    def test_uncertain_identity(self):
        a, b = self.alice, self.bob
        for ids, options, reason in itertools.product([['a'], ['a', 'c'], [], None, ['missing'], 'a'],
                                                      [[a, b], [a], (a, b), [a, a], [a, 'nope'], 'x', [a, 5]],
                                                      ['r', '', ' ', None]):
            self.same('uncertain_identity', ids=ids, options=options, reason=reason)
        self.settled()

    def test_correct_identity_anchor(self):
        for entity, reference, decision, reason in itertools.product(
                [self.alice, 'nope', None, 5], ['123', 123, True, 'x', '５', -1, '', 1.5],
                ['exclude', 'restore', 'bad'], ['r', '', None]):
            self.same('correct_identity_anchor', entity=entity, reference=reference, decision=decision, reason=reason)
        self.settled()

    def test_set_kind_mascot_work(self):
        a, sk, w = self.alice, self.skin, self.work
        for entity, kind, artist in itertools.product([a, sk, 'nope', None, self.local], ['', 'oc', 'bad', None],
                                                      ['', 'someone', None, 'y' * 121]):
            self.same('set_kind', entity=entity, kind=kind, artist=artist)
        for entity, mascot in itertools.product([self.bob, sk, 'nope', None], [True, False, 1, None]):
            self.same('set_mascot', entity=entity, mascot=mascot)
        for entity, work in itertools.product([self.bob, sk, 'nope', None, self.local2, a],
                                              [w, '', None, 'nope', 5, True, 0, self.bob]):
            self.same('set_work', entity=entity, work=work)
        self.settled()

    def test_shared_oc(self):
        self.same('shared_oc')
        self.same('shared_oc')
        self.settled()

    def copyright_of(self, tag):
        return 'series_x' if tag.endswith('_(series_x)') else 'series_y' if tag.endswith('_(series_y)') else ''

    def test_set_profile(self):
        a, sk = self.alice, self.skin
        base = dict(entity=a, name=None, aliases=None, work=None, tag=None)
        choices = dict(entity=[a, sk, 'nope', None, self.local, self.bob],
                       name=[None, '', '  ', '新名字', 'x' * 121, 5, 'アリス', ' 爱丽丝 '],
                       aliases=[None, [], ['甲', '甲', ' 乙 '], 'x', [' '], ['y' * 121], ['a'] * 51],
                       work=[None, '', self.work, 'nope', 5],
                       tag=[None, '', 'bob_tag', 'alice_tag', 'hero_(series_x)', 'a b', 5, 'Hero_(Series_Y)'])
        calls = [base | {k: v} for k, vs in choices.items() for v in vs]
        pick = random.Random(7)
        calls += [{k: pick.choice(vs) for k, vs in choices.items()} for _ in range(150)]
        for kwargs in calls:
            self.same('set_profile', copyright_of=self.copyright_of, **kwargs)
        self.settled()

    def test_link_local(self):
        l1, l2 = self.local, self.local2
        for items in ([], 'x', [5], [{'id': l1, 'tag': 'a b'}], [{'id': l1}], [{'id': 'nope', 'tag': 'nope_tag'}],
                      [{'id': l1, 'tag': 'bob_tag'}], [{'id': l1, 'tag': 'x'}] * 201, [{'id': self.skin, 'tag': 'sk_tag'}],
                      [{'id': l1, 'tag': 'hero_(series_x)'}, {'id': l2, 'tag': 'heroine_(series_x)'}],
                      [{'id': l2, 'tag': 'solo_(series_y)'}]):
            self.same('link_local', items=items, copyright_of=self.copyright_of)
        self.settled()


class RefusalsWithoutPolicy(Refusals):
    """策略未设置：旧代码先查参数类型、再查策略（「请先完成两级身份迁移」）；合并是破坏式合并，文案用合并之后的名字。"""
    policy = None


class RefusalsWithNamesInCore(Refusals):
    """名字也切到核心：文案里的名字读核心的显示投影。"""
    names_in_core = True


class RefusalsWithoutPolicyWithNamesInCore(Refusals):
    policy = None
    names_in_core = True


if __name__ == '__main__':
    unittest.main()
