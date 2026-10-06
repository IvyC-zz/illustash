-- Existing Python migration SQL; keep rules unchanged.

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
;
