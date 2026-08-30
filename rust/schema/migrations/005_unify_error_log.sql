CREATE TABLE IF NOT EXISTS errors_mobile (
    id         TEXT PRIMARY KEY,
    context    TEXT NOT NULL,
    message    TEXT NOT NULL,
    detail     TEXT,
    created_at TEXT NOT NULL
);
INSERT OR IGNORE INTO errors (id, context, message, detail, created_at) SELECT id, context, message, detail, created_at FROM errors_mobile;
DROP TABLE errors_mobile;
