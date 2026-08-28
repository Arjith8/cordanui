# cordanui-agents backend — self-contained build spec

Give this to an agent with **no repo access**. It is the only context it needs.

---

## 0. what is cordanui

Personal goal tracker. Three peers, `Turso (libSQL cloud)` is only shared state:

```
TUI (Rust/ratatui) ──Hrana POST /v2/pipeline──┐
                                              ├─ Turso
Mobile (Expo/React Native) ──Hrana───────────┘
                         │
Agent backend (this) ──Hrana + HTTP wake── Turso
```

* No client-to-client calls. HTTP to backend is wake-and-point `{task_id}` only; data lives in Turso.
* `goals.status = 'agent_mode' && agent_status = 'queued'` means "pick me up".
* Backend: `sync pull → run provider/agent plugin → stream agent_progress → write agent_result → sync push`.

## 1. what you must build

A Rust binary `cordanui-agents` (or any language, Rust reference) that:

1. Opens a local SQLite file (WAL, `busy_timeout 5000`) at `~/.local/share/cordanui/cordanui.db` (or `$CORDANUI_DB` if set). Creates schema if missing.
2. Reads Turso credentials from `~/.config/cordanui/config.toml`:
   ```toml
   [turso]
   url = "libsql://…-turso.io"  # or https://
   token = "eyJ…"
   ```
   `libsql://` → `https://` for HTTP. If missing → `bail!("requires Turso sync")`.
3. Polls for queued tasks and/or serves `POST /wake`.
4. Resolves which **plugin** to run (any with `capabilities.agent=true` or `capabilities.provider=true`), collects its settings, invokes it via **JSON-over-stdio** (binary) or **in-process Lua** (`main.lua`), streams progress to DB, writes final result. Also merges declarative `mobile.json` → `goals.metadata` so mobile FE changes without code on device.

---

## 2. canonical DB schema — create exactly this

```sql
CREATE TABLE IF NOT EXISTS goals (
    id           TEXT PRIMARY KEY,  -- UUID, subgoals: "<parent>.<uuid>"
    title        TEXT NOT NULL,
    description  TEXT,
    status       TEXT NOT NULL DEFAULT 'pending', -- pending|in_progress|completed|agent_mode
    parent_id    TEXT REFERENCES goals(id) ON DELETE CASCADE,
    sheet_id     TEXT REFERENCES goal_sheets(id) ON DELETE SET NULL,
    sort_order   INTEGER NOT NULL DEFAULT 0,
    created_at   TEXT NOT NULL,  -- RFC3339
    updated_at   TEXT NOT NULL,
    completed_at TEXT,
    agent_status   TEXT,  -- queued|running|completed|failed
    agent_result   TEXT,  -- JSON {content, files[]}
    agent_progress TEXT,  -- JSON {message, detail?}
    metadata       TEXT,  -- JSON, plugins attach arbitrary data
    deleted_at     TEXT   -- tombstone, readers filter IS NULL
);
CREATE TABLE IF NOT EXISTS goal_sheets (id TEXT PK, name TEXT NOT NULL, created_at TEXT NOT NULL, deleted_at TEXT);
CREATE TABLE IF NOT EXISTS themes (id TEXT PK, name TEXT NOT NULL, source TEXT DEFAULT 'builtin', colors_json TEXT NOT NULL, last_used_at TEXT, deleted_at TEXT);
CREATE TABLE IF NOT EXISTS settings (key TEXT PK, value TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS plugins (id TEXT PK, source TEXT NOT NULL, dir TEXT NOT NULL, active INT DEFAULT 0, installed_at TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS errors (id TEXT PK, context TEXT NOT NULL, message TEXT NOT NULL, detail TEXT, created_at TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS _migrations (version INT PK, name TEXT NOT NULL, applied_at TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS _outbox (table_name TEXT NOT NULL, row_id TEXT NOT NULL, PRIMARY KEY(table_name,row_id));
CREATE TABLE IF NOT EXISTS _sync_state (key TEXT PK, value TEXT NOT NULL);
```

Mark dirty after every write: `INSERT OR IGNORE INTO _outbox(table_name,row_id) VALUES('goals',?)`.

---

## 3. sync (Hrana over HTTP)

You do **not** need libSQL driver. Use `POST {https_url}/v2/pipeline` with `Authorization: Bearer {token}`.
Body: `{requests:[{type:"execute",stmt:{sql,args:[{type:"text",value}]}}...,{type:"close"}]}`.

* `goals` = LWW on `updated_at`. Push: `SELECT * FROM goals WHERE updated_at > last_push` → `INSERT … ON CONFLICT(id) DO UPDATE SET col=excluded.col WHERE excluded.updated_at > goals.updated_at`. Pull: `SELECT * FROM goals WHERE updated_at > last_pull` → same upsert locally, skip if local `updated_at >= remote`. Tombstones (`deleted_at IS NOT NULL`) propagate same way; readers filter.
* `goal_sheets, themes` small → full replace pull/push (not incremental) is acceptable for MVP.
* `settings` pull-only except `agent.url` etc. Never sync keys `turso_url, turso_token, sync.*, _*` (device-local).
* Cursors in `_sync_state` (`last_pull`, `last_push`) as RFC3339.

