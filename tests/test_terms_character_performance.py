"""Performance shortcuts retain the old validation, history and committed projections."""
import sqlite3
import tempfile
import unittest
from pathlib import Path
from unittest import mock

from tests.test_terms_core import core
from tests.test_web import fixture
from web import terms_core
from web.store import Invalid, Store


class CharacterPerformance(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        if core() is None:
            raise unittest.SkipTest('no terms core')

    def setUp(self):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.root = Path(temp.name)
        self.s = Store(fixture(self.root), self.root / 'library.sqlite', identity='hybrid')
        self.s.refresh()
        with self.s.connect() as c:
            self.ids = dict(c.execute("SELECT key,entity_id FROM links WHERE system='ccip'"))
        self.old_path = self.root / 'old.sqlite'
        with sqlite3.connect(self.s.path) as src, sqlite3.connect(self.old_path) as dst:
            src.backup(dst)

    def python_writes(self, call):
        """What the Store wrote on its own connections during `call`: nothing, once no write rehearses the old code
        (docs/TERMS_NAMES_PLAN.md N5); every write is the core's."""
        from web import store as store_module
        statements = []

        class Tracing(sqlite3.Connection):
            def execute(self, sql, params=()):
                statements.append(sql)
                return super().execute(sql, params)

            def executemany(self, sql, rows):
                statements.append(sql)
                return super().executemany(sql, rows)

        connect = sqlite3.connect
        def traced(*args, **kwargs):
            kwargs.setdefault('factory', Tracing)
            return connect(*args, **kwargs)
        with mock.patch.object(store_module.sqlite3, 'connect', traced):
            result = call()
        writes = [q for q in statements if q.lstrip().upper().startswith(('INSERT', 'UPDATE', 'DELETE', 'REPLACE', 'BEGIN IMMEDIATE'))]
        return result, writes

    def test_no_write_rehearses_the_old_code(self):
        a, b = self.ids['alice'], self.ids['bob']
        calls = [
            lambda: self.s.decide_identity(a=a, b=b, kind='duplicate_of', reason='只提议'),
            lambda: self.s.change(['b'], 'confirmed', a),
            lambda: self.s.change(['c'], 'unknown'),
            lambda: self.s.sweep(b),
            lambda: self.s.answer(a, yes=['a']),
            lambda: self.s.reassign(['b'], a, b),
            lambda: self.s.uncertain_identity(['c'], [a, b], '看不清'),
            lambda: self.s.set_mascot(b, True),
            lambda: self.s.set_kind(b, 'oc', 'someone'),
            lambda: self.s.set_profile(a, name='新名字', aliases=['别名']),
            lambda: self.s.create_entity('新角色测试'),
            lambda: self.s.create_entity('新角色测试'),
            lambda: self.s.create_linked('someone_(series)', 'Someone'),
            lambda: self.s.shared_oc(),
            lambda: self.s.shared_oc(),
            lambda: self.s.merge(a, b),
        ]
        for call in calls:
            result, writes = self.python_writes(call)
            self.assertTrue(result)
            self.assertEqual(writes, [])
        terms_core.run('verify-characters', '--db', self.s.path)

    def test_refused_write_keeps_history(self):
        with self.s.connect() as c:
            before = c.execute('SELECT count(*) FROM actions').fetchone()[0]
        with self.assertRaises(Invalid):
            self.s.change(['missing'], 'confirmed', self.ids['alice'])
        with self.s.connect() as c:
            self.assertEqual(c.execute('SELECT count(*) FROM actions').fetchone()[0], before)
        terms_core.run('verify-characters', '--db', self.s.path)

    def test_core_projection_failure_is_atomic_after_skipping_plan_refresh(self):
        with sqlite3.connect(self.s.path) as c:
            c.execute("CREATE TRIGGER fail_projection BEFORE INSERT ON memberships BEGIN SELECT RAISE(ABORT,'test projection failure'); END")
        with self.s.connect() as c:
            before = c.execute('SELECT count(*) FROM actions').fetchone()[0]
            old = list(map(tuple, c.execute('SELECT * FROM assertions ORDER BY uuid,entity')))
        with self.assertRaisesRegex(RuntimeError, 'test projection failure'):
            self.s.change(['b'], 'confirmed', self.ids['alice'])
        with self.s.connect() as c:
            self.assertEqual(c.execute('SELECT count(*) FROM actions').fetchone()[0], before)
            self.assertEqual(list(map(tuple, c.execute('SELECT * FROM assertions ORDER BY uuid,entity'))), old)
        with sqlite3.connect(self.s.path) as c:
            c.execute('DROP TRIGGER fail_projection')
        terms_core.run('verify-characters', '--db', self.s.path)

    def test_graph_batch_invalidates_only_changed_entities_with_exact_results(self):
        old = Store.open(self.s.source, self.old_path)
        def version(s):
            with s.connect() as c:
                return int(c.execute("SELECT value FROM meta WHERE key='names_version'").fetchone()[0])
        def state(s):
            with s.connect() as c:
                return {r[0]: tuple(r[1:]) for r in c.execute('SELECT id,parent_id,variant,kind,merged_into FROM entities')}
        previous = state(self.s)
        # 先装载快照，随后验证真正变化仍会使缓存失效。
        old.characters()
        self.s.characters()
        before = [version(s) for s in (old, self.s)]
        for s in (old, self.s):
            s.decide_identity(a=self.ids['alice'], b=self.ids['bob'], kind='form_of',
                              status='accepted', reason='count invalidations')
        after = [version(s) for s in (old, self.s)]
        current = state(self.s)
        changed = sum(previous.get(ident) != fields for ident, fields in current.items())
        # 原断言钉住无变化实体也触发一次的实现细节；现在钉住可见结构、缓存结果及准确的变更数。
        self.assertEqual(after[1] - before[1], changed)
        self.assertGreater(after[1], before[1])
        self.assertLessEqual(after[1] - before[1], after[0] - before[0])
        self.assertEqual(state(old), current)
        self.assertEqual(old.characters(), self.s.characters())
        terms_core.run('verify-characters', '--db', self.s.path)
