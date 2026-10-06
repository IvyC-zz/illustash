"""图库包（.pilib）：一个图库一个文件夹，自包含；图片不复制，只建索引。设计见 docs/LIBRARIES.md。

包里的文件（路径就是契约，以后 Rust 核心直接打开同一个包）：
    library.json        配置：format、id、名字、来源列表
    source.sqlite       源库（旧的 pi.db）
    library.sqlite      应用库
    features/           本图库的 CCIP 特征（full-features.npy + full-manifest.csv）
    thumbnails/         缩略图缓存，可随时删掉重建
    backups/            升级前的自动备份
    album-char-map.json 相册名到角色的映射（只有从旧库迁来的图库有）

包外的外部角色索引、锚点和检查状态在 SHARED；模型和 tag 表是安装资源。最近图库记在 APP_SUPPORT/libraries.json。

命令行：
    python -m web.library adopt --from review --to ~/Pictures/Illustash/我的图库.pilib   # 把旧的 review/ 数据迁成图库
    python -m web.library info <图库包>
"""
import argparse
from contextlib import closing
import datetime
import json
import os
import sqlite3
import sys
import uuid
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
FORMAT = 1
EXT = '.pilib'
HOME = Path.home() / 'Pictures' / 'Illustash'
APP_SUPPORT = Path(os.environ.get('ILLUSTASH_APP_SUPPORT', Path.home() / 'Library' / 'Application Support' / 'Illustash'))
class LibraryError(Exception):
    pass


def shared_root(root, configured=None, *, cwd=None):
    """共享资源相对路径以资源根为准；旧 CWD 中已有另一套资源时要求显式绝对路径。"""
    cwd = Path.cwd() if cwd is None else Path(cwd)
    root = Path(root)
    root = (root if root.is_absolute() else cwd / root).resolve()
    if configured is None:
        return root / 'review'
    configured = Path(configured)
    if configured.is_absolute():
        return configured.resolve()
    desired, legacy = (root / configured).resolve(), (cwd / configured).resolve()
    names = ('reference.sqlite', 'char-index', 'game-chars', 'char-anchors.npy', 'char-anchors.txt', 'char-patch')
    if desired != legacy:
        for name in names:
            try:
                (legacy / name).lstat()
            except FileNotFoundError:
                continue
            raise LibraryError('ILLUSTASH_SHARED 相对路径在旧工作目录下已有共享资源；请指定原目录的绝对路径')
    return desired


# 开发期共享资源默认仍在资源根的 review/，不移动已有数据。
SHARED = shared_root(os.environ.get('ILLUSTASH_ROOT', ROOT), os.environ.get('ILLUSTASH_SHARED'))
CONFIG = 'library.json'
# Schema plus the reference rows a new library's source database starts from (character index, model runs).
REFERENCE = SHARED / 'reference.sqlite'
REFERENCE_TABLES = ('characters', 'character_aliases', 'runs')
PHOTOS_APPS = {'com.atebits.Tweetie2': 'x', 'net.pxv.iphone': 'pixiv'}


def is_library(path):
    return (Path(path) / CONFIG).is_file()


def shared_dir(source):
    """Where the shared resources live for a store whose source database is `source`.
    Inside a library package they are outside it (SHARED); a bare source database keeps them beside it (tests, old layout)."""
    parent = Path(source).resolve().parent
    return SHARED if is_library(parent) else parent


def backups_dir(db):
    parent = Path(db).resolve().parent
    return parent / ('backups' if is_library(parent) else 'library-backups')


def features_dir(source):
    parent = Path(source).resolve().parent
    return parent / 'features' if is_library(parent) else parent


