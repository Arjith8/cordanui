//! Embedded Lua plugin runtime.
//!
//! Plugins with `runtime = "lua"` in their manifest are executed in-process
//! by the host. The plugin repo root contains a `main.lua` script that must
//! define a global `plugin` table with handler functions:
//!
//! ```lua
//! -- provider example
//! plugin = {}
//!
//! function plugin.complete(request)
//!   -- request: { model, prompt, system?, max_tokens?, temperature?, config? }
//!   local res = cordanui.http.request({
//!     url = "https://example.com/v1/chat/completions",
//!     method = "POST",
//!     headers = { ["authorization"] = "Bearer " .. cordanui.config.api_key },
//!     body = cordanui.json.encode({ model = request.model, messages = { ... } }),
//!   })
//!   if res.status ~= 200 then error("upstream returned " .. res.status) end
//!   local body = cordanui.json.decode(res.body)
//!   return { content = body.choices[1].message.content }
//! end
//!
//! function plugin.agent_run(config, emit)
//!   -- config: AgentRunConfig table; emit(event) streams NDJSON events
//!   emit({ type = "progress", message = "working..." })
//!   emit({ type = "result", content = "done", files = {} })
//! end
//! ```
//!
//! The host injects a `cordanui` global (the exported API surface):
//!
//! - `cordanui.plugin.name` — plugin name from the manifest
//! - `cordanui.config` — settings collected from the manifest's `[[field]]`
//!   form, namespaced keys stripped to bare field keys
//! - `cordanui.log.info/warn/error(msg)` — host log stream
//! - `cordanui.json.encode(value)` / `.decode(str)` — JSON bridge
//! - `cordanui.http.request{url, method?, headers?, body?}` →
//!   `{status, body}` — HTTP via the host (reqwest), awaitable
//!
//! A second global, `cord`, restyles the UI live (see [`crate::style`]):
//!
//! ```lua
//! cord.g.style.primary("#ff8800")          -- persist at DB level (syncs)
//! cord["local"].style.primary("#ff8800")   -- this session only
//! ```
//!
//! Rendering is declarative: plugins never touch the terminal directly.
//! A `cordanui.ui` widget-builder module is planned; the settings-form
//! contract (`[[field]]`) already follows this model.

use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};

use crate::protocol::{AgentEvent, AgentRunConfig, CompleteRequest, CompleteResponse};
use crate::style::{parse_color, SharedStyleHost};
use crate::ui::{SharedUiHost, UiLevel, UiRequest, UiResponse};
use anyhow::{bail, Context, Result};
use mlua::{Function, Lua, LuaSerdeExt, Table, Value, Value as LuaValue};

/// An in-process Lua plugin: a loaded and initialized `main.lua`.
pub struct LuaPlugin {
    lua: Arc<Lua>,
    pub name: String,
}

impl std::fmt::Debug for LuaPlugin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LuaPlugin")
            .field("name", &self.name)
            .finish()
    }
}

impl LuaPlugin {
    /// Load a Lua plugin from its directory, injecting collected settings
    /// as `cordanui.config`. `styles` backs the `cord.g` / `cord.local`
    /// styling API — pass `None` when the host has no style storage and
    /// those calls will error for the plugin.
    pub fn load(
        dir: &Path,
        name: &str,
        config: Option<serde_json::Value>,
        styles: Option<SharedStyleHost>,
        ui: Option<SharedUiHost>,
    ) -> Result<Self> {
        let entry = dir.join(crate::manifest::PluginManifest::LUA_ENTRY);
        let source = std::fs::read_to_string(&entry)
            .with_context(|| format!("reading {}", entry.display()))?;

        let lua = Arc::new(Lua::new());
        register_api(&lua, dir, name, config).context("registering cordanui API")?;
        register_cord(&lua, styles, ui).context("registering cord styling API")?;

        // Let scripts require sibling files relative to the plugin root.
        let package: Table = lua.globals().get("package")?;
        let path: String = package.get("path")?;
        package.set(
            "path",
            format!(
                "{}/?.lua;{}/?/init.lua;{}",
                dir.display(),
                dir.display(),
                path
            ),
        )?;

        lua.load(source)
            .set_name("main.lua")
            .exec()
            .with_context(|| format!("loading {}", entry.display()))?;

        // Fail fast if the entry point doesn't define what we need.
        let plugin: Table = lua
            .globals()
            .get("plugin")
            .context("main.lua must define a global `plugin` table")?;
        if !plugin.contains_key("complete")? && !plugin.contains_key("agent_run")? {
            bail!("plugin table defines neither complete nor agent_run");
        }

        Ok(Self {
            lua,
            name: name.to_string(),
        })
    }

    /// One-shot completion: calls `plugin.complete(request)`, converts the
    /// returned table into a [`CompleteResponse`].
    pub async fn complete(&self, request: &CompleteRequest) -> Result<CompleteResponse> {
        let plugin: Table = self.lua.globals().get("plugin")?;
        let func: Function = plugin
            .get("complete")
            .context("plugin.complete not defined")?;

        let arg = to_lua(&self.lua, serde_json::to_value(request)?)?;
        let ret: LuaValue = func
            .call_async(arg)
            .await
            .map_err(|e| anyhow::anyhow!("plugin.complete failed: {e}"))?;

        let response = serde_json::from_value(to_json(ret)?)
            .context("plugin.complete returned an invalid response shape")?;
        Ok(response)
    }

