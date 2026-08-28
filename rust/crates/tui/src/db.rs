//! Database access layer for the TUI.
//!
//! Uses `cordanui_sync::Database` (rusqlite, bundled SQLite) — local-first,
//! synchronous, no async runtime. When `~/.config/cordanui/config.toml`
//! contains a `[turso]` section, sync is enabled: writes mark rows dirty in
//! the device-local `_outbox`, and a background worker pushes/pulls over the
//! same Hrana-over-HTTP protocol mobile uses. Otherwise it's local-only.
//!
//! Deletes are soft (`deleted_at` tombstone) so they propagate to other
//! clients via sync. All read paths filter `deleted_at IS NULL`.

use cordanui_schema::{AgentStatus, CreateGoalInput, Goal, GoalStatus, UpdateGoalInput};
use cordanui_sync::{Database, SyncConfig, Value};

/// Open the database. If a Turso config exists at
/// `~/.config/cordanui/config.toml`, sync is enabled (push/pull over
/// Hrana-over-HTTP). Otherwise opens local-only. Never touches the network.
pub fn open() -> anyhow::Result<Database> {
    let config = SyncConfig::load()?;
    Database::open(&config)
}

/// Whether sync (push/pull over Hrana) is enabled.
pub fn is_sync_enabled(db: &Database) -> bool {
    db.is_sync_enabled()
}

/// Trigger a manual sync (push outbox + pull remote). No-op if sync is not
/// enabled. Network failures propagate; local data is never affected.
pub fn sync(db: &Database) -> anyhow::Result<()> {
    db.sync()
}

// ---------- public API ----------

const SELECT_COLS: &str = "id, title, description, status, parent_id, sheet_id, sort_order, \
     created_at, updated_at, completed_at, agent_status, agent_result, \
     agent_progress, metadata";

/// Fetch all (non-deleted) goals, ordered: roots first, then children grouped
/// by parent, each bucket sorted by `sort_order` then `created_at`.
pub fn get_all(db: &Database) -> anyhow::Result<Vec<Goal>> {
    let result = db.query_simple(
        &format!("SELECT {SELECT_COLS} FROM goals WHERE deleted_at IS NULL ORDER BY parent_id IS NOT NULL, parent_id, sort_order, created_at"),
    )?;
    Ok(result.rows().iter().map(values_to_goal).collect())
}