class Library:
    def __init__(self, path, config):
        self.path, self.config = Path(path), config

    name = property(lambda self: self.config['name'])
    source = property(lambda self: self.path / 'source.sqlite')
    db = property(lambda self: self.path / 'library.sqlite')
    features = property(lambda self: self.path / 'features')
    thumbnails = property(lambda self: self.path / 'thumbnails')
    backups = property(lambda self: self.path / 'backups')
    # A test library is a frozen snapshot of the persona it was cut from: never synced with its sources.
    frozen = property(lambda self: bool(self.config.get('test') or self.config.get('frozen')))

    def photos(self):
        """The Apple Photos library of the first photos source, if any."""
        for s in self.config.get('sources', []):
            if s.get('kind') == 'photos':
                return Path(os.path.expanduser(s['path']))
        return None

    def save(self):
        write_config(self.path, self.config)

    def info(self):
        size = lambda p: sum(f.stat().st_size for f in (p, Path(str(p) + '-wal')) if f.exists())
        return {'path': str(self.path), 'name': self.name, 'id': self.config['id'], 'format': self.config['format'],
                'sources': self.config.get('sources', []), 'test': self.config.get('test'),
                'source_bytes': size(self.source), 'library_bytes': size(self.db)}


def write_config(path, config):
    tmp = Path(path) / (CONFIG + '.tmp')
    tmp.write_text(json.dumps(config, ensure_ascii=False, indent=2) + '\n', encoding='utf-8', newline='\n')
    os.replace(tmp, Path(path) / CONFIG)


def now():
    return datetime.datetime.now().astimezone().isoformat(timespec='seconds')


def photos_source(path, apps=None):
    return {'id': 's1', 'kind': 'photos', 'path': str(path), 'apps': dict(apps or PHOTOS_APPS)}


def create(path, name=None, sources=(), test=None):
    """A new, empty library package. Refuses to touch an existing path."""
    path = Path(path).expanduser().absolute()
    if path.suffix != EXT:
        raise LibraryError(f'图库包的扩展名必须是 {EXT}')
    if path.exists():
        raise LibraryError(f'已存在：{path}')
    path.mkdir(parents=True)
    for sub in ('features', 'thumbnails', 'backups'):
        (path / sub).mkdir()
    config = {'format': FORMAT, 'id': str(uuid.uuid4()).upper(), 'name': name or path.stem, 'created_at': now(),
              'sources': list(sources)}
    if test:
        config['test'] = test
    write_config(path, config)
    return Library(path, config)


def open_library(path):
    path = Path(path).expanduser().absolute()
    if not is_library(path):
        raise LibraryError(f'不是图库包：{path}')
    try:
        config = json.loads((path / CONFIG).read_text(encoding='utf-8'))
    except ValueError as exc:
        raise LibraryError(f'library.json 无法读取：{exc}') from exc
    fmt = config.get('format')
    if not isinstance(fmt, int) or fmt > FORMAT:
        raise LibraryError(f'图库格式 {fmt} 比这个版本的程序新，请先升级程序')
    for sub in ('features', 'thumbnails', 'backups'):
        (path / sub).mkdir(exist_ok=True)
    return Library(path, config)


# ---------- source database: asset links and presets (docs/LIBRARIES.md section 3) ----------
PRESETS = ('x', 'pixiv', 'other')


def ensure_source_schema(c):
    """`assets.uuid` is the library's internal asset id; where an asset lives outside is in `asset_links`.
    Photos assets keep their Photos UUID as the internal id, so nothing existing is renamed."""
    columns = {r[1] for r in c.execute('PRAGMA table_info(assets)')}
    if 'source_id' not in columns:
        c.execute('ALTER TABLE assets ADD COLUMN source_id TEXT')
    if 'preset' not in columns:
        c.execute('ALTER TABLE assets ADD COLUMN preset TEXT')   # 'x' | 'pixiv' | 'other'; NULL = not known yet
    c.execute("""CREATE TABLE IF NOT EXISTS asset_links(asset TEXT NOT NULL, system TEXT NOT NULL, key TEXT NOT NULL,
                 source_id TEXT NOT NULL, current INTEGER NOT NULL DEFAULT 1, PRIMARY KEY(system, key, source_id))""")
    c.execute('CREATE INDEX IF NOT EXISTS asset_links_asset ON asset_links(asset)')
    # Folder sources: what each file looked like when last read, so a rescan only hashes what changed (tests/reference/web/folder.py).
    c.execute("""CREATE TABLE IF NOT EXISTS files(source_id TEXT NOT NULL, path TEXT NOT NULL, size INTEGER NOT NULL,
                 mtime_ns INTEGER NOT NULL, md5 TEXT NOT NULL, asset TEXT NOT NULL, PRIMARY KEY(source_id, path))""")
    c.execute('CREATE INDEX IF NOT EXISTS files_md5 ON files(md5)')


