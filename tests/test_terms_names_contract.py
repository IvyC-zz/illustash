"""名字投影 N0（docs/TERMS_NAMES_PLAN.md）：在旧代码上钉住选名、搜索、历史文案的现有行为。

夹具见 tests/terms_names_fixture.py，每条分支一个身份。`snapshot` 是全量结果，N1 起核心的投影与它逐字对照。
N4：`SwitchedNamesContract` 照服务端顺序切换（标签 → 角色 → 名字），快照走切换后的读路径（`names.view` 工厂给出的
核心投影、核心的搜索），同一批断言原样通过，全量快照也与切换前逐字相等。"""
import random
import tempfile
import unittest
from pathlib import Path

from tests.terms_names_fixture import build, snapshot
from tests import terms_seed
from tests.test_terms_core import core
from web import names, terms_core
from web.server import adopt_characters, adopt_labels, adopt_names


class NamesContract(unittest.TestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.store, self.ids = build(Path(temp.name), legacy=True)
        self.switch()
        with self.store.connect() as c:
            self.snap = self.read(c)

    def switch(self):
        """固定旧输入完成真实启动切换后，按原只读算法逐字核对。"""
        terms_seed.switch(self.store)

    def read(self, c):
        return snapshot(c)

    def shown(self, ident, skin_only=False):
        e = self.snap['skin_only' if skin_only else 'entity'][ident]
        return e['name'], e['name_source']

    def test_works(self):
        self.assertEqual(self.shown('w-ba'), ('蔚蓝档案', 'builtin'))           # 词表压过 bangumi
        self.assertEqual(self.shown('w-th'), ('东方', 'user'))                  # 用户的中文名压过词表
        self.assertEqual(self.shown('w-q'), ('シリーズ', 'index'))              # 机翻中文排在 index 日文之后
        self.assertEqual(self.snap['entity']['w-q']['names'], {'zh': '模型名', 'ja': 'シリーズ', 'en': 'Series Q en'})
        self.assertEqual(self.shown('w-wiki'), ('维基名', 'wiki'))             # wiki 中文按 bangumi 排
        self.assertEqual(self.shown('w-none'), ('Series None', 'readable'))
        self.assertEqual(self.shown('w-user'), ('我的作品', 'user'))           # 作品自己的用户名字先于 qualifier
        self.assertEqual(self.snap['choose']['work:series_u']['name'], '词表外')
        self.assertEqual(self.shown('w-local'), ('本地作品', 'index'))         # 没有 qualifier：自己的名字
        self.assertEqual(self.snap['entity']['w-ba']['tag'], 'blue_archive')

    def test_characters(self):
        self.assertEqual(self.shown('c-prio'), ('b名', 'bangumi'))
        self.assertEqual(self.snap['entity']['c-prio']['names'], {'zh': 'b名', 'ja': 'i名', 'en': None})
        self.assertEqual(self.shown('c-pref'), ('zzz', 'index'))                # 首选先于来源
        self.assertEqual(self.shown('c-en'), ('En Only', 'readable'))           # 英文名不显示：可读的 tag（去掉 qualifier）
        self.assertEqual(self.snap['entity']['c-en']['names']['en'], 'English Index')
        self.assertEqual(self.shown('c-rep'), ('借来的名字', 'bangumi'))        # 合并的代表借成员的名字
        self.assertEqual(self.snap['choose']['character:rep_(blue_archive)']['name_source'], 'readable')
        self.assertEqual(self.shown('c-user'), ('我起的', 'user'))              # 有用户名字：只用自己的名字
        self.assertEqual(self.shown('c-local'), ('本地角色', 'user'))
        self.assertEqual(self.snap['entity']['c-local']['tag'], '')
        self.assertEqual(self.shown('c-oc'), ('OC名', 'user'))                  # 原创角色：只用自己的名字，但保留 tag
        self.assertEqual(self.snap['entity']['c-oc']['tag'], 'oc_tag')
        self.assertEqual(self.shown('c-noname'), ('未命名角色', 'readable'))
        self.assertEqual(self.shown('c-model'), ('机翻', 'model'))
        self.assertEqual(self.shown('c-wd'), ('Wd Only Tag', 'readable'))       # 只有 WD 的当前链接；退役的链接不算
        self.assertEqual(self.snap['entity']['c-wd']['tag'], 'wd_only_tag')

    def test_skins(self):
        self.assertEqual(self.shown('s-swim'), ('b名 · 泳装', 'builtin'))
        self.assertEqual(self.shown('s-swim', True), ('泳装', 'builtin'))
        self.assertEqual(self.snap['skin_only']['s-swim']['tag'], 'swimsuit')
        self.assertEqual(self.shown('s-former'), ('b名 · 墨缇斯', 'index'))      # 以前的人物：保留自己的名字
        self.assertEqual(self.shown('s-same'), ('b名 · Sleepwear', 'readable'))  # 与父同名：皮肤的可读名
        self.assertEqual(self.shown('s-shown'), ('b名 · 样式', 'bangumi'))
        self.assertEqual(self.shown('s-model'), ('机翻 · 女仆', 'model'))        # 父名是机翻：整名标成 model
        self.assertEqual(self.shown('s-compound'), ('zzz · 新年 · 和服', 'builtin'))
        self.assertEqual(self.shown('s-local'), ('便装的我', 'user'))            # 本地角色的皮肤：只用自己的名字，不拼父名

    def test_history_is_rendered_with_current_names(self):
        self.assertEqual(list(self.snap['labels'].values()), [
            '已将 2 张设为 b名', '已将 1 张移出 b名 · 泳装', '已将 3 张从 b名 换成 我起的', '已将 zzz 标为已检查（4 张）',
            '已将 借来的名字 合并到 借来的名字（2 条人工结论）', '随便什么文案'])

    def test_search(self):
        hits = self.snap['search']
        for query, ident in (('蔚蓝', 'c-rep'), ('碧蓝', 'c-rep'), ('东方', 'c-user'), ('泳装', 'c-prio'), ('女仆', 'c-model'),
                             ('b名 · 泳装', 'c-prio'), ('Q 系列', 'c-prio'), ('シリーズ', 'c-prio'), ('借来', 'c-mem'),
                             ('en only', 'c-en'), ('wd only', 'c-wd'), ('retired', 'c-wd'), ('爱丽丝', self.ids['alice'])):
            self.assertIn(ident, hits[query], query)
        self.assertEqual(hits['b名'], {'c-prio': 2})                            # 完全相同 = 2，开头 = 1，包含 = 0
        self.assertEqual(hits['Prio'], hits['prio'])

    def test_snapshot_is_stable(self):
        with self.store.connect() as c:
            self.assertEqual(self.read(c), self.snap)


class SwitchedNamesContract(NamesContract):
    """名字切到核心之后，同一批断言。"""
    @classmethod
    def setUpClass(cls):
        if core() is None:
            raise unittest.SkipTest('no terms core binary and no cargo')

    def switch(self):
        with self.store.connect() as c:
            self.before = snapshot(c)                           # 切换前旧代码的全量结果
        terms_seed.switch(self.store)
        with self.store.connect() as c:
            self.assertTrue(terms_core.names_in_core(c))

    def read(self, c):
        self.assertIsInstance(names.view(c), names.ProjectedView)
        return snapshot(c, view=names.view, search=names.search_hits)

    def test_app_read_path_equals_the_old_code(self):
        self.assertEqual(self.snap, self.before)
        with self.store.connect() as c:                         # 切换后的数据上，旧代码的算法仍给出同样的结果
            self.assertEqual(snapshot(c), self.before)


    def test_core_search_equals_the_old_search(self):
        with self.store.connect() as c:
            compare_search(self, c)

    def test_keys_outside_the_projection(self):
        """投影只收库里出现的键；别的键（任意 tag 的 work_name、词表里有但库里没有的作品）由 View 选，结果相同。"""
        with self.store.connect() as c:
            for kind, key in (('work', 'not_a_known_series'), ('work', 'pokemon'), ('skin', 'maid_dress'),
                              ('character', 'nobody_(nowhere)')):
                shown = names.view(c)
                shown.load()
                self.assertNotIn((kind, key), shown.chosen)
                self.assertEqual(names.view(c).choose(kind, key), names.View(c).choose(kind, key), key)
            self.assertEqual(c.execute("SELECT work_name('not_a_known_series')").fetchone()[0], 'Not A Known Series')
            self.assertEqual(c.execute("SELECT work_name('pokemon')").fetchone()[0], '宝可梦')


class SearchEdges(SwitchedNamesContract):
    """夹具没有的两种数据形状，切换前补进旧表：作品的两个名字都命中时，旧代码按行的顺序后者覆盖前者（abc 完全相同、
    abcd 只是包含，结果是 0 不是 2）；只在 casefold 下才对上的比较（sß 按 casefold 是 sss，以 ß 即 ss 开头）。"""
    def switch(self):
        with self.store.connect() as c:
            c.execute("INSERT INTO entities(id,name,kind) VALUES ('w-edge','','work')")
            c.execute("INSERT INTO entities(id,name,kind,work_id) VALUES ('c-edge','','character','w-edge')")
            c.execute("""INSERT INTO links(system,key,entity_id,current,source,added_at)
                         VALUES ('danbooru','edge_tag','c-edge',1,'import','2026-09-01 00:00:00')""")
            for ident, source, value in (('w-edge', 'index', 'abc'), ('w-edge', 'bangumi', 'abcd'), ('c-edge', 'index', 'sß')):
                c.execute("INSERT INTO names(entity_id,lang,value,source,role) VALUES (?,'zh',?,?,'display')", (ident, value, source))
        super().switch()

    def test_edges(self):
        with self.store.connect() as c:
            self.assertEqual(names.search_hits_python(c, 'abc'), {'w-edge': 2, 'c-edge': 0})   # 作品自己按名字是 2
            self.assertEqual(names.search_hits_python(c, 'ß'), {'c-edge': 1})
            for q in ('abc', 'abcd', 'ß', 'sß', 'SS'):
                self.assertEqual(names.search_hits(c, q), names.search_hits_python(c, q), q)

    def test_app_read_path_equals_the_old_code(self):
        self.assertEqual(self.snap, self.before)


def compare_search(test, c):
    """夹具里每个名字、别名、链接 key、文字键和皮肤词，原样、改大小写、随机切片，加上随机拼的「父 · 皮肤」：核心的搜索
    与旧的 search_hits 逐个相等。"""
    terms = sorted({r[0] for r in c.execute(
        """SELECT value FROM names UNION SELECT value FROM display_names UNION SELECT value FROM name_aliases
           UNION SELECT key FROM links UNION SELECT work FROM entities UNION SELECT variant FROM entities
           UNION SELECT artist FROM entities""") if r[0]}
        | {names.skin_zh(v) for v in names.SKINS} | set(names.WORK_NAMES.values()))
    pick = random.Random(20260929)
    queries = set(terms) | {'Straße', 'STRASSE', '%', '_', '\\', '·', ' · '}
    for t in terms:
        queries |= {t.upper(), t.casefold(), names.readable(t, True), t.replace('_', ' ')}
        i = pick.randrange(len(t))
        queries |= {t[i:], t[:i + 1], t[i:i + 2]}
    for _ in range(200):
        queries.add(pick.choice(terms) + ' · ' + pick.choice(terms))
    queries = sorted(q for q in queries if q and q.strip() == q)
    test.assertGreater(len(queries), 300)
    for q in queries:
        test.assertEqual(names.search_hits(c, q), names.search_hits_python(c, q), q)


if __name__ == '__main__':
    unittest.main()
