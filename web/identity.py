"""Evidence-first identity relations and two-level (person / variant) recognition.

Raw IDs, assertions, candidates and links never move during a merge. canonical is a rebuildable
projection. Existing pre-relation merges retain their legacy baseline.

The relation layer runs under one policy, 'hybrid' (meta identity_policy; chosen by the user 2026-09-25): WD / CCIP /
PROTO machine results stay formal memberships ("machine results count, people remove the wrong ones"); ambiguous ones
are flagged by the store's ambiguous_claims view instead of withdrawn. The strict policy 'v2' (only calibrated
identity_claims become machine memberships) was abandoned 2026-09-30; a library still marked 'v2'
is converted to hybrid by the store's migration.
"""
import hashlib
import json
from pathlib import Path
import sqlite3

SCHEMA = '''
CREATE TABLE IF NOT EXISTS identity_relations(
 id INTEGER PRIMARY KEY,a TEXT NOT NULL REFERENCES entities(id),b TEXT NOT NULL REFERENCES entities(id),
 kind TEXT NOT NULL CHECK(kind IN ('duplicate_of','form_of','different_from')),
 status TEXT NOT NULL DEFAULT 'proposed' CHECK(status IN ('proposed','accepted','rejected')),
 UNIQUE(a,b,kind),CHECK(a<>b));
CREATE TABLE IF NOT EXISTS identity_evidence(
 relation_id INTEGER NOT NULL REFERENCES identity_relations(id),fingerprint TEXT NOT NULL,
 source TEXT NOT NULL,payload TEXT NOT NULL,created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
 PRIMARY KEY(relation_id,fingerprint));
CREATE TABLE IF NOT EXISTS identity_decisions(
 id INTEGER PRIMARY KEY,relation_id INTEGER NOT NULL REFERENCES identity_relations(id),
 previous_status TEXT NOT NULL,status TEXT NOT NULL,reason TEXT NOT NULL,
 created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);
CREATE TABLE IF NOT EXISTS identity_decision_evidence(decision_id INTEGER NOT NULL,fingerprint TEXT NOT NULL,
 PRIMARY KEY(decision_id,fingerprint));
CREATE TABLE IF NOT EXISTS identity_baseline(
 id TEXT PRIMARY KEY,parent_id TEXT,variant TEXT,kind TEXT,merged_into TEXT);
CREATE TABLE IF NOT EXISTS identity_canonical(entity TEXT PRIMARY KEY,canonical TEXT NOT NULL);
CREATE INDEX IF NOT EXISTS identity_canonical_root ON identity_canonical(canonical,entity);
CREATE TABLE IF NOT EXISTS identity_scopes(uuid TEXT NOT NULL,entity TEXT NOT NULL,members_json TEXT NOT NULL,
 PRIMARY KEY(uuid,entity));
CREATE TABLE IF NOT EXISTS identity_suspended(uuid TEXT NOT NULL,entity TEXT NOT NULL,reason TEXT NOT NULL,
 options_json TEXT NOT NULL,PRIMARY KEY(uuid,entity));
CREATE TABLE IF NOT EXISTS identity_uncertain(uuid TEXT PRIMARY KEY,options_json TEXT NOT NULL,reason TEXT NOT NULL,
 created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);
CREATE TABLE IF NOT EXISTS identity_anchor_decisions(entity TEXT NOT NULL,reference TEXT NOT NULL,
 decision TEXT NOT NULL CHECK(decision IN ('exclude','restore')),reason TEXT NOT NULL,
 created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,PRIMARY KEY(entity,reference));
CREATE TABLE IF NOT EXISTS identity_anchor_history(id INTEGER PRIMARY KEY,entity TEXT NOT NULL,reference TEXT NOT NULL,
 decision TEXT NOT NULL,reason TEXT NOT NULL,created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);
CREATE TABLE IF NOT EXISTS identity_pairs(a TEXT NOT NULL,b TEXT NOT NULL,original_distance REAL,
 distance REAL,diagnosis TEXT NOT NULL,bank_version TEXT NOT NULL,evidence_json TEXT NOT NULL,PRIMARY KEY(a,b));
CREATE VIEW IF NOT EXISTS identity_nodes AS
 SELECT e.id,CASE WHEN e.parent_id IS NULL THEN 'character' ELSE 'variant' END AS node_type,
 e.parent_id,coalesce(m.canonical,e.id) AS canonical_id,
 coalesce(p.canonical,e.parent_id,m.canonical,e.id) AS character_id,
 CASE WHEN e.parent_id IS NOT NULL THEN coalesce(m.canonical,e.id) END AS variant_id
 FROM entities e LEFT JOIN identity_canonical m ON m.entity=e.id
 LEFT JOIN identity_canonical p ON p.entity=e.parent_id WHERE e.kind<>'work';
'''

