# AGENTS-PROVIDERS.md — Writing cordanui Provider Plugins

Provider-specific companion to [`AGENTS.md`](./AGENTS.md). Read that first
for the general contract (manifest layout, subprocess spawning, JSON-over-
stdio protocols). This document covers what is *specific to provider
plugins*: how to talk to upstream LLM APIs, which request/response specs to
follow, and what must be validated before you emit anything on stdout.

This file is self-contained — no cordanui source access required.

---

## 1. What a provider plugin is

A provider plugin exposes one or more LLM models to cordanui through the
standard plugin CLI:

| Subcommand | stdin | stdout |
|---|---|---|
| `complete --model <id>` | one `CompleteRequest` JSON object | one `CompleteResponse` JSON object |
| `agent-run --task-id <id>` | one `AgentRunConfig` JSON object | newline-delimited `AgentEvent` JSON objects |

Manifest requirements:

```toml
[plugin]
name = "provider-myzen"
version = "0.1.0"

[capabilities]
provider = true

[provider]
models = ["model-id-1", "model-id-2"]   # REQUIRED, non-empty
api_key_env = "MY_PROVIDER_API_KEY"     # optional; prefer a [[field]] form instead

# Recommended: let users paste their key in the TUI (Configure) instead of
# exporting an env var. See AGENTS.md §9.
[[field]]
key = "api_key"
label = "API Key"
type = "secret"
required = true

[build]
cmd = "cargo build --release"
bin = "target/release/provider-myzen"
```

**Key sourcing rule**: read credentials from `config.api_key` first, then
fall back to the `api_key_env` environment variable. This makes the plugin
work both for TUI users (Configure form) and headless/CLI users (env var).
Never hardcode keys, never log them, never persist them.

---

## 2. The two wire specs that matter

Almost every LLM gateway speaks one of these (or both):

### 2.1 OpenAI Chat Completions spec

`POST {base_url}/chat/completions`

```json
// request
{
  "model": "model-id",
  "messages": [
    {"role": "system", "content": "..."},   // only if CompleteRequest.system
    {"role": "user",   "content": "<prompt>"}
  ],
  "max_tokens": 1024,          // omit when null
  "temperature": 0.7           // omit when null
}
```

```json
// response (HTTP 200)
{
  "choices": [ { "message": { "role": "assistant", "content": "..." } } ],
  "usage": { "prompt_tokens": 12, "completion_tokens": 34 }
}
```

Extraction rules: text = `choices[0].message.content`; usage maps directly
(`total_tokens` may be absent).

### 2.2 Anthropic Messages spec

`POST {base_url}/v1/messages` — different auth header!

```
x-api-key: <key>            # NOT Authorization: Bearer
anthropic-version: 2023-06-01
```

```json
// request — max_tokens is REQUIRED here even when the host sends null
{
  "model": "claude-model-id",
  "max_tokens": 1024,
  "system": "...",                       // string, not a message
  "messages": [ { "role": "user", "content": "<prompt>" } ]
}
```

```json
// response
{
  "content": [ { "type": "text", "text": "..." } ],
  "usage": { "input_tokens": 12, "output_tokens": 34 }
}
```

Extraction rules: text = concatenation of all `content[]` entries where
`type == "text"` (there can be several); usage renames input/output →
prompt/completion tokens.

**Validation gotchas between the two specs:**
- Auth header differs (`Authorization: Bearer` vs `x-api-key`).
- `max_tokens` optional in OpenAI, mandatory in Anthropic — default it
  yourself (e.g. 1024) when the host omits it.
- System prompt is a message in OpenAI, a top-level `system` field in
  Anthropic.

### 2.3 OpenAI Responses API (rare, but real)

Some gateways expose GPT models at `{base}/responses` instead:

```json
// request
{ "model": "gpt-x", "input": "<prompt>", "instructions": "<system>" }

// response
{ "output_text": "...", "usage": { "input_tokens": 12, "output_tokens": 34 } }
```

If you support any model on this endpoint, note that `output_text` is the
convenience field; robustly extract by joining `output[]` message contents
when it's absent.

### 2.4 Rule of thumb for routing a model ID to an endpoint

| Model id prefix | Endpoint | Spec |
|---|---|---|
| `gpt*` | `/responses` (or `/chat/completions` if the gateway says so) | Responses / OpenAI |
| `claude*` | `/messages` | Anthropic |
| anything else | `/chat/completions` | OpenAI |

