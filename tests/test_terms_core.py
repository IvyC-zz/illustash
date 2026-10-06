"""Python 的撤销把词条核心写的步骤交给 Rust 核心（tests/reference/web/terms_core.py）。只用临时库；需要 cargo 或已构建的核心。"""
import os
import shutil
import sqlite3
import subprocess
import tempfile
import unittest
from pathlib import Path
from unittest import mock

from web import terms_core
from web.store import Invalid, Store
from tests.test_web import fixture


def core():
    """The terms core binary; built with cargo when this checkout has none. None when neither is possible."""
    if terms_core.binary() is None and shutil.which('cargo'):
        subprocess.run(['cargo', 'build', '--quiet', '--manifest-path', str(terms_core.ROOT / 'native/terms/Cargo.toml')],
                       check=True, timeout=900)
    return terms_core.binary()


class OwnsTest(unittest.TestCase):
    def test_same_rule_as_the_core(self):
        self.assertTrue(terms_core.owns([{'terms': 'verdicts'}]))
        self.assertTrue(terms_core.owns([{'terms': {'kind': {'term': 'k', 'had': 'character'}}}]))
        for before in ([], None, {}, [{'terms': 'verdicts', 'uuid': 'a'}], [{'terms': 'verdicts'}, {'tag': 'x'}],
                       [{'uuid': 'a', 'tag': 'x', 'had': None}], ['terms']):
            self.assertFalse(terms_core.owns(before), before)


@unittest.skipIf(os.name == 'nt', 'macOS/Linux only for now')
class UndoDelegationTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.exe = core()
        if cls.exe is None:
            raise unittest.SkipTest('no terms core binary and no cargo')

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name).resolve()
        source = fixture(self.root)
        # 撤销派发夹具仅含图片和画师；不引入无关的相册角色结论。
        with sqlite3.connect(source) as c:
            c.execute('DELETE FROM characters')
            c.execute('DELETE FROM character_aliases')
            c.execute("DELETE FROM labels WHERE dim IN ('character','char_ext')")
            c.execute("DELETE FROM decisions WHERE dim='character'")
        self.store = Store(source, self.root / 'app.sqlite')
        self.store.refresh()
        done = subprocess.run([str(self.exe), 'migrate', '--db', str(self.store.path)], capture_output=True, text=True)
        self.assertEqual(done.returncode, 0, done.stderr)

    def tearDown(self):
        self.temp.cleanup()

    def sql(self, query, args=()):
        with sqlite3.connect(self.store.path) as c:
            return c.execute(query, args).fetchall()

    def terms_step(self, label='已确认 1 张是 Some Artist 画的'):
        """What the core's judge writes: one action and its verdict (the projection is left to the core)."""
        with sqlite3.connect(self.store.path) as c:
            c.execute("INSERT OR IGNORE INTO term_assets(namespace,key) VALUES ('photos','a')")
            c.execute("INSERT OR IGNORE INTO terms(key,kind,origin) VALUES ('artist-1','artist','user')")
            action = c.execute("INSERT INTO actions(label,before_json) VALUES (?,'[{\"terms\":\"verdicts\"}]')", (label,)).lastrowid
            c.execute("""INSERT INTO term_verdicts(action_id,anchor,asset_id,term_id,verdict,origin)
                         SELECT ?1,?1,a.id,t.id,'yes','user' FROM term_assets a,terms t WHERE a.namespace='photos' AND a.key='a' AND t.key='artist-1'""", (action,))
        return action

    def effective(self):
        return self.sql('SELECT count(*) FROM term_effective_verdicts')[0][0]

    def test_a_terms_step_is_undone_by_the_core(self):
        tagged = self.store.curate(['b'], 'exclude')
        action = self.terms_step()
        with self.store.connect() as c:                                       # what the history bar shows
            self.assertEqual(Store.last_action(c)['label'], '已确认 1 张是 Some Artist 画的')
        self.assertEqual(self.effective(), 1)
        after = self.store.undo(action)
        self.assertEqual(after['id'], tagged['id'])                          # 外围 Python 步骤重新成为最后一步
        self.assertEqual(self.sql('SELECT undone FROM actions WHERE id=?', (action,)), [(1,)])
        self.assertEqual(self.effective(), 0)
        self.assertEqual(self.sql('SELECT count(*) FROM term_verdicts')[0][0], 1)   # kept, only its step is undone
        self.store.undo(tagged['id'])                                         # 外围步骤仍由 Python 撤销
        self.assertEqual(self.sql("SELECT count(*) FROM asset_tags WHERE uuid='b'")[0][0], 0)
        self.assertEqual(self.sql("SELECT count(*) FROM curation WHERE uuid='b'")[0][0], 0)

    def test_a_later_python_step_is_never_skipped(self):
        action = self.terms_step()
        tagged = self.store.curate(['b'], 'exclude')
        with self.assertRaisesRegex(Invalid, '操作历史已变化'):
            self.store.undo(action)
        self.assertEqual(self.effective(), 1)
        self.store.undo(tagged['id'])
        self.assertIsNone(self.store.undo(action))
        self.assertEqual(self.effective(), 0)

    def test_an_undone_step_cannot_be_undone_again(self):
        action = self.terms_step()
        self.store.undo(action)
        with self.assertRaisesRegex(Invalid, '操作历史已变化'):
            self.store.undo(action)

    def test_without_the_core_the_step_stays(self):
        action = self.terms_step()
        with mock.patch.dict(os.environ, {'ILLUSTASH_TERMS_BIN': str(self.root / 'missing')}):
            with self.assertRaisesRegex(Invalid, '找不到词条核心'):
                self.store.undo(action)
        self.assertEqual(self.sql('SELECT undone FROM actions WHERE id=?', (action,)), [(0,)])
        self.assertEqual(self.effective(), 1)

    def test_a_core_without_schema_registration_is_refused(self):
        (self.root / 'other').mkdir()
        other = Store(fixture(self.root / 'other'), self.root / 'other.sqlite')
        with sqlite3.connect(other.path) as c:
            # 已完成切换的库损坏了核心版本登记；公共入口仍须收到原核心拒绝，不能偷偷初始化。
            c.execute("DELETE FROM meta WHERE key='terms_schema'")
            action = c.execute("INSERT INTO actions(label,before_json) VALUES ('x','[{\"terms\":\"verdicts\"}]')").lastrowid
        with self.assertRaisesRegex(Invalid, '还没有词条表'):
            other.undo(action)


if __name__ == '__main__':
    unittest.main()
