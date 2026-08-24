# AGENTS — Adding a Provider to cordanui (end-to-end)

The complete runbook for getting a new LLM provider working in cordanui,
from zero to selectable model. Three stages:

```
A. author the plugin   →  B. install & activate   →  C. use it
```

Read alongside:
- [`AGENTS.md`](./AGENTS.md) — general plugin contract (manifest, protocols)
- [`AGENTS-PROVIDERS.md`](./AGENTS-PROVIDERS.md) — upstream LLM API specs
  (OpenAI / Anthropic / Responses), validation gates, Zen case study

---

## Stage A — Author the provider plugin

1. Create a repo named after the plugin (e.g. `provider-myzen`) with
   `cordanui.toml` at the root:

   ```toml
   [plugin]
   name = "provider-myzen"
   version = "0.1.0"

   [capabilities]
   provider = true

   [provider]
   models = ["glm-5.2", "kimi-k3", "claude-sonnet-4-5"]  # exactly what you support
   api_key_env = "MYZEN_API_KEY"

   [build]
   cmd = "cargo build --release"
   bin = "target/release/provider-myzen"
   ```

2. Implement two subcommands over JSON-stdio (shapes in `AGENTS.md` §4):
   - `complete --model <id>` — one-shot, one JSON response.
   - `agent-run --task-id <id>` — NDJSON stream: progress…, one result/error.

3. Route upstream calls per spec (`AGENTS-PROVIDERS.md` §2/§4):
   - OpenAI-compatible → `POST {base}/chat/completions`, Bearer auth
   - Anthropic → `POST {base}/v1/messages`, `x-api-key` +
     `anthropic-version`, `max_tokens` mandatory
   - GPT-style → `/responses`
   - Run all seven validation gates before emitting anything on stdout.

4. Declare a settings form so users can paste their key in the TUI
   (`AGENTS.md` §9):

   ```toml
   [[field]]
   key = "api_key"
   label = "API Key"
   type = "secret"
   required = true

   [[field]]
   key = "default_model"
   label = "Default model"
   type = "select"
   options = ["glm-5.2", "kimi-k3"]   # mirror [provider].models
   ```

   Read credentials as `config.api_key` (host-injected) with the env var
   (`api_key_env`) as fallback.

5. Pass the testing checklists in both guides. Non-negotiables:
   - every listed model round-trips against the live gateway
   - bad key / unknown model / HTTP 500 all produce clean in-band errors

## Stage B — Install & activate

6. Push the repo to GitHub (public). The plugin manager finds repos by
   link or name search.

7. In cordanui: `<leader>+p` → `i` → paste the repo URL or `owner/repo`
   → Enter. The manager clones into
   `~/.local/share/cordanui/plugins/<repo>`, validates `cordanui.toml`,
   registers it **active** in the `plugins` table (most recent first),
   and returns you to the list.

8. If it got deactivated somehow, select it and press Enter to re-activate.

## Stage C — Configure & use

9. In the plugin manager select the provider and press `c` (Configure).
   Fill in the form the plugin declared (`api_key`, default model, …) and
   Enter-save each field. Values are stored namespaced under
   `<plugin>.<key>` and injected into every invocation as the request's
   `config` object.

10. Headless alternative: instead of Configure, export the env var named by
    `api_key_env`; plugins fall back to it when `config.api_key` is absent.

---

## Current host-side state (honest gaps)

What works today vs. what the host still needs to grow:

| Capability | Status |
|---|---|
| Install / uninstall / activate plugins | ✅ plugin manager |
| Registry (`plugins` table) with active flag | ✅ |
| Theme packs import + live apply | ✅ |
| Provider subprocess invocation (`complete` / `agent-run`) | ✅ runtime crate (`spawn.rs`) |
| Declarative settings forms ([[field]] → Configure UI) | ✅ |
| Settings injection into requests (`config` object) | ⚙️ plumbing done (`settings_to_config`); host spawn wiring pending |
| Model picker reading `[provider].models` | ❌ not built yet |
| Goal → "run with agent/provider" UI | ❌ not built yet |
| Storing provider/model choice on goals (`agent_status` cols exist) | ❌ not wired |
| Key entry via TUI (Configure form) | ✅ |
| Key presence check surfaced before first run ("run Configure") | ❌ not wired |
| Model picker reading `[provider].models` | ❌ not built yet |

Until the ❌ rows land, providers are testable end-to-end via direct CLI
invocation of the plugin binary (see testing checklists), and by the agent
backend once it consumes the same registry.

## Adding a *new* gateway later (e.g. another aggregator)

If it's OpenAI-compatible or Anthropic-spec, no new concepts are needed —
repeat Stage A with its base URL/auth/model ids. Only gateways that break
both specs would require extending this document first.
