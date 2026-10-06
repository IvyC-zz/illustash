"""Refresh Pixiv display names independently of account identity; no name-based merging."""
from provenance import db, evidence
from provenance.net import BudgetExceeded, NotAsked, Stopped


def run(library, net, limit=None, log=print):
    counts = dict(updated=0, failed=0, stopped='')
    if not getattr(net, 'authorized', False):
        return {**counts, 'stopped': '外部请求未授权'}
    c = db.connect(library)
    try:
        rows = c.execute("SELECT artist_id,key FROM artist_links WHERE system='pixiv' ORDER BY artist_id").fetchall()
        waits = [m for r in rows if (m := evidence.retry_in(c, 'artist:' + r['artist_id'], 'nicknames'))]
        rows = [r for r in rows if evidence.due(c, 'artist:' + r['artist_id'], 'nicknames')][:limit]
        if waits and not rows:
            counts['stopped'] = f'Pixiv 昵称：{len(waits)} 位画师' + evidence.waiting(min(waits))
        for artist, key in rows:
            try:
                result = evidence.cached(c, net, f'https://www.pixiv.net/ajax/user/{key}', {'lang': 'ja'})
                body = result.get('body') or {}
                name = body.get('name')
                if not isinstance(name, str) or not name.strip():
                    raise ConnectionError('昵称响应无效')
                with c:
                    evidence.nickname(c, artist, name, 'pixiv')
                    evidence.record(c, 'artist:' + artist, 'nicknames', 'hit')
                counts['updated'] += 1
                if counts['updated'] % 20 == 0:
                    log(f"Pixiv 昵称：{counts['updated']}/{len(rows)}")
            except (Stopped, BudgetExceeded, ConnectionError) as exc:
                if isinstance(exc, (NotAsked, BudgetExceeded)):   # never asked: no back-off, not a failure
                    counts['stopped'] = str(exc)
                    break
                with c:
                    evidence.record(c, 'artist:' + artist, 'nicknames', 'error')
                counts['failed'] += 1
                if isinstance(exc, (Stopped, BudgetExceeded)):
                    counts['stopped'] = str(exc)
                    break
    finally:
        c.close()
    return counts