# 「1girl + 1boy」without a multi-person tag (lead_assets) is one named character with a nameless man (pov, Sensei,
# faceless_male...). Measured 2026-09-26 on reviewed images: every one had a single named character; runner-up claims
# with weak evidence were wrong 78/78, with strong evidence (web/store.py STRONG_WD / STRONG_CCIP) right 11/13.
# So a weak claim there counts only as the top candidate, and not beside a confirmed other character.
# A mascot (entities.mascot, the user's choice: 涂鸦老师, 佩洛洛…) is ranked after every character, so it never takes
# the single-person slot. On one-character images it rides along as an extra only on strong evidence: measured
# 2026-09-26, 616 of 1,059 doodle-sensei claims there were weak, and it had 36 human rejections.
NOT_MASCOT = '''NOT (EXISTS(SELECT 1 FROM entities x WHERE x.id=v.entity AND x.mascot=1)
  AND (coalesce(v.wd,0)>=0.9 OR coalesce(v.ccip,0)>=0.95))'''
LEAD_RULE = '''AND NOT (EXISTS(SELECT 1 FROM lead_assets o WHERE o.uuid=v.uuid) AND ''' + NOT_MASCOT + '''
  AND coalesce(v.wd,0)<0.9 AND coalesce(v.ccip,0)<0.95
  AND (v.rank>1 OR EXISTS(SELECT 1 FROM family_confirmed f WHERE f.uuid=v.uuid AND f.entity<>v.entity)))'''

# A claim only CCIP makes, at a low score, is a lead (「也可能是」), not an owner. 随机抽查 round 1:
# only-CCIP claims were right 45% of the time; below this score 4 of 37 were right, confirmed on answers given after
# the line was drawn. The evidence stays in family_evidence, so these still show as leads and can be asked about.
WEAK_CCIP_BELOW = 0.925
WEAK_CCIP = f"AND NOT (v.sources='CCIP' AND coalesce(v.ccip,0)<{WEAK_CCIP_BELOW})"

# The machine rule over person-level evidence (family_evidence groups by character_id).
# Rejecting a variant never rejects the person; "暂不判断" (identity_uncertain) takes the image out.
HYBRID_MACHINE = '''SELECT v.uuid,v.entity,'machine','machine',v.sources,v.score,v.source_count,v.ccip_only
 FROM family_evidence v
 WHERE NOT EXISTS(SELECT 1 FROM effective_assertions s WHERE s.uuid=v.uuid AND s.character_id=v.entity AND s.variant_id IS NULL AND s.status='rejected')
 AND NOT EXISTS(SELECT 1 FROM identity_uncertain u WHERE u.uuid=v.uuid)
 AND NOT EXISTS(SELECT 1 FROM identity_suspended h WHERE h.uuid=v.uuid)
 AND NOT EXISTS(SELECT 1 FROM family_confirmed f WHERE f.uuid=v.uuid AND f.entity=v.entity)
 AND NOT (v.ccip_only=1 AND EXISTS(SELECT 1 FROM family_confirmed f WHERE f.uuid=v.uuid AND f.entity<>v.entity))
 AND NOT (EXISTS(SELECT 1 FROM single_assets o WHERE o.uuid=v.uuid) AND ''' + NOT_MASCOT + ''' AND
  (v.rank>1 OR EXISTS(SELECT 1 FROM family_confirmed f WHERE f.uuid=v.uuid AND f.entity<>v.entity)))
 ''' + LEAD_RULE + WEAK_CCIP

