//! cordanui-sync — local-first SQLite storage with a custom row-level sync
//! layer (the "one protocol, two clients" design).
//!
//! Storage: rusqlite (bundled SQLite), fully synchronous. Opening the
//! database NEVER touches the network.
//!
//! Sync (see [`sync`]): the TUI speaks the exact same protocol as mobile —
//! Hrana-over-HTTP against Turso Cloud's `/v2/pipeline` — with row-level
//! last-write-wins semantics:
//!
//! - writes land locally and mark rows in the device-local `_outbox`
//! - push: outbox rows → remote, with server-side LWW
//!   (`WHERE excluded.updated_at > table.updated_at`)
//! - pull: incremental by `updated_at` cursor + local LWW guard
//! - deletes are soft (`deleted_at` tombstones) so they propagate
//! - `_outbox` / `_sync_state` are device-local (leading underscore)
//!
//! Synced tables: goals (incremental LWW), goal_sheets + themes (full
//! bidirectional replace incl. tombstones), settings (TUI pushes, mobile
//! pulls; snapshot semantics). Device-local: plugins, errors, `_*`.

use std::path::{Path, PathBuf};
use std::sync::{Arc};

use anyhow::{Context, Result};

pub use crate::types::Value;

pub mod sync;
pub mod types;

// ---------- public types ----------

/// A synchronous database handle. Cloning opens a new connection to the
/// same file, so the sync worker never blocks UI queries.
pub struct Database {
    shared: Arc<Shared>,
    conn: rusqlite::Connection,
    sync_enabled: bool,
}

impl Clone for Database {
    fn clone(&self) -> Self {
        Self {
            shared: Arc::clone(&self.shared),
            conn: self
                .shared
                .open_conn()
                .expect("clone: failed to open database connection"),
            sync_enabled: self.sync_enabled,
        }
    }
}

// SAFETY: Database owns a single rusqlite::Connection (which is !Send due to
// RefCell), but every clone opens an independent connection to the same file.
// The shared state (path, remote, http client) is Send/Sync. Access to the
// connection is not shared across threads without cloning — Axum's State
// clones the Arc<AgentRunner> which clones the Database handle's connection
// via the Clone impl above. As long as callers do not share a single handle
// concurrently without external synchronization (they always clone), this is
// safe. The agent backend's tasks are sequential per handle.
unsafe impl Send for Database {}
unsafe impl Sync for Database {}

struct Shared {
    path: PathBuf,
    remote: Option<sync::RemoteConfig>,
    http: reqwest::blocking::Client,
}

impl Shared {
    /// Open an independent SQLite connection to the database file. Each
    /// `Database` handle owns one, so the sync worker never blocks UI
    /// queries (WAL mode makes concurrent handles safe).
    fn open_conn(&self) -> Result<rusqlite::Connection> {
        let conn = rusqlite::Connection::open(&self.path)?;
        conn.execute_batch(
            "PRAGMA journal_mode = WAL; \
             PRAGMA foreign_keys = ON; \
             PRAGMA busy_timeout = 5000;",
        )?;
        Ok(conn)
    }
}

/// A query result — a vec of rows, each row a vec of column values.
pub struct QueryResult {
    rows: Vec<Vec<Value>>,
}

/// Configuration for opening a database.
#[derive(Debug, Clone, Default)]
pub struct SyncConfig {
    pub turso_url: Option<String>,
    pub turso_token: Option<String>,
    pub db_path: PathBuf,
}

impl SyncConfig {
    /// Load from the config file at `~/.config/cordanui/config.toml`.
    /// Returns a local-only config if the file doesn't exist.
    pub fn load() -> Result<Self> {
        let db_path = default_db_path();

        let config_path = config_file_path();
        if !config_path.exists() {
            return Ok(Self {
                db_path,
                ..Default::default()
            });
        }

        let contents = std::fs::read_to_string(&config_path)
            .with_context(|| format!("reading config at {}", config_path.display()))?;

        let parsed: toml::Value = toml::from_str(&contents).context("parsing config.toml")?;

        let turso = parsed.get("turso");
        let (turso_url, turso_token) = match turso {
            Some(t) => (
                t.get("url").and_then(|v| v.as_str()).map(String::from),
                t.get("token").and_then(|v| v.as_str()).map(String::from),
            ),
            None => (None, None),
        };

        Ok(Self {
            turso_url,
            turso_token,
            db_path,
        })
    }

    pub fn is_sync_enabled(&self) -> bool {
        self.turso_url.is_some() && self.turso_token.is_some()
    }
}

