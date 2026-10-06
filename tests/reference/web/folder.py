"""Folder sources: keep a library in step with a plain folder of images (docs/LIBRARIES.md section 4).

The folder is only read. Every image file becomes an asset with a fresh internal id; its relative path is an
`asset_links` row ('file') and `files` remembers size, mtime and MD5, so a rescan only hashes what changed and a
moved or renamed file keeps its asset, and with it every human verdict. Vanished files are marked trashed, the same
as a picture deleted from Photos. Returns the same counts as `web.sync.scan`, so `Syncer` runs either.
"""
import datetime
import hashlib
import os
import sqlite3
import uuid
from pathlib import Path

from PIL import Image

from web.library import ensure_source_schema

EXTENSIONS = {'.jpg', '.jpeg', '.png', '.webp', '.gif'}
PRESET_NAMES = {'x': 'X', 'pixiv': 'Pixiv', 'other': '其他'}


def walk(root):
    """(relative posix path, absolute path, stat) for every image file, skipping hidden files and folders."""
    for folder, dirs, files in os.walk(root):
        dirs[:] = sorted(d for d in dirs if not d.startswith('.') and not d.endswith('.pilib'))
        for name in sorted(files):
            if name.startswith('.') or Path(name).suffix.lower() not in EXTENSIONS:
                continue
            path = Path(folder) / name
            try:
                st = path.stat()
            except OSError:
                continue
            yield path.relative_to(root).as_posix(), path, st


def md5_of(path):
    h = hashlib.md5()
    with open(path, 'rb') as f:
        for chunk in iter(lambda: f.read(1 << 20), b''):
            h.update(chunk)
    return h.hexdigest()


def describe(path, st):
    """Width, height (header only) and when the file arrived (birth time where the platform keeps it)."""
    try:
        with Image.open(path) as im:
            width, height = im.size
    except (OSError, ValueError, Image.DecompressionBombError):
        width = height = 0
    born = getattr(st, 'st_birthtime', None) or st.st_mtime
    return width, height, datetime.datetime.fromtimestamp(born).isoformat(timespec='seconds')


def scan(source, pi_db):
    """Write new images of a folder source into the source database, follow moved files, trash vanished ones."""
    root = Path(source['path'])
    if not root.is_dir():
        raise FileNotFoundError(f'文件夹不存在或无法访问：{root}')
    sid, preset = source['id'], source.get('preset', 'other')
    db = sqlite3.connect(pi_db, timeout=30)
    try:
        ensure_source_schema(db)
        known = {p: (size, mtime, md5, asset) for p, size, mtime, md5, asset in
                 db.execute('SELECT path,size,mtime_ns,md5,asset FROM files WHERE source_id=?', (sid,))}
        trashed = {u for (u,) in db.execute('SELECT uuid FROM assets WHERE source_id=? AND coalesce(trashed,0)=1', (sid,))}
        seen, fresh = {}, []            # path -> asset for unchanged files; files still to hash
        for rel, path, st in walk(root):
            old = known.get(rel)
            if old and old[0] == st.st_size and old[1] == st.st_mtime_ns:
                seen[rel] = old[3]
            else:
                fresh.append((rel, path, st))
        hashed = []
        for rel, path, st in fresh:
            try:
                hashed.append((rel, path, st, md5_of(path)))
            except OSError:
                continue
        # A file whose old path vanished and whose content turns up elsewhere was moved or renamed.
        vanished = {p: v for p, v in known.items() if p not in seen and p not in {h[0] for h in hashed}}
        movable = {}
        for p, (_, _, md5, asset) in vanished.items():
            movable.setdefault(md5, []).append((p, asset))
        new, moved, changed = [], [], []
        for rel, path, st, md5 in hashed:
            if rel in known:                               # same path, new content: same asset, recognise again
                changed.append((rel, path, st, md5, known[rel][3]))
            elif movable.get(md5):
                old_path, asset = movable[md5].pop(0)
                vanished.pop(old_path, None)
                moved.append((rel, path, st, md5, asset, old_path))
            else:
                new.append((rel, path, st, md5, str(uuid.uuid4()).upper()))
        gone = [v[3] for v in vanished.values()]
        back = [a for a in list(seen.values()) + [m[4] for m in moved] if a in trashed]
        with db:
            for rel, path, st, md5, asset in new:
                width, height, added = describe(path, st)
                db.execute("""INSERT INTO assets(uuid,filename,width,height,added_at,hidden,trashed,stable_hash,orig_size,
                              src_kind,src_path,in_scope,source_id,preset) VALUES (?,?,?,?,?,0,0,?,?,'orig',?,1,?,?)""",
                           (asset, path.name, width, height, added, md5, st.st_size, str(path), sid, preset))
                db.execute("INSERT OR REPLACE INTO asset_links(asset,system,key,source_id) VALUES (?,'file',?,?)", (asset, rel, sid))
                db.execute('INSERT OR REPLACE INTO files VALUES (?,?,?,?,?,?)', (sid, rel, st.st_size, st.st_mtime_ns, md5, asset))
            for rel, path, st, md5, asset, old_path in moved:
                db.execute('UPDATE assets SET filename=?,src_path=? WHERE uuid=?', (path.name, str(path), asset))
                db.execute("DELETE FROM asset_links WHERE system='file' AND key=? AND source_id=?", (old_path, sid))
                db.execute("INSERT OR REPLACE INTO asset_links(asset,system,key,source_id) VALUES (?,'file',?,?)", (asset, rel, sid))
                db.execute('DELETE FROM files WHERE source_id=? AND path=?', (sid, old_path))
                db.execute('INSERT OR REPLACE INTO files VALUES (?,?,?,?,?,?)', (sid, rel, st.st_size, st.st_mtime_ns, md5, asset))
            for rel, path, st, md5, asset in changed:
                width, height, _ = describe(path, st)
                db.execute('UPDATE assets SET width=?,height=?,stable_hash=?,orig_size=? WHERE uuid=?', (width, height, md5, st.st_size, asset))
                db.execute('DELETE FROM labels WHERE uuid=?', (asset,))        # stale recognition: the backlog picks it up again
                db.execute('DELETE FROM embeddings WHERE uuid=?', (asset,))
                db.execute('UPDATE files SET size=?,mtime_ns=?,md5=? WHERE source_id=? AND path=?', (st.st_size, st.st_mtime_ns, md5, sid, rel))
            db.executemany('UPDATE assets SET trashed=1 WHERE uuid=?', [(a,) for a in gone])
            db.executemany('UPDATE assets SET trashed=0 WHERE uuid=?', [(a,) for a in back])
            total = db.execute('SELECT count(*) FROM files WHERE source_id=?', (sid,)).fetchone()[0] - len(gone)
    finally:
        db.close()
    name = PRESET_NAMES.get(preset, preset)
    return {'new': len(new), 'returned': len(back), 'gone': len(gone), 'moved': len(moved), 'changed': len(changed),
            'sources': {sid: {'name': f'{name}（{root.name}）', 'photos': total, 'collected': total}}}