    /// Streaming agent run: calls `plugin.agent_run(config, emit)` where
    /// `emit` forwards events to `on_event` synchronously as they happen.
    /// Returns the terminal event (result or error); errors if the run
    /// ended without one.
    pub async fn agent_run<F>(&self, cfg: &AgentRunConfig, on_event: F) -> Result<AgentEvent>
    where
        F: FnMut(&AgentEvent) + Send + 'static,
    {
        let plugin: Table = self.lua.globals().get("plugin")?;
        let func: Function = plugin
            .get("agent_run")
            .context("plugin.agent_run not defined")?;

        // create_function needs Fn, on_event is FnMut — share through a mutex.
        let handler: Arc<Mutex<F>> = Arc::new(Mutex::new(on_event));
        let last_terminal: Arc<Mutex<Option<AgentEvent>>> = Arc::new(Mutex::new(None));

        let terminal = last_terminal.clone();
        let emit = self.lua.create_function(move |_, ev: LuaValue| {
            let event: AgentEvent =
                serde_json::from_value(to_json(ev).map_err(mlua::Error::external)?)
                    .map_err(mlua::Error::external)?;
            if matches!(event, AgentEvent::Result { .. } | AgentEvent::Error { .. }) {
                *terminal.lock().expect("terminal lock") = Some(event.clone());
            }
            (handler.lock().expect("handler lock"))(&event);
            Ok(())
        })?;

        let arg = to_lua(&self.lua, serde_json::to_value(cfg)?)?;
        func.call_async::<()>((arg, emit))
            .await
            .map_err(|e| anyhow::anyhow!("plugin.agent_run failed: {e}"))?;

        let final_event = last_terminal.lock().expect("terminal lock").take();
        match final_event {
            Some(event) => Ok(event),
            None => bail!("agent_run ended without a result or error event"),
        }
    }
}

// ---------- cordanui.* API surface ----------

fn register_api(
    lua: &Lua,
    plugin_dir: &Path,
    name: &str,
    config: Option<serde_json::Value>,
) -> mlua::Result<()> {
    let api = lua.create_table()?;

    // cordanui.plugin
    let meta = lua.create_table()?;
    meta.set("name", name)?;
    api.set("plugin", meta)?;

    // cordanui.config — injected settings (empty table when none).
    let config = match config {
        Some(serde_json::Value::Object(map)) => serde_json::Value::Object(map),
        _ => serde_json::Value::Object(Default::default()),
    };
    api.set("config", to_lua(lua, config)?)?;

    // cordanui.log.{info,warn,error}
    let log = lua.create_table()?;
    log.set(
        "info",
        lua.create_function(|_, msg: String| {
            tracing::info!(target: "plugin", "{msg}");
            Ok(())
        })?,
    )?;
    log.set(
        "warn",
        lua.create_function(|_, msg: String| {
            tracing::warn!(target: "plugin", "{msg}");
            Ok(())
        })?,
    )?;
    log.set(
        "error",
        lua.create_function(|_, msg: String| {
            tracing::error!(target: "plugin", "{msg}");
            Ok(())
        })?,
    )?;
    api.set("log", log)?;

    // cordanui.json.{encode,decode}
    let json = lua.create_table()?;
    json.set(
        "encode",
        lua.create_function(|_, v: LuaValue| {
            serde_json::to_string(&v).map_err(mlua::Error::external)
        })?,
    )?;
    json.set(
        "decode",
        lua.create_function(|lua, s: String| {
            let v: serde_json::Value = serde_json::from_str(&s).map_err(mlua::Error::external)?;
            to_lua(lua, v)
        })?,
    )?;
    api.set("json", json)?;

    // cordanui.array(tbl) — marks a table as a JSON array. Needed because
    // Lua cannot distinguish {} (empty array) from {} (empty map); without
    // this, `files = {}` serializes as a JSON object and fails to decode.
    api.set(
        "array",
        lua.create_function(|lua, t: Table| {
            t.set_metatable(Some(lua.array_metatable()))?;
            Ok(t)
        })?,
    )?;

    // cordanui.http.request — HTTP through the host. Awaitable from Lua.
    let http = lua.create_table()?;
    http.set(
        "request",
        lua.create_async_function(|lua, params: Table| {
            let lua = lua.clone();
            async move {
                let url: String = params.get("url")?;
                let method: Option<String> = params.get("method")?;
                let headers: Option<Table> = params.get("headers")?;
                let body: Option<String> = params.get("body")?;

                let mut req = http_client().request(method_from(&method), &url);
                if let Some(hs) = headers {
                    for pair in hs.pairs::<String, String>() {
                        let (k, v) = pair?;
                        req = req.header(k, v);
                    }
                }
                if let Some(b) = body {
                    req = req.body(b);
                }

                let resp = req.send().await.map_err(mlua::Error::external)?;
                let status = resp.status().as_u16();
                let text = resp.text().await.map_err(mlua::Error::external)?;

                let out = lua.create_table()?;
                out.set("status", status)?;
                out.set("body", text)?;
                Ok(out)
            }
        })?,
    )?;
    api.set("http", http)?;

    // cordanui.plugin_dir — where this plugin lives on disk.
    api.set("plugin_dir", plugin_dir.display().to_string())?;

    lua.globals().set("cordanui", api)?;
    Ok(())
}