/// Fetch a single (non-deleted) goal by ID. Returns `None` if not found.
pub fn get(db: &Database, id: &str) -> anyhow::Result<Option<Goal>> {
    let result = db.query_first(
        &format!("SELECT {SELECT_COLS} FROM goals WHERE id = ? AND deleted_at IS NULL"),
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
        "INSERT INTO goals (id, title, description, status, parent_id, sheet_id, sort_order, created_at, updated_at) \
         VALUES (?, ?, ?, 'pending', ?, ?, ?, ?, ?)",
        vec![
            Value::from(id.clone()),
            Value::from(input.title),
            input.description.map(Value::from).unwrap_or(Value::Null),
            input.parent_id.map(Value::from).unwrap_or(Value::Null),
            input.sheet_id.map(Value::from).unwrap_or(Value::Null),
            Value::from(input.sort_order.unwrap_or(0)),
            Value::from(ts.clone()),
            Value::from(ts),
        ],
    )?;
    db.mark_dirty("goals", &id)?;
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
        params.push(Value::Text(status.as_str().to_string()));
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
                .map(|s| Value::Text(s.as_str().to_string()))
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
    if let Some(parent_id) = input.parent_id {
        fields.push("parent_id = ?");
        params.push(parent_id.map(Value::from).unwrap_or(Value::Null));
    }
    if let Some(sheet_id) = input.sheet_id {
        fields.push("sheet_id = ?");
        params.push(sheet_id.map(Value::from).unwrap_or(Value::Null));
    }

    // Always bump updated_at
    fields.push("updated_at = ?");
    params.push(Value::from(cordanui_schema::now_iso()));

    params.push(Value::from(id));

    let sql = format!("UPDATE goals SET {} WHERE id = ?", fields.join(", "));
    db.execute(&sql, params)?;
    db.mark_dirty("goals", id)?;
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

/// Soft-delete a goal and all its descendants.
///
/// Sets `deleted_at = now` on the row and every nested subgoal (tombstones),
/// bumps `updated_at` so the change wins LWW and propagates via sync, and
/// marks every affected row dirty in the outbox. Reads (`get`, `get_all`, …)
/// filter `deleted_at IS NULL`, so tombstoned rows vanish from the UI.
pub fn delete(db: &Database, id: &str) -> anyhow::Result<()> {
    let ts = cordanui_schema::now_iso();
    // Recursively collect this goal and all descendant IDs. We tombstone
    // instead of hard-delete so the deletion propagates to other clients
    // via sync; `ON DELETE CASCADE` therefore never fires.
    let ids: Vec<String> = collect_subtree(db, id)?;
    if ids.is_empty() {
        return Ok(());
    }
    let placeholders = vec!["?"; ids.len()].join(", ");
    let params: Vec<Value> = ids.iter().map(|i| Value::from(i.clone())).collect();
    db.execute(
        &format!(
            "UPDATE goals SET deleted_at = ?, updated_at = ? WHERE id IN ({placeholders})"
        ),
        {
            let mut p = vec![Value::from(ts.clone()), Value::from(ts)];
            p.extend(params);
            p
        },
    )?;
    for id in &ids {
        db.mark_dirty("goals", id)?;
    }
    Ok(())
}

/// Collect a goal's ID and every descendant ID (any depth). Deleted rows
/// are excluded so a re-delete is a no-op.
fn collect_subtree(db: &Database, root: &str) -> anyhow::Result<Vec<String>> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_string()];
    while let Some(id) = stack.pop() {
        out.push(id.clone());
        let children = db.query(
            "SELECT id FROM goals WHERE parent_id = ? AND deleted_at IS NULL",
            vec![Value::from(id)],
        )?;
        for row in children.rows() {
            if let Some(Value::Text(c)) = row.first() {
                stack.push(c.clone());
            }
        }
    }
    Ok(out)
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
        assert!(
            !root.id.contains('.'),
            "root id should have no dots: {}",
            root.id
        );

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
    let result = db.query_simple(&format!(
        "SELECT {SELECT_COLS} FROM goals WHERE parent_id IS NULL AND deleted_at IS NULL ORDER BY sort_order, created_at"
    ))?;
    Ok(result.rows().iter().map(values_to_goal).collect())
}

/// Fetch immediate (non-deleted) children of a goal.
pub fn get_children(db: &Database, parent_id: &str) -> anyhow::Result<Vec<Goal>> {
    let result = db.query(
        &format!(
            "SELECT {SELECT_COLS} FROM goals WHERE parent_id = ? AND deleted_at IS NULL ORDER BY sort_order, created_at"
        ),
        vec![Value::from(parent_id)],
    )?;
    Ok(result.rows().iter().map(values_to_goal).collect())
}

/// Get the next available sort_order for a new goal under `parent_id`
/// (or at the root level if `parent_id` is None). Only counts non-deleted
/// siblings.
pub fn next_sort_order(db: &Database, parent_id: Option<&str>) -> anyhow::Result<i64> {
    match parent_id {
        Some(pid) => db.query_scalar_i64(
            "SELECT COALESCE(MAX(sort_order), -1) + 1 FROM goals WHERE parent_id = ? AND deleted_at IS NULL",
            vec![Value::from(pid)],
        ),
        None => db.query_scalar_i64(
            "SELECT COALESCE(MAX(sort_order), -1) + 1 FROM goals WHERE parent_id IS NULL AND deleted_at IS NULL",
            vec![],
        ),
    }
}

