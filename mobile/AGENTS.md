# AGENTS.md — Writing cordanui Plugins

Instructions for building a cordanui plugin from scratch. This document is
**self-contained**: you do not need access to the cordanui source tree —
everything on the wire is specified here.

A cordanui plugin is one of two things:

1. **A standalone executable** that the host spawns as a subprocess and
   talks to over **JSON on stdin/stdout** (the classic runtime). No shared
   libraries, no linking. Any language works; examples are in Rust.
2. **A Lua script** (`main.lua`) executed in-process by the host's embedded
   Lua 5.4 runtime (`runtime = "lua"` in the manifest). No build step at
   all: installing the plugin = clone + activate. See section 10.

Building an LLM **provider** (OpenAI/Anthropic-style gateways)? Read
[`AGENTS-PROVIDERS.md`](./AGENTS-PROVIDERS.md) after this — it adds the
upstream API specs and validation rules.

---

## 1. Repository layout

Your repo must contain a `cordanui.toml` manifest at its root:

```
my-plugin/
├── cordanui.toml        # manifest (required, must be at repo root)
├── README.md
└── src/
    └── main.rs          # or whatever your build produces
```

Install location at runtime:

```
~/.local/share/cordanui/plugins/<plugin-name>/
```

The host runs `<install-dir>/<bin>` (see `[build].bin` below) as a
subprocess with one subcommand per invocation.

---

## 2. The manifest: `cordanui.toml`

```toml
[plugin]
name = "my-plugin"              # required; must match binary name unless [build].bin overrides
version = "0.1.0"               # required, semver string
description = "What this does"  # optional

[capabilities]                  # declare ONLY what you fully implement
provider = false                # LLM provider (complete / agent-run)
tool = false                    # RESERVED — no wire protocol defined yet, do not use
agent = false                   # autonomous agent backend (uses agent-run, section 4.2)
theme = false                   # theme pack (section 5)
command = false                 # RESERVED — no wire protocol defined yet, do not use

# Required when capabilities.provider = true or capabilities.agent = true:
[provider]
models = ["model-id-1", "model-id-2"]
api_key_env = "MY_PLUGIN_API_KEY"

# Optional build config:
[build]
cmd = "cargo build --release"       # default
bin = "target/release/my-plugin"    # default: target/release/<plugin.name>
```

Rules:

- `[plugin].name` and `version` are the only strictly required fields.
- Never set a capability flag for something you don't implement — the host
  will invoke it.
- `api_key_env` names an environment variable. **Never hardcode API keys,
  never persist them, never log them.** Read them from env only.
- `tool` and `command` capabilities exist in the schema but their protocols
  are not finalized. Do not ship plugins that claim them.

---

## 3. CLI contract

| Subcommand | Capability | stdin | stdout |
|---|---|---|---|
| `complete --model <id>` | `provider` | one `CompleteRequest` JSON object | one `CompleteResponse` JSON object |
| `agent-run --task-id <id>` | `provider` / `agent` | one `AgentRunConfig` JSON object | newline-delimited `AgentEvent` JSON objects |

Exit code 0 = success. Non-zero = failure *before* an `error` event could
be produced (e.g. bad args). Runtime failures during `agent-run` must be
reported as an `error` **event**, not just an exit code.

**stdout is protocol-only.** All logging/diagnostics go to stderr. A single
debug print on stdout corrupts the stream and breaks the host.

---

## 4. Protocols (exact wire formats)

### 4.1 One-shot completion (`complete --model <id>`)

stdin — one JSON object:

```json
{
  "model": "model-id-1",
  "prompt": "Write a haiku about goals",
  "system": null,
  "max_tokens": null,
  "temperature": null
}
```

| Field | Type | Notes |
|---|---|---|
| `model` | string | present both here and as the CLI flag |
| `prompt` | string | the user instruction |
| `system` | string \| null | optional system prompt; may be absent entirely |
| `max_tokens` | integer \| null | optional |
| `temperature` | number \| null | optional |

stdout — one JSON object:

```json
{ "content": "…generated text…", "usage": null }
```

`usage`, when present:

```json
{ "prompt_tokens": 12, "completion_tokens": 34, "total_tokens": 46 }
```

(`total_tokens` optional inside `usage`.)

### 4.2 Streaming agent run (`agent-run --task-id <id>`)

stdin — one JSON object:

