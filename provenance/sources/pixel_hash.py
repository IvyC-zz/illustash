"""Danbooru pixel hash (docs/PROVENANCE.md section 3.11): decode the original, hash the normalised pixels, ask Danbooru.

Only the hash text leaves the machine. A hit is the same pixels, so it is recorded as the post and the artist directly.
The hash reproduces Danbooru's MediaFile::Image#pixel_hash: sRGB, an alpha band, raw PAM, MD5.
"""
import hashlib
import io
import json
import re

from PIL import Image, ImageCms

from provenance import assets, db
from provenance.net import BudgetExceeded, Stopped

SRGB = ImageCms.createProfile('sRGB')
SKIP_ARTISTS = {'banned_artist', 'unknown_artist', 'anonymous_artist', 'artist_request'}


def pixel_hash(file):
    im = Image.open(io.BytesIO(file) if isinstance(file, bytes) else file)
    icc = im.info.get('icc_profile')
    if im.mode == 'P':
        im = im.convert('RGBA' if 'transparency' in im.info else 'RGB')
    elif im.mode in ('L', 'LA', 'I', 'I;16', 'F', 'CMYK', '1'):
        im = im.convert('RGBA' if im.mode == 'LA' else 'RGB')
    if icc:
        try:
            alpha = im.getchannel('A') if im.mode == 'RGBA' else None
            im = ImageCms.profileToProfile(im.convert('RGB'), ImageCms.ImageCmsProfile(io.BytesIO(icc)), SRGB, outputMode='RGB')
            if alpha:
                im.putalpha(alpha)
        except (OSError, ValueError, ImageCms.PyCMSError):
            pass
    if im.mode != 'RGBA':
        im = im.convert('RGBA')
    header = f'P7\nWIDTH {im.width}\nHEIGHT {im.height}\nDEPTH 4\nMAXVAL 255\nTUPLTYPE RGB_ALPHA\nENDHDR\n'.encode()
    return hashlib.md5(header + im.tobytes()).hexdigest()


def links_of(post):
    """Artist links a Danbooru post gives: its artist tags, and the account in its declared source."""
    out = [('danbooru', a, f'https://danbooru.donmai.us/artists/show_or_new?name={a}')
           for a in (post.get('tag_string_artist') or '').split() if a not in SKIP_ARTISTS]
    src = post.get('source') or ''
    m = re.match(r'https?://(?:www\.|mobile\.)?(?:twitter|x)\.com/([A-Za-z0-9_]{1,15})/status/\d+', src)
    if m and m.group(1).lower() not in ('i', 'intent'):
        out.append(('x', m.group(1).lower(), f'https://x.com/{m.group(1)}'))
    return out


def source_post(post):
    """The upstream post Danbooru declares, as (site, post_id, url), when it is a Pixiv or X post."""
    src = post.get('source') or ''
    if (m := re.search(r'pixiv\.net/(?:en/)?artworks/(\d+)|illust_id=(\d+)|/img/[\d/]+/(\d+)_p\d+', src)):
        pid = next(g for g in m.groups() if g)
        return 'pixiv', pid, f'https://www.pixiv.net/artworks/{pid}'
    if (m := re.search(r'(?:twitter|x)\.com/[^/]+/status/(\d+)', src)):
        return 'x', m.group(1), src
    return None


BATCH = 50    # hashes per Danbooru request (search[pixel_hash][]=…, repeated); 100 makes a ~6 KB URL that Cloudflare refuses (520)


def _hash(item):
    asset, path = item
    try:
        return asset, pixel_hash(path)
    except (OSError, ValueError, TypeError, SyntaxError):
        return asset, None


def _hashes(items, workers):
    """(asset, hash or None), decoding the originals on several cores: decoding, not the network, is the slow part."""
    if workers <= 1:
        yield from map(_hash, items)
        return
    from concurrent.futures import ProcessPoolExecutor
    pool = ProcessPoolExecutor(workers)
    try:
        yield from pool.map(_hash, items, chunksize=8)
    finally:   # stopping early (a refusal, the budget, the app's 停止) must not wait for every queued picture
        pool.shutdown(wait=False, cancel_futures=True)


