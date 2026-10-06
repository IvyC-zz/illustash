#!/usr/bin/env python3
"""Build grouped human-verdict splits and calibrate evidence thresholds offline.

Unreviewed images are never negatives. Album imports and bulk sweeps are not evaluation
gold. Known copies/crops/variants share a split. Production PROTO scores are excluded:
offline prototypes are refit using the training split only. Nothing is written to a DB.
"""
import argparse
import collections
import hashlib
import json
import math
import sqlite3
import sys
from pathlib import Path

import numpy as np

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT))
from web.features import Features
from web.learn import MIN_SEEDS, prototypes_for
from web.relate import GROUPING
from web.research_constants import MULTI_TAGS, STRONG_WD, STRONG_CCIP


def split_for(group):
    bucket = int(hashlib.sha256(('classification-v1:' + group).encode()).hexdigest()[:8], 16) % 10
    return 'train' if bucket < 6 else 'dev' if bucket < 8 else 'test'


def image_groups(c):
    parent = {u: u for u, in c.execute('SELECT uuid FROM assets')}
    def find(u):
        while parent[u] != u:
            parent[u] = parent[parent[u]]
            u = parent[u]
        return u
    def union(a, b):
        if a in parent and b in parent:
            a, b = find(a), find(b)
            parent[max(a, b)] = min(a, b)
    first = {}
    for u, group in c.execute('SELECT uuid,grp FROM duplicates'):
        union(u, first.setdefault(group, u))
    for a, b, kind in c.execute('SELECT a,b,kind FROM relations'):
        if kind in GROUPING:
            union(a, b)
    return {u: find(u) for u in parent}


def dataset(c):
    groups = image_groups(c)
    confirmed = set(map(tuple, c.execute('SELECT uuid,entity FROM family_confirmed')))
    family_count = collections.Counter(u for u, _ in confirmed)
    solo = {u for u, in c.execute('SELECT uuid FROM single_assets') if family_count[u] <= 1}
    multi = {u for u, tag in c.execute('SELECT uuid,value FROM general_hits') if tag in MULTI_TAGS}
    multi |= {u for u, n in family_count.items() if n > 1}
    solo -= multi
    scores = collections.defaultdict(dict)
    for u, entity, source, score in c.execute("""SELECT v.uuid,coalesce(e.parent_id,e.id),v.source,max(v.score)
        FROM candidates v JOIN entities e ON e.id=v.entity GROUP BY 1,2,3"""):
        if source in ('WD', 'CCIP'):
            scores[u, entity][source] = score
    paired, ignored_skins = collections.defaultdict(list), 0
    for u, entity, parent, status in c.execute("""SELECT s.uuid,s.entity,e.parent_id,s.status
        FROM assertions s JOIN entities e ON e.id=s.entity JOIN assets a ON a.uuid=s.uuid
        WHERE s.origin='review' AND e.merged_into IS NULL"""):
        family = parent or entity
        if status == 'rejected' and parent:
            ignored_skins += 1
            continue
        if status == 'confirmed' and (u, family) not in confirmed:
            continue
        paired[groups[u], family].append({'uuid': u, 'group': groups[u], 'entity': family,
            'label': int(status == 'confirmed'), 'split': split_for(groups[u]), 'origin': 'review',
            'people': 'multi' if u in multi else 'solo' if u in solo else 'unknown',
            'skin_verdict': bool(parent), 'WD': scores[u, family].get('WD'), 'CCIP': scores[u, family].get('CCIP')})
    rows, conflicts = [], []
    for (group, entity), members in sorted(paired.items()):
        if len({r['label'] for r in members}) != 1:
            conflicts.append({'group': group, 'entity': entity, 'uuids': sorted(r['uuid'] for r in members)})
            continue
        # One fixed representative per artwork/identity; no duplicate-weighted statistics.
        row = min(members, key=lambda r: r['uuid'])
        rows.append(row | {'reviewed_copies': len(members)})
    return rows, {'conflicts': conflicts, 'ignored_skin_rejections': ignored_skins,
                  'reviewed_pairs_before_dedup': sum(map(len, paired.values())),
                  'independent_artwork_groups': len({r['group'] for r in rows})}, groups