```json
{
  "task_id": "abc123",
  "title": "Plan a product launch",
  "description": "extra context",
  "model": null,
  "config": null
}
```

All fields except `task_id` and `title` may be absent. `config` is an
arbitrary JSON value if the host has plugin-specific settings; tolerate it
being any shape or missing.

stdout — **newline-delimited JSON events**, one event per line:

```json
{"type":"progress","message":"Starting...","detail":"model: x"}
{"type":"progress","message":"Drafting plan","detail":null}
{"type":"result","content":"final output","files":[],"usage":null}
```

Event grammar (the `"type"` field is the discriminator):

- `progress` — `{ "type":"progress", "message": string, "detail": string|null }`
  Emit freely while working. First one should come immediately after spawn.
- `result` — `{ "type":"result", "content": string, "files": [file], "usage": usage|null }`
  Terminal. Exactly one per run. `file` = `{ "path": string, "content": string|null }`.
- `error` — `{ "type":"error", "message": string, "detail": string|null }`
  Terminal alternative to `result`.

After emitting `result` or `error`: exit promptly, emit nothing further.

### 4.3 Copy-paste protocol structs (Rust + serde)

These mirror the host exactly. Field names and casing are wire format — do
not rename anything.

```rust
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompleteRequest {
    pub model: String,
    pub prompt: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompleteResponse {
    pub content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Usage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentRunConfig {
    pub task_id: String,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum AgentEvent {
    #[serde(rename = "progress")]
    Progress { message: String, detail: Option<String> },
    #[serde(rename = "result")]
    Result(AgentResult),
    #[serde(rename = "error")]
    Error { message: String, detail: Option<String> },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentResult {
    pub content: String,
    #[serde(default)]
    pub files: Vec<AgentFile>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentFile {
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
}
```

---

## 5. Theme packs (`capabilities.theme = true`)

Theme plugins carry no subprocess protocol — the artifact is data that the
host imports into its shared `themes` database table. Ship one JSON file
per theme at the repo root, named `<theme-id>.json` (e.g. `my-theme.json`):

```json
{
  "id": "my-theme",
  "name": "My Theme",
  "colors": {
    "background": "#0f172a",
    "onBackground": "#f9fafb",
    "surface": "#1e293b",
    "onSurface": "#f9fafb",
    "surfaceVariant": "#1f2937",
    "onSurfaceVariant": "#9ca3af",
    "primary": "#3b82f6",
    "onPrimary": "#ffffff",
    "secondary": "#38bdf8",
    "onSecondary": "#082f49",
    "tertiary": "#a855f7",
    "onTertiary": "#ffffff",
    "success": "#22c55e",
    "onSuccess": "#052e16",
    "error": "#ef4444",
    "onError": "#ffffff",
    "outline": "#334155",
    "outlineVariant": "#6b7280"
  }
}
```

Contract:

- **Color keys are style variables** using the Compose / Material 3 role
  vocabulary (`background`, `primary`, `onSurface`, ...). Anyone who has
  used Compose, Flutter, or CSS design tokens already knows them. There
  are no widget-specific tokens like `statusWip`: statuses use standard
  roles (pending → `onSurfaceVariant`, in-progress → `primary`,
  completed → `success`, agent mode → `tertiary`).
- **Legacy keys still load**: themes authored against the old mobile token
  names (`bg`, `accent`, `statusDone`, ...) are aliased to their roles on
  read (`bg` → `background`, `accent` → `primary`, `statusDone` →
  `success`, ...) by both hosts. New themes should use canonical role
  names only — the mobile client has migrated too; duplicate legacy keys
  are harmless but unnecessary.
- Any subset of keys is valid; missing or unknown tokens fall back to the
  builtin dark palette.
- On install the host inserts a row into the `themes` table:
  `(id, name, source, colors_json)` where `colors_json` is the `colors`
  object serialized verbatim and `source` is set to your plugin's
  **GitHub repo URL** (the host fills this in — do not include it in the
  JSON). Builtins use `source = 'builtin'`.
- Values accept `#rgb`, `#rrggbb`, `rgb(r,g,b)` and `rgba(r,g,b,a)`
  (alpha is dropped — terminals have no alpha channel).
- `id` must be unique across the user's installed themes; re-installing
  with the same id updates the existing row.
- Resolution at runtime: the host reads `settings.theme_mode`; if
  `'explicit'` it uses `settings.selected_theme_id`, otherwise
  (`'system'`) terminals resolve to builtin dark. A theme is only visible
  once the user selects it.