/// The `cord` global — live restyling.
///
/// ```lua
/// cord.g.style.primary("#ff8800")          -- persist (DB-level, syncs)
/// cord["local"].style.primary("#ff8800")   -- this session only
/// -- ("local" is a Lua keyword, so bracket indexing is required)
/// cord.g.style.reset("primary")            -- clear one override
/// cord.g.style.resetAll()                  -- clear all overrides in scope
/// cord.style.get("primary")                -- effective value (hex) or ""
/// ```
///
/// Any variable name works: the 18 core roles plus whatever custom names
/// plugins introduce. Colors accept `#rgb`, `#rrggbb`, `rgb(r,g,b)` and
/// `rgba(r,g,b,a)` (alpha is dropped).
fn register_cord(
    lua: &Lua,
    styles: Option<SharedStyleHost>,
    ui: Option<SharedUiHost>,
) -> mlua::Result<()> {
    let cord = lua.create_table()?;
    register_cord_ui(lua, &cord, ui)?;

    for scope in ["g", "local"] {
        let persistent = scope == "g";
        let namespace = lua.create_table()?;
        let style = lua.create_table()?;

        // Dynamic variables: style.<any-name> resolves to a setter.
        let mt = lua.create_table()?;
        mt.set(
            "__index",
            lua.create_function({
                let styles = styles.clone();
                move |lua, (_tbl, var): (Table, String)| {
                    let styles = styles.clone();
                    Ok(lua.create_function(move |_, value: String| {
                        let Some(host) = styles.as_ref() else {
                            return Err(mlua::Error::runtime(
                                "styling is not available in this host",
                            ));
                        };
                        let Some(hex) = parse_color(&value) else {
                            return Err(mlua::Error::runtime(format!(
                                "invalid color '{value}' — use #rgb, #rrggbb or rgb(r,g,b)"
                            )));
                        };
                        host.set(persistent, &var, &hex);
                        Ok(())
                    })?)
                }
            })?,
        )?;
        style.set_metatable(Some(mt))?;

        // style.reset(var) / style.resetAll()
        style.set(
            "reset",
            lua.create_function({
                let styles = styles.clone();
                move |_, var: String| {
                    let Some(host) = styles.as_ref() else {
                        return Err(mlua::Error::runtime(
                            "styling is not available in this host",
                        ));
                    };
                    host.clear(persistent, &var);
                    Ok(())
                }
            })?,
        )?;
        style.set(
            "resetAll",
            lua.create_function({
                let styles = styles.clone();
                move |_, ()| {
                    let Some(host) = styles.as_ref() else {
                        return Err(mlua::Error::runtime(
                            "styling is not available in this host",
                        ));
                    };
                    host.clear_all(persistent);
                    Ok(())
                }
            })?,
        )?;

        namespace.set("style", style)?;
        cord.set(scope, namespace)?;
    }

    // cord.style.get(var) — the effective override, if any.
    let lookup = lua.create_table()?;
    lookup.set(
        "get",
        lua.create_function(move |_, var: String| {
            Ok(styles
                .as_ref()
                .and_then(|h| h.resolved(&var))
                .unwrap_or_default())
        })?,
    )?;
    cord.set("style", lookup)?;

    lua.globals().set("cord", cord)?;
    Ok(())
}

