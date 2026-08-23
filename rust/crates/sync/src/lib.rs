//! cordanui-sync — libSQL wrapper with Turso embedded replica sync.
//!
//! Provides a synchronous database API that the TUI and agent backend can
//! use without an async runtime. Internally uses a tokio runtime to drive
//! the async libsql client.
//!
//! ## modes
//!
//! - **Local-only**: when no Turso URL/token is configured, opens a local
//!   libSQL database file. All reads/writes are local. No sync.
//! - **Embedded replica**: when `turso_url` + `turso_token` are provided,
//!   opens a local file as an embedded replica of a remote Turso primary.
//!   Reads are local (fast, offline-capable). Writes go to the local file
//!   and are pushed to Turso in the background. `sync()` pulls remote
//!   changes.
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

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use libsql::{Connection as LibsqlConnection, Database as LibsqlDatabase};

// Re-export Value so consumers (TUI, agent backend) can use it without
// directly depending on the libsql crate.
pub use libsql::Value;

// ---------- public types ----------

/// A synchronous database connection. Wraps an async libsql connection,
/// blocking on each operation via an internal tokio runtime.
pub struct Database {
    conn: LibsqlConnection,
    db: Arc<LibsqlDatabase>,
    runtime: tokio::runtime::Runtime,
    sync_enabled: bool,
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
    /// Open a database with the given config. If sync is enabled, creates an
    /// embedded replica; otherwise opens a local-only database.
    pub fn open(config: &SyncConfig) -> Result<Self> {
        // libSQL won't create parent directories — make sure they exist.
        if let Some(dir) = config.db_path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;

        let (db, sync_enabled) = if config.is_sync_enabled() {
            let url = config.turso_url.as_ref().unwrap();
            let token = config.turso_token.as_ref().unwrap();
            tracing::info!(url = %url, "opening embedded replica with Turso sync");

            let db = runtime.block_on(async {
                libsql::Builder::new_remote_replica(&config.db_path, url.clone(), token.clone())
                    .build()
                    .await
            })?;
            (db, true)
        } else {
            tracing::info!(path = %config.db_path.display(), "opening local-only database");
            let db = runtime.block_on(async {
                libsql::Builder::new_local(&config.db_path).build().await
            })?;
            (db, false)
        };

        let db = Arc::new(db);
        let conn = runtime.block_on(async { db.connect() })?;

        // Apply schema
        runtime.block_on(async {
            conn.execute_batch(cordanui_schema::SCHEMA_SQL).await
        })?;

        // Enable foreign keys
        runtime.block_on(async {
            conn.execute("PRAGMA foreign_keys = ON;", ()).await
        })?;

        Ok(Self {
            conn,
            db,
            runtime,
            sync_enabled,
        })
    }

    /// Open a local-only database at the default path.
    pub fn open_local() -> Result<Self> {
        let config = SyncConfig {
            db_path: default_db_path(),
            ..Default::default()
        };
        Self::open(&config)
    }

    pub fn is_sync_enabled(&self) -> bool {
        self.sync_enabled
    }

    /// Sync with the remote Turso primary. Pulls remote changes and pushes
    /// local writes. No-op if sync is not enabled.
    pub fn sync(&self) -> Result<()> {
        if !self.sync_enabled {
            return Ok(());
        }
        self.runtime
            .block_on(async { self.db.sync().await })?;
        Ok(())
    }

    /// Execute a statement that returns no rows (INSERT, UPDATE, DELETE).
    pub fn execute(&self, sql: &str, params: Vec<Value>) -> Result<()> {
        self.runtime.block_on(async {
            self.conn.execute(sql, params).await
        })?;
        Ok(())
    }

    /// Execute a statement with no parameters.
    pub fn execute_simple(&self, sql: &str) -> Result<()> {
        self.runtime.block_on(async {
            self.conn.execute(sql, ()).await
        })?;
        Ok(())
    }

    /// Execute a batch of statements (e.g. schema migration).
    pub fn execute_batch(&self, sql: &str) -> Result<()> {
        self.runtime.block_on(async {
            self.conn.execute_batch(sql).await
        })?;
        Ok(())
    }

    /// Execute a query and return all rows. Each row is a `Vec<Value>`.
    pub fn query(&self, sql: &str, params: Vec<Value>) -> Result<QueryResult> {
        let stmt = self
            .runtime
            .block_on(async { self.conn.prepare(sql).await })?;

        let mut rows = self
            .runtime
            .block_on(async { stmt.query(params).await })?;

        let mut result_rows = Vec::new();
        loop {
            let row_opt = self.runtime.block_on(async { rows.next().await });
            match row_opt {
                Ok(Some(row)) => {
                    let mut values = Vec::new();
                    let col_count = row.column_count();
                    for i in 0..col_count {
                        let val = row.get_value(i)?;
                        values.push(val);
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

fn config_file_path() -> PathBuf {
    let base = dirs::config_dir()
        .or_else(|| dirs::home_dir().map(|h| h.join(".config")))
        .expect("cannot determine config directory");
    base.join("cordanui").join("config.toml")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_local_database() {
        let dir = std::env::temp_dir().join("cordanui-sync-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let db_path = dir.join("test.db");
        let config = SyncConfig {
            db_path,
            ..Default::default()
        };
        let db = Database::open(&config).unwrap();
        assert!(!db.is_sync_enabled());
    }

    #[test]
    fn round_trip_goal() {
        let dir = std::env::temp_dir().join("cordanui-sync-test2");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let db_path = dir.join("test.db");
        let config = SyncConfig {
            db_path,
            ..Default::default()
        };
        let db = Database::open(&config).unwrap();

        // Insert a goal
        let id = cordanui_schema::new_id();
        let ts = cordanui_schema::now_iso();
        db.execute(
            "INSERT INTO goals (id, title, description, status, parent_id, sort_order, created_at, updated_at) \
             VALUES (?, ?, NULL, 'pending', NULL, 0, ?, ?)",
            vec![
                Value::from(id.clone()),
                Value::from("Test goal"),
                Value::from(ts.clone()),
                Value::from(ts),
            ],
        )
        .unwrap();

        // Read it back
        let result = db
            .query_first(
                "SELECT title FROM goals WHERE id = ?",
                vec![Value::from(id)],
            )
            .unwrap();
        assert!(result.is_some());
        let row = result.unwrap();
        match &row[0] {
            Value::Text(s) => assert_eq!(s, "Test goal"),
            v => panic!("expected text, got {v:?}"),
        }
    }

    #[test]
    fn config_loads_without_file() {
        // When no config file exists, should return local-only config
        // (we can't easily test the real path, but we can test the logic)
        let config = SyncConfig::default();
        assert!(!config.is_sync_enabled());
    }
}
