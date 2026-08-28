# cordanui-chat — self-contained plugin spec

> Give this to an agent with **no repo access**. It is the only context it needs.
> The chat plugin is **separate from** `cordanui-agents` backend. It works standalone via direct LLM HTTP; if `cordanui-agents` is active it may optionally delegate via `cord.services`.

---

## 0. what is cordanui

Goal tracker with three peers; `Turso (libSQL cloud)` is only shared state.

```
TUI (Rust/ratatui) ──Hrana POST /v2/pipeline──┐
                                              ├─ Turso
Mobile (Expo) ──Hrana─────────────────────────┘
Agent backend (optional, Rust) ──Hrana + POST /wake ── Turso
Chat plugin (this, Lua) ── uses TUI plugin runtime, no backend required
```

* TUI is the primary client. Mobile is thin: goals, triggers, occasional view. No plugin runtime on mobile.
* Plugins are either **binary** (Rust CLI, JSON stdio) or **Lua** (`runtime="lua"`, `main.lua` in-process via mlua 5.4). This spec is for a **Lua** chat plugin.
* If you build an LLM provider, read `AGENTS-PROVIDERS.md` after this. This chat spec is consumer of a provider, not a provider itself.

## 1. what you must build

A Lua plugin `cordanui-chat` that provides a **chat interface** inside the TUI:

* Entry: `plugin.commands["cordanui-chat.open"]` → `<leader>;` filterable list → opens a persistent panel.
* Panel: message list + input draft + typing indicator, `Enter` sends, `Esc` closes (panel replaced if another opens).
* LLM: calls an OpenAI-compatible `/chat/completions` (or Zen gateway) via `cordanui.http.request`. Reuse any model from `cordanui.config`.
* Persistence: history survives restarts and syncs to other clients via `settings` / `goals.metadata` (Turso LWW). No new tables required for MVP.
* Backend awareness: if `cordanui-agents` service is running (`cord.services.is_running`), you may delegate via `cord.services.request`; otherwise direct HTTP. Both must work.

Repo layout you must ship:

```
cordanui-chat/
├── cordanui.toml   # manifest at repo root (required)
├── main.lua        # entry point (required for runtime="lua")
└── README.md
```

Install location at runtime (host handles): `~/.local/share/cordanui/plugins/cordanui-chat/`.

## 2. manifest `cordanui.toml`

```toml
# NOTE: runtime is a manifest-ROOT key, before any [section]. If under [plugin], TOML silently attaches to [plugin] and host rejects.
runtime = "lua"

[plugin]
name = "cordanui-chat"
version = "0.1.0"
description = "Chat panel for cordanui"

[capabilities]
# Claim ONLY what you implement. Host will invoke it.
provider = false  # set true if you also want to be selectable in agent picker
command = true    # you provide commands
agent = false
theme = false

# Optional provider block only if provider=true
# [provider]
# models = ["grok-code","gpt-5","claude-sonnet-4-5"]
# api_key_env = "OPENCODE_API_KEY"

[[field]]
key = "api_key"
label = "API Key"
type = "secret"
required = true

[[field]]
key = "base_url"
label = "Base URL"
type = "text"
default = "https://opencode.ai/zen/v1"

[[field]]
key = "default_model"
label = "Default model"
type = "select"
options = ["grok-code","gpt-5","claude-sonnet-4-5","gemini-3-pro"]
default = "grok-code"

[[field]]
key = "system_prompt"
label = "System prompt"
type = "text"
default = "You are a helpful assistant for goal tracking."

[[help]]
title = "Chat"
text = """
Run `cordanui-chat.open` from <leader>; (command palette).
Type message, Enter sends, Esc closes. History persists via settings and syncs via Turso.
If cordanui-agents is running, /clear clears history.
"""

[[help]]
title = "Keys"
text = "Enter send · Esc close · Backspace delete · /clear to wipe history"
```

