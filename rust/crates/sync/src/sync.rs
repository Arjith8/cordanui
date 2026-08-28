//! The custom row-level sync layer: Hrana-over-HTTP against Turso Cloud's
//! `/v2/pipeline`, identical in semantics to the mobile client.
//!
//! Per-table strategy:
//! - `goals`      : incremental LWW both ways (updated_at cursors + guards)
//! - `goal_sheets`: full bidirectional replace (small; tombstones included)
//! - `themes`     : full bidirectional replace (small; tombstones included)
//! - `settings`   : full bidirectional replace of non-device-local keys
//!                  (TUI is the primary writer; last sync wins per key)
//!
//! Device-local (never synced): `plugins`, `errors`, `_outbox`,
//! `_sync_state`, `_migrations`, and anything else prefixed `_`.

use std::collections::HashSet;

use anyhow::{Context, Result};
use serde_json::json;

use crate::{Database, Value};

fn rv(v: Value) -> rusqlite::types::Value {
    v.into()
}
fn rvs(vs: Vec<Value>) -> Vec<rusqlite::types::Value> {
    vs.into_iter().map(|v| v.into()).collect()
}

/// Normalized remote configuration.
#[derive(Debug, Clone)]
pub struct RemoteConfig {
    pub base_url: String,
    pub token: String,
}

/// `libsql://` and `turso://` are address schemes meaning HTTPS.
pub fn normalize_base_url(url: &str) -> String {
    url.trim()
        .replacen("libsql://", "https://", 1)
        .replacen("turso://", "https://", 1)
        .trim_end_matches('/')
        .to_string()
}

// ---------- table configuration ----------

struct TableSync {
    name: &'static str,
    /// Columns synced, in wire order. Must match the canonical schema.
    cols: &'static [&'static str],
    /// LWW column for conditional upserts (None = blind replace).
    lww: Option<&'static str>,
}

const GOALS: TableSync = TableSync {
    name: "goals",
    cols: &[
        "id",
        "title",
        "description",
        "status",
        "parent_id",
        "sheet_id",
        "sort_order",
        "created_at",
        "updated_at",
        "completed_at",
        "agent_status",
        "agent_result",
        "agent_progress",
        "metadata",
        "deleted_at",
    ],
    lww: Some("updated_at"),
};

const FULL_REPLACE_TABLES: [TableSync; 2] = [
    TableSync {
        name: "goal_sheets",
        cols: &["id", "name", "created_at", "deleted_at"],
        lww: None,
    },
    TableSync {
        name: "themes",
        cols: &["id", "name", "source", "colors_json", "last_used_at", "deleted_at"],
        lww: None,
    },
];

/// Settings keys that never leave the device.
fn is_device_local_setting(key: &str) -> bool {
    key == "turso_url"
        || key == "turso_token"
        || key.starts_with("sync.")
        || key.starts_with('_')
}

// ---------- Hrana-over-HTTP client ----------

struct HranaResult {
    rows: Vec<Vec<Value>>,
}

fn value_to_arg(v: &Value) -> serde_json::Value {
    match v {
        Value::Null => json!({"type": "null"}),
        Value::Integer(i) => json!({"type": "integer", "value": i.to_string()}),
        Value::Real(f) => json!({"type": "float", "value": f.to_string()}),
        Value::Text(s) => json!({"type": "text", "value": s}),
        Value::Blob(b) => json!({"type": "blob", "value": b}),
    }
}

fn json_to_value(v: &serde_json::Value) -> Value {
    use serde_json::Value as J;
    let Some(obj) = v.as_object() else {
        return Value::Null;
    };
    let t = obj.get("type").and_then(|t| t.as_str()).unwrap_or("null");
    let val = obj.get("value");
    match t {
        "integer" => Value::Integer(
            val.and_then(|v| v.as_str().and_then(|s| s.parse().ok()).or_else(|| v.as_i64()))
                .unwrap_or(0),
        ),
        "float" => Value::Real(
            val.and_then(|v| v.as_str().and_then(|s| s.parse().ok()).or_else(|| v.as_f64()))
                .unwrap_or(0.0),
        ),
        "text" => Value::Text(
            val.and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .unwrap_or_default(),
        ),
        "blob" => Value::Blob(
            val.and_then(|v| v.as_str())
                .map(|s| s.as_bytes().to_vec())
                .unwrap_or_default(),
        ),
        _ => Value::Null,
    }
}

