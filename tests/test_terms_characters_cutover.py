"""S4 角色切到词条核心：写保护、重启、失败留旧表并能重试、命令行撤销、学习经核心发布。"""
import contextlib
import io
import sqlite3
import tempfile
import unittest
from pathlib import Path
from unittest import mock

import numpy as np

from tests.test_terms_core import core
from tests.test_web import fixture
from tests import terms_seed
from tests.test_web_v2 import assert_memberships_current
from web import learn, terms_core
from web.features import Features
from web.server import adopt_characters, adopt_labels
from web.store import Invalid, Store


class Cutover(unittest.TestCase):
    policy = None

    @classmethod
    def setUpClass(cls):
        if core() is None:
            raise unittest.SkipTest('no terms core binary and no cargo')

    def setUp(self):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.root = Path(temp.name)
        self.source = fixture(self.root)
        self.store = Store(self.source, self.root / 'app.sqlite', identity=self.policy)
        self.store.refresh()
        self.alice, self.bob = self.store.entities('Alice')[0]['id'], self.store.entities('Bob')[0]['id']
        self.store.change(['c'], 'confirmed', self.bob)
        self.store = terms_seed.startup_input(self.store)

    def tearDown(self):
        assert_memberships_current(self, self.store)

    def switch(self):
        terms_seed.switch(self.store)
        self.assertTrue(self.store.characters_in_core())

    def flag(self):
        with self.store.connect() as c:
            return terms_core.characters_in_core(c)

    def test_the_character_projection_is_mirrored_whole_at_the_switch(self):
        """S5 真实副本上发现：切换后人物 / 皮肤那一片 term_projection 是空的（只有后来的命令按图增量维护）。"""
        from tests.test_terms_character_learning import rebuilt_snapshot, snapshot
        self.switch()
        with self.store.connect() as c:
            rows = c.execute("""SELECT count(*) FROM term_projection p JOIN terms t ON t.id=p.term_id
                WHERE t.kind IN ('character','skin')""").fetchone()[0]
            self.assertEqual(rows, c.execute('SELECT (SELECT count(*) FROM memberships)+(SELECT count(*) FROM tag_memberships)'
                                             '+(SELECT count(*) FROM variant_memberships)').fetchone()[0])
        self.assertGreater(rows, 0)
        self.assertEqual(snapshot(self.store, ('term_projection',)), rebuilt_snapshot(self.store, ('term_projection',)))

    def test_a_switched_library_gets_the_core_schema_upgrade_at_start(self):
        """切过的库，启动时的 adopt_* 都直接返回，不会顺带迁移；核心命令又拒绝旧版本的库。所以每次启动都先升级词条表
        （tests/reference/web/server.py upgrade_terms）。这里把版本号退回 3、放回一张冻结副本，模拟第 4 版之前切换的库。"""
        from web.server import upgrade_terms
        self.switch()
        with self.store.connect() as c:
            c.execute('CREATE TABLE legacy_assertions AS SELECT * FROM assertions')
            c.execute("UPDATE meta SET value='3' WHERE key='terms_schema'")
        with self.assertRaisesRegex(Invalid, "第 3 版"):                 # 版本不对：核心拒绝
            self.store.change(['a'], 'rejected', self.alice)
        self.assertEqual(upgrade_terms(self.store)['from'], 3)
        with self.store.connect() as c:
            self.assertGreaterEqual(int(c.execute("SELECT value FROM meta WHERE key='terms_schema'").fetchone()[0]), 4)
            self.assertFalse(c.execute("SELECT 1 FROM sqlite_master WHERE name LIKE 'legacy_%'").fetchone())
        self.store.change(['a'], 'rejected', self.alice)               # 升级后照常写
        terms_core.run('verify-characters', '--db', self.store.path)

    def test_python_writes_to_core_tables_are_refused(self):
        self.switch()
        with sqlite3.connect(self.store.path) as c:
            for sql in ("UPDATE entities SET work='x'", "DELETE FROM assertions", "INSERT INTO candidates VALUES ('a','x','WD',1)",
                        "DELETE FROM memberships", "UPDATE links SET current=0", "INSERT INTO work_review(entity_id) VALUES ('x')", "INSERT INTO learn_state VALUES ('x','s','t')",
                        "INSERT INTO imported VALUES ('a','x')"):
                with self.subTest(sql=sql), self.assertRaisesRegex(sqlite3.IntegrityError, 'written by the terms core'):
                    c.execute(sql)
            c.execute("INSERT INTO curation(uuid,status,origin) VALUES ('a','excluded','review')")   # 外围表照旧可写
            c.execute("DELETE FROM curation")
        # 第 4 版起不留冻结副本（回退靠切换前的备份）。
        with self.store.connect() as c:
            self.assertFalse(c.execute("SELECT 1 FROM sqlite_master WHERE name LIKE 'legacy_%'").fetchone())

    def test_restart_keeps_working_and_does_not_switch_twice(self):
        self.switch()
        again = Store(self.source, self.store.path)                  # 重启：迁移、名字版本、相册标题、WD 派生照常
        self.assertTrue(adopt_characters(again))
        step = again.change(['b'], 'rejected', self.alice)
        after = again.undo(step['id'])
        with again.connect() as c:
            self.assertEqual(after, Store.last_action(c))
        self.store = again

    def test_refusals_and_retry(self):
        # 标签还没切：不切角色。
        self.assertFalse(adopt_characters(self.store))
        self.assertTrue(adopt_labels(self.store))
        # 绕过历史改了表：收编核对不通过，什么都没切（有备份），图库只读打开（用户决定 2026-09-30）。
        with self.store.connect() as c:
            c.execute("UPDATE assertions SET origin='album' WHERE uuid='c'")
            Store._refresh_memberships(c)
        backups = set((self.root / 'backups').glob('*')) if (self.root / 'backups').exists() else set()
        with contextlib.redirect_stdout(io.StringIO()):
            self.assertFalse(adopt_characters(self.store))
        self.assertFalse(self.flag())
        self.assertIn('只读', self.store.read_only)
        with self.assertRaises(sqlite3.DatabaseError), self.store.connect() as c:
            c.execute("UPDATE assertions SET origin='review' WHERE uuid='c'")          # 旧表不再可写
        self.store = Store(self.source, self.store.path)                               # 下次启动：可写，再试
        with self.store.connect() as c:
            self.assertFalse(c.execute("SELECT 1 FROM sqlite_master WHERE name LIKE 'legacy_assertions'").fetchone())
            c.execute("UPDATE assertions SET origin='review' WHERE uuid='c'")
            Store._refresh_memberships(c)
        self.assertTrue(adopt_characters(self.store))                                  # 这次成功
        self.assertTrue(self.flag())
        self.assertGreater(len(set(self.store.path.parent.rglob('library-*.sqlite')) - backups), 0, '切换前备份')

    def test_identity_policy_and_schema_upgrades_are_refused_after(self):
        self.switch()
        with self.assertRaises(Invalid):
            self.store.activate_identity(mode='hybrid')
        with self.store.connect() as c:
            c.execute("UPDATE meta SET value='35' WHERE key='schema_version'")
        with self.assertRaisesRegex(Invalid, '词条核心的迁移'):
            Store(self.source, self.store.path)
        with self.store.connect() as c:
            c.execute("UPDATE meta SET value='36' WHERE key='schema_version'")

    def test_command_line_undo_takes_character_steps(self):
        self.switch()
        step = self.store.change(['b'], 'confirmed', self.alice)
        done = terms_core.run('undo', '--db', self.store.path, '--action', step['id'])
        self.assertEqual(done['action'], step['id'])
        self.assertFalse(any(a['id'] == self.alice for a in self.store.detail('b')['assertions'] if a.get('status') == 'confirmed'
                             and a.get('origin') == 'review'))
        terms_core.run('verify-characters', '--db', self.store.path)

    def test_learning_publishes_through_the_core(self):
        self.switch()
        vectors = {u: np.eye(8, dtype=np.float32)[i % 8] for i, u in enumerate(('a', 'b', 'c'))}
        features = Features(self.source, np.stack(list(vectors.values())), list(vectors))
        with mock.patch.object(terms_core.LearningPublisher, 'publish', wraps=terms_core.LearningPublisher.publish, autospec=True) as spy:
            learn.learn(self.store, features, force=True, reclaim=True)
        self.assertEqual(spy.call_count, 1)
        terms_core.run('verify-characters', '--db', self.store.path)


class HybridCutover(Cutover):
    policy = 'hybrid'


if __name__ == '__main__':
    unittest.main()
