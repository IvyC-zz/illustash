"""IQDB (docs/PROVENANCE.md section 9, 2026-09-29): Danbooru's perceptual search for pictures no step has an artist for.

The Rust module crates/iqdb computes each picture's IQDB signature locally and sends only its `iqdb_…` text; this file
chooses the pictures and records what comes back, like pixel_hash does. Measured on pictures without an artist: X 42%,
Pixiv 10% (mostly R18 Danbooru never got); every answer >= 80 checked against a known artist was right (50/50).

Order: four pictures prioritised by source/rating yield, then one from the original seed/save-run queue. This brings
direct matches forward without starving lower-yield pictures. Manual selections keep their existing order.
A run asks at most PER_RUN pictures. Misses are asked again after RETRY_DAYS (Danbooru keeps growing).
"""
import datetime
import json
import os
import shutil
import sqlite3
import subprocess
import threading
from pathlib import Path

from provenance import assets, db, evidence
from provenance.sources.pixel_hash import links_of, source_post
from provenance.net import UA

ROOT = Path(__file__).resolve().parents[2]
CRATE = ROOT / 'crates' / 'iqdb'
BINARY = CRATE / 'target' / 'release' / 'illustash-iqdb'
HOST = 'danbooru.donmai.us'
BACKEND = 'iqdb'
PER_RUN = 10000   # background lookup cap; priority changes neither this cap nor the request pace
PER_SAVE_RUN = 3
SAVE_GAP = 300          # seconds between saves that still belong to one run of saves
RETRY_DAYS = 90
HIT = 80.0
PACE, WORKERS = 0.5, 2   # at most 2 requests a second, two in flight (2026-09-29, the user's choice)

# Frozen ordering from 3,879 earlier lookups, checked on disjoint later pictures (2026-09-29).
# Scheduling hints only: ratings never change the match threshold or an image's attribution.
# Unknown ratings keep their relative order at a neutral priority and share the 20% original-order quota.
PRIORITY = {key: rank for rank, key in enumerate((
    ('x', 'questionable'), ('x', 'sensitive'), ('pixiv', 'general'), ('x', 'explicit'),
    ('pixiv', 'sensitive'), ('x', 'general'), ('pixiv', 'questionable'), ('pixiv', 'explicit'),
))}


def _ratings(library):
    """Use the core's existing ratings, read-only; an uninitialised library keeps the old order."""
    try:
        c = sqlite3.connect(Path(library.db).resolve().as_uri() + '?mode=ro', uri=True, timeout=30)
        try:
            return dict(c.execute('SELECT uuid,rating FROM asset_rating'))
        finally:
            c.close()
    except sqlite3.OperationalError:
        return {}


def _prioritize(pictures, ratings):
    """A permutation of the original queue: four yield-prioritised slots, then one original-order slot."""
    ranked = iter(sorted(pictures, key=lambda p: PRIORITY.get((p['preset'], ratings.get(p['asset'])), 5.5)))
    original, seen, out = iter(pictures), set(), []
    for index in range(len(pictures)):
        queue = original if index % 5 == 4 else ranked
        picture = next(p for p in queue if p['asset'] not in seen)
        seen.add(picture['asset'])
        out.append(picture)
    return out


def binary():
    """The Rust module, built on first use (or after its sources changed) with the project's cargo."""
    configured = os.environ.get('ILLUSTASH_IQDB_BIN')
    if configured:
        installed = Path(configured)
        if not installed.is_absolute() or not installed.is_file():
            raise RuntimeError('可选 IQDB 程序不可用：ILLUSTASH_IQDB_BIN 必须指向已安装的可执行文件')
        return installed
    sources = [p for p in (CRATE / 'src').glob('*.rs')] + [CRATE / 'Cargo.toml']
    if not BINARY.exists() or max(p.stat().st_mtime for p in sources) > BINARY.stat().st_mtime:
        # An app opened from Finder has no ~/.cargo/bin on its PATH.
        cargo = shutil.which('cargo') or str(Path.home() / '.cargo' / 'bin' / 'cargo')
        subprocess.run([cargo, 'build', '--release', '--quiet'], cwd=CRATE, check=True, capture_output=True)
    return BINARY


