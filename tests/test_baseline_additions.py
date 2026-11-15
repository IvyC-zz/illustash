"""v7 历史参考投影的边界：当前全状态、旧行值和未登记对象仍严格保留。"""
import copy
import json
from pathlib import Path
import re
import sqlite3
import tempfile
import unittest

from tests.fixed_expectations import _baseline_additions, _historical_projection, baseline_state


class HistoricalAdditions(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        with sqlite3.connect(self.root / 'library.sqlite') as c:
            c.executescript("""
                CREATE TABLE meta(key TEXT PRIMARY KEY,value TEXT);
                INSERT INTO meta VALUES ('terms_schema','9'),('human_revision','3'),('preserved','yes');
                CREATE TABLE actions(id INTEGER PRIMARY KEY,label TEXT);
                INSERT INTO actions VALUES (1,'旧行为');
                CREATE TABLE human_state_marks(value TEXT);
                INSERT INTO human_state_marks VALUES ('reset');
                CREATE TABLE human_state_mark_undo(value TEXT);
                INSERT INTO human_state_mark_undo VALUES ('before-and-after');
            """)
        self.live = baseline_state(self.root)
        self.frozen = copy.deepcopy(self.live)
        for name in ('human_state_marks', 'human_state_mark_undo'):
            del self.frozen['app'][name]
        self.frozen['app']['meta']['rows'] = [row for row in self.frozen['app']['meta']['rows']
                                             if json.loads(row)[0] != 'human_revision']

    def test_current_state_keeps_additions_and_historical_projection_is_non_mutating(self):
        self.assertIn('human_state_marks', self.live['app'])
        self.assertIn('human_state_mark_undo', self.live['app'])
        self.assertTrue(any(json.loads(row)[0] == 'human_revision' for row in self.live['app']['meta']['rows']))
        original = copy.deepcopy(self.live)
        self.assertEqual(_historical_projection(self.live, self.frozen), self.frozen)
        self.assertEqual(self.live, original)
        with sqlite3.connect(self.root / 'library.sqlite') as c:
            c.execute("UPDATE human_state_marks SET value='changed'")
        self.assertNotEqual(baseline_state(self.root), original)

    def test_unlisted_tables_old_cells_types_and_multiplicity_remain_visible(self):
        for altered in (
            {'human_future': {'columns': ['v'], 'rows': ['[1]']}},
            {'actions': {'columns': ['id', 'label'], 'rows': ['[1,"改变"]']}},
            {'actions': {'columns': ['id', 'label'], 'rows': ['["1","旧行为"]']}},
            {'actions': {'columns': ['id', 'label'], 'rows': self.live['app']['actions']['rows'] * 2}},
        ):
            changed = copy.deepcopy(self.live)
            changed['app'].update(altered)
            self.assertNotEqual(_historical_projection(changed, self.frozen), self.frozen)

    def test_frozen_existing_table_or_key_cannot_be_hidden(self):
        for name in ('human_state_marks', 'human_state_mark_undo', 'meta'):
            old = copy.deepcopy(self.frozen)
            old['app'][name] = self.live['app'][name]
            with self.assertRaises(AssertionError):
                _historical_projection(self.live, old)

    def test_source_database_and_older_versions_are_not_projected(self):
        live = {**self.live, 'source': copy.deepcopy(self.live['app'])}
        projected = _historical_projection(live, self.frozen)
        self.assertEqual(projected['source'], live['source'])
        with sqlite3.connect(self.root / 'library.sqlite') as c:
            c.execute("UPDATE meta SET value='8' WHERE key='terms_schema'")
        old = baseline_state(self.root)
        self.assertEqual(_historical_projection(old, self.frozen), old)

    def test_every_manifest_entry_names_existing_semantic_tests(self):
        root = Path(__file__).resolve().parents[1]
        additions = _baseline_additions()
        self.assertEqual({e['name'] for e in additions['tables']}, {
            'human_state_marks', 'human_state_mark_undo', 'human_object_contributions', 'human_entity_origins'})
        self.assertEqual({e['name'] for e in additions['meta_keys']}, {'human_revision'})
        self.assertEqual({(e['physical'], e['view']) for e in additions['physical_views']}, {
            ('term_records', 'terms'), ('term_relation_records', 'term_relations')})
        for entry in additions['tables'] + additions['meta_keys'] + additions['physical_views']:
            self.assertEqual(entry['introduced_in'], 9)
            for check in entry['checks']:
                path, function = check.rsplit('::', 1)
                self.assertRegex((root / path).read_text(), rf'fn {re.escape(function)}\(')

    def test_physical_view_mapping_compares_every_cell_and_rejects_hidden_history(self):
        with sqlite3.connect(self.root / 'library.sqlite') as c:
            c.executescript('CREATE TABLE term_records(id INTEGER PRIMARY KEY,key TEXT,attrs TEXT);'
                           'INSERT INTO term_records VALUES(1,\'synthetic\',\'{}\');'
                           'CREATE VIEW terms AS SELECT * FROM term_records;')
        live = baseline_state(self.root)
        frozen = copy.deepcopy(self.frozen)
        frozen['app']['terms'] = live['app']['terms']
        self.assertEqual(_historical_projection(live, frozen), frozen)
        for cells in ('[1,"different","{}"]', '["1","synthetic","{}"]'):
            changed = copy.deepcopy(live)
            changed['app']['term_records']['rows'] = [cells]
            with self.assertRaises(AssertionError):
                _historical_projection(changed, frozen)
        hidden = copy.deepcopy(live)
        hidden['app']['terms']['rows'] = []
        with self.assertRaises(AssertionError):
            _historical_projection(hidden, frozen)
        old = copy.deepcopy(frozen)
        old['app']['term_records'] = live['app']['term_records']
        with self.assertRaises(AssertionError):
            _historical_projection(live, old)
        self.assertIn('term_records', baseline_state(self.root)['app'])


if __name__ == '__main__':
    unittest.main()
