"""冻结索引输入的查找不受 SQL 浮点文本格式影响，仍拒绝任何真实输入变化。"""
from contextlib import closing
import hashlib
import json
from pathlib import Path
import sqlite3
import tempfile
import unittest
from unittest.mock import patch

import numpy as np
from tests import index_expectations as reference


class IndexExpectationTest(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix='illustash-index-reference-')
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name) / 'input'
        name = 'test_recognition_snapshot_alias_and_claims_on_large_boundary'
        self.files = reference.records()['legacy'][name][0]['files']
        reference.restore(self.files, self.root)
        self.anchors = np.load(self.root / 'char-anchors.npy')
        self.ids = (self.root / 'char-anchors.txt').read_text().splitlines()
        self.source = self.root / 'pi.db'
        # 期望直接来自冻结 SQL 与数组对应的原始记录，不依赖本机的 SQL 导出器。
        encoded = reference.encode(((self.anchors, self.ids, str(self.source)), {}))
        encoded['tuple'][0]['tuple'][2] = {'file': hashlib.sha256(self.files['pi.db']['sql'].encode()).hexdigest()}
        key = hashlib.sha256(json.dumps(encoded, sort_keys=True, ensure_ascii=False).encode()).hexdigest()
        self.expected = reference.decode(reference.records()['functions']['char_scan.anchor_index'][key]['value'])
        dump = reference.database_dump
        # SQL 注释只改变序列化文本，作用等同于 SQLite 版本间的小数位写法差异。
        alternate = patch.object(reference, 'database_dump', side_effect=lambda c: dump(c) + '\n-- same database')
        alternate.start()
        self.addCleanup(alternate.stop)

    def test_sql_text_variation_keeps_exact_recorded_values(self):
        with self.assertRaises(KeyError):
            reference.value('char_scan.anchor_index', self.anchors, self.ids, str(self.source))
        actual = reference.char_scan.anchor_index(self.anchors, self.ids, str(self.source))
        self.assertEqual(actual[0], self.expected[0])
        for index in (1, 2, 3):
            np.testing.assert_array_equal(actual[index], self.expected[index])
        self.assertEqual(actual[4], self.expected[4])

    def test_database_changes_cannot_use_sql_text_fallback(self):
        for sql in (
            'UPDATE characters SET anchor_p50=anchor_p50+1 WHERE rowid=1',
            "INSERT INTO embeddings VALUES ('synthetic','ccip',X'00000000')",
            'UPDATE characters SET rowid=rowid+1000',
            'ALTER TABLE characters ADD COLUMN synthetic INTEGER',
            'PRAGMA user_version=1',
        ):
            with self.subTest(sql=sql):
                reference.restore(self.files, self.root)
                with closing(sqlite3.connect(self.source)) as connection, connection:
                    connection.execute(sql)
                with self.assertRaises(KeyError):
                    reference.char_scan.anchor_index(self.anchors, self.ids, str(self.source))

    def test_array_and_id_changes_cannot_use_sql_text_fallback(self):
        changed = self.anchors.copy()
        changed[0, 0] += 1
        for anchors, ids in ((changed, self.ids), (self.anchors, ['changed', *self.ids[1:]])):
            with self.subTest(changed_array=anchors is changed):
                with self.assertRaises(KeyError):
                    reference.char_scan.anchor_index(anchors, ids, str(self.source))
        missing = self.root / 'missing.db'
        with self.assertRaises(KeyError):
            reference.char_scan.anchor_index(self.anchors, self.ids, str(missing))
        self.assertFalse(missing.exists())


if __name__ == '__main__':
    unittest.main()
