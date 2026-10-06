"""Who drew a picture, read from the library package's provenance.sqlite (written only by provenance/, read here).

The core never writes it and its attributions stay untouched: artists are shown beside the characters, and the
library can be filtered to one artist's pictures. No provenance.sqlite (not run yet, or not a package): nothing shown.
Human verdicts (是 / 不是 TA) live in the core's own artist_verdicts (web/store.py) and are applied here on reading:
`verdicts` {artist: 'yes'|'no'} for one picture, `rejected` {artist: {uuid: verdict}} for the whole library.

Once the library's artists live in the terms core (meta terms_artists=core, docs/TERMS_ARTISTS.md), who drew what, the
counts, the filter and the audit pool come from the core's projection in library.sqlite, and links and nicknames from
its term_links / term_names; posts (image_sources) are still provenance's. The legacy reads stay for libraries the
core has not taken over (no core program available).
"""
import hashlib
import json
import sqlite3
from pathlib import Path

from web import terms_core

SITES = {'x': 'X', 'pixiv': 'Pixiv', 'danbooru': 'Danbooru'}
# The artist judge's strong methods (native/terms/src/schema.rs, Kind::Artist): the ones provenance writes as 'auto'.
STRONG = ('pixel_hash', 'gallery_match', 'iqdb', 'same_picture')


def _open(library):
    path = Path(library.path) / 'provenance.sqlite' if library else None
    if not path or not path.exists():
        return None
    c = sqlite3.connect(path.resolve().as_uri() + '?mode=ro', uri=True, timeout=10)
    c.row_factory = sqlite3.Row
    return c


def _canonical(c, artist):
    for _ in range(10):   # merged artists point at the one kept
        r = c.execute('SELECT merged_into FROM artists WHERE id=?', (artist,)).fetchone()
        if not r or not r['merged_into']:
            return artist
        artist = r['merged_into']
    return artist


def _describe(c, artist):
    links = {}
    for r in c.execute('SELECT system,key,url FROM artist_links WHERE artist_id=? ORDER BY system,key', (artist,)):
        links.setdefault(r['system'], (r['key'], r['url']))
    nickname = None
    if c.execute("SELECT 1 FROM sqlite_master WHERE name='artist_names'").fetchone():
        nickname = c.execute('SELECT name FROM artist_names WHERE artist_id=? ORDER BY updated_at DESC LIMIT 1', (artist,)).fetchone()
    return _card(artist, links, nickname)


def _card(artist, links, nickname):
    """Name and homepages from {system: (key, url)} and a nickname row (or None); the same rule for both sources."""
    homes = []
    if 'x' in links:
        homes.append({'site': 'X', 'label': 'X', 'url': f"https://x.com/{links['x'][0]}"})
    elif 'x_id' in links:
        homes.append({'site': 'X', 'label': 'X', 'url': f"https://x.com/i/user/{links['x_id'][0]}"})
    if 'pixiv' in links:
        homes.append({'site': 'Pixiv', 'label': 'Pixiv', 'url': f"https://www.pixiv.net/users/{links['pixiv'][0]}"})
    if 'danbooru' in links:
        homes.append({'site': 'Danbooru', 'label': 'Danbooru', 'url': f"https://danbooru.donmai.us/artists/show_or_new?name={links['danbooru'][0]}"})
    if nickname:
        name = nickname[0]
    elif 'name' in links:
        name = links['name'][0]
    elif 'x' in links:
        name = '@' + links['x'][0]
    elif 'danbooru' in links:
        name = links['danbooru'][0].replace('_', ' ')
    elif 'pixiv' in links:
        name = f"Pixiv {links['pixiv'][0]}"
    else:
        name = '未知画师'
    return {'id': artist, 'name': name, 'homes': homes}


def _core(library):
    """A read-only connection to library.sqlite when the terms core owns this library's artists; None otherwise."""
    path = Path(library.path) / 'library.sqlite' if library else None
    if not path or not path.exists():
        return None
    c = sqlite3.connect(path.resolve().as_uri() + '?mode=ro', uri=True, timeout=10)
    c.row_factory = sqlite3.Row
    if terms_core.artists_in_core(c):
        return c
    c.close()
    return None