/// The `cord.ui` table — host-rendered modal dialogs.
///
/// ```lua
/// local name  = cord.ui.input{ title = "Goal", placeholder = "..." }
/// local ok    = cord.ui.confirm{ title = "Delete", message = "sure?" }
/// local idx   = cord.ui.pick{ title = "Pick", items = { "a", "b" } } -- 1-based
/// ```
///
/// All three await the user's answer; the host keeps its event loop (and
/// other plugins) running while a plugin waits. Cancel resolves to
/// `nil`/`false`; a host that cannot show the dialog raises an error.
fn register_cord_ui(lua: &Lua, cord: &Table, ui: Option<SharedUiHost>) -> mlua::Result<()> {
    let api = lua.create_table()?;

    // cord.ui.input{title?, placeholder?, prefill?} -> string | nil
    api.set(
        "input",
        lua.create_async_function({
            let ui = ui.clone();
            move |_, params: Table| {
                let ui = ui.clone();
                async move {
                    let title: Option<String> = params.get("title").ok();
                    let placeholder: Option<String> = params.get("placeholder").ok();
                    let prefill: Option<String> = params.get("prefill").ok();
                    match ui_answer(
                        ui.as_ref(),
                        UiRequest::Input {
                            title: title.unwrap_or_default(),
                            placeholder,
                            prefill,
                        },
                    )
                    .await?
                    {
                        UiResponse::Text(v) => Ok(v),
                        _ => Err(mlua::Error::runtime("unexpected response to input")),
                    }
                }
            }
        })?,
    )?;

    // cord.ui.confirm{title?, message} -> boolean
    api.set(
        "confirm",
        lua.create_async_function({
            let ui = ui.clone();
            move |_, params: Table| {
                let ui = ui.clone();
                async move {
                    let title: Option<String> = params.get("title").ok();
                    let message: String = params.get("message").unwrap_or_default();
                    match ui_answer(
                        ui.as_ref(),
                        UiRequest::Confirm {
                            title: title.unwrap_or_default(),
                            message,
                        },
                    )
                    .await?
                    {
                        UiResponse::Confirmed(v) => Ok(v),
                        _ => Err(mlua::Error::runtime("unexpected response to confirm")),
                    }
                }
            }
        })?,
    )?;

    // cord.ui.pick{title?, items = {...}} -> index (1-based) | nil
    api.set(
        "pick",
        lua.create_async_function({
            let ui = ui.clone();
            move |_, params: Table| {
                let ui = ui.clone();
                async move {
                    let title: Option<String> = params.get("title").ok();
                    let items: Vec<String> = params.get("items").unwrap_or_default();
                    if items.is_empty() {
                        return Err(mlua::Error::runtime(
                            "cord.ui.pick needs a non-empty items list",
                        ));
                    }
                    match ui_answer(
                        ui.as_ref(),
                        UiRequest::Pick {
                            title: title.unwrap_or_default(),
                            items,
                        },
                    )
                    .await?
                    {
                        UiResponse::Choice(idx) => Ok(idx.map(|i| i as mlua::Integer + 1)),
                        _ => Err(mlua::Error::runtime("unexpected response to pick")),
                    }
                }
            }
        })?,
    )?;

    // cord.ui.multiselect{title?, items = {...}, selected? = {1-based}} ->
    // array of 1-based indices (possibly empty) | nil on cancel
    api.set(
        "multiselect",
        lua.create_async_function({
            let ui = ui.clone();
            let lua = lua.clone();
            move |_, params: Table| {
                let ui = ui.clone();
                let lua = lua.clone();
                async move {
                    let title: Option<String> = params.get("title").ok();
                    let items: Vec<String> = params.get("items").unwrap_or_default();
                    if items.is_empty() {
                        return Err(mlua::Error::runtime(
                            "cord.ui.multiselect needs a non-empty items list",
                        ));
                    }
                    let preselected: Vec<mlua::Integer> =
                        params.get("selected").unwrap_or_default();
                    let preselected: Vec<usize> = preselected
                        .into_iter()
                        .map(|i| (i - 1).max(0) as usize)
                        .filter(|i| *i < items.len())
                        .collect();
                    match ui_answer(
                        ui.as_ref(),
                        UiRequest::MultiSelect {
                            title: title.unwrap_or_default(),
                            items,
                            preselected,
                        },
                    )
                    .await?
                    {
                        UiResponse::Choices(idx) => match idx {
                            // Cancelled -> nil.
                            None => Ok(Value::Nil),
                            Some(indices) => {
                                let t = lua.create_table()?;
                                for i in indices {
                                    t.push(i as mlua::Integer + 1)?;
                                }
                                // Empty table = submitted with nothing selected
                                // (distinct from nil = cancelled).
                                Ok(Value::Table(t))
                            }
                        },
                        _ => Err(mlua::Error::runtime("unexpected response to multiselect")),
                    }
                }
            }
        })?,
    )?;

    // cord.ui.text{title?, placeholder?, prefill?} -> string | nil
    // Multi-line: Enter inserts a newline; the host's submit chord commits.
    api.set(
        "text",
        lua.create_async_function({
            let ui = ui.clone();
            move |_, params: Table| {
                let ui = ui.clone();
                async move {
                    let title: Option<String> = params.get("title").ok();
                    let placeholder: Option<String> = params.get("placeholder").ok();
                    let prefill: Option<String> = params.get("prefill").ok();
                    match ui_answer(
                        ui.as_ref(),
                        UiRequest::Text {
                            title: title.unwrap_or_default(),
                            placeholder,
                            prefill,
                        },
                    )
                    .await?
                    {
                        UiResponse::Text(v) => Ok(v),
                        _ => Err(mlua::Error::runtime("unexpected response to text")),
                    }
                }
            }
        })?,
    )?;

    // cord.ui.notify(message | { message, level? }) -> true
    // Fire-and-forget: the host shows a transient status message; there is
    // nothing to await. level: "info" (default) | "warn" | "error".
    api.set(
        "notify",
        lua.create_function({
            let ui = ui.clone();
            move |_, params: Value| {
                let Some(host) = ui.as_ref() else {
                    return Err(mlua::Error::runtime(
                        "cord.ui is not available in this host",
                    ));
                };
                let (message, level) = match &params {
                    Value::String(s) => (s.to_str()?.to_string(), UiLevel::Info),
                    Value::Table(t) => {
                        let message: String = t.get("message")?;
                        let level: Option<String> = t.get("level").ok();
                        let level = match level.as_deref() {
                            Some("warn") => UiLevel::Warn,
                            Some("error") => UiLevel::Error,
                            _ => UiLevel::Info,
                        };
                        (message, level)
                    }
                    _ => {
                        return Err(mlua::Error::runtime(
                            "cord.ui.notify takes a string or a table {message, level}",
                        ))
                    }
                };
                host.notify(level, message);
                Ok(true)
            }
        })?,
    )?;

    cord.set("ui", api)?;
    Ok(())
}

