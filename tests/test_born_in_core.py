"""C3 核心默认出生与已有旧库启动切换：固定旧输入对照首次刷新，打开旧库不提前改形态。"""
import contextlib
import io
import tempfile
import unittest
from pathlib import Path
from unittest import mock

from tests.test_terms_core import core
from tests.test_web import fixture
from tests import terms_seed
from web import terms_core
from web.server import adopt_characters, adopt_labels, adopt_names
from web.store import Invalid, Store

TABLES = ('entities', 'links', 'names', 'display_names', 'name_aliases', 'assertions', 'candidates', 'memberships',
          'tag_memberships', 'family_evidence', 'work_review', 'general_hits', 'asset_rating', 'asset_tags', 'duplicates',
          'single_assets', 'lead_assets', 'excluded')


def contents(store):
    """每张表的行（去掉时间列），身份 id 换成它的 ccip key。"""
    out = {}
    with store.connect() as c:
        key = dict(c.execute("SELECT entity_id,'<'||key||'>' FROM links WHERE system='ccip'"))
        for table in TABLES:
            cols = sorted(r[1] for r in c.execute(f'PRAGMA table_info({table})') if r[1] != 'ts' and not r[1].endswith('_at'))
            out[table] = sorted(repr(tuple(key.get(v, v) if isinstance(v, str) else v for v in r))
                                for r in c.execute(f'SELECT {",".join(cols)} FROM {table}'))
    return out


class BornInCore(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        if core() is None:
            raise unittest.SkipTest('no terms core binary and no cargo')

    def setUp(self):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.root = Path(temp.name)
        self.source = fixture(self.root)

    def switched(self, store):
        with store.connect() as c:
            return {part: getattr(terms_core, f'{part}_in_core')(c) for part in ('artists', 'labels', 'characters', 'names')}

    def test_a_new_library_is_born_in_the_core(self):
        store = Store(self.source, self.root / 'app.sqlite', legacy=False)
        self.assertEqual(self.switched(store), dict.fromkeys(('artists', 'labels', 'characters', 'names'), True))
        with store.connect() as c:
            self.assertEqual(int(c.execute("SELECT value FROM meta WHERE key='terms_schema'").fetchone()[0]),
                             terms_core.run('migrate', '--db', store.path)['to'])

    def test_its_first_refresh_equals_the_old_shape_switched_afterwards(self):
        born = Store(self.source, self.root / 'born.sqlite', legacy=False)
        born.refresh()
        old = terms_seed.base(self.root, db=self.root / 'old.sqlite')
        self.assertFalse(any(self.switched(old).values()))
        with contextlib.redirect_stdout(io.StringIO()):
            self.assertTrue(adopt_labels(old) and adopt_characters(old) and adopt_names(old))
        terms_core.cutover_artists(old.path)
        self.assertEqual(contents(born), contents(old))
        self.assertTrue(terms_core.run('verify-characters', '--db', born.path)['committed'])
        self.assertTrue(terms_core.run('verify-names', '--db', born.path)['committed'])

    def test_the_server_start_path_writes_through_the_core(self):
        store = Store(self.source, self.root / 'app.sqlite', initialize=False, legacy=False)
        store.initialize()
        store.refresh()
        alice = store.entities('Alice')[0]['id']
        step = store.change(['b'], 'confirmed', alice)
        self.assertEqual(step['label'], '已将 1 张设为 Alice')
        with store.connect() as c:
            self.assertEqual(c.execute("SELECT status FROM assertions WHERE uuid='b' AND entity=?", (alice,)).fetchone()[0], 'confirmed')
        store.undo(step['id'])
        self.assertTrue(terms_core.run('verify-characters', '--db', store.path)['committed'])
        reopened = Store(self.source, store.path, legacy=False)            # 已有的库不会再「出生」一次
        self.assertTrue(all(self.switched(reopened).values()))

    def test_an_existing_old_library_is_not_reshaped_by_opening_it(self):
        terms_seed.base(self.root)
        again = Store(self.source, self.root / 'app.sqlite', legacy=False)
        self.assertFalse(any(self.switched(again).values()))              # 它在启动时照常切换（tests/reference/web/server.py adopt_*）

    def test_without_the_core_no_library_is_created(self):
        with mock.patch.object(terms_core, 'binary', return_value=None), self.assertRaises(Invalid):
            Store(self.source, self.root / 'app.sqlite', legacy=False)


if __name__ == '__main__':
    unittest.main()


class StartupOrder(unittest.TestCase):
    """启动时已有的库先升级核心 schema 再 initialize：名字在核心时 initialize
    就会经核心写相册标题别名，而核心拒绝旧版本的库。"""
    @classmethod
    def setUpClass(cls):
        if core() is None:
            raise unittest.SkipTest('no terms core binary and no cargo')

    def test_an_older_terms_schema_is_upgraded_before_initialize_writes_through_the_core(self):
        import sqlite3
        from web.server import open_store
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            source = fixture(root)
            db = root / 'app.sqlite'
            store = Store(source, db)
            with contextlib.redirect_stdout(io.StringIO()):
                store.refresh()
            self.assertTrue(store._album_titles(), '夹具要有能对上角色的相册，initialize 才会经核心写别名')
            current = terms_core.run('migrate', '--db', db)['to']
            with sqlite3.connect(db) as c:   # 旧版本号：核心的命令会拒绝它
                c.execute("UPDATE meta SET value=? WHERE key='terms_schema'", (str(current - 1),))
            with self.assertRaises(terms_core.Failed):
                Store(source, db, initialize=False).initialize()
            with contextlib.redirect_stdout(io.StringIO()):
                opened = open_store(source, db)
            with opened.connect() as c:
                self.assertEqual(int(c.execute("SELECT value FROM meta WHERE key='terms_schema'").fetchone()[0]), current)
            self.assertTrue(terms_core.run('verify-names', '--db', db)['committed'])