def _core_describe(lc, artist):
    links = {}
    for r in lc.execute("""SELECT l.system,l.key,l.url FROM term_links l JOIN terms t ON t.id=l.term_id
            WHERE t.key=? AND l.system<>'provenance' ORDER BY l.system,l.key""", (artist,)):
        links.setdefault(r['system'], (r['key'], r['url']))
    nickname = lc.execute("""SELECT n.value FROM term_names n JOIN terms t ON t.id=n.term_id WHERE t.key=? AND n.role='display'
            ORDER BY n.preferred DESC,n.position,n.value LIMIT 1""", (artist,)).fetchone()
    return _card(artist, links, nickname)


def _core_assets(lc, artist):
    """Pictures the core shows for this artist (机器认定 or 人工确认), still in the library."""
    return [r[0] for r in lc.execute("""SELECT a.key FROM term_projection p
            JOIN terms t ON t.id=p.term_id AND t.key=? AND t.kind='artist'
            JOIN term_assets a ON a.id=p.asset_id AND a.removed_at IS NULL
            WHERE p.state IN ('machine','confirmed')""", (artist,))]


def _core_for_asset(lc, pc, asset, present, keep_rejected):
    states = ('machine', 'confirmed', 'rejected') if keep_rejected else ('machine', 'confirmed')
    rows = lc.execute(f"""SELECT t.key,p.state,p.term_id,p.asset_id FROM term_projection p
            JOIN term_assets a ON a.id=p.asset_id AND a.namespace='library' AND a.key=?
            JOIN terms t ON t.id=p.term_id AND t.kind='artist'
            WHERE p.state IN ({','.join('?' * len(states))}) ORDER BY t.key""", (asset, *states)).fetchall()
    artists = []
    for r in rows:
        info = _core_describe(lc, r['key'])
        mine = set(_core_assets(lc, r['key']))
        info['count'] = len(mine & present) if present is not None else len(mine)
        info['confirmed'] = r['state'] == 'confirmed'
        info['rejected'] = r['state'] == 'rejected'
        # Every method of the evidence in effect (provenance's latest snapshot), as the old reading listed them.
        info['methods'] = sorted({x[0] for x in lc.execute(
            "SELECT DISTINCT method FROM term_current_evidence WHERE asset_id=? AND term_id=?", (r['asset_id'], r['term_id']))})
        artists.append(info)
    rejected = {r[0] for r in lc.execute("""SELECT t.key FROM term_projection p
            JOIN term_assets a ON a.id=p.asset_id AND a.namespace='library' AND a.key=?
            JOIN terms t ON t.id=p.term_id AND t.kind='artist' WHERE p.state='rejected'""", (asset,))}
    # 「也许是 TA」: candidates nobody has judged, with the reason of their best current evidence.
    maybe = []
    for r in lc.execute("""SELECT t.key,e.method,e.data,e.score FROM term_projection p
            JOIN term_assets a ON a.id=p.asset_id AND a.namespace='library' AND a.key=?
            JOIN terms t ON t.id=p.term_id AND t.kind='artist'
            JOIN term_current_evidence e ON e.asset_id=p.asset_id AND e.term_id=p.term_id
            WHERE p.state='maybe' ORDER BY coalesce(e.score,0) DESC,t.key""", (asset,)):
        if r['key'] not in {m['id'] for m in maybe}:
            maybe.append({**_core_describe(lc, r['key']), 'reason': _reason(r['method'], r['data'])})
    return {'artists': artists, 'posts': _posts(pc, asset, lambda artist: artist in rejected) if pc else [], 'maybe': maybe}


def _posts(c, asset, rejected):
    """One post per site: the original first, then its mirrors. `rejected(artist_id)`: judged 不是 on this picture."""
    posts, seen = [], set()
    for r in c.execute("SELECT site,url,artist_id,method FROM image_sources WHERE asset=? ORDER BY CASE site WHEN 'pixiv' THEN 0 WHEN 'x' THEN 1 ELSE 2 END, post_id", (asset,)):
        # A post matched from a rejected artist's works goes with the artist; a pixel-hash post is the same file whatever its tag says.
        if r['method'] != 'pixel_hash' and r['artist_id'] and rejected(r['artist_id']):
            continue
        if r['site'] not in seen:
            seen.add(r['site'])
            posts.append({'site': SITES.get(r['site'], r['site']), 'url': r['url']})
    return posts


