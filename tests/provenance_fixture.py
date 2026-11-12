"""合成机器缓存夹具；固定 schema 与明确的测试行，不计算或迁移生产数据。"""
from pathlib import Path
import sqlite3

SCHEMA = (Path(__file__).parent / 'fixtures/provenance_schema.sql').read_text()
VERSION = 5   # 溯源缓存的 schema 版本（原 provenance.db.VERSION）


class Connection(sqlite3.Connection):
    """离开 ``with`` 时提交（或回滚）并关闭，这样测试读取文件字节之前 WAL 已经 checkpoint 进文件。"""
    def __exit__(self, *exc):
        try:
            return super().__exit__(*exc)
        finally:
            self.close()


def connect(library):
    """按原来的 Python 连接的方式打开溯源缓存：WAL、外键、当前 schema 及其 user_version。"""
    c = sqlite3.connect(Path(library.path) / 'provenance.sqlite', timeout=30, factory=Connection)
    c.row_factory = sqlite3.Row
    c.execute('PRAGMA journal_mode=WAL')
    c.execute('PRAGMA foreign_keys=ON')
    if c.execute('PRAGMA user_version').fetchone()[0] < VERSION:
        c.executescript(SCHEMA)
        c.execute(f'PRAGMA user_version={VERSION}')
    return c


def artist_for(c, links, source):
    # 夹具用稳定的键，生成的输入就不依赖随机的 UUID 来源。
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