# Raw assertions remain available for audit. Suspended group-scoped assertions and
# contradictory verdicts in an equivalence group cannot become training labels.
VIEWS = '''
CREATE VIEW effective_assertions AS
 SELECT s.uuid,n.canonical_id AS entity,n.character_id,n.variant_id,s.status,s.origin,s.entity AS raw_entity
 FROM assertions s JOIN identity_nodes n ON n.id=s.entity JOIN assets a ON a.uuid=s.uuid
 WHERE NOT EXISTS(SELECT 1 FROM identity_suspended h WHERE h.uuid=s.uuid AND h.entity=s.entity)
 AND NOT EXISTS(SELECT 1 FROM identity_uncertain u WHERE u.uuid=s.uuid);
CREATE VIEW family_confirmed AS
 SELECT DISTINCT s.uuid,s.character_id AS entity FROM effective_assertions s
 WHERE s.status='confirmed' AND NOT EXISTS(SELECT 1 FROM effective_assertions r
 WHERE r.uuid=s.uuid AND r.status='rejected' AND
 (r.entity=s.entity OR (r.character_id=s.character_id AND r.variant_id IS NULL)));
CREATE VIEW membership_rules AS
 SELECT f.uuid,f.entity,'confirmed' AS status,
 (SELECT s.origin FROM effective_assertions s WHERE s.uuid=f.uuid AND s.character_id=f.entity
 AND s.status='confirmed' ORDER BY s.variant_id IS NOT NULL,s.origin LIMIT 1) AS origin,
 coalesce(v.sources,'') AS sources,v.score,coalesce(v.source_count,0) AS source_count,coalesce(v.ccip_only,0) AS ccip_only
 FROM family_confirmed f LEFT JOIN family_evidence v ON v.uuid=f.uuid AND v.entity=f.entity
 UNION ALL
 {machine};
{variants};
CREATE VIEW identity_assignments AS
 SELECT m.uuid,m.entity AS character_id,v.entity AS variant_id,m.status,m.origin
 FROM memberships m LEFT JOIN variant_memberships v ON v.uuid=m.uuid AND v.parent_id=m.entity;
'''

# rebuild() projects persons and canonicals onto entities (parent_id, merged_into), so hybrid reads them through
# indexes. Joining every membership to the unindexed identity_nodes view instead (the abandoned v2 view did) left the
# sidebar's variant counts unfinished after 10 minutes on the real library (53k memberships × 8k nodes); this takes 0.2 s.
HYBRID_VARIANTS = '''CREATE VIEW variant_memberships AS
 SELECT m.uuid,e.id AS entity,e.parent_id,e.variant,
 CASE WHEN s.status='confirmed' AND s.origin<>'album' THEN 'confirmed' ELSE 'machine' END AS status
 FROM memberships m JOIN entities e ON e.parent_id=m.entity AND e.merged_into IS NULL
 LEFT JOIN assertions s ON s.uuid=m.uuid AND s.entity=e.id
 WHERE (s.status='confirmed' AND s.origin<>'album') OR (coalesce(s.status,'')<>'rejected' AND EXISTS
 (SELECT 1 FROM candidates v JOIN entities x ON x.id=v.entity WHERE v.uuid=m.uuid AND (x.id=e.id OR x.merged_into=e.id)))'''


def policy(c):
    row = c.execute("SELECT value FROM meta WHERE key='identity_policy'").fetchone()
    return row[0] if row and row[0] == 'hybrid' else None


def enabled(c):
    """The relation layer (canonical projection, person/variant nodes, decisions) is active."""
    return policy(c) is not None


def install(c):
    for sql in SCHEMA.split(';'):
        if sql.strip():
            c.execute(sql)