Don't hardcode this blindly — prefer fetching `{base}/models` once to
confirm which endpoint a model lives behind when the gateway offers it.

---

## 3. Case study: OpenCode Zen

- Base URL: `https://opencode.ai/zen/v1`
- Auth: `Authorization: Bearer $OPENCODE_API_KEY` (get a key at
  opencode.ai/auth) — Bearer everywhere, including Claude models
- Model catalog: `GET https://opencode.ai/zen/v1/models`
- Endpoint families actually in use:
  - `/chat/completions` — deepseek-v4-*, glm-*, kimi-k*, minimax-*,
    qwen3-coder, grok-*, most `-free` models
  - `/responses` — gpt-5.x family
  - `/messages` — claude-opus-4-6, claude-sonnet-4-5, other claude-*
- So a minimal Zen provider routes exactly per §2.4 and always sends
  `Authorization: Bearer` (Zen does NOT use x-api-key even for Claude).

Smoke-test your key before writing code:

```bash
curl -s https://opencode.ai/zen/v1/models \
  -H "Authorization: Bearer $OPENCODE_API_KEY" | head -40

curl -s https://opencode.ai/zen/v1/chat/completions \
  -H "Authorization: Bearer $OPENCODE_API_KEY" \
  -H "Content-Type: application/json" \
  -d '{"model":"glm-5.2","messages":[{"role":"user","content":"ping"}]}'
```

---

## 4. Mandatory validation (do these before emitting stdout)

The host trusts your stdout completely. A malformed line breaks the UI, so
validate aggressively and fail loudly via `error` events.

1. **Model gate.** If `--model <id>` is not in your manifest `models`
   list → `error` event immediately. Never forward unknown models upstream.
2. **Key gate.** Env var missing/empty → `error` event before reading
   upstream. Never log the key.
3. **Request shaping per §2.** Null host fields must be *omitted*, not sent
   as JSON `null` (some gateways 400 on explicit nulls) — except Anthropic
   `max_tokens`, which you must fill with a default instead.
4. **HTTP status gate.** Non-2xx → `error` event containing the status code
   and ≤200 chars of the response body. Map common cases:
   - 401/403 → "invalid or missing API key"
   - 404 → "unknown model or wrong endpoint for this model"
   - 429 → "rate limited"
   - 5xx → "upstream error, retry later"
5. **Response shape gate.** Before trusting the payload: OpenAI requires a
   non-empty `choices` array with `.message.content` present (it may be ""
   legitimately, but not missing); Anthropic requires at least one
   `content[]` entry of type `text`. Missing usage is fine — emit
   `"usage": null` rather than inventing numbers.
6. **Stream discipline (agent-run).** Emit the first `progress` event as
   soon as the upstream connection opens, then roughly every chunk/batch —
   hosts show these to the user. Exactly one terminal `result`/`error`.
7. **stdout hygiene.** Only protocol JSON on stdout. HTTP libraries love
   printing warnings — they go nowhere near stdout.

## 5. Streaming over SSE (for agent-run)

Both major specs stream as Server-Sent Events; parse accordingly:

**OpenAI-style** (`stream: true`):
```
data: {"choices":[{"delta":{"content":"He"}}]}
data: {"choices":[{"delta":{"content":"llo"}}]}
data: [DONE]
```
Accumulate `delta.content` until `[DONE]`.

**Anthropic-style:**
```
event: content_block_delta
data: {"type":"content_block_delta","delta":{"type":"text_delta","text":"He"}}
event: message_stop
```
Accumulate `delta.text` from `content_block_delta` events; stop on
`message_stop`.

Emit a cordanui `progress` event per N deltas (e.g. every 10 chunks or
every ~200 chars) with the accumulated length, then a single `result` with
the full text. Usage arrives in the terminal frame(s) (`message_delta` /
final chunk) — capture it there.

## 6. Testing checklist

- [ ] Manifest `models` non-empty; requesting an unlisted model errors fast
- [ ] `complete` round-trip against a real gateway for EACH spec you route
      (one OpenAI-compatible model, one Anthropic model, one Responses
      model if supported)
- [ ] Wrong API key → clean `error` mentioning auth, no stack trace
- [ ] Unknown model at the gateway (listed locally but 404 upstream) →
      mapped 404-style error event
- [ ] `agent-run` streams ≥2 progress events then exactly one result;
      stdout contains nothing but NDJSON events
- [ ] Upstream 500 mid-stream → terminal `error` event, exit code 0
      (the failure was reported in-band)
