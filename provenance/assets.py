"""Read-only views of the library's own databases: which pictures to look at and where their original bytes are."""
import sqlite3
from pathlib import Path


def open_source(library):
    c = sqlite3.connect(Path(library.source).resolve().as_uri() + '?mode=ro', uri=True, timeout=30)
    c.row_factory = sqlite3.Row
    return c


def pictures(library, presets=('x', 'pixiv')):
    """(asset, preset, source_id, src_path) of the pictures still in the library from these presets."""
    with open_source(library) as c:
        q = ','.join('?' * len(presets))
        return [dict(r) for r in c.execute(
            f"""SELECT uuid AS asset, preset, source_id, src_path FROM assets
                WHERE in_scope=1 AND coalesce(trashed,0)=0 AND preset IN ({q})""", presets)]


def original(library, picture):
    """The original file: pixel hashes need the bytes as saved, not Photos' resized derivative."""
    source = next((s for s in library.config['sources'] if s['id'] == picture['source_id']), None)
    if source and source.get('kind') == 'photos':
        folder = Path(source['path']).expanduser() / 'originals' / picture['asset'][0]
        found = sorted(folder.glob(picture['asset'] + '.*'))
        return found[0] if found else None
    return Path(picture['src_path']) if picture['src_path'] else None
