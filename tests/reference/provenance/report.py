"""How much of a library has an artist, by preset and by the seed source that led there. No network."""
from collections import Counter

from provenance import assets, db


def run(library, net=None, limit=None, log=print):
    pictures = {p['asset']: p['preset'] for p in assets.pictures(library, ('x', 'pixiv', 'other'))}
    with db.connect(library) as c:
        attributed = {}
        for r in c.execute("SELECT asset, artist_id, method FROM image_artists WHERE status='auto'"):
            attributed.setdefault(r['asset'], set()).add(r['artist_id'])
        seeds = {}
        for r in c.execute('SELECT artist_id, source FROM seeds'):
            seeds.setdefault(r['artist_id'], set()).add(r['source'])
        posts = {r['asset'] for r in c.execute("SELECT DISTINCT asset FROM image_sources WHERE site IN ('pixiv','x')")}
    total = Counter(pictures.values())
    have = Counter(pictures[a] for a in attributed if a in pictures)
    by_source = Counter()
    for a, artists in attributed.items():
        if a in pictures:
            for s in {s for artist in artists for s in seeds.get(artist, ())}:
                by_source[s] += 1
    out = {'pictures': dict(total), 'with_artist': {k: f'{have[k]} ({have[k] / total[k]:.1%})' for k in total if total[k]},
           'with_original_post': f'{len(posts & pictures.keys())} ({len(posts & pictures.keys()) / max(1, len(pictures)):.1%})',
           'by_seed_source': dict(by_source.most_common()),
           'conflicts': sum(1 for a, s in attributed.items() if len(s) > 1 and a in pictures),
           # the same, as numbers for the app (settings page 找回画师)
           'numbers': {'pictures': dict(total), 'with_artist': dict(have), 'with_original_post': len(posts & pictures.keys())}}
    return out
