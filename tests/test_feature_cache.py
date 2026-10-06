import sqlite3
import tempfile
import unittest
from pathlib import Path

import numpy as np

from web.features import Features


class FeatureCacheTest(unittest.TestCase):
    def test_changed_overlay_deletion_and_base_replacement(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            db = root/'source.db'
            with sqlite3.connect(db) as c:
                c.execute('CREATE TABLE embeddings(uuid,model,vec,PRIMARY KEY(uuid,model))')
            (root/'full-manifest.csv').write_text('uuid\nA\n')
            np.save(root/'full-features.npy', np.array([[1., 0.]], np.float32))
            f = Features(db).load()
            before = f.X
            f.load()
            self.assertIs(f.X, before)
            old_version = f.versions['A']
            with sqlite3.connect(db) as c:
                c.execute('INSERT INTO embeddings VALUES (?,?,?)', ('A','ccip',np.array([np.nan,1.],np.float32).tobytes()))
            with self.assertRaises(ValueError): f.load()
            self.assertIs(f.X, before)
            self.assertEqual(f.versions['A'], old_version)
            with sqlite3.connect(db) as c:
                c.execute('INSERT OR REPLACE INTO embeddings VALUES (?,?,?)', ('A','ccip',np.array([0.,1.],np.float32).tobytes()))
            f.load()
            np.testing.assert_array_equal(f.matrix(['A'])[1], [[0., 1.]])
            self.assertNotEqual(f.versions['A'], old_version)
            with sqlite3.connect(db) as c:
                c.execute('DELETE FROM embeddings')
            f.load()
            np.testing.assert_array_equal(f.matrix(['A'])[1], [[1., 0.]])
            np.save(root/'full-features.npy', np.array([[0., -1.]], np.float32))
            f.load()
            np.testing.assert_array_equal(f.matrix(['A'])[1], [[0., -1.]])

    def test_invalid_vectors_and_manifest_are_rejected(self):
        for X, ids in [(np.zeros((1,2), np.float32), ['a']), (np.ones((2,2)), ['a']),
                       (np.ones((2,2)), ['a','a']), (np.array([[np.nan,1]]), ['a'])]:
            with self.assertRaises(ValueError): Features(Path('/unused'), X, ids)
