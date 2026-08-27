-- cordanui schema (canonical)
-- Single source of truth for the data model. All clients (TUI, mobile,
-- agent backend) adhere to this schema. Turso/SQLite-compatible.

CREATE TABLE IF NOT EXISTS goals (
    id           TEXT PRIMARY KEY,        -- UUID
    title        TEXT NOT NULL,
    description  TEXT,
    status       TEXT NOT NULL DEFAULT 'pending',
                 -- pending | in_progress | completed | agent_mode
    parent_id    TEXT REFERENCES goals(id) ON DELETE CASCADE,
                 -- NULL = top-level goal
    sheet_id     TEXT REFERENCES goal_sheets(id) ON DELETE SET NULL,
                 -- NULL = default sheet (mobile organizes goals into sheets)
    sort_order   INTEGER NOT NULL DEFAULT 0,
    created_at   TEXT NOT NULL,
    updated_at   TEXT NOT NULL,
    completed_at TEXT,
    -- agent fields (only populated when status = 'agent_mode')
    agent_status   TEXT,   -- queued | running | completed | failed
    agent_result   TEXT,   -- JSON output from the agent
    agent_progress TEXT,   -- streaming progress, JSON
    -- plugin extensibility
    metadata      TEXT,     -- JSON, plugins can attach arbitrary data
    -- soft-delete tombstone (sync): set = row is deleted on this device,
    -- propagates to other clients via sync; readers must filter NULL
    deleted_at    TEXT
);

-- Goal sheets (mobile organizational grouping; TUI ignores them).
CREATE TABLE IF NOT EXISTS goal_sheets (
    id         TEXT PRIMARY KEY,        -- UUID
    name       TEXT NOT NULL,
    created_at TEXT NOT NULL,
    deleted_at TEXT                      -- soft-delete tombstone (sync)
);

CREATE INDEX IF NOT EXISTS idx_goals_parent_id ON goals(parent_id);
CREATE INDEX IF NOT EXISTS idx_goals_status ON goals(status);
CREATE INDEX IF NOT EXISTS idx_goals_sort_order ON goals(sort_order);
CREATE INDEX IF NOT EXISTS idx_goals_sheet_id ON goals(sheet_id);

-- Themes (shared with mobile clients; see agent-docs/theme-system-spec.md).
-- `colors_json` is a JSON object mapping any subset of the canonical token
-- names to hex colors; missing tokens fall back to client-side defaults.
CREATE TABLE IF NOT EXISTS themes (
    id           TEXT PRIMARY KEY,        -- 'builtin-dark', 'builtin-light', or UUID for plugin themes
    name         TEXT NOT NULL,
    source       TEXT NOT NULL DEFAULT 'builtin',
                 -- builtin | GitHub repo URL of the plugin that added the theme
    colors_json  TEXT NOT NULL,
    last_used_at TEXT,                    -- NULL until explicitly selected; drives MRU ordering
    deleted_at   TEXT                     -- soft-delete tombstone (sync)
);

-- Generic key-value settings shared across clients.
-- Known keys: theme_mode ('system'|'explicit'), selected_theme_id (themes.id).
CREATE TABLE IF NOT EXISTS settings (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

-- Installed plugins. `source` is the GitHub URL the plugin came from;
-- rows are listed most-recent-first; `active` gates whether the host uses it.
CREATE TABLE IF NOT EXISTS plugins (
    id           TEXT PRIMARY KEY,        -- manifest [plugin].name
    source       TEXT NOT NULL,           -- GitHub repo URL
    dir          TEXT NOT NULL,           -- install directory on disk
    active       INTEGER NOT NULL DEFAULT 0,
    installed_at TEXT NOT NULL
);

-- Error / diagnostics log. Every client writes failures it couldn't handle
-- (sync errors, agent crashes, plugin install failures, ...). Reviewable in
-- the TUI (errors view) and on mobile's profile page. Writers must never
-- fail because of this table.
CREATE TABLE IF NOT EXISTS errors (
    id         TEXT PRIMARY KEY,          -- UUID
    context    TEXT NOT NULL,             -- subsystem: 'sync', 'agent', 'plugin', ...
    message    TEXT NOT NULL,
    detail     TEXT,
    created_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_errors_created_at ON errors(created_at);

-- Shared migration bookkeeping. Both clients (TUI + mobile) run the same
-- migration list and record applied versions here, so any database file —
-- regardless of which client created it — has identical schema state.
CREATE TABLE IF NOT EXISTS _migrations (
    version    INTEGER PRIMARY KEY,
    name       TEXT NOT NULL,
    applied_at TEXT NOT NULL
);

-- Device-local sync outbox: rows pending push to the remote. Populated by
-- the client's write layer, drained by the sync layer. The leading
-- underscore marks it device-local — never synced.
CREATE TABLE IF NOT EXISTS _outbox (
    table_name TEXT NOT NULL,
    row_id     TEXT NOT NULL,
    PRIMARY KEY (table_name, row_id)
);

-- Device-local sync cursors (last_pull / last_push) and device identity.
-- Also never synced.
CREATE TABLE IF NOT EXISTS _sync_state (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
