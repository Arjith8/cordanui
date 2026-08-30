CREATE TABLE IF NOT EXISTS plugins (
    id           TEXT PRIMARY KEY,
    source       TEXT NOT NULL,
    dir          TEXT NOT NULL,
    active       INTEGER NOT NULL DEFAULT 0,
    installed_at TEXT NOT NULL
);
