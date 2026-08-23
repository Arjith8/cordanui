//! Database access layer for the TUI.
//!
//! Uses `cordanui_sync::Database` (libSQL) — local-first with optional
//! Turso embedded replica sync. Same schema, same queries as before, just
//! backed by libSQL instead of rusqlite.
//!
//! When `~/.config/cordanui/config.toml` contains a `[turso]` section, the
//! database opens as an embedded replica and syncs to Turso. Otherwise it's
//! local-only.

use cordanui_schema::{AgentStatus, CreateGoalInput, Goal, GoalStatus, UpdateGoalInput};
use cordanui_sync::{Database, SyncConfig, Value};

/// Open the database. If a Turso config exists at
/// `~/.config/cordanui/config.toml`, opens as an embedded replica.
/// Otherwise opens local-only.
pub fn open() -> anyhow::Result<Database> {
    let config = SyncConfig::load()?;
    Database::open(&config)
}

/// Whether sync (embedded replica) is enabled.
pub fn is_sync_enabled(db: &Database) -> bool {
    db.is_sync_enabled()
}

/// Trigger a manual sync. No-op if sync is not enabled.
pub fn sync(db: &Database) -> anyhow::Result<()> {
    db.sync()
}

// ---------- public API ----------

const SELECT_COLS: &str = "id, title, description, status, parent_id, sort_order, \
     created_at, updated_at, completed_at, agent_status, agent_result, \
     agent_progress, metadata";

/// Fetch all goals, ordered: roots first, then children grouped by parent,
/// each bucket sorted by `sort_order` then `created_at`.
pub fn get_all(db: &Database) -> anyhow::Result<Vec<Goal>> {
    let result = db.query_simple(
        &format!("SELECT {SELECT_COLS} FROM goals ORDER BY parent_id IS NOT NULL, parent_id, sort_order, created_at"),
    )?;
    Ok(result.rows().iter().map(values_to_goal).collect())
}

/// Fetch a single goal by ID. Returns `None` if not found.
pub fn get(db: &Database, id: &str) -> anyhow::Result<Option<Goal>> {
    let result = db.query_first(
        &format!("SELECT {SELECT_COLS} FROM goals WHERE id = ?"),
        vec![Value::from(id)],
    )?;
    Ok(result.map(|row| values_to_goal(&row)))
}

/// Create a new goal. Returns the created row.
///
/// Goal IDs are hierarchical paths: `<parent-id>.<uuid>` for subgoals,
/// plain `<uuid>` for roots. A goal's ID therefore encodes its entire
/// ancestry chain, e.g. `a1b2....c3d4.e5f6....7890`.
pub fn create(db: &Database, input: CreateGoalInput) -> anyhow::Result<Goal> {
    let new_seg = cordanui_schema::new_id();
    let id = match &input.parent_id {
        Some(parent_id) => {
            // Verify the parent exists and inherit its full path as prefix.
            match get(db, parent_id)? {
                Some(_) => format!("{parent_id}.{new_seg}"),
                None => anyhow::bail!("create: parent goal not found: {parent_id}"),
            }
        }
        None => new_seg,
    };
    let ts = cordanui_schema::now_iso();
    db.execute(
        "INSERT INTO goals (id, title, description, status, parent_id, sort_order, created_at, updated_at) \
         VALUES (?, ?, ?, 'pending', ?, ?, ?, ?)",
        vec![
            Value::from(id.clone()),
            Value::from(input.title),
            input.description.map(Value::from).unwrap_or(Value::Null),
            input.parent_id.map(Value::from).unwrap_or(Value::Null),
            Value::from(input.sort_order.unwrap_or(0)),
            Value::from(ts.clone()),
            Value::from(ts),
        ],
    )?;
    get(db, &id)?.ok_or_else(|| anyhow::anyhow!("create: insert returned no row"))
}