If you can't implement pipeline, a correct stub is: `sync()` = no-op when offline, else push then pull as above. Do not block if token missing.

---

## 4. plugin system you must speak

### 4.1 manifest `cordanui.toml` at plugin repo root, installed to `~/.local/share/cordanui/plugins/<id>/cordanui.toml`

```toml
# for Lua:
runtime = "lua"   # MUST be at manifest root, before any [section]
[plugin]
name = "my-agent"
version = "0.1.0"
[capabilities]
agent = true      # this backend cares about agent||provider
provider = false
[provider]        # only if provider=true
models = ["grok-code","gpt-5"]
api_key_env = "OPENCODE_API_KEY"
[[field]]         # declarative settings, host renders form
key="api_key" label="API Key" type="secret" required=true
[[help]]          # optional
title="Getting started" text="…"

[service]  # only for this backend itself when shipped as plugin
cmd = "./target/release/cordanui-agents"
args = ["--serve","--port","8081"]
addr = "http://127.0.0.1:8081"
health = "http://127.0.0.1:8081/health"
```

Validation: `runtime` ∈ `{binary,lua}`, `service.addr/health` must be `http(s)://`.

### 4.2 discovery

`SELECT id, dir, active FROM plugins ORDER BY installed_at DESC` where `active=1`. For each, read `dir/cordanui.toml` via `PluginManifest::from_dir`, skip if not `agent||provider`. Binary plugins require `binary_path = dir/<bin>` exists (default `target/release/<name>`); Lua never needs a binary. Collect settings:

```sql
SELECT key,value FROM settings WHERE key LIKE 'my-agent.%'  -- strip prefix
```
Merge manifest `[[field]]` defaults where missing. Convert to `config: Option<Value::Object>` where every value is `String` (even bools/numbers). Empty → `None`.

### 4.3 wire — exact JSON, do not rename fields

```rust
pub struct AgentRunConfig {
    pub task_id: String, pub title: String,
    pub description: Option<String>,
    pub model: Option<String>,
    pub config: Option<serde_json::Value>,
}
#[derive(Deserialize)] #[serde(tag="type")]
pub enum AgentEvent {
    #[serde(rename="progress")] Progress{ message:String, detail:Option<String> },
    #[serde(rename="result")]   Result(AgentResult),
    #[serde(rename="error")]    Error{ message:String, detail:Option<String> },
}
pub struct AgentResult { pub content:String, #[serde(default)] pub files:Vec<AgentFile>, pub usage:Option<Usage> }
pub struct AgentFile { pub path:String, pub content:Option<String> }
```

Binary invocation:
```
$ <binary> agent-run --task-id <id> < AgentRunConfig.json
stdout NDJSON: {"type":"progress","message":"…","detail":null}\n … {"type":"result","content":"…","files":[]}\n
stderr = logs only. Exit 0, buffered stdout must be flushed per line, first progress immediately.
```

Lua invocation (if you support it, otherwise stub and document):
```lua
plugin = {}
function plugin.agent_run(cfg, emit)
  emit{type="progress", message="working"}
  emit{type="result", content="done", files=cordanui.array({})}
end
```
Load via `LuaPlugin::load(dir,name,config, HostHooks::new())` and call `agent_run(cfg, |ev| tx.send(ev))`.

---

## 5. provider/agent resolution

`goals.metadata` is JSON written by TUI/mobile picker. Parse:

```json
{"agent":"my-agent", "provider":"provider-zen", "model":"grok-code"}
```
`agent` wins over `provider` (compat). `model` optional.

Algorithm `resolve_provider(goal)`:

```
wanted = metadata.agent ?? metadata.provider   // string or None
for pass in [0,1]:
  for row in plugins (active):
    if !row.capabilities.agent && !row.capabilities.provider: continue
    if pass==0 && wanted!=None && manifest.name != wanted: continue
    if pass==0 && wanted==None: continue
    has_models = manifest.provider?.models?.len>0
    if provider && !agent && !has_models: continue // malformed provider
    model = metadata.model if in manifest.provider.models else first model else ""
    config = settings_to_config(collected values)
    return {plugin_name, model, dir, manifest, config}
bail!("no active agent/provider plugin found")
```

`AgentRunConfig.model = if model=="" {None} else {Some(model)}`.

---

## 6. HTTP + poll

