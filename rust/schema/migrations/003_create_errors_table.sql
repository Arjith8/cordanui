CREATE TABLE IF NOT EXISTS errors (
    id         TEXT PRIMARY KEY,
    context    TEXT NOT NULL,
    message    TEXT NOT NULL,
    detail     TEXT,
    created_at TEXT NOT NULL
);
