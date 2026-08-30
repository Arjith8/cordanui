ALTER TABLE goals ADD COLUMN deleted_at TEXT;
ALTER TABLE themes ADD COLUMN deleted_at TEXT;
ALTER TABLE goal_sheets ADD COLUMN deleted_at TEXT;
CREATE TABLE IF NOT EXISTS _outbox (
    table_name TEXT NOT NULL,
    row_id     TEXT NOT NULL,
    PRIMARY KEY (table_name, row_id)
);
CREATE TABLE IF NOT EXISTS _sync_state (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
