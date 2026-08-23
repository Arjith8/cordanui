//! cordanui agents backend.
//!
//! An HTTP server that receives a task ID, reads the corresponding goal
//! from the database, runs a provider plugin to complete it, and writes
//! progress + result back to the database.
//!
//! This is an optional plugin — the core TUI + goal tracker works without
//! it. Install it only if you want agent/task execution.
//!
//! Phase 1 (no Turso sync): uses local SQLite with the same schema. When
//! Turso sync (phase 2) lands, the db module swaps to libSQL — the server
//! logic stays the same.
//!
//! ## endpoints
//!
//! - `POST /run` — body: `{ "task_id": "..." }`. Wakes the backend, reads
//!   the goal from DB, runs the provider plugin, streams progress to DB,
//!   writes final result to DB.
//! - `GET /health` — health check. Returns `{ "status": "ok" }`.
//!
//! ## configuration
//!
//! All via environment variables:
//! - `CORDANUI_PORT` — port to listen on (default: 3737)
//! - `CORDANUI_AUTH_TOKEN` — shared secret for auth (header: `Authorization: Bearer <token>`)
//! - `CORDANUI_PLUGIN_DIR` — where installed plugins live (default: `~/.local/share/cordanui/plugins`)
//! - `CORDANUI_PROVIDER_PLUGIN` — which provider plugin to use (default: "provider-claude")
//! - `CORDANUI_PROVIDER_MODEL` — which model to use (default: plugin's first model)
//! - `CORDANUI_DB_PATH` — override the database path (default: `~/.local/share/cordanui/cordanui.db`)

mod config;
mod db;
mod executor;
mod server;

pub use config::Config;
pub use executor::ExecutionResult;
pub use server::serve;