- Themes are layer two of four — see section 11 for how user overrides
  (`cord.g`) and session tweaks (`cord["local"]`) stack on top of a theme.

---

## 6. Implementation checklist

1. Repo root contains `cordanui.toml`; default build output path resolves
   to `target/release/<plugin.name>` (or set `[build].bin` explicitly).
2. Parse CLI args, then read **all of stdin** before doing anything else.
3. Validate credentials/env first. On runtime failure during `agent-run`,
   emit an `error` event rather than only exiting non-zero.
4. Emit a first `progress` event immediately after starting work — hosts
   treat startup silence as suspicious.
5. **Flush stdout after every event line.** Buffered stdout hides progress
   from the UI.
6. Tolerate unknown/missing optional fields — the host may add fields.
7. Test protocol round-trips against section 4's exact JSON shapes.

## 7. Testing checklist (run all before shipping)

- [ ] `cordanui.toml` parses as valid TOML; `name` matches binary path
- [ ] `echo '{"model":"m","prompt":"hi"}' | ./my-plugin complete --model m`
      → single valid JSON object on stdout, nothing else
- [ ] `echo '{"task_id":"t1","title":"T"}' | ./my-plugin agent-run --task-id t1`
      → NDJSON stream ending in exactly one terminal event
- [ ] Missing API key → clean `error` event (not a stack trace)
- [ ] Invalid JSON on stdin → non-zero exit with useful message on stderr
- [ ] stdout carries zero log lines under normal operation
- [ ] Theme files validate as JSON; all colors are `#rrggbb`; keys use
      the exact camelCase token names from section 5

## 8. Distribution notes

- The cordanui plugin manager searches GitHub for repos; name the repo the
  same as the plugin and keep `cordanui.toml` at the repo root so it can be
  detected and validated.
- Builds run `[build].cmd` automatically inside the cloned repo dir after
  install/update — **skipped for data-only plugins** (theme packs) and
  **Lua plugins** (`runtime = "lua"`, which have no build step at all).
  Keep builds hermetic; pin dependency versions. The binary must end up at
  `target/release/<plugin.name>` (or `[build].bin`) or the install fails.

---

## 9. Declarative settings (`[[field]]`) — the fallback form

This is the **fallback** configuration surface, used when the user
presses `c` and your plugin does NOT define `plugin.configure`
(section 10.1). It exists for binary providers and simple plugins: the
host renders a form from your manifest, stores the answers, and hands
them back on every invocation. You cannot add custom logic here — if you
need that, define `plugin.configure` and build the page yourself from
`cord.ui.*` + `cord.config` (sections 12–13, and `cord.config` below).

Plugins using this form never touch the database and never prompt at
runtime.

```toml
[[field]]
key = "api_key"          # required; unique within the manifest
label = "API Key"        # shown in the form
type = "secret"          # text | secret | number | bool | select
required = true

[[field]]
key = "base_url"
type = "text"
default = "https://example.com/v1"

[[field]]
key = "model"
type = "select"
options = ["glm-5.2", "kimi-k3"]
default = "glm-5.2"

[[field]]
key = "stream"
type = "bool"
default = "true"
```

Contract:

- **Authoring rules**: keys must be unique and non-empty; `select` requires
  non-empty `options` and its `default` must be one of them; `bool`
  defaults are `"true"`/`"false"` strings; `number` defaults must parse.
  The host validates all of this and refuses to open a broken form.
