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
use crate::ui::{
    PanelSpec, SharedBuffersHost, SharedConfigHost, SharedErrorLogHost, SharedPanelHost,
    SharedServiceHost, SharedSheetsHost, SharedUiHost, UiLevel, UiRequest, UiResponse, Widget,
};
use anyhow::{bail, Context, Result};
use mlua::{Function, Lua, LuaSerdeExt, Table, Value, Value as LuaValue};

/// A command registered by a plugin via `plugin.commands`.
#[derive(Debug, Clone)]
pub struct CommandInfo {
    pub name: String,
    pub desc: String,
}

/// Host capabilities handed to a plugin at load time. Everything is
/// optional: absent surfaces make the corresponding `cord.*` calls error
/// cleanly instead of existing silently.
#[derive(Default, Clone)]
pub struct HostHooks {
    pub styles: Option<SharedStyleHost>,
    pub ui: Option<SharedUiHost>,
    pub panels: Option<SharedPanelHost>,
    pub config: Option<SharedConfigHost>,
    pub services: Option<SharedServiceHost>,
    pub errors: Option<SharedErrorLogHost>,
    pub sheets: Option<SharedSheetsHost>,
    pub buffers: Option<SharedBuffersHost>,
}

impl HostHooks {
    /// No host capabilities attached.
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_styles(mut self, styles: SharedStyleHost) -> Self {
        self.styles = Some(styles);
        self
    }

    pub fn with_ui(mut self, ui: SharedUiHost) -> Self {
        self.ui = Some(ui);
        self
    }

    pub fn with_panels(mut self, panels: SharedPanelHost) -> Self {
        self.panels = Some(panels);
        self
    }

    pub fn with_config(mut self, config: SharedConfigHost) -> Self {
        self.config = Some(config);
        self
    }

    pub fn with_services(mut self, services: SharedServiceHost) -> Self {
        self.services = Some(services);
        self
    }

    pub fn with_errors(mut self, errors: SharedErrorLogHost) -> Self {
        self.errors = Some(errors);
        self
    }

    pub fn with_sheets(mut self, sheets: SharedSheetsHost) -> Self {
        self.sheets = Some(sheets);
        self
    }

