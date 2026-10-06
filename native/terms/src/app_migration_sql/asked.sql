-- Existing Python migration SQL; keep rules unchanged.
CREATE TABLE IF NOT EXISTS asked(uuid TEXT NOT NULL,entity TEXT NOT NULL,kind TEXT NOT NULL,
 answer TEXT NOT NULL CHECK(answer IN ('yes','no','later')),action_id INTEGER,asked_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
 PRIMARY KEY(uuid,entity));
CREATE INDEX IF NOT EXISTS asked_entity ON asked(entity,answer,asked_at);
CREATE TABLE IF NOT EXISTS audit_sample(uuid TEXT PRIMARY KEY,entity TEXT NOT NULL,stratum TEXT NOT NULL,
 solo INTEGER NOT NULL,created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);
