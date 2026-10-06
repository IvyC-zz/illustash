"""One hop of Pixiv's related users from every verified artist with a Pixiv account (docs/PROVENANCE.md section 8).

Measured 2026-09-28: 10 related users from each of 5 verified artists, 19 of 50 had pictures in the library, 59 new
pictures. Each run expands the artists verified since the last one, so repeated runs walk further out and stop by
themselves when verification finds nothing new. Needs a bound Pixiv session (the related list is empty without one).
"""
import json

from provenance import accounts, db
from provenance.net import BudgetExceeded, Stopped

PER_ARTIST = 10


def run(library, net, limit=None, log=print):
    counts = {'expanded': 0, 'candidates': 0, 'stopped': ''}
    if not accounts.pixiv_user_id(net):   # the related list is empty without a login
        counts['stopped'] = '需要绑定 Pixiv 账号'
        return counts
    c = db.connect(library)
    try:
        todo = [(r['artist_id'], r['key']) for r in c.execute(
            """SELECT l.artist_id, l.key FROM artist_links l
               JOIN lookups v ON v.target = 'artist:' || l.artist_id AND v.backend = 'verify' AND v.last_result = 'hit'
               WHERE l.system = 'pixiv' AND NOT EXISTS (SELECT 1 FROM lookups x WHERE x.target = 'artist:' || l.artist_id
                     AND x.backend = 'pixiv_related' AND x.last_result IN ('hit','miss'))""")][:limit]
        for artist, uid in todo:
            try:
                body = (net.get(f'https://www.pixiv.net/ajax/user/{uid}/recommends',
                                {'userNum': PER_ARTIST, 'workNum': 3, 'isR18': 'true', 'lang': 'ja'}) or {}).get('body') or {}
            except (Stopped, BudgetExceeded) as exc:
                counts['stopped'] = str(exc)
                break
            except ConnectionError:
                continue
            related = [str(u['userId']) for u in body.get('recommendUsers', []) if str(u.get('userId')) != uid][:PER_ARTIST]
            with c:
                for other in related:
                    candidate = db.artist_for(c, [('pixiv', other, f'https://www.pixiv.net/users/{other}')], 'pixiv_related')
                    c.execute("INSERT OR IGNORE INTO seeds(artist_id,source,evidence) VALUES (?, 'pixiv_related', ?)",
                              (candidate, json.dumps({'from': artist})))
                c.execute("""INSERT INTO lookups(target,backend,attempts,last_at,last_result) VALUES (?, 'pixiv_related', 1, datetime('now'), ?)
                             ON CONFLICT(target,backend) DO UPDATE SET attempts=attempts+1,last_at=excluded.last_at,last_result=excluded.last_result""",
                          ('artist:' + artist, 'hit' if related else 'miss'))
            counts['expanded'] += 1
            counts['candidates'] += len(related)
            if counts['expanded'] % 20 == 0:
                log(f"Pixiv 相关画师：{counts['expanded']}/{len(todo)}，新候选 {counts['candidates']} 个")
    finally:
        c.close()
    return counts
