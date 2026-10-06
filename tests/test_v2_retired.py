"""身份策略 v2 已放弃（2026-09-30）：还标着 v2 的库，启动时先备份，再转成 hybrid。"""
import sqlite3
import tempfile
import unittest
from pathlib import Path

from tests.test_web import fixture
from tests import terms_seed
from web import identity
from web.store import Store, backups_dir


class V2Retired(unittest.TestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.root = Path(temp.name)
        self.source = fixture(self.root)

    def hybrid_library(self, name):
        store = Store(self.source, self.root / name, identity='hybrid')
        store.refresh()
        return store

    def test_a_v2_library_becomes_hybrid_after_a_backup(self):
        reference = self.hybrid_library('hybrid.sqlite')
        store = terms_seed.startup_input(self.hybrid_library('v2.sqlite'))
        with sqlite3.connect(store.path) as c:
            # 样子照旧版 v2 的库：策略标记、v2 独有的两张表和 meta，机器归属被 v2 收回。
            c.execute("UPDATE meta SET value='v2' WHERE key='identity_policy'")
            c.execute("""CREATE TABLE identity_claims(uuid TEXT PRIMARY KEY,character_id TEXT,variant_id TEXT,status TEXT NOT NULL,
                options_json TEXT NOT NULL,d1 REAL,d2 REAL,reason TEXT NOT NULL,bank_version TEXT NOT NULL,scope_json TEXT NOT NULL DEFAULT '[]')""")
            c.execute("INSERT INTO identity_claims VALUES ('b',NULL,NULL,'undetermined','[]',NULL,NULL,'uncalibrated','',  '[]')")
            c.execute('CREATE TABLE identity_calibration(id TEXT PRIMARY KEY,payload TEXT NOT NULL)')
            c.execute("INSERT INTO meta VALUES ('identity_calibration_active','x'),('identity_bank_error','y')")
            c.execute("DELETE FROM memberships WHERE status='machine'")
        before = len(list(backups_dir(store.path).glob('*.sqlite'))) if backups_dir(store.path).exists() else 0
        reopened = Store(self.source, store.path, legacy=True)
        with reopened.connect() as c:
            self.assertEqual(identity.policy(c), 'hybrid')
            tables = {r[0] for r in c.execute("SELECT name FROM sqlite_master WHERE type='table'")}
            self.assertFalse({'identity_claims', 'identity_calibration'} & tables)
            self.assertFalse(c.execute("SELECT 1 FROM meta WHERE key IN ('identity_calibration_active','identity_bank_error')").fetchone())
            got = sorted(map(tuple, c.execute("SELECT m.uuid,l.key,m.status,m.origin FROM memberships m JOIN links l ON l.entity_id=m.entity AND l.system='ccip'")))
        with reference.connect() as c:
            want = sorted(map(tuple, c.execute("SELECT m.uuid,l.key,m.status,m.origin FROM memberships m JOIN links l ON l.entity_id=m.entity AND l.system='ccip'")))
        self.assertEqual(got, want)                     # 机器归属按 hybrid 的规则回来了（两个库的身份 id 不同，按 ccip key 比）
        self.assertTrue(any(r[2] == 'machine' for r in got))
        self.assertEqual(len(list(backups_dir(store.path).glob('*.sqlite'))), before + 1)
        Store(self.source, store.path, legacy=True)     # 再开一次什么都不做
        self.assertEqual(len(list(backups_dir(store.path).glob('*.sqlite'))), before + 1)
        terms_seed.switch(reopened)

    def test_only_hybrid_can_be_activated(self):
        store = terms_seed.base(self.root)
        self.source = store.source
        with self.assertRaises(ValueError):
            store.activate_identity(mode='v2')
        self.assertEqual(store.activate_identity()['policy'], 'hybrid')
        terms_seed.switch(store)


if __name__ == '__main__':
    unittest.main()