- **Storage**: values live in the host's shared key-value `settings` table
  under `<plugin.name>.<field.key>`. You cannot read or write outside your
  namespace. Secrets render masked in the UI (stored as plain text today;
  keyring migration is planned — don't rely on the storage being encrypted).
- **How you receive values**: on every invocation the host collects your
  stored values into a JSON object and passes it as the `config` field of
  `CompleteRequest` / `AgentRunConfig`:

  ```json
  { "task_id": "…", "title": "…", "config": {
      "api_key": "sk-…", "base_url": "https://example.com/v1",
      "model": "glm-5.2", "stream": "true" } }
  ```

  All values arrive as **strings** regardless of field type. Absent
  optional fields may be missing from `config` entirely — tolerate it and
  fall back to sensible defaults.
- **User flow**: plugin manager → select plugin → `c` (Configure). There is
  no runtime prompting: if a required value is unset, fail fast with an
  `error` event telling the user to run *Configure* on your plugin.

---

## 10. Lua plugins (`runtime = "lua"`)

The embedded Lua runtime runs your plugin **in-process** — no binary, no
build step, no subprocess protocol. Users install your plugin by cloning
the repo; there is no cargo (or any toolchain) requirement.

### 10.1 Manifest

```toml
# NOTE: `runtime` is a manifest-ROOT key. It must appear BEFORE any
# [section] header, or TOML silently attaches it to [plugin] and the
# host treats your plugin as a binary that doesn't exist.
runtime = "lua"

[plugin]
name = "provider-myzen"
version = "0.1.0"
description = "My provider, in Lua"

[capabilities]
provider = true

[provider]
models = ["model-a", "model-b"]

[[field]]
key = "api_key"
label = "API Key"
type = "secret"
required = true
```

No `[build]` section. The host skips the build step for `runtime = "lua"`
plugins automatically. Your repo root must contain a `main.lua`.

**Common mistake**: writing `runtime = "lua"` under `[plugin]`. The host
detects this and refuses with an explicit error — but don't rely on it;
put the key at the top of the file.

### 10.2 Entry points

`main.lua` must define a global `plugin` table:

```lua
plugin = {}

-- One-shot completion (mirrors section 4.1's CompleteRequest/Response).
-- request: { model, prompt, system?, max_tokens?, temperature?, config? }
function plugin.complete(request)
  return {
    content = "generated text",
    usage = { prompt_tokens = 1, completion_tokens = 2 },  -- optional
  }
end

-- Streaming agent run (mirrors section 4.2). Call emit(event) to stream;
-- exactly one terminal event required.
-- config: { task_id, title, description?, model?, config? }
function plugin.agent_run(config, emit)
  emit({ type = "progress", message = "working" })
  emit({ type = "error", message = "no api key" })  -- or result:
  -- emit({ type = "result", content = "done", files = cordanui.array({}) })
end
```

Define only what you need (at least one of the two). Errors raised by
`plugin.complete` surface to the user as clean failures; in `agent_run`
prefer emitting an `error` event over raising.

### 10.3 The `cordanui` API

| Binding | Notes |
|---|---|
| `cordanui.plugin.name` | your plugin's name from the manifest |
| `cordanui.config` | settings collected from your `[[field]]` form (section 9), keys stripped of the plugin namespace; empty table if none |
| `cordanui.log.info/warn/error(msg)` | goes to the host log, never to the wire |
| `cordanui.json.encode(value)` / `.decode(str)` | JSON bridge |
| `cordanui.http.request{url=, method=, headers={}, body=}` | HTTP via the host (reqwest); awaitable; returns `{ status, body }`; 120s timeout |
| `cordanui.array(tbl)` | marks a table as a JSON **array** — REQUIRED for empty arrays like `files = {}`, since Lua cannot distinguish `{}` (empty array) from `{}` (empty map) |
| `cordanui.plugin_dir` | absolute path of your installed repo (for reading data files) |

`require` works for sibling files inside your repo (`require("lib")` loads
`<repo>/lib.lua`).

Settings arrive on `cordanui.config` directly (not nested under `config`
like the subprocess protocol) and are strings, same as section 9.

### 10.4 Reference implementation

[`provider-zen`](./provider-zen/) is a complete Lua provider: manifest
with settings form, OpenAI-compatible chat completions via
`cordanui.http.request`, env-var fallback for the API key, and both entry
points. Read it before writing your own.

### 10.1a Self-owned configuration (`plugin.configure`)

When the user presses the configure key, a Lua plugin that defines
`plugin.configure` owns the entire page. The host only facilitates the
call — you render whatever you want with panels and dialogs (sections
12–13) and persist values yourself with `cord.config`:

```lua
function plugin.configure()
  local current = cord.config.get("variant", "moon")
  local idx = cord.ui.pick{ title = "Variant", items = { "main", "moon", "dawn" } }
  if not idx then return "cancelled" end
  cord.config.set("variant", ({ "main", "moon", "dawn" })[idx])
  -- apply immediately, e.g. cord["local"].style.* for preview
  return "variant = " .. ({ "main", "moon", "dawn" })[idx]
end
```

- The call runs to completion on a worker thread; dialogs/panels it opens
  are answered through the normal event loop.
- A returned string becomes the host status message; errors show as
  `✖ <message>`.
- `cord.config.get(key, default?)` / `cord.config.set(key, value)` persist
  under your plugin's namespace in the shared `settings` table — the same
  place the fallback form reads and writes, and synced via Turso.
- Plugins without `plugin.configure` get the declarative `[[field]]`
  fallback form (section 9).

### 10.5 Current limitations (spike status)

- The Lua runtime currently covers providers (complete + agent-run) and
  anything expressible over HTTP + JSON. UI rendering from Lua plugins is
  planned (`cordanui.ui`) but not implemented — declarative widgets only,
  plugins will never draw to the terminal directly.
- Sandboxing is not enforced yet: scripts run with full Lua stdlib
  (including `os`/`io`). Treat installing a Lua plugin like running its
  code locally — same trust level as the binary runtime today.
- Binary (`subprocess`) plugins remain fully supported; neither runtime is
  deprecated. Long-running isolated workloads may still prefer binaries.

---

## 11. Live styling (`cord.g` / `cord["local"]`)

Lua plugins can restyle the UI at runtime through the `cord` global.
Colors are addressed by the same style variables themes use (section 5).

```lua
-- Global: persisted in the settings table, syncs to every client via Turso
cord.g.style.primary("#ff8800")
cord.g.style.background("rgb(18, 18, 18)")

-- Session: applied to this client only, gone when it exits.
-- ("local" is a Lua keyword, so bracket indexing is required.)
cord["local"].style.primary("#ff8800")

-- Clearing
cord.g.style.reset("primary")   -- remove one override
cord.g.style.resetAll()         -- remove every override in this scope

-- Reading: effective override for a variable, "" if none
local current = cord.style.get("primary")
```

Contract:

- **Any variable name works.** The 18 core roles are always defined;
  beyond that a plugin can introduce its own names (e.g.
  `cord.g.style.myPluginGlow("#f0f")`) and read them back anywhere via
  `cord.style.get`. Unknown names resolve to `onBackground`, so a theme
  that doesn't know about your variable degrades gracefully instead of
  breaking — but you should ship a sensible default yourself, either by
  setting it on activation or by handling the fallback in your UI code.
- **Precedence** (later wins): builtin palette → active theme → `cord.g`
  overrides (DB) → `cord["local"]` overrides (session).
- **Accepted colors**: `#rgb`, `#rrggbb`, `rgb(r,g,b)`, `rgba(r,g,b,a)`
  (alpha dropped). Anything else raises an error naming the valid forms.
- **Persistence**: `.g` writes land in the shared `settings` table under
  `style.<var>` and sync to all clients through Turso; `.local` never
  touches storage.
- Changes apply live — the host re-resolves the palette within a frame or
  two; plugins do not need to trigger a redraw.

---

## 12. Dialogs (`cord.ui.*`) — plugins that need user input at runtime

The settings form (section 9) is static and host-initiated. When a plugin
needs an answer *now* — mid agent-run, on a command — it awaits one of the
host-rendered modals. Plugins never draw to the terminal; the host owns
the widgets, layout, and keystrokes. All three calls are awaitable: the
host's event loop keeps running (and other plugins keep working) while
one waits.

```lua
-- Single-line text. Returns the string, or nil if cancelled.
local name = cord.ui.input{ title = "Goal name", placeholder = "what?", prefill = nil }

-- Yes/no. Returns true / false.
local ok = cord.ui.confirm{ title = "Delete", message = "remove this goal?" }

-- Pick one of N. Returns the 1-based index, or nil if cancelled.
local items = { "grok-code", "claude-sonnet-4-5" }
local idx = cord.ui.pick{ title = "Model", items = items }
if idx then use_model(items[idx]) end

-- Toggle any of N. Returns an array of 1-based indices (possibly empty),
-- or nil if cancelled.
local picked = cord.ui.multiselect{ title = "Tags", items = tags, selected = { 2 } }

-- Multi-line text. Enter inserts a newline; Ctrl+Enter submits.
-- Returns the string, or nil if cancelled.
local body = cord.ui.text{ title = "Description", prefill = goal.description }

-- Non-blocking status message. level: "info" (default) | "warn" | "error".
cord.ui.notify("sync complete")
cord.ui.notify{ message = "quota low", level = "warn" }
```

Contract:

- **Keys**: `title` is always optional. `input` takes `placeholder` and
  `prefill`; `confirm` takes `message`; `pick` requires a non-empty
  `items` array of strings; `multiselect` additionally takes an optional
  `selected` array of 1-based indices; `text` is like `input` but
  multi-line.
- **Cancel semantics**: Esc resolves to `nil` for input/pick/multiselect/
  text and `false` for confirm — never an error. An empty input submitted
  with Enter also resolves to `nil`; a multiselect submitted with nothing
  selected returns an empty table (distinct from nil).
- **Refusals are errors**: if the host cannot show the dialog right now
  (another dialog is open), the call raises a Lua error carrying the
  reason. Wrap in `pcall` when a refusal is acceptable:
  `local ok, val = pcall(cord.ui.input, { title = "..." })`.
- **Concurrency**: multiple plugins can wait simultaneously; each gets
  its own dialog in turn. Only one dialog is on screen at a time.
- **Timeouts**: there are none — a dialog stays open until answered or
  cancelled. Don't block an agent run behind a question unless you mean it.

---

## 13. Panels (`cord.ui.show_panel`) — plugin-owned screens

For long-lived UI (dashboards, live views), a panel is a persistent
window the host renders from your widget tree every frame. You never
touch the terminal: you return declarative widgets and receive key names.

```lua
local sel = 1
local items = { "alpha", "beta", "gamma" }

cord.ui.show_panel{
  title = "My dashboard",
  draw = function()
    return {
      { content = "selected " .. sel, fg = "primary", bold = true },
      { items = items, highlight = sel },
      { children = {                                   -- vertical stack
          { content = "footer" },
      } },
    }
  end,
  on_key = function(key)
    if key == "down" or key == "j" then
      sel = math.min(sel + 1, #items); return true     -- handled → redraw
    elseif key == "up" or key == "k" then
      sel = math.max(sel - 1, 1); return true
    elseif key == "esc" then
      cord.ui.close_panel(); return true               -- close it yourself
    end
    return false                                       -- pass through to host
  end,
}
```

Contract:

- **Widget shapes** (detected by fields):
  - `{ content = "..", fg = "role"?, bold = bool? }` — a text line; `fg`
    is any style variable name (section 11 vocabulary).
  - `{ items = {".."}, highlight = n? }` — a list; `highlight` is a
    1-based index.
  - `{ children = {widget, ...} }` — vertical column.
  - The `draw` return may be a single widget or an array of widgets.
- **Key names**: single characters arrive as themselves (`"j"`, `"+"`),
  named keys as `"up"`, `"down"`, `"enter"`, `"esc"`, `"tab"`,
  `"backspace"`, `"pageup"`, etc.; control chords as `"ctrl+x"`.
  Returning `true` means handled (host redraws); `false`/`nil` passes
  the key through. **Unhandled Esc closes the panel.**
- **Lifecycle**: `show_panel` returns immediately. One panel at a time —
  a second `show_panel` replaces the first. Close via
  `cord.ui.close_panel()` or unhandled Esc. Your Lua state (upvalues)
  persists across frames, so plain locals are your view model.

---

## 14. Commands (`plugin.commands`) — user-invocable functions

Register functions users can run from the TUI's command line
(`<leader>;` — type to filter, Enter to run):

```lua
local M = {}

function M.select()
  local flavors = { "rose-pine", "rose-pine-moon", "rose-pine-dawn" }
  local idx = cord.ui.pick{ title = "Flavor", items = flavors }
  if not idx then return "cancelled" end          -- shown on the status line
  cord.g.style.background("#191724")
  return "switched to " .. flavors[idx]
end

plugin.commands = {
  ["rose-pine.select"] = { run = M.select, desc = "Pick a flavor" },
}
```

Contract:

- **Naming**: prefix with your plugin name (`"<plugin>.<action>"`) — all
  commands across plugins share one namespace.
- **Entry**: `{ run = function, desc = string }`. Entries without `run`
  are skipped. `desc` shows in the filterable command list.
- **Anything goes inside `run`**: dialogs (`cord.ui.*`), panels, style
  writes — your Lua state is long-lived (loaded once at startup and kept),
  so upvalues persist between invocations.
- **Return value**: a string is shown on the host status line; anything
  else is ignored. Errors surface as `✖ <message>` on the status line.
- **Capabilities**: set `command = true` under `[capabilities]` if your
  plugin exists primarily to provide commands.
