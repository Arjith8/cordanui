//! cordanui schema — shared data types + SQL migrations.
//!
//! This crate is the single source of truth for the goal data model in Rust.
//! It mirrors `schema/schema.sql` (the canonical SQL). Both the TUI and the
//! agent backend depend on this crate.
//!
//! Phase 1: no agent fields are populated; they exist in the schema for
//! forward compatibility with phase 6 (agent backend).

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// The canonical SQL schema. Embedded so any Rust client can bootstrap a
/// local DB on first run without shipping a separate .sql file.
///
/// This always describes the **latest** shape — fresh installs get it
/// directly, existing databases are brought up to date by [`MIGRATIONS`].
pub const SCHEMA_SQL: &str = include_str!("../../../schema/schema.sql");

/// A single forward-only schema migration.
///
/// Invariants:
/// - Versions are strictly increasing and never edited or reused once
///   shipped — append new ones at the end.
/// - `sql` must be idempotent *per version*: it runs at most once per
///   database (recorded in `_migrations`), so plain DDL is fine.
/// - Fresh installs never execute these: they get the final schema from
///   `SCHEMA_SQL` and each migration is recorded as already applied.
#[derive(Debug, Clone, Copy)]
pub struct Migration {
    pub version: i64,
    pub name: &'static str,
    pub sql: &'static str,
}

/// All migrations, oldest first. Applied on every startup; anything already
/// recorded in `_migrations` is skipped.
pub const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        name: "themes_drop_is_dark_source_is_github_url",
        sql: "ALTER TABLE themes DROP COLUMN is_dark;",
    },
    Migration {
        version: 2,
        name: "create_plugins_table",
        sql: "CREATE TABLE IF NOT EXISTS plugins (\
                  id           TEXT PRIMARY KEY,\
                  source       TEXT NOT NULL,\
                  dir          TEXT NOT NULL,\
                  active       INTEGER NOT NULL DEFAULT 0,\
                  installed_at TEXT NOT NULL\
              );",
    },
    Migration {
        version: 3,
        name: "create_errors_table",
        sql: "CREATE TABLE IF NOT EXISTS errors (\
                  id         TEXT PRIMARY KEY,\
                  context    TEXT NOT NULL,\
                  message    TEXT NOT NULL,\
                  detail     TEXT,\
                  created_at TEXT NOT NULL\
              );",
    },
    Migration {
        version: 4,
        name: "goal_sheets_and_sheet_id",
        // Mobile's sheet grouping, promoted to the shared schema. The
        // ALTER is safe on every database that runs this migration: TUI
        // databases lack sheet_id, and mobile databases that already have
        // it never execute this step (their runner records it via the
        // legacy-alignment path instead).
        sql: "CREATE TABLE IF NOT EXISTS goal_sheets (\
                  id         TEXT PRIMARY KEY,\
                  name       TEXT NOT NULL,\
                  created_at TEXT NOT NULL\
              );\
              ALTER TABLE goals ADD COLUMN sheet_id TEXT REFERENCES goal_sheets(id) ON DELETE SET NULL;\
              CREATE INDEX IF NOT EXISTS idx_goals_sheet_id ON goals(sheet_id);",
    },
    Migration {
        version: 5,
        name: "unify_error_log",
        // Mobile's device-local log merges into the shared `errors` table.
        // The CREATE makes this work on databases that never had
        // errors_mobile (nothing to copy, then dropped).
        sql: "CREATE TABLE IF NOT EXISTS errors_mobile (\
                  id         TEXT PRIMARY KEY,\
                  context    TEXT NOT NULL,\
                  message    TEXT NOT NULL,\
                  detail     TEXT,\
                  created_at TEXT NOT NULL\
              );\
              INSERT OR IGNORE INTO errors (id, context, message, detail, created_at) \
                  SELECT id, context, message, detail, created_at FROM errors_mobile;\
              DROP TABLE errors_mobile;",
    },
    Migration {
        version: 6,
        name: "soft_delete_tombstones",
        // Deletes become tombstones so they can propagate through sync.
        // Readers filter `deleted_at IS NULL`.
        sql: "ALTER TABLE goals ADD COLUMN deleted_at TEXT;\
              ALTER TABLE themes ADD COLUMN deleted_at TEXT;\
              ALTER TABLE goal_sheets ADD COLUMN deleted_at TEXT;\
              CREATE TABLE IF NOT EXISTS _outbox (\
                  table_name TEXT NOT NULL,\
                  row_id     TEXT NOT NULL,\
                  PRIMARY KEY (table_name, row_id)\
              );\
              CREATE TABLE IF NOT EXISTS _sync_state (\
                  key   TEXT PRIMARY KEY,\
                  value TEXT NOT NULL\
              );",
    },
];

/// Goal lifecycle status. Stored as TEXT in SQLite.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GoalStatus {
    Pending,
    InProgress,
    Completed,
    AgentMode,
}

impl GoalStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::InProgress => "in_progress",
            Self::Completed => "completed",
            Self::AgentMode => "agent_mode",
        }
    }

    /// Parse from a DB string. Unknown values fall back to `Pending`.
    pub fn from_db(s: &str) -> Self {
        match s {
            "pending" => Self::Pending,
            "in_progress" => Self::InProgress,
            "completed" => Self::Completed,
            "agent_mode" => Self::AgentMode,
            _ => Self::Pending,
        }
    }

    /// The single-char glyph shown in the TUI status column.
    pub fn glyph(&self) -> &str {
        match self {
            Self::Pending => "○",
            Self::InProgress => "◐",
            Self::Completed => "✓",
            Self::AgentMode => "⤴",
        }
    }
}