def _save_runs(pictures):
    """Pictures in runs of saves no more than SAVE_GAP apart, oldest first."""
    runs, last = [], None
    for p in sorted((p for p in pictures if p.get('added_at')), key=lambda p: p['added_at']):
        t = datetime.datetime.fromisoformat(p['added_at']).timestamp()
        if last is None or t - last > SAVE_GAP:
            runs.append([])
        runs[-1].append(p)
        last = t
    return runs


def order(library, c, only=None):
    """Pictures to ask about, yield-prioritised with a 20% original-queue quota. With `only` (按需查找),
    just those pictures still without an artist, asked again even when a miss is not due yet: the person asked."""
    done = {r[0] for r in c.execute("SELECT DISTINCT asset FROM image_artists WHERE status IN ('auto','confirmed')")}
    if only is not None:
        only = set(only)
        return [p for p in assets.pictures(library, ('x', 'pixiv', 'other')) if p['asset'] in only and p['asset'] not in done]
    todo = [p for p in assets.pictures(library, ('x', 'pixiv')) if p['asset'] not in done and evidence.due(c, p['asset'], BACKEND)]
    with assets.open_source(library) as s:
        added = dict(s.execute('SELECT uuid, added_at FROM assets'))
    for p in todo:
        p['added_at'] = added.get(p['asset'])
    from provenance.sources.groups import unsolved_originals
    originals = set(unsolved_originals(library, c))   # an original character is one artist's: one answer names it
    lead = [p for p in todo if p['asset'] in originals]
    todo = [p for p in todo if p['asset'] not in originals]
    pixiv = [p for p in todo if p['preset'] == 'pixiv']
    first = []
    for run in _save_runs(pixiv):
        step = max(1, len(run) // PER_SAVE_RUN)
        first += run[::step][:PER_SAVE_RUN]
    chosen = {p['asset'] for p in first}
    x = [p for p in todo if p['preset'] == 'x']
    mixed = [p for pair in zip(first, x) for p in pair] + first[len(x):] + x[len(first):]   # both kinds early in every run
    original = lead + mixed + [p for p in pixiv if p['asset'] not in chosen]
    return _prioritize(original, _ratings(library))


def _record(c, asset, score, post):
    links = links_of(post)
    artist = db.artist_for(c, links, BACKEND) if links else None
    proof = {'danbooru_post': post['id'], 'score': round(score, 1)}
    c.execute("""INSERT OR REPLACE INTO image_sources(asset,site,post_id,url,original,artist_id,method,similarity)
                 VALUES (?, 'danbooru', ?, ?, ?, ?, 'iqdb', ?)""",
              (asset, str(post['id']), f"https://danbooru.donmai.us/posts/{post['id']}", post.get('source') or '', artist, round(score, 1)))
    if (up := source_post(post)):          # the Pixiv work also anchors local verification around it
        c.execute("INSERT OR REPLACE INTO image_sources(asset,site,post_id,url,artist_id,method,similarity) VALUES (?,?,?,?,?, 'iqdb', ?)",
                  (asset, *up, artist, round(score, 1)))
    if artist:
        c.execute("""INSERT INTO image_artists(asset,artist_id,method,score,evidence,status) VALUES (?,?, 'iqdb', ?, ?, 'auto')
                     ON CONFLICT(asset,artist_id) DO NOTHING""", (asset, artist, score / 100, json.dumps(proof)))
        c.execute("INSERT OR IGNORE INTO seeds(artist_id,source,evidence) VALUES (?, 'iqdb', ?)", (artist, json.dumps({'asset': asset, **proof})))
    return artist


def _looked(c, asset, result):
    delay = f'+{RETRY_DAYS} days' if result == 'miss' else '+7 days' if result == 'unreadable' else '+1 hour'
    c.execute("""INSERT INTO lookups(target,backend,attempts,last_at,last_result,next_due_at) VALUES (?,?,1,datetime('now'),?,datetime('now',?))
                 ON CONFLICT(target,backend) DO UPDATE SET attempts=attempts+1,last_at=excluded.last_at,last_result=excluded.last_result,
                 next_due_at=excluded.next_due_at""", (asset, BACKEND, 'miss' if result == 'unreadable' else result, delay))


def run(library, net, limit=None, log=print, run_binary=None, stop=None, only=None):
    """`stop` (a threading.Event) ends it early when it runs beside other steps (tests/reference/provenance/run.py)."""
    counts = {'asked': 0, 'hits': 0, 'artists': 0, 'failed': 0, 'stopped': ''}
    if not getattr(net, 'authorized', False):
        return {**counts, 'stopped': '外部请求未授权'}
    if HOST in net.stopped:
        return {**counts, 'stopped': f'{HOST}: {net.stopped[HOST]}'}
    c = db.connect(library)
    report = getattr(log, 'progress', None) or (lambda **numbers: None)
    try:
        todo = order(library, c, only)[:PER_RUN if limit is None else limit]
        if not todo:
            return counts
        log(f'IQDB：{len(todo)} 张待查（只发签名文本）')
        paths = [(p['asset'], assets.original(library, p) or p['src_path']) for p in todo]
        lines = ''.join(json.dumps({'asset': a, 'path': str(path)}) + '\n' for a, path in paths if path)
        budget = max(0, net.budget - net.used)
        try:
            program = str(run_binary or binary())
        except (OSError, subprocess.CalledProcessError) as exc:
            return {**counts, 'stopped': f'IQDB 模块编译失败：{exc}'}
        command = [program, '--pace', str(PACE), '--workers', str(WORKERS), '--hit', str(HIT), '--budget-bytes', str(budget), '--user-agent', UA]
        proc = subprocess.Popen(command, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True,
                                env={**os.environ})

        def feed():   # in its own thread: both pipes would fill up and wait on each other otherwise
            try:
                proc.stdin.write(lines)
                proc.stdin.close()
            except (BrokenPipeError, OSError, ValueError):
                pass
        threading.Thread(target=feed, daemon=True).start()
        new_artists = set()
        for line in proc.stdout:
            if stop is not None and stop.is_set():
                counts['stopped'] = '已手动停止'
                break
            try:
                e = json.loads(line)
            except ValueError:
                continue
            if 'stopped' in e:
                net.stopped[HOST] = e['stopped'].split(': ', 1)[-1]
                counts['stopped'] = e['stopped']
                break
            if 'budget' in e:
                counts['stopped'] = f'已用 {net.used / 2 ** 20:.0f} MB，达到本次上限'
                break
            with net.lock:
                net.used += e.get('bytes', 0)
            with c:
                if e['status'] == 'hit' and e.get('post'):
                    artist = _record(c, e['asset'], e['score'], e['post'])
                    counts['hits'] += 1
                    if artist and artist not in new_artists:
                        new_artists.add(artist)
                    _looked(c, e['asset'], 'hit')
                elif e['status'] in ('miss', 'unreadable'):
                    _looked(c, e['asset'], e['status'])
                else:
                    counts['failed'] += 1   # network trouble: not recorded, asked again next run
            counts['asked'] += 1
            if counts['asked'] % 50 == 0 or counts['asked'] == len(todo):
                log(f"IQDB：已查 {counts['asked']}/{len(todo)}，命中 {counts['hits']}，新画师线索 {len(new_artists)}，流量 {net.used / 2 ** 20:.0f} MB")
                report(label='图片', done=counts['asked'], total=len(todo), found=counts['hits'], mb=round(net.used / 2 ** 20, 1))
        counts['artists'] = len(new_artists)
    finally:
        if 'proc' in locals() and proc.poll() is None:   # stopped early (refusal, budget, interruption)
            proc.kill()
        if 'proc' in locals():
            proc.wait(timeout=30)
            proc.stdout.close()
        c.close()
    return counts
