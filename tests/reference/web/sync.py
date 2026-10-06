"""Keep the library in step with Apple Photos: read-only diff, local recognition, incremental import.

Photos is only read. New X / Pixiv images and removals are written into the source database
(`pi.db`), recognised by `web.infer` in a child process, then imported batch by batch with
`Store.import_changes`. Runs in the web server process, which holds the Photos permission.
"""
import datetime
import json
import os
import sqlite3
import subprocess
import sys
import tempfile
import threading
import time
from pathlib import Path

PHOTOS = Path.home() / 'Pictures' / 'Photos Library.photoslibrary'
# Only these apps' saves are collected for now; other sources come later via settings.
COLLECT = {'com.atebits.Tweetie2': 'X', 'net.pxv.iphone': 'Pixiv'}
APPS = {**COLLECT, 'com.apple.springboard': '截屏', 'com.apple.camera': '相机', 'com.apple.mobileslideshow': '照片',
        'com.apple.mobilesafari': 'Safari', 'tv.danmaku.bilianime': '哔哩哔哩', 'ai.x.GrokApp': 'Grok',
        'com.tencent.xin': '微信', 'com.baidu.netdisk': '百度网盘', 'app.nicegram': 'Nicegram', '': '未知来源'}
# Above this many images the first pass belongs on the Windows machine (docs/WEB_V2.md).
LOCAL_LIMIT = 3000
CORE_DATA_EPOCH = 978307200


def read_photos(library):
    """Every non-trashed still image in Photos, with the fields the source database keeps."""
    db = sqlite3.connect(f"file:{library / 'database' / 'Photos.sqlite'}?mode=ro", uri=True, timeout=30)
    try:
        db.execute('PRAGMA query_only=ON')
        return db.execute("""SELECT a.ZUUID,a.Z_PK,a.ZWIDTH,a.ZHEIGHT,a.ZADDEDDATE,a.ZHIDDEN,a.ZDIRECTORY,a.ZFILENAME,
            coalesce(d.ZIMPORTEDBYBUNDLEIDENTIFIER,''),d.ZORIGINALSTABLEHASH,d.ZORIGINALFILESIZE
            FROM ZASSET a LEFT JOIN ZADDITIONALASSETATTRIBUTES d ON a.ZADDITIONALATTRIBUTES=d.Z_PK
            WHERE a.ZKIND=0 AND a.ZTRASHEDSTATE=0""").fetchall()
    finally:
        db.close()


def source_path(library, uuid, directory, filename):
    """Photos' local medium copy when present (as in the original manifest), else the original."""
    derivative = library / 'resources' / 'derivatives' / uuid[0] / f'{uuid}_1_105_c.jpeg'
    original = library / 'originals' / (directory or '') / (filename or '')
    if derivative.exists() or not (filename and original.exists()):
        return 'deriv', str(derivative)
    return 'orig', str(original)


def scan(library, pi_db, source=None):
    """Write new collected images into pi.db and mark vanished ones trashed. Returns counts and per-app totals.
    `source` (a library.json photos source) also records each new image's asset link and preset."""
    photos = read_photos(library)
    live = {r[0].upper() for r in photos}
    sources = {}
    db = sqlite3.connect(pi_db, timeout=30)
    try:
        known = dict(db.execute('SELECT uuid,coalesce(trashed,0) FROM assets WHERE in_scope=1'))
        pending = {r[0] for r in db.execute("""SELECT uuid FROM assets a WHERE in_scope=1
            AND NOT EXISTS(SELECT 1 FROM labels l WHERE l.uuid=a.uuid AND l.dim='rating')""")}
        new, new_apps, back = [], [], []
        retry_paths = []
        for uuid, pk, width, height, added, hidden, directory, filename, app, stable, size in photos:
            uuid = uuid.upper()
            entry = sources.setdefault(app, {'name': APPS.get(app, app), 'photos': 0, 'collected': 0})
            entry['photos'] += 1
            if app not in COLLECT:
                continue
            entry['collected'] += 1
            if uuid in known:
                if known[uuid]:
                    back.append(uuid)
                if uuid in pending:
                    # The previous path may have been a missing iCloud derivative. A newly
                    # downloaded original is usable even if that derivative never appears.
                    kind, path = source_path(library, uuid, directory, filename)
                    retry_paths.append((kind, path, uuid))
                continue
            kind, path = source_path(library, uuid, directory, filename)
            when = datetime.datetime.fromtimestamp(added + CORE_DATA_EPOCH).isoformat(timespec='seconds') if added is not None else ''
            new.append((uuid, filename, pk, width, height, when, hidden, 0, stable, size, kind, path, 1))
            new_apps.append(app)
        gone = [u for u, trashed in known.items() if not trashed and u not in live]
        with db:
            db.executemany('INSERT OR REPLACE INTO assets(uuid,filename,z_pk,width,height,added_at,hidden,trashed,stable_hash,'
                           'orig_size,src_kind,src_path,in_scope) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?)', new)
            db.executemany('UPDATE assets SET trashed=1 WHERE uuid=?', [(u,) for u in gone])
            db.executemany('UPDATE assets SET trashed=0 WHERE uuid=?', [(u,) for u in back])
            db.executemany('UPDATE assets SET src_kind=?,src_path=? WHERE uuid=?', retry_paths)
            if source is not None:
                from web.library import ensure_source_schema, link_photos
                ensure_source_schema(db)
                link_photos(db, source, [(r[0], app) for r, app in zip(new, new_apps)])
    finally:
        db.close()
    return {'new': len(new), 'returned': len(back), 'gone': len(gone), 'sources': sources}