def _no(verdicts, artist):
    return {u for u, v in (verdicts or {}).get(artist, {}).items() if v == 'no'}


def for_asset(library, asset, present=None, verdicts=None, rejected=None, keep_rejected=False):
    """Artists of one picture with their homepages and how many library pictures they have; the posts it came from.
    `present`: the library's asset ids, to count only pictures still in it. An artist judged 不是 is left out
    (kept, with rejected=True, for the acceptance check); one judged 是 carries confirmed=True.
    `methods`: how the machine found the artist ('pixel_hash', 'gallery_match', …)."""
    lc = _core(library)
    if lc is not None:
        pc = _open(library)
        try:
            return _core_for_asset(lc, pc, asset, present, keep_rejected)
        finally:
            lc.close()
            if pc is not None:
                pc.close()
    c = _open(library)
    if c is None:
        return {'artists': [], 'posts': []}
    try:
        return _for_asset(c, asset, present, verdicts, rejected, keep_rejected)
    finally:
        c.close()


def _for_asset(c, asset, present, verdicts, rejected, keep_rejected):
    verdicts = {_canonical(c, a): v for a, v in (verdicts or {}).items()}
    artists, seen = [], set()
    maybe = []
    for r in c.execute("""SELECT artist_id, status, method, evidence FROM image_artists WHERE asset=? AND status IN ('auto','candidate')
                          ORDER BY status='candidate', score DESC""", (asset,)):
        artist = _canonical(c, r['artist_id'])
        if artist in seen or (verdicts.get(artist) == 'no' and not keep_rejected):
            continue
        if r['status'] == 'candidate' and verdicts.get(artist) != 'yes':   # not settled: 「也许是 TA」 with the reason
            if verdicts.get(artist) != 'no' and artist not in {m['id'] for m in maybe}:
                maybe.append({**_describe(c, artist), 'reason': _reason(r['method'], r['evidence'])})
            continue
        seen.add(artist)
        info = _describe(c, artist)
        mine = set(_assets(c, artist, _yes(rejected, artist))) - _no(rejected, artist)
        info['count'] = len(mine & present) if present is not None else len(mine)
        info['confirmed'] = verdicts.get(artist) == 'yes'
        info['rejected'] = verdicts.get(artist) == 'no'
        ids = [artist] + [x['id'] for x in c.execute('SELECT id FROM artists WHERE merged_into=?', (artist,))]
        info['methods'] = sorted({x['method'] for x in c.execute(
            f"SELECT method FROM image_artists WHERE asset=? AND status IN ('auto','candidate') AND artist_id IN ({','.join('?' * len(ids))})", [asset, *ids])})
        artists.append(info)
    # (main 的写法在这里按站点集合过滤 maybe，站点名不会是画师 id，实际不过滤；候选在上面已按画师去重。)
    return {'artists': artists, 'posts': _posts(c, asset, lambda artist: verdicts.get(_canonical(c, artist)) == 'no'), 'maybe': maybe}


AUDIT_SALT = 'artist-audit-v1'