def activate(c, mode='hybrid'):
    """Turn the relation layer on (hybrid; the only policy since v2 was abandoned)."""
    if mode != 'hybrid':
        raise ValueError('未知身份策略')
    install(c)
    c.execute("INSERT OR REPLACE INTO meta VALUES ('identity_policy',?)", (mode,))
    baseline(c)
    for a,b,label,kind in c.execute('SELECT id,parent_id,variant,kind FROM identity_baseline WHERE parent_id IS NOT NULL').fetchall():
        r=propose(c,a,b,'form_of',{'inherited_parent_id':b,'variant_label':label,'previous_kind':kind,
                                 'review_status':'inherited_not_newly_verified'},'legacy-hierarchy')
        previous=c.execute('SELECT status FROM identity_relations WHERE id=?',(r,)).fetchone()[0]
        c.execute("UPDATE identity_relations SET status='accepted' WHERE id=?",(r,))
        c.execute("INSERT INTO identity_decisions(relation_id,previous_status,status,reason) VALUES (?,?,'accepted','保留既有层级并使其可撤销；不代表新增人工确认')",(r,previous))
        c.execute("UPDATE identity_baseline SET parent_id=NULL,kind='character' WHERE id=?",(a,))
    # Historical destructive merges cannot be reverse-engineered without loss.
    # Turn them into explicit, auditable edges and conservatively scope their
    # extant target assertions: a future split asks for confirmation.
    for a,b in c.execute('SELECT id,merged_into FROM identity_baseline WHERE merged_into IS NOT NULL').fetchall():
        r=propose(c,a,b,'duplicate_of',{'legacy_destructive_merge':True,'raw_provenance':'not_recoverable_without_review'},'migration')
        previous=c.execute('SELECT status FROM identity_relations WHERE id=?',(r,)).fetchone()[0]
        c.execute("UPDATE identity_relations SET status='accepted' WHERE id=?",(r,))
        c.execute("INSERT INTO identity_decisions(relation_id,previous_status,status,reason) VALUES (?,?,'accepted','保留迁移前已存在的人工合并；没有新增自动确认')",(r,previous))
        c.execute('UPDATE identity_baseline SET merged_into=NULL WHERE id=?',(a,))
        c.execute('''INSERT OR IGNORE INTO identity_scopes SELECT uuid,entity,? FROM assertions WHERE entity=?''',
                  (json.dumps(sorted((a,b))),b))
    rebuild(c)
    install_views(c)
    # This covers all existing write entrypoints, including album imports and bulk
    # operations. Snapshots explicitly restore these rows on undo.
    for event in ('INSERT', 'UPDATE'):
        c.execute(f'''CREATE TRIGGER IF NOT EXISTS identity_assertion_{event.lower()} AFTER {event} ON assertions BEGIN
          DELETE FROM identity_scopes WHERE uuid=new.uuid AND entity=new.entity;
          DELETE FROM identity_suspended WHERE uuid=new.uuid AND entity=new.entity;
          INSERT INTO identity_scopes SELECT new.uuid,new.entity,json_group_array(entity)
          FROM (SELECT entity FROM identity_canonical WHERE canonical=(SELECT canonical FROM identity_canonical WHERE entity=new.entity) ORDER BY entity)
          HAVING count(*)>1;
        END''')
    c.execute('''CREATE TRIGGER IF NOT EXISTS identity_assertion_delete AFTER DELETE ON assertions BEGIN
      DELETE FROM identity_scopes WHERE uuid=old.uuid AND entity=old.entity;
      DELETE FROM identity_suspended WHERE uuid=old.uuid AND entity=old.entity;
    END''')


def install_views(c):
    """(Re)create hybrid's ownership views; also used by schema migrations to replace old definitions."""
    for view in ('identity_assignments', 'variant_memberships', 'membership_rules', 'family_confirmed', 'effective_assertions'):
        c.execute('DROP VIEW IF EXISTS ' + view)
    sql = VIEWS.replace('{machine}', HYBRID_MACHINE)
    for statement in sql.replace('{variants}', HYBRID_VARIANTS).split(';'):
        if statement.strip():
            c.execute(statement)


def baseline(c):
    c.execute('''INSERT OR IGNORE INTO identity_baseline
      SELECT id,parent_id,variant,kind,merged_into FROM entities WHERE kind<>'work' ''')


def propose(c, a, b, kind, evidence=None, source='manual'):
    if kind not in ('duplicate_of', 'form_of', 'different_from') or not isinstance(a, str) or not isinstance(b, str) or a == b:
        raise ValueError('无效身份关系')
    if c.execute("SELECT count(*) FROM entities WHERE id IN (?,?) AND kind<>'work'", (a, b)).fetchone()[0] != 2:
        raise ValueError('关系端点不存在')
    if kind == 'different_from':
        a, b = sorted((a, b))
    # A duplicate relation is symmetric, but b records the desired display representative.
    if kind == 'duplicate_of':
        row = c.execute('SELECT id FROM identity_relations WHERE kind=? AND ((a=? AND b=?) OR (a=? AND b=?))', (kind,a,b,b,a)).fetchone()
    else:
        row = c.execute('SELECT id FROM identity_relations WHERE a=? AND b=? AND kind=?', (a,b,kind)).fetchone()
    ident = row[0] if row else c.execute('INSERT INTO identity_relations(a,b,kind) VALUES (?,?,?)', (a,b,kind)).lastrowid
    if evidence is not None:
        payload = json.dumps(evidence, ensure_ascii=False, sort_keys=True, allow_nan=False)
        def semantic(value):
            if isinstance(value,dict):
                return {k:semantic(v) for k,v in value.items() if k not in ('response_ids','response_id','fetched_at')}
            if isinstance(value,list):return [semantic(v) for v in value]
            return value
        fp = hashlib.sha256((source + '\n' + json.dumps(semantic(evidence),ensure_ascii=False,sort_keys=True,allow_nan=False)).encode()).hexdigest()
        c.execute('INSERT OR IGNORE INTO identity_evidence(relation_id,fingerprint,source,payload) VALUES (?,?,?,?)', (ident,fp,source,payload))
    return ident


