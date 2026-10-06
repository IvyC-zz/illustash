"""Group recommendations (docs/PROVENANCE.md section 9, 2026-09-29): an artist found for some pictures of a group is
offered for the rest of it, with a strength that depends on the kind of group. Local only, no request.

How often the pictures of a group that already had an artist agreed on it (this library, 2026-09-29):
    duplicates, similar groups   100% (921 / 491 groups)   the same drawing: attributed outright ('same_picture')
    original characters split    100% (8 groups)           an artist's own character: a strong candidate
    unmatched clusters            41%                       a weak candidate, needs two agreeing pictures
    save runs (Pixiv / X)         29% / 32%                 a weak candidate, needs two agreeing pictures
    ordinary characters            8%                       not used: many artists draw the same character
Candidates are shown as 「也许是 TA」 with their reason; 是 TA / 不是 in the detail panel settle them (the core's verdicts),
and 「找 TA 的其他图」 checks them picture by picture. Earlier group candidates are recomputed on every run.
"""
import collections
import json
import sqlite3
from pathlib import Path

from provenance import assets, db

# kind -> (method written, status, least agreeing pictures, score)
RULES = {
    'duplicate': ('same_picture', 'auto', 1, 1.0),
    'similar': ('same_picture', 'auto', 1, 1.0),
    'oc': ('group_oc', 'candidate', 1, 0.9),
    'cluster': ('group_cluster', 'candidate', 2, 0.4),
    'run': ('group_run', 'candidate', 2, 0.3),
}


def library_groups(library):
    """{kind: {group key: [picture]}} from the core's own groupings (read only) and the save runs."""
    from provenance.sources.iqdb import _save_runs
    out = {kind: collections.defaultdict(list) for kind in RULES}
    try:
        c = sqlite3.connect(Path(library.db).resolve().as_uri() + '?mode=ro', uri=True, timeout=30)
    except sqlite3.OperationalError:   # no core database yet (a new library): only the save runs
        c = None
    try:
        if c is None:
            raise sqlite3.OperationalError('no core database')
        for key, u in c.execute('SELECT grp, uuid FROM duplicates'):
            out['duplicate'][key].append(u)
        for key, u in c.execute('SELECT gid, uuid FROM similar_groups'):
            out['similar'][key].append(u)
        for key, u in c.execute("""SELECT m.entity, m.uuid FROM memberships m JOIN entities e ON e.id = m.entity
                                   WHERE e.original = 1 AND e.label_only = 0"""):
            out['oc'][key].append(u)
        for key, u in c.execute('SELECT cid, uuid FROM clusters'):
            out['cluster'][key].append(u)
    except sqlite3.OperationalError:   # a library from before one of these tables: those groups are simply absent
        pass
    finally:
        if c is not None:
            c.close()
    with assets.open_source(library) as s:
        added = dict(s.execute('SELECT uuid, added_at FROM assets'))
    for preset in ('pixiv', 'x'):
        runs = _save_runs([dict(p, added_at=added.get(p['asset'])) for p in assets.pictures(library, (preset,))])
        for i, run in enumerate(runs):
            out['run'][f'{preset}:{i}'] = [p['asset'] for p in run]
    return out


def unsolved_originals(library, c):
    """Pictures of original characters no picture of which has an artist yet, up to 3 per character: the first to ask
    IQDB about (one answer names the artist of the whole character)."""
    owned = {r[0] for r in c.execute("SELECT DISTINCT asset FROM image_artists WHERE status IN ('auto','confirmed')")}
    out = []
    for members in library_groups(library)['oc'].values():
        if not any(u in owned for u in members):
            out += members[:3]
    return out


def run(library, net=None, limit=None, log=print):
    counts = {'auto': 0, 'candidates': 0, 'stopped': ''}
    groups = library_groups(library)
    c = db.connect(library)
    try:
        owners = collections.defaultdict(set)
        for asset, artist in c.execute("SELECT asset, artist_id FROM image_artists WHERE status='auto'"):
            owners[asset].add(artist)
        with c:
            c.execute("DELETE FROM image_artists WHERE status='candidate' AND method LIKE 'group_%'")
            for kind, members_of in groups.items():
                method, status, least, score = RULES[kind]
                for key, members in members_of.items():
                    solved = [u for u in members if owners.get(u)]
                    if not solved or len(solved) == len(members):
                        continue
                    votes = collections.Counter(a for u in solved for a in owners[u])
                    artist, n = votes.most_common(1)[0]
                    if n < least or n != len(solved) or len(votes) > 1:   # every picture with an artist agrees, and enough of them
                        continue
                    proof = json.dumps({'group': kind, 'count': n, 'from': solved[:5]})
                    for u in members:
                        if u in owners:
                            continue
                        done = c.execute("""INSERT INTO image_artists(asset,artist_id,method,score,evidence,status) VALUES (?,?,?,?,?,?)
                                            ON CONFLICT(asset,artist_id) DO NOTHING""", (u, artist, method, score, proof, status)).rowcount
                        counts['auto' if status == 'auto' else 'candidates'] += done
        log(f"分组推荐：同一张图直接认定 {counts['auto']} 张，推荐候选 {counts['candidates']} 张")
    finally:
        c.close()
    return counts
