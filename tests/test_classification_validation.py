import sqlite3
import json
import tempfile
import unittest
from pathlib import Path

import numpy as np

from tests.test_web import fixture
from tests import terms_seed
from tools.calibrate_classification import dataset, image_groups, split_for, calibrate, metrics, add_prototype_scores, read_gold, label_inventory
from web.features import Features
from web.store import Store


class ValidationTest(unittest.TestCase):
    def test_label_inventory_keeps_imports_separate_and_read_only(self):
        with tempfile.TemporaryDirectory() as tmp:
            source = Path(tmp) / 'source.sqlite'
            with sqlite3.connect(source) as src:
                src.execute('CREATE TABLE decisions(dim, "by")')
                src.execute("INSERT INTO decisions VALUES ('character','human:album')")
            c = sqlite3.connect(':memory:')
            self.addCleanup(c.close)
            c.execute('CREATE TABLE assertions(origin,status)')
            c.executemany('INSERT INTO assertions VALUES (?,?)',
                          [('album', 'confirmed'), ('review', 'rejected'), ('legacy', 'confirmed')])
            before = source.read_bytes()
            result = label_inventory(c, source)
            self.assertEqual(result['unknown_assertion_origins'], ['legacy'])
            self.assertEqual(result['source_decisions'],
                             [{'dimension': 'character', 'author': 'human:album', 'count': 1}])
            self.assertEqual(source.read_bytes(), before)
            self.assertEqual(c.execute('SELECT count(*) FROM assertions').fetchone()[0], 3)

    def test_external_gold_needs_explicit_complete_review_for_negatives(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            store = Store(fixture(root),root/'app.sqlite');store.refresh()
            path = root/'gold.jsonl'
            item = {'uuid':'a','group':'a','reviewed':False,'characters':[],'people':'none','complete':True}
            with store.connect() as c:
                groups=image_groups(c)
                path.write_text(json.dumps(item)+'\n')
                self.assertEqual(read_gold(c,path,groups),( [],0))
                item.update(reviewed=True,complete=False)
                path.write_text(json.dumps(item)+'\n')
                self.assertEqual(read_gold(c,path,groups),( [],1))
                item['complete']=True
                path.write_text(json.dumps(item)+'\n')
                rows,count=read_gold(c,path,groups)
                self.assertEqual(count,1);self.assertEqual([r['label'] for r in rows],[0])
                item['people']='solo'
                path.write_text(json.dumps(item)+'\n')
                with self.assertRaises(ValueError):read_gold(c,path,groups)

    def test_only_explicit_verdicts_and_group_conflicts(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            store = Store(fixture(root), root/'app.sqlite')
            store.refresh()
            alice = store.entities('Alice')[0]['id']
            with store.connect() as c:
                self.assertEqual(dataset(c)[0], [])  # album membership is not review gold
            with terms_seed.staged(store) as store, store.connect() as c:
                c.execute("INSERT INTO entities(id,name,kind,parent_id,variant) VALUES ('skin','','skin',?,'summer')", (alice,))
            store.change(['c'], 'remove_variant', 'skin')
            store.change(['a'], 'confirmed', alice)
            store.change(['b'], 'rejected', alice)
            with store.connect() as c:
                rows, audit, _ = dataset(c)
                self.assertEqual(sorted(r['label'] for r in rows), [0, 1])
                self.assertEqual(audit['ignored_skin_rejections'], 1)
                c.execute("INSERT INTO relations(a,b,kind) VALUES ('a','b','variant')")
                groups = image_groups(c)
                self.assertEqual(groups['a'], groups['b'])
                self.assertEqual(split_for(groups['a']), split_for(groups['b']))
                rows, audit, _ = dataset(c)
                self.assertEqual(rows, [])
                self.assertEqual(len(audit['conflicts']), 1)

    def test_threshold_selection_never_uses_test_labels(self):
        rows = [{'uuid': str(i), 'group': str(i), 'entity': 'a', 'split': part,
                 'label': int(i % 2 == 0), 'WD': .99 if i % 2 == 0 else .8}
                for part in ('dev', 'test') for i in range(200)]
        a = calibrate(rows, 'WD', .9)
        for r in rows:
            if r['split'] == 'test': r['label'] = 1-r['label']
        b = calibrate(rows, 'WD', .9)
        self.assertEqual(a['dev_selected'], b['dev_selected'])
        self.assertEqual(a['independent_holdout_gate'], 'pass')
        self.assertEqual(b['independent_holdout_gate'], 'hold')
        self.assertFalse(a['production_threshold_changed'])

    def test_repeated_artwork_does_not_inflate_precision_evidence(self):
        rows = [{'group': 'one', 'entity': str(i), 'label': 1, 'WD': .99} for i in range(100)]
        result = metrics(rows, 'WD', .9)
        self.assertEqual(result['selected'], 1)
        self.assertLess(result['precision_lower95'], .3)

    def test_prototypes_ignore_all_holdout_seeds(self):
        rows = [{'uuid': str(i), 'group': str(i), 'entity': 'a', 'label': 1,
                 'split': 'test', 'people': 'solo'} for i in range(10)]
        f = Features(Path('/unused'), np.tile([1., 0.], (10, 1)), [str(i) for i in range(10)])
        self.assertEqual(add_prototype_scores(rows, f), 0)
        self.assertTrue(all(r['PROTO'] is None for r in rows))
        for r in rows[:5]: r['split'] = 'train'
        self.assertEqual(add_prototype_scores(rows, f), 1)
        self.assertEqual(rows[-1]['PROTO'], 1.)
