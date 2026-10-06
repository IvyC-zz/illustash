"""Research snapshot adapter cannot orchestrate the application or write formal tables."""
import sqlite3
import tempfile
import unittest
from pathlib import Path
from web.research_snapshot import Snapshot


class SnapshotBoundaryTest(unittest.TestCase):
    def test_research_cache_commits_formal_writes_and_attach_are_denied(self):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            with sqlite3.connect(root/'source.sqlite'):
                pass
            with sqlite3.connect(root/'library.sqlite') as c:
                c.execute('CREATE TABLE actions(id INTEGER)')
                c.execute('CREATE TABLE entities(id TEXT,merged_into TEXT)')
                c.execute("INSERT INTO entities VALUES ('old','new'),('new',NULL)")
            snapshot = Snapshot(root)
            with snapshot.connect() as c:
                c.execute('CREATE TABLE clusters(uuid TEXT,cid INTEGER)')
                c.execute("INSERT INTO clusters VALUES ('synthetic',1)")
                self.assertEqual(snapshot._resolve(c,'old'),'new')
                for sql in ('INSERT INTO actions VALUES (1)', 'DELETE FROM entities',
                            "ATTACH ':memory:' AS external"):
                    with self.assertRaises(sqlite3.DatabaseError):
                        c.execute(sql)
            with snapshot.connect() as c:
                self.assertEqual(tuple(c.execute('SELECT * FROM clusters').fetchone()),('synthetic',1))
                self.assertEqual(c.execute('SELECT count(*) FROM actions').fetchone()[0],0)