Rules: `name`/`version` required. `provider`/`agent` only if you implement `complete`/`agent_run`. `tool` is RESERVED. `api_key_env` never hardcode, read from `cordanui.config` or `os.getenv`.

## 3. host API you may use (all optional, error if host lacks)

### 3.1 `cordanui.*` (always available in Lua)

| Binding | Notes |
|---|---|
| `cordanui.plugin.name` | your manifest name |
| `cordanui.plugin_dir` | absolute install dir |
| `cordanui.config` | map bare keys from `[[field]]` (all strings), `cordanui.config.api_key` etc. Empty if none. |
| `cordanui.log.info/warn/error(msg)` | to host log, never stdout |
| `cordanui.json.encode(value)` / `decode(str)` | JSON bridge |
| `cordanui.array(tbl)` | marks table as JSON **array** — REQUIRED for `files = cordanui.array({})` else `{}` is object |
| `cordanui.http.request{url, method="GET", headers={}, body}` | awaitable, returns `{status, body: string}`, 120s timeout, via reqwest. Use for LLM. |

`require("sibling")` loads `<plugin>/sibling.lua`.

### 3.2 `cord.*` (injected, check existence)

```lua
-- Config (persisted in settings table, synced via Turso, mirrored to ~/.config/cordanui/config.toml [plugins.cordanui-chat])
cord.config.get(key, default?) -> string|nil
cord.config.set(key, value) -> true  -- value is string

-- Services (long-running backends, e.g. cordanui-agents)
cord.services.is_running("cordanui-agents") -> bool
cord.services.start("cordanui-agents", extraArgs?) -> true
cord.services.stop("cordanui-agents") -> true
cord.services.request("cordanui-agents", {method="POST", path="/wake", headers={}, body={task_id="..."}}) -> {status, body}
-- request requires is_running true, addresses manifest [service].addr or health origin

-- Styling (live, 18 core roles: background, primary, success, tertiary, etc.)
cord.g.style.primary("#ff8800")  -- persisted settings.style.<var>, syncs
cord["local"].style.primary("#ff8800") -- session only
cord.g.style.reset("primary"); cord.g.style.resetAll()
cord.style.get("primary") -> hex or ""

-- Errors
cord.errors.list(limit?) -> [{created_at, context, message, detail}]
cord.errors.clear() -> true

-- Dialogs (awaitable, host renders, you never draw)
cord.ui.input{title?, placeholder?, prefill?} -> string|nil  -- nil = cancel
cord.ui.text{title?, placeholder?, prefill?} -> string|nil   -- multiline, Ctrl+Enter submit
cord.ui.confirm{title?, message} -> true|false
cord.ui.pick{title?, items={"a","b"}} -> 1-based idx|nil
cord.ui.multiselect{title?, items, selected?} -> {idx,...}|nil
cord.ui.notify("msg") or {message, level="info"|"warn"|"error"} -> true

-- Panels (persistent declarative UI, one at a time)
cord.ui.show_panel{title?, draw=function()-> widget|widget[], on_key=function(key)->bool}
cord.ui.close_panel() -> true
-- draw return shapes (detected by fields):
-- {content="…", fg="primary", bold=true}  -- text line, fg is style var
-- {items={"a","b"}, highlight=1}           -- list, 1-based highlight
-- {children={widget,…}}                    -- vertical column, array of widgets also column
-- Keys: "j","k","enter","esc","tab","backspace","up","down","ctrl+x" etc. Return true=handled+redraw, false=pass-through, unhandled Esc closes.
```

Concurrency: only one dialog on screen, one panel at a time (second replaces first). Upvalues persist across frames (your `local history = {}` survives).

### 3.3 `plugin` table you must define in `main.lua`

