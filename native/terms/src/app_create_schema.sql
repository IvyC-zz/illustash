-- Fresh application schema 36, from web/store.py SCHEMA/V2_SCHEMA/ASKED,
-- tests/reference/web/names.py SCHEMA/WORK_NAMES, tests/reference/web/works.py migrate, web/identity.py SCHEMA,
-- and tests/reference/web/effects.py SCHEMA. No existing-library migrations or identity activation.

CREATE TABLE IF NOT EXISTS meta(key TEXT PRIMARY KEY,value TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS entities(id TEXT PRIMARY KEY,name TEXT NOT NULL,work TEXT NOT NULL DEFAULT '',external_key TEXT UNIQUE);
CREATE TABLE IF NOT EXISTS assets(uuid TEXT PRIMARY KEY,filename TEXT,src_path TEXT,width INTEGER,height INTEGER,added_at TEXT,src_kind TEXT);
CREATE INDEX IF NOT EXISTS assets_date ON assets(added_at DESC,uuid);
CREATE TABLE IF NOT EXISTS candidates(uuid TEXT NOT NULL,entity TEXT NOT NULL,source TEXT NOT NULL,score REAL NOT NULL,PRIMARY KEY(uuid,entity,source));
CREATE INDEX IF NOT EXISTS candidates_entity ON candidates(entity,uuid);
CREATE TABLE IF NOT EXISTS general_hits(uuid TEXT NOT NULL,value TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS assertions(uuid TEXT NOT NULL,entity TEXT NOT NULL,status TEXT NOT NULL CHECK(status IN ('confirmed','rejected')),origin TEXT NOT NULL,PRIMARY KEY(uuid,entity));
CREATE INDEX IF NOT EXISTS assertions_entity ON assertions(entity,status,uuid);
CREATE TABLE IF NOT EXISTS deferred(uuid TEXT PRIMARY KEY);
CREATE TABLE IF NOT EXISTS imported(uuid TEXT NOT NULL,value TEXT NOT NULL,PRIMARY KEY(uuid,value));
CREATE TABLE IF NOT EXISTS asset_tags(uuid TEXT NOT NULL,tag TEXT NOT NULL,origin TEXT NOT NULL,created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,PRIMARY KEY(uuid,tag));
CREATE INDEX IF NOT EXISTS asset_tags_tag ON asset_tags(tag,uuid);
CREATE TABLE IF NOT EXISTS asset_rating(uuid TEXT PRIMARY KEY,rating TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS actions(id INTEGER PRIMARY KEY AUTOINCREMENT,label TEXT NOT NULL,before_json TEXT NOT NULL,ts TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,undone INTEGER NOT NULL DEFAULT 0);
CREATE TABLE IF NOT EXISTS image_sigs(uuid TEXT PRIMARY KEY,ratio REAL,grid BLOB,ok INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS relations(a TEXT NOT NULL,b TEXT NOT NULL,kind TEXT NOT NULL,changed REAL,note TEXT NOT NULL DEFAULT '',
 checked_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,PRIMARY KEY(a,b));
CREATE TABLE IF NOT EXISTS links(system TEXT NOT NULL,key TEXT NOT NULL,entity_id TEXT NOT NULL REFERENCES entities(id),
 current INTEGER NOT NULL DEFAULT 1,source TEXT NOT NULL,added_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,PRIMARY KEY(system,key));
CREATE INDEX IF NOT EXISTS links_entity ON links(entity_id,system);
CREATE TABLE IF NOT EXISTS names(entity_id TEXT NOT NULL REFERENCES entities(id),lang TEXT NOT NULL DEFAULT '' CHECK(lang IN ('','zh','ja','en')),
 value TEXT NOT NULL,source TEXT NOT NULL,role TEXT NOT NULL CHECK(role IN ('display','alias')),preferred INTEGER NOT NULL DEFAULT 0,
 position INTEGER NOT NULL DEFAULT 0,PRIMARY KEY(entity_id,role,lang,source,value));
CREATE INDEX IF NOT EXISTS names_value ON names(value COLLATE NOCASE);
CREATE TABLE IF NOT EXISTS asked(uuid TEXT NOT NULL,entity TEXT NOT NULL,kind TEXT NOT NULL,
 answer TEXT NOT NULL CHECK(answer IN ('yes','no','later')),action_id INTEGER,asked_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
 PRIMARY KEY(uuid,entity));
CREATE INDEX IF NOT EXISTS asked_entity ON asked(entity,answer,asked_at);
CREATE TABLE IF NOT EXISTS audit_sample(uuid TEXT PRIMARY KEY,entity TEXT NOT NULL,stratum TEXT NOT NULL,
 solo INTEGER NOT NULL,created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);
CREATE TABLE IF NOT EXISTS display_names(
 kind TEXT NOT NULL, key TEXT NOT NULL, lang TEXT NOT NULL, source TEXT NOT NULL, value TEXT NOT NULL,
 PRIMARY KEY(kind,key,lang,source,value));
CREATE TABLE IF NOT EXISTS name_aliases(kind TEXT NOT NULL,key TEXT NOT NULL,value TEXT NOT NULL,position INTEGER NOT NULL,PRIMARY KEY(kind,key,value));
CREATE TABLE IF NOT EXISTS name_cache(key TEXT PRIMARY KEY,value TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS name_settings(key TEXT PRIMARY KEY,value TEXT NOT NULL);

ALTER TABLE entities ADD COLUMN variant TEXT NOT NULL DEFAULT '';
ALTER TABLE entities ADD COLUMN parent_id TEXT REFERENCES entities(id);
ALTER TABLE entities ADD COLUMN tag TEXT;
ALTER TABLE entities ADD COLUMN merged_into TEXT REFERENCES entities(id);
ALTER TABLE entities ADD COLUMN kind TEXT NOT NULL DEFAULT '';
ALTER TABLE entities ADD COLUMN artist TEXT NOT NULL DEFAULT '';
ALTER TABLE entities ADD COLUMN label_only INTEGER NOT NULL DEFAULT 0;
ALTER TABLE entities ADD COLUMN mascot INTEGER NOT NULL DEFAULT 0;
ALTER TABLE entities ADD COLUMN mascot_origin TEXT;
ALTER TABLE entities ADD COLUMN work_id TEXT REFERENCES entities(id);
ALTER TABLE entities ADD COLUMN original INTEGER NOT NULL DEFAULT 0;
CREATE INDEX IF NOT EXISTS entities_work_id ON entities(work_id);
CREATE TABLE IF NOT EXISTS work_review(entity_id TEXT PRIMARY KEY REFERENCES entities(id),
        status TEXT NOT NULL DEFAULT 'pending',note TEXT NOT NULL DEFAULT '',parent_locked INTEGER NOT NULL DEFAULT 0);

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
CREATE TABLE IF NOT EXISTS asked(uuid TEXT NOT NULL,entity TEXT NOT NULL,kind TEXT NOT NULL,
 answer TEXT NOT NULL CHECK(answer IN ('yes','no','later')),action_id INTEGER,asked_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
 PRIMARY KEY(uuid,entity));
CREATE INDEX IF NOT EXISTS asked_entity ON asked(entity,answer,asked_at);
CREATE TABLE IF NOT EXISTS audit_sample(uuid TEXT PRIMARY KEY,entity TEXT NOT NULL,stratum TEXT NOT NULL,
 solo INTEGER NOT NULL,created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);
CREATE TABLE IF NOT EXISTS backend_effects(
    id INTEGER PRIMARY KEY, kind TEXT NOT NULL, status TEXT NOT NULL DEFAULT 'pending',
    error TEXT NOT NULL DEFAULT '', created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);
INSERT OR REPLACE INTO meta VALUES ('schema_version','36');
INSERT OR IGNORE INTO display_names VALUES ('work','touhou','zh','builtin','东方Project');
INSERT OR IGNORE INTO display_names VALUES ('work','hololive','zh','builtin','hololive');
INSERT OR IGNORE INTO display_names VALUES ('work','nijisanji','zh','builtin','彩虹社');
INSERT OR IGNORE INTO display_names VALUES ('work','vocaloid','zh','builtin','VOCALOID');
INSERT OR IGNORE INTO display_names VALUES ('work','fate','zh','builtin','Fate');
INSERT OR IGNORE INTO display_names VALUES ('work','idolmaster','zh','builtin','偶像大师');
INSERT OR IGNORE INTO display_names VALUES ('work','kancolle','zh','builtin','舰队Collection');
INSERT OR IGNORE INTO display_names VALUES ('work','azur_lane','zh','builtin','碧蓝航线');
INSERT OR IGNORE INTO display_names VALUES ('work','girls''_frontline','zh','builtin','少女前线');
INSERT OR IGNORE INTO display_names VALUES ('work','pokemon','zh','builtin','宝可梦');
INSERT OR IGNORE INTO display_names VALUES ('work','genshin_impact','zh','builtin','原神');
INSERT OR IGNORE INTO display_names VALUES ('work','arknights','zh','builtin','明日方舟');
INSERT OR IGNORE INTO display_names VALUES ('work','blue_archive','zh','builtin','蔚蓝档案');
INSERT OR IGNORE INTO display_names VALUES ('work','honkai:_star_rail','zh','builtin','崩坏：星穹铁道');
INSERT OR IGNORE INTO display_names VALUES ('work','zenless_zone_zero','zh','builtin','绝区零');
INSERT OR IGNORE INTO display_names VALUES ('work','wuthering_waves','zh','builtin','鸣潮');
INSERT OR IGNORE INTO display_names VALUES ('work','honkai_impact_3rd','zh','builtin','崩坏3');
INSERT OR IGNORE INTO display_names VALUES ('work','umamusume','zh','builtin','赛马娘');
INSERT OR IGNORE INTO display_names VALUES ('work','love_live!','zh','builtin','LoveLive!');
INSERT OR IGNORE INTO display_names VALUES ('work','bang_dream!','zh','builtin','BanG Dream!');
INSERT OR IGNORE INTO display_names VALUES ('work','project_sekai','zh','builtin','世界计划');
INSERT OR IGNORE INTO display_names VALUES ('work','princess_connect!','zh','builtin','公主连结');