pub fn next_sort_order_in_sheet(
    db: &Database,
    parent_id: Option<&str>,
    sheet_id: Option<&str>,
) -> anyhow::Result<i64> {
    match (parent_id, sheet_id) {
        (Some(pid), Some(sid)) => db.query_scalar_i64(
            "SELECT COALESCE(MAX(sort_order), -1) + 1 FROM goals WHERE parent_id = ? AND sheet_id = ? AND deleted_at IS NULL",
            vec![Value::from(pid), Value::from(sid)],
        ),
        (Some(pid), None) => db.query_scalar_i64(
            "SELECT COALESCE(MAX(sort_order), -1) + 1 FROM goals WHERE parent_id = ? AND sheet_id IS NULL AND deleted_at IS NULL",
            vec![Value::from(pid)],
        ),
        (None, Some(sid)) => db.query_scalar_i64(
            "SELECT COALESCE(MAX(sort_order), -1) + 1 FROM goals WHERE parent_id IS NULL AND sheet_id = ? AND deleted_at IS NULL",
            vec![Value::from(sid)],
        ),
        (None, None) => db.query_scalar_i64(
            "SELECT COALESCE(MAX(sort_order), -1) + 1 FROM goals WHERE parent_id IS NULL AND sheet_id IS NULL AND deleted_at IS NULL",
            vec![],
        ),
    }
}

// ---------- sheets (buffers) ----------

pub fn list_sheets(db: &Database) -> anyhow::Result<Vec<cordanui_schema::GoalSheet>> {
    let result = db.query_simple(
        "SELECT id, name, created_at, deleted_at FROM goal_sheets WHERE deleted_at IS NULL ORDER BY created_at",
    )?;
    Ok(result
        .rows()
        .iter()
        .map(|row| cordanui_schema::GoalSheet {
            id: match row.get(0) {
                Some(Value::Text(s)) => s.clone(),
                _ => String::new(),
            },
            name: match row.get(1) {
                Some(Value::Text(s)) => s.clone(),
                _ => String::new(),
            },
            created_at: match row.get(2) {
                Some(Value::Text(s)) => s.clone(),
                _ => String::new(),
            },
            deleted_at: match row.get(3) {
                Some(Value::Text(s)) => Some(s.clone()),
                _ => None,
            },
        })
        .collect())
}

pub fn create_sheet(db: &Database, name: &str) -> anyhow::Result<cordanui_schema::GoalSheet> {
    let id = cordanui_schema::new_id();
    let ts = cordanui_schema::now_iso();
    db.execute(
        "INSERT INTO goal_sheets (id, name, created_at) VALUES (?, ?, ?)",
        vec![Value::from(id.clone()), Value::from(name), Value::from(ts.clone())],
    )?;
    db.mark_dirty("goal_sheets", &id)?;
    Ok(cordanui_schema::GoalSheet {
        id,
        name: name.to_string(),
        created_at: ts,
        deleted_at: None,
    })
}

pub fn delete_sheet(db: &Database, id: &str) -> anyhow::Result<()> {
    let ts = cordanui_schema::now_iso();
    db.execute(
        "UPDATE goal_sheets SET deleted_at = ? WHERE id = ? AND deleted_at IS NULL",
        vec![Value::from(ts.clone()), Value::from(id)],
    )?;
    db.mark_dirty("goal_sheets", id)?;
    Ok(())
}

pub fn rename_sheet(db: &Database, id: &str, name: &str) -> anyhow::Result<()> {
    db.execute(
        "UPDATE goal_sheets SET name = ? WHERE id = ? AND deleted_at IS NULL",
        vec![Value::from(name), Value::from(id)],
    )?;
    db.mark_dirty("goal_sheets", id)?;
    Ok(())
}

// ---------- plugins registry ----------

/// A row of the `plugins` table.
#[derive(Debug, Clone)]
pub struct PluginRow {
    pub id: String,
    pub source: String,
    pub dir: String,
    pub active: bool,
    pub installed_at: String,
}

const PLUGIN_COLS: &str = "id, source, dir, active, installed_at";

/// All installed plugins, most recently installed first.
pub fn list_plugins(db: &Database) -> anyhow::Result<Vec<PluginRow>> {
    let result = db.query_simple(&format!(
        "SELECT {PLUGIN_COLS} FROM plugins ORDER BY installed_at DESC, id"
    ))?;
    Ok(result.rows().iter().map(values_to_plugin).collect())
}

