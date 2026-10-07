"""合成机器缓存夹具；固定 schema 与明确的测试行，不计算或迁移生产数据。"""
from pathlib import Path
import sqlite3

SCHEMA = (Path(__file__).parent / 'fixtures/provenance_schema.sql').read_text()
VERSION = 5   # the provenance cache schema version (former provenance.db.VERSION)


def connect(library):
    """The provenance cache as the former Python connection opened it: WAL, foreign keys,
    the current schema and its user_version."""
    c = sqlite3.connect(Path(library.path) / 'provenance.sqlite', timeout=30)
    c.row_factory = sqlite3.Row
    c.execute('PRAGMA journal_mode=WAL')
    c.execute('PRAGMA foreign_keys=ON')
    if c.execute('PRAGMA user_version').fetchone()[0] < VERSION:
        c.executescript(SCHEMA)
        c.execute(f'PRAGMA user_version={VERSION}')
    return c


def artist_for(c, links, source):
    # A stable fixture key keeps generated inputs independent of a random UUID provider.
    artist = next((r[0] for s,k,_ in links for r in c.execute(
        'SELECT artist_id FROM artist_links WHERE system=? AND key=?', (s,k))), None)
    if artist is None:
        artist = 'synthetic-artist-' + str(c.execute('SELECT count(*) FROM artists').fetchone()[0])
        c.execute('INSERT INTO artists(id) VALUES (?)', (artist,))
    c.executemany('INSERT OR IGNORE INTO artist_links(system,key,artist_id,url,source) VALUES (?,?,?,?,?)',
                  [(s,k,artist,url,source) for s,k,url in links])
    return artist


def path_of(lib):
    return lib.path / 'provenance.sqlite'
