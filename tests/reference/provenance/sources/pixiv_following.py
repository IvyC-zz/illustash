"""Read the bound account's public following list; follows are leads, never image attributions."""
from provenance import accounts, db, evidence
from provenance.net import BudgetExceeded, NotAsked, Stopped


def run(library, net, limit=None, log=print):
    counts = dict(candidates=0, pages=0, stopped='')
    if not getattr(net, 'authorized', False):
        return {**counts, 'stopped': '外部请求未授权'}
    if not (uid := accounts.pixiv_user_id(net)):
        return {**counts, 'stopped': '需要绑定 Pixiv 账号'}
    c = db.connect(library)
    target = 'following:' + uid
    try:
        if not evidence.due(c, target, 'pixiv_following'):   # listed within the week, or backing off after an error
            if (minutes := evidence.retry_in(c, target, 'pixiv_following')):
                counts['stopped'] = 'Pixiv 关注：' + evidence.waiting(minutes)
            return counts
        row = c.execute("SELECT cursor FROM lookups WHERE target=? AND backend='pixiv_following'", (target,)).fetchone()
        offset = int(row[0] or 0) if row else 0
        while limit is None or counts['candidates'] < limit:
            try:
                result = evidence.cached(c, net, f'https://www.pixiv.net/ajax/user/{uid}/following',
                    {'offset': offset, 'limit': 100, 'rest': 'show', 'tag': '', 'lang': 'ja'})
                body = result.get('body')
                if not isinstance(body, dict) or not isinstance(body.get('users'), list):
                    raise ConnectionError('关注列表响应格式无效')
                users = body['users']
                take = users if limit is None else users[:max(0, limit - counts['candidates'])]
                with c:
                    for user in take:
                        key = str(user.get('userId', ''))
                        if not key.isdecimal():
                            raise ValueError('Pixiv 用户 ID 无效')
                        artist = db.artist_for(c, [('pixiv', key, f'https://www.pixiv.net/users/{key}')], 'pixiv_following')
                        evidence.seed(c, artist, 'pixiv_following', {'account': uid, 'user_id': key})
                        evidence.nickname(c, artist, user.get('userName'), 'pixiv')
                    offset += len(take)
                    evidence.record(c, target, 'pixiv_following', 'hit' if take else 'miss')
                    complete = not users or offset >= int(body.get('total', offset + 1))
                    c.execute("UPDATE lookups SET cursor=?,next_due_at=CASE WHEN ? THEN next_due_at ELSE NULL END WHERE target=? AND backend='pixiv_following'",
                              (None if complete else str(offset), complete, target))
                counts['pages'] += 1
                counts['candidates'] += len(take)
                if complete:
                    break
            except (Stopped, BudgetExceeded, ConnectionError, ValueError) as exc:
                if not isinstance(exc, (NotAsked, BudgetExceeded)):   # never asked: no back-off (2026-09-28 run 12)
                    with c:
                        evidence.record(c, target, 'pixiv_following', 'error')
                counts['stopped'] = str(exc)
                break
    finally:
        c.close()
    return counts