/// Record a freshly installed plugin. Active by default.
pub fn add_plugin(db: &Database, id: &str, source: &str, dir: &str) -> anyhow::Result<()> {
    let ts = cordanui_schema::now_iso();
    db.execute(
        "INSERT INTO plugins (id, source, dir, active, installed_at) \
         VALUES (?, ?, ?, 1, ?) \
         ON CONFLICT(id) DO UPDATE SET \
             source = excluded.source, dir = excluded.dir, \
             active = 1, installed_at = excluded.installed_at",
        vec![
            Value::from(id),
            Value::from(source),
            Value::from(dir),
            Value::from(ts),
        ],
    )?;
    Ok(())
}

/// Toggle / set a plugin's active flag.
pub fn set_plugin_active(db: &Database, id: &str, active: bool) -> anyhow::Result<()> {
    db.execute(
        "UPDATE plugins SET active = ? WHERE id = ?",
        vec![Value::from(active as i64), Value::from(id)],
    )?;
    Ok(())
}

/// Remove a plugin's registry row (does not touch its files).
pub fn remove_plugin_row(db: &Database, id: &str) -> anyhow::Result<()> {
    db.execute("DELETE FROM plugins WHERE id = ?", vec![Value::from(id)])?;
    Ok(())
}

/// Upsert a theme row on behalf of an installed plugin.
pub fn upsert_theme(
    db: &Database,
    id: &str,
    name: &str,
    source: &str,
    colors_json: &str,
) -> anyhow::Result<()> {
    db.execute(
        "INSERT INTO themes (id, name, source, colors_json) VALUES (?, ?, ?, ?) \
         ON CONFLICT(id) DO UPDATE SET \
             name = excluded.name, source = excluded.source, colors_json = excluded.colors_json",
        vec![
            Value::from(id),
            Value::from(name),
            Value::from(source),
            Value::from(colors_json),
        ],
    )?;
    db.mark_dirty("themes", id)?;
    Ok(())
}

/// Make `theme_id` the active theme (explicit mode).
pub fn set_active_theme(db: &Database, theme_id: &str) -> anyhow::Result<()> {
    db.execute(
        "INSERT INTO settings (key, value) VALUES ('theme_mode', 'explicit') \
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        vec![],
    )?;
    db.execute(
        "INSERT INTO settings (key, value) VALUES ('selected_theme_id', ?) \
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        vec![Value::from(theme_id)],
    )?;
    Ok(())
}

/// Read a single setting value by key, or None if not present.
pub fn get_setting(db: &Database, key: &str) -> Option<String> {
    db.query_first(
        "SELECT value FROM settings WHERE key = ?",
        vec![Value::from(key)],
    )
    .ok()
    .flatten()
    .and_then(|row| match row.first() {
        Some(Value::Text(v)) => Some(v.clone()),
        _ => None,
    })
}

/// Merge a JSON patch into a goal's `metadata` (read-modify-write), same
/// contract as the agent backend's `merge_metadata` — plugins write
/// `mobile.json` / `__metadata__.json` files to declaratively update mobile FE.
pub fn merge_metadata(db: &Database, id: &str, patch: serde_json::Value) -> anyhow::Result<()> {
    let goal = get(db, id)?.ok_or_else(|| anyhow::anyhow!("goal not found: {id}"))?;
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
    update(
        db,
        id,
        UpdateGoalInput {
            metadata: Some(Some(merged)),
            ..Default::default()
        },
    )?;
    Ok(())
}

/// Write a setting value (upsert). Used for synced keys like `agent.url`.
pub fn set_setting(db: &Database, key: &str, value: &str) -> anyhow::Result<()> {
    db.execute(
        "INSERT INTO settings (key, value) VALUES (?, ?) \
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        vec![Value::from(key), Value::from(value)],
    )?;
    // Note: we intentionally do NOT mark_dirty here — settings sync uses
    // the full-replace strategy (push entire settings table). The push
    // path in sync.rs already includes all non-device-local keys.
    Ok(())
}

/// Revert to system mode (builtin dark in the TUI).
pub fn clear_theme_selection(db: &Database) -> anyhow::Result<()> {
    db.execute(
        "INSERT INTO settings (key, value) VALUES ('theme_mode', 'system') \
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        vec![],
    )?;
    Ok(())
}

