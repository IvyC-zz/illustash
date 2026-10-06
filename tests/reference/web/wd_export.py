"""WD ratings and general tags as evidence packages for the terms core (docs/TERMS_LABELS.md, docs/EVIDENCE_PACKAGES.md).

source.sqlite keeps every raw WD score (about 20 million rows); the core keeps what its type policies can use: every
rating, and general tags at WD >= 0.2 (topic keep floor, native/terms/src/schema.rs). Packages are cut per `chunk`
pictures so that neither side holds millions of rows in memory; each one imports in its own transaction.

Terms are referenced by link: topics as ('wd', tag), ratings as ('wd_rating', value). Not 'danbooru': a Danbooru
artist link and a WD general tag must never resolve to the same term.
"""
import functools
import hashlib
import json
from pathlib import Path
import sqlite3
import time

FORMAT = 'illustash-evidence/1'
PRODUCER = 'wd'
NAMESPACE = 'library'
TOPIC_FLOOR = 0.2
KINDS = {'general': ('topic', 'wd'), 'rating': ('rating', 'wd_rating')}


def _ro(path):
    c = sqlite3.connect(Path(path).resolve().as_uri() + '?mode=ro', uri=True, timeout=30)
    c.row_factory = sqlite3.Row
    return c


@functools.lru_cache(maxsize=4096)   # one inference run stamps many rows alike
def _observed(ts, fallback):
    try:
        return time.strftime('%Y-%m-%dT%H:%M:%SZ', time.strptime(ts, '%Y-%m-%d %H:%M:%S'))
    except (TypeError, ValueError):
        return fallback


def _rows(src, uuids, now):
    marks = ','.join('?' * len(uuids))
    # Older sources (and test fixtures) lack the provenance columns: which model run said so.
    have = {r[1] for r in src.execute('PRAGMA table_info(labels)')}
    column = lambda name, default: name if name in have else f"{default} AS {name}"
    extra = ','.join((column('source', "'wd'"), column('model_ver', 'NULL'), column('ts', 'NULL')))
    for r in src.execute(f"""SELECT uuid,dim,value,conf,{extra} FROM labels
            WHERE uuid IN ({marks}) AND (+dim='rating' OR (+dim='general' AND +conf>=?)) ORDER BY uuid,dim,value,source""",
                         (*uuids, TOPIC_FLOOR)):
        if r['conf'] is None:
            continue
        kind, system = KINDS[r['dim']]
        yield {'id': f"{r['uuid']}:{r['dim']}:{r['value']}:{r['source']}",
               'asset': {'namespace': NAMESPACE, 'id': r['uuid']},
               'term': {'kind': kind, 'link': {'system': system, 'key': r['value']}},
               'source': PRODUCER, 'method': r['source'], 'version': r['model_ver'] or 'unknown',
               'score': {'value': float(r['conf']), 'metric': 'wd_probability'},
               'observed_at': _observed(r['ts'], now), 'data': {}}


def write(source, library_id, uuids, out_dir):
    """One package with the WD evidence of these pictures (those without any WD result are left out)."""
    out = Path(out_dir)
    out.mkdir(parents=True)
    now = time.strftime('%Y-%m-%dT%H:%M:%SZ', time.gmtime())
    seen, n = [], 0
    digest = hashlib.sha256()
    with _ro(source) as src, (out / 'evidence.jsonl').open('wb') as f:
        for row in _rows(src, list(uuids), now):
            line = (json.dumps(row, ensure_ascii=False, separators=(',', ':'), allow_nan=False) + '\n').encode()
            f.write(line)
            digest.update(line)
            n += len(line)
            if not seen or seen[-1] != row['asset']['id']:
                seen.append(row['asset']['id'])
    manifest = {'format': FORMAT, 'library_id': library_id, 'producer': {'name': PRODUCER, 'version': 'labels'},
                'created_at': now,
                'assets': [{'asset': {'namespace': NAMESPACE, 'id': u}, 'status': 'completed', 'error': None} for u in seen],
                'files': {'evidence.jsonl': {'bytes': n, 'sha256': digest.hexdigest()}}}
    (out / 'manifest.json').write_text(json.dumps(manifest, ensure_ascii=False) + '\n', encoding='utf-8')
    return out, len(seen)


def packages(source, library_id, uuids, out_dir, chunk=1000):
    """Write packages for `uuids` in chunks under out_dir; yields each package directory."""
    uuids = sorted(uuids)
    for i in range(0, len(uuids), chunk):
        path, count = write(source, library_id, uuids[i:i + chunk], Path(out_dir) / f'wd-{i // chunk:05d}')
        if count:
            yield path
