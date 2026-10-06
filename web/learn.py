"""Learning from the user's verdicts, and grouping look-alike unmatched images.

Prototypes: a character with at least MIN_SEEDS human-confirmed images gets up to MAX_PROTOS prototypes
(each a tight group of its confirmed images, so casual wear and a swimsuit become separate prototypes).
Each prototype claims library images within its own threshold: the 90th percentile of its group's
distances, capped at CAP, and kept below every image the user removed from that character (negatives).
Claims enter the ownership ranking as PROTO evidence (web/store.py EVIDENCE); human verdicts still win.

Weak claims (`weak_claims`) are leads for「也可能是」and never decide ownership: a prototype's outer ring
(threshold .. HINT_SCALE × threshold, at most HINT_CAP), and the middle tier (p75..p90) of characters patched in
from Danbooru, whose own claims stop at p75 (tools/char_scan.py claim_thresholds).

Clusters: unmatched images grouped by density — the image with most neighbours within RADIUS seeds a
group of its free neighbours, and so on — which proved pure on the real pool (2026-09-23 sheets).
WD appearance tags then refine them (2026-09-24, checked on the real pool): a group holding two hair-colour
families (each ≥ SPLIT_SHARE) is split; each group gets its most distinctive traits (rarer tags weigh more);
"same person?" hints need close centres and no hair-colour conflict, or somewhat farther centres (≤ TRAIT_NEAR)
with near-identical weighted trait profiles (≥ TRAIT_SIMILAR). On the real pool no group needed a split and
one close pair conflicted; of the farther trait-alike pairs about 6 in 10 were one person, so they are shown as
the weaker「外观相似」.
"""
import hashlib
import json
import threading
import time

import numpy as np

from web.features import Features, pair_block_rows
from web.library import shared_dir
from web.research_constants import VISIBLE

MIN_SEEDS = 5
MAX_PROTOS = 8
MAX_SEEDS = 600           # sampled per character; mika alone has ~3,000 confirmations
GROUP = 0.06              # a prototype's group: confirmed images within this of its centre image
CAP = 0.08                # widest claim; curated upstream anchors sit at ~0.09 (p90 median)
FLOOR = 0.03
NEGATIVE_MARGIN = 0.9     # stay this fraction inside the nearest removed image
HINT_SCALE, HINT_CAP = 1.5, 0.11
RADIUS = 0.06             # clustering radius for unmatched images
# The 未识别 groups (2026-09-26): on confirmed single-person images, groups at 0.07 are 99.1% one person
# (0.06: 99.4%); on the unmatched pool 0.07 with pairs covers 42% of images instead of 17% (0.06, three or more).
GROUP_RADIUS, GROUP_MIN = 0.07, 2
NEAR = 0.08               # two cluster centres this close are probably one person
TRAIT_NEAR, TRAIT_SIMILAR, SPLIT_SHARE = 0.12, 0.85, 0.3
# Appearance tags checked against tags/tags_v3.csv; WD confuses neighbouring colours, so hair counts by family.
HAIR = {'blonde_hair': 'blonde', 'brown_hair': 'brown', 'light_brown_hair': 'brown', 'black_hair': 'black',
        'white_hair': 'white', 'grey_hair': 'white', 'pink_hair': 'pink', 'red_hair': 'red', 'blue_hair': 'blue',
        'light_blue_hair': 'blue', 'dark_blue_hair': 'blue', 'aqua_hair': 'blue', 'green_hair': 'green',
        'purple_hair': 'purple', 'orange_hair': 'orange'}
TRAITS = tuple(HAIR) + ('blue_eyes', 'red_eyes', 'brown_eyes', 'green_eyes', 'purple_eyes', 'yellow_eyes', 'pink_eyes',
    'black_eyes', 'aqua_eyes', 'grey_eyes', 'orange_eyes', 'halo', 'mechanical_halo', 'animal_ears', 'cat_ears', 'fox_ears',
    'rabbit_ears', 'dog_ears', 'wolf_ears', 'horse_ears', 'horns', 'demon_horns', 'dragon_horns', 'pointy_ears', 'tail', 'wings',
    'ahoge', 'twintails', 'ponytail', 'side_ponytail', 'braid', 'hair_bun', 'drill_hair', 'hair_over_one_eye', 'multicolored_hair',
    'streaked_hair', 'two-tone_hair', 'gradient_hair', 'heterochromia', 'x_hair_ornament', 'dark_skin', 'glasses',
    'very_long_hair', 'short_hair')


