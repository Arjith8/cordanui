# product_plan.md

Living document. Updated as we build.

## status

- [~] **1. TUI base** — goal list, add/edit/complete goals + subgoals,
      ratatui UI. **Scaffold done, `cargo check` passes.** Now backed by
      libSQL (via `cordanui-sync`) instead of rusqlite. Needs runtime testing.
- [~] **2. Turso sync** — **Done.** `rust/crates/sync` wraps libSQL embedded
      replicas in a synchronous API. TUI and agent backend both use it.
      Local-only when no config; embedded replica with background sync when
      `~/.config/cordanui/config.toml` has a `[turso]` section. 3 tests passing.
- [ ] **3. Plugin runtime** — manifest parsing, subprocess spawning, JSON
      stdio protocol. **Done** — `rust/crates/plugin-runtime`, 9 tests passing.
- [ ] **4. Plugin manager** — GitHub search, clone, build, lockfile.
- [~] **5. Reference provider plugin** — **Done.** `rust/plugins/provider-zen/`
      uses OpenCode Zen gateway (OpenAI-compatible `/chat/completions`).
      Compiles clean. Not yet tested against live API (needs OPENCODE_API_KEY).
      Replaces the original `provider-claude` plan — Zen gives access to
      Claude, GPT, Gemini, Qwen, and more via a single key.
- [~] **6. Agent backend** — HTTP server, reads task from DB, runs
      provider plugin, streams results. **Scaffold done, `cargo check`
      passes.** Now lives at `rust/plugins/cordanui-agents/` — it's an optional
      plugin, not a core crate. Uses libSQL via `cordanui-sync` (same as TUI).
- [~] **7. Mobile app** — thin Turso client, goal CRUD, agent trigger,
      pre-warm. **Local-first scaffold done** (goal tree CRUD, local
      SQLite). Turso sync, agent trigger, chat pending.
- [ ] **8. Theme system** — plugin-based theming.

## phase 1 — TUI base

Goal: a working local goal tracker. No network, no AI, no plugins.

### scope

- Cargo workspace at `rust/` with members: `rust/crates/tui`, `rust/crates/schema`.
- `rust/crates/schema`: the `goals` table migration + Rust types
  (`Goal`, `GoalStatus`, `CreateGoal`, `UpdateGoal`).
- `rust/crates/tui`: ratatui app with:
  - goal list view (flat list of top-level goals)
  - expand a goal to see its subgoals (recursive nesting)
  - add goal (prompt for title)
  - add subgoal (under a selected goal)
  - edit goal title/description
  - complete a goal (marks `completed`, sets `completed_at`)
  - delete a goal (cascades to subgoals)
  - reorder goals within the same parent (`sort_order`)
  - local SQLite storage via `rusqlite` (no sync yet)
  - persistence at `~/.local/share/cordanui/cordanui.db`
  - keybindings help overlay
- Status bar showing: total goals, completed count, pending count.

### out of scope (phase 1)

- Turso / libSQL sync
- plugins
- agent mode
- mobile

### keybindings (draft)

```
 j / k        move selection
 enter        expand/collapse goal (toggles subgoals)
 a            add goal (top-level)
 A            add subgoal under selected goal
 e            edit selected goal
 space        toggle complete
 d            delete selected goal
 J / K        move goal up/down (reorder)
 ?            help overlay
 q            quit
```

### decisions still open

- rusqlite vs sqlx for local SQLite? (leaning rusqlite — synchronous, simple,
  embedded; no async runtime needed for local-only phase)
- how to handle description editing in a TUI — inline text field vs a
  modal editor pane?

## phase 2 — Turso sync

Swap local SQLite for libSQL embedded replica. Same schema, same queries,
just backed by libSQL local + background sync to Turso.

### scope

- `crates/sync`: libSQL embedded replica wrapper.
- TUI uses `crates/sync` instead of raw rusqlite.
- Config at `~/.config/cordanui/config.toml` for Turso URL + auth token.
- Conflict resolution: last-write-wins on `updated_at`.
- Offline-first: writes go local, sync happens when online.

### decisions still open

- libSQL Rust crate maturity — `libsql` vs going through the C bindings?
- how to surface sync status in the TUI (synced / syncing / offline / error)?

## phase 3 — plugin runtime

### scope

- `crates/plugin-runtime`:
  - parse `cordanui.toml` manifests
  - spawn plugin binaries as subprocesses
  - write JSON to stdin, read JSON lines from stdout
  - one-shot invocation mode
  - streaming mode (long-running, line-delimited JSON events)
- plugin manifest schema + validation
- the JSON stdio protocol types (request/response/event)

### decisions still open

- exact JSON protocol shape (one request object in, one-or-many events out)
- how plugins declare version compatibility with the TUI host

## phase 4 — plugin manager

### scope

- TUI commands: `:plugins`, `:plugins search`, `:plugins install`,
  `:plugins update`, `:plugins enable`, `:plugins disable`
- GitHub search for the `cordanui-plugin` topic
- clone + `cargo build --release` locally
- lockfile at `~/.config/cordanui/plugins.lock.toml`
- plugin storage at `~/.local/share/cordanui/plugins/<name>/`

## phase 5 — reference provider plugin

### scope

- `plugins/provider-claude/` — separate cargo project, builds to a binary
- implements the provider protocol: `complete` (one-shot) and
  `agent-run` (streaming)
- proves the plugin system end-to-end with a real LLM

## phase 6 — agent backend

### scope

- `crates/agent-backend`: HTTP server (axum)
- `POST /run { task_id }` — wake endpoint
- reads task from Turso by ID
- spawns the configured provider plugin
- streams `agent_progress` writes to Turso
- writes final `agent_result` + `agent_status=completed`
- scale-to-0 friendly (stateless, idempotent on task_id)
- auth: shared secret in header, configured via env

### decisions still open

- deploy target: Fly Machines / Modal / a plain VM? (user's choice, but
  the binary should be deploy-agnostic)
- how to handle long-running agent tasks vs serverless timeouts

## phase 7 — mobile app

> **Status:** local-first scaffold complete (phase 1 of mobile). See
> `mobile/` and `mobile/README.md`. Stack: React Native + Expo, pnpm.
> Local SQLite via expo-sqlite, schema mirrors `schema/schema.sql`.
> Remaining: Turso sync, agent trigger + wake, chat.

### scope

- separate codebase under `mobile/`
- thin client: goal CRUD, mark agent_mode, view agent progress/result
- talks Turso directly (libSQL client or HTTP API)
- pre-warm backend on app open (fire-and-forget wake ping)
- occasional chat (messages stored in Turso, agent responds via backend)

### decisions (resolved)

- ~~Flutter / React Native / native?~~ → **React Native (Expo)**, pnpm
- libSQL client availability on mobile vs HTTP API to Turso — still open
  (investigate at sync phase)

## phase 8 — theme system

### scope

- themes as plugins (capability: `theme`)
- theme plugin outputs a TOML/JSON color map on invocation
- TUI loads active theme, applies to ratatui styles

## notes

- The TUI is the primary client and the testbed. Everything new lands
  there first.
- Mobile is deliberately minimal — if a feature isn't on the "goals,
  triggers, chat" list, it doesn't go in mobile.
- The agent backend has no intelligence of its own — it's an executor.
  The intelligence lives in provider plugins.
