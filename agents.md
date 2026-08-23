# agents.md

Canonical context for any agent (or human) working on cordanui.
Read this first.

## what is cordanui

cordanui is a goal tracker with three parts:

1. **TUI** — Rust + ratatui. The primary client. Goal list, subgoals,
   plugin manager, agent triggers.
2. **Mobile app** — minimal. Goals, task triggers, occasional chat. Talks
   to Turso directly. No plugin system.
3. **Agent backend** — a separate Rust binary. Scale-to-0 or VM. Reads a
   task from Turso, runs a provider plugin, writes results back to Turso.

The unifying idea: if a task is pending and you'd rather an agent do it,
you flip it to "agent mode" and an agent picks it up and gets it done.

## architecture

```
┌─────────────────────────────────────────────────────┐
│  TUI (Rust / ratatui)                                │
│  - goal list (base, no AI)                            │
│  - plugin manager (lazy.nvim-style: search GH,       │
│    clone, cargo build, load)                          │
│  - libSQL embedded replica (local SQLite + sync)      │
│  - plugins = Rust CLIs, spawned as subprocesses       │
│    comms = JSON over stdio                            │
└──────────────┬──────────────────┬───────────────────┘
               │ sync (libSQL)      │ HTTP (wake + point)
               ▼                   ▼
┌──────────────────────┐   ┌──────────────────────────┐
│  Turso (libSQL cloud)│   │  Agent Backend            │
│  - single source of  │   │  - scale-to-0 or VM       │
│    truth via sync    │   │  - reads task from Turso  │
│  - schema = contract │   │  - runs provider plugin   │
│                      │   │  - writes results to Turso│
└───────────┬──────────┘   └──────────────────────────┘
            │ sync
            ▼
┌──────────────────────────┐
│  Mobile (minimal)         │
│  - talks Turso directly   │
│  - goals, triggers, chat  │
│  - no plugin system       │
│  - pre-warms backend on   │
│    app open               │
└──────────────────────────┘
```

### key design principles

- **TUI and Mobile are fully independent peers.** They share Turso and
  nothing else. No client-to-client coordination. If one triggers the
  agent, the other sees the result through Turso sync.
- **Turso is the only shared state.** The HTTP call to the backend is a
  wake-and-point (just a task ID), not a data transfer. The backend reads
  the task from Turso and writes results back to Turso.
- **Plugins are Rust, built locally, act as CLIs.** The TUI spawns them as
  subprocesses, passes JSON via stdin, reads JSON from stdout. One-shot
  invocations for simple things, long-running processes with streaming
  output for the agent layer. No in-process loading, no ABI headaches.
- **Base has zero providers.** No Claude, no OpenAI, nothing. A provider
  is just another plugin. We ship a reference Claude provider plugin but
  it is not in the core.
- **Mobile talks Turso directly** — own data layer, schema is the only
  shared contract. Minimal: goals, task triggers, occasional chat.

## repo structure

```
cordanui/
├── rust/                      # Cargo workspace
│   ├── Cargo.toml             # workspace root
│   ├── config.example.toml    # sample Turso config (copy to ~/.config/cordanui/config.toml)
│   ├── schema/                # canonical SQL schema (shared contract)
│   ├── crates/
│   │   ├── tui/               # ratatui app (binary) — goal list + plugin mgr + sync
│   │   ├── schema/            # shared types + SQLite migrations (tui + backend)
│   │   ├── sync/              # libSQL embedded replica wrapper (tui + backend) — Turso sync
│   │   └── plugin-runtime/    # manifest parsing, subprocess spawning, JSON stdio protocol
│   └── plugins/
│       ├── cordanui-agents/   # optional agent backend (HTTP server, reads task from DB, runs provider)
│       └── provider-zen/      # OpenCode Zen provider plugin (GPT, Claude, Gemini, Qwen, etc.)
├── mobile/                    # mobile app (separate codebase)
└── agent_docs/                # product plan + status
```

Core crates (`schema`, `sync`, `plugin-runtime`, `tui`) are required for a
working goal tracker. Everything under `rust/plugins/` is optional — install
only what you need. Someone who just wants a goal list doesn't need
`cordanui-agents` or any provider plugins.

`schema` and `sync` are shared between `tui` and `cordanui-agents` — both
are Rust, both touch the DB. Mobile does not use these; it implements its
own thin layer against the same schema.

## data model (Turso / SQLite)

Single self-referencing table. A goal can have subgoals; nesting is
unlimited.