// ---------- plugin settings (declarative [ui] forms) ----------

/// All values stored for one plugin, with the namespace prefix stripped.
/// Returns a map of bare field key → stored value.
///
/// Merge order: the local config.toml mirror (`[plugins.<name>]`) provides
/// the base, DB rows (synced via Turso) win on conflict. A fresh device
/// therefore restores its mirrored config even before the first sync
/// lands.
pub fn get_plugin_settings(
    db: &Database,
    plugin: &str,
) -> anyhow::Result<std::collections::BTreeMap<String, String>> {
    let mut map = cordanui_sync::read_plugin_settings(plugin);
    let result = db.query_simple(&format!(
        "SELECT key, value FROM settings WHERE key LIKE '{}.%'",
        escape_like(plugin)
    ))?;
    let prefix = format!("{plugin}.");
    for row in result.rows() {
        if let (Some(Value::Text(k)), Some(Value::Text(v))) = (row.first(), row.get(1)) {
            if let Some(bare) = k.strip_prefix(&prefix) {
                map.insert(bare.to_string(), v.clone());
            }
        }
    }
    Ok(map)
}

/// Store one setting under the plugin's namespace.
///
/// Writes to the shared `settings` table (the runtime source of truth,
/// synced via Turso) AND mirrors into `[plugins.<name>]` in the local
/// config.toml, so configuration survives without the remote. The mirror
/// is best-effort: a config-file failure never fails the DB write.
pub fn set_plugin_setting(
    db: &Database,
    plugin: &str,
    key: &str,
    value: &str,
) -> anyhow::Result<()> {
    db.execute(
        "INSERT INTO settings (key, value) VALUES (?, ?) \
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        vec![Value::from(format!("{plugin}.{key}")), Value::from(value)],
    )?;
    if let Err(e) = cordanui_sync::write_plugin_setting(plugin, key, value) {
        eprintln!("cordanui: could not mirror plugin setting to config.toml: {e:#}");
    }
    Ok(())
}

/// The full namespaced key for a plugin field (for diagnostics).
pub fn plugin_setting_key(plugin: &str, key: &str) -> String {
    format!("{plugin}.{key}")
}