def link_photos(c, source, rows):
    """Record Photos assets: `rows` are (uuid, bundle id). The preset comes from the source's app map."""
    apps = source.get('apps', {})
    c.executemany("UPDATE assets SET source_id=?, preset=coalesce(?, preset) WHERE uuid=?",
                  [(source['id'], apps.get(app, 'other') if app is not None else None, u) for u, app in rows])
    c.executemany("INSERT OR IGNORE INTO asset_links(asset,system,key,source_id) VALUES (?,'photos',?,?)",
                  [(u, u, source['id']) for u, _ in rows])


def upgrade_source(lib):
    """Bring a library's source database to the current layout; cheap when already done.
    Presets of Photos assets are filled from Photos itself when it is readable, else on the next sync."""
    photos = next((s for s in lib.config.get('sources', []) if s.get('kind') == 'photos'), None)
    with sqlite3.connect(lib.source, timeout=30) as c:
        ensure_source_schema(c)
        if photos is None:
            return
        missing = [u for (u,) in c.execute('SELECT uuid FROM assets WHERE source_id IS NULL OR preset IS NULL')]
        if not missing:
            return
        app = {}
        try:
            from web.sync import read_photos
            app = {r[0].upper(): r[8] for r in read_photos(Path(os.path.expanduser(photos['path'])))}
        except (sqlite3.Error, OSError):
            pass
        link_photos(c, photos, [(u, app.get(u)) for u in missing])


# ---------- new libraries: folder sources and the reference database ----------
def guess_preset(path):
    """X / Pixiv / other from a folder name (Android keeps each app's saves in its own folder)."""
    import re
    words = re.split(r'[^a-z0-9]+', ' '.join(Path(path).parts[-2:]).lower())
    if any('pixiv' in w for w in words):
        return 'pixiv'
    if any(w in ('x', 'twitter', 'tweetie', 'twimg') for w in words):
        return 'x'
    return 'other'


def folder_source(path, preset=None, source_id='s1'):
    path = Path(path).expanduser().resolve()
    if preset is not None and preset not in PRESETS:
        raise LibraryError(f'来源预设只能是 {"、".join(PRESETS)}')
    return {'id': source_id, 'kind': 'folder', 'path': str(path), 'preset': preset or guess_preset(path),
            'preset_origin': 'user' if preset else 'auto'}


def _copy_schema(dst, attached, rows_of=()):
    tables = dst.execute(f"SELECT name, sql FROM {attached}.sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'").fetchall()
    for _, sql in tables:
        dst.execute(sql)
    for name, _ in tables:
        if name in rows_of:
            dst.execute(f'INSERT INTO main.{name} SELECT * FROM {attached}.{name}')
    for (sql,) in dst.execute(f"SELECT sql FROM {attached}.sqlite_master WHERE type='index' AND sql IS NOT NULL").fetchall():
        dst.execute(sql.replace('CREATE INDEX ', 'CREATE INDEX IF NOT EXISTS ', 1))


def export_reference(source, dest=None):
    """The shared starting point for new libraries: the full source schema, rows only for REFERENCE_TABLES."""
    dest = Path(dest or REFERENCE)
    tmp = dest.with_suffix('.tmp')
    tmp.unlink(missing_ok=True)
    # uri=True: ATTACH of a file: URI otherwise depends on SQLite being built with SQLITE_USE_URI.
    # closing：Windows 不能替换仍打开的文件；sqlite3 的 with 只提交不关闭。
    with closing(sqlite3.connect(tmp.resolve().as_uri(), uri=True)) as dst:
        dst.execute('ATTACH ? AS ref', (Path(source).resolve().as_uri() + '?mode=ro',))
        _copy_schema(dst, 'ref', REFERENCE_TABLES)
        dst.commit()
        dst.execute('DETACH ref')
    os.replace(tmp, dest)
    return dest