def audit(library, present, verdicts, n=50):
    """The acceptance sample (docs/PROVENANCE.md section 8): pictures with a machine-found artist, in a fixed random
    order (a hash of the id), the first `n` still in the library. The same pictures every time, whatever has been
    answered, so 是 / 不是 on them measure the machine's accuracy; answering never changes which pictures are drawn.
    `verdicts`: {artist: {uuid: verdict}} (Store.artist_verdicts())."""
    lc = _core(library)
    if lc is not None:
        pc = _open(library)
        try:
            # The machine's own finds: pictures with strong evidence for some artist, whatever people answered since.
            pool = [r[0] for r in lc.execute(f"""SELECT DISTINCT a.key FROM term_current_evidence e
                    JOIN term_assets a ON a.id=e.asset_id AND a.removed_at IS NULL JOIN terms t ON t.id=e.term_id AND t.kind='artist'
                    WHERE e.method IN ({','.join('?' * len(STRONG))})""", STRONG) if r[0] in present]
            pool.sort(key=lambda a: hashlib.sha1(f'{AUDIT_SALT}:{a}'.encode()).hexdigest())
            return {'items': [{'uuid': a, **_core_for_asset(lc, pc, a, present, True)} for a in pool[:n]], 'pool': len(pool)}
        finally:
            lc.close()
            if pc is not None:
                pc.close()
    c = _open(library)
    if c is None:
        return {'items': [], 'pool': 0}
    try:
        pool = [r[0] for r in c.execute("SELECT DISTINCT asset FROM image_artists WHERE status='auto'") if r[0] in present]
        pool.sort(key=lambda a: hashlib.sha1(f'{AUDIT_SALT}:{a}'.encode()).hexdigest())
        items = []
        for asset in pool[:n]:
            mine = {a: v[asset] for a, v in verdicts.items() if asset in v}
            items.append({'uuid': asset, **_for_asset(c, asset, present, mine, verdicts, keep_rejected=True)})
        return {'items': items, 'pool': len(pool)}
    finally:
        c.close()


def _assets(c, artist, yes=()):
    """Pictures attributed to an artist: the machine's, and candidates the person confirmed (`yes`: their uuids)."""
    ids = [artist] + [r['id'] for r in c.execute('SELECT id FROM artists WHERE merged_into=?', (artist,))]
    q = ','.join('?' * len(ids))
    out = [r['asset'] for r in c.execute(f"SELECT DISTINCT asset FROM image_artists WHERE status='auto' AND artist_id IN ({q})", ids)]
    if yes:
        mine = set(out)
        out += [r['asset'] for r in c.execute(f"SELECT DISTINCT asset FROM image_artists WHERE status='candidate' AND artist_id IN ({q})", ids)
                if r['asset'] in yes and r['asset'] not in mine]
    return out


def _yes(verdicts, artist):
    return {u for u, v in (verdicts or {}).get(artist, {}).items() if v == 'yes'}


# Why a candidate is offered (provenance/sources/groups.py, ocr.py): shown under 「也许是 TA」.
REASONS = {'group_oc': '同一原创角色里有 {n} 张是 TA 画的', 'group_cluster': '相似的未识别图里有 {n} 张是 TA 画的',
           'group_run': '同一段保存的图里有 {n} 张是 TA 画的', 'ocr': '图上读到了 TA 的账号'}


def _reason(method, evidence):
    try:
        n = json.loads(evidence or '{}').get('count', 1)
    except ValueError:
        n = 1
    return REASONS.get(method, '有线索指向 TA').format(n=n)


def assets_of(library, artist, rejected=None):
    """Every picture attributed to an artist (and to the artists merged into it), less the ones judged 不是 TA."""
    lc = _core(library)
    if lc is not None:
        try:
            return _core_assets(lc, artist)
        finally:
            lc.close()
    c = _open(library)
    if c is None:
        return []
    try:
        no = _no(rejected, artist)
        return [a for a in _assets(c, artist, _yes(rejected, artist)) if a not in no]
    finally:
        c.close()


def runs(library, limit=5):
    """The latest provenance runs, newest first, with the coverage the last finished one measured (settings page).
    None when provenance has never run here, or its database predates run records."""
    c = _open(library)
    if c is None:
        return None
    try:
        rows = c.execute('SELECT * FROM runs ORDER BY id DESC LIMIT ?', (limit,)).fetchall()
    except sqlite3.OperationalError:
        return None
    finally:
        c.close()
    out = []
    for r in rows:
        run = {k: r[k] for k in ('id', 'kind', 'steps', 'started_at', 'finished_at', 'status', 'reason', 'found', 'traffic_mb')}
        run['report'] = json.loads(r['report']) if r['report'] else None
        out.append(run)
    coverage = next((x['report'].get('numbers') for x in out if x['report'] and x['report'].get('numbers')), None)
    return {'runs': out, 'coverage': coverage}


def name_of(library, artist):
    lc = _core(library)
    if lc is not None:
        try:
            return _core_describe(lc, artist)['name']
        finally:
            lc.close()
    c = _open(library)
    if c is None:
        return None
    try:
        return _describe(c, _canonical(c, artist))['name']
    finally:
        c.close()
