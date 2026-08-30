CREATE TABLE IF NOT EXISTS goal_sheets (
    id         TEXT PRIMARY KEY,
    name       TEXT NOT NULL,
    created_at TEXT NOT NULL
);
ALTER TABLE goals ADD COLUMN sheet_id TEXT REFERENCES goal_sheets(id) ON DELETE SET NULL;
CREATE INDEX IF NOT EXISTS idx_goals_sheet_id ON goals(sheet_id);
