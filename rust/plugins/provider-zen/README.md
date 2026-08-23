# provider-zen

A cordanui provider plugin for [OpenCode Zen](https://opencode.ai/zen).

Zen is an AI gateway that gives access to GPT, Claude, Gemini, Qwen,
DeepSeek, Grok, Kimi, and more through a single API key. This plugin uses
the OpenAI-compatible `/chat/completions` endpoint, which works with the
widest range of models.

## status

Working scaffold. Compiles clean. Not yet tested against the live Zen API
(needs an `OPENCODE_API_KEY`).

## setup

1. Get an API key at [opencode.ai/auth](https://opencode.ai/auth)
2. Set the environment variable:
   ```bash
   export OPENCODE_API_KEY="your-key-here"
   ```
3. Build the plugin:
   ```bash
   cd plugins/provider-zen
   cargo build --release
   ```
4. Install it (copy or symlink to `~/.local/share/cordanui/plugins/provider-zen/`)

## how it works

The plugin is a standalone Rust binary with two subcommands:

### `complete --model <model>`

One-shot completion. Reads a `CompleteRequest` JSON object from stdin, calls
`POST https://opencode.ai/zen/v1/chat/completions` (non-streaming), writes a
`CompleteResponse` JSON object to stdout.

```bash
echo '{"model":"gpt-5.4","prompt":"Write a haiku about goals"}' | \
  ./provider-zen complete --model gpt-5.4
```

### `agent-run --task-id <id>`

Streaming agent run. Reads an `AgentRunConfig` JSON object from stdin, calls
`POST https://opencode.ai/zen/v1/chat/completions` with `stream: true`, reads
SSE chunks, emits newline-delimited `AgentEvent` JSON objects to stdout
(progress events with accumulated content, then a final result event).

```bash
echo '{"task_id":"abc","title":"Plan a product launch"}' | \
  ./provider-zen agent-run --task-id abc
```

## supported models

The manifest declares support for:

| Model ID | Provider |
|---|---|
| `gpt-5.4` | OpenAI |
| `gpt-5.4-mini` | OpenAI |
| `claude-sonnet-4-5` | Anthropic |
| `claude-haiku-4-5` | Anthropic |
| `gemini-3.7-flash` | Google |
| `qwen-3-coder-480b` | Alibaba |
| `deepseek-v4-pro` | DeepSeek |
| `grok-code` | xAI |
| `kimi-k2` | Moonshot |

See the [full Zen model list](https://opencode.ai/docs/zen/) for all
available models. Any model accessible via the `/chat/completions` endpoint
will work — just pass it as the `model` field.

## configuration

The `cordanui.toml` manifest declares the plugin's capabilities and models.
The agent backend (`cordanui-agents`) reads this manifest to find the binary
and validate the provider capability.

```toml
[plugin]
name = "provider-zen"
version = "0.1.0"

[capabilities]
provider = true

[provider]
models = ["gpt-5.4", "claude-sonnet-4-5", ...]
api_key_env = "OPENCODE_API_KEY"
```

## architecture

```
cordanui-agents (host)
    │
    │ spawn subprocess: provider-zen agent-run --task-id X
    │ stdin: AgentRunConfig JSON
    │
    ▼
provider-zen (plugin binary)
    │
    │ POST https://opencode.ai/zen/v1/chat/completions
    │   Authorization: Bearer $OPENCODE_API_KEY
    │   stream: true
    │
    ▼
OpenCode Zen Gateway
    │
    │ SSE chunks (data: {"choices":[{"delta":{"content":"..."}}]})
    │
    ▼
provider-zen
    │
    │ stdout: newline-delimited AgentEvent JSON
    │   {"type":"progress","message":"...","detail":"..."}
    │   {"type":"result","content":"...","files":[]}
    │
    ▼
cordanui-agents (host)
    │ writes agent_progress / agent_result to DB
```

## files

```
src/
├── main.rs       # CLI entry point: clap subcommands, stdin reading, dispatch
├── protocol.rs   # Protocol types (mirrors rust/crates/plugin-runtime/src/protocol.rs)
└── zen.rs        # HTTP client: blocking complete + streaming agent-run
```
