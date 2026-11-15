"""固定合成输入与历史应用导入边界；不构建或调用原生候选。"""
from contextlib import closing
import hashlib
import json
from pathlib import Path
import sqlite3
import subprocess
import sys
import tempfile
import unittest

from tests.synthetic_source import source_fixture, synthetic_reference
from tools.wait_limit import WAIT


class SyntheticSourceTest(unittest.TestCase):
    def test_helpers_do_not_import_application_or_test_consumers(self):
        code = '''
import importlib.abc
import sys
from pathlib import Path
sys.path.insert(0, str(Path.cwd()))
blocked = {'web.store', 'web.server', 'web.library', 'tests.test_web',
           'tests.test_native_library_create'}
class Refuse(importlib.abc.MetaPathFinder):
    def find_spec(self, fullname, path, target=None):
        if fullname in blocked:
            raise AssertionError('unexpected fixture dependency: ' + fullname)
sys.meta_path.insert(0, Refuse())
from tests.synthetic_source import source_fixture, synthetic_reference
from pathlib import Path
import tempfile
with tempfile.TemporaryDirectory() as directory:
    root = Path(directory)
    source_fixture(root)
    synthetic_reference(root)
assert not blocked.intersection(sys.modules)
'''
        result = subprocess.run([sys.executable, '-c', code], cwd=Path(__file__).resolve().parents[1],
                                capture_output=True, text=True, timeout=WAIT)
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_reference_has_fixed_schema_and_only_reference_rows(self):
        # DDL 指纹取自 67d11a65 的原始辅助函数，不是原生候选。
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for path, digest, counts in (
                (source_fixture(root), '7b51615e0b4b1f6cfdf759193f0f1fc771eb4840ff28bd315ace4d17561958d6',
                 {'assets': 4, 'character_aliases': 2, 'characters': 2, 'decisions': 2, 'labels': 7}),
                (synthetic_reference(root), 'dadce096b39c710109ed0a2cf921376e60ed7c5c930c313c17e56896d58ec5be',
                 {'assets': 0, 'character_aliases': 2, 'characters': 2, 'decisions': 0,
                  'embeddings': 0, 'labels': 0, 'runs': 1}),
            ):
                with closing(sqlite3.connect(path)) as c:
                    self.assertEqual(c.execute('PRAGMA quick_check').fetchone()[0], 'ok')
                    schema = c.execute('SELECT type,name,tbl_name,sql FROM sqlite_schema ORDER BY type,name').fetchall()
                    self.assertEqual(hashlib.sha256(json.dumps(schema, ensure_ascii=False).encode()).hexdigest(), digest)
                    self.assertEqual({name: c.execute(f'SELECT count(*) FROM {name}').fetchone()[0]
                                      for kind, name, _, _ in schema if kind == 'table'}, counts)
                    self.assertEqual(c.execute('SELECT rowid,* FROM characters').fetchall(),
                                     [(1, 'alice', 'Alice', None, None, 'Example', 'alice_tag'),
                                      (2, 'bob', 'Bob', None, None, 'Example', 'bob_tag')])
                    self.assertEqual(c.execute('SELECT rowid,* FROM character_aliases').fetchall(),
                                     [(1, 'alice', '爱丽丝'), (2, 'alice', 'a_li_ce')])
            self.assertEqual(json.loads((root / 'album-char-map.json').read_text()),
                             {'map': {'alice_album': {'char_id': 'alice'}}})
