ALTER TABLE goals ADD COLUMN due_at TEXT;
ALTER TABLE goals ADD COLUMN remind_at TEXT;
ALTER TABLE goals ADD COLUMN repeat_rule TEXT;
CREATE INDEX IF NOT EXISTS idx_goals_due_at ON goals(due_at);