def label_inventory(c, source):
    """Inventory existing labels without promoting imported membership to gold."""
    assertions = [dict(zip(('origin', 'status', 'count'), row)) for row in c.execute(
        'SELECT origin,status,count(*) FROM assertions GROUP BY origin,status')]
    with sqlite3.connect(Path(source).resolve().as_uri() + '?mode=ro', uri=True) as src:
        decisions = [dict(zip(('dimension', 'author', 'count'), row)) for row in src.execute(
            'SELECT dim,"by",count(*) FROM decisions GROUP BY dim,"by"')]
    return {
        'assertions': assertions, 'source_decisions': decisions,
        'policies': {
            'review': 'Eligible current explicit identity verdict; group conflicts are excluded.',
            'album': 'Imported Photos membership; provenance alone does not certify per-image correctness or completeness.',
            'sweep': 'Bulk acceptance of machine memberships; keep separate from independent evaluation.',
            'history': 'Undo snapshots are previous state, not additional current labels.',
        },
        'unknown_assertion_origins': sorted({r['origin'] for r in assertions}
                                           - {'review', 'album', 'sweep'}),
    }


def add_prototype_scores(rows, features):
    by_entity = collections.defaultdict(list)
    for r in rows:
        by_entity[r['entity']].append(r)
        r['PROTO'] = None
    trained = 0
    for records in by_entity.values():
        train = [r for r in records if r['split'] == 'train']
        _, seeds = features.matrix([r['uuid'] for r in train if r['label'] == 1 and r['people'] == 'solo'])
        _, negatives = features.matrix([r['uuid'] for r in train if r['label'] == 0])
        if len(seeds) < MIN_SEEDS:
            continue
        protos = prototypes_for(seeds, negatives)
        if not protos:
            continue
        trained += 1
        P = np.stack([p[0] for p in protos])
        limit = np.array([p[1] for p in protos])
        for r in records:
            _, X = features.matrix([r['uuid']])
            if len(X):
                d = Features.distance(X, P)[0]
                accepted = d <= limit
                if accepted.any():
                    r['PROTO'] = 1.0 - float(d[accepted].min())
    return trained


def read_gold(c, path, groups):
    """Read separately reviewed scenes. Only explicit completeness permits omitted-label negatives."""
    identities = {e: (parent or e, kind) for e, parent, kind in c.execute('SELECT id,parent_id,kind FROM entities WHERE merged_into IS NULL')}
    scores = collections.defaultdict(dict)
    for u, e, source, score in c.execute('SELECT uuid,entity,source,score FROM candidates'):
        if e in identities and source in ('WD', 'CCIP'):
            key = (u, identities[e][0])
            scores[key][source] = max(score, scores[key].get(source, 0))
    by_image = collections.defaultdict(set)
    for u, e in scores: by_image[u].add(e)
    out, seen, seen_groups = [], set(), set()
    with Path(path).open() as f:
        for line in f:
            item = json.loads(line)
            if item.get('reviewed') is not True: continue
            u = item['uuid']
            if u in seen or u not in groups or item['group'] != groups[u] or groups[u] in seen_groups:
                raise ValueError('Duplicate, missing, or regrouped validation image; rebuild/reconcile the manifest')
            seen.add(u)
            seen_groups.add(groups[u])
            people, chars = item.get('people'), item.get('characters')
            if people not in ('solo', 'multi', 'none', 'unknown') or not isinstance(chars, list):
                raise ValueError('Human people label and character list are required')
            if any(e not in identities or identities[e][1] == 'work' for e in chars):
                raise ValueError('Use existing character/skin IDs; unknown identity is not a negative')
            positives = {identities[e][0] for e in chars}
            complete = item.get('complete') is True
            if (people == 'none' and positives) or (people == 'solo' and len(positives) > 1):
                raise ValueError('Conflicting people and character labels')
            if complete and people in ('solo', 'multi') and not positives:
                raise ValueError('Unidentified people do not form a complete negative character list')
            if complete and people == 'unknown':
                raise ValueError('An uncertain scene cannot be exhaustively labelled')
            for e in sorted(positives | (by_image[u] if complete else set())):
                out.append({'uuid': u, 'group': groups[u], 'entity': e, 'label': int(e in positives),
                            'split': split_for(groups[u]), 'origin': 'validation', 'people': people,
                            'skin_verdict': False, 'reviewed_copies': 1,
                            'WD': scores[u,e].get('WD'), 'CCIP': scores[u,e].get('CCIP')})
    return out, len(seen)