def rebuild(c):
    """Recompute full components from accepted edges; never union incrementally on undo."""
    baseline(c)
    rows, mapping, hierarchy, families = _graph(c)
    _project(c, rows, mapping, hierarchy, families)


def accept_many(c, ids, reason):
    """Accept relations one by one, each checked against the graph so far; rebuild once at the end.

    A relation that would contradict the graph (a variant with two people, a cycle, different_from)
    stays proposed and is returned with the reason, for a person to look at.
    """
    baseline(c)
    accepted, skipped = [], []
    for ident in ids:
        previous = c.execute('SELECT status FROM identity_relations WHERE id=?', (ident,)).fetchone()
        if not previous or previous[0] != 'proposed':
            continue
        c.execute("UPDATE identity_relations SET status='accepted' WHERE id=?", (ident,))
        try:
            _graph(c)
        except ValueError as exc:
            c.execute("UPDATE identity_relations SET status='proposed' WHERE id=?", (ident,))
            skipped.append({'relation_id': ident, 'reason': str(exc)})
            continue
        decision = c.execute("INSERT INTO identity_decisions(relation_id,previous_status,status,reason) VALUES (?,?,'accepted',?)",
                             (ident, previous[0], reason)).lastrowid
        c.execute('INSERT INTO identity_decision_evidence SELECT ?,fingerprint FROM identity_evidence WHERE relation_id=?', (decision, ident))
        accepted.append(ident)
    if accepted:
        rebuild(c)
    return {'accepted': accepted, 'skipped': skipped}


def _graph(c):
    """(baseline rows, canonical mapping, variant -> person, id -> person); raises ValueError on a contradiction."""
    rows = {r[0]: tuple(r[1:]) for r in c.execute('SELECT * FROM identity_baseline')}
    parent = {i:i for i in rows}
    def find(i):
        while parent[i] != i:
            parent[i] = parent[parent[i]]
            i = parent[i]
        return i
    def join(a,b):
        a,b=find(a),find(b)
        if a != b:
            parent[a]=b
    # Preserve only the *original* legacy state; computed merged_into values must
    # never be read back as new accepted evidence.
    for i, (_,_,_,merged) in rows.items():
        if merged in parent:
            join(i,merged)
    accepted = list(c.execute("SELECT id,a,b,kind FROM identity_relations WHERE status='accepted' ORDER BY id"))
    for _,a,b,kind in accepted:
        if kind=='duplicate_of':
            join(a,b)
    mapping={i:find(i) for i in rows}
    # Parent semantics belong to an equivalence component. Reinterpreting one
    # alias as a variant must reinterpret its aliases too, without changing IDs
    # or requiring a duplicate form_of edge for every spelling.
    hierarchy={}
    def attach(a,b):
        child,person=mapping[a],mapping[b]
        if child==person:
            raise ValueError('变体不能与其人物重复')
        if child in hierarchy and hierarchy[child]!=person:
            raise ValueError('变体已有另一个父人物，需先撤销旧关系')
        hierarchy[child]=person
    for i,r in rows.items():
        if r[0]:attach(i,r[0])
    for _,a,b,kind in accepted:
        if kind=='different_from' and mapping[a]==mapping[b]:
            raise ValueError('确认不同与重复等价组冲突')
        if kind=='form_of':attach(a,b)
    def root(i):
        i=mapping[i];seen=set()
        while i in hierarchy:
            if i in seen:
                raise ValueError('人物—变体关系存在环')
            seen.add(i);i=hierarchy[i]
        return i
    families={i:root(i) for i in rows}
    for _,a,b,kind in accepted:
        if kind=='different_from' and families[a]==families[b]:
            raise ValueError('确认不同与人物—变体关系冲突')
    return rows, mapping, hierarchy, families


