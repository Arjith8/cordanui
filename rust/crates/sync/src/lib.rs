//! cordanui-sync — Turso Database engine wrapper with local-first sync.
//!
//! Provides a synchronous database API that the TUI and agent backend can
//! use without an async runtime. Internally uses a tokio runtime to drive
//! the async `turso` client.
//!
//! ## modes
//!
//! - **Local-only**: when no Turso URL/token is configured, opens a local
//!   database file. All reads/writes are local. No sync.
//! - **Synced**: when `turso_url` + `turso_token` are provided, the same
//!   local file is backed by a Turso Cloud remote. Opening NEVER touches
//!   the network (local-first): reads/writes always land in the local
//!   file instantly, and [`Database::sync`] performs an explicit
//!   push-then-pull over HTTP. Offline edits stay safe locally until the
//!   next successful push.
//!
//! Conflict strategy is the server's "last push wins" (row-level logical
//! CDC), which matches the mobile app's LWW-by-updated_at contract far
//! better than page-frame replication ever did.
//!
//! ## config
//!
//! Turso config lives at `~/.config/cordanui/config.toml`:
//!
//! ```toml
//! [turso]
//! url = "libsql://your-db.turso.io"
//! token = "your-auth-token"
//! ```
//!
//! If the file doesn't exist or the `[turso]` section is missing, the
//! database opens in local-only mode.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};

// Re-export Value so consumers (TUI, agent backend) can use it without
// directly depending on the turso crate.
pub use turso::Value;

// ---------- public types ----------

/// A synchronous database connection. Wraps an async turso connection,
/// blocking on each operation via an internal tokio runtime.
///
/// Cheap to clone: everything inside is an Arc handle. Hosts should open
/// the database ONCE per process and clone for additional handles.
#[derive(Clone)]
pub struct Database {
    conn: turso::Connection,
    inner: Inner,
    runtime: Arc<tokio::runtime::Runtime>,
    sync_enabled: bool,
}

/// The underlying engine handle. `sync::Builder` requires a remote URL, so
/// local-only databases go through the plain builder instead.
#[derive(Clone)]
enum Inner {
    Local(turso::Database),
    Synced(Arc<turso::sync::Database>),
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
    /// Open a database with the given config. Never touches the network:
    /// with sync credentials the local file is bound to a Turso Cloud
    /// remote, but actual push/pull only happens inside [`Database::sync`]
    /// (and therefore off the startup path entirely).
    pub fn open(config: &SyncConfig) -> Result<Self> {
        // The engine won't create parent directories — make sure they exist.
        if let Some(dir) = config.db_path.parent() {
            std::fs::create_dir_all(dir)?;
        }

        let runtime = Arc::new(
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?,
        );

        let path = config.db_path.to_string_lossy().into_owned();
        let (inner, sync_enabled) = if config.is_sync_enabled() {
            tracing::info!(
                url = %config.turso_url.as_ref().unwrap(),
                "opening local database bound to Turso remote (sync deferred)"
            );
            // Local-first: never bootstrap from / never require the
            // remote at open time. Initial population happens by
            // replaying data through this connection and pushing.
            let db = runtime.block_on(
                turso::sync::Builder::new_remote(&path)
                    .with_remote_url(config.turso_url.as_ref().unwrap().clone())
                    .with_auth_token(config.turso_token.as_ref().unwrap().clone())
                    .bootstrap_if_empty(false)
                    .build(),
            )?;
            (Inner::Synced(Arc::new(db)), true)
        } else {
            tracing::info!(path = %config.db_path.display(), "opening local-only database");
            let db = runtime.block_on(turso::Builder::new_local(&path).build())?;
            (Inner::Local(db), false)
        };

        let t = std::time::Instant::now();
        let conn = match &inner {
            // sync Database::connect is async; plain Database::connect is sync
            Inner::Local(db) => db.connect(),
            Inner::Synced(db) => runtime.block_on(async { db.connect().await }),
        }?;
        tracing::debug!(elapsed = ?t.elapsed(), "database opened");

        Self::finish(conn, inner, runtime, sync_enabled)
    }

    /// Open a local-only database at the default path.
    pub fn open_local() -> Result<Self> {
        let config = SyncConfig {
            db_path: default_db_path(),
            ..Default::default()
        };
        Self::open(&config)
    }