```lua
plugin = {}

-- Optional: one-shot LLM (if capabilities.provider=true)
function plugin.complete(request) -- {model,prompt,system?,max_tokens?,temperature?,config?}
  return {content="…", usage={prompt_tokens=1, completion_tokens=2}}
end

-- Optional: streaming agent (if capabilities.agent|provider)
function plugin.agent_run(cfg, emit) -- cfg {task_id,title,description?,model?,config?}, emit(event)
  emit{type="progress", message="working"}
  emit{type="result", content="done", files=cordanui.array({})}
end

-- Commands (if capabilities.command=true)
plugin.commands = {
  ["cordanui-chat.open"] = {run=function() … end, desc="Open chat"},
  ["cordanui-chat.clear"] = {run=function() … end, desc="Clear history"},
}

-- Optional: own config page (replaces [[field]] fallback form)
function plugin.configure() -- runs on worker thread, may use cord.ui.* + cord.config
  local idx = cord.ui.pick{title="Model", items={"grok-code","gpt-5"}}
  if idx then cord.config.set("default_model", ({"grok-code","gpt-5"})[idx]) end
  return "saved"
end
```

## 4. chat behavior to implement

### 4.1 entry

* Register `plugin.commands["cordanui-chat.open"]` and `["cordanui-chat.clear"]`. Host exposes via `<leader>;` filterable list. `open` must also be invocable headless (no UI required for test).
* `open` does `cord.ui.show_panel{title="Chat — cordanui-chat", draw=…, on_key=…}` and returns `"chat opened"` (shown on status line). `close` is `cord.ui.close_panel()` or unhandled `Esc`.

### 4.2 panel state (Lua upvalues = view model)

```lua
local history = {} -- persisted, see §5
local draft = ""
local sending = false
-- history: {{role="user"|"assistant", content="…", at="RFC3339"}}
```

`draw()` returns:

```lua
function draw()
  local items = {}
  for _,m in ipairs(history) do items[#items+1] = m.role .. ": " .. m.content end
  return {
    {content="Chat ("..#history.." msgs) — Enter send, Esc close, /clear wipe", fg="primary", bold=true},
    {items=items, highlight=#items>0 and #items or nil},
    {content=(sending and "…thinking…" or "> "..draft), fg=sending and "tertiary" or "secondary"},
  }
end
```

`on_key(key)`:

* `"esc"` → `cord.ui.close_panel(); return true`
* `"enter"` → if `draft:sub(1,1)=="/"` handle `/clear` (clear `history`, persist, `cord.ui.notify`), else push `history+=user`, set `sending=true`, trigger async LLM (see §4.3), `draft=""`, `return true`
* `1-char` → `draft=draft..key; return true`
* `"backspace"` → `draft=draft:sub(1,-2); return true`
* else `return false`

### 4.3 LLM call

Do **not** block `on_key` long if host is single-threaded; `cordanui.http.request` is awaitable (host event loop stays alive). Use:

```lua
local API_KEY = cordanui.config.api_key or os.getenv("OPENCODE_API_KEY") or os.getenv(cordanui.config.api_key_env or "")
local BASE = cordanui.config.base_url or "https://opencode.ai/zen/v1"
local MODEL = cordanui.config.default_model or "grok-code"
local SYSTEM = cordanui.config.system_prompt or "You are helpful."

-- Build messages: system + history (user/assistant)
local messages = {{role="system", content=SYSTEM}}
for _,m in ipairs(history) do messages[#messages+1]={role=m.role, content=m.content} end

-- Option A: direct (works without backend)
local body = cordanui.json.encode{model=MODEL, messages=messages, temperature=0.7}
local res = cordanui.http.request{url=BASE.."/chat/completions", method="POST",
  headers={["content-type"]="application/json", ["authorization"]="Bearer "..(API_KEY or "")},
  body=body}
if res.status ~= 200 then cord.ui.notify{message="chat failed HTTP "..res.status..": "..res.body, level="error"}; sending=false; return end
local parsed = cordanui.json.decode(res.body)
local content = parsed.choices[1].message.content
history[#history+1]={role="assistant", content=content, at=os.date("!%Y-%m-%dT%H:%M:%SZ")}
-- persist (see §5)
cord.config.set("chat.history", cordanui.json.encode(history))

-- Option B: via backend if active (optional)
if cord.services and cord.services.is_running("cordanui-agents") then
  -- alternative: POST to backend's LLM endpoint or use agent task
  -- local r = cord.services.request("cordanui-agents", {method="POST", path="/chat", body={messages=messages}})
end
```

