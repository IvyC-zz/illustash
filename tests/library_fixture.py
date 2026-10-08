"""合成包的布置工具；ZIP 输入、来源字典和最近列表均由测试显式给出。

仅复制夹具数据，不初始化、迁移或打开生产应用，不执行研究计算。
"""
from contextlib import closing
import json
import os
from pathlib import Path
import sqlite3
import tempfile
import zipfile

from tests.fixed_expectations import Library as Package

APP_SUPPORT = SHARED = HOME = REFERENCE = None
EXPORTED = ('library.json', 'source.sqlite', 'library.sqlite', 'features/full-features.npy',
            'features/full-manifest.csv', 'album-char-map.json')
REFERENCE_TABLES = ('characters', 'character_aliases', 'runs')

class Library(Package):
    def __init__(self, path, config=None):
        super().__init__(path)
        if config is not None:
            self._config = config


def open_library(path):
    return Library(path)


def folder_source(path, preset=None, source_id='s1'):
    return dict(id=source_id, kind='folder', path=str(Path(path).resolve()),
                preset=preset or 'other', preset_origin='user' if preset else 'auto')


def ensure_source_schema(c):
    columns = {r[1] for r in c.execute('PRAGMA table_info(assets)')}
    for col in ('source_id', 'preset'):
        if col not in columns:
            c.execute('ALTER TABLE assets ADD COLUMN ' + col + ' TEXT')
    c.executescript("""CREATE TABLE IF NOT EXISTS asset_links(asset TEXT NOT NULL,system TEXT NOT NULL,key TEXT NOT NULL,
    source_id TEXT NOT NULL,current INTEGER NOT NULL DEFAULT 1,PRIMARY KEY(system,key,source_id));
    CREATE INDEX IF NOT EXISTS asset_links_asset ON asset_links(asset);
    CREATE TABLE IF NOT EXISTS files(source_id TEXT NOT NULL,path TEXT NOT NULL,size INTEGER NOT NULL,mtime_ns INTEGER NOT NULL,
    md5 TEXT NOT NULL,asset TEXT NOT NULL,PRIMARY KEY(source_id,path)); CREATE INDEX IF NOT EXISTS files_md5 ON files(md5);""")


def unique_path(folder, name):
    base = Path(folder) / (name + '.pilib')
    number = 2
    while base.exists():
        base = Path(folder) / f'{name} {number}.pilib'
        number += 1
    return base


def export_library(lib, dest):
    """独立生成可信 ZIP 输入；数据库 backup 会包含已提交的 WAL 行。"""
    with tempfile.TemporaryDirectory() as tmp, zipfile.ZipFile(dest, 'w', zipfile.ZIP_DEFLATED) as z:
        for name in EXPORTED:
            source = lib.path / name
            if not source.exists():
                continue
            if name.endswith('.sqlite'):
                copy = Path(tmp) / source.name
                with closing(sqlite3.connect(source)) as a, closing(sqlite3.connect(copy)) as b:
                    a.backup(b)
                z.write(copy, name)
            else:
                z.write(source, name)
    return Path(dest)


def import_library(archive, folder):
    """只恢复本测试刚生成的可信输入；待测导入拒绝逻辑在原生 HTTP 中。"""
    with zipfile.ZipFile(archive) as z:
        config = json.loads(z.read('library.json'))
        path = unique_path(folder, config['name'])
        path.mkdir(parents=True)
        z.extractall(path)
    lib = Library(path)
    lib.config['imported_from'] = Path(archive).name
    lib.save()
    return lib


def remember(lib, front=True):
    APP_SUPPORT.mkdir(parents=True, exist_ok=True)
    path = APP_SUPPORT / 'libraries.json'
    items = json.loads(path.read_text()).get('recent', []) if path.exists() else []
    entry = dict(path=str(lib.path), name=lib.name, opened_at='2026-10-01T00:00:00+00:00')
    items = [item for item in items if item['path'] != str(lib.path)]
    items.insert(0 if front else len(items), entry)
    path.write_text(json.dumps({'recent':items}, ensure_ascii=False))


def set_preset(lib, source_id, preset):
    """明确布置期望字段；不含输入验证或生产调用。"""
    source = next(s for s in lib.config['sources'] if s['id'] == source_id)
    source.update(preset=preset, preset_origin='user')
    lib.save()
    with closing(sqlite3.connect(lib.source)) as c, c:
        c.execute('UPDATE assets SET preset=? WHERE source_id=?', (preset, source_id))
    return source
