"""Hand provenance's machine results to the terms core as an evidence package (docs/EVIDENCE_PACKAGES.md).

Once a library's artists live in the core (docs/TERMS_ARTISTS.md), provenance.sqlite is this worker's own cache: the
core never reads image_artists again, it imports what this module writes. Each package is a full snapshot of every
picture still in the library: the core keeps rows it already has by fingerprint, and what a picture no longer has
(a candidate the groups step did not find again) stops counting (schema 3: provenance evidence counts per snapshot).

The mapping must stay byte-for-byte the one native/terms/src/legacy_artists.rs used when the library was cut over
(source, method, metric, data, and the content-derived version), or the first package after cutover would add every
attribution a second time instead of recognising it.
"""
from decimal import Decimal
import hashlib
import json
import math
import os
from pathlib import Path
import sqlite3
import time

from provenance import db

FORMAT = 'illustash-evidence/1'
PRODUCER = 'provenance'
NAMESPACE = 'library'            # the library's own assets.uuid, whatever source a picture came from
METRICS = {'pixel_hash': 'exact_match', 'gallery_match': 'gallery_similarity', 'ocr': 'ocr_confidence'}


def _float(x):
    """A float laid out the way serde_json (ryu) writes it. Python's repr has the same shortest digits but switches to
    exponents at other magnitudes and writes them differently ('e-09' against 'e-9'); a hash over the text must match.
    Pinned by native/terms/src/strict_json.rs (float_layout) and tests/test_terms_artists.py with the same literals."""
    if not math.isfinite(x):
        raise ValueError('JSON 不能表示非有限数值')
    if x == 0:
        return '-0.0' if math.copysign(1, x) < 0 else '0.0'
    sign = '-' if x < 0 else ''
    d = Decimal(repr(abs(x))).normalize()
    _, digits, k = d.as_tuple()
    digits = ''.join(map(str, digits))
    n = len(digits)
    kk = n + k                       # 10^(kk-1) <= |x| < 10^kk
    if 0 <= k and kk <= 16:
        body = digits + '0' * k + '.0'
    elif 0 < kk <= 16:
        body = digits[:kk] + '.' + digits[kk:]
    elif -5 < kk <= 0:
        body = '0.' + '0' * -kk + digits
    else:
        exp = kk - 1
        body = (digits if n == 1 else digits[0] + '.' + digits[1:]) + 'e' + ('+' if exp > 0 else '') + str(exp)
    return sign + body


def canonical(value):
    """serde_json's compact form with sorted keys, floats included: what the core hashes."""
    if isinstance(value, dict):
        return '{' + ','.join(json.dumps(k, ensure_ascii=False) + ':' + canonical(value[k]) for k in sorted(value)) + '}'
    if isinstance(value, (list, tuple)):
        return '[' + ','.join(canonical(v) for v in value) + ']'
    if isinstance(value, float):
        return _float(value)
    return json.dumps(value, ensure_ascii=False)


def _ro(path):
    c = sqlite3.connect(Path(path).resolve().as_uri() + '?mode=ro', uri=True, timeout=30)
    c.row_factory = sqlite3.Row
    return c


def _observed(updated_at, fallback):
    """SQLite CURRENT_TIMESTAMP (UTC) as ISO 8601 with a zone; the package requires one."""
    try:
        return time.strftime('%Y-%m-%dT%H:%M:%SZ', time.strptime(updated_at, '%Y-%m-%d %H:%M:%S'))
    except (TypeError, ValueError):
        return fallback


def _own(artist):
    return {'kind': 'artist', 'link': {'system': PRODUCER, 'key': artist}}