```sql
CREATE TABLE goals (
    id          TEXT PRIMARY KEY,        -- UUID
    title       TEXT NOT NULL,
    description TEXT,
    status      TEXT NOT NULL DEFAULT 'pending',
                -- pending | in_progress | completed | agent_mode
    parent_id   TEXT REFERENCES goals(id),  -- NULL = top-level goal
    sort_order  INTEGER DEFAULT 0,
    created_at  TEXT NOT NULL,
    updated_at  TEXT NOT NULL,
    completed_at TEXT,
    -- agent fields (only populated when status = 'agent_mode')
    agent_status   TEXT,   -- queued | running | completed | failed
    agent_result   TEXT,   -- JSON output from the agent
    agent_progress TEXT,   -- streaming progress, JSON
    -- plugin extensibility
    metadata    TEXT      -- JSON, plugins can attach arbitrary data
);
```

Sync works because libSQL embedded replicas handle the replication — each
client writes locally, Turso syncs in the background. Last-write-wins on
`updated_at` for conflict resolution (simple, good enough for a personal
goal tracker).

## agent flow

```
TUI or Mobile (either one, independently)
  1. writes status=agent_mode, agent_status=queued to Turso (local → syncs)
  2. HTTP POST to backend { task_id: "abc123" }
  3. waits for ACK (backend is awake, has the task_id)

Backend
  4. reads task from Turso (shared DB — no task payload over HTTP, just the ID)
  5. runs provider plugin subprocess
  6. streams progress → writes agent_progress to Turso
  7. writes agent_result + agent_status=completed to Turso
  8. idles / scales to 0

Either client
  9. sees progress + result via Turso sync, renders it
```

The HTTP call is a wake-and-point, not a data transfer. The task data
lives in Turso, the backend reads it from there, both clients read
results from there. No client-to-client coordination needed.

## plugin system

Plugins are Rust crates that build to a binary. The TUI's plugin manager
fetches them from GitHub, builds them locally with cargo, and runs them as
subprocesses.

### manifest (`cordanui.toml` in plugin repo root)

```toml
[plugin]
name = "provider-claude"
version = "0.1.0"
description = "Anthropic Claude provider"

[capabilities]
provider = true        # can act as an LLM provider
# other capability types: tool, agent, theme, command

[provider]
models = ["claude-sonnet-4-5", "claude-opus-4-1"]
api_key_env = "ANTHROPIC_API_KEY"

[build]
cmd = "cargo build --release"
bin = "target/release/provider-claude"
```

### protocol

The TUI spawns the plugin binary with a subcommand and JSON on stdin,
reads JSON from stdout:

```
# one-shot: provider complete
$ provider-claude complete --model claude-sonnet-4-5 < input.json
{"content": "...", "usage": {...}}

# streaming: agent run (long process, streams JSON lines to stdout)
$ provider-claude agent-run --task-id abc123 < config.json
{"type":"progress","message":"Searching the web..."}
{"type":"progress","message":"Consolidating data..."}
{"type":"result","content":"...","files":[...]}
```

### plugin manager (TUI)

- `:plugins` — browse installed plugins
- `:plugins search <query>` — search GitHub for the `cordanui-plugin` topic
- `:plugins install <repo>` — clone, build, add to lockfile
- `:plugins update` — git pull + rebuild all
- `:plugins enable/disable <name>`

Lockfile at `~/.config/cordanui/plugins.lock.toml` (mirrors lazy.nvim's
lockfile).

## build order

1. **TUI base** — goal list, add/edit/complete goals + subgoals, local
   SQLite storage, ratatui UI. No sync, no plugins yet. Just a working goal
   tracker.
2. **Turso sync** — swap local SQLite for libSQL embedded replica, sync to
   Turso.
3. **Plugin runtime** — manifest parsing, subprocess spawning, JSON stdio
   protocol.
4. **Plugin manager** — GitHub search, clone, build, lockfile.
5. **Reference provider plugin** — provider-claude, proves the plugin
   system works end-to-end.
6. **Agent backend** — HTTP server, reads task from Turso, runs provider
   plugin, streams results.
7. **Mobile app** — thin Turso client, goal CRUD, agent trigger, pre-warm.
8. **Theme system** — plugin-based theming.

Detailed status lives in `agent_docs/product_plan.md`.

## conventions

- Rust workspace at `rust/` with a single `Cargo.toml` and members in `crates/`.
- Commits: conventional, lowercase, e.g. `feat(tui): add goal creation`,
  `fix(sync): handle conflict on parent_id`.
- Docs that agents should read live in `agent_docs/`.