/// Submit a request and wait for the host's answer. No host, refused
/// request, or dropped channel all surface here:
/// - no host attached → error ("UI is not available in this host")
/// - `Refused(reason)` → error carrying the reason
/// - dropped responder → treated as cancel (`None`)
async fn ui_answer(ui: Option<&SharedUiHost>, request: UiRequest) -> mlua::Result<UiResponse> {
    let Some(host) = ui else {
        return Err(mlua::Error::runtime(
            "cord.ui is not available in this host",
        ));
    };
    let (tx, rx) = tokio::sync::oneshot::channel();
    host.submit(crate::ui::PendingUi {
        request,
        respond: tx,
    });
    match rx.await {
        Ok(UiResponse::Refused(reason)) => Err(mlua::Error::runtime(format!(
            "the host refused the dialog: {reason}"
        ))),
        Ok(response) => Ok(response),
        // Host dropped the channel without answering — treat as cancel.
        Err(_) => Ok(UiResponse::Text(None)),
    }
}

fn http_client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(120))
            .build()
            .expect("building shared HTTP client")
    })
}

fn method_from(m: &Option<String>) -> reqwest::Method {
    match m.as_deref().map(|s| s.to_uppercase()).as_deref() {
        Some("POST") => reqwest::Method::POST,
        Some("PUT") => reqwest::Method::PUT,
        Some("PATCH") => reqwest::Method::PATCH,
        Some("DELETE") => reqwest::Method::DELETE,
        _ => reqwest::Method::GET,
    }
}

// ---------- JSON <-> Lua conversion ----------

/// Convert a JSON value into an equivalent Lua value. Arrays become
/// 1-indexed tables; objects become string-keyed tables.
fn to_lua(lua: &Lua, v: serde_json::Value) -> mlua::Result<LuaValue> {
    use serde_json::Value as J;
    Ok(match v {
        J::Null => LuaValue::Nil,
        J::Bool(b) => LuaValue::Boolean(b),
        J::Number(n) => match n.as_i64() {
            Some(i) => LuaValue::Integer(i),
            None => LuaValue::Number(n.as_f64().unwrap_or(f64::NAN)),
        },
        J::String(s) => LuaValue::String(lua.create_string(&s)?),
        J::Array(items) => {
            let t = lua.create_table()?;
            for (i, item) in items.into_iter().enumerate() {
                t.set(i + 1, to_lua(lua, item)?)?;
            }
            LuaValue::Table(t)
        }
        J::Object(map) => {
            let t = lua.create_table()?;
            for (k, val) in map {
                t.set(k, to_lua(lua, val)?)?;
            }
            LuaValue::Table(t)
        }
    })
}