impl Database {
    /// Open a database with the given config. Never touches the network.
    pub fn open(config: &SyncConfig) -> Result<Self> {
        if let Some(dir) = config.db_path.parent() {
            std::fs::create_dir_all(dir)?;
        }

        let sync_enabled = config.is_sync_enabled();
        let remote = if sync_enabled {
            tracing::info!(
                url = %config.turso_url.as_ref().unwrap(),
                "local database bound to Turso remote (sync is explicit push/pull)"
            );
            let base_url = config
                .turso_url
                .as_ref()
                .unwrap()
                .trim()
                .replacen("libsql://", "https://", 1)
                .replacen("turso://", "https://", 1)
                .trim_end_matches('/')
                .to_string();
                                
            Some(sync::RemoteConfig {
                base_url: base_url,
                token: config.turso_token.as_ref().unwrap().clone(),
            })
        } else {
            tracing::info!(path = %config.db_path.display(), "opening local-only database");
            None
        };

        // reqwest::blocking::Client creates its own tokio runtime internally.
        // If Database::open is called from within an existing tokio runtime
        // (e.g. cordanui-agents #[tokio::main]), dropping that inner runtime
        // inside the outer async context panics at tokio::runtime::blocking::shutdown.
        // Build it on a fresh thread outside any runtime.
        let http = std::thread::spawn(|| {
            reqwest::blocking::Client::builder()
                .timeout(std::time::Duration::from_secs(60))
                .build()
        })
        .join()
        .unwrap()?;
        let shared = Arc::new(Shared {
            path: config.db_path.clone(),
            remote,
            http,
        });

        let conn = shared.open_conn()?;
        let db = Database {
            shared,
            conn,
            sync_enabled,
        };
        db.run_migrations()?;
        Ok(db)
    }

    /// Open a local-only database at the default path.
    pub fn open_local() -> Result<Self> {
        let config = SyncConfig {
            db_path: default_db_path(),
            ..Default::default()
        };
        Self::open(&config)
    }

    /// Schema bootstrap + shared migrations. Runs on every open; each
    /// migration executes at most once per database (`_migrations`).
    fn run_migrations(&self) -> Result<()> {
        // Does this database predate the migration system? (A DB with a
        // `goals` table was created before/without migrations; a fresh
        // empty file gets the latest schema directly and only needs its
        // migrations recorded.)
        let pre_existing: bool = self
            .conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'goals'",
                [],
                |r| r.get::<_, i64>(0),
            )
            .map(|n| n > 0)
            .unwrap_or(false);

        if let Err(e) = self.conn.execute_batch(cordanui_schema::SCHEMA_SQL) {
            if pre_existing {
                // `SCHEMA_SQL` is the *latest* schema. On a pre-existing DB it may
                // reference columns that haven't been migrated yet (e.g.
                // `idx_goals_due_at` needs `due_at` added in v7). That shows up as
                // `no such column: due_at` at offset 54 inside the CREATE INDEX.
                // Migrations will add the column + index, so just warn and continue.
                tracing::warn!(
                    error = %e,
                    "SCHEMA_SQL batch failed on pre-existing DB — continuing to migrations"
                );
            } else {
                return Err(e.into());
            }
        }

