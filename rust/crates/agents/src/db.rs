//! Database access layer for the agent backend.
//!
//! Shares the same `cordanui_sync::Database` as the TUI — same local
//! SQLite file, same schema, same Hrana-over-HTTP sync. The backend
//! calls `db.sync()` before polling for queued tasks (pull remote
//! writes from mobile/TUI) and after writing results (push them out).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use cordanui_schema::{AgentStatus, Goal, GoalStatus};
use cordanui_sync::{Database, Value};

/// Sync the local DB with Turso (push + pull). Required before reading
/// queued tasks and after writing results.
pub fn sync(db: &Database) -> Result<()> {
    db.sync().context("sync failed")
}

/// Query for all goals queued for agent execution: `status = 'agent_mode'`
/// AND `agent_status = 'queued'`. Non-deleted only.
pub fn get_queued_tasks(db: &Database) -> Result<Vec<String>> {
    let result = db.query(
        "SELECT id FROM goals \
         WHERE status = 'agent_mode' AND agent_status = 'queued' \
         AND deleted_at IS NULL \
         ORDER BY updated_at",
        vec![],
    )?;
    Ok(result
        .rows()
        .iter()
        .filter_map(|row| match row.first() {
            Some(Value::Text(id)) => Some(id.clone()),
            _ => None,
        })
        .collect())
}

/// Fetch a single goal by ID (non-deleted). Returns None if not found.
pub fn get_goal(db: &Database, id: &str) -> Result<Option<Goal>> {
    let result = db.query_first(
        "SELECT id, title, description, status, parent_id, sort_order, \
         created_at, updated_at, completed_at, agent_status, agent_result, \
         agent_progress, metadata \
         FROM goals WHERE id = ? AND deleted_at IS NULL",
        vec![Value::from(id)],
    )?;
    Ok(result.map(|row| values_to_goal(&row)))
}

/// Mark a goal as `agent_status = 'running'` and bump `updated_at`.
pub fn set_running(db: &Database, id: &str) -> Result<()> {
    db.execute(
        "UPDATE goals SET agent_status = 'running', updated_at = ? WHERE id = ?",
        vec![Value::from(cordanui_schema::now_iso()), Value::from(id)],
    )?;
    db.mark_dirty("goals", id)?;
    Ok(())
}

/// Write streaming progress to a goal. Does NOT bump updated_at (progress
/// updates are high-frequency; LWW on updated_at would cause churn).
pub fn set_progress(db: &Database, id: &str, progress_json: &str) -> Result<()> {
    db.execute(
        "UPDATE goals SET agent_progress = ? WHERE id = ?",
        vec![Value::from(progress_json), Value::from(id)],
    )?;
    db.mark_dirty("goals", id)?;
    Ok(())
}

/// Write the final result: `agent_status`, `agent_result`, and bump
/// `updated_at` so the change wins LWW and syncs to other clients.
pub fn set_result(
    db: &Database,
    id: &str,
    status: AgentStatus,
    result: Option<&str>,
) -> Result<()> {
    let now = cordanui_schema::now_iso();
    db.execute(
        "UPDATE goals SET agent_status = ?, agent_result = ?, updated_at = ? WHERE id = ?",
        vec![
            Value::from(status.as_str()),
            result.map(Value::from).unwrap_or(Value::Null),
            Value::from(now),
            Value::from(id),
        ],
    )?;
    db.mark_dirty("goals", id)?;
    Ok(())
}

/// Merge a JSON object patch into a goal's `metadata` column (read-modify-write).
/// Null metadata is created; invalid JSON is replaced. `patch` keys equal to
/// `Value::Null` delete the key. Used so agent plugins can declare mobile widgets
/// via files like `mobile.json` or `__metadata__.json` in their result.
pub fn merge_metadata(db: &Database, id: &str, patch: serde_json::Value) -> Result<()> {
    let goal = get_goal(db, id)?.ok_or_else(|| anyhow::anyhow!("goal not found: {id}"))?;
    let mut meta: serde_json::Map<String, serde_json::Value> = goal
        .metadata
        .as_deref()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default();
    if let Some(obj) = patch.as_object() {
        for (k, v) in obj {
            if v.is_null() {
                meta.remove(k);
            } else {
                meta.insert(k.clone(), v.clone());
            }
        }
    }
    let merged = serde_json::Value::Object(meta).to_string();
    let now = cordanui_schema::now_iso();
    db.execute(
        "UPDATE goals SET metadata = ?, updated_at = ? WHERE id = ?",
        vec![Value::from(merged), Value::from(now), Value::from(id)],
    )?;
    db.mark_dirty("goals", id)?;
    Ok(())
}

// ---------- plugin resolution ----------

/// A row of the `plugins` table.
#[derive(Debug, Clone)]
pub struct PluginRow {
    pub id: String,
    pub dir: String,
    pub active: bool,
}

/// All installed plugins from the registry.
pub fn list_plugins(db: &Database) -> Result<Vec<PluginRow>> {
    let result = db.query_simple(
        "SELECT id, dir, active FROM plugins ORDER BY installed_at DESC, id",
    )?;
    Ok(result
        .rows()
        .iter()
        .map(|row| PluginRow {
            id: text(row, 0),
            dir: text(row, 1),
            active: matches!(row.get(2), Some(Value::Integer(n)) if *n != 0),
        })
        .collect())
}