def _project(c, rows, mapping, hierarchy, families):
    before={r[0]:(r[1],r[2]) for r in c.execute('SELECT id,parent_id,merged_into FROM entities')}
    suspended=set()
    for u,e,members in c.execute('SELECT uuid,entity,members_json FROM identity_scopes').fetchall():
        old=json.loads(members)
        if len({mapping.get(i,i) for i in old})>1:
            c.execute('INSERT OR REPLACE INTO identity_suspended VALUES (?,?,?,?)',(u,e,'equivalence_split',members))
            suspended.add(u)
    c.execute('DELETE FROM identity_canonical')
    c.executemany('INSERT INTO identity_canonical VALUES (?,?)',mapping.items())
    for i,(p,variant,kind,merged) in rows.items():
        p=families[i] if mapping[i] in hierarchy else None
        if p and not variant:
            tag=c.execute("SELECT key FROM links WHERE entity_id=? AND system='danbooru' ORDER BY current DESC LIMIT 1",(i,)).fetchone()
            variant=tag[0] if tag else i
        c.execute('UPDATE entities SET parent_id=?,variant=?,kind=?,merged_into=? WHERE id=?',
                  (p,variant if p else '','skin' if p else kind,mapping[i] if mapping[i]!=i else None,i))
    # Only a changed projection regroups evidence. Rejecting a proposal must not redo work.
    version=graph_version(c)
    previous=c.execute("SELECT value FROM meta WHERE key='identity_graph_version'").fetchone()
    if not previous or previous[0]!=version:
        from .store import Store
        # Hybrid keeps machine results: regroup the evidence of the images this change touches.
        changed={r[0] for r in c.execute('SELECT id,parent_id,merged_into FROM entities') if before.get(r[0])!=(r[1],r[2])}
        uuids=affected_images(c,changed)|suspended
        Store._refresh_evidence(c,sorted(uuids))
        Store._refresh_memberships(c,sorted(uuids))
    c.execute("INSERT OR REPLACE INTO meta VALUES ('identity_graph_version',?)",(version,))


def affected_images(c, entities):
    """Images whose person-level evidence or verdicts can change when these identities are re-projected."""
    if not entities:
        return set()
    c.execute('CREATE TEMP TABLE IF NOT EXISTS reprojected(id TEXT PRIMARY KEY)')
    c.execute('DELETE FROM reprojected')
    c.executemany('INSERT INTO reprojected VALUES (?)', [(e,) for e in entities])
    return {r[0] for r in c.execute('''SELECT uuid FROM candidates WHERE entity IN (SELECT id FROM reprojected)
        UNION SELECT uuid FROM proto_claims WHERE entity IN (SELECT id FROM reprojected)
        UNION SELECT uuid FROM assertions WHERE entity IN (SELECT id FROM reprojected)''')}


def decide(c, ident, status, reason):
    if status not in ('proposed','accepted','rejected') or not isinstance(reason,str) or not reason.strip():
        raise ValueError('请保存明确的关系决议及理由')
    row=c.execute('SELECT status FROM identity_relations WHERE id=?',(ident,)).fetchone()
    if not row:
        raise ValueError('关系候选不存在')
    previous=row[0]
    c.execute('UPDATE identity_relations SET status=? WHERE id=?',(status,ident))
    decision=c.execute('INSERT INTO identity_decisions(relation_id,previous_status,status,reason) VALUES (?,?,?,?)',
                      (ident,previous,status,reason.strip())).lastrowid
    c.execute('INSERT INTO identity_decision_evidence SELECT ?,fingerprint FROM identity_evidence WHERE relation_id=?',(decision,ident))
    rebuild(c)
    return {'relation_id':ident,'previous_status':previous,'decision_id':decision,'status':status}


def pending_hierarchy_people(c):
    """Inherited name-derived hierarchy preserves old human views, not new trust.

    Explicitly reaffirming an inherited form records its reviewed evidence. Until
    then its entire person aggregation is ineligible for automatic assignment.
    """
    return {r[0] for r in c.execute("""SELECT DISTINCT n.character_id
      FROM identity_relations r JOIN identity_nodes n ON n.id=r.a
      JOIN identity_evidence e ON e.relation_id=r.id AND e.source='legacy-hierarchy'
      WHERE r.kind='form_of' AND r.status='accepted' AND NOT EXISTS(
        SELECT 1 FROM identity_decision_evidence x WHERE x.fingerprint=e.fingerprint
        AND x.decision_id=(SELECT max(d.id) FROM identity_decisions d WHERE d.relation_id=r.id))""")}