fn to_json(v: LuaValue) -> Result<serde_json::Value> {
    serde_json::to_value(&v).context("converting Lua value to JSON")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::Usage;

    const TMP: &str = "cordanui-lua-plugin-test";

    fn fixture(name: &str, main_lua: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(TMP).join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("main.lua"), main_lua).unwrap();
        dir
    }

    fn cleanup(name: &str) {
        let _ = std::fs::remove_dir_all(std::env::temp_dir().join(TMP).join(name));
    }

    #[tokio::test]
    async fn complete_round_trip() {
        let dir = fixture(
            "echo",
            r#"
plugin = {}
function plugin.complete(req)
  return {
    content = "echo:" .. req.prompt .. ":" .. req.model,
    usage = { prompt_tokens = 1, completion_tokens = 2, total_tokens = 3 },
  }
end
"#,
        );
        let plugin = LuaPlugin::load(&dir, "echo", None, None, None).unwrap();
        let resp = plugin
            .complete(&CompleteRequest {
                model: "test-model".into(),
                prompt: "hello".into(),
                system: None,
                max_tokens: None,
                temperature: None,
                config: None,
            })
            .await
            .unwrap();
        assert_eq!(resp.content, "echo:hello:test-model");
        assert_eq!(
            resp.usage,
            Some(Usage {
                prompt_tokens: 1,
                completion_tokens: 2,
                total_tokens: Some(3)
            })
        );
        cleanup("echo");
    }

    #[tokio::test]
    async fn config_injection() {
        let dir = fixture(
            "cfg",
            r#"
plugin = {}
function plugin.complete(req)
  return { content = "key=" .. (cordanui.config.api_key or "MISSING") }
end
"#,
        );
        let config = serde_json::json!({ "api_key": "sk-test-123" });
        let plugin = LuaPlugin::load(&dir, "cfg", Some(config), None, None).unwrap();
        let resp = plugin
            .complete(&CompleteRequest {
                model: "m".into(),
                prompt: "p".into(),
                system: None,
                max_tokens: None,
                temperature: None,
                config: None,
            })
            .await
            .unwrap();
        assert_eq!(resp.content, "key=sk-test-123");
        cleanup("cfg");
    }

    #[tokio::test]
    async fn agent_run_streams_events() {
        // Local HTTP server so the script's http.request round-trips for real.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (mut sock, _) = listener.accept().unwrap();
            let mut buf = [0u8; 1024];
            let _ = std::io::Read::read(&mut sock, &mut buf); // read the request head
            let body = br#"{"ok":true}"#;
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                String::from_utf8_lossy(body)
            );
            use std::io::Write;
            let _ = sock.write_all(resp.as_bytes());
        });

        let dir = fixture(
            "stream",
            &format!(
                r#"
plugin = {{}}
function plugin.agent_run(cfg, emit)
  emit({{ type = "progress", message = "checking upstream" }})
  local res = cordanui.http.request({{ url = "http://127.0.0.1:{port}/", method = "GET" }})
  assert(res.status == 200, "expected 200, got " .. tostring(res.status))
  local body = cordanui.json.decode(res.body)
  emit({{ type = "progress", message = "upstream said ok=" .. tostring(body.ok) }})
  emit({{ type = "result", content = "done for task " .. cfg.task_id, files = cordanui.array({{}}) }})
end
"#
            ),
        );

        let plugin = LuaPlugin::load(&dir, "stream", None, None, None).unwrap();

        let messages = Arc::new(Mutex::new(Vec::new()));
        let sink = messages.clone();
        let result = plugin
            .agent_run(
                &AgentRunConfig {
                    task_id: "t-42".into(),
                    title: "Test".into(),
                    description: None,
                    model: None,
                    config: None,
                },
                move |e| {
                    if let AgentEvent::Progress { message, .. } = e {
                        sink.lock().unwrap().push(message.clone());
                    }
                },
            )
            .await
            .unwrap();

        server.join().unwrap();
        assert_eq!(
            *messages.lock().unwrap(),
            vec!["checking upstream", "upstream said ok=true"]
        );
        match result {
            AgentEvent::Result(r) => {
                assert_eq!(r.content, "done for task t-42");
                assert!(r.files.is_empty());
            }
            other => panic!("expected Result, got {:?}", other.event_type()),
        }
        cleanup("stream");
    }

    #[tokio::test]
    async fn missing_entry_script_is_a_clean_error() {
        let dir = std::env::temp_dir().join(TMP).join("empty");
        std::fs::create_dir_all(&dir).unwrap();
        let err = LuaPlugin::load(&dir, "empty", None, None, None).unwrap_err();
        assert!(err.to_string().contains("main.lua"));
        cleanup("empty");
    }

    #[tokio::test]
    async fn missing_terminal_event_errors() {
        let dir = fixture(
            "noterminal",
            r#"
plugin = {}
function plugin.agent_run(cfg, emit)
  emit({ type = "progress", message = "forever" })
end
"#,
        );
        let plugin = LuaPlugin::load(&dir, "noterminal", None, None, None).unwrap();
        let err = plugin
            .agent_run(
                &AgentRunConfig {
                    task_id: "t".into(),
                    title: "T".into(),
                    description: None,
                    model: None,
                    config: None,
                },
                |_| {},
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("without a result or error event"));
        cleanup("noterminal");
    }

    /// The reference provider-zen repo must parse as a Lua plugin and its
    /// main.lua must load cleanly (syntax + API references checked at load).
    #[test]
    fn reference_provider_zen_round_trips() {
        let dir =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins/provider-zen");
        let manifest = crate::PluginManifest::from_dir(&dir).unwrap();
        assert!(manifest.is_lua());
        assert!(manifest.capabilities.provider);
        assert_eq!(
            manifest.validate(),
            Vec::<String>::new(),
            "reference plugin manifest has problems"
        );
        let config = serde_json::json!({
            "api_key": "test-key",
            "base_url": "http://127.0.0.1:1", // unreachable; we only load
            "default_model": "grok-code",
        });
        let _plugin = LuaPlugin::load(&dir, &manifest.plugin.name, Some(config), None, None)
            .expect("provider-zen should load");
    }

    // ---------- cord.* styling ----------

    #[derive(Default)]
    struct MockStyles {
        persistent: Mutex<std::collections::BTreeMap<String, String>>,
        session: Mutex<std::collections::BTreeMap<String, String>>,
    }

    impl crate::style::StyleHost for MockStyles {
        fn set(&self, persistent: bool, var: &str, hex: &str) {
            let map = if persistent {
                &self.persistent
            } else {
                &self.session
            };
            map.lock().unwrap().insert(var.into(), hex.into());
        }
        fn clear(&self, persistent: bool, var: &str) {
            let map = if persistent {
                &self.persistent
            } else {
                &self.session
            };
            map.lock().unwrap().remove(var);
        }
        fn clear_all(&self, persistent: bool) {
            let map = if persistent {
                &self.persistent
            } else {
                &self.session
            };
            map.lock().unwrap().clear();
        }
        fn resolved(&self, var: &str) -> Option<String> {
            self.session
                .lock()
                .unwrap()
                .get(var)
                .cloned()
                .or_else(|| self.persistent.lock().unwrap().get(var).cloned())
        }
    }

    const STYLES_LUA: &str = r##"
plugin = {}
function plugin.complete(req)
  -- exercise the whole API surface
  cord.g.style.background("#112233")
  cord.g.style.primary("rgb(255, 136, 0)")
  cord["local"].style.background("#abcdef")
  return { content = cord.style.get("background") or "" }