def rows(library):
    """(assets, terms, evidence) of a snapshot: the attributions of pictures still in the library."""
    lib_db = Path(library.path) / 'library.sqlite'
    with _ro(lib_db) as c:
        present = {r[0] for r in c.execute('SELECT uuid FROM assets')}
    now = time.strftime('%Y-%m-%dT%H:%M:%SZ', time.gmtime())
    evidence, assets = [], []
    c = _ro(db.path_of(library))
    try:
        for r in c.execute('SELECT asset,artist_id,method,score,evidence,status,updated_at FROM image_artists ORDER BY asset,artist_id'):
            if r['asset'] not in present:
                continue
            try:
                data = json.loads(r['evidence'])
            except (TypeError, ValueError):
                data = None
            if not isinstance(data, dict):
                data = {'raw': r['evidence']}
            data['status'] = r['status']
            score = r['score'] if isinstance(r['score'], (int, float)) and math.isfinite(r['score']) else None
            score = float(score) if score is not None else None
            version = 'v4/' + hashlib.sha256(canonical({'score': score, 'data': data}).encode()).hexdigest()[:12]
            evidence.append({
                'id': f"{r['asset']}:{r['artist_id']}",
                'asset': {'namespace': NAMESPACE, 'id': r['asset']},
                'term': _own(r['artist_id']),
                'source': PRODUCER, 'method': r['method'], 'version': version,
                'score': None if score is None else {'value': score, 'metric': METRICS.get(r['method'], f"{r['method'].lower()}_score")},
                'observed_at': _observed(r['updated_at'], now), 'data': data,
            })
        for r in c.execute('SELECT id,merged_into FROM artists WHERE merged_into IS NOT NULL ORDER BY id'):
            evidence.append({'id': f"merge:{r['id']}", 'relation': {'kind': 'same_as', 'a': _own(r['id']), 'b': _own(r['merged_into'])},
                             'source': PRODUCER, 'method': 'merge', 'version': 'v4', 'score': None, 'observed_at': now, 'data': {}})
        links, names = {}, {}
        for r in c.execute('SELECT system,key,artist_id,url FROM artist_links ORDER BY system,key'):
            link = {'system': r['system'], 'key': r['key']}
            if r['url']:
                link['url'] = r['url']
            links.setdefault(r['artist_id'], []).append(link)
        for r in c.execute('SELECT artist_id,source,name FROM artist_names ORDER BY artist_id,source'):
            names.setdefault(r['artist_id'], []).append({'value': r['name'], 'source': r['source']})
        terms = [{'term': _own(a), 'links': links.get(a, []), 'names': names.get(a, [])}
                 for (a,) in c.execute('SELECT id FROM artists ORDER BY id')]
    finally:
        c.close()
    # A full snapshot: every picture in the library counts as completed, so one whose candidates are gone (the groups
    # step recomputes them each run) is 查无结果 and the core drops them (native/terms/src/store.rs publish_snapshot).
    assets = [{'asset': {'namespace': NAMESPACE, 'id': u}, 'status': 'completed', 'error': None} for u in sorted(present)]
    return assets, terms, evidence


def write(library, out_dir):
    """Write a snapshot package into `out_dir` (created; must not exist yet). Returns its path."""
    out = Path(out_dir)
    out.mkdir(parents=True)
    assets, terms, evidence = rows(library)
    files = {}
    for name, items in (('evidence.jsonl', evidence), ('terms.jsonl', terms)):
        body = ''.join(canonical(x) + '\n' for x in items).encode()
        (out / name).write_bytes(body)
        files[name] = {'bytes': len(body), 'sha256': hashlib.sha256(body).hexdigest()}
    manifest = {'format': FORMAT, 'library_id': library.config['id'],
                'producer': {'name': PRODUCER, 'version': f'db-v{db.VERSION}'},
                'created_at': time.strftime('%Y-%m-%dT%H:%M:%SZ', time.gmtime()),
                'assets': assets, 'files': files}
    tmp = out / f'.manifest.{os.getpid()}'
    tmp.write_text(canonical(manifest) + '\n', encoding='utf-8')
    tmp.replace(out / 'manifest.json')           # the manifest last: a half-written package never validates
    return out