impl std::fmt::Display for GoalStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Agent execution status. NULL until a goal enters agent mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentStatus {
    Queued,
    Running,
    Completed,
    Failed,
}

impl AgentStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
        }
    }

    pub fn from_db(s: &str) -> Self {
        match s {
            "queued" => Self::Queued,
            "running" => Self::Running,
            "completed" => Self::Completed,
            "failed" => Self::Failed,
            _ => Self::Queued,
        }
    }
}

/// A goal row. Maps 1:1 to the `goals` table.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Goal {
    pub id: String,
    pub title: String,
    pub description: Option<String>,
    pub status: GoalStatus,
    pub parent_id: Option<String>,
    pub sort_order: i64,
    pub created_at: String,
    pub updated_at: String,
    pub completed_at: Option<String>,
    pub agent_status: Option<AgentStatus>,
    pub agent_result: Option<String>,
    pub agent_progress: Option<String>,
    pub metadata: Option<String>,
}

/// Input for creating a new goal.
#[derive(Debug, Clone, Default)]
pub struct CreateGoalInput {
    pub title: String,
    pub description: Option<String>,
    pub parent_id: Option<String>,
    pub sort_order: Option<i64>,
}

/// Input for updating an existing goal. `None` means "don't change".
///
/// Note: the wrapping `Option` on each field distinguishes "set to NULL"
/// (`Some(None)`) from "don't touch" (`None`). For non-nullable fields like
/// `title`, the inner value is always `Some`.
#[derive(Debug, Clone, Default)]
pub struct UpdateGoalInput {
    pub title: Option<String>,
    pub description: Option<Option<String>>,
    pub status: Option<GoalStatus>,
    pub sort_order: Option<i64>,
    pub completed_at: Option<Option<String>>,
    pub agent_status: Option<Option<AgentStatus>>,
    pub agent_result: Option<Option<String>>,
    pub agent_progress: Option<Option<String>>,
    pub metadata: Option<Option<String>>,
}

impl UpdateGoalInput {
    pub fn is_empty(&self) -> bool {
        self.title.is_none()
            && self.description.is_none()
            && self.status.is_none()
            && self.sort_order.is_none()
            && self.completed_at.is_none()
            && self.agent_status.is_none()
            && self.agent_result.is_none()
            && self.agent_progress.is_none()
            && self.metadata.is_none()
    }
}

/// A goal with its children expanded into a tree. Used by the TUI for the
/// expandable goal list.
#[derive(Debug, Clone)]
pub struct GoalTreeNode {
    pub goal: Goal,
    pub children: Vec<GoalTreeNode>,
}

/// Generate a new UUID v4 string.
pub fn new_id() -> String {
    Uuid::new_v4().to_string()
}

/// Current UTC timestamp in ISO 8601, matching the mobile app's format.
pub fn now_iso() -> String {
    chrono::Utc::now().to_rfc3339()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The mobile app consumes a committed TS export of this crate's
    /// schema + migrations. Fail here if someone edits the source of
    /// truth without re-running
    /// `cargo run -p cordanui-schema --example export_ts`.
    #[test]
    fn generated_ts_is_up_to_date() {
        let expected = {
            // Mirror export_ts.rs::generated_ts (kept in sync by this test).
            let mut out = String::new();
            out.push_str(
                "// AUTO-GENERATED by `cargo run -p cordanui-schema --example export_ts`.\n\
                 // Source of truth: rust/schema/schema.sql + crates/schema MIGRATIONS.\n\
                 // Do not edit by hand — regenerate instead.\n\n",
            );
            out.push_str("export const SCHEMA_SQL = ");
            out.push_str(&serde_json::to_string(SCHEMA_SQL).unwrap());
            out.push_str(";\n\n");
            let latest = MIGRATIONS.iter().map(|m| m.version).max().unwrap_or(0);
            out.push_str(&format!(
                "export const LATEST_SCHEMA_VERSION = {latest};\n\n"
            ));
            out.push_str(
                "export interface SharedMigration {\n  version: number;\n  name: string;\n  sql: string;\n}\n\n",
            );
            out.push_str("export const SHARED_MIGRATIONS: SharedMigration[] = ");
            let migrations: Vec<serde_json::Value> = MIGRATIONS
                .iter()
                .map(|m| {
                    serde_json::json!({
                        "version": m.version,
                        "name": m.name,
                        "sql": m.sql,
                    })
                })
                .collect();
            out.push_str(&serde_json::to_string_pretty(&migrations).unwrap());
            out.push_str(";\n");
            out
        };

        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../mobile/src/db/schema.generated.ts");
        let actual = std::fs::read_to_string(&path).unwrap_or_else(|e| {
            panic!(
                "cannot read {}: run `cargo run -p cordanui-schema --example export_ts` ({e})",
                path.display()
            )
        });
        assert_eq!(
            actual, expected,
            "schema.generated.ts is stale — re-run the export_ts example"
        );
    }

    #[test]
    fn versions_are_sequential_and_unique() {
        let mut seen = std::collections::HashSet::new();
        let mut last = 0;
        for m in MIGRATIONS {
            assert!(m.version > last, "versions must strictly increase");
            assert!(seen.insert(m.version), "duplicate version {}", m.version);
            assert!(!m.name.trim().is_empty());
            last = m.version;
        }
    }
}