```
cordanui-agents [--poll [--interval 30] | --serve [--port 8081] | --run-once <task_id>]
--poll:   loop { sync(); get_queued_tasks() → process_task sequentially; sleep interval }
--serve:  spawn poll_loop(60) background, then axum Router:
          POST /wake  {"task_id":"abc"} → process_task(task_id) → 200 {"ok":true,"task_id":"abc"}
          GET  /health → 200 "ok"
          listen 0.0.0.0:{port}
--run-once: sync(); process_task(id); sync()
```

`get_queued_tasks`: `SELECT id FROM goals WHERE status='agent_mode' AND agent_status='queued' AND deleted_at IS NULL ORDER BY updated_at`.

`process_task(task_id)` (idempotent):

```
sync()
goal = SELECT * FROM goals WHERE id=? AND deleted_at IS NULL; if None return
if goal.agent_status != "queued" return
UPDATE goals SET agent_status='running', updated_at=now() WHERE id=?
mark_dirty
resolved = resolve_provider(goal) or { set_result(Failed, "no provider…"); sync(); return }
cfg = AgentRunConfig{…}
on_event = |ev| if Progress { set_progress(json({message,detail})) }
result = if manifest.is_lua { run_lua } else { run_binary }
match result {
  Ok(Result r) => {
    // PLUGIN → MOBILE FE EXTENSIBILITY: files named mobile.json / __metadata__.json
    for f in r.files {
      if f.path=="__metadata__.json" && f.content is JSON object → merge_metadata(task_id, patch)
      if f.path=="mobile.json" && f.content is JSON → merge_metadata(task_id, {"mobile": parsed})
        // if parsed itself has {mobile,card,widgets} unwrap accordingly
    }
    set_result(Completed, json({content:r.content, files:r.files}))
  }
  Ok(Error{message,detail}) => set_result(Failed, "message: detail")
  Err(e) => set_result(Failed, "agent run error: …")
}
sync()
```

Helpers:

```sql
set_running(id): UPDATE goals SET agent_status='running', updated_at=now() WHERE id=?
set_progress(id, json): UPDATE goals SET agent_progress=? WHERE id=?  -- no updated_at bump, high freq
set_result(id, status, result?): UPDATE goals SET agent_status=?, agent_result=?, updated_at=now() WHERE id=?
merge_metadata(id, patch JSON object): SELECT metadata FROM goals WHERE id=? → parse object or {} → for k,v in patch (null deletes) → UPDATE goals SET metadata=json(obj), updated_at=now()
```

---

## 7. mobile FE hook (why `mobile.json`)

Mobile renders `goals.metadata.mobile` declaratively as a card (`GoalItem → PluginCard`). Widgets vocabulary you must emit (no code, just JSON):

```rust
Text{content:String, fg:Option<String>, bold:Option<bool>} // fg = primary|success|error|onSurface|…
List{items:Vec<String>, highlight:Option<usize>} // 1-based
Column{children:Vec<Widget>}
```

Example `mobile.json` content a plugin can return:

```json
[{"content":"Deployed to staging","fg":"success","bold":true},
 {"items":["Review PR","Run QA"],"highlight":1}]
```

or wrapped: `{"card": {"content":"hi"}}`. Backend wraps as `{"mobile": <value>}` and merges. Mobile's `parseMobileWidgets` will render it under the goal. No extra tables needed; `metadata` LWW sync does the rest.

---

## 8. testing checklist

* [ ] `cargo check` passes, no `cordanui_plugin_runtime::AgentStatus` import (use `cordanui_schema::AgentStatus`)
* [ ] `cordanui-agents --run-once <id>` on a locally queued goal writes `agent_progress` then `agent_result={"content":…,"files":…}` and, if `files` contained `mobile.json`, `SELECT metadata` contains `"mobile"`
* [ ] `POST /wake` triggers immediate run without waiting for poll interval
* [ ] `resolve_provider` picks `metadata.agent` first, falls back to first active plugin, handles pure `agent` (no models) with `model=None` and provider with per-model expansion
* [ ] Binary plugin: `echo '{"task_id":"t1","title":"T"}' | ./my-agent agent-run --task-id t1` → NDJSON ending in one `result`
* [ ] Missing API key → `{"type":"error","message":"…"}`, not stack trace

---

## 9. notes for implementer

* Do not hardcode API keys; read `api_key_env` from env or `config`.
* `stdout` is protocol-only; logs to `stderr`.
* Tolerate unknown fields in `AgentRunConfig`.
* `Database` clones must open new SQLite connection (WAL). If sharing `Arc<AgentRunner>` across axum handlers, make `Database: Send+Sync` via `unsafe impl` (each clone is independent file handle) or wrap in `Arc<Mutex<Database>>`.
* Ship as plugin: include `cordanui.toml` with `[service] autostart=false` so TUI `cord.services.start("cordanui-agents")` and `cordanui service start cordanui-agents` can supervise it.

Build this exactly and TUI (`<leader>r` picker) + mobile (`assignToAgent`) will show live progress and plugin-driven cards via Turso sync.
