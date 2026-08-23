//! Database access for the agent backend.
//!
//! Uses `cordanui_sync::Database` (libSQL) — local-first with optional
//! Turso embedded replica sync. Same schema as the TUI.
//!
//! The agent backend only needs to: read a goal by ID, update agent_status,
//! write agent_progress, and write agent_result. It doesn't do general CRUD.

use anyhow::Result;
use cordanui_schema::{AgentStatus, Goal, GoalStatus, UpdateGoalInput};
use cordanui_sync::{Database, SyncConfig, Value};

/// Open the database. If a Turso config exists at
/// `~/.config/cordanui/config.toml`, opens as an embedded replica.
/// Otherwise opens local-only.
pub fn open(path: Option<&std::path::Path>) -> Result<Database> {
    let config = match path {
        Some(p) => SyncConfig {
            db_path: p.to_path_buf(),
            ..Default::default()
        },
        None => SyncConfig::load()?,
    };
    Database::open(&config)
}

/// Fetch a goal by ID. Returns None if not found.
pub fn get_goal(db: &Database, id: &str) -> Result<Option<Goal>> {
    let result = db.query_first(
        "SELECT id, title, description, status, parent_id, sort_order, \
         created_at, updated_at, completed_at, agent_status, agent_result, \
         agent_progress, metadata \
         FROM goals WHERE id = ?",
        vec![Value::from(id)],
    )?;
    Ok(result.map(|row| values_to_goal(&row)))
}

/// Update a goal. Only writes fields set in `input`.
pub fn update_goal(db: &Database, id: &str, input: UpdateGoalInput) -> Result<()> {
    if input.is_empty() {
        return Ok(());
    }

    let mut fields: Vec<&str> = Vec::new();
    let mut params: Vec<Value> = Vec::new();

    if let Some(title) = input.title {
        fields.push("title = ?");
        params.push(Value::from(title));
    }
    if let Some(desc) = input.description {
        fields.push("description = ?");
        params.push(desc.map(Value::from).unwrap_or(Value::Null));
    }
    if let Some(status) = input.status {
        fields.push("status = ?");
        params.push(Value::from(status.as_str()));
    }
    if let Some(sort) = input.sort_order {
        fields.push("sort_order = ?");
        params.push(Value::from(sort));
    }
    if let Some(completed_at) = input.completed_at {
        fields.push("completed_at = ?");
        params.push(completed_at.map(Value::from).unwrap_or(Value::Null));
    }
    if let Some(agent_status) = input.agent_status {
        fields.push("agent_status = ?");
        params.push(
            agent_status
                .map(|s| Value::from(s.as_str()))
                .unwrap_or(Value::Null),
        );
    }
    if let Some(agent_result) = input.agent_result {
        fields.push("agent_result = ?");
        params.push(agent_result.map(Value::from).unwrap_or(Value::Null));
    }
    if let Some(agent_progress) = input.agent_progress {
        fields.push("agent_progress = ?");
        params.push(agent_progress.map(Value::from).unwrap_or(Value::Null));
    }
    if let Some(metadata) = input.metadata {
        fields.push("metadata = ?");
        params.push(metadata.map(Value::from).unwrap_or(Value::Null));
    }

    // Always bump updated_at
    fields.push("updated_at = ?");
    params.push(Value::from(cordanui_schema::now_iso()));

    params.push(Value::from(id));

    let sql = format!("UPDATE goals SET {} WHERE id = ?", fields.join(", "));
    db.execute(&sql, params)?;
    Ok(())
}

/// Set agent_status to Running.
pub fn set_agent_running(db: &Database, id: &str) -> Result<()> {
    update_goal(
        db,
        id,
        UpdateGoalInput {
            agent_status: Some(Some(AgentStatus::Running)),
            ..Default::default()
        },
    )
}

/// Write a progress update.
pub fn write_progress(db: &Database, id: &str, progress_json: &str) -> Result<()> {
    update_goal(
        db,
        id,
        UpdateGoalInput {
            agent_progress: Some(Some(progress_json.to_string())),
            ..Default::default()
        },
    )
}

/// Write the final result and mark the goal completed.
pub fn write_result(db: &Database, id: &str, result_json: &str) -> Result<()> {
    update_goal(
        db,
        id,
        UpdateGoalInput {
            agent_status: Some(Some(AgentStatus::Completed)),
            agent_result: Some(Some(result_json.to_string())),
            status: Some(GoalStatus::Completed),
            completed_at: Some(Some(cordanui_schema::now_iso())),
            ..Default::default()
        },
    )
}

/// Mark the goal as failed.
pub fn write_failure(db: &Database, id: &str, error_message: &str) -> Result<()> {
    let error_json = serde_json::json!({ "error": error_message }).to_string();
    update_goal(
        db,
        id,
        UpdateGoalInput {
            agent_status: Some(Some(AgentStatus::Failed)),
            agent_result: Some(Some(error_json)),
            ..Default::default()
        },
    )
}

// ---------- helpers ----------

fn values_to_goal(row: &Vec<Value>) -> Goal {
    let get_str = |i: usize| -> String {
        match row.get(i) {
            Some(Value::Text(s)) => s.clone(),
            _ => String::new(),
        }
    };
    let get_opt_str = |i: usize| -> Option<String> {
        match row.get(i) {
            Some(Value::Text(s)) => Some(s.clone()),
            _ => None,
        }
    };
    let get_i64 = |i: usize| -> i64 {
        match row.get(i) {
            Some(Value::Integer(n)) => *n,
            _ => 0,
        }
    };

    let status_str = get_str(3);
    let agent_status_str = get_opt_str(9);

    Goal {
        id: get_str(0),
        title: get_str(1),
        description: get_opt_str(2),
        status: GoalStatus::from_db(&status_str),
        parent_id: get_opt_str(4),
        sort_order: get_i64(5),
        created_at: get_str(6),
        updated_at: get_str(7),
        completed_at: get_opt_str(8),
        agent_status: agent_status_str.map(|s| AgentStatus::from_db(&s)),
        agent_result: get_opt_str(10),
        agent_progress: get_opt_str(11),
        metadata: get_opt_str(12),
    }
}