def backlog(pi_db, library_uuids):
    """(to recognise, recognised but not yet in the library, in the library but no longer in Photos)."""
    db = sqlite3.connect(f'file:{pi_db}?mode=ro', uri=True, timeout=30)
    try:
        live = {u: ready for u, ready in db.execute("""SELECT uuid,EXISTS(SELECT 1 FROM labels l WHERE l.uuid=a.uuid AND l.dim='rating')
            FROM assets a WHERE in_scope=1 AND coalesce(trashed,0)=0""")}
    finally:
        db.close()
    todo = sorted(u for u, ready in live.items() if not ready)
    ready = sorted(u for u, ok in live.items() if ok and u not in library_uuids)
    removed = sorted(u for u in library_uuids if u not in live)
    return todo, ready, removed


def recognise(pi_db, uuids):
    """Run web.infer in a child process; yield its progress messages."""
    with tempfile.NamedTemporaryFile('w', suffix='.txt', delete=False) as f:
        f.write('\n'.join(uuids))
    try:
        proc = subprocess.Popen([sys.executable, '-m', 'web.infer', str(pi_db), f.name], cwd=Path(__file__).resolve().parents[3],
                                stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, env={**os.environ, 'PYTHONUNBUFFERED': '1'})
        for line in proc.stdout:
            if line.startswith('{'):
                yield json.loads(line)
        if proc.wait():
            raise RuntimeError('识别进程异常退出：' + proc.stderr.read()[-300:].strip())
    finally:
        os.unlink(f.name)


class Syncer:
    """One sync at a time, in a background thread; `status()` is what the settings page shows."""
    def __init__(self, store, library=PHOTOS, recogniser=recognise, local_limit=LOCAL_LIMIT, per_run=0, on_import=None, source=None,
                 scanner=None):
        self.store, self.library, self.recognise, self.local_limit = store, Path(library), recogniser, local_limit
        self.source = source   # the library.json photos source: new images get their asset link and preset
        self.scanner = scanner   # another source's scan (web.folder), same counts as `scan`; default: Photos
        self.on_import = on_import   # e.g. let the learner claim and regroup the new images
        self.per_run = per_run   # recognise at most this many per run (0 = all); for trial runs
        self.lock = threading.Lock()
        self.state = {'status': 'idle'}
        self.thread = None
        self.blocked = None          # e.g. the character index is being rewritten (tests/reference/web/index_update.py)

    def status(self):
        with self.lock:
            return dict(self.state)

    def update(self, **fields):
        with self.lock:
            self.state.update(fields)

    def start(self):
        with self.lock:
            if self.thread and self.thread.is_alive() or (self.blocked and self.blocked()):
                return False
            self.state = {'status': 'scanning', 'started_at': time.strftime('%Y-%m-%d %H:%M:%S')}
            self.thread = threading.Thread(target=self.run, name='photos-sync', daemon=True)
            self.thread.start()
            return True

    def run(self):
        run_id = self.store.sync_begin()
        counts = {'found': 0, 'removed': 0, 'imported': 0, 'failed': 0}
        try:
            found = self.scanner() if self.scanner else scan(self.library, self.store.source, self.source)
            counts['found'] = found['new'] + found['returned']
            self.store.sync_update(run_id, found=counts['found'], sources=json.dumps(found['sources'], ensure_ascii=False))
            self.store.refresh_paths()
            todo, ready, removed = backlog(self.store.source, self.store.library_uuids())
            if removed or ready:
                self.update(status='importing')
                self.store.import_changes(ready, removed, run_id=run_id)
                counts.update(removed=len(removed), imported=len(ready))
            self.update(**counts, total=len(todo), done=0)
            if len(todo) > self.local_limit:
                message = f'有 {len(todo)} 张新图，超过本机处理上限 {self.local_limit}，请交给 Windows 识别后再同步'
                return self.finish(run_id, 'needs_windows', counts, message)
            if self.per_run:
                todo = todo[:self.per_run]
                self.update(total=len(todo))
            if todo:
                self.update(status='recognising')
                for step in self.recognise(self.store.source, todo):
                    if 'batch' not in step:
                        continue
                    if step['batch']:
                        self.store.import_changes(step['batch'], run_id=run_id)
                    counts['imported'] += len(step['batch'])
                    counts['failed'] += len(step['failed'])
                    self.update(**counts, done=step['done'], ms_per_image=step.get('ms_per_image'),
                                last_error=step['failed'][-1]['error'] if step['failed'] else self.state.get('last_error'))
                    self.store.sync_update(run_id, imported=counts['imported'], failed=counts['failed'])
            message = f"{counts['failed']} 张暂时读不到（可能还在 iCloud），下次同步重试" if counts['failed'] else ''
            self.finish(run_id, 'done', counts, message)
        except Exception as exc:
            self.finish(run_id, 'error', counts, f'{type(exc).__name__}: {exc}'[:300])

    def finish(self, run_id, status, counts, message):
        self.store.sync_update(run_id, status=status, message=message, finished=True, **counts)
        if self.on_import and (counts.get('imported') or counts.get('removed')):
            self.on_import()
        self.update(status=status, message=message, finished_at=time.strftime('%Y-%m-%d %H:%M:%S'), **counts)
