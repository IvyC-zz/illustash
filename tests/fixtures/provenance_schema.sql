
CREATE TABLE IF NOT EXISTS pending_runs(kind TEXT PRIMARY KEY, requested_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);
CREATE TABLE IF NOT EXISTS artist_names(
 artist_id TEXT NOT NULL REFERENCES artists(id), source TEXT NOT NULL, name TEXT NOT NULL,
 updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP, PRIMARY KEY(artist_id,source));

-- Artist identities: internal ids only; every outside account is a link (same idea as characters, docs/DATA_MODEL.md).
CREATE TABLE IF NOT EXISTS artists(
  id TEXT PRIMARY KEY,
  merged_into TEXT REFERENCES artists(id),
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);
CREATE TABLE IF NOT EXISTS artist_links(          -- one (system, key) belongs to one artist
  system TEXT NOT NULL,                             -- 'danbooru' | 'x' | 'x_id' | 'pixiv' | 'name'
  key TEXT NOT NULL,                                -- danbooru tag, lowercase handle, numeric id, pixiv user id …
  artist_id TEXT NOT NULL REFERENCES artists(id),
  url TEXT NOT NULL DEFAULT '',
  source TEXT NOT NULL,                             -- where the link came from: 'danbooru_post' | 'danbooru_artist' | 'ocr' | …
  added_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  PRIMARY KEY(system, key));
CREATE INDEX IF NOT EXISTS artist_links_artist ON artist_links(artist_id);

-- Where a picture was posted; several per picture (after Monbooru's image_sources).
CREATE TABLE IF NOT EXISTS image_sources(
  asset TEXT NOT NULL,
  site TEXT NOT NULL,                               -- 'danbooru' | 'pixiv' | 'x' | …
  post_id TEXT NOT NULL,
  url TEXT NOT NULL,
  original TEXT NOT NULL DEFAULT '',                -- the upstream post the site declares (Danbooru source)
  artist_id TEXT REFERENCES artists(id),
  method TEXT NOT NULL,                             -- 'pixel_hash' | 'gallery_match' | 'saucenao' | …
  similarity REAL NOT NULL DEFAULT 0,               -- 0 = exact
  fetched_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  PRIMARY KEY(asset, site, post_id));

-- Who drew a picture, with or without a known post.
CREATE TABLE IF NOT EXISTS image_artists(
  asset TEXT NOT NULL,
  artist_id TEXT NOT NULL REFERENCES artists(id),
  method TEXT NOT NULL,                             -- 'pixel_hash' | 'gallery_match' | 'same_work' | 'save_session' | …
  score REAL,
  evidence TEXT NOT NULL DEFAULT '{}',
  status TEXT NOT NULL CHECK(status IN ('auto','candidate')),
  updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  PRIMARY KEY(asset, artist_id));

-- Candidate artists from a seed source, before local verification (docs/PROVENANCE.md section 5).
CREATE TABLE IF NOT EXISTS seeds(
  artist_id TEXT NOT NULL REFERENCES artists(id),
  source TEXT NOT NULL,                             -- 'pixel_hash' | 'ocr' | 'pixiv_following' | 'x_following' | 'pixiv_related' | …
  evidence TEXT NOT NULL DEFAULT '{}',
  added_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  PRIMARY KEY(artist_id, source));

-- Raw leads from a bound account, before they are resolved to an artist (v2): e.g. the numeric ids an X archive's
-- following list holds. Only the ids are kept, nothing else from the account.
CREATE TABLE IF NOT EXISTS leads(
  source TEXT NOT NULL,                             -- 'x_following' | …
  system TEXT NOT NULL,                             -- the link system the key belongs to: 'x_id' | 'pixiv' | …
  key TEXT NOT NULL,
  status TEXT NOT NULL DEFAULT 'pending' CHECK(status IN ('pending','resolved','unresolved')),
  artist_id TEXT REFERENCES artists(id),
  next_due_at TEXT,                                 -- an unresolved lead is asked again later: Danbooru keeps adding artists
  added_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  PRIMARY KEY(source, system, key));

-- Per-target lookup history and retry ladder (after Monbooru's image_lookups); errors never count as misses.
CREATE TABLE IF NOT EXISTS lookups(
  target TEXT NOT NULL,                             -- an asset, or 'artist:<id>'
  backend TEXT NOT NULL,                            -- 'pixel_hash' | 'danbooru_gallery' | 'pixiv_gallery' | …
  attempts INTEGER NOT NULL DEFAULT 0,
  last_at TEXT, last_result TEXT NOT NULL DEFAULT '',
  next_due_at TEXT, cursor TEXT,
  PRIMARY KEY(target, backend));

-- Local OCR results (never sent anywhere).
CREATE TABLE IF NOT EXISTS marks(
  asset TEXT NOT NULL, idx INTEGER NOT NULL, text TEXT NOT NULL, conf REAL, box TEXT NOT NULL,
  kind TEXT NOT NULL DEFAULT '', engine TEXT NOT NULL, PRIMARY KEY(asset, idx, engine));

-- Signatures of public reference thumbnails (the thumbnails themselves are not kept): 16x16 grey grid and, from v5,
-- the 24x24 colour check, so a rerun or new library pictures are compared without downloading anything again.
CREATE TABLE IF NOT EXISTS ref_sigs(
  site TEXT NOT NULL, post_id TEXT NOT NULL, page INTEGER NOT NULL DEFAULT 0, artist_id TEXT,
  ratio REAL NOT NULL, grid BLOB NOT NULL, url TEXT, colour BLOB, PRIMARY KEY(site, post_id, page));

-- One row per run (v3), so the app can show when it last ran and what it found. `report` is report.run's coverage.
CREATE TABLE IF NOT EXISTS runs(
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  kind TEXT NOT NULL,                               -- 'full' | 'new' (after a sync) | 'cli'
  steps TEXT NOT NULL,
  started_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  finished_at TEXT,
  status TEXT NOT NULL DEFAULT 'running',           -- 'running' | 'done' | 'stopped' | 'failed'; 'running' with no process = cut off
  reason TEXT NOT NULL DEFAULT '',                  -- why it stopped: budget, a site refusing, an error
  results TEXT NOT NULL DEFAULT '{}',               -- step -> its counts
  found INTEGER NOT NULL DEFAULT 0,                 -- pictures that got their first artist in this run
  traffic_mb REAL NOT NULL DEFAULT 0,
  report TEXT);

-- Permanent cache of outside answers.
CREATE TABLE IF NOT EXISTS ext_cache(key TEXT PRIMARY KEY, body BLOB NOT NULL, fetched_at TEXT NOT NULL);

PRAGMA user_version=5;