        for m in cordanui_schema::MIGRATIONS {
            let done: bool = self
                .conn
                .query_row(
                    "SELECT COUNT(*) FROM _migrations WHERE version = ?",
                    [m.version],
                    |r| r.get::<_, i64>(0),
                )
                .map(|n| n > 0)
                .unwrap_or(false);
            if done {
                continue;
            }

            if pre_existing {
                // A migration may fail against a schema that already
                // reflects it (e.g. after a purge lost bookkeeping) — the
                // shape is what matters, so record it either way.
                if let Err(e) = self.conn.execute_batch(m.sql) {
                    tracing::warn!(
                        version = m.version,
                        name = m.name,
                        error = %e,
                        "migration DDL failed — recording as applied (schema likely already final)"
                    );
                }
            }
            self.conn.execute(
                "INSERT INTO _migrations (version, name, applied_at) \
                 VALUES (?, ?, datetime('now'))",
                rusqlite::params![m.version, m.name],
            )?;
        }
        Ok(())
    }

    pub fn is_sync_enabled(&self) -> bool {
        self.sync_enabled
    }

    /// Read-only access to the underlying connection (crate-internal).
    pub(crate) fn conn(&self) -> &rusqlite::Connection {
        &self.conn
    }

    /// Push outbox rows to Turso, then pull remote changes. No-op when
    /// sync is not configured. Network failures propagate — local data is
    /// never affected.
    pub fn sync(&self) -> Result<()> {
        if !self.sync_enabled {
            return Ok(());
        }
        let remote = self.shared.remote.as_ref().expect("sync enabled without remote config").clone();
        let shared = Arc::clone(&self.shared);
        // `sync::push`/`pull` use `reqwest::blocking::Client` which creates its
        // own tokio runtime internally. If `Database::sync` is called from
        // within an existing tokio runtime (e.g. `cordanui-agents`
        // `#[tokio::main]`), dropping that inner runtime inside the outer async
        // context panics at `tokio::runtime::blocking::shutdown`. Run the
        // blocking HTTP on a fresh thread outside any runtime.
        let handle = std::thread::spawn(move || {
            let db = Database {
                shared: Arc::clone(&shared),
                conn: shared.open_conn().expect("sync thread: failed to open db conn"),
                sync_enabled: true,
            };
            let res: anyhow::Result<()> = (|| {
                sync::push(&db, &db.shared.http, &remote)?;
                sync::pull(&db, &db.shared.http, &remote)
            })();
            res
        });
        handle.join().unwrap()?;
        Ok(())
    }

    /// Mark a row as pending push. Called by the client's write layer
    /// after every local write to a synced table.
    pub fn mark_dirty(&self, table: &str, id: &str) -> Result<()> {
        self.conn.execute(
            "INSERT OR IGNORE INTO _outbox (table_name, row_id) VALUES (?, ?)",
            rusqlite::params![table, id],
        )?;
        Ok(())
    }

    // ---------- query API ----------

    /// Execute a statement that returns no rows (INSERT, UPDATE, DELETE).
    pub fn execute(&self, sql: &str, params: Vec<Value>) -> Result<()> {
        let rusqlite_params: Vec<rusqlite::types::Value> =
            params.into_iter().map(|v| v.into()).collect();
        self.conn
            .execute(sql, rusqlite::params_from_iter(rusqlite_params))?;
        Ok(())
    }

    /// Execute a statement with no parameters.
    pub fn execute_simple(&self, sql: &str) -> Result<()> {
        self.conn.execute_batch(sql)?;
        Ok(())
    }

    /// Execute a batch of statements (e.g. schema migration).
    pub fn execute_batch(&self, sql: &str) -> Result<()> {
        self.conn.execute_batch(sql)?;
        Ok(())
    }

    /// Execute a query and return all rows. Each row is a `Vec<Value>`.
    pub fn query(&self, sql: &str, params: Vec<Value>) -> Result<QueryResult> {
        let rusqlite_params: Vec<rusqlite::types::Value> =
            params.into_iter().map(|v| v.into()).collect();
        let mut stmt = self.conn.prepare(sql)?;
        let col_count = stmt.column_count();
        let mut rows = stmt.query(rusqlite::params_from_iter(rusqlite_params))?;
        let mut result_rows = Vec::new();
        while let Some(row) = rows.next()? {
            let mut values = Vec::with_capacity(col_count);
            for i in 0..col_count {
                let v: rusqlite::types::Value = row.get(i)?;
                values.push(Value::from(v));
            }
            result_rows.push(values);
        }
        Ok(QueryResult { rows: result_rows })
    }

    /// Execute a query with no parameters and return all rows.
    pub fn query_simple(&self, sql: &str) -> Result<QueryResult> {
        self.query(sql, Vec::new())
    }

    /// Execute a query and return the first row, or None.
    pub fn query_first(&self, sql: &str, params: Vec<Value>) -> Result<Option<Vec<Value>>> {
        let result = self.query(sql, params)?;
        Ok(result.rows.into_iter().next())
    }

    /// Execute a query that returns a single integer scalar.
    pub fn query_scalar_i64(&self, sql: &str, params: Vec<Value>) -> Result<i64> {
        let row = self
            .query_first(sql, params)?
            .ok_or_else(|| anyhow::anyhow!("query returned no rows"))?;
        match row.into_iter().next() {
            Some(Value::Integer(i)) => Ok(i),
            Some(v) => Err(anyhow::anyhow!("expected integer, got {v:?}")),
            None => Err(anyhow::anyhow!("query returned no columns")),
        }
    }
}

impl QueryResult {
    pub fn rows(&self) -> &[Vec<Value>] {
        &self.rows
    }

    pub fn len(&self) -> usize {
        self.rows.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }
}

// ---------- helpers ----------

fn default_db_path() -> PathBuf {
    let base = dirs::data_dir()
        .or_else(|| dirs::home_dir().map(|h| h.join(".local").join("share")))
        .expect("cannot determine data directory");
    base.join("cordanui").join("cordanui.db")
}

/// The config file location, public so hosts can offer in-app editing
/// (global settings page writes Turso credentials here).
pub fn config_file_path() -> PathBuf {
    let base = dirs::config_dir()
        .or_else(|| dirs::home_dir().map(|h| h.join(".config")))
        .expect("cannot determine config directory");
    base.join("cordanui").join("config.toml")
}

// ---------- global settings: turso credentials ----------

