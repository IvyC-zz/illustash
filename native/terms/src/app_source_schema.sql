-- web/library.py ensure_source_schema, after cloning the reference tables and indices.
-- Missing source_id/preset columns are added conditionally by app_create.rs.
CREATE TABLE IF NOT EXISTS asset_links(asset TEXT NOT NULL, system TEXT NOT NULL, key TEXT NOT NULL,
                 source_id TEXT NOT NULL, current INTEGER NOT NULL DEFAULT 1, PRIMARY KEY(system, key, source_id));
CREATE INDEX IF NOT EXISTS asset_links_asset ON asset_links(asset);
CREATE TABLE IF NOT EXISTS files(source_id TEXT NOT NULL, path TEXT NOT NULL, size INTEGER NOT NULL,
                 mtime_ns INTEGER NOT NULL, md5 TEXT NOT NULL, asset TEXT NOT NULL, PRIMARY KEY(source_id, path));
CREATE INDEX IF NOT EXISTS files_md5 ON files(md5);
