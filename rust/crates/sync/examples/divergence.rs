//! Divergence experiment — what ACTUALLY happens when an embedded replica
//! and the remote primary both moved on since the last sync.
//!
//! Run against a real Turso instance (this is the test we can't fake):
//!
//! ```sh
//! cargo run -p cordanui-sync --example divergence -- \
//!   libsql://your-db.turso.io your-auth-token
//! ```
//!
//! The "mobile" role is played by a libSQL remote client — the same
//! Hrana-over-HTTP protocol the phone's fetch-based client speaks.
//!
//! Scenarios (each uses fresh goal ids, so runs don't collide):
//!   S1  additive divergence      — replica adds A offline, remote adds C
//!   S2  conflicting edit (remote newer timestamp)
//!   S3  conflicting edit (replica newer timestamp)
//!   S4  delete divergence        — replica deletes D offline
//!   S5  two replicas diverge, sync in sequence
//!
//! Each scenario prints the observed state on both sides. Record the
//! results in `agent_docs/sync-divergence-experiment.md`.

use std::path::PathBuf;

use libsql::{Builder, Database};

const GOAL_COLS: &str = "id, title, description, status, parent_id, sort_order, \
     created_at, updated_at, completed_at";

const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS goals (\
        id TEXT PRIMARY KEY, \
        title TEXT NOT NULL, \
        description TEXT, \
        status TEXT NOT NULL DEFAULT 'pending', \
        parent_id TEXT, \
        sort_order INTEGER DEFAULT 0, \
        created_at TEXT NOT NULL, \
        updated_at TEXT NOT NULL, \
        completed_at TEXT)";

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let url = args.next().expect("usage: divergence <url> <token>");
    let token = args.next().expect("usage: divergence <url> <token>");

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;

    // The "mobile" role: a pure remote client (Hrana over HTTP).
    let remote = runtime.block_on(async {
        Builder::new_remote(url.clone(), token.clone())
            .build()
            .await
    })?;
    let remote = remote.connect()?;
    runtime.block_on(async { remote.execute_batch(SCHEMA).await })?;

    // S1–S5, each in its own temp replica file.
    let dir = std::env::temp_dir().join("cordanui-divergence");
    std::fs::create_dir_all(&dir)?;

    let replica = |name: &str| -> anyhow::Result<Database> {
        let path: PathBuf = dir.join(format!("{name}.db"));
        let _ = std::fs::remove_file(&path);
        Ok(runtime.block_on(async {
            Builder::new_remote_replica(path, url.clone(), token.clone())
                .build()
                .await
        })?)
    };

    println!("=== S1: additive divergence (replica adds A offline, remote adds C) ===");
    {
        let rep = replica("s1")?;
        let rep_conn = runtime.block_on(async { rep.connect() })?;
        // Replica writes A locally (no sync yet — "offline").
        runtime.block_on(async {
            rep_conn
                .execute(
                    &format!(
                        "INSERT INTO goals ({GOAL_COLS}) VALUES ('s1-a', 'A from tui', NULL, 'pending', NULL, 0, '2026-08-25T10:00:00Z', '2026-08-25T10:00:00Z', NULL)"
                    ),
                    (),
                )
                .await?;
            anyhow::Ok(())
        })?;
        // Remote writes C.
        runtime.block_on(async {
            remote.execute(
                &format!(
                    "INSERT INTO goals ({GOAL_COLS}) VALUES ('s1-c', 'C from mobile', NULL, 'pending', NULL, 0, '2026-08-25T10:05:00Z', '2026-08-25T10:05:00Z', NULL)"
                ),
                (),
            )
            .await?;
            anyhow::Ok(())
        })?;
        // Replica comes online and syncs.
        match runtime.block_on(async { rep.sync().await }) {
            Ok(_) => println!("  sync: ok"),
            Err(e) => println!("  sync: ERROR — {e}"),
        }
        dump(&runtime, &rep_conn, &remote, "S1");
    }

    println!("\n=== S2: conflicting edit, remote timestamp NEWER ===");
    {
        let rep = replica("s2")?;
        let rep_conn = runtime.block_on(async { rep.connect() })?;
        // Seed B on the remote, pull it into the replica.
        runtime.block_on(async {
            remote.execute(
                &format!(
                    "INSERT INTO goals ({GOAL_COLS}) VALUES ('s2-b', 'B original', NULL, 'pending', NULL, 0, '2026-08-25T10:00:00Z', '2026-08-25T10:00:00Z', NULL)"
                ),
                (),
            )
            .await?;
            rep.sync().await?;
            anyhow::Ok(())
        })?;
        // Both edit B; remote's edit is newer.
        runtime.block_on(async {
            rep_conn
                .execute(
                    "UPDATE goals SET title = 'B edited by tui', updated_at = '2026-08-25T11:00:00Z' WHERE id = 's2-b'",
                    (),
                )
                .await?;
            remote.execute(
                "UPDATE goals SET title = 'B edited by mobile', updated_at = '2026-08-25T12:00:00Z' WHERE id = 's2-b'",
                (),
            )
            .await?;
            anyhow::Ok(())
        })?;
        match runtime.block_on(async { rep.sync().await }) {
            Ok(_) => println!("  sync: ok"),
            Err(e) => println!("  sync: ERROR — {e}"),
        }
        dump(&runtime, &rep_conn, &remote, "S2");
    }

    println!("\n=== S3: conflicting edit, REPLICA timestamp NEWER ===");
    {
        let rep = replica("s3")?;
        let rep_conn = runtime.block_on(async { rep.connect() })?;
        runtime.block_on(async {
            remote.execute(
                &format!(
                    "INSERT INTO goals ({GOAL_COLS}) VALUES ('s3-b', 'B original', NULL, 'pending', NULL, 0, '2026-08-25T10:00:00Z', '2026-08-25T10:00:00Z', NULL)"
                ),
                (),
            )
            .await?;
            rep.sync().await?;
            // Replica edits newer; remote edits older.
            rep_conn
                .execute(
                    "UPDATE goals SET title = 'B edited by tui', updated_at = '2026-08-25T12:00:00Z' WHERE id = 's3-b'",
                    (),
                )
                .await?;
            remote.execute(
                "UPDATE goals SET title = 'B edited by mobile', updated_at = '2026-08-25T11:00:00Z' WHERE id = 's3-b'",
                (),
            )
            .await?;
            anyhow::Ok(())
        })?;
        match runtime.block_on(async { rep.sync().await }) {
            Ok(_) => println!("  sync: ok"),
            Err(e) => println!("  sync: ERROR — {e}"),
        }
        dump(&runtime, &rep_conn, &remote, "S3");
    }

    println!("\n=== S4: delete divergence (replica deletes D offline) ===");
    {
        let rep = replica("s4")?;
        let rep_conn = runtime.block_on(async { rep.connect() })?;
        runtime.block_on(async {
            remote.execute(
                &format!(
                    "INSERT INTO goals ({GOAL_COLS}) VALUES ('s4-d', 'D to delete', NULL, 'pending', NULL, 0, '2026-08-25T10:00:00Z', '2026-08-25T10:00:00Z', NULL)"
                ),
                (),
            )
            .await?;
            rep.sync().await?;
            rep_conn
                .execute("DELETE FROM goals WHERE id = 's4-d'", ())
                .await?;
            anyhow::Ok(())
        })?;
        match runtime.block_on(async { rep.sync().await }) {
            Ok(_) => println!("  sync: ok"),
            Err(e) => println!("  sync: ERROR — {e}"),
        }
        dump(&runtime, &rep_conn, &remote, "S4");
    }

    println!("\n=== S5: two replicas diverge, sync in sequence ===");
    {
        let rep1 = replica("s5a")?;
        let rep2 = replica("s5b")?;
        let rep1_conn = runtime.block_on(async { rep1.connect() })?;
        let rep2_conn = runtime.block_on(async { rep2.connect() })?;
        runtime.block_on(async {
            remote.execute(
                &format!(
                    "INSERT INTO goals ({GOAL_COLS}) VALUES ('s5-base', 'seed', NULL, 'pending', NULL, 0, '2026-08-25T10:00:00Z', '2026-08-25T10:00:00Z', NULL)"
                ),
                (),
            )
            .await?;
            rep1.sync().await?;
            rep2.sync().await?;
            // Both replicas edit the SAME row offline.
            rep1_conn
                .execute(
                    "UPDATE goals SET title = 'edited on replica 1', updated_at = '2026-08-25T11:00:00Z' WHERE id = 's5-base'",
                    (),
                )
                .await?;
            rep2_conn
                .execute(
                    "UPDATE goals SET title = 'edited on replica 2', updated_at = '2026-08-25T11:30:00Z' WHERE id = 's5-base'",
                    (),
                )
                .await?;
            anyhow::Ok(())
        })?;
        print!("  replica1 sync: ");
        match runtime.block_on(async { rep1.sync().await }) {
            Ok(_) => println!("ok"),
            Err(e) => println!("ERROR — {e}"),
        }
        print!("  replica2 sync: ");
        match runtime.block_on(async { rep2.sync().await }) {
            Ok(_) => println!("ok"),
            Err(e) => println!("ERROR — {e}"),
        }
        // Dump via remote (shared) + replica2.
        dump(&runtime, &rep2_conn, &remote, "S5");
    }

    println!("\nDone. Record results in agent_docs/sync-divergence-experiment.md");
    Ok(())
}

fn dump(
    runtime: &tokio::runtime::Runtime,
    replica_conn: &libsql::Connection,
    remote: &libsql::Connection,
    label: &str,
) {
    let show = |what: &str, rows: Vec<(String, String)>| {
        println!("  {label} {what}:");
        if rows.is_empty() {
            println!("    (no s* goals)");
        }
        for (id, title) in rows {
            println!("    {id} = \"{title}\"");
        }
    };
    let query = |conn: &libsql::Connection| -> Vec<(String, String)> {
        runtime.block_on(async {
            let mut stmt = conn
                .prepare("SELECT id, title FROM goals WHERE id LIKE 's%' ORDER BY id")
                .await
                .expect("prepare");
            let mut rows = stmt.query(()).await.expect("query");
            let mut out = Vec::new();
            while let Some(row) = rows.next().await.expect("row") {
                let id: String = row.get(0).unwrap();
                let title: String = row.get(1).unwrap();
                out.push((id, title));
            }
            out
        })
    };
    show("replica", query(replica_conn));
    show("remote ", query(remote));
}