/// Get a plugin's settings from the DB (namespaced keys stripped to bare
/// field keys). Mirrors the TUI's `db::get_plugin_settings` but without
/// the config.toml mirror (the backend doesn't write config files).
pub fn get_plugin_settings(
    db: &Database,
    plugin: &str,
) -> Result<BTreeMap<String, String>> {
    let escaped = plugin.replace('\'', "''").replace('%', "\\%").replace('_', "\\_");
    let result = db.query_simple(&format!(
        "SELECT key, value FROM settings WHERE key LIKE '{escaped}.%'"
    ))?;
    let prefix = format!("{plugin}.");
    let mut map = BTreeMap::new();
    for row in result.rows() {
        if let (Some(Value::Text(k)), Some(Value::Text(v))) = (row.first(), row.get(1)) {
            if let Some(bare) = k.strip_prefix(&prefix) {
                map.insert(bare.to_string(), v.clone());
            }
        }
    }
    Ok(map)
}

/// Serialize settings into the `config` JSON object for plugin invocations.
/// Empty map → None.
pub fn settings_to_config(values: &BTreeMap<String, String>) -> Option<serde_json::Value> {
    if values.is_empty() {
        return None;
    }
    let mut obj = serde_json::Map::new();
    for (k, v) in values {
        obj.insert(k.clone(), serde_json::Value::String(v.clone()));
    }
    Some(serde_json::Value::Object(obj))
}

/// Read the `agent.url` setting (written by the TUI when it announces
/// agent capability). Returns None if not set.
pub fn get_agent_url(db: &Database) -> Option<String> {
    db.query_first(
        "SELECT value FROM settings WHERE key = 'agent.url'",
        vec![],
    )
    .ok()
    .flatten()
    .and_then(|row| match row.first() {
        Some(Value::Text(v)) => Some(v.clone()),
        _ => None,
    })
}

// ---------- helpers ----------

fn text(row: &Vec<Value>, i: usize) -> String {
    match row.get(i) {
        Some(Value::Text(s)) => s.clone(),
        _ => String::new(),
    }
}

fn values_to_goal(row: &Vec<Value>) -> Goal {
    let opt_str = |i: usize| -> Option<String> {
        match row.get(i) {
            Some(Value::Text(s)) => Some(s.clone()),
            _ => None,
        }
    };
    Goal {
        id: text(row, 0),
        title: text(row, 1),
        description: opt_str(2),
        status: GoalStatus::from_db(&text(row, 3)),
        parent_id: opt_str(4),
        sort_order: 0,
        created_at: text(row, 5),
        updated_at: text(row, 6),
        completed_at: opt_str(7),
        agent_status: opt_str(8).map(|s| AgentStatus::from_db(&s)),
        agent_result: opt_str(9),
        agent_progress: opt_str(10),
        metadata: opt_str(11),
    }
}

/// Unused but referenced by the public API surface; avoids dead-code
/// warnings for the PathBuf import in contexts where only query helpers
/// are used.
#[allow(dead_code)]
pub fn plugin_dir_path(dir: &str) -> PathBuf {
    PathBuf::from(dir)
}

#[cfg(test)]
mod tests {
    use super::*;
    use cordanui_sync::SyncConfig;

    fn test_db() -> Database {
        let dir = std::env::temp_dir().join(format!(
            "cordanui-agents-test-{}",
            cordanui_schema::new_id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let config = SyncConfig {
            db_path: dir.join("test.db"),
            ..Default::default()
        };
        Database::open(&config).unwrap()
    }

    #[test]
    fn queue_lifecycle() {
        let db = test_db();

        // Create a goal directly.
        let id = cordanui_schema::new_id();
        let ts = cordanui_schema::now_iso();
        db.execute(
            "INSERT INTO goals (id, title, status, sort_order, created_at, updated_at) \
             VALUES (?, 'test', 'pending', 0, ?, ?)",
            vec![Value::from(id.clone()), Value::from(ts.clone()), Value::from(ts)],
        )
        .unwrap();

        // No queued tasks initially.
        assert!(get_queued_tasks(&db).unwrap().is_empty());

        // Flip to agent_mode + queued.
        db.execute(
            "UPDATE goals SET status = 'agent_mode', agent_status = 'queued', updated_at = ? WHERE id = ?",
            vec![Value::from(cordanui_schema::now_iso()), Value::from(&id)],
        )
        .unwrap();
        db.mark_dirty("goals", &id).unwrap();

        // Now it shows as queued.
        let queued = get_queued_tasks(&db).unwrap();
        assert_eq!(queued, vec![id.clone()]);

        // Mark running.
        set_running(&db, &id).unwrap();
        assert!(get_queued_tasks(&db).unwrap().is_empty());

        // Write result.
        set_result(&db, &id, AgentStatus::Completed, Some("done!")).unwrap();
        let goal = get_goal(&db, &id).unwrap().unwrap();
        assert_eq!(goal.agent_status, Some(AgentStatus::Completed));
        assert_eq!(goal.agent_result.as_deref(), Some("done!"));
    }
}
