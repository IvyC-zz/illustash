"""名字投影 N1（docs/TERMS_NAMES_PLAN.md）：核心的显示投影与旧代码（tests/reference/web/names.py 的 View）全量逐字相等。

夹具与快照见 tests/terms_names_fixture.py。核心只读名字的兼容表，写自己的两张投影表 term_name_display、term_name_choice；
这里把它们读回成 View 的形状，与快照比较：每个身份的完整名与皮肤短名、每个文字键的 choose。"""
import tempfile
import unittest
from pathlib import Path

from tests.terms_names_fixture import build, snapshot
from tests import terms_seed
from tests.test_terms_core import core
from web import names, terms_core


def projected(c):
    """两张投影表，读成与 snapshot 相同的形状。"""
    shown = {0: {}, 1: {}}
    for r in c.execute('SELECT * FROM term_name_display'):
        shown[r['skin_only']][r['entity']] = {'name': r['name'], 'name_source': r['name_source'],
                                               'names': {l: r[l] for l in ('zh', 'ja', 'en')}, 'tag': r['tag']}
    choice = {f"{r['kind']}:{r['key']}": {'name': r['name'], 'name_source': r['name_source'],
                                            'names': {l: r[l] for l in ('zh', 'ja', 'en')}}
              for r in c.execute('SELECT * FROM term_name_choice')}
    return shown[0], shown[1], choice


class NamesProjection(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        if core() is None:
            raise unittest.SkipTest('no terms core binary and no cargo')

    def setUp(self):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.store, self.ids = build(Path(temp.name))
        terms_core.run('migrate', '--db', self.store.path)

    def assert_same(self):
        out = terms_core.run('names-project', '--db', self.store.path)
        with self.store.connect() as c:
            snap = snapshot(c)
            entity, skin_only, choice = projected(c)
            view = names.View(c)
            extra = {k: view.choose(*k.split(':', 1)) for k in choice if k not in snap['choose']}
        self.assertEqual(entity, snap['entity'])
        self.assertEqual(skin_only, snap['skin_only'])
        self.assertEqual({k: choice[k] for k in snap['choose']}, snap['choose'])
        self.assertEqual({k: choice[k] for k in extra}, {k: {x: v[x] for x in ('name', 'name_source', 'names')} for k, v in extra.items()})
        self.assertEqual(out['shown'], 2 * len(snap['entity']))
        return snap

    def test_every_identity_and_key_matches_the_old_view(self):
        snap = self.assert_same()
        self.assertGreater(len(snap['choose']), 25)

    def test_after_name_changes_it_still_matches(self):
        """改名、首选、合并、退役链接、词表作品的用户名字：每改一处都整表重算并全量对照。"""
        changes = [
            "INSERT INTO names(entity_id,lang,value,source,role,preferred) VALUES ('c-prio','zh','用户名','user','display',0)",
            "UPDATE names SET preferred=1 WHERE entity_id='c-pref' AND value='aaa'",
            "UPDATE entities SET merged_into=NULL WHERE id='c-mem'",
            "UPDATE links SET current=0 WHERE entity_id='c-wd' AND system='wd'",
            "INSERT INTO display_names VALUES ('work','blue_archive','zh','user','我的蔚蓝')",
            "DELETE FROM names WHERE entity_id='s-former'",
            "UPDATE entities SET original=1 WHERE id='c-model'",
            "INSERT INTO display_names VALUES ('skin','maid','zh','model','机翻女仆')",
            "INSERT INTO display_names VALUES ('skin','sleepwear','zh','bangumi','睡衣')",
        ]
        for step, sql in enumerate(changes):
            with self.subTest(sql=sql):
                # 这些 SQL 是旧库的启动输入；每一累计状态先收编，再验收完整核心投影。
                temp = tempfile.TemporaryDirectory()
                self.addCleanup(temp.cleanup)
                self.store, self.ids = build(Path(temp.name), legacy=True)
                with self.store.connect() as c:
                    for change in changes[:step + 1]:
                        c.execute(change)
                terms_seed.switch(self.store)
                self.assert_same()


if __name__ == '__main__':
    unittest.main()