def _record(c, asset, posts):
    c.execute("""INSERT INTO lookups(target,backend,attempts,last_at,last_result) VALUES (?, 'pixel_hash', 1, datetime('now'), ?)
                 ON CONFLICT(target,backend) DO UPDATE SET attempts=attempts+1,last_at=excluded.last_at,last_result=excluded.last_result""",
              (asset, 'hit' if posts else 'miss'))
    for post in posts:
        links = links_of(post)
        artist = db.artist_for(c, links, 'danbooru_post') if links else None
        up = source_post(post)
        c.execute("""INSERT OR REPLACE INTO image_sources(asset,site,post_id,url,original,artist_id,method)
                     VALUES (?, 'danbooru', ?, ?, ?, ?, 'pixel_hash')""",
                  (asset, str(post['id']), f"https://danbooru.donmai.us/posts/{post['id']}", post.get('source') or '', artist))
        if up:
            c.execute("INSERT OR REPLACE INTO image_sources(asset,site,post_id,url,artist_id,method) VALUES (?,?,?,?,?, 'pixel_hash')",
                      (asset, *up, artist))
        if artist:
            c.execute("""INSERT OR REPLACE INTO image_artists(asset,artist_id,method,score,evidence,status)
                         VALUES (?,?, 'pixel_hash', 1, ?, 'auto')""", (asset, artist, json.dumps({'danbooru_post': post['id']})))
            c.execute("INSERT OR IGNORE INTO seeds(artist_id,source,evidence) VALUES (?, 'pixel_hash', ?)",
                      (artist, json.dumps({'asset': asset, 'danbooru_post': post['id']})))


def run(library, net, limit=None, log=print, workers=None, only=None):
    """Look up every not-yet-looked-up X/Pixiv picture, BATCH hashes per request. Stops cleanly on budget or refusal.
    `only`: just these pictures (按需查找 from the library view)."""
    only = set(only) if only is not None else None
    import os
    counts = {'looked': 0, 'hits': 0, 'misses': 0, 'unreadable': 0, 'failed': 0, 'stopped': ''}
    workers = workers if workers is not None else max(1, (os.cpu_count() or 2) - 1)
    c = db.connect(library)
    try:
        done = {r['target'] for r in c.execute("SELECT target FROM lookups WHERE backend='pixel_hash' AND last_result IN ('hit','miss')")}
        todo = [p for p in assets.pictures(library) if p['asset'] not in done and (only is None or p['asset'] in only)][:limit]
        items = [(p['asset'], assets.original(library, p)) for p in todo]
        batch = []

        def flush():
            by_hash = {}
            for asset, digest in batch:
                by_hash.setdefault(digest, []).append(asset)
            found = net.get('https://danbooru.donmai.us/media_assets.json',
                            [('search[pixel_hash][]', h) for h in by_hash] + [('only', 'id,pixel_hash,post[id,tag_string_artist,source]'), ('limit', '1000')])
            posts = {}
            for a in found or []:
                if a.get('post'):
                    posts.setdefault(a.get('pixel_hash'), []).append(a['post'])
            with c:
                for digest, owners in by_hash.items():
                    for asset in owners:
                        _record(c, asset, posts.get(digest, []))
                        counts['looked'] += 1
                        counts['hits' if posts.get(digest) else 'misses'] += 1
            batch.clear()

        for asset, digest in _hashes(items, workers):
            if digest is None:
                counts['unreadable'] += 1
                continue
            batch.append((asset, digest))
            if len(batch) >= BATCH:
                try:
                    flush()
                except (Stopped, BudgetExceeded) as exc:
                    counts['stopped'] = str(exc)
                    break
                except ConnectionError as exc:
                    counts['failed'] += len(batch)   # not recorded: asked again next run
                    log(f'像素哈希：一批 {len(batch)} 张没查成（{exc}），下次再查')
                    batch.clear()
                if counts['looked'] % 1000 < BATCH:
                    log(f"像素哈希：已查 {counts['looked']}/{len(items)}，命中 {counts['hits']}")
                    if (report := getattr(log, 'progress', None)):
                        report(label='图片', done=counts['looked'], total=len(items), found=counts['hits'], mb=round(net.used / 2 ** 20, 1))
        if batch and not counts['stopped']:
            try:
                flush()
            except (Stopped, BudgetExceeded) as exc:
                counts['stopped'] = str(exc)
            except ConnectionError:
                counts['failed'] += len(batch)
    finally:
        c.close()
    return counts