def init_source(lib, reference=None):
    """An empty source database for a new library: the reference schema and character index, no assets."""
    reference = Path(reference or REFERENCE)
    if not reference.is_file():
        raise LibraryError(f'缺少参考库 {reference}（python -m web.library export-reference）')
    if lib.source.exists():
        raise LibraryError(f'源库已存在：{lib.source}')
    with sqlite3.connect(lib.source.resolve().as_uri(), uri=True) as dst:
        dst.execute('ATTACH ? AS ref', (reference.resolve().as_uri() + '?mode=ro',))
        _copy_schema(dst, 'ref', REFERENCE_TABLES)
        ensure_source_schema(dst)
        dst.commit()
        dst.execute('DETACH ref')


def new_folder_library(folder, dest=None, name=None, preset=None):
    folder = Path(folder).expanduser().resolve()
    if not folder.is_dir():
        raise LibraryError(f'不是文件夹：{folder}')
    source = folder_source(folder, preset)
    lib = create(dest or HOME / f'{folder.name}{EXT}', name or folder.name, [source])
    try:
        init_source(lib)
    except BaseException:
        import shutil
        shutil.rmtree(lib.path)
        raise
    return lib


def new_photos_library(dest, name=None, photos=None):
    from web.sync import PHOTOS
    lib = create(dest, name, [photos_source(photos or PHOTOS)])
    try:
        init_source(lib)
    except BaseException:
        import shutil
        shutil.rmtree(lib.path)
        raise
    return lib


def unique_path(folder, name):
    """<folder>/<name>.pilib, or <name> 2.pilib, <name> 3.pilib … when taken."""
    folder, clean = Path(folder).expanduser(), ''.join(c for c in name if c not in '/:\\').strip() or '图库'
    path, n = folder / f'{clean}{EXT}', 2
    while path.exists():
        path, n = folder / f'{clean} {n}{EXT}', n + 1
    return path


def set_preset(lib, source_id, preset):
    """The user's choice of a folder source's preset; also rewrites the preset of that source's images."""
    if preset not in PRESETS:
        raise LibraryError(f'来源预设只能是 {"、".join(PRESETS)}')
    source = next((s for s in lib.config['sources'] if s['id'] == source_id and s.get('kind') == 'folder'), None)
    if source is None:
        raise LibraryError('没有这个文件夹来源')
    source['preset'], source['preset_origin'] = preset, 'user'
    lib.save()
    with sqlite3.connect(lib.source, timeout=30) as c:
        ensure_source_schema(c)
        c.execute('UPDATE assets SET preset=? WHERE source_id=?', (preset, source_id))
    return source


# ---------- export / import: one zip, recognition and human knowledge, no pictures ----------
EXPORTED = ('library.json', 'source.sqlite', 'library.sqlite', 'features/full-features.npy', 'features/full-manifest.csv',
            'album-char-map.json')


def export_library(lib, dest):
    """A consistent zip of the library (SQLite backup API, never a copy of a file being written). Pictures,
    thumbnails and backups stay out: the zip is for moving machines or sharing a test library."""
    import tempfile
    import zipfile
    dest = Path(dest).expanduser()
    dest.parent.mkdir(parents=True, exist_ok=True)
    tmp = dest.with_name(dest.name + '.tmp')
    with tempfile.TemporaryDirectory() as work, zipfile.ZipFile(tmp, 'w', zipfile.ZIP_DEFLATED, allowZip64=True) as z:
        for rel in EXPORTED:
            src = lib.path / rel
            if not src.exists():
                continue
            if src.suffix == '.sqlite':
                copy = Path(work) / src.name
                with closing(sqlite3.connect(src.as_uri() + '?mode=ro', uri=True)) as a, closing(sqlite3.connect(copy)) as b:
                    a.backup(b)
                z.write(copy, rel)
                copy.unlink()
            else:
                z.write(src, rel)
    os.replace(tmp, dest)
    return dest