/// Read the Turso credentials from `path`. `(None, None)` when absent.
pub fn read_turso_credentials_at(path: &Path) -> (Option<String>, Option<String>) {
    let Ok(contents) = std::fs::read_to_string(path) else {
        return (None, None);
    };
    let Ok(parsed) = toml::from_str::<toml::Value>(&contents) else {
        return (None, None);
    };
    match parsed.get("turso") {
        Some(t) => (
            t.get("url").and_then(|v| v.as_str()).map(String::from),
            t.get("token").and_then(|v| v.as_str()).map(String::from),
        ),
        None => (None, None),
    }
}

/// Read Turso credentials from the default config file.
pub fn read_turso_credentials() -> (Option<String>, Option<String>) {
    read_turso_credentials_at(&config_file_path())
}

/// Write Turso credentials to `path`, preserving every other section in
/// the file (keybinds, ...). Creates the file if missing. Refuses to
/// overwrite an existing file that fails to parse — that path used to
/// silently replace the whole file (dropping [turso] and friends).
pub fn write_turso_credentials_at(path: &Path, url: &str, token: &str) -> Result<()> {
    let existing = std::fs::read_to_string(path).ok();
    let root = match existing {
        Some(contents) => match toml::from_str::<toml::Value>(&contents) {
            Ok(v) => v,
            Err(e) => anyhow::bail!(
                "refusing to overwrite {}: existing file is not valid TOML ({e})",
                path.display()
            ),
        },
        None => toml::Value::Table(Default::default()),
    };
    let mut root = root;

    let table = root
        .as_table_mut()
        .context("config.toml root is not a table")?;
    let mut turso = toml::map::Map::new();
    turso.insert("url".into(), toml::Value::String(url.to_string()));
    turso.insert("token".into(), toml::Value::String(token.to_string()));
    table.insert("turso".into(), toml::Value::Table(turso));

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    std::fs::write(
        path,
        toml::to_string_pretty(&root).context("serializing config.toml")?,
    )
    .with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

/// Write Turso credentials to the default config file.
pub fn write_turso_credentials(url: &str, token: &str) -> Result<()> {
    write_turso_credentials_at(&config_file_path(), url, token)
}

// ---------- plugin settings: local mirror ----------

// Plugin settings created through the UI (declarative forms,
// `cord.config.set`) live in the shared `settings` table — which syncs via
// Turso. These helpers mirror them into `[plugins.<name>]` in the local
// config.toml so a device's plugin configuration survives without the
// remote and can be inspected/edited by hand. The database stays the
// runtime source of truth; the file is a durable shadow copy.

/// Read one plugin's mirrored settings from `path` (`[plugins.<name>]`).
pub fn read_plugin_settings_at(
    path: &Path,
    plugin: &str,
) -> std::collections::BTreeMap<String, String> {
    let Ok(contents) = std::fs::read_to_string(path) else {
        return Default::default();
    };
    let Ok(parsed) = toml::from_str::<toml::Value>(&contents) else {
        return Default::default();
    };
    parsed
        .get("plugins")
        .and_then(|p| p.get(plugin))
        .and_then(|t| t.as_table())
        .map(|table| {
            table
                .iter()
                .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                .collect()
        })
        .unwrap_or_default()
}

/// Read one plugin's mirrored settings from the default config file.
pub fn read_plugin_settings(plugin: &str) -> std::collections::BTreeMap<String, String> {
    read_plugin_settings_at(&config_file_path(), plugin)
}

/// Mirror one plugin setting into `path` under `[plugins.<name>]`,
/// preserving every other section. Refuses to overwrite an existing file
/// that fails to parse (same clobber-protection as the creds writer).
pub fn write_plugin_setting_at(path: &Path, plugin: &str, key: &str, value: &str) -> Result<()> {
    let existing = std::fs::read_to_string(path).ok();
    let root = match existing {
        Some(contents) => match toml::from_str::<toml::Value>(&contents) {
            Ok(v) => v,
            Err(e) => anyhow::bail!(
                "refusing to overwrite {}: existing file is not valid TOML ({e})",
                path.display()
            ),
        },
        None => toml::Value::Table(Default::default()),
    };
    let mut root = root;

    let table = root
        .as_table_mut()
        .context("config.toml root is not a table")?;
    let plugins = table
        .entry("plugins")
        .or_insert_with(|| toml::Value::Table(Default::default()))
        .as_table_mut()
        .context("[plugins] section is not a table")?;
    let entry = plugins
        .entry(plugin.to_string())
        .or_insert_with(|| toml::Value::Table(Default::default()))
        .as_table_mut()
        .with_context(|| format!("[plugins.{plugin}] section is not a table"))?;
    entry.insert(key.to_string(), toml::Value::String(value.to_string()));

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    std::fs::write(
        path,
        toml::to_string_pretty(&root).context("serializing config.toml")?,
    )
    .with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

/// Mirror one plugin setting into the default config file.
pub fn write_plugin_setting(plugin: &str, key: &str, value: &str) -> Result<()> {
    write_plugin_setting_at(&config_file_path(), plugin, key, value)
}