def independent_rows(rows):
    """One deterministically chosen identity question per artwork for precision intervals."""
    groups = {}
    for r in rows:
        key = r['group']
        if key not in groups or hashlib.sha256(r['entity'].encode()).digest() < hashlib.sha256(groups[key]['entity'].encode()).digest():
            groups[key] = r
    return list(groups.values())


def metrics(rows, source, threshold):
    rows = independent_rows(rows)
    tp = fp = fn = tn = 0
    for r in rows:
        selected = r.get(source) is not None and r[source] >= threshold
        tp += selected and r['label'] == 1
        fp += selected and r['label'] == 0
        fn += not selected and r['label'] == 1
        tn += not selected and r['label'] == 0
    n = tp + fp
    precision = tp / n if n else None
    lower = None
    if n:
        z = 1.96
        lower = (precision + z*z/(2*n) - z*math.sqrt(precision*(1-precision)/n + z*z/(4*n*n))) / (1 + z*z/n)
    return {'tp': tp, 'fp': fp, 'fn': fn, 'tn': tn, 'selected': n, 'precision': precision,
            'precision_lower95': lower, 'recall': tp/(tp+fn) if tp+fn else None,
            'false_positive_rate': fp/(fp+tn) if fp+tn else None}


def calibrate(rows, source, baseline):
    dev = independent_rows([r for r in rows if r['split'] == 'dev'])
    test = independent_rows([r for r in rows if r['split'] == 'test'])
    grid = sorted(set([baseline, .35, .5, .7, .8, .85, .9, .92, .94, .95, .96, .97, .98, .99, .995]))
    curve = [{'threshold': t, **metrics(dev, source, t)} for t in grid]
    eligible = [r for r in curve if r['selected'] >= 30 and (r['precision_lower95'] or 0) >= .90]
    choice = max(eligible, key=lambda r: (r['recall'] or 0, r['precision_lower95'], r['threshold'])) if eligible else None
    selected = choice['threshold'] if choice else baseline
    holdout = metrics(test, source, selected)
    enough = all(sum(r['label'] == y for r in part) >= 30 for part in (dev, test) for y in (0, 1))
    passed = bool(choice and enough and holdout['selected'] >= 30 and (holdout['precision_lower95'] or 0) >= .90)
    return {'baseline': baseline, 'dev_curve': curve, 'dev_selected': selected if choice else None,
            'test_baseline': metrics(test, source, baseline), 'test_selected': holdout,
            'independent_holdout_gate': 'pass' if passed else 'hold',
            'reason': 'Reviewed-sample precision gate passed; still requires end-to-end ownership evaluation' if passed else
                      'Insufficient independent labels or precision lower bound below 90%; retain production threshold',
            'production_threshold_changed': False}


def review_queue(c, groups, rows, per_stratum=25):
    labelled = {r['group'] for r in rows}
    solo = {u for u, in c.execute('SELECT uuid FROM single_assets')}
    tags = collections.defaultdict(set)
    for u, tag in c.execute('SELECT uuid,value FROM general_hits'):
        if tag in set(MULTI_TAGS) | {'1girl', '1boy', '1other', 'solo'}:
            tags[u].add(tag)
    evidence = collections.defaultdict(list)
    for u, e, wd, cc in c.execute('SELECT uuid,entity,wd,ccip FROM family_evidence'):
        evidence[u].append((e, wd, cc))
    variants = {u for u, in c.execute('SELECT DISTINCT v.uuid FROM candidates v JOIN entities e ON e.id=v.entity WHERE e.parent_id IS NOT NULL')}
    sizes = dict(c.execute('SELECT entity,count(*) FROM memberships GROUP BY entity'))
    buckets = collections.defaultdict(list)
    for u in sorted(groups, key=lambda u: hashlib.sha256(u.encode()).digest()):
        if groups[u] in labelled:
            continue
        e = evidence[u]
        if tags[u] & set(MULTI_TAGS): kind = 'multi_model_hint'
        elif not tags[u]: kind = 'no_person_model_hint'
        elif u in variants: kind = 'skin_candidate'
        elif len(e) > 1: kind = 'competing_characters'
        elif any(sizes.get(x[0], 0) < 20 for x in e): kind = 'sparse_character'
        elif not e: kind = 'unmatched'
        elif u in solo: kind = 'solo'
        else: kind = 'other'
        buckets[kind].append(u)
    out, seen = [], set()
    for kind, uuids in sorted(buckets.items()):
        n = 0
        for u in uuids:
            if groups[u] in seen: continue
            seen.add(groups[u]); n += 1
            out.append({'uuid': u, 'group': groups[u], 'stratum': kind, 'split': split_for(groups[u]),
                        'reviewed': False, 'characters': [], 'people': None, 'complete': False,
                        'image_url': f'http://127.0.0.1:8777/image?id={u}&size=full'})
            if n >= per_stratum: break
    return out