    pub fn with_buffers(mut self, buffers: SharedBuffersHost) -> Self {
        self.buffers = Some(buffers);
        self
    }
}

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
        hooks: HostHooks,
    ) -> Result<Self> {
        let entry = dir.join(crate::manifest::PluginManifest::LUA_ENTRY);
        let source = std::fs::read_to_string(&entry)
            .with_context(|| format!("reading {}", entry.display()))?;

        let lua = Arc::new(Lua::new());
        register_api(&lua, dir, name, config, hooks.ui.clone())
            .context("registering cordanui API")?;
        register_cord(
            &lua,
            hooks.styles,
            hooks.ui,
            hooks.panels,
            hooks.config,
            hooks.services,
            hooks.errors,
            hooks.sheets,
            hooks.buffers,
            name,
        )
        .context("registering cord styling API")?;

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
        let has_provider = plugin.contains_key("complete")? || plugin.contains_key("agent_run")?;
        let has_commands = plugin.contains_key("commands")?;
        let has_configure = plugin.contains_key("configure")?;
        if !has_provider && !has_commands && !has_configure {
            bail!("plugin table defines neither complete, agent_run, commands, nor configure");
        }

        Ok(Self {
            lua,
            name: name.to_string(),
        })
    }

    /// Registered commands from the optional `plugin.commands` table:
    ///
    /// ```lua
    /// plugin.commands = {
    ///   ["rose-pine.select"] = { run = M.select, desc = "Pick a flavor" },
    /// }
    /// ```
    ///
    /// Entries without a `run` function are skipped. Sorted by name.
    pub fn list_commands(&self) -> Vec<CommandInfo> {
        let mut out = Vec::new();
        let Ok(commands): mlua::Result<Table> = self
            .lua
            .globals()
            .get("plugin")
            .and_then(|p: Table| p.get("commands"))
        else {
            return out;
        };
        for pair in commands.pairs::<String, Table>().flatten() {
            let (name, entry) = pair;
            if entry.get::<Function>("run").is_err() {
                continue;
            }
            let desc = entry.get::<String>("desc").unwrap_or_default();
            out.push(CommandInfo { name, desc });
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    }

    /// Invoke a registered command. The command runs to completion and may
    /// open dialogs (`cord.ui.*`) or panels while it runs. If it returns a
    /// string, the host surfaces it as a status message.
    pub async fn call_command(&self, name: &str) -> Result<Option<String>> {
        let plugin: Table = self.lua.globals().get("plugin")?;
        let commands: Table = plugin
            .get("commands")
            .context("plugin does not define a commands table")?;
        let entry: Table = commands
            .get(name)
            .with_context(|| format!("no command named '{name}'"))?;
        let run: Function = entry.get("run").context("command has no run function")?;

        match run
            .call_async::<LuaValue>(())
            .await
            .map_err(|e| anyhow::anyhow!("command '{name}' failed: {e}"))?
        {
            LuaValue::String(s) => Ok(Some(s.to_string_lossy())),
            _ => Ok(None),
        }
    }

    /// True if the plugin defines `plugin.configure` — a self-owned
    /// settings page the host opens when the user presses the configure
    /// key. Plugins without one fall back to the declarative `[[field]]`
    /// form (or nothing).
    pub fn has_configure(&self) -> bool {
        self.lua
            .globals()
            .get::<Table>("plugin")
            .and_then(|p: Table| p.get::<Function>("configure"))
            .is_ok()
    }

    /// Invoke `plugin.configure`. Like commands, this runs to completion
    /// and typically opens a panel or dialogs; a returned string becomes
    /// a host status message.
    pub async fn call_configure(&self) -> Result<Option<String>> {
        let plugin: Table = self.lua.globals().get("plugin")?;
        let configure: Function = plugin
            .get("configure")
            .context("plugin does not define configure")?;
        match configure
            .call_async::<LuaValue>(())
            .await
            .map_err(|e| anyhow::anyhow!("configure failed: {e}"))?
        {
            LuaValue::String(s) => Ok(Some(s.to_string_lossy())),
            _ => Ok(None),
        }
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
    ui: Option<crate::ui::SharedUiHost>,
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

    // cordanui.log.{info,warn,error} — warn/error also go through UiHost::notify
    // so `poll_plugin_ui_requests` can dump them to the shared `errors` table
    // like every other subsystem.
    let log = lua.create_table()?;
    log.set(
        "info",
        lua.create_function(|_, msg: String| {
            tracing::info!(target: "plugin", "{msg}");
            Ok(())
        })?,
    )?;
    {
        let ui_warn = ui.clone();
        log.set(
            "warn",
            lua.create_function(move |_, msg: String| {
                tracing::warn!(target: "plugin", "{msg}");
                if let Some(host) = ui_warn.as_ref() {
                    host.notify(crate::ui::UiLevel::Warn, msg.clone());
                }
                Ok(())
            })?,
        )?;
    }
    {
        let ui_err = ui.clone();
        log.set(
            "error",
            lua.create_function(move |_, msg: String| {
                tracing::error!(target: "plugin", "{msg}");
                if let Some(host) = ui_err.as_ref() {
                    host.notify(crate::ui::UiLevel::Error, msg.clone());
                }
                Ok(())
            })?,
        )?;
    }
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
    panels: Option<SharedPanelHost>,
    config: Option<SharedConfigHost>,
    services: Option<SharedServiceHost>,
    errors: Option<SharedErrorLogHost>,
    sheets: Option<SharedSheetsHost>,
    buffers: Option<SharedBuffersHost>,
    plugin_name: &str,
) -> mlua::Result<()> {
    let cord = lua.create_table()?;
    register_cord_config(lua, &cord, config, plugin_name)?;
    register_cord_services(lua, &cord, services)?;
    register_cord_ui(lua, &cord, ui, panels)?;
    register_cord_errors(lua, &cord, errors)?;
    register_cord_sheets(lua, &cord, sheets)?;
    register_cord_buffers(lua, &cord, buffers)?;

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

/// The `cord.services` table — lifecycle control + HTTP transport for
/// plugin-declared `[service]` processes (any language).
///
/// ```lua
/// if not cord.services.is_running("cordanui-agents") then
///   cord.services.start("cordanui-agents")
/// end
/// local res = cord.services.request("cordanui-agents", {
///   method = "POST",
///   path = "/wake",
///   body = { task_id = "abc" },
/// })
/// ```
fn register_cord_services(
    lua: &Lua,
    cord: &Table,
    services: Option<SharedServiceHost>,
) -> mlua::Result<()> {
    let api = lua.create_table()?;

    api.set(
        "start",
        lua.create_function({
            let services = services.clone();
            move |_, (name, extra): (String, Option<Vec<String>>)| {
                let Some(host) = services.as_ref() else {
                    return Err(mlua::Error::runtime(
                        "cord.services is not available in this host",
                    ));
                };
                host.start(&name, &extra.unwrap_or_default())
                    .map_err(mlua::Error::external)?;
                Ok(true)
            }
        })?,
    )?;

    api.set(
        "stop",
        lua.create_function({
            let services = services.clone();
            move |_, name: String| {
                let Some(host) = services.as_ref() else {
                    return Err(mlua::Error::runtime(
                        "cord.services is not available in this host",
                    ));
                };
                host.stop(&name).map_err(mlua::Error::external)?;
                Ok(true)
            }
        })?,
    )?;

    api.set(
        "is_running",
        lua.create_function({
            let services = services.clone();
            move |_, name: String| {
                let Some(host) = services.as_ref() else {
                    return Err(mlua::Error::runtime(
                        "cord.services is not available in this host",
                    ));
                };
                Ok(host.is_running(&name))
            }
        })?,
    )?;

    // cord.services.request(name, {method?, path, headers?, body?})
    // Addressed to the service's manifest addr/health base URL. Requires
    // the service to be running — start it first.
    api.set(
        "request",
        lua.create_async_function({
            let services = services.clone();
            let lua = lua.clone();
            move |_, (name, params): (String, Table)| {
                let services = services.clone();
                let lua = lua.clone();
                async move {
                    let Some(host) = services.as_ref() else {
                        return Err(mlua::Error::runtime(
                            "cord.services is not available in this host",
                        ));
                    };
                    if !host.is_running(&name) {
                        return Err(mlua::Error::runtime(format!(
                            "service '{name}' is not running — call cord.services.start first"
                        )));
                    }
                    let Some(base) = host.base_url(&name) else {
                        return Err(mlua::Error::runtime(format!(
                            "service '{name}' declares no addr/health url"
                        )));
                    };
                    let path: String = params.get("path").unwrap_or_else(|_| "/".into());
                    let url = format!("{}{}", base.trim_end_matches('/'), path);
                    let method: Option<String> = params.get("method").ok();
                    let headers: Option<Table> = params.get("headers").ok();
                    let json_body: Option<LuaValue> = params.get("body").ok();

                    let mut req = http_client().request(method_from(&method), &url);
                    if let Some(hs) = headers {
                        for pair in hs.pairs::<String, String>() {
                            let (k, v) = pair?;
                            req = req.header(k, v);
                        }
                    }
                    if let Some(body) = json_body {
                        let json = serde_json::to_string(&body).map_err(mlua::Error::external)?;
                        req = req.header("content-type", "application/json").body(json);
                    }
                    let resp = req.send().await.map_err(mlua::Error::external)?;
                    let status = resp.status().as_u16();
                    let text = resp.text().await.map_err(mlua::Error::external)?;
                    let out = lua.create_table()?;
                    out.set("status", status)?;
                    out.set("body", text)?;
                    Ok(out)
                }
            }
        })?,
    )?;

    cord.set("services", api)?;
    Ok(())
}

/// The `cord.config` table — namespaced settings persistence for the
/// plugin's own configuration pages.
///
/// ```lua
/// local variant = cord.config.get("variant", "moon")
/// cord.config.set("variant", "dawn")
/// ```
///
/// Keys are scoped under the plugin's name by the host; values are
/// strings stored in the shared `settings` table (same place the
/// declarative fallback form reads and writes).
/// The `cord.errors` table — read access to the host's error log.
///
/// ```lua
/// local entries = cord.errors.list(50)   -- newest first
/// for _, e in ipairs(entries) do
///   print(e.created_at, e.context, e.message)
/// end
/// cord.errors.clear()
/// ```
fn register_cord_errors(
    lua: &Lua,
    cord: &Table,
    errors: Option<SharedErrorLogHost>,
) -> mlua::Result<()> {
    let api = lua.create_table()?;

    // cord.errors.list(limit?) -> array of {created_at, context, message, detail}
    let errors_list = errors.clone();
    api.set(
        "list",
        lua.create_function(move |lua, limit: Option<u32>| {
            let Some(host) = errors_list.as_ref() else {
                return Err(mlua::Error::runtime(
                    "cord.errors is not available in this host",
                ));
            };
            let entries = host.list(limit.unwrap_or(200));
            let out = lua.create_table()?;
            for (i, e) in entries.iter().enumerate() {
                let row = lua.create_table()?;
                row.set("created_at", e.created_at.clone())?;
                row.set("context", e.context.clone())?;
                row.set("message", e.message.clone())?;
                row.set("detail", e.detail.clone())?; // nil when absent
                out.set(i + 1, row)?;
            }
            Ok(out)
        })?,
    )?;

    // cord.errors.clear() -> true
    let errors_clear = errors.clone();
    api.set(
        "clear",
        lua.create_function(move |_, ()| {
            let Some(host) = errors_clear.as_ref() else {
                return Err(mlua::Error::runtime(
                    "cord.errors is not available in this host",
                ));
            };
            host.clear();
            Ok(true)
        })?,
    )?;

    cord.set("errors", api)?;
    Ok(())
}

fn register_cord_sheets(
    lua: &Lua,
    cord: &Table,
    sheets: Option<SharedSheetsHost>,
) -> mlua::Result<()> {
    let api = lua.create_table()?;

    // cord.sheets.list() -> [{id, name}]
    let sheets_list = sheets.clone();
    api.set(
        "list",
        lua.create_function(move |lua, ()| {
            let Some(host) = sheets_list.as_ref() else {
                return Err(mlua::Error::runtime("cord.sheets is not available in this host"));
            };
            let list = host.list_sheets();
            let out = lua.create_table()?;
            for (i, s) in list.iter().enumerate() {
                let row = lua.create_table()?;
                row.set("id", s.id.clone())?;
                row.set("name", s.name.clone())?;
                out.set(i + 1, row)?;
            }
            Ok(out)
        })?,
    )?;

    // cord.sheets.create(name) -> id
    let sheets_create = sheets.clone();
    api.set(
        "create",
        lua.create_function(move |_, name: String| {
            let Some(host) = sheets_create.as_ref() else {
                return Err(mlua::Error::runtime("cord.sheets is not available in this host"));
            };
            let id = host.create_sheet(&name).map_err(|e| mlua::Error::runtime(e.to_string()))?;
            Ok(id)
        })?,
    )?;

    // cord.sheets.delete(id) -> true
    let sheets_delete = sheets.clone();
    api.set(
        "delete",
        lua.create_function(move |_, id: String| {
            let Some(host) = sheets_delete.as_ref() else {
                return Err(mlua::Error::runtime("cord.sheets is not available in this host"));
            };
            host.delete_sheet(&id).map_err(|e| mlua::Error::runtime(e.to_string()))?;
            Ok(true)
        })?,
    )?;

    // cord.sheets.select(id|nil) -> true
    let sheets_select = sheets.clone();
    api.set(
        "select",
        lua.create_function(move |_, id: Option<String>| {
            let Some(host) = sheets_select.as_ref() else {
                return Err(mlua::Error::runtime("cord.sheets is not available in this host"));
            };
            host.select_sheet(id).map_err(|e| mlua::Error::runtime(e.to_string()))?;
            Ok(true)
        })?,
    )?;

    // cord.sheets.current() -> id|nil
    let sheets_current = sheets.clone();
    api.set(
        "current",
        lua.create_function(move |_, ()| {
            let Some(host) = sheets_current.as_ref() else {
                return Err(mlua::Error::runtime("cord.sheets is not available in this host"));
            };
            Ok(host.current_sheet())
        })?,
    )?;

    cord.set("sheets", api)?;
    Ok(())
}

fn register_cord_buffers(
    lua: &Lua,
    cord: &Table,
    buffers: Option<SharedBuffersHost>,
) -> mlua::Result<()> {
    let api = lua.create_table()?;

    // cord.buffers.create{ name, draw, on_key } -> id
    // draw: function()-> widget, on_key: function(key)->bool (optional)
    let buffers_create = buffers.clone();
    api.set(
        "create",
        lua.create_function(move |_, params: Table| {
            let Some(host) = buffers_create.as_ref() else {
                return Err(mlua::Error::runtime("cord.buffers is not available in this host"));
            };
            let name: String = params.get("name").map_err(|_| mlua::Error::runtime("buffers.create needs name"))?;
            let draw_fn: Function = params.get("draw").map_err(|_| mlua::Error::runtime("buffers.create needs draw function"))?;
            let on_key_fn: Option<Function> = params.get("on_key").ok();
            let draw_fn_for_draw = draw_fn.clone();
            let draw_fn_arc = std::sync::Arc::new(std::sync::Mutex::new(draw_fn_for_draw));
            let draw: std::sync::Arc<dyn Fn() -> Widget + Send + Sync> = std::sync::Arc::new(move || -> Widget {
                let f = draw_fn_arc.lock().unwrap().clone();
                match f.call::<LuaValue>(()) {
                    Ok(v) => Widget::from_lua(&v).ok().flatten().unwrap_or_else(Widget::empty),
                    Err(e) => {
                        tracing::error!(target: "plugin", "buffer draw failed: {e}");
                        Widget::empty()
                    }
                }
            });
            let on_key_fn_arc = on_key_fn.map(|f| std::sync::Arc::new(std::sync::Mutex::new(f)));
            let on_key: std::sync::Arc<dyn Fn(&str) -> bool + Send + Sync> = std::sync::Arc::new(move |key: &str| -> bool {
                on_key_fn_arc
                    .as_ref()
                    .and_then(|arc| arc.lock().unwrap().call::<bool>(key).ok())
                    .unwrap_or(false)
            });
            let spec = crate::ui::PanelSpec {
                title: name.clone(),
                draw,
                on_key,
            };
            let id = host.create_buffer(name, spec);
            Ok(id)
        })?,
    )?;

    // cord.buffers.list() -> [id]
    let buffers_list = buffers.clone();
    api.set(
        "list",
        lua.create_function(move |lua, ()| {
            let Some(host) = buffers_list.as_ref() else {
                return Err(mlua::Error::runtime("cord.buffers is not available in this host"));
            };
            let list = host.list_buffers();
            let out = lua.create_table()?;
            for (i, id) in list.iter().enumerate() {
                out.set(i + 1, id.clone())?;
            }
            Ok(out)
        })?,
    )?;

    // cord.buffers.select(id|nil) -> true
    let buffers_select = buffers.clone();
    api.set(
        "select",
        lua.create_function(move |_, id: Option<String>| {
            let Some(host) = buffers_select.as_ref() else {
                return Err(mlua::Error::runtime("cord.buffers is not available in this host"));
            };
            host.select_buffer(id);
            Ok(true)
        })?,
    )?;

    // cord.buffers.remove(id) -> true
    let buffers_remove = buffers.clone();
    api.set(
        "remove",
        lua.create_function(move |_, id: String| {
            let Some(host) = buffers_remove.as_ref() else {
                return Err(mlua::Error::runtime("cord.buffers is not available in this host"));
            };
            host.remove_buffer(&id);
            Ok(true)
        })?,
    )?;

    // cord.buffers.current() -> id|nil
    let buffers_current = buffers.clone();
    api.set(
        "current",
        lua.create_function(move |_, ()| {
            let Some(host) = buffers_current.as_ref() else {
                return Err(mlua::Error::runtime("cord.buffers is not available in this host"));
            };
            Ok(host.current_buffer())
        })?,
    )?;

    // cord.buffers.update(id, {draw, on_key}) -> true
    let buffers_update = buffers.clone();
    api.set(
        "update",
        lua.create_function(move |_, (id, params): (String, Table)| {
            let Some(host) = buffers_update.as_ref() else {
                return Err(mlua::Error::runtime("cord.buffers is not available in this host"));
            };
            let draw_fn: Option<Function> = params.get("draw").ok();
            let on_key_fn: Option<Function> = params.get("on_key").ok();
            // If no draw, keep existing; but for simplicity require draw
            let draw_fn = draw_fn.ok_or_else(|| mlua::Error::runtime("buffers.update needs draw"))?;
            let draw_fn_for_draw = draw_fn.clone();
            let draw_fn_arc = std::sync::Arc::new(std::sync::Mutex::new(draw_fn_for_draw));
            let draw: std::sync::Arc<dyn Fn() -> Widget + Send + Sync> = std::sync::Arc::new(move || -> Widget {
                let f = draw_fn_arc.lock().unwrap().clone();
                match f.call::<LuaValue>(()) {
                    Ok(v) => Widget::from_lua(&v).ok().flatten().unwrap_or_else(Widget::empty),
                    Err(e) => {
                        tracing::error!(target: "plugin", "buffer draw failed: {e}");
                        Widget::empty()
                    }
                }
            });
            let on_key_fn_arc = on_key_fn.map(|f| std::sync::Arc::new(std::sync::Mutex::new(f)));
            let on_key: std::sync::Arc<dyn Fn(&str) -> bool + Send + Sync> = std::sync::Arc::new(move |key: &str| -> bool {
                on_key_fn_arc
                    .as_ref()
                    .and_then(|arc| arc.lock().unwrap().call::<bool>(key).ok())
                    .unwrap_or(false)
            });
            let spec = crate::ui::PanelSpec {
                title: id.clone(),
                draw,
                on_key,
            };
            host.update_buffer(&id, spec).map_err(|e| mlua::Error::runtime(e.to_string()))?;
            Ok(true)
        })?,
    )?;

    cord.set("buffers", api)?;
    Ok(())
}

fn register_cord_config(
    lua: &Lua,
    cord: &Table,
    config: Option<SharedConfigHost>,
    plugin_name: &str,
) -> mlua::Result<()> {
    let api = lua.create_table()?;

    // cord.config.get(key, default?) -> string | nil
    api.set(
        "get",
        lua.create_function({
            let config = config.clone();
            let name = plugin_name.to_string();
            move |_, (key, default): (String, Option<String>)| {
                let Some(host) = config.as_ref() else {
                    return Err(mlua::Error::runtime(
                        "cord.config is not available in this host",
                    ));
                };
                Ok(host.get(&name, &key).or(default))
            }
        })?,
    )?;

    // cord.config.set(key, value) -> true
    api.set(
        "set",
        lua.create_function({
            let config = config.clone();
            let name = plugin_name.to_string();
            move |_, (key, value): (String, String)| {
                let Some(host) = config.as_ref() else {
                    return Err(mlua::Error::runtime(
                        "cord.config is not available in this host",
                    ));
                };
                host.set(&name, &key, &value);
                Ok(true)
            }
        })?,
    )?;

    cord.set("config", api)?;
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
fn register_cord_ui(
    lua: &Lua,
    cord: &Table,
    ui: Option<SharedUiHost>,
    panels: Option<SharedPanelHost>,
) -> mlua::Result<()> {
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

    // cord.ui.show_panel{title?, draw = fn, on_key = fn?} -> true
    // Opens a persistent panel. draw() returns a widget tree each frame;
    // on_key(keyname) -> bool (true = handled). Returns immediately; the
    // panel lives until closed.
    api.set(
        "show_panel",
        lua.create_function({
            let panels = panels.clone();
            move |_, params: Table| {
                let Some(host) = panels.as_ref() else {
                    return Err(mlua::Error::runtime(
                        "cord.ui is not available in this host",
                    ));
                };
                let title: String = params.get("title").unwrap_or_default();
                let draw_fn: Function = params
                    .get("draw")
                    .map_err(|_| mlua::Error::runtime("show_panel needs a draw function"))?;
                let on_key_fn: Option<Function> = params.get("on_key").ok();

                let draw_fn_for_draw = draw_fn.clone();
                let draw_fn_arc = std::sync::Arc::new(std::sync::Mutex::new(draw_fn_for_draw));
                let draw: std::sync::Arc<dyn Fn() -> Widget + Send + Sync> = std::sync::Arc::new(move || -> Widget {
                    let f = draw_fn_arc.lock().unwrap().clone();
                    match f.call::<LuaValue>(()) {
                        Ok(v) => Widget::from_lua(&v)
                            .ok()
                            .flatten()
                            .unwrap_or_else(Widget::empty),
                        Err(e) => {
                            tracing::error!(target: "plugin", "panel draw failed: {e}");
                            Widget::empty()
                        }
                    }
                });

                let on_key_fn_arc = on_key_fn.map(|f| std::sync::Arc::new(std::sync::Mutex::new(f)));
                let on_key: std::sync::Arc<dyn Fn(&str) -> bool + Send + Sync> = std::sync::Arc::new(move |key: &str| -> bool {
                    on_key_fn_arc
                        .as_ref()
                        .and_then(|arc| arc.lock().unwrap().call::<bool>(key).ok())
                        .unwrap_or(false)
                });

                host.open_panel(PanelSpec {
                    title,
                    draw,
                    on_key,
                });
                Ok(true)
            }
        })?,
    )?;

    // cord.ui.close_panel() -> true
    api.set(
        "close_panel",
        lua.create_function({
            let panels = panels.clone();
            move |_, ()| {
                let Some(host) = panels.as_ref() else {
                    return Err(mlua::Error::runtime(
                        "cord.ui is not available in this host",
                    ));
                };
                host.close_panel();
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
        let plugin = LuaPlugin::load(&dir, "echo", None, HostHooks::new()).unwrap();
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
        let plugin = LuaPlugin::load(&dir, "cfg", Some(config), HostHooks::new()).unwrap();
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

        let plugin = LuaPlugin::load(&dir, "stream", None, HostHooks::new()).unwrap();

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
        let err = LuaPlugin::load(&dir, "empty", None, HostHooks::new()).unwrap_err();
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
        let plugin = LuaPlugin::load(&dir, "noterminal", None, HostHooks::new()).unwrap();
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
        let _plugin = LuaPlugin::load(&dir, &manifest.plugin.name, Some(config), HostHooks::new())
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
        let plugin = LuaPlugin::load(
            &dir,
            "styles",
            None,
            HostHooks::new().with_styles(host.clone()),
        )
        .unwrap();

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
        let plugin = LuaPlugin::load(
            &dir,
            "styles-reset",
            None,
            HostHooks::new().with_styles(host.clone()),
        )
        .unwrap();
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

    /// Combined style+ui(+panel) test host with an answer queue. Popped
    /// LIFO; empty queue falls back to each dialog kind's cancel value.
    #[derive(Default)]
    struct QueueHost {
        queue: std::sync::Mutex<Vec<UiResponse>>,
        persistent: std::sync::Mutex<std::collections::BTreeMap<String, String>>,
        session: std::sync::Mutex<std::collections::BTreeMap<String, String>>,
        notifications: std::sync::Mutex<Vec<(String, String)>>,
    }
    impl crate::style::StyleHost for QueueHost {
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
            self.session.lock().unwrap().get(var).cloned()
        }
    }
    impl crate::ui::UiHost for QueueHost {
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
    impl crate::ui::PanelHost for QueueHost {
        fn open_panel(&self, _spec: crate::ui::PanelSpec) {}
        fn close_panel(&self) {}
    }
    impl crate::ui::ConfigHost for QueueHost {
        fn get(&self, _plugin: &str, key: &str) -> Option<String> {
            self.persistent.lock().unwrap().get(key).cloned()
        }
        fn set(&self, _plugin: &str, key: &str, value: &str) {
            self.persistent
                .lock()
                .unwrap()
                .insert(key.into(), value.into());
        }
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
            HostHooks::new().with_ui(MockUi::answering(UiResponse::Text(Some("hello".into())))),
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
            HostHooks::new().with_ui(MockUi::answering(UiResponse::Text(None))),
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
            HostHooks::new().with_ui(MockUi::answering(UiResponse::Choice(Some(1)))),
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
        let plugin = LuaPlugin::load(
            &dir,
            "ui-more",
            None,
            HostHooks::new().with_ui(host.clone() as _),
        )
        .unwrap();

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
            HostHooks::new().with_ui(MockUi::answering(UiResponse::Refused(
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
        let plugin = LuaPlugin::load(&dir, "ui-nohost", None, HostHooks::new()).unwrap();
        let resp = complete_simple(&plugin).await;
        assert!(
            resp.content.contains("not available"),
            "unexpected: {}",
            resp.content
        );
        cleanup("ui-nohost");
    }

    // ---------- cord.ui.show_panel ----------

    #[derive(Default)]
    struct CapturedPanel {
        spec: Mutex<Option<crate::ui::PanelSpec>>,
        closed: std::sync::atomic::AtomicBool,
    }
    impl crate::ui::PanelHost for CapturedPanel {
        fn open_panel(&self, spec: crate::ui::PanelSpec) {
            *self.spec.lock().unwrap() = Some(spec);
        }
        fn close_panel(&self) {
            self.closed
                .store(true, std::sync::atomic::Ordering::Relaxed);
        }
    }

    #[test]
    fn ui_panel_draw_and_key_round_trip() {
        use super::*; // HostHooks in scope via crate::lua; Widget via plugin_runtime root

        let dir = fixture(
            "panel",
            r##"
plugin = {}
local sel = 1
function plugin.complete(req)
  cord.ui.show_panel{
    title = "My dashboard",
    draw = function()
      return {
        { content = "sel=" .. sel },
        { items = { "one", "two", "three" }, highlight = sel },
      }
    end,
    on_key = function(key)
      if key == "down" then sel = math.min(sel + 1, 3); return true end
      if key == "up" then sel = math.max(sel - 1, 1); return true end
      if key == "q" then cord.ui.close_panel(); return true end
      return false
    end,
  }
  return { content = "closed" }
end
"##,
        );
        let host = std::sync::Arc::new(CapturedPanel::default());
        let plugin = LuaPlugin::load(
            &dir,
            "panel",
            None,
            HostHooks::new().with_panels(host.clone()),
        )
        .unwrap();

        // Drive complete() to completion on this thread's runtime; the
        // panel call itself is non-blocking, so complete() finishes only
        // when the script's function returns.
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let resp = rt.block_on(async {
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
        });
        assert_eq!(resp.content, "closed");

        let spec = host.spec.lock().unwrap().take().expect("panel opened");
        assert_eq!(spec.title, "My dashboard");

        // Frame 1: highlight on item 0.
        match (spec.draw)() {
            Widget::Column { children } => {
                assert!(matches!(&children[0],
                    Widget::Text { content, .. } if content == "sel=1"));
                assert!(matches!(
                    &children[1],
                    Widget::List {
                        highlight: Some(0),
                        ..
                    }
                ));
            }
            other => panic!("expected column, got {other:?}"),
        }

        // Keys mutate plugin state; unhandled keys report pass-through.
        assert!(!(spec.on_key)("left"));
        assert!((spec.on_key)("down"));
        assert!((spec.on_key)("down"));

        // Frame 2 after two downs: highlight moved to index 2.
        match (spec.draw)() {
            Widget::Column { children } => {
                assert!(matches!(&children[1],
                    Widget::List { highlight: Some(2), items } if items.len() == 3));
            }
            other => panic!("expected column, got {other:?}"),
        }

        // Plugin closes its own panel.
        assert!((spec.on_key)("q"));
        assert!(host.closed.load(std::sync::atomic::Ordering::Relaxed));
        cleanup("panel");
    }

    // ---------- plugin.commands ----------

    #[test]
    fn commands_list_and_invoke_with_dialog() {
        let dir = fixture(
            "commands",
            r##"
plugin = {}
local M = {}

function M.select()
  local flavors = { "rose-pine", "moon", "dawn" }
  local idx = cord.ui.pick{ title = "Flavor", items = flavors }
  if not idx then return "cancelled" end
  cord.g.style.primary("#ebbcba")
  return "switched to " .. flavors[idx]
end

plugin.commands = {
  ["rose-pine.select"] = { run = M.select, desc = "Pick a flavor" },
  ["broken.no-run"] = { desc = "no run fn - must be skipped" },
}
"##,
        );
        let host = Arc::new(QueueHost::default());
        (*host.queue.lock().unwrap()).push(UiResponse::Choice(Some(1)));
        let plugin = LuaPlugin::load(
            &dir,
            "commands",
            None,
            HostHooks::new()
                .with_styles(host.clone())
                .with_ui(host.clone()),
        )
        .unwrap();

        // Registry listing: the run-less entry is skipped, sorted by name.
        let cmds = plugin.list_commands();
        assert_eq!(cmds.len(), 1);
        assert_eq!(cmds[0].name, "rose-pine.select");
        assert_eq!(cmds[0].desc, "Pick a flavor");

        // Invoke: the awaited pick is answered by the mock queue.
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let msg = rt
            .block_on(plugin.call_command("rose-pine.select"))
            .unwrap();
        assert_eq!(msg.as_deref(), Some("switched to moon"));
        // The command restyled through cord.g.
        assert_eq!(
            host.persistent.lock().unwrap().get("primary"),
            Some(&"#ebbcba".to_string())
        );

        // Unknown names are clean errors.
        let err = rt
            .block_on(plugin.call_command("rose-pine.does-not-exist"))
            .unwrap_err();
        assert!(err.to_string().contains("no command named"), "{err}");
        cleanup("commands");
    }

    // ---------- plugin.configure + cord.config ----------

    #[test]
    fn configure_entry_point_with_config_persistence() {
        let dir = fixture(
            "configure",
            r##"
plugin = {}

function plugin.configure()
  local current = cord.config.get("variant", "moon")
  local idx = cord.ui.pick{ title = "Variant", items = { "main", "moon", "dawn" } }
  if not idx then return "cancelled" end
  local chosen = ({ "main", "moon", "dawn" })[idx]
  cord.config.set("variant", chosen)
  return "variant = " .. chosen .. " (was " .. current .. ")"
end
"##,
        );
        let host = Arc::new(QueueHost::default());
        (*host.queue.lock().unwrap()).push(UiResponse::Choice(Some(2))); // dawn
        let plugin = LuaPlugin::load(
            &dir,
            "configure",
            None,
            HostHooks::new()
                .with_ui(host.clone())
                .with_config(host.clone()),
        )
        .unwrap();

        assert!(plugin.has_configure());
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let msg = rt.block_on(plugin.call_configure()).unwrap();
        assert_eq!(msg.as_deref(), Some("variant = dawn (was moon)"));
        // cord.config.set persisted under the plugin's namespace.
        assert_eq!(
            host.persistent.lock().unwrap().get("variant"),
            Some(&"dawn".to_string())
        );
        cleanup("configure");
    }

    // ---------- cord.services ----------

    #[test]
    fn services_lifecycle_and_request() {
        use crate::ui::ServiceHost;
        use std::sync::atomic::{AtomicBool, Ordering};

        #[derive(Default)]
        struct MockServices {
            running: AtomicBool,
        }
        impl crate::ui::ServiceHost for MockServices {
            fn start(&self, _plugin: &str, _extra: &[String]) -> anyhow::Result<()> {
                self.running.store(true, Ordering::Relaxed);
                Ok(())
            }
            fn stop(&self, _plugin: &str) -> anyhow::Result<()> {
                self.running.store(false, Ordering::Relaxed);
                Ok(())
            }
            fn is_running(&self, _plugin: &str) -> bool {
                self.running.load(Ordering::Relaxed)
            }
            fn base_url(&self, _plugin: &str) -> Option<String> {
                Some("http://127.0.0.1:18099".into())
            }
        }

        let dir = fixture(
            "services",
            r##"
plugin = {}
function plugin.complete(req)
  assert(not cord.services.is_running("cordanui-agents"))
  cord.services.start("cordanui-agents")
  assert(cord.services.is_running("cordanui-agents"))

  -- request while running: hits the (mock) base url — connection to a
  -- dead port errors, which proves the transport was addressed.
  local ok, err = pcall(function()
    return cord.services.request("cordanui-agents", { path = "/wake", body = { task_id = "t" } })
  end)

  cord.services.stop("cordanui-agents")
  assert(not cord.services.is_running("cordanui-agents"))

  -- request while stopped: clean lua error, not a crash
  local ok2, err2 = pcall(function()
    return cord.services.request("cordanui-agents", { path = "/" })
  end)
  local matched = tostring(err2):find("not running") ~= nil
  return { content = tostring(matched) }
end
"##,
        );
        let host = Arc::new(MockServices::default());
        let plugin = LuaPlugin::load(
            &dir,
            "services",
            None,
            HostHooks::new().with_services(host.clone()),
        )
        .unwrap();

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let resp = rt.block_on(async {
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
        });
        // The stopped-service request produced the "not running" error.
        assert_eq!(resp.content, "true");
        cleanup("services");
    }
}