Handle missing `api_key` → `cord.ui.notify{level="error"}` and `emit{type="error"}` if in `agent_run`.

### 4.4 persistence & sync (no new tables for MVP)

Host has **no DDL** for plugins. Use existing synced storage:

* Primary: `cord.config.set("chat.history", json)` → `settings` row `key="cordanui-chat.chat.history"` (`LIKE 'cordanui-chat.%'` stripped to bare). Synced LWW via Turso; survives restarts, reaches other TUI clients. Limit: single global thread.
* Per-goal variant (optional): store `history` keyed by goal. Since `cord.config` is global, key by goal id: `cord.config.set("chat."..goal_id, json)`. For goal-scoped chat, prompt user `cord.ui.pick{title="Goal", items=goal_titles}` then key accordingly.
* Mobile FE hook (already in host): if you want messages visible on mobile without plugin runtime, return `files` from `agent_run` or write `mobile.json` via backend `merge_metadata`. Host merges `files [{path="mobile.json", content=json({content, items})}]` → `goals.metadata.mobile` → `mobile/src/components/PluginCard.tsx` renders. For pure chat plugin (no agent), this path is not needed; `settings` sync is enough for TUI.

Load on `open`:

```lua
local raw = cord.config.get("chat.history")
if raw then local ok, v = pcall(cordanui.json.decode, raw); if ok and type(v)=="table" then history=v end end
```

`/clear` just `history={}; cord.config.set("chat.history", cordanui.json.encode(history))`.

## 5. wire & widget exact shapes (do not rename)

* Settings values are always **strings** regardless of `[[field]] type`. `select` defaults must be one of `options`.
* Panel widgets: `{content, fg?, bold?}` `fg` is style var (`primary`/`success`/`tertiary`/`secondary`/`onBackground` etc., unknown → `onBackground`). `{items, highlight?}` `highlight` 1-based. `{children}` vertical stack. Array return = column.
* Dialog cancel: `nil` for input/pick/text/multiselect, `false` for confirm. Refusal (modal already open) raises Lua error → `pcall`.
* LLM: OpenAI-compatible. For non-OpenAI, adapt: Anthropic `x-api-key`, Gemini `?key=`. Keep `cordanui.http.request` 120s timeout.

## 6. testing checklist (run all)

* [ ] `cordanui.toml` parses, `runtime="lua"` at root, `[[field]]` validates, `name` matches dir
* [ ] `<leader>;` shows `cordanui-chat.open` + `cordanui-chat.clear`, Enter runs, status line shows return string
* [ ] `open` → panel with history list, typing shows in draft line, `Enter` appends user msg, shows `…thinking…`, then assistant reply, persists `cord.config.get("chat.history")` non-empty, survives TUI restart
* [ ] `Esc` closes, second `open` restores history
* [ ] Missing `api_key` → `cord.ui.notify` error, no crash
* [ ] `cord.services.is_running("cordanui-agents")` true → chat still works (either path), false → direct HTTP still works
* [ ] `cargo run -p cordanui-tui` shows `<leader>h` new tab `cordanui-chat` with help text

## 7. notes

* Do not `print` to stdout — corrupts host. Use `cordanui.log.*` and `cord.ui.notify`.
* `cordanui.array({})` required for empty `files` else Lua `{}` is object.
* History is `settings` JSON — keep < 32KB per key, truncate oldest if needed.
* If you need per-goal chat threads at scale, propose host add `chat_messages(id, goal_id, role, content, created_at, deleted_at)` + sync + `cord.chat` API; for now stay within `settings`/`metadata`.
