# cordanui-sync

libSQL wrapper with Turso embedded replica sync.

Provides a synchronous database API that the TUI and agent backend can use
without an async runtime. Internally uses a tokio runtime to drive the
async libsql client.

## modes

- **Local-only** (default): when no Turso config is present, opens a local
  libSQL database file. All reads/writes are local. No sync.
- **Embedded replica**: when `~/.config/cordanui/config.toml` contains a
  `[turso]` section with `url` and `token`, opens a local file as an
  embedded replica of a remote Turso primary. Reads are local (fast,
  offline-capable). Writes go to the local file and are pushed to Turso.
  `sync()` pulls remote changes.

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

// Manual sync (pull remote changes + push local writes)
db.sync()?;
```

## how it works

The `Database` struct wraps an async libsql connection. Each method
(`execute`, `query`, `sync`) blocks on the internal tokio runtime via
`block_on`. This lets the TUI (which is synchronous — crossterm event loop)
use libSQL without managing an async runtime.

When sync is enabled, libSQL's embedded replica handles replication:
- Writes go to the local file first (fast, offline)
- libSQL pushes writes to the Turso primary in the background
- `sync()` pulls remote changes from the primary to the local replica
- Last-write-wins on `updated_at` for conflict resolution