    /// Post-open setup shared by every mode: pragmas + schema migrations.
    fn finish(
        conn: turso::Connection,
        inner: Inner,
        runtime: Arc<tokio::runtime::Runtime>,
        sync_enabled: bool,
    ) -> Result<Self> {
        // Foreign keys must be re-enabled per connection.
        runtime.block_on(async { conn.execute_batch("PRAGMA foreign_keys = ON;").await })?;

        // Apply schema + migrations. This runs on every startup; the
        // `_migrations` table records what has been applied so each
        // migration executes at most once per database.
        runtime.block_on(async {
            // Does this database predate the migration system? (A DB with a
            // `goals` table was created before/without migrations; a fresh
            // empty file gets the latest schema directly and only needs its
            // migrations recorded.)
            let mut rows = conn
                .query(
                    "SELECT name FROM sqlite_master WHERE type = 'table' AND name = 'goals'",
                    Vec::<Value>::new(),
                )
                .await?;
            let pre_existing = rows.next().await?.is_some();

            conn.execute_batch(cordanui_schema::SCHEMA_SQL).await?;

            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS _migrations (\
                     version    INTEGER PRIMARY KEY,\
                     name       TEXT NOT NULL,\
                     applied_at TEXT NOT NULL\
                 );",
            )
            .await?;

            for m in cordanui_schema::MIGRATIONS {
                let mut rows = conn
                    .query(
                        "SELECT 1 FROM _migrations WHERE version = ?",
                        vec![Value::from(m.version)],
                    )
                    .await?;
                if rows.next().await?.is_some() {
                    continue; // already applied
                }

                if pre_existing {
                    conn.execute_batch(m.sql).await?;
                }
                // Fresh installs: SCHEMA_SQL already produced the final
                // shape — just record the migration as applied.

                conn.execute(
                    "INSERT INTO _migrations (version, name, applied_at) \
                     VALUES (?, ?, datetime('now'))",
                    vec![Value::from(m.version), Value::from(m.name)],
                )
                .await?;
            }
            Ok::<(), anyhow::Error>(())
        })?;

        Ok(Self {
            conn,
            inner,
            runtime,
            sync_enabled,
        })
    }

    pub fn is_sync_enabled(&self) -> bool {
        self.sync_enabled
    }

    /// Push local changes to Turso, then pull remote changes down. No-op
    /// if sync is not enabled. Network failures propagate to the caller —
    /// local data is never affected.
    pub fn sync(&self) -> Result<()> {
        match &self.inner {
            Inner::Local(_) => Ok(()),
            Inner::Synced(db) => self.runtime.block_on(async {
                db.push().await?;
                let changed = db.pull().await?;
                tracing::debug!(changed, "sync pull finished");
                Ok(())
            }),
        }
    }

    /// Execute a statement that returns no rows (INSERT, UPDATE, DELETE).
    pub fn execute(&self, sql: &str, params: Vec<Value>) -> Result<()> {
        self.runtime
            .block_on(async { self.conn.execute(sql, params).await })?;
        Ok(())
    }

    /// Execute a statement with no parameters.
    pub fn execute_simple(&self, sql: &str) -> Result<()> {
        self.runtime
            .block_on(async { self.conn.execute(sql, ()).await })?;
        Ok(())
    }

    /// Execute a batch of statements (e.g. schema migration).
    pub fn execute_batch(&self, sql: &str) -> Result<()> {
        self.runtime
            .block_on(async { self.conn.execute_batch(sql).await })?;
        Ok(())
    }

    /// Execute a query and return all rows. Each row is a `Vec<Value>`.
    pub fn query(&self, sql: &str, params: Vec<Value>) -> Result<QueryResult> {
        let mut rows = self.runtime.block_on(async {
            let mut stmt = self.conn.prepare(sql).await?;
            stmt.query(params).await
        })?;

        let mut result_rows = Vec::new();
        loop {
            let row_opt = self.runtime.block_on(async { rows.next().await });
            match row_opt {
                Ok(Some(row)) => {
                    let mut values = Vec::new();
                    for i in 0..row.column_count() {
                        values.push(row.get_value(i)?);
                    }
                    result_rows.push(values);
                }
                Ok(None) => break,
                Err(e) => return Err(e.into()),
            }
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
/// the file (keybinds, ...). Creates the file if missing.
pub fn write_turso_credentials_at(path: &Path, url: &str, token: &str) -> Result<()> {
    let mut root = std::fs::read_to_string(path)
        .ok()
        .and_then(|c| toml::from_str::<toml::Value>(&c).ok())
        .unwrap_or_else(|| toml::Value::Table(Default::default()));

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
/// preserving every other section.
pub fn write_plugin_setting_at(path: &Path, plugin: &str, key: &str, value: &str) -> Result<()> {
    let mut root = std::fs::read_to_string(path)
        .ok()
        .and_then(|c| toml::from_str::<toml::Value>(&c).ok())
        .unwrap_or_else(|| toml::Value::Table(Default::default()));

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

#[cfg(test)]
mod tests;
