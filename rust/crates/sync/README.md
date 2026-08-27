# cordanui-sync

Local-first SQLite storage with a custom row-level sync layer — the "one
protocol, two clients" design. The TUI speaks the exact same Hrana-over-HTTP
protocol as the mobile client against Turso Cloud.

## storage

`rusqlite` (bundled SQLite), fully synchronous. Opening the database never
touches the network — there is no replica handshake, no degraded mode. A
local DB file always opens instantly.

## sync

When `~/.config/cordanui/config.toml` contains a `[turso]` section with `url`
and `token`, sync is enabled. The TUI's write layer marks every changed row
dirty in a device-local `_outbox` table; a background worker (on its own
`std::thread`) periodically:

1. **push** — uploads outbox rows to Turso via `POST {url}/v2/pipeline`
   (Hrana over HTTPS) with server-side last-write-wins
   (`WHERE excluded.updated_at > table.updated_at`), then clears the outbox.
2. **pull** — fetches remote changes incrementally (by `updated_at` cursor
   for goals; full table for the small tables) and applies them with a local
   LWW guard.

Network failures are just failed pushes/pulls — logged in the `errors`
table, surfaced in the status bar. Local data is never affected.

## per-table strategy

| table       | direction        | strategy                                   |
| ----------- | ---------------- | ------------------------------------------ |
| `goals`     | two-way          | incremental LWW both ways (`updated_at` cursors + guards) |
| `goal_sheets` | two-way        | full bidirectional upsert (small table, tombstones included) |
| `themes`    | two-way          | full bidirectional upsert (tombstones included) |
| `settings`  | TUI pushes, mobile pulls | snapshot of non-device-local keys |

Device-local (never synced): `plugins`, `errors`, `_outbox`, `_sync_state`,
`_migrations`, and anything prefixed `_`.

Deletes are soft (`deleted_at` tombstone) so they propagate to other clients.
Read paths filter `deleted_at IS NULL`.

## setup

1. Create a Turso database at [turso.tech](https://turso.tech)
2. Copy the sample config:
   ```bash
   mkdir -p ~/.config/cordanui
   cp config.example.toml ~/.config/cordanui/config.toml
   ```
3. Edit `~/.config/cordanui/config.toml` with your Turso URL and token:
   ```toml
   [turso]
   url = "libsql://your-db.turso.io"
   token = "your-auth-token"
   ```

Without this file, cordanui runs in local-only mode — no sync, no network.

## API

```rust
use cordanui_sync::{Database, SyncConfig, Value};

// Open with config (auto-detects Turso config file)
let config = SyncConfig::load()?;
let db = Database::open(&config)?;

// Or open local-only
let db = Database::open_local()?;

// Execute (INSERT, UPDATE, DELETE)
db.execute(
    "INSERT INTO goals (id, title) VALUES (?, ?)",
    vec![Value::from("abc"), Value::from("My goal")],
)?;

// Query (SELECT)
let result = db.query(
    "SELECT title FROM goals WHERE id = ?",
    vec![Value::from("abc")],
)?;

// Mark a row pending push (called by the write layer after every local
// write to a synced table).
db.mark_dirty("goals", "abc")?;

// Manual sync (push outbox + pull remote)
db.sync()?;
```

## how it works

The `Database` struct wraps a `rusqlite::Connection` (bundled SQLite, WAL
mode). All methods are synchronous — no tokio runtime, no async juggling.
The sync worker clones a `Database` handle (each clone opens its own
connection to the same file) and calls `db.sync()` directly on a background
`std::thread`; the UI thread is never blocked.

Sync uses `reqwest::blocking` to POST Hrana pipeline requests to Turso over
HTTPS. The wire protocol is identical to the mobile client's, so both
clients interoperate against the same cloud DB.