def import_library(archive, folder=None):
    """Unpack an exported zip into a new package beside the other libraries; refuses anything that is not one."""
    import zipfile
    archive = Path(archive).expanduser()
    with zipfile.ZipFile(archive) as z:
        names = set(z.namelist())
        if 'library.json' not in names or 'source.sqlite' not in names:
            raise LibraryError('不是导出的图库（缺少 library.json 或 source.sqlite）')
        if any(n.startswith('/') or '..' in Path(n).parts or n not in EXPORTED for n in names):
            raise LibraryError('导出文件里有不认识的内容，已拒绝导入')
        config = json.loads(z.read('library.json'))
        fmt = config.get('format')
        if not isinstance(fmt, int) or fmt > FORMAT:
            raise LibraryError(f'图库格式 {fmt} 比这个版本的程序新，请先升级程序')
        path = unique_path(folder or HOME, config.get('name') or archive.stem)
        path.mkdir(parents=True)
        try:
            for n in names:
                (path / n).parent.mkdir(parents=True, exist_ok=True)
                with z.open(n) as src, open(path / n, 'wb') as out:   # streamed: a source database can be gigabytes
                    import shutil
                    shutil.copyfileobj(src, out, 1 << 20)
            for sub in ('features', 'thumbnails', 'backups'):
                (path / sub).mkdir(exist_ok=True)
            config['imported_from'] = archive.name
            write_config(path, config)
        except BaseException:
            import shutil
            shutil.rmtree(path)
            raise
    return open_library(path)


def forget(path):
    """Take a library off the recently opened list; the package itself is left alone."""
    items = [i for i in recent() if Path(i['path']) != Path(path).expanduser().absolute()]
    APP_SUPPORT.mkdir(parents=True, exist_ok=True)
    tmp = _recent_file().with_suffix('.tmp')
    tmp.write_text(json.dumps({'recent': items}, ensure_ascii=False, indent=2) + '\n', encoding='utf-8', newline='\n')
    os.replace(tmp, _recent_file())


# ---------- recently opened ----------
def _recent_file():
    return APP_SUPPORT / 'libraries.json'


def recent():
    try:
        items = json.loads(_recent_file().read_text(encoding='utf-8')).get('recent', [])
    except (OSError, ValueError):
        return []
    return [i for i in items if is_library(i.get('path', ''))]


def remember(library, front=True):
    """Add to the recently opened list; `front=False` lists it without making it the one opened next time."""
    items = [i for i in recent() if Path(i['path']) != library.path]
    entry = {'path': str(library.path), 'name': library.name, 'opened_at': now()}
    if front:
        items.insert(0, entry)
    else:
        items.append(entry)
    APP_SUPPORT.mkdir(parents=True, exist_ok=True)
    tmp = _recent_file().with_suffix('.tmp')
    tmp.write_text(json.dumps({'recent': items[:20]}, ensure_ascii=False, indent=2) + '\n', encoding='utf-8', newline='\n')
    os.replace(tmp, _recent_file())


def last():
    items = recent()
    return Path(items[0]['path']) if items else None


# ---------- adopting the old review/ layout ----------
# old name in review/ -> path inside the package
LEGACY = {'pi.db': 'source.sqlite', 'library.sqlite': 'library.sqlite',
          'full-features.npy': 'features/full-features.npy', 'full-manifest.csv': 'features/full-manifest.csv',
          'album-char-map.json': 'album-char-map.json', 'web-thumbnails': 'thumbnails', 'library-backups': 'backups'}
# left behind as symlinks so the research tools that still read review/pi.db keep working
LINK_BACK = ('pi.db', 'library.sqlite', 'full-features.npy', 'full-manifest.csv')


