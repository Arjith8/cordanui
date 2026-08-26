//! ONE-OFF migration: move the existing libSQL database over to the Turso
//! Database engine with sync enabled.
//!
//! What it does:
//! 1. Reads every row of the shared tables from the OLD libsql/sqlite file
//!    (via rusqlite, read-only).
//! 2. Renames the old files to `*.libsql-bak` (nothing is deleted).
//! 3. Opens a FRESH synced database at the canonical path
//!    (`bootstrap_if_empty(false)` — the remote is never consulted).
//! 4. Replays all rows through the new connection — writes go through the
//!    engine, so they are CDC-tracked and will be uploaded by push().
//! 5. Runs one sync (push + pull).
//!
//! Idempotence guard: refuses to run if a `.libsql-bak` already exists.

use std::path::{Path, PathBuf};

use cordanui_sync::{Database, SyncConfig};
use rusqlite::Connection;

const TABLES: &[&str] = &["goals", "themes", "settings", "plugins"];

fn main() -> anyhow::Result<()> {
    let data_dir = dirs::data_dir()
        .expect("data dir")
        .join("cordanui");
    let old_path: PathBuf = data_dir.join("cordanui.db");
    let bak_path: PathBuf = data_dir.join("cordanui.db.libsql-bak");

    anyhow::ensure!(
        old_path.exists(),
        "no database found at {}",
        old_path.display()
    );
    anyhow::ensure!(
        !bak_path.exists(),
        "already migrated — {} exists",
        bak_path.display()
    );

    // --- 1. read everything out of the old file ---
    let conn = Connection::open_with_flags(
        &old_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    let mut dump: Vec<(String, Vec<String>, Vec<Vec<cordanui_sync::Value>>)> = Vec::new();
    for table in TABLES {
        let exists: bool = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
                [table],
                |r| r.get::<_, i64>(0),
            )?
            > 0;
        if !exists {
            println!("skip {table} (not present in old db)");
            continue;
        }
        let mut stmt = conn.prepare(&format!("SELECT * FROM {table}"))?;
        let cols: Vec<String> = stmt.column_names().iter().map(|s| s.to_string()).collect();
        let n_cols = cols.len();
        let mut rows_out = Vec::new();
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            let mut values = Vec::with_capacity(n_cols);
            for i in 0..n_cols {
                let v: rusqlite::types::Value = row.get(i)?;
                values.push(match v {
                    rusqlite::types::Value::Null => cordanui_sync::Value::Null,
                    rusqlite::types::Value::Integer(n) => cordanui_sync::Value::Integer(n),
                    rusqlite::types::Value::Real(f) => cordanui_sync::Value::Real(f),
                    rusqlite::types::Value::Text(s) => cordanui_sync::Value::Text(s),
                    rusqlite::types::Value::Blob(b) => cordanui_sync::Value::Blob(b),
                });
            }
            rows_out.push(values);
        }
        println!("read {}: {} rows, {} cols", table, rows_out.len(), n_cols);
        dump.push((table.to_string(), cols, rows_out));
    }
    drop(conn);

    // --- 2. set old files aside ---
    std::fs::rename(&old_path, &bak_path)?;
    for suffix in ["-wal", "-shm", "-client_wal_index"] {
        let src = PathBuf::from(format!("{}{}", old_path.display(), suffix));
        if src.exists() {
            let dst = PathBuf::from(format!("{}{}", bak_path.display(), suffix));
            std::fs::rename(&src, &dst)?;
        }
    }
    println!("old files moved aside ({} kept)", bak_path.display());

    // --- 3. fresh synced database at the canonical path ---
    let config = SyncConfig::load()?;
    let db = Database::open(&config)?;
    println!(
        "new engine opened (sync bound: {})",
        db.is_sync_enabled()
    );

    // --- 4. replay rows through the new engine (CDC-tracked) ---
    for (table, cols, rows) in &dump {
        let col_list = cols.join(", ");
        let placeholders = vec!["?"; cols.len()].join(", ");
        let sql = format!(
            "INSERT OR REPLACE INTO {table} ({col_list}) VALUES ({placeholders})"
        );
        for row in rows {
            db.execute(&sql, row.clone())?;
        }
        println!("replayed {table}: {} rows", rows.len());
    }

    // --- 5. push everything up ---
    match db.sync() {
        Ok(()) => println!("sync (push+pull): OK"),
        Err(e) => {
            println!("sync failed (local data is safe, run <leader>s later): {e:#}");
        }
    }

    let count = db.query_simple("SELECT COUNT(*) FROM goals")?;
    println!("goals in new local db: {:?}", count.rows()[0][0]);
    Ok(())
}

#[allow(dead_code)]
fn unused(_: &Path) {}