/// Execute a batch of statements; returns one result per statement.
/// Handles both `response.result` (singular, current) and
/// `response.results` (array, older) Hrana shapes.
fn pipeline(
    http: &reqwest::blocking::Client,
    remote: &RemoteConfig,
    stmts: &[(String, Vec<Value>)],
) -> Result<Vec<HranaResult>> {
    let mut requests = Vec::with_capacity(stmts.len() + 1);
    for (sql, args) in stmts {
        requests.push(json!({
            "type": "execute",
            "stmt": {
                "sql": sql,
                "args": args.iter().map(value_to_arg).collect::<Vec<_>>(),
            }
        }));
    }
    requests.push(json!({"type": "close"}));

    let url = format!("{}/v2/pipeline", remote.base_url);
    let response = http
        .post(&url)
        .bearer_auth(&remote.token)
        .json(&json!({ "requests": requests }))
        .send()
        .context("turso: request failed")?;
    let status = response.status();
    if !status.is_success() {
        let body = response.text().unwrap_or_default();
        anyhow::bail!("turso: http {status}: {}", truncate(&body, 200));
    }

    let json: serde_json::Value = response.json().context("turso: bad JSON response")?;
    let results = json
        .get("results")
        .and_then(|r| r.as_array())
        .ok_or_else(|| anyhow::anyhow!("turso: response has no results array"))?;

    let mut out = Vec::with_capacity(stmts.len());
    for i in 0..stmts.len() {
        let entry = results.get(i).ok_or_else(|| {
            anyhow::anyhow!(
                "turso: no response for stmt #{i} ({})",
                truncate(&stmts[i].0, 60)
            )
        })?;
        if entry.get("type").and_then(|t| t.as_str()) != Some("ok") {
            anyhow::bail!(
                "turso: stmt #{i} failed ({}): {}",
                truncate(&stmts[i].0, 60),
                truncate(&entry.to_string(), 200)
            );
        }
        let response = entry.get("response").ok_or_else(|| {
            anyhow::anyhow!("turso: stmt #{i} response missing (close mismatch?)")
        })?;
        // Hrana v2: response.result (singular). Older: response.results.
        let list: Vec<&serde_json::Value> = if let Some(r) = response.get("results").and_then(|r| r.as_array()) {
            r.iter().collect()
        } else if let Some(r) = response.get("result") {
            vec![r]
        } else {
            anyhow::bail!(
                "turso: stmt #{i} response has no result: {}",
                truncate(&response.to_string(), 120)
            );
        };
        for r in list {
            let raw_rows = r
                .get("rows")
                .and_then(|x| x.as_array())
                .cloned()
                .unwrap_or_default();
            let rows: Vec<Vec<Value>> = raw_rows
                .iter()
                .map(|row| {
                    row.as_array()
                        .map(|cells| cells.iter().map(json_to_value).collect())
                        .unwrap_or_default()
                })
                .collect();
            out.push(HranaResult { rows });
        }
    }
    Ok(out)
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let cut: String = s.chars().take(max.saturating_sub(1)).collect();
    format!("{cut}…")
}

// ---------- push ----------

