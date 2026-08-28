//! cordanui-agents — the agent backend.
//!
//! A standalone Rust binary that runs on a server or VM. It reads queued
//! goals from the shared Turso database, invokes the configured provider
//! plugin (binary or Lua), streams progress back to Turso, and writes the
//! final result.
//!
//! Two modes:
//! - **poll mode** (default): periodically scans Turso for goals with
//!   `status = 'agent_mode' AND agent_status = 'queued'`, runs each one.
//! - **wake mode** (`--serve`): starts an HTTP server that receives
//!   `POST /wake { task_id }` from a client (TUI or mobile), which triggers
//!   an immediate poll. The HTTP call is a wake-and-point — the task data
//!   lives in Turso, not in the HTTP body.
//!
//! The backend uses the same crates as the TUI:
//! - `cordanui-sync` for the shared database + Hrana-over-HTTP sync
//! - `cordanui-plugin-runtime` for manifest parsing + plugin invocation
//! - `cordanui-schema` for the shared data model
//!
//! It is an optional component — installed as a plugin, not part of the
//! core app. The TUI announces agent capability by writing `agent.url` to
//! the synced settings table; mobile reads it to show/hide the agent UI.

mod db;
mod runner;

use std::sync::Arc;

use anyhow::Result;
use tracing_subscriber::EnvFilter;

const DEFAULT_POLL_INTERVAL_SECS: u64 = 30;
const DEFAULT_PORT: u16 = 8081;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let db = cordanui_sync::Database::open(&cordanui_sync::SyncConfig::load()?)?;
    if !db.is_sync_enabled() {
        anyhow::bail!(
            "agent backend requires Turso sync to be configured. \
             Set [turso] in ~/.config/cordanui/config.toml"
        );
    }

    let runner = Arc::new(runner::AgentRunner::new(db)?);
    let args: Vec<String> = std::env::args().skip(1).collect();

    match args.first().map(String::as_str) {
        None | Some("--poll") => {
            let interval = parse_flag(&args, "--interval")
                .and_then(|s| s.parse().ok())
                .unwrap_or(DEFAULT_POLL_INTERVAL_SECS);
            tracing::info!(interval_secs = interval, "starting poll mode");
            runner.poll_loop(interval).await;
            Ok(())
        }
        Some("--serve") => {
            let port = parse_flag(&args, "--port")
                .and_then(|s| s.parse().ok())
                .unwrap_or(DEFAULT_PORT);
            tracing::info!(port, "starting wake mode (HTTP server)");
            runner.serve(port).await
        }
        Some("--run-once") => {
            let task_id = args
                .get(1)
                .ok_or_else(|| anyhow::anyhow!("usage: cordanui-agents --run-once <task_id>"))?;
            runner.process_task(task_id.clone()).await;
            Ok(())
        }
        Some(other) => {
            anyhow::bail!("unknown mode '{other}'. Use --poll, --serve, or --run-once <task_id>")
        }
    }
}

/// Extract the value after a --flag from args, if present.
fn parse_flag<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    let idx = args.iter().position(|a| a == flag)?;
    args.get(idx + 1).map(String::as_str)
}
