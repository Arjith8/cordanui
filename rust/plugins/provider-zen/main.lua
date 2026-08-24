-- provider-zen: OpenCode Zen provider plugin (Lua runtime).
--
-- Talks to the OpenAI-compatible /chat/completions endpoint on the Zen
-- gateway. Credentials come from cordanui.config.api_key (host-injected
-- from the plugin's settings form) with the OPENCODE_API_KEY env var as
-- fallback.

local M = {}

local BASE = cordanui.config.base_url or "https://opencode.ai/zen/v1"
local API_KEY = cordanui.config.api_key or os.getenv("OPENCODE_API_KEY")

--- One chat completion against the OpenAI-compatible endpoint.
-- messages: array of { role = "system"|"user"|"assistant", content = "..." }
-- returns the assistant message content string.
function M.chat(model, messages, max_tokens)
  if not API_KEY then
    error("no api key: set it in the plugin Configure form or export OPENCODE_API_KEY", 0)
  end

  local payload = { model = model, messages = messages }
  if max_tokens then payload.max_tokens = max_tokens end

  local res = cordanui.http.request({
    url = BASE .. "/chat/completions",
    method = "POST",
    headers = {
      ["content-type"] = "application/json",
      ["authorization"] = "Bearer " .. API_KEY,
    },
    body = cordanui.json.encode(payload),
  })

  if res.status ~= 200 then
    error("zen gateway returned HTTP " .. tostring(res.status) .. ": " .. res.body, 0)
  end

  local body = cordanui.json.decode(res.body)
  local choice = body.choices and body.choices[1]
  if not choice then
    error("zen gateway returned no choices: " .. res.body, 0)
  end
  return choice.message.content, body.usage
end

-- ---------- host entry points ----------

plugin = {}

--- one-shot completion
-- request: { model, prompt, system?, max_tokens?, temperature?, config? }
function plugin.complete(request)
  local messages = {}
  if request.system then
    table.insert(messages, { role = "system", content = request.system })
  end
  table.insert(messages, { role = "user", content = request.prompt })

  local content, usage = M.chat(request.model, messages, request.max_tokens)
  return { content = content, usage = usage }
end

--- streaming agent run
-- config: { task_id, title, description?, model?, config? }
-- emit(event) forwards NDJSON events to the host.
function plugin.agent_run(config, emit)
  local model = config.model or cordanui.config.default_model or "grok-code"
  emit({ type = "progress", message = "starting agent run (" .. model .. ")" })

  local task = config.title
  if config.description and config.description ~= "" then
    task = task .. "\n\n" .. config.description
  end

  emit({ type = "progress", message = "asking " .. model .. "..." })
  local content = M.chat(model, {
    { role = "system", content =
      "You are a goal-completion agent inside cordanui. Do the task, " ..
      "be concise, output only what is asked for." },
    { role = "user", content = task },
  })

  emit({ type = "result", content = content, files = cordanui.array({}) })
end

return M
