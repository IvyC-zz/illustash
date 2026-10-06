"""Bounded database adapter for an explicitly provided disposable research snapshot.

No application initialization, schema upgrades, history, account access or HTTP bridge.
The native owner creates the snapshot and publishes accepted results into the application.
"""
from contextlib import contextmanager
from pathlib import Path
import json
import sqlite3
import subprocess

TABLES = {'clusters', 'cluster_info', 'image_sigs', 'relations', 'similar_paired', 'similar_retry'}


class Snapshot:
    def __init__(self, path):
        root = Path(path).resolve()
        self.source, self.path = root / 'source.sqlite', root / 'library.sqlite'
        if not self.source.is_file() or not self.path.is_file():
            raise ValueError('research snapshot databases missing')

    @contextmanager
    def connect(self):
        c = sqlite3.connect(self.path.as_uri() + '?mode=rw', uri=True, timeout=20)
        c.row_factory = sqlite3.Row
        def authorize(action, table, column, database, trigger):
            if action in (sqlite3.SQLITE_INSERT, sqlite3.SQLITE_UPDATE, sqlite3.SQLITE_DELETE,
                          sqlite3.SQLITE_CREATE_TABLE, sqlite3.SQLITE_DROP_TABLE):
                if table not in TABLES and table not in ('sqlite_master', 'sqlite_temp_master'):
                    return sqlite3.SQLITE_DENY
            if action in (sqlite3.SQLITE_ATTACH, sqlite3.SQLITE_DETACH):
                return sqlite3.SQLITE_DENY
            return sqlite3.SQLITE_OK
        c.set_authorizer(authorize)
        try:
            yield c
            c.commit()
        except BaseException:
            c.rollback()
            raise
        finally:
            c.close()

    def require_writable(self):
        # Only this adapter's research tables are writable; stable publication is native.
        pass

    @staticmethod
    def _resolve(c, ident):
        for _ in range(16):
            row = c.execute('SELECT merged_into FROM entities WHERE id=?', (ident,)).fetchone()
            if not row or not row[0]:
                return ident
            ident = row[0]
        return ident

    def image_path(self, ident):
        with self.connect() as c:
            row = c.execute('SELECT src_path FROM assets WHERE uuid=?', (ident,)).fetchone()
            return Path(row[0]) if row and row[0] else None

    def refresh_similar(self):
        # Rust rebuilds similar/duplicate projections after validating the complete result.
        pass

    def publish_learning(self, core, batch):
        result = subprocess.run([core, 'publish-learning', '--db', str(self.path)],
                                input=json.dumps(batch, allow_nan=False), text=True,
                                capture_output=True, timeout=1800)
        if result.returncode:
            raise ValueError(result.stderr.strip() or 'snapshot learning publication failed')
        return json.loads(result.stdout)
