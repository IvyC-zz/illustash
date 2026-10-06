-- Existing Python migration SQL; keep rules unchanged.

CREATE INDEX IF NOT EXISTS entities_parent ON entities(parent_id,id);
CREATE TABLE IF NOT EXISTS reviews(entity TEXT NOT NULL,reviewed_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,action_id INTEGER PRIMARY KEY);
CREATE TABLE IF NOT EXISTS prefs(key TEXT PRIMARY KEY,value TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS family_evidence(uuid TEXT NOT NULL,entity TEXT NOT NULL,sources TEXT NOT NULL,score REAL NOT NULL,
 source_count INTEGER NOT NULL,ccip_only INTEGER NOT NULL,wd REAL,ccip REAL,rank INTEGER NOT NULL,PRIMARY KEY(entity,uuid));
CREATE INDEX IF NOT EXISTS family_evidence_uuid ON family_evidence(uuid,entity);
CREATE TABLE IF NOT EXISTS single_assets(uuid TEXT PRIMARY KEY);
CREATE TABLE IF NOT EXISTS lead_assets(uuid TEXT PRIMARY KEY);
CREATE TABLE IF NOT EXISTS duplicates(uuid TEXT PRIMARY KEY,grp TEXT NOT NULL,size INTEGER NOT NULL,pos INTEGER NOT NULL,grp_added TEXT NOT NULL,
 rep INTEGER NOT NULL,hidden INTEGER NOT NULL DEFAULT 0);
CREATE INDEX IF NOT EXISTS duplicates_grp ON duplicates(grp,pos);
CREATE TABLE IF NOT EXISTS curation(uuid TEXT PRIMARY KEY,status TEXT NOT NULL CHECK(status IN ('included','excluded')),
 origin TEXT NOT NULL,created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);
CREATE TABLE IF NOT EXISTS sync_runs(id INTEGER PRIMARY KEY AUTOINCREMENT,started_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
 finished_at TEXT,status TEXT NOT NULL,found INTEGER NOT NULL DEFAULT 0,removed INTEGER NOT NULL DEFAULT 0,
 imported INTEGER NOT NULL DEFAULT 0,failed INTEGER NOT NULL DEFAULT 0,sources TEXT,message TEXT);
CREATE TABLE IF NOT EXISTS sync_added(uuid TEXT PRIMARY KEY,run_id INTEGER NOT NULL);
CREATE INDEX IF NOT EXISTS sync_added_run ON sync_added(run_id,uuid);
CREATE TABLE IF NOT EXISTS similar_groups(uuid TEXT PRIMARY KEY,gid INTEGER NOT NULL);
CREATE INDEX IF NOT EXISTS similar_groups_gid ON similar_groups(gid,uuid);
CREATE TABLE IF NOT EXISTS clusters(uuid TEXT PRIMARY KEY,cid INTEGER NOT NULL);
CREATE INDEX IF NOT EXISTS clusters_cid ON clusters(cid,uuid);
CREATE TABLE IF NOT EXISTS cluster_info(cid INTEGER PRIMARY KEY,size INTEGER NOT NULL,near INTEGER,near_dist REAL,built_at TEXT NOT NULL,
 near_kind TEXT,traits TEXT);
CREATE TABLE IF NOT EXISTS prototypes(entity TEXT NOT NULL,k INTEGER NOT NULL,vec BLOB NOT NULL,threshold REAL NOT NULL,
 seeds INTEGER NOT NULL,hint REAL NOT NULL,PRIMARY KEY(entity,k));
CREATE TABLE IF NOT EXISTS weak_claims(uuid TEXT NOT NULL,entity TEXT NOT NULL,source TEXT NOT NULL,score REAL NOT NULL,
 PRIMARY KEY(entity,uuid,source));
CREATE INDEX IF NOT EXISTS weak_claims_uuid ON weak_claims(uuid,entity);
CREATE TABLE IF NOT EXISTS proto_claims(uuid TEXT NOT NULL,entity TEXT NOT NULL,score REAL NOT NULL,PRIMARY KEY(entity,uuid));
CREATE INDEX IF NOT EXISTS proto_claims_uuid ON proto_claims(uuid,entity);
CREATE TABLE IF NOT EXISTS learn_state(entity TEXT PRIMARY KEY,signature TEXT NOT NULL,learned_at TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS auto_rules(uuid TEXT PRIMARY KEY,kind TEXT NOT NULL,reason TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS excluded(uuid TEXT PRIMARY KEY,reason TEXT NOT NULL,origin TEXT NOT NULL);
CREATE VIEW IF NOT EXISTS family_confirmed AS
 SELECT s.uuid,coalesce(e.parent_id,e.id) AS entity
 FROM assertions s JOIN entities e ON e.id=s.entity JOIN assets a ON a.uuid=s.uuid
 WHERE s.status='confirmed' AND NOT EXISTS
 (SELECT 1 FROM assertions r WHERE r.uuid=s.uuid AND r.entity=coalesce(e.parent_id,e.id) AND r.status='rejected')
 GROUP BY s.uuid,coalesce(e.parent_id,e.id);
CREATE VIEW IF NOT EXISTS membership_rules AS
 SELECT f.uuid,f.entity,'confirmed' AS status,
 (SELECT s.origin FROM assertions s JOIN entities x ON x.id=s.entity
  WHERE s.uuid=f.uuid AND coalesce(x.parent_id,x.id)=f.entity AND s.status='confirmed'
  ORDER BY (x.parent_id IS NULL) DESC,s.origin,s.entity LIMIT 1) AS origin,
 coalesce(v.sources,'') AS sources,v.score,coalesce(v.source_count,0) AS source_count,coalesce(v.ccip_only,0) AS ccip_only
 FROM family_confirmed f LEFT JOIN family_evidence v ON v.uuid=f.uuid AND v.entity=f.entity
 UNION ALL
 SELECT v.uuid,v.entity,'machine','machine',v.sources,v.score,v.source_count,v.ccip_only
 FROM family_evidence v
 WHERE NOT EXISTS(SELECT 1 FROM assertions s WHERE s.uuid=v.uuid AND s.entity=v.entity AND s.status='rejected')
 AND NOT EXISTS(SELECT 1 FROM family_confirmed f WHERE f.uuid=v.uuid AND f.entity=v.entity)
 AND NOT (v.ccip_only=1 AND EXISTS(SELECT 1 FROM family_confirmed f WHERE f.uuid=v.uuid AND f.entity<>v.entity))
 AND NOT (EXISTS(SELECT 1 FROM single_assets o WHERE o.uuid=v.uuid) AND NOT (EXISTS(SELECT 1 FROM entities x WHERE x.id=v.entity AND x.mascot=1)
  AND (coalesce(v.wd,0)>=0.9 OR coalesce(v.ccip,0)>=0.95)) AND
  (v.rank>1 OR EXISTS(SELECT 1 FROM family_confirmed f WHERE f.uuid=v.uuid AND f.entity<>v.entity)))
 AND NOT (EXISTS(SELECT 1 FROM lead_assets o WHERE o.uuid=v.uuid) AND NOT (EXISTS(SELECT 1 FROM entities x WHERE x.id=v.entity AND x.mascot=1)
  AND (coalesce(v.wd,0)>=0.9 OR coalesce(v.ccip,0)>=0.95))
  AND coalesce(v.wd,0)<0.9 AND coalesce(v.ccip,0)<0.95
  AND (v.rank>1 OR EXISTS(SELECT 1 FROM family_confirmed f WHERE f.uuid=v.uuid AND f.entity<>v.entity))) AND NOT (v.sources='CCIP' AND coalesce(v.ccip,0)<0.925);
CREATE TABLE IF NOT EXISTS memberships(uuid TEXT NOT NULL,entity TEXT NOT NULL,status TEXT NOT NULL,origin TEXT,
 sources TEXT NOT NULL,score REAL,source_count INTEGER NOT NULL,ccip_only INTEGER NOT NULL,skey TEXT NOT NULL,
 PRIMARY KEY(entity,uuid)) WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS memberships_uuid ON memberships(uuid,skey);
CREATE TABLE IF NOT EXISTS tag_memberships(uuid TEXT NOT NULL,entity TEXT NOT NULL,status TEXT NOT NULL,origin TEXT,
 sources TEXT NOT NULL,score REAL,source_count INTEGER NOT NULL,ccip_only INTEGER NOT NULL,skey TEXT NOT NULL,
 PRIMARY KEY(entity,uuid)) WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS tag_memberships_uuid ON tag_memberships(uuid);
CREATE VIEW IF NOT EXISTS variant_memberships AS
 SELECT m.uuid,e.id AS entity,e.parent_id,e.variant,
 CASE WHEN s.status='confirmed' AND s.origin<>'album' THEN 'confirmed' ELSE 'machine' END AS status
 FROM memberships m JOIN entities e ON e.parent_id=m.entity
 LEFT JOIN assertions s ON s.uuid=m.uuid AND s.entity=e.id
 WHERE (s.status='confirmed' AND s.origin<>'album') OR (coalesce(s.status,'')<>'rejected' AND EXISTS
 (SELECT 1 FROM candidates v WHERE v.uuid=m.uuid AND v.entity=e.id));
;
CREATE VIEW IF NOT EXISTS ambiguous_claims AS
 SELECT m.uuid,m.entity,r.entity AS rival,t.ccip-r.ccip AS margin
 FROM memberships m JOIN single_assets o ON o.uuid=m.uuid
 JOIN family_evidence t ON t.uuid=m.uuid AND t.entity=m.entity AND t.rank=1
 JOIN family_evidence r ON r.uuid=m.uuid AND r.rank=2
 WHERE m.status='machine' AND ((t.wd IS NOT NULL AND t.ccip IS NOT NULL)*4 + (coalesce(t.wd,0)>=0.9 OR coalesce(t.ccip,0)>=0.95)*2 + (t.wd IS NOT NULL))=((r.wd IS NOT NULL AND r.ccip IS NOT NULL)*4 + (coalesce(r.wd,0)>=0.9 OR coalesce(r.ccip,0)>=0.95)*2 + (r.wd IS NOT NULL))
 AND (t.ccip IS NULL OR r.ccip IS NULL OR t.ccip-r.ccip<0.03)
 AND NOT EXISTS(SELECT 1 FROM assertions s WHERE s.uuid=m.uuid AND s.entity=r.entity AND s.status='rejected')
 AND NOT EXISTS(SELECT 1 FROM entities x WHERE x.id=r.entity AND x.mascot=1)
;
