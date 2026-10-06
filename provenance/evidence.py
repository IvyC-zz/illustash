"""Shared candidate evidence, bounded retry metadata, and expiring JSON response cache."""
import json
from provenance.net import Stopped


def due(c, target, backend):
    row = c.execute("SELECT next_due_at IS NULL OR next_due_at<=datetime('now') FROM lookups WHERE target=? AND backend=?", (target, backend)).fetchone()
    return row is None or bool(row[0])


def retry_in(c, target, backend):
    """Minutes until a lookup that last failed may be tried again; None when it did not fail or is due."""
    row = c.execute("""SELECT (julianday(next_due_at) - julianday('now')) * 1440 FROM lookups
                       WHERE target=? AND backend=? AND last_result='error' AND next_due_at>datetime('now')""", (target, backend)).fetchone()
    return max(1, round(row[0])) if row else None


def waiting(minutes):
    return f'上次请求出错，约 {minutes} 分钟后自动重试'


def record(c, target, backend, result):
    delay = '+1 hour' if result == 'error' else '+7 days' if result == 'hit' else '+1 day'
    c.execute("""INSERT INTO lookups(target,backend,attempts,last_at,last_result,next_due_at)
        VALUES (?,?,1,datetime('now'),?,datetime('now',?)) ON CONFLICT(target,backend) DO UPDATE SET
        attempts=attempts+1,last_at=excluded.last_at,last_result=excluded.last_result,next_due_at=excluded.next_due_at""",
        (target, backend, result, delay))


def cached(c, net, url, params=None):
    if not getattr(net, 'authorized', False):
        raise Stopped('外部请求未授权')
    key = url + ':' + json.dumps(params or {}, sort_keys=True)
    row = c.execute("SELECT body FROM ext_cache WHERE key=? AND fetched_at>datetime('now','-7 days')", (key,)).fetchone()
    if row:
        return json.loads(row[0])
    result = net.get(url, params)
    if result is None or (isinstance(result, dict) and result.get('error')):
        raise ConnectionError('上游未返回有效结果')
    with c:
        c.execute("INSERT OR REPLACE INTO ext_cache VALUES (?,?,datetime('now'))", (key, json.dumps(result)))
    return result


def seed(c, artist, source, evidence):
    c.execute('INSERT OR REPLACE INTO seeds(artist_id,source,evidence) VALUES (?,?,?)',
              (artist, source, json.dumps({'version': 1, 'source': source, **evidence}, ensure_ascii=False)))


def nickname(c, artist, name, source):
    if isinstance(name, str) and name.strip():
        c.execute("""INSERT INTO artist_names(artist_id,source,name) VALUES (?,?,?)
            ON CONFLICT(artist_id,source) DO UPDATE SET name=excluded.name,updated_at=CURRENT_TIMESTAMP""",
            (artist, source, name.strip()[:300]))
