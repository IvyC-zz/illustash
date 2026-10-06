-- Empty source for a base installation; schema only, no personal or reference data.
-- Shared folder bookkeeping is in app_source_schema.sql.
CREATE TABLE assets(uuid TEXT PRIMARY KEY,filename TEXT,z_pk INTEGER,width INTEGER,height INTEGER,
    added_at TEXT,hidden INTEGER,trashed INTEGER,stable_hash TEXT,orig_size INTEGER,
    src_kind TEXT,src_path TEXT,in_scope INTEGER DEFAULT 1,source_id TEXT,preset TEXT);
CREATE TABLE labels(uuid TEXT NOT NULL,dim TEXT NOT NULL,value TEXT NOT NULL,conf REAL,
    source TEXT NOT NULL,model_ver TEXT,run_id TEXT,ts TEXT DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY(uuid,dim,value,source));
CREATE INDEX idx_labels_dim_val ON labels(dim,value);
CREATE INDEX idx_labels_uuid ON labels(uuid);
CREATE TABLE decisions(uuid TEXT NOT NULL,dim TEXT NOT NULL,value TEXT NOT NULL,"by" TEXT,
    ts TEXT DEFAULT CURRENT_TIMESTAMP,note TEXT,PRIMARY KEY(uuid,dim,value));
CREATE TABLE runs(run_id TEXT PRIMARY KEY,dim TEXT,model TEXT,model_ver TEXT,started TEXT,
    finished TEXT,n_assets INTEGER,params TEXT);
CREATE TABLE embeddings(uuid TEXT NOT NULL,model TEXT NOT NULL,vec BLOB NOT NULL,PRIMARY KEY(uuid,model));
CREATE TABLE characters(char_id TEXT PRIMARY KEY,danbooru_tag TEXT,wd_tag TEXT,copyright TEXT,
    base_tag TEXT,skin TEXT,post_count INTEGER,gender TEXT,gender_conf REAL,core_tags TEXT,n_ref INTEGER,
    anchor_p50 REAL,anchor_p75 REAL,anchor_p90 REAL,anchor_row INTEGER,name_en TEXT,name_ja TEXT,name_zh TEXT);
CREATE TABLE character_aliases(char_id TEXT NOT NULL,alias TEXT NOT NULL,kind TEXT NOT NULL,
    PRIMARY KEY(char_id,alias,kind));