def bank_context(c):
    """Reference purity/dedup depends on tag ownership, hierarchy and exclusions."""
    columns={r[1] for r in c.execute('PRAGMA table_info(entities)')}
    merged='merged_into' if 'merged_into' in columns else 'NULL'
    nodes=list(map(tuple,c.execute(f"SELECT id,parent_id,{merged} FROM entities WHERE kind<>'work' ORDER BY id")))
    tags=list(map(tuple,c.execute("SELECT key,entity_id FROM links WHERE system='danbooru' ORDER BY key,entity_id")))
    exclusions=list(map(tuple,c.execute("SELECT entity,reference FROM identity_anchor_decisions WHERE decision='exclude' ORDER BY entity,reference"))) if c.execute("SELECT 1 FROM sqlite_master WHERE name='identity_anchor_decisions'").fetchone() else []
    return hashlib.sha256(json.dumps([nodes,tags,exclusions]).encode()).hexdigest()


def graph_version(c):
    rows=list(map(tuple,c.execute('SELECT id,parent_id,canonical_id FROM identity_nodes ORDER BY id')))
    return hashlib.sha256(json.dumps([rows,sorted(pending_hierarchy_people(c))]).encode()).hexdigest()


def scope_snapshot(c,u,e):
    result={}
    for table,fields in (('identity_scopes','members_json'),('identity_suspended','reason,options_json')):
        row=c.execute(f'SELECT {fields} FROM {table} WHERE uuid=? AND entity=?',(u,e)).fetchone()
        result[table]=list(row) if row else None
    row=c.execute('SELECT options_json,reason,created_at FROM identity_uncertain WHERE uuid=?',(u,)).fetchone()
    result['identity_uncertain']=list(row) if row else None
    return result


def restore_scope(c,u,e,snapshot):
    for table in ('identity_scopes','identity_suspended'):
        c.execute(f'DELETE FROM {table} WHERE uuid=? AND entity=?',(u,e))
        row=snapshot.get(table)
        if row:
            c.execute(f'INSERT INTO {table} VALUES ({",".join("?" for _ in range(2+len(row)))})',(u,e,*row))
    c.execute('DELETE FROM identity_uncertain WHERE uuid=?',(u,))
    if snapshot.get('identity_uncertain'):
        c.execute('INSERT INTO identity_uncertain VALUES (?,?,?,?)',(u,*snapshot['identity_uncertain']))


def load_bank(c):
    row=c.execute("SELECT value FROM meta WHERE key='identity_bank'").fetchone()
    if not row:
        return None
    import numpy as np
    path=Path(row[0])
    meta=json.loads((path/'bank.json').read_text())
    if meta['context']!=bank_context(c):
        raise ValueError('身份、tag 或锚点排除已改变，必须重建参考库')
    vectors=np.load(path/'vectors.npy',allow_pickle=False)
    if hashlib.sha256((path/'vectors.npy').read_bytes()).hexdigest()!=meta['vectors_sha256']:
        raise ValueError('锚点指纹变化，拒绝使用旧校准')
    expected=hashlib.sha256((meta['vectors_sha256']+meta['reference_vectors_sha256']+meta['context']+json.dumps(meta['prototypes'],sort_keys=True)).encode()).hexdigest()
    if expected!=meta['version']:
        raise ValueError('锚点归属或版本指纹不匹配')
    if vectors.ndim!=2 or vectors.shape[1]!=768 or len(vectors)!=len(meta['prototypes']) or not np.isfinite(vectors).all() or (np.linalg.norm(vectors,axis=1)<=1e-12).any():
        raise ValueError('锚点数据不合法')
    mapping={r[0]:(r[1],r[2]) for r in c.execute('SELECT id,character_id,variant_id FROM identity_nodes')}
    keep=[i for i,p in enumerate(meta['prototypes']) if p['entity'] in mapping]
    if len(keep)!=len(meta['prototypes']):
        raise ValueError('锚点引用不存在的身份')
    owners=[mapping[meta['prototypes'][i]['entity']][0] for i in keep]
    variants=[mapping[meta['prototypes'][i]['entity']][1] for i in keep]
    return meta,vectors[keep],owners,variants
