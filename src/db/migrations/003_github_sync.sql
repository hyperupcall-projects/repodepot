-- Set by a sync when GitHub says the repo doesn't exist (or isn't visible to the token).
ALTER TABLE repos ADD COLUMN not_found_at TEXT;

-- Set by a sync when GitHub says the repo now lives under another name.
ALTER TABLE repos ADD COLUMN renamed_to_id INTEGER REFERENCES repos(id) ON DELETE SET NULL;

-- Small bits of app state, e.g. when the last sync finished.
CREATE TABLE meta (
    key    TEXT PRIMARY KEY,
    value  TEXT NOT NULL
);