end
"##;

    #[tokio::test]
    async fn cord_styling_routes_g_and_local() {
        let dir = fixture("styles", STYLES_LUA);
        let host = Arc::new(MockStyles::default());
        let plugin = LuaPlugin::load(&dir, "styles", None, Some(host.clone()), None).unwrap();

        let resp = plugin
            .complete(&CompleteRequest {
                model: "m".into(),
                prompt: "p".into(),
                system: None,
                max_tokens: None,
                temperature: None,
                config: None,
            })
            .await
            .unwrap();

        // `get` sees the session value first (local wins over global).
        assert_eq!(resp.content, "#abcdef");

        // g landed in persistent storage, normalized from rgb().
        assert_eq!(
            host.persistent.lock().unwrap().get("background"),
            Some(&"#112233".to_string())
        );
        assert_eq!(
            host.persistent.lock().unwrap().get("primary"),
            Some(&"#ff8800".to_string())
        );
        // local landed in session storage only.
        assert_eq!(
            host.session.lock().unwrap().get("background"),
            Some(&"#abcdef".to_string())
        );
        cleanup("styles");
    }

    #[tokio::test]
    async fn cord_reset_and_invalid_colors() {
        let dir = fixture(
            "styles-reset",
            r##"
plugin = {}
function plugin.complete(req)
  cord.g.style.primary("#ff0000")
  local err = select(2, pcall(function() cord["local"].style.primary("hotpink") end))
  cord.g.style.reset("primary")
  return { content = tostring(err) }
end
"##,
        );
        let host = Arc::new(MockStyles::default());
        let plugin = LuaPlugin::load(&dir, "styles-reset", None, Some(host.clone()), None).unwrap();
        let resp = plugin
            .complete(&CompleteRequest {
                model: "m".into(),
                prompt: "p".into(),
                system: None,
                max_tokens: None,
                temperature: None,
                config: None,
            })
            .await
            .unwrap();

        // invalid color raised with a helpful message
        assert!(
            resp.content.contains("invalid color 'hotpink'"),
            "unexpected: {}",
            resp.content
        );
        // reset cleared it
        assert!(host.persistent.lock().unwrap().get("primary").is_none());
        cleanup("styles-reset");
    }

    // ---------- cord.ui.* modal dialogs ----------

    /// One-shot complete with a minimal request, for tests.
    async fn complete_simple(plugin: &LuaPlugin) -> CompleteResponse {
        plugin
            .complete(&CompleteRequest {
                model: "m".into(),
                prompt: "p".into(),
                system: None,
                max_tokens: None,
                temperature: None,
                config: None,
            })
            .await
            .unwrap()
    }

    /// A host that answers every dialog with a canned response.
    #[derive(Default)]
    struct MockUi {
        next: Mutex<Option<UiResponse>>,
    }

    impl MockUi {
        fn answering(response: UiResponse) -> Arc<Self> {
            Arc::new(Self {
                next: Mutex::new(Some(response)),
            })
        }
    }

    impl crate::ui::UiHost for MockUi {
        fn submit(&self, pending: crate::ui::PendingUi) {
            let canned = self.next.lock().unwrap().take();
            // No canned answer left: respond with each dialog kind's
            // documented cancel value.
            let response = canned.unwrap_or_else(|| match &pending.request {
                UiRequest::Input { .. } | UiRequest::Text { .. } => UiResponse::Text(None),
                UiRequest::Confirm { .. } => UiResponse::Confirmed(false),
                UiRequest::Pick { .. } => UiResponse::Choice(None),
                UiRequest::MultiSelect { .. } => UiResponse::Choices(None),
            });
            let _ = pending.respond.send(response);
        }
    }

    #[tokio::test]
    async fn ui_input_returns_text() {
        let dir = fixture(
            "ui-input",
            r##"
plugin = {}
function plugin.complete(req)
  local name = cord.ui.input{ title = "Goal name", placeholder = "what?" }
  if name == nil then return { content = "cancelled" } end
  return { content = "typed:" .. name }
end
"##,
        );
        let plugin = LuaPlugin::load(
            &dir,
            "ui-input",
            None,
            None,
            Some(MockUi::answering(UiResponse::Text(Some("hello".into())))),
        )
        .unwrap();
        let resp = complete_simple(&plugin).await;
        assert_eq!(resp.content, "typed:hello");
        cleanup("ui-input");
    }

    #[tokio::test]
    async fn ui_cancel_resolves_to_nil() {
        // The mock answers the first dialog with Text(None) (cancel), then
        // falls through to its default Text(None) for the rest.
        let dir = fixture(
            "ui-cancel",
            r##"
plugin = {}
function plugin.complete(req)
  local a = cord.ui.input{ title = "t" }
  local ok = cord.ui.confirm{ title = "t", message = "m" }
  local idx = cord.ui.pick{ title = "t", items = { "x", "y" } }
  local desc = (a == nil and "nil" or a) .. "/" .. tostring(ok) .. "/" .. tostring(idx)
  return { content = desc }
end
"##,
        );
        let plugin = LuaPlugin::load(
            &dir,
            "ui-cancel",
            None,
            None,
            Some(MockUi::answering(UiResponse::Text(None))),
        )
        .unwrap();
        let resp = complete_simple(&plugin).await;
        // input -> nil; confirm -> false; pick -> nil (all cancels)
        assert_eq!(resp.content, "nil/false/nil");
        cleanup("ui-cancel");
    }

    #[tokio::test]
    async fn ui_pick_returns_one_based_index() {
        let dir = fixture(
            "ui-pick",
            r##"
plugin = {}
function plugin.complete(req)
  local items = { "grok-code", "claude-sonnet-4-5", "gpt-5" }
  local idx = cord.ui.pick{ title = "Model", items = items }
  return { content = items[idx] }
end
"##,
        );
        let plugin = LuaPlugin::load(
            &dir,
            "ui-pick",
            None,
            None,
            Some(MockUi::answering(UiResponse::Choice(Some(1)))),
        )
        .unwrap();
        let resp = complete_simple(&plugin).await;
        assert_eq!(resp.content, "claude-sonnet-4-5");
        cleanup("ui-pick");
    }

    #[tokio::test]
    async fn ui_multiselect_text_and_notify() {
        use std::sync::Mutex as SM;

        #[derive(Default)]
        struct NotifyCapture {
            notifications: SM<Vec<(String, String)>>,
        }
        impl crate::ui::UiHost for NotifyCapture {
            fn submit(&self, _pending: crate::ui::PendingUi) {
                unreachable!("notify test never opens a dialog");
            }
            fn notify(&self, level: crate::ui::UiLevel, message: String) {
                self.notifications
                    .lock()
                    .unwrap()
                    .push((level.as_str().to_string(), message));
            }
        }

        // A queue-based host: answers dialogs in order, then falls back to
        // each kind's cancel default. Also captures notify() calls.
        #[derive(Default)]
        struct QueueUi {
            queue: SM<Vec<UiResponse>>,
            notifications: SM<Vec<(String, String)>>,
        }
        impl crate::ui::UiHost for QueueUi {
            fn submit(&self, pending: crate::ui::PendingUi) {
                let canned = self.queue.lock().unwrap().pop();
                let response = canned.unwrap_or_else(|| match &pending.request {
                    UiRequest::Input { .. } | UiRequest::Text { .. } => UiResponse::Text(None),
                    UiRequest::Confirm { .. } => UiResponse::Confirmed(false),
                    UiRequest::Pick { .. } => UiResponse::Choice(None),
                    UiRequest::MultiSelect { .. } => UiResponse::Choices(None),
                });
                let _ = pending.respond.send(response);
            }
            fn notify(&self, level: crate::ui::UiLevel, message: String) {
                self.notifications
                    .lock()
                    .unwrap()
                    .push((level.as_str().to_string(), message));
            }
        }

        let dir = fixture(
            "ui-more",
            r##"
plugin = {}
function plugin.complete(req)
  -- multiselect: two of three preselected (1-based), answered {2,3} 1-based
  local picked = cord.ui.multiselect{ title = "Tags", items = { "a", "b", "c" }, selected = { 1 } }
  local sum = 0
  for _, i in ipairs(picked or {}) do sum = sum + i end

  -- multiline text round trip
  local body = cord.ui.text{ title = "Body", prefill = "line1\nline2" }

  -- fire-and-forget notifications
  cord.ui.notify("plain string")
  cord.ui.notify{ message = "careful", level = "warn" }

  return { content = sum .. "|" .. tostring(body) }
end
"##,
        );
        let host = Arc::new(QueueUi::default());
        // Popped LIFO — push in reverse answer order.
        (*host.queue.lock().unwrap()).push(UiResponse::Text(Some("line1\nline2".into())));
        (*host.queue.lock().unwrap()).push(UiResponse::Choices(Some(vec![1, 2])));
        let plugin = LuaPlugin::load(&dir, "ui-more", None, None, Some(host.clone() as _)).unwrap();

        let resp = complete_simple(&plugin).await;

        assert_eq!(resp.content, "5|line1\nline2");
        let notes = host.notifications.lock().unwrap();
        assert_eq!(notes.len(), 2);
        assert_eq!(notes[0], ("info".to_string(), "plain string".to_string()));
        assert_eq!(notes[1], ("warn".to_string(), "careful".to_string()));
        cleanup("ui-more");
    }

    #[tokio::test]
    async fn ui_refusal_is_a_lua_error() {
        let dir = fixture(
            "ui-refused",
            r##"
plugin = {}
function plugin.complete(req)
  local ok, err = pcall(function()
    return cord.ui.input{ title = "t" }
  end)
  return { content = tostring(err) }
end
"##,
        );
        let plugin = LuaPlugin::load(
            &dir,
            "ui-refused",
            None,
            None,
            Some(MockUi::answering(UiResponse::Refused(
                "another dialog is open".into(),
            ))),
        )
        .unwrap();
        let resp = complete_simple(&plugin).await;
        assert!(
            resp.content.contains("refused the dialog"),
            "unexpected: {}",
            resp.content
        );
        cleanup("ui-refused");
    }

    #[tokio::test]
    async fn ui_without_host_errors_cleanly() {
        let dir = fixture(
            "ui-nohost",
            r##"
plugin = {}
function plugin.complete(req)
  local ok, err = pcall(function()
    return cord.ui.pick{ title = "t", items = { "a" } }
  end)
  return { content = tostring(err) }
end
"##,
        );
        let plugin = LuaPlugin::load(&dir, "ui-nohost", None, None, None).unwrap();
        let resp = complete_simple(&plugin).await;
        assert!(
            resp.content.contains("not available"),
            "unexpected: {}",
            resp.content
        );
        cleanup("ui-nohost");
    }
}