def run(library, source, output, gold=None):
    library, source, output = map(lambda p: Path(p).resolve(), (library, source, output))
    output.mkdir(parents=True, exist_ok=False)
    with sqlite3.connect(library.as_uri() + '?mode=ro', uri=True) as c:
        c.execute('BEGIN')
        rows, audit, groups = dataset(c)
        audit['label_inventory'] = label_inventory(c, source)
        audit['independently_reviewed_scenes'] = 0
        if gold:
            extra, count = read_gold(c, gold, groups)
            known_groups = {r['group'] for r in rows}
            if any(r['group'] in known_groups for r in extra):
                raise ValueError('Gold overlaps existing verdict groups; keep independent validation scenes separate')
            rows += extra
            audit['independently_reviewed_scenes'] = count
        queue = review_queue(c, groups, rows)
        identity_options = [(r[0], r[1]) for r in c.execute("SELECT id,name FROM entities WHERE kind='character' AND merged_into IS NULL ORDER BY name")]
    trained = add_prototype_scores(rows, Features(source).load())
    payload = ''.join(json.dumps(r, ensure_ascii=False, sort_keys=True)+'\n' for r in rows)
    (output/'verdicts.jsonl').write_text(payload)
    (output/'review-queue.jsonl').write_text(''.join(json.dumps(r, ensure_ascii=False)+'\n' for r in queue))
    from tools.validation_worksheet import write_worksheet
    write_worksheet(output/'review.html', queue, identity_options)
    report = {'dataset_sha256': hashlib.sha256(payload.encode()).hexdigest(), 'label_source': 'explicit review only',
              'pairs': len(rows), 'split_counts': dict(collections.Counter(r['split'] for r in rows)),
              'source_coverage': {s: dict(collections.Counter(r['split'] for r in rows if r.get(s) is not None))
                                  for s in ('WD', 'CCIP', 'PROTO')},
              'label_counts': dict(collections.Counter(r['label'] for r in rows)), 'audit': audit,
              'offline_prototype_characters': trained, 'unreviewed_queue_size': len(queue),
              'thresholds': {s: calibrate(rows, s, baseline) for s, baseline in [('WD', STRONG_WD), ('CCIP', STRONG_CCIP), ('PROTO', STRONG_CCIP)]},
              'limitations': ['Reviewed cases are selection-biased; metrics are not whole-library accuracy.',
                             'Precision intervals use one fixed identity question per artwork to avoid correlated votes.',
                             'Known duplicate/variant groups are split together; undiscovered duplicates may remain.',
                             'External model/reference training overlap cannot be ruled out.',
                             'Source-wise strong-evidence calibration does not validate ownership ranking interactions.',
                             'Review-queue strata are model hints, not human ground truth.'],
              'production_thresholds_changed': False}
    (output/'report.json').write_text(json.dumps(report, ensure_ascii=False, indent=2))
    return report


if __name__ == '__main__':
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--library', type=Path, default=ROOT/'review/library.sqlite')
    p.add_argument('--source', type=Path, default=ROOT/'review/pi.db')
    p.add_argument('--output', type=Path, required=True)
    p.add_argument('--gold', type=Path, help='human-reviewed JSONL exported from review.html; unreviewed rows are ignored')
    a = p.parse_args()
    result = run(a.library, a.source, a.output, a.gold)
    print(json.dumps({k: v for k, v in result.items() if k != 'thresholds'}, ensure_ascii=False, indent=2))
