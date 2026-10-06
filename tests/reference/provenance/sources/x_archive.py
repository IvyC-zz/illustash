"""X following list from the official data archive (X › Settings › Download an archive of your data).

X's API is too expensive for a personal library, so an account is "bound" by importing its archive. Only
data/following.js is read, and only the numeric account ids are kept: direct messages, tweets and everything else in
the archive are never opened. Each id is resolved to an artist through the X account link Danbooru artist pages
register (https://twitter.com/intent/user?user_id=…, which Danbooru normalises with x.com/i/user/…). Only the id is sent.
"""
import json
import zipfile
from pathlib import Path

from provenance import db
from provenance.net import BudgetExceeded, Stopped

RETRY_DAYS = 30


def _following_js(path):
    path = Path(path).expanduser()
    if path.is_file() and zipfile.is_zipfile(path):
        with zipfile.ZipFile(path) as z:
            name = next((n for n in z.namelist() if n.endswith('data/following.js')), None)
            if not name:
                raise ValueError('归档里没有 data/following.js')
            return z.read(name).decode('utf-8')
    found = sorted(path.glob('**/data/following.js')) if path.is_dir() else []
    if not found:
        raise ValueError('没有找到 data/following.js（请选择 X 数据归档的 zip 或解压后的文件夹）')
    return found[0].read_text(encoding='utf-8')


def following_ids(path):
    text = _following_js(path)
    return [str(x['following']['accountId']) for x in json.loads(text[text.index('=') + 1:]) if x.get('following', {}).get('accountId')]


def import_archive(library, path):
    """Record the following list as leads; ids already known stay as they are. Returns how many are new."""
    ids = following_ids(path)
    with db.connect(library) as c:
        before = c.execute("SELECT count(*) FROM leads WHERE source='x_following'").fetchone()[0]
        c.executemany("INSERT OR IGNORE INTO leads(source,system,key) VALUES ('x_following','x_id',?)", [(i,) for i in ids])
        return c.execute("SELECT count(*) FROM leads WHERE source='x_following'").fetchone()[0] - before


def run(library, net, limit=None, log=print):
    counts = {'asked': 0, 'resolved': 0, 'unresolved': 0, 'stopped': ''}
    c = db.connect(library)
    try:
        todo = [r['key'] for r in c.execute("""SELECT key FROM leads WHERE source='x_following' AND
                   (status='pending' OR (status='unresolved' AND next_due_at <= datetime('now'))) ORDER BY added_at, key""")][:limit]
        for key in todo:
            try:
                found = net.get('https://danbooru.donmai.us/artists.json',
                                {'search[url_matches]': f'https://twitter.com/intent/user?user_id={key}', 'only': 'id,name,is_banned'})
            except (Stopped, BudgetExceeded) as exc:
                counts['stopped'] = str(exc)
                break
            except ConnectionError:
                continue
            artists = [a for a in (found or []) if not a.get('is_banned')]
            with c:
                if len(artists) == 1:
                    name = artists[0]['name']
                    artist = db.artist_for(c, [('danbooru', name, f'https://danbooru.donmai.us/artists/{artists[0]["id"]}'),
                                               ('x_id', key, f'https://x.com/i/user/{key}')], 'x_following')
                    c.execute("UPDATE leads SET status='resolved',artist_id=?,next_due_at=NULL WHERE source='x_following' AND key=?", (artist, key))
                    c.execute("INSERT OR IGNORE INTO seeds(artist_id,source,evidence) VALUES (?, 'x_following', ?)",
                              (artist, json.dumps({'x_id': key})))
                    counts['resolved'] += 1
                else:   # none, or ambiguous: ask again later rather than guess
                    c.execute(f"UPDATE leads SET status='unresolved',next_due_at=datetime('now','+{RETRY_DAYS} days') WHERE source='x_following' AND key=?", (key,))
                    counts['unresolved'] += 1
            counts['asked'] += 1
            if counts['asked'] % 100 == 0:
                log(f"X 关注：已查 {counts['asked']}/{len(todo)}，对上 {counts['resolved']} 位画师")
    finally:
        c.close()
    return counts