/// Upload every row pending in `_outbox`, then clear it. Server-side LWW
/// for goals (conditional upsert); full replace for the small tables.
pub(crate) fn push(db: &Database, http: &reqwest::blocking::Client, remote: &RemoteConfig) -> Result<()> {
    let conn = db.conn();
    let mut pending: std::collections::HashMap<&'static str, Vec<String>> =
        std::collections::HashMap::new();
    {
        let mut stmt = conn.prepare("SELECT table_name, row_id FROM _outbox ORDER BY rowid")?;
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            let table: String = row.get(0)?;
            let id: String = row.get(1)?;
            match table.as_str() {
                "goals" => pending.entry("goals").or_default().push(id),
                "goal_sheets" => pending.entry("goal_sheets").or_default().push(id),
                "themes" => pending.entry("themes").or_default().push(id),
                // Unknown table (stale entry) — drop it.
                _ => {}
            }
        }
    }

    let mut all_stmts: Vec<(String, Vec<Value>)> = Vec::new();
    let mut drained: Vec<&'static str> = Vec::new();

    for table in [&GOALS, &FULL_REPLACE_TABLES[0], &FULL_REPLACE_TABLES[1]] {
        let Some(ids) = pending.get(table.name) else {
            continue;
        };
        if ids.is_empty() {
            continue;
        }
        let placeholders = vec!["?"; ids.len()].join(", ");
        let params: Vec<Value> = ids.iter().map(|id| Value::Text(id.clone())).collect();
        let sql = format!(
            "SELECT {} FROM {} WHERE id IN ({placeholders})",
            table.cols.join(", "),
            table.name
        );
        let result = db.query(&sql, params)?;
        let upsert = upsert_sql(table);
        for row in result.rows() {
            all_stmts.push((upsert.clone(), row.clone()));
        }
        drained.push(table.name);
    }

    // Settings snapshot: TUI is the primary writer; every sync replaces
    // the remote's non-device-local keys with ours.
    {
        let result = db.query("SELECT key, value FROM settings", vec![])?;
        for row in result.rows() {
            let key = match row.first() {
                Some(Value::Text(k)) => k.clone(),
                _ => continue,
            };
            if is_device_local_setting(&key) {
                continue;
            }
            let value = row.get(1).cloned().unwrap_or(Value::Null);
            all_stmts.push((
                "INSERT INTO settings (key, value) VALUES (?, ?) \
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value"
                    .to_string(),
                vec![Value::Text(key), value],
            ));
        }
    }

    if !all_stmts.is_empty() {
        pipeline(http, remote, &all_stmts)?;
    }
    for name in drained {
        conn.execute(
            "DELETE FROM _outbox WHERE table_name = ?",
            rusqlite::params![name],
        )?;
    }
    Ok(())
}

/// Conditional upsert: LWW for tables with an LWW column, blind replace
/// otherwise.
fn upsert_sql(table: &TableSync) -> String {
    let cols = table.cols.join(", ");
    let placeholders = vec!["?"; table.cols.len()].join(", ");
    let updates: Vec<String> = table
        .cols
        .iter()
        .filter(|c| **c != "id")
        .map(|c| format!("{c} = excluded.{c}"))
        .collect();
    let lww_guard = table
        .lww
        .map(|c| format!(" WHERE excluded.{c} > {}.{c}", table.name))
        .unwrap_or_default();
    let updates_str = updates.join(", ");
    format!(
        "INSERT INTO {name} ({cols}) VALUES ({placeholders}) \
         ON CONFLICT(id) DO UPDATE SET {updates_str}{lww_guard}",
        name = table.name
    )
}

// ---------- pull ----------

