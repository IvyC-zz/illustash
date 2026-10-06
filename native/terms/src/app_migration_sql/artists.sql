CREATE TABLE IF NOT EXISTS artist_verdicts(uuid TEXT NOT NULL,artist TEXT NOT NULL,
 verdict TEXT NOT NULL CHECK(verdict IN ('yes','no')),created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,PRIMARY KEY(uuid,artist));
CREATE INDEX IF NOT EXISTS artist_verdicts_artist ON artist_verdicts(artist,verdict);