/// Update fields on an existing goal. Only fields set in `input` are written;
/// `updated_at` is always bumped.
pub fn update(db: &Database, id: &str, input: UpdateGoalInput) -> anyhow::Result<Option<Goal>> {
    if input.is_empty() {
        return get(db, id);
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
    get(db, id)
}

/// Mark a goal as completed.
pub fn complete(db: &Database, id: &str) -> anyhow::Result<Option<Goal>> {
    let ts = cordanui_schema::now_iso();
    update(
        db,
        id,
        UpdateGoalInput {
            status: Some(GoalStatus::Completed),
            completed_at: Some(Some(ts)),
            ..Default::default()
        },
    )
}

/// Revert a completed goal back to pending.
pub fn uncomplete(db: &Database, id: &str) -> anyhow::Result<Option<Goal>> {
    update(
        db,
        id,
        UpdateGoalInput {
            status: Some(GoalStatus::Pending),
            completed_at: Some(None),
            ..Default::default()
        },
    )
}

/// Delete a goal. `ON DELETE CASCADE` removes all subgoals.
pub fn delete(db: &Database, id: &str) -> anyhow::Result<()> {
    db.execute(
        "DELETE FROM goals WHERE id = ?",
        vec![Value::from(id)],
    )?;
    Ok(())
}

/// Fetch root goals (parent_id IS NULL).
#[cfg(test)]
mod tests {
    use super::*;
    use cordanui_sync::SyncConfig;

    fn test_db_named(name: &str) -> Database {
        let dir = std::env::temp_dir().join(format!("cordanui-tui-db-test-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let config = SyncConfig {
            db_path: dir.join("test.db"),
            ..Default::default()
        };
        Database::open(&config).unwrap()
    }

    fn test_db() -> Database {
        // Unique per invocation so parallel tests never share a file.
        test_db_named(&cordanui_schema::new_id())
    }

    #[test]
    fn hierarchical_ids() {
        let db = test_db();

        let root = create(
            &db,
            CreateGoalInput {
                title: "root".into(),
                description: None,
                parent_id: None,
                sort_order: Some(0),
            },
        )
        .unwrap();
        // Root: plain uuid, no dots.
        assert!(!root.id.contains('.'), "root id should have no dots: {}", root.id);

        let child = create(
            &db,
            CreateGoalInput {
                title: "child".into(),
                description: None,
                parent_id: Some(root.id.clone()),
                sort_order: Some(0),
            },
        )
        .unwrap();
        // Child: <root-id>.<uuid>
        assert!(
            child.id.starts_with(&format!("{}.", root.id)),
            "child id should extend root path: {}",
            child.id
        );
        assert_eq!(child.parent_id.as_deref(), Some(root.id.as_str()));

        let grandchild = create(
            &db,
            CreateGoalInput {
                title: "grandchild".into(),
                description: None,
                parent_id: Some(child.id.clone()),
                sort_order: Some(0),
            },
        )
        .unwrap();
        // Grandchild: <root-id>.<child-uuid>.<uuid>
        assert!(
            grandchild.id.starts_with(&format!("{}.", child.id)),
            "grandchild id should extend child path: {}",
            grandchild.id
        );
        assert_eq!(grandchild.id.matches('.').count(), 2);
    }

    #[test]
    fn create_under_missing_parent_fails() {
        let db = test_db();
        let result = create(
            &db,
            CreateGoalInput {
                title: "orphan".into(),
                description: None,
                parent_id: Some("nonexistent".into()),
                sort_order: Some(0),
            },
        );
        assert!(result.is_err());
    }
}

pub fn get_roots(db: &Database) -> anyhow::Result<Vec<Goal>> {
    let result = db.query_simple(
        &format!("SELECT {SELECT_COLS} FROM goals WHERE parent_id IS NULL ORDER BY sort_order, created_at"),
    )?;
    Ok(result.rows().iter().map(values_to_goal).collect())
}

/// Fetch immediate children of a goal.
pub fn get_children(db: &Database, parent_id: &str) -> anyhow::Result<Vec<Goal>> {
    let result = db.query(
        &format!("SELECT {SELECT_COLS} FROM goals WHERE parent_id = ? ORDER BY sort_order, created_at"),
        vec![Value::from(parent_id)],
    )?;
    Ok(result.rows().iter().map(values_to_goal).collect())
}

/// Get the next available sort_order for a new goal under `parent_id`
/// (or at the root level if `parent_id` is None).
pub fn next_sort_order(db: &Database, parent_id: Option<&str>) -> anyhow::Result<i64> {
    match parent_id {
        Some(pid) => db.query_scalar_i64(
            "SELECT COALESCE(MAX(sort_order), -1) + 1 FROM goals WHERE parent_id = ?",
            vec![Value::from(pid)],
        ),
        None => db.query_scalar_i64(
            "SELECT COALESCE(MAX(sort_order), -1) + 1 FROM goals WHERE parent_id IS NULL",
            vec![],
        ),
    }
}

// ---------- helpers ----------

/// Map a row of `Value`s to a `Goal`. Column order must match
/// `SELECT_COLS`.
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
