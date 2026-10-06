"""Schema 23 -> 24 regressions, including import and persisted undo."""
import sqlite3
import tempfile
import unittest
from pathlib import Path

from tests.test_web import fixture
from tests import terms_seed
from web import names
from web.store import Store


class NamesMigrationTest(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.source = fixture(self.root)
        if self._testMethodName in ('test_legacy_oc_keeps_its_name_and_curated_rows', 'test_repeated_import_does_not_duplicate_legacy_aliases'):
            self.store = terms_seed.base(self.root, db=self.root / 'library.sqlite')
            self.source = self.store.source
        else:
            self.store = Store(self.source, self.root / 'library.sqlite')
            self.store.refresh()
        self.alice = self.store.entities('Alice')[0]['id']

    def legacy(self):
        with self.store.connect() as c:
            c.execute('CREATE TABLE aliases(entity TEXT,alias TEXT,PRIMARY KEY(entity,alias))')
            c.execute("INSERT OR IGNORE INTO aliases SELECT entity_id,value FROM names WHERE role='alias'")
            c.execute('DROP TABLE names')
            c.execute("UPDATE meta SET value='23' WHERE key='schema_version'")

    def test_legacy_oc_keeps_its_name_and_curated_rows(self):
        self.legacy()
        with self.store.connect() as c:
            c.execute("UPDATE entities SET name='人工原创名字',kind='oc' WHERE id=?", (self.alice,))
            before = {t: list(map(tuple, c.execute('SELECT * FROM ' + t)))
                      for t in ('memberships', 'assertions', 'candidates', 'links')}
        migrated = Store(self.source, self.store.path, legacy=True)
        self.assertEqual(migrated.entities('人工原创名字')[0]['name'], '人工原创名字')
        with migrated.connect() as c:
            for t, rows in before.items():
                after = list(map(tuple, c.execute('SELECT * FROM ' + t)))
                if t == 'links':
                    self.assertTrue(set(rows).issubset(set(after)))
                else:
                    self.assertEqual(rows, after)
            self.assertEqual(c.execute('PRAGMA quick_check').fetchone()[0], 'ok')
        self.assertEqual(len(list((self.root / 'library-backups').glob('*.sqlite'))), 1)
        terms_seed.switch(migrated)

    def test_repeated_import_does_not_duplicate_legacy_aliases(self):
        self.legacy()
        migrated = Store(self.source, self.store.path, legacy=True)
        terms_seed.switch(migrated)
        def rows():
            with migrated.connect() as c:
                return sorted(map(tuple, c.execute('SELECT * FROM names')))
        before = rows()
        migrated.refresh()
        self.assertEqual(before, rows())
        migrated.refresh()
        self.assertEqual(before, rows())

    def test_marking_linked_character_oc_does_not_erase_display(self):
        before = self.store.entities('alice')[0]['name']
        self.store.set_kind(self.alice, 'oc')
        self.assertEqual(self.store.entities('alice')[0]['name'], before)

    def test_preferred_name_can_change_without_accumulating_preferences(self):
        for value in ('甲', '乙', '甲'):
            self.store.set_profile(self.alice, name=value)
        with self.store.connect() as c:
            self.assertEqual(c.execute('SELECT count(*) FROM names WHERE entity_id=? AND preferred=1', (self.alice,)).fetchone()[0], 1)
        self.assertEqual(self.store.entities('alice')[0]['name'], '甲')

    def test_merge_undo_keeps_preexisting_alias(self):
        local = self.store.create_entity('独立名字')['id']
        from web import terms_core
        terms_core.names_command(self.store.path, {'op': 'alias', 'entity': self.alice, 'value': '独立名字', 'source': 'legacy', 'lang': ''})
        action = self.store.merge(local, self.alice)
        Store(self.source, self.store.path).undo(action['id'])
        with self.store.connect() as c:
            self.assertEqual(c.execute("SELECT count(*) FROM names WHERE entity_id=? AND value='独立名字'", (self.alice,)).fetchone()[0], 1)