fn escape_like(s: &str) -> String {
    s.replace('\'', "''")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

// ---------- style overrides (cord.g.style.*) ----------

/// All global style overrides, keyed by bare variable name (the
/// `style.` prefix is stripped). These sync to every client via Turso.
pub fn get_style_overrides(
    db: &Database,
) -> anyhow::Result<std::collections::BTreeMap<String, String>> {
    let result = db.query_simple("SELECT key, value FROM settings WHERE key LIKE 'style.%'")?;
    let mut map = std::collections::BTreeMap::new();
    for row in result.rows() {
        if let (Some(Value::Text(k)), Some(Value::Text(v))) = (row.first(), row.get(1)) {
            if let Some(bare) = k.strip_prefix("style.") {
                map.insert(bare.to_string(), v.clone());
            }
        }
    }
    Ok(map)
}

/// Persist one global style override (`settings` key `style.<var>`).
pub fn set_style_override(db: &Database, var: &str, hex: &str) -> anyhow::Result<()> {
    set_plugin_setting(db, "style", var, hex)
}

/// Remove one global style override.
pub fn clear_style_override(db: &Database, var: &str) -> anyhow::Result<()> {
    db.execute(
        "DELETE FROM settings WHERE key = ?",
        vec![Value::from(format!("style.{var}"))],
    )?;
    Ok(())
}

/// Remove all global style overrides.
pub fn clear_all_style_overrides(db: &Database) -> anyhow::Result<()> {
    db.execute("DELETE FROM settings WHERE key LIKE 'style.%'", vec![])?;
    Ok(())
}

/// Serialize a plugin's settings into the `config` JSON object handed to
/// subprocess invocations. Empty map → None (field omitted on the wire).
pub fn settings_to_config(
    values: &std::collections::BTreeMap<String, String>,
) -> Option<serde_json::Value> {
    if values.is_empty() {
        return None;
    }
    let mut obj = serde_json::Map::new();
    for (k, v) in values {
        obj.insert(k.clone(), serde_json::Value::String(v.clone()));
    }
    Some(serde_json::Value::Object(obj))
}

fn values_to_plugin(row: &Vec<Value>) -> PluginRow {
    let s = |i: usize| -> String {
        match row.get(i) {
            Some(Value::Text(v)) => v.clone(),
            _ => String::new(),
        }
    };
    let active = matches!(row.get(3), Some(Value::Integer(n)) if *n != 0);
    PluginRow {
        id: s(0),
        source: s(1),
        dir: s(2),
        active,
        installed_at: s(4),
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
    let agent_status_str = get_opt_str(10);

    Goal {
        id: get_str(0),
        title: get_str(1),
        description: get_opt_str(2),
        status: GoalStatus::from_db(&status_str),
        parent_id: get_opt_str(4),
        sheet_id: get_opt_str(5),
        sort_order: get_i64(6),
        created_at: get_str(7),
        updated_at: get_str(8),
        completed_at: get_opt_str(9),
        agent_status: agent_status_str.map(|s| AgentStatus::from_db(&s)),
        agent_result: get_opt_str(11),
        agent_progress: get_opt_str(12),
        metadata: get_opt_str(13),
    }
}

// ---------- errors view (diagnostics log) ----------

/// One row of the `errors` table.
#[derive(Debug, Clone)]
pub struct ErrorRow {
    pub context: String,
    pub message: String,
    pub detail: Option<String>,
    pub created_at: String,
}

fn error_text(v: &Value) -> String {
    match v {
        Value::Text(s) => s.clone(),
        _ => String::new(),
    }
}

/// Log a failure into the `errors` table. Never fails the caller: error
/// logging must not be able to cause errors. Failures to log are printed
/// to stderr (visible when not in raw mode).
pub fn log_error(db: &Database, context: &str, message: &str, detail: Option<&str>) {
    let result = db.execute(
        "INSERT INTO errors (id, context, message, detail, created_at) \
         VALUES (?, ?, ?, ?, ?)",
        vec![
            Value::from(cordanui_schema::new_id()),
            Value::from(context),
            Value::from(message),
            detail.map(Value::from).unwrap_or(Value::Null),
            Value::from(cordanui_schema::now_iso()),
        ],
    );
    if let Err(e) = result {
        eprintln!("cordanui: could not record error ({context}): {e:#}");
    }
}

/// Recent errors, newest first.
pub fn get_errors(db: &Database, limit: i64) -> anyhow::Result<Vec<ErrorRow>> {
    let result = db.query(
        "SELECT context, message, detail, created_at FROM errors \
         ORDER BY created_at DESC LIMIT ?",
        vec![Value::from(limit)],
    )?;
    Ok(result
        .rows()
        .iter()
        .map(|row| ErrorRow {
            context: row.first().map(error_text).unwrap_or_default(),
            message: row.get(1).map(error_text).unwrap_or_default(),
            detail: row.get(2).map(error_text),
            created_at: row.get(3).map(error_text).unwrap_or_default(),
        })
        .collect())
}

/// Delete every logged error.
pub fn clear_errors(db: &Database) -> anyhow::Result<()> {
    db.execute_simple("DELETE FROM errors")
}

// ---------- purge (danger zone) ----------

/// Delete ALL data rows: goals, goal_sheets, themes, settings, plugins
/// registry, error log. Bookkeeping (`_migrations`) is deliberately
/// preserved — purging it would make the next open re-run shipped
/// migrations against an already final schema and crash. The schema itself
/// stays in place. Sync bookkeeping (`_outbox` / `_sync_state`) is reset
/// so the next sync pulls the full remote state instead of appearing to
/// do nothing.
pub fn purge_all(db: &Database) -> anyhow::Result<()> {
    for table in [
        "goals",
        "goal_sheets",
        "themes",
        "settings",
        "plugins",
        "errors",
    ] {
        db.execute_simple(&format!("DELETE FROM {table}"))?;
    }
    db.execute_simple("DELETE FROM _outbox")?;
    db.execute_simple("DELETE FROM _sync_state")?;
    Ok(())
}
