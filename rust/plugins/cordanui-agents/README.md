# cordanui-agents

Optional agent backend plugin for cordanui.

An HTTP server that receives a task ID, reads the corresponding goal from
the database, runs a provider plugin to complete it, and writes progress +
result back to the database.

**This is optional.** The core TUI + goal tracker works without it. Install
it only if you want agent/task execution.

## status

Scaffold complete. Uses local SQLite (same schema as TUI) until Turso sync
(phase 2) lands. When that happens, only the `db` module swaps to libSQL —
the server and executor logic stays the same.

## run

```bash
cargo run -p cordanui-agents
```

### environment variables

| Variable | Default | Description |
|---|---|---|
| `CORDANUI_PORT` | `3737` | Port to listen on |
| `CORDANUI_AUTH_TOKEN` | (none) | Shared secret for auth. If set, requests must include `Authorization: Bearer <token>` |
| `CORDANUI_PLUGIN_DIR` | `~/.local/share/cordanui/plugins` | Where installed provider plugins live |
| `CORDANUI_PROVIDER_PLUGIN` | `provider-claude` | Which provider plugin to use |
| `CORDANUI_PROVIDER_MODEL` | (plugin default) | Which model to use |
| `CORDANUI_DB_PATH` | `~/.local/share/cordanui/cordanui.db` | Override the database path |
| `RUST_LOG` | `info` | Tracing log level |

## endpoints

### `POST /run`

Body: `{ "task_id": "abc123" }`

Wakes the backend, reads the goal from DB, runs the provider plugin,
streams progress to DB, writes final result to DB. Returns immediately with
an acknowledgement — the actual execution runs in the background.

```bash
curl -X POST http://localhost:3737/run \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer $CORDANUI_AUTH_TOKEN" \
  -d '{"task_id": "abc123"}'
```

Response: `{ "task_id": "abc123", "accepted": true, "message": "task accepted, execution started" }`

### `GET /health`

Returns `{ "status": "ok", "version": "0.1.0" }`.

## architecture

```
POST /run { task_id }
       │
       ▼
   ┌──────────┐
   │  Server  │  axum HTTP, auth check, ack immediately
   └────┬─────┘
        │ spawn background task
        ▼
   ┌──────────┐
   │ Executor │  reads goal from DB, resolves plugin, runs it
   └────┬─────┘
        │ spawn subprocess
        ▼
   ┌──────────┐
   │  Plugin  │  agent-run --task-id X < config.json
   │ (binary) │  → streams JSON lines to stdout
   └────┬─────┘
        │ progress events
        ▼
   ┌──────────┐
   │   DB     │  write agent_progress, agent_result, agent_status
   └──────────┘
```

## structure

```
src/
├── main.rs       # binary entry point — tokio runtime, logging init
├── config.rs     # Config struct, loads from env vars
├── db.rs         # SQLite access: get goal, write progress/result/failure
├── executor.rs   # ties it together: read goal → resolve plugin → run → write back
└── server.rs     # axum HTTP server, /run and /health endpoints
```

## relationship to the rest of cordanui

```
cordanui/
├── rust/
│   ├── crates/
│   │   ├── schema/            # shared types (required)
│   │   ├── plugin-runtime/    # manifest parsing + subprocess spawning (required)
│   │   └── tui/               # goal tracker (required)
│   └── plugins/
│       └── cordanui-agents/   # ← you are here (optional)
```

The TUI has no dependency on this plugin. It's a standalone HTTP service
that reads/writes the same database. Clients (TUI, mobile) trigger it with
a `POST /run { task_id }` and read results back from the shared DB.