def _groups(X, radius, minimum, limit=None):
    """Density-leader grouping of normalised rows: returns lists of row indices, biggest first."""
    if not len(X):
        return []
    # Keep adjacency as bits, not an N×N float matrix plus an N×N boolean matrix.
    # Counts and leader order match the dense algorithm; unpack only selected rows.
    n = len(X)
    packed = np.empty((n, (n + 7) // 8), np.uint8)
    counts = np.empty(n, np.int64)
    block = pair_block_rows(n)
    for start in range(0, n, block):
        near = Features.distance(X[start:start + block], X) <= radius
        counts[start:start + block] = near.sum(1)
        packed[start:start + block] = np.packbits(near, axis=1)
    free = np.ones(len(X), bool)
    out = []
    for i in np.argsort(-counts):
        if not free[i]:
            continue
        members = np.nonzero(np.unpackbits(packed[i], count=n).astype(bool) & free)[0]
        if len(members) < minimum:
            continue
        free[members] = False
        out.append(members)
        if limit and len(out) >= limit:
            break
    return out


def prototypes_for(seeds, negatives):
    """[(vector, threshold, seed count, hint limit)] for one character."""
    if len(seeds) > MAX_SEEDS:
        seeds = seeds[np.random.default_rng(0).choice(len(seeds), MAX_SEEDS, replace=False)]
    out = []
    for members in _groups(seeds, GROUP, max(3, int(0.05 * len(seeds))), MAX_PROTOS):
        centre = seeds[members].mean(0)
        centre /= np.linalg.norm(centre)
        spread = Features.distance(seeds[members], centre[None])[:, 0]
        threshold = min(max(float(np.percentile(spread, 90)), FLOOR), CAP)   # python floats: float32 0.03 < FLOOR
        hint = min(HINT_SCALE * threshold, HINT_CAP)
        if len(negatives):
            nearest = NEGATIVE_MARGIN * float(Features.distance(negatives, centre[None]).min())
            threshold, hint = min(threshold, nearest), min(hint, nearest)
        if threshold >= FLOOR:
            out.append((centre.astype(np.float32), threshold, int(len(members)), max(hint, threshold)))
    return out


def signatures(c, features=None):
    """Content fingerprints: undo may reuse rowids; removed assets change the seed pool too.

    The version prefix invalidates old count:rowid signatures and prototypes learned with
    skin rejections as parent negatives. Ordering is independent of insertion/undo order.
    """
    hashes = {}
    versions = features.versions if features is not None else {}
    from . import identity
    if identity.enabled(c):
        # A family depends only on its own members, so one identity decision relearns only the families it touches.
        members = {}
        for ident, family, canonical in c.execute('SELECT id,character_id,canonical_id FROM identity_nodes ORDER BY id'):
            members.setdefault(family, []).append([ident, canonical])
        context = lambda family: json.dumps(members.get(family, []))
        from tools.calibrate_classification import image_groups
        groups = image_groups(c)
        rows = c.execute('''SELECT s.character_id,s.raw_entity,s.uuid,s.status,s.origin,
            EXISTS(SELECT 1 FROM single_assets a WHERE a.uuid=s.uuid)
            FROM effective_assertions s ORDER BY 1,2,3''')
        for family, entity, uuid, status, origin, solo in rows:
            digest = hashes.get(family) or hashes.setdefault(family, hashlib.sha256(context(family).encode()))
            digest.update(json.dumps([entity,uuid,status,origin,solo,groups[uuid]]).encode())
            digest.update(versions.get(uuid,b'missing'))
        return {e:'identity-v2:'+h.hexdigest() for e,h in hashes.items()}
    for family, entity, uuid, status, active in c.execute("""SELECT coalesce(x.parent_id,x.id),
        s.entity,s.uuid,s.status,a.uuid IS NOT NULL FROM assertions s JOIN entities x ON x.id=s.entity
        LEFT JOIN assets a ON a.uuid=s.uuid ORDER BY 1,2,3"""):
        digest = hashes.setdefault(family, hashlib.sha256())
        digest.update((json.dumps([entity, uuid, status, active], ensure_ascii=True) + '\n').encode())
        digest.update(versions.get(uuid, b'missing'))
    return {e: 'v3:' + h.hexdigest() for e, h in hashes.items()}


def learn(store, features, force=False, reclaim=False, anchors=None, *, publisher=None):
    """Refit prototypes of characters whose verdicts changed, then re-claim the whole library. Returns counts.

    `reclaim` re-claims even when nothing was refitted (new images after a sync); `anchors` adds patched-anchor leads."""
    store.require_writable()
    if publisher is None:
        from . import terms_core
        publisher = terms_core.LearningPublisher(store.path, lock=store._labels_lock)
    token = publisher.snapshot()
    fitted = {}
    with store.connect() as c:
        c.execute('BEGIN')  # signatures and training rows must describe the same snapshot
        # Label-only marks (the shared OC) are never learned; a stored model of one is dropped below.
        marks = {r[0] for r in c.execute('SELECT id FROM entities WHERE label_only=1 OR mascot=1')}   # 当作标签: not learned either
        current = {e: sig for e, sig in signatures(c, features).items() if e not in marks}
        known = dict(c.execute('SELECT entity,signature FROM learn_state'))
        dirty = [e for e, sig in current.items() if force or known.get(e) != sig] + [e for e in known if e not in current]
        if not dirty and not reclaim:
            return {'relearned': 0, 'changed': 0}
        for entity in dirty:
            if entity in marks:
                fitted[entity] = []
                continue
            confirmed = [r[0] for r in c.execute("SELECT DISTINCT uuid FROM family_confirmed WHERE entity=? ORDER BY uuid", (entity,))]
            removed = [r[0] for r in c.execute("""SELECT s.uuid FROM assertions s JOIN assets a ON a.uuid=s.uuid
                WHERE s.entity=? AND s.status='rejected' ORDER BY s.uuid""", (entity,))]
            _, seeds = features.matrix(confirmed)
            _, negatives = features.matrix(removed)
            fitted[entity] = prototypes_for(seeds, negatives) if len(seeds) >= MIN_SEEDS else []
        # Train and claim from the same read snapshot. Unchanged prototypes/states
        # are retained; the core replaces the resulting complete batch atomically.
        protos = [tuple(r) for r in c.execute('SELECT * FROM prototypes') if r['entity'] not in fitted]
        protos += [(e, k, v.tobytes(), t, n, h) for e, values in fitted.items()
                   for k, (v, t, n, h) in enumerate(values)]
        stamp = time.strftime('%Y-%m-%d %H:%M:%S')
        dirty_set = set(dirty)
        states = [tuple(r) for r in c.execute('SELECT * FROM learn_state') if r['entity'] not in dirty_set]
        states += [(e, current[e], stamp) for e in dirty if e in current]
        claims, weak = _claim_results(store, c, features, anchors,
                                     rows=[(e, v, t, h) for e, k, v, t, n, h in protos])
        batch = {'v': 1, 'token': token, 'relearned': len(dirty),
                 'prototypes': [(e, k, v.hex(), t, n, h) for e, k, v, t, n, h in protos],
                 'learn_state': states,
                 'proto_claims': [(u, e, score) for (u, e), score in claims.items()],
                 'weak_claims': [(u, e, source, score) for (u, e, source), score in weak.items()]}
    # Python 的读连接先关闭，核心随后原子发布。
    return publisher.publish(batch)


def _scan(X, uuids, P, owner, lo, hi):
    """{(uuid, entity): best score} for distances in (lo, hi] to rows of P."""
    found = {}
    if not len(P) or not len(X):
        return found
    for start in range(0, len(X), 8192):
        D = Features.distance(X[start:start + 8192], P)
        for i, j in zip(*np.nonzero((D > lo) & (D <= hi))):
            key, score = (uuids[start + i], owner[j]), 1.0 - float(D[i, j])
            found[key] = max(score, found.get(key, 0.0))
    return found


def _claim_results(store, c, features, anchors=None, *, rows=None):
    """The existing numerical learner; no writes. Both publishers use these exact calculations."""
    if rows is None:
        rows = c.execute('SELECT entity,vec,threshold,hint FROM prototypes').fetchall()
    library = [r[0] for r in c.execute('SELECT uuid FROM assets')]
    uuids, X = features.matrix(library)
    P = np.stack([np.frombuffer(r[1], np.float32) for r in rows]) if rows else np.zeros((0, X.shape[1]), np.float32)
    owner = [r[0] for r in rows]
    limit = np.array([r[2] for r in rows], np.float32)
    claims = _scan(X, uuids, P, owner, -1.0, limit)
    weak = {(u, e, 'PROTO'): s for (u, e), s in _scan(X, uuids, P, owner, limit, np.array([r[3] for r in rows], np.float32)).items()
            if (u, e) not in claims}
    if anchors:
        owners = [store._resolve(c, a) for a in anchors['entities']]
        weak |= {(u, e, 'CCIP'): s for (u, e), s in _scan(X, uuids, anchors['vectors'], owners, anchors['lo'], anchors['hi']).items()}
    return claims, weak


def _hair(tags):
    families = {HAIR[t] for t in tags if t in HAIR}
    return next(iter(families)) if len(families) == 1 else None


def _split(members, tags, uuids):
    """Split a group holding two or more hair-colour families; images without a single family join the largest part."""
    families = [_hair(tags.get(uuids[i], ())) for i in members]
    counts = {f: families.count(f) for f in set(families) if f}
    big = sorted((f for f, n in counts.items() if n >= 3 and n >= SPLIT_SHARE * len(members)), key=lambda f: -counts[f])
    if len(big) < 2:
        return [members]
    parts = {f: [] for f in big}
    for i, f in zip(members, families):
        parts[f if f in parts else big[0]].append(i)
    return [np.array(p) for p in parts.values()]


def oc_groups(store, features, entity):
    """Look-alike groups inside one character's images (the shared OC), same grouping as the unmatched pool.

    Computed on demand and never stored: splitting the OC mark into separate OCs is rebuilt whenever it is asked for."""
    with store.connect() as c:
        pool = [r[0] for r in c.execute(f"SELECT m.uuid FROM memberships m WHERE m.entity=? AND {VISIBLE.format('m')}", (entity,))]
        marks = ','.join('?' for _ in TRAITS)
        tags = {}
        for u, v in c.execute(f'SELECT uuid,value FROM general_hits WHERE value IN ({marks})', TRAITS):
            tags.setdefault(u, set()).add(v)
    uuids, X = features.matrix(pool)
    groups = [part for g in _groups(X, RADIUS, 2) for part in _split(g, tags, uuids) if len(part) >= 2]
    return {'total': len(pool), 'groups': [[uuids[i] for i in g] for g in groups]}


def cluster(store, features):
    """Regroup the unmatched pool. Returns the number of groups."""
    with store.connect() as c:
        pool = [r[0] for r in c.execute(f"""SELECT a.uuid FROM assets a WHERE {VISIBLE.format('a')}
            AND NOT EXISTS(SELECT 1 FROM memberships m WHERE m.uuid=a.uuid) AND NOT EXISTS(SELECT 1 FROM deferred d WHERE d.uuid=a.uuid)""")]
        marks = ','.join('?' for _ in TRAITS)
        tags = {}
        for u, v in c.execute(f'SELECT uuid,value FROM general_hits WHERE value IN ({marks})', TRAITS):
            tags.setdefault(u, set()).add(v)
    uuids, X = features.matrix(pool)
    groups = [part for g in _groups(X, GROUP_RADIUS, GROUP_MIN) for part in _split(g, tags, uuids) if len(part) >= GROUP_MIN]
    n = len(groups)
    centres = np.stack([X[g].mean(0) / np.linalg.norm(X[g].mean(0)) for g in groups]) if n else np.zeros((0, X.shape[1]))
    D = Features.distance(centres, centres) if n else np.zeros((0, 0))
    np.fill_diagonal(D, 9)
    # Trait profile per group: share of members with each tag; rarer traits (fewer groups have them) weigh more.
    share = np.array([[sum(t in tags.get(uuids[i], ()) for i in g) / len(g) for t in TRAITS] for g in groups]) if n else np.zeros((0, len(TRAITS)))
    weight = np.log((n + 1) / ((share >= 0.5).sum(0) + 1))
    profile = share * weight
    profile /= np.maximum(np.linalg.norm(profile, axis=1, keepdims=True), 1e-9)
    alike = profile @ profile.T
    hair = [max(((f, sum(share[k][TRAITS.index(t)] for t in HAIR if HAIR[t] == f)) for f in set(HAIR.values())), key=lambda x: x[1]) for k in range(n)]
    dominant = [f if s >= 0.6 else None for f, s in hair]
    stamp = time.strftime('%Y-%m-%d %H:%M:%S')
    with store.connect() as c:
        c.execute('BEGIN IMMEDIATE')
        c.execute('DELETE FROM clusters')
        c.execute('DELETE FROM cluster_info')
        for k, members in enumerate(groups):
            near = (None, None, None)
            for j in np.argsort(D[k]):
                if D[k, j] > TRAIT_NEAR:
                    break
                if dominant[k] and dominant[j] and dominant[k] != dominant[j]:
                    continue                                   # different hair colour: not one person
                if D[k, j] <= NEAR:
                    near = (int(j) + 1, float(D[k, j]), 'look')
                    break
                if alike[k, j] >= TRAIT_SIMILAR:
                    near = (int(j) + 1, float(D[k, j]), 'trait')
                    break
            traits = [TRAITS[t].replace('_', ' ') for t in np.argsort(-share[k] * weight) if share[k][t] >= 0.5][:4]
            c.execute('INSERT INTO cluster_info VALUES (?,?,?,?,?,?,?)', (k + 1, len(members), near[0], near[1], stamp, near[2], json.dumps(traits)))
            c.executemany('INSERT INTO clusters VALUES (?,?)', [(uuids[i], k + 1) for i in members])
    return n


def patch_anchors(store):
    """Anchors of characters patched in from Danbooru, with their middle tier (p75, p90] as weak-claim range."""
    import json
    import sqlite3
    import sys
    from pathlib import Path
    review = shared_dir(store.source)
    tools = Path(__file__).resolve().parents[1] / 'tools'
    if str(tools) not in sys.path:
        sys.path.insert(0, str(tools))
    from char_scan import anchor_index
    anchors_path, ids_path = review / 'char-anchors.npy', review / 'char-anchors.txt'
    if not anchors_path.exists():
        return None
    A = np.load(anchors_path)
    ids = [line.strip() for line in ids_path.open() if line.strip()]
    patched = set()
    for path in (review / 'char-index').glob('*.json'):
        try:
            if json.loads(path.read_text(encoding='utf-8')).get('source') == 'danbooru-patch':
                patched.add(path.stem)
        except ValueError:
            continue
    _, _, _, th75, ent = anchor_index(A, ids, str(store.source))
    db = sqlite3.connect(f'file:{store.source}?mode=ro', uri=True)
    p90 = dict(db.execute('SELECT char_id,anchor_p90 FROM characters'))
    db.close()
    keep = [i for i, c in enumerate(ids) if c in patched and p90.get(c)]
    if not keep:
        return None
    with store.connect() as c:
        entity = dict(c.execute("SELECT key,entity_id FROM links WHERE system='ccip'"))
    keep = [i for i in keep if ent[i] in entity]
    vectors = A[keep] / np.linalg.norm(A[keep], axis=1, keepdims=True)
    return {'entities': [entity[ent[i]] for i in keep], 'vectors': vectors.astype(np.float32),
            'lo': np.array([th75[i] for i in keep], np.float32), 'hi': np.array([p90[ids[i]] for i in keep], np.float32)}


QUIET, QUIET_MAX = 1.5, 10.0


class Learner:
    """Background thread: relearn after the user's verdicts change, regroup the unmatched pool when asked."""
    def __init__(self, store, features, *, publisher=None):
        self.store, self.features = store, features
        self.publisher = publisher
        self.wake, self.regroup = threading.Event(), threading.Event()
        self.state = {'status': 'idle'}
        self.anchors = None
        self.thread = threading.Thread(target=self.run, name='learner', daemon=True)

    def start(self):
        self.regroup.set()
        self.wake.set()
        self.thread.start()

    def reload(self):
        """The external index changed: re-read the patched anchors, then claim and regroup again."""
        self.anchors = None
        self.poke(regroup=True)

    def poke(self, regroup=False):
        if regroup:
            self.regroup.set()
        self.wake.set()

    def run(self):
        while True:
            self.wake.wait(timeout=60)
            self.wake.clear()
            # Wait for the edits to pause (QUIET s without a poke, at most QUIET_MAX s): a pass holds the write lock
            # for 0.4–1.2 s, and starting one after every answer made the next answer wait for it.
            started = time.monotonic()
            while time.monotonic() - started < QUIET_MAX and self.wake.wait(QUIET):
                self.wake.clear()
            try:
                self.state = {'status': 'learning'}
                self.features.load()
                if self.anchors is None:
                    self.anchors = patch_anchors(self.store) or {}
                # A regroup request comes with new images (start, sync): claim them even if no verdict changed.
                extra = {'publisher': self.publisher} if self.publisher is not None else {}
                result = learn(self.store, self.features, reclaim=self.regroup.is_set(), anchors=self.anchors, **extra)
                if result.get('status') == 'stale':
                    # No models/signatures were written. Keep regroup pending, skip clustering,
                    # and retry after the normal quiet/debounce period from a fresh snapshot.
                    self.wake.set()
                    self.state = {'status': 'idle'}
                    time.sleep(2)
                    continue
                result.pop('status', None)  # transport status must not replace the public idle state
                if self.regroup.is_set():
                    self.regroup.clear()
                    self.state = {'status': 'grouping'}
                    result['clusters'] = cluster(self.store, self.features)
                self.state = {'status': 'idle', 'at': time.strftime('%H:%M:%S'), **result}
            except Exception as exc:
                self.state = {'status': 'error', 'message': f'{type(exc).__name__}: {exc}'[:300]}
            time.sleep(2)   # batch bursts of edits into one pass
