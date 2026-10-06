"""名字收编 N2（docs/TERMS_NAMES_PLAN.md）：names / display_names / name_aliases 收进核心主数据，影子阶段逐行比对。

名字夹具库（tests/terms_names_fixture.py）照服务端顺序切到核心（先标签、再角色），再跑 sync-names：零差异；再跑一次不改
任何行；旧表改了之后再同步仍一致；核心被改坏时 verify-names 报出；预检不通过整次回滚。"""
import sqlite3
import tempfile
import unittest
from pathlib import Path

from tests.terms_names_fixture import build
from tests.test_terms_core import core
from web import terms_core
from web.server import adopt_characters, adopt_labels


class NamesSync(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        if core() is None:
            raise unittest.SkipTest('no terms core binary and no cargo')

    def setUp(self):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.store, self.ids = build(Path(temp.name), legacy=True)
        self.assertTrue(adopt_labels(self.store))
        self.assertTrue(adopt_characters(self.store))

    def sync(self):
        return terms_core.run('sync-names', '--db', self.store.path)

    def rows(self):
        with sqlite3.connect(self.store.path) as c:
            return [c.execute(f'SELECT count(*) FROM {t}').fetchone()[0] for t in ('term_names', 'term_key_names', 'term_key_aliases')]

    def test_sync_matches_and_twice_changes_nothing(self):
        first = self.sync()
        self.assertTrue(first['committed'])
        for table, diff in first['tables'].items():
            self.assertEqual((diff['missing'], diff['extra']), (0, 0), table)
            self.assertGreater(diff['old'], 0, table)
        rows = self.rows()
        again = self.sync()
        self.assertEqual(sum(again['changed'].values()), 0)
        self.assertEqual(self.rows(), rows)
        self.assertTrue(terms_core.run('verify-names', '--db', self.store.path)['committed'])

    def test_later_changes_to_the_old_tables_follow(self):
        self.sync()
        with sqlite3.connect(self.store.path) as c:
            c.execute("INSERT INTO names(entity_id,lang,value,source,role,preferred) VALUES ('c-prio','zh','新名','model','display',0)")
            c.execute("UPDATE names SET preferred=1 WHERE entity_id='c-pref' AND value='aaa'")
            c.execute("DELETE FROM names WHERE entity_id='s-former'")
            c.execute("INSERT INTO display_names VALUES ('work','series_q','zh','bangumi','新作品名')")
            c.execute("UPDATE name_aliases SET position=3")
        out = self.sync()
        self.assertTrue(out['committed'])
        # 整行比较：改首选、改顺序都是一删一加。
        self.assertEqual(out['changed'], {'names': 1 + 2 + 1, 'display_names': 1, 'name_aliases': 2})
        self.assertTrue(all((d['missing'], d['extra']) == (0, 0) for d in out['tables'].values()))

    def test_a_broken_core_is_reported(self):
        self.sync()
        for sql in ("DELETE FROM term_names WHERE rowid=(SELECT min(rowid) FROM term_names WHERE term_id IN (SELECT term_id FROM term_tree))",
                    "UPDATE term_key_names SET value='坏了' WHERE kind='work'",
                    "DELETE FROM term_key_aliases"):
            with self.subTest(sql=sql):
                path = Path(self.store.path).with_name('broken.sqlite')
                with sqlite3.connect(self.store.path) as src, sqlite3.connect(path) as dst:
                    src.backup(dst)
                with sqlite3.connect(path) as c:
                    c.execute(sql)
                with self.assertRaisesRegex(terms_core.Failed, '名字收编核对没有通过'):
                    terms_core.run('verify-names', '--db', path)
                path.unlink()

    def test_preflight_refuses_and_leaves_nothing(self):
        with sqlite3.connect(self.store.path) as c:
            c.execute("INSERT INTO names(entity_id,lang,value,source,role) VALUES ('c-prio','zh',?,'model','display')", ('长' * 201,))
        before = self.rows()
        with self.assertRaisesRegex(terms_core.Failed, '名字为空或超过 200 字'):
            self.sync()
        self.assertEqual(self.rows(), before)

    def test_characters_must_be_in_the_core_first(self):
        fresh, _ = build(Path(tempfile.mkdtemp()), legacy=True)
        terms_core.run('migrate', '--db', fresh.path)
        with self.assertRaisesRegex(terms_core.Failed, '先把角色切到词条核心'):
            terms_core.run('sync-names', '--db', fresh.path)


if __name__ == '__main__':
    unittest.main()