/// Pull remote changes and apply them with local LWW guards. Rows pending
/// in the outbox are skipped for full-replace tables (a local edit always
/// beats an incoming replace of the same row; the pending push carries it).
pub(crate) fn pull(db: &Database, http: &reqwest::blocking::Client, remote: &RemoteConfig) -> Result<()> {
    let conn = db.conn();

    // --- goals: incremental, LWW ---
    let last_pull = get_state(&conn, "last_pull").unwrap_or_default();
    let cols = GOALS.cols.join(", ");
    let sql = format!("SELECT {cols} FROM goals WHERE updated_at > ?");
    let results = pipeline(
        http,
        remote,
        &[(sql, vec![Value::Text(last_pull)])],
    )?;
    let mut newest = get_state(&conn, "last_pull").unwrap_or_default();
    let upsert = upsert_sql(&GOALS);
    for row in &results[0].rows {
        // LWW guard locally: only apply if the remote row is newer than
        // the local one (or local is missing).
        let id = row.first().cloned().unwrap_or(Value::Null);
        let remote_updated = row.get(8).cloned().unwrap_or(Value::Null);
        let local_updated: Option<Value> = {
            let mut stmt = conn.prepare("SELECT updated_at FROM goals WHERE id = ?")?;
            stmt.query_row(rusqlite::params_from_iter([rv(id.clone())]), |r| r.get::<_, rusqlite::types::Value>(0).map(|v| Value::from(v)))
                .ok()
        };
        if let (Some(Value::Text(local_t)), Value::Text(remote_t)) =
            (&local_updated, &remote_updated)
        {
            if local_t >= remote_t {
                continue; // local is same or newer — keep it
            }
        }
        conn.execute(&upsert, rusqlite::params_from_iter(rvs(row.clone())))?;
        // The remote state supersedes any pending local push of this row.
        conn.execute(
            "DELETE FROM _outbox WHERE table_name = 'goals' AND row_id = ?",
            rusqlite::params_from_iter([rv(id.clone())]),
        )?;
        if let Value::Text(t) = &remote_updated {
            if t.as_str() > newest.as_str() {
                newest = t.clone();
            }
        }
    }
    set_state(&conn, "last_pull", &newest);

    // --- full-replace tables: goal_sheets, themes ---
    for table in &FULL_REPLACE_TABLES {
        let cols = table.cols.join(", ");
        let sql = format!("SELECT {cols} FROM {}", table.name);
        let results = match pipeline(http, remote, &[(sql, Vec::new())]) {
            Ok(r) => r,
            Err(e) => {
                let msg = e.to_string();
                if msg.contains("no such table") || msg.contains("no such column") {
                    tracing::warn!(table = table.name, error = %e, "skipping pull for missing table/column");
                    continue;
                }
                return Err(e);
            }
        };
        let upsert = upsert_sql(table);
        // Rows pending local push win for this round.
        let pending: HashSet<String> = {
            let mut stmt = conn.prepare("SELECT row_id FROM _outbox WHERE table_name = ?")?;
            let rows = stmt.query_map(rusqlite::params![table.name], |r| r.get::<_, String>(0))?;
            rows.flatten().collect()
        };
        for row in &results[0].rows {
            let id = match row.first() {
                Some(Value::Text(id)) => id.clone(),
                _ => continue,
            };
            if pending.contains(&id) {
                continue;
            }
            conn.execute(&upsert, rusqlite::params_from_iter(rvs(row.clone())))?;
        }
    }

    // --- settings: remote wins for non-device-local keys ---
    let results = pipeline(
        http,
        remote,
        &[("SELECT key, value FROM settings".to_string(), Vec::new())],
    )?;
    for row in &results[0].rows {
        let key = match row.first() {
            Some(Value::Text(k)) => k.clone(),
            _ => continue,
        };
        if is_device_local_setting(&key) {
            continue;
        }
        let value = row.get(1).cloned().unwrap_or(Value::Null);
        conn.execute(
            "INSERT INTO settings (key, value) VALUES (?, ?) \
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            rusqlite::params![key, value],
        )?;
    }

    Ok(())
}

fn get_state(conn: &rusqlite::Connection, key: &str) -> Option<String> {
    conn.query_row(
        "SELECT value FROM _sync_state WHERE key = ?",
        rusqlite::params![key],
        |r| r.get(0),
    )
    .ok()
}

fn set_state(conn: &rusqlite::Connection, key: &str, value: &str) {
    let _ = conn.execute(
        "INSERT INTO _sync_state (key, value) VALUES (?, ?) \
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        rusqlite::params![key, value],
    );
}
