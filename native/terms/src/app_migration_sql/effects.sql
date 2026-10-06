-- Existing Python migration SQL; keep rules unchanged.
CREATE TABLE IF NOT EXISTS backend_effects(
    id INTEGER PRIMARY KEY, kind TEXT NOT NULL, status TEXT NOT NULL DEFAULT 'pending',
    error TEXT NOT NULL DEFAULT '', created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);
