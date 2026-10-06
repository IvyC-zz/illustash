"""Work identities and arbitrary-depth trees; no network or image writes."""
import uuid
from . import names

# UNION (not UNION ALL) also terminates on damaged legacy cycles.
TREE = """WITH RECURSIVE tree(id) AS (SELECT id FROM entities WHERE id=? AND kind='work'
 UNION SELECT e.id FROM entities e JOIN tree t ON e.parent_id=t.id WHERE e.kind='work')"""


def require(c, ident):
    row = c.execute("SELECT * FROM entities WHERE id=? AND kind='work' AND merged_into IS NULL", (ident,)).fetchone()
    if not row:
        raise ValueError('作品不存在')
    return row


def descendants(c, ident):
    require(c, ident)
    return [r[0] for r in c.execute(TREE + ' SELECT id FROM tree', (ident,))]


def migrate(c):
    columns = {r[1] for r in c.execute('PRAGMA table_info(entities)')}
    if 'work_id' not in columns:
        c.execute('ALTER TABLE entities ADD COLUMN work_id TEXT REFERENCES entities(id)')
    if 'original' not in columns:
        c.execute('ALTER TABLE entities ADD COLUMN original INTEGER NOT NULL DEFAULT 0')
    c.execute('CREATE INDEX IF NOT EXISTS entities_work_id ON entities(work_id)')
    c.execute("""CREATE TABLE IF NOT EXISTS work_review(entity_id TEXT PRIMARY KEY REFERENCES entities(id),
        status TEXT NOT NULL DEFAULT 'pending',note TEXT NOT NULL DEFAULT '',parent_locked INTEGER NOT NULL DEFAULT 0)""")
    c.execute("UPDATE entities SET original=1 WHERE kind='oc'")
    c.execute("UPDATE entities SET kind=CASE WHEN parent_id IS NULL THEN 'character' ELSE 'skin' END WHERE kind<>'work'")
    for (key,) in c.execute("SELECT DISTINCT work FROM entities WHERE work<>'' AND kind<>'work'").fetchall():
        existing = c.execute("SELECT entity_id FROM links WHERE system='qualifier' AND key=?", (key,)).fetchone()
        ident = existing[0] if existing else str(uuid.uuid4())
        if not existing:
            c.execute("INSERT INTO entities(id,name,kind) VALUES (?,'','work')", (ident,))
            c.execute("INSERT INTO links(system,key,entity_id,source) VALUES ('qualifier',?,?,'import')", (key, ident))
            # Retain the old readable label as an index name, never pretend the qualifier is a remote tag.
            names.put_name(c, ident, 'en', 'index', names.readable(key))
        c.execute('INSERT OR IGNORE INTO work_review(entity_id) VALUES (?)', (ident,))
        for lang, source, value in c.execute("SELECT lang,source,value FROM display_names WHERE kind='work' AND key=?", (key,)).fetchall():
            names.put_name(c, ident, lang, source, value)
        for value, pos in c.execute("SELECT value,position FROM name_aliases WHERE kind='work' AND key=?", (key,)).fetchall():
            names.put_name(c, ident, '', 'danbooru', value, role='alias', position=pos)
        c.execute("UPDATE entities SET work_id=? WHERE work=? AND kind='character' AND original=0", (ident, key))
    # The old string remains a compatibility rendering key, never an identity lookup.
    c.execute("UPDATE entities SET work='',work_id=NULL WHERE original=1")
    c.execute("UPDATE entities SET work_id=NULL WHERE kind='skin'")


def rows(c, visible, query=''):
    result = []
    for r in c.execute("""SELECT e.id,e.parent_id,entity_name(e.id) AS name,
        (SELECT key FROM links WHERE entity_id=e.id AND system='danbooru' AND current=1 LIMIT 1) AS tag,
        (SELECT key FROM links WHERE entity_id=e.id AND system='qualifier' LIMIT 1) AS qualifier,
        coalesce(w.status,'pending') AS status,w.note
        FROM entities e LEFT JOIN work_review w ON w.entity_id=e.id WHERE e.kind='work' AND e.merged_into IS NULL""").fetchall():
        item = dict(r)
        item['count'] = c.execute(TREE + f""" SELECT count(DISTINCT m.uuid) FROM memberships m
            JOIN entities e ON e.id=m.entity WHERE e.work_id IN (SELECT id FROM tree) AND {visible}""", (r['id'],)).fetchone()[0]
        item['watchable'] = bool(item['tag'])
        values = [item['name'], item['tag'] or '', item['qualifier'] or '']
        values += [n[0] for n in c.execute('SELECT value FROM names WHERE entity_id=?', (r['id'],))]
        needle = names.normalize(query.strip())
        if not needle or any(needle in names.normalize(v.replace('_', ' ')) or needle in names.normalize(v) for v in values):
            result.append(item)
    return sorted(result, key=lambda r: (-r['count'], r['name'], r['id']))
