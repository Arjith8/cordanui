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
pub const SCHEMA_SQL: &str = include_str!("../../../schema/schema.sql");

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
