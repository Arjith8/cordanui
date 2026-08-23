# AGENTS.md — Writing cordanui Plugins

Instructions for building a cordanui plugin from scratch. This document is
**self-contained**: you do not need access to the cordanui source tree —
everything on the wire is specified here.

A cordanui plugin is a **standalone executable** that the host spawns as a
subprocess and talks to over **JSON on stdin/stdout**. No shared libraries,
no linking, no special runtime. Any language works; examples are in Rust.

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
    "bg": "#0f172a",
    "surface": "#1e293b",
    "border": "#1f2937",
    "treeLine": "#334155",
    "text": "#f9fafb",
    "textDim": "#9ca3af",
    "textFaint": "#6b7280",
    "accent": "#3b82f6",
    "onAccent": "#ffffff",
    "danger": "#ef4444",
    "statusPending": "#9ca3af",
    "statusWip": "#3b82f6",
    "statusDone": "#22c55e",
    "statusAgent": "#a855f7"
  }
}
```

Contract:

- On install the host inserts a row into the `themes` table:
  `(id, name, source, colors_json)` where `colors_json` is the `colors`
  object serialized verbatim and `source` is set to your plugin's
  **GitHub repo URL** (the host fills this in — do not include it in the
  JSON). Builtins use `source = 'builtin'`.
- **Color keys are camelCase** (`treeLine`, `textDim`, `textFaint`,
  `onAccent`, `statusPending`, `statusWip`, `statusDone`,
  `statusAgent`) — this matches the mobile client's token names, which
  share the same table. Do not use snake_case.
- Values are hex strings, `#rrggbb` exactly (3-digit `#rgb` is rejected;
  bad values silently fall through to defaults).
- Any subset of keys is valid; missing or unknown tokens fall back to the
  builtin dark palette. Include all 14 for a complete look.
- `id` must be unique across the user's installed themes; re-installing
  with the same id updates the existing row.
- Resolution at runtime: the host reads `settings.theme_mode`; if
  `'explicit'` it uses `settings.selected_theme_id`, otherwise
  (`'system'`) terminals resolve to builtin dark. A theme is only visible
  once the user selects it.

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
- Builds run `[build].cmd` inside the cloned repo dir. Keep builds hermetic;
  pin dependency versions.
