"""原生启动备份失败必须拒绝修改；标签契约由 Rust 固定调用重放验收。"""
import contextlib
import os
import sqlite3
import tempfile
import unittest
from pathlib import Path

class RequiredBackupCliTest(unittest.TestCase):
    """Direct shipped CLI must refuse a file-backed upgrade when the backup cannot be created (the cutover commands
    went with #201 step 4)."""
    def test_upgrade_refuses_backup_failure_without_modification(self):
        binary = os.environ.get('ILLUSTASH_TERMS_BIN')
        if not binary or not Path(binary).is_file():
            self.skipTest('set ILLUSTASH_TERMS_BIN explicitly for native CLI acceptance')
        import subprocess
        with tempfile.TemporaryDirectory(prefix='illustash-required-backup-cli-') as tmp:
            root = Path(tmp)
            for command in ('migrate',):
                with self.subTest(command=command):
                    folder = root / command
                    folder.mkdir()
                    db = folder / 'library.sqlite'
                    with contextlib.closing(sqlite3.connect(db)) as c, c:
                        c.executescript("CREATE TABLE synthetic_rows(value BLOB); INSERT INTO synthetic_rows(rowid,value) VALUES (27,x'0001ff');")
                    prepared = subprocess.run([binary, 'migrate', '--db', str(db)], capture_output=True, text=True)
                    self.assertEqual(prepared.returncode, 0, prepared.stderr)
                    if command == 'migrate':
                        with contextlib.closing(sqlite3.connect(db)) as c, c:
                            c.execute("UPDATE meta SET value='6' WHERE key='terms_schema'")
                    blocker = folder / 'library-backups'
                    blocker.write_text('synthetic backup blocker')
                    with contextlib.closing(sqlite3.connect(db)) as c, c:
                        before = list(c.iterdump())
                    refused = subprocess.run([binary, command, '--db', str(db)], capture_output=True, text=True)
                    self.assertNotEqual(refused.returncode, 0)
                    self.assertEqual(refused.stdout, '')
                    with contextlib.closing(sqlite3.connect(db)) as c, c:
                        self.assertEqual(list(c.iterdump()), before)
                        self.assertEqual(c.execute('SELECT rowid,value FROM synthetic_rows').fetchone(), (27, b'\x00\x01\xff'))
                    self.assertEqual(blocker.read_text(), 'synthetic backup blocker')