def _checkpoint(db):
    """Fold the WAL into the database file so the move takes one file, and fail if anything holds it open."""
    c = sqlite3.connect(db, timeout=5)
    try:
        busy, _, _ = c.execute('PRAGMA wal_checkpoint(TRUNCATE)').fetchone()
        if busy:
            raise LibraryError(f'{db.name} 仍被其他进程打开（请先停止图库服务）')
        if c.execute('PRAGMA quick_check').fetchone()[0] != 'ok':
            raise LibraryError(f'{db.name} 完整性检查失败，已停止迁移')
    finally:
        c.close()


def adopt(review, dest, name='我的图库', photos=None):
    """Move the old review/ data into a new library package (a rename on the same volume, never a copy),
    then leave symlinks for the database files so older tools keep reading the same data."""
    review, dest = Path(review).resolve(), Path(dest).expanduser()
    if not (review / 'pi.db').is_file() or (review / 'pi.db').is_symlink():
        raise LibraryError(f'{review} 里没有旧的 pi.db（可能已经迁移过）')
    dest.parent.mkdir(parents=True, exist_ok=True)
    if os.stat(review).st_dev != os.stat(dest.parent).st_dev:
        raise LibraryError('图库包必须和 review/ 在同一个磁盘上（迁移用移动，不复制 5 GB 以上的数据）')
    for db in ('pi.db', 'library.sqlite'):
        if (review / db).exists():
            _checkpoint(review / db)
    from web.sync import PHOTOS
    lib = create(dest, name, [photos_source(photos or PHOTOS)])
    moved = []
    try:
        for old, new in LEGACY.items():
            src = review / old
            if not src.exists():
                continue
            target = lib.path / new
            if target.is_dir() and not any(target.iterdir()):
                target.rmdir()
            os.replace(src, target)
            moved.append((src, target))
            for suffix in ('-wal', '-shm'):
                side = Path(str(src) + suffix)
                if side.exists():
                    side.unlink()   # checkpointed and truncated above; SQLite recreates them next to the new path
    except BaseException:
        for src, target in reversed(moved):
            os.replace(target, src)
        raise
    for old in LINK_BACK:
        target = lib.path / LEGACY[old]
        if target.exists():
            os.symlink(target, review / old)
    lib.config['adopted_from'] = str(review)
    lib.save()
    return lib


def main(argv=None):
    ap = argparse.ArgumentParser(description='图库包')
    sub = ap.add_subparsers(dest='cmd', required=True)
    a = sub.add_parser('adopt', help='把旧的 review/ 数据迁成一个图库包')
    a.add_argument('--from', dest='review', type=Path, default=ROOT / 'review')
    a.add_argument('--to', type=Path, default=HOME / f'我的图库{EXT}')
    a.add_argument('--name', default='我的图库')
    i = sub.add_parser('info', help='显示图库包的信息')
    i.add_argument('path', type=Path)
    n = sub.add_parser('new', help='用一个文件夹新建图库')
    n.add_argument('--folder', type=Path, required=True)
    n.add_argument('--preset', choices=PRESETS, help='来源预设；不给时按文件夹名猜')
    n.add_argument('--to', type=Path, help=f'图库包路径（默认 {HOME}/<文件夹名>{EXT}）')
    n.add_argument('--name')
    r = sub.add_parser('export-reference', help='从一个图库导出新建图库用的参考库')
    r.add_argument('--from', dest='library', type=Path, default=HOME / f'我的图库{EXT}')
    args = ap.parse_args(argv)
    try:
        if args.cmd == 'adopt':
            lib = adopt(args.review, args.to, args.name)
            remember(lib)
            print(f'已迁移：{lib.path}')
        elif args.cmd == 'new':
            lib = new_folder_library(args.folder, args.to, args.name, args.preset)
            remember(lib, front=False)
            print(f"已新建：{lib.path}（来源预设 {lib.config['sources'][0]['preset']}）")
        elif args.cmd == 'export-reference':
            print(f'参考库：{export_reference(open_library(args.library).source)}')
        else:
            print(json.dumps(open_library(args.path).info(), ensure_ascii=False, indent=2))
    except LibraryError as exc:
        sys.exit(str(exc))


if __name__ == '__main__':
    main()
