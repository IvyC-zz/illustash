-- 旧库（web/store.py、web/identity.py、tests/reference/web/works.py 建的）里 legacy_characters 读的表，从 Python 建的库原样导出。
-- 只给测试用：用 tests.test_web.fixture 建库、activate_identity(hybrid) 之后从 sqlite_master 导出。旧 schema 变了要重新导出，不手改。
CREATE TABLE meta(key TEXT PRIMARY KEY,value TEXT NOT NULL);
CREATE TABLE actions(id INTEGER PRIMARY KEY AUTOINCREMENT,label TEXT NOT NULL,before_json TEXT NOT NULL,ts TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,undone INTEGER NOT NULL DEFAULT 0);
CREATE TABLE assets(uuid TEXT PRIMARY KEY,filename TEXT,src_path TEXT,width INTEGER,height INTEGER,added_at TEXT,src_kind TEXT);
CREATE TABLE entities(id TEXT PRIMARY KEY,name TEXT NOT NULL,work TEXT NOT NULL DEFAULT '',external_key TEXT UNIQUE, variant TEXT NOT NULL DEFAULT '', parent_id TEXT REFERENCES entities(id), tag TEXT, merged_into TEXT REFERENCES entities(id), kind TEXT NOT NULL DEFAULT '', artist TEXT NOT NULL DEFAULT '', label_only INTEGER NOT NULL DEFAULT 0, mascot INTEGER NOT NULL DEFAULT 0, mascot_origin TEXT, work_id TEXT REFERENCES entities(id), original INTEGER NOT NULL DEFAULT 0);
CREATE TABLE links(system TEXT NOT NULL,key TEXT NOT NULL,entity_id TEXT NOT NULL REFERENCES entities(id),
 current INTEGER NOT NULL DEFAULT 1,source TEXT NOT NULL,added_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,PRIMARY KEY(system,key));
CREATE TABLE work_review(entity_id TEXT PRIMARY KEY REFERENCES entities(id),
        status TEXT NOT NULL DEFAULT 'pending',note TEXT NOT NULL DEFAULT '',parent_locked INTEGER NOT NULL DEFAULT 0);
CREATE TABLE assertions(uuid TEXT NOT NULL,entity TEXT NOT NULL,status TEXT NOT NULL CHECK(status IN ('confirmed','rejected')),origin TEXT NOT NULL,PRIMARY KEY(uuid,entity));
CREATE TABLE candidates(uuid TEXT NOT NULL,entity TEXT NOT NULL,source TEXT NOT NULL,score REAL NOT NULL,PRIMARY KEY(uuid,entity,source));
CREATE TABLE deferred(uuid TEXT PRIMARY KEY);
CREATE TABLE identity_relations(
 id INTEGER PRIMARY KEY,a TEXT NOT NULL REFERENCES entities(id),b TEXT NOT NULL REFERENCES entities(id),
 kind TEXT NOT NULL CHECK(kind IN ('duplicate_of','form_of','different_from')),
 status TEXT NOT NULL DEFAULT 'proposed' CHECK(status IN ('proposed','accepted','rejected')),
 UNIQUE(a,b,kind),CHECK(a<>b));
CREATE TABLE identity_evidence(
 relation_id INTEGER NOT NULL REFERENCES identity_relations(id),fingerprint TEXT NOT NULL,
 source TEXT NOT NULL,payload TEXT NOT NULL,created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
 PRIMARY KEY(relation_id,fingerprint));
CREATE TABLE identity_decisions(
 id INTEGER PRIMARY KEY,relation_id INTEGER NOT NULL REFERENCES identity_relations(id),
 previous_status TEXT NOT NULL,status TEXT NOT NULL,reason TEXT NOT NULL,
 created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);
CREATE TABLE identity_decision_evidence(decision_id INTEGER NOT NULL,fingerprint TEXT NOT NULL,
 PRIMARY KEY(decision_id,fingerprint));
CREATE TABLE identity_baseline(
 id TEXT PRIMARY KEY,parent_id TEXT,variant TEXT,kind TEXT,merged_into TEXT);
