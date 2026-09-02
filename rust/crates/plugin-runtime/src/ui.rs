//! Host-owned UI primitives — how a Lua plugin asks the user a question.
//!
//! Plugins never draw to the terminal. They *request* a modal (input box,
//! confirm, picker) through `cord.ui.*`, and the host renders it with its
//! own widgets and routes all keystrokes. The plugin's Lua call simply
//! awaits the answer:
//!
//! ```lua
//! local name = cord.ui.input({ title = "Goal name" })
//! if name then cord.notify("you typed: " .. name) end
//!
//! local ok = cord.ui.confirm({ title = "Delete", message = "sure?" })
//! local idx = cord.ui.pick({ title = "Pick one", items = { "a", "b" } })
//! ```
//!
//! Contract notes:
//!
//! - Calls are awaitable: the host keeps its event loop running while a
//!   plugin waits; other plugins keep working.
//! - Cancelling (Esc) resolves to `nil` / `false`, never an error.
//! - If the host cannot show the dialog right now it answers `Refused`,
//!   which surfaces as a Lua error naming the reason.
//! - If the host drops its end entirely the wait resolves like a cancel.

use std::sync::Arc;

use tokio::sync::oneshot;

/// A modal a plugin wants shown.
#[derive(Debug, Clone)]
pub enum UiRequest {
    /// Single-line text entry. Returns the text, or `None` on cancel.
    Input {
        title: String,
        placeholder: Option<String>,
        prefill: Option<String>,
    },
    /// Yes/no question. Returns the answer.
    Confirm { title: String, message: String },
    /// Pick one of N options. Returns the 0-based selection index, or
    /// `None` on cancel.
    Pick { title: String, items: Vec<String> },
    /// Toggle any of N options. Returns the 0-based indices left selected
    /// on submit (possibly empty), or `None` on cancel.
    MultiSelect {
        title: String,
        items: Vec<String>,
        /// Indices selected upfront.
        preselected: Vec<usize>,
    },
    /// Multi-line text entry (Enter inserts a newline; the host chooses
    /// its submit chord, e.g. Ctrl+Enter). Returns the text or `None`.
    Text {
        title: String,
        placeholder: Option<String>,
        prefill: Option<String>,
    },
}

impl UiRequest {
    /// The dialog title, for hosts that queue multiple requests.
    pub fn title(&self) -> &str {
        match self {
            Self::Input { title, .. }
            | Self::Confirm { title, .. }
            | Self::Pick { title, .. }
            | Self::MultiSelect { title, .. }
            | Self::Text { title, .. } => title,
        }
    }
}

/// The user's answer.
#[derive(Debug, Clone)]
pub enum UiResponse {
    /// Answered text (input/text), boolean (confirm), 0-based index (pick),
    /// or 0-based index set (multiselect). The `None`/`false` forms mean
    /// cancelled.
    Text(Option<String>),
    Confirmed(bool),
    Choice(Option<usize>),
    Choices(Option<Vec<usize>>),
    /// The host could not show the dialog (e.g. another dialog is open).
    /// Surfaced to Lua as an error carrying this reason.
    Refused(String),
}

/// Severity of a non-blocking notification ([`UiHost::notify`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UiLevel {
    Info,
    Warn,
    Error,
}

impl UiLevel {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Info => "info",
            Self::Warn => "warn",
            Self::Error => "error",
        }
    }
}

// ---------- tier 2: persistent panels ----------

/// One node of a declarative widget tree. Plugins never touch the
/// terminal — they return these from their `draw` callback and the host
/// renders them.
#[derive(Debug, Clone, PartialEq)]
pub enum Widget {
    /// A styled line of text.
    Text {
        content: String,
        /// Style variable name (e.g. "primary"); None = default foreground.
        fg: Option<String>,
        bold: bool,
    },
    /// Items rendered top-to-bottom, one per line, with an optional
    /// highlighted row.
    List {
        items: Vec<String>,
        highlight: Option<usize>,
    },
    /// Vertical stack of children.
    Column { children: Vec<Widget> },
    /// Horizontal row (for vsplit). Children laid out left→right.
    Row { children: Vec<Widget> },
}

impl Widget {
    pub fn text(content: impl Into<String>) -> Self {
        Self::Text {
            content: content.into(),
            fg: None,
            bold: false,
        }
    }

    /// An empty tree (renders nothing).
    pub fn empty() -> Self {
        Self::Column {
            children: Vec::new(),
        }
    }

    /// Extract a widget tree from a Lua value. Accepted shapes:
    /// - `{ content = "..", fg = "role?", bold = bool? }`      → Text
    /// - `{ items = {..}, highlight = n? }`                    → List (1-based)
    /// - `{ children = {widget,...} }`                         → Column
    /// - an array of any of the above                          → Column
    /// - `nil`                                                 → None
    pub fn from_lua(value: &LuaValue) -> mlua::Result<Option<Widget>> {
        let LuaValue::Table(t) = value else {
            return Ok(match value {
                LuaValue::Nil => None,
                LuaValue::String(s) => Some(Self::text(s.to_string_lossy())),
                other => {
                    return Err(mlua::Error::runtime(format!(
                        "cannot render {} as a widget",
                        other.type_name()
                    )))
                }
            });
        };

        if let Ok(items) = t.get::<Vec<String>>("items") {
            let highlight: Option<usize> = t
                .get::<mlua::Integer>("highlight")
                .ok()
                .map(|n| (n - 1).max(0) as usize);
            return Ok(Some(Self::List { items, highlight }));
        }
        if let Ok(children_values) = t.get::<Vec<LuaValue>>("children") {
            let mut children = Vec::new();
            for child in &children_values {
                if let Some(w) = Self::from_lua(child)? {
                    children.push(w);
                }
            }
            return Ok(Some(Self::Column { children }));
        }
        if let Ok(row_values) = t.get::<Vec<LuaValue>>("row") {
            let mut children = Vec::new();
            for child in &row_values {
                if let Some(w) = Self::from_lua(child)? {
                    children.push(w);
                }
            }
            return Ok(Some(Self::Row { children }));
        }
        if let Ok(content) = t.get::<String>("content") {
            let fg: Option<String> = t.get("fg").ok();
            let bold: bool = t.get("bold").unwrap_or(false);
            return Ok(Some(Self::Text { content, fg, bold }));
        }
        // Plain array of widgets.
        let pairs: Vec<(LuaValue, LuaValue)> = t.pairs().collect::<Result<_, _>>()?;
        if !pairs.is_empty() && pairs.iter().all(|(k, _)| matches!(k, LuaValue::Integer(_))) {
            let mut children = Vec::new();
            for (_, v) in pairs {
                if let Some(w) = Self::from_lua(&v)? {
                    children.push(w);
                }
            }
            return Ok(Some(Self::Column { children }));
        }
        Err(mlua::Error::runtime(
            "widget needs one of: content, items, children",
        ))
    }
}

use mlua::Value as LuaValue;

/// A long-lived panel: `draw` runs every frame, `on_key` receives key
/// names ("a", "up", "enter", "esc", ...). Returning `true` from `on_key`
/// means "handled, redraw"; `false` passes the key through to the host
/// (Esc pass-through closes the panel).
#[derive(Clone)]
pub struct PanelSpec {
    pub title: String,
    pub draw: Arc<dyn Fn() -> Widget + Send + Sync>,
    pub on_key: Arc<dyn Fn(&str) -> bool + Send + Sync>,
}

/// Host side of `cord.ui.show_panel` / `cord.ui.close_panel`.
pub trait PanelHost: Send + Sync {
    fn open_panel(&self, spec: PanelSpec);
    /// No-op if no panel is open.
    fn close_panel(&self);
}

/// Convenience alias for hosts sharing one bridge across runtimes.
pub type SharedPanelHost = std::sync::Arc<dyn PanelHost>;

/// Host side of `cord.config` — namespaced key/value persistence for a
/// plugin's own settings. The host scopes every key under the plugin's
/// name; plugins cannot read or write outside their namespace. Backed by
/// the shared `settings` table, so values written here are exactly what
/// the declarative fallback form and subprocess `config` injection see.
pub trait ConfigHost: Send + Sync {
    fn get(&self, plugin: &str, key: &str) -> Option<String>;
    fn set(&self, plugin: &str, key: &str, value: &str);
}

/// Convenience alias for hosts sharing one bridge across runtimes.
pub type SharedConfigHost = std::sync::Arc<dyn ConfigHost>;

/// One entry of the host's error/diagnostics log.
#[derive(Debug, Clone)]
pub struct ErrorEntry {
    pub created_at: String,
    pub context: String,
    pub message: String,
    pub detail: Option<String>,
}

/// Host side of `cord.errors` — read access to the shared error log
/// (`errors` table). Read-only by design: plugins may review diagnostics
/// but every writer (host or plugin) appends through the same
/// never-fails logging path, not through this trait.
pub trait ErrorLogHost: Send + Sync {
    /// Recent entries, newest first.
    fn list(&self, limit: u32) -> Vec<ErrorEntry>;
    /// Delete all entries.
    fn clear(&self);
}

/// Convenience alias for hosts sharing one bridge across runtimes.
pub type SharedErrorLogHost = std::sync::Arc<dyn ErrorLogHost>;

/// Host side of `cord.services` — lifecycle control for plugin-declared
/// `[service]` processes. The service itself is any-language binary; this
/// interface is how Lua plugins drive it.
pub trait ServiceHost: Send + Sync {
    fn start(&self, plugin: &str, extra_args: &[String]) -> anyhow::Result<()>;
    fn stop(&self, plugin: &str) -> anyhow::Result<()>;
    fn is_running(&self, plugin: &str) -> bool;
    /// Base URL for `cord.services.request` (from the manifest's
    /// `addr`, or derived from `health`).
    fn base_url(&self, plugin: &str) -> Option<String>;
}

/// Convenience alias for hosts sharing one bridge across runtimes.
pub type SharedServiceHost = std::sync::Arc<dyn ServiceHost>;

/// Host side of `cord.sheets` — sheets (buffers) for work/project separation.
/// Backed by `goal_sheets` table, synced via Turso.
pub trait SheetsHost: Send + Sync {
    fn list_sheets(&self) -> Vec<cordanui_schema::GoalSheet>;
    fn create_sheet(&self, name: &str) -> anyhow::Result<String>;
    fn delete_sheet(&self, id: &str) -> anyhow::Result<()>;
    fn select_sheet(&self, id: Option<String>) -> anyhow::Result<()>;
    fn current_sheet(&self) -> Option<String>;
}
pub type SharedSheetsHost = std::sync::Arc<dyn SheetsHost>;

/// Host side of `cord.buffers` — plugin-controlled buffers that appear as
/// sheet tabs but render a declarative PanelSpec instead of goals. Used for
/// chat/model pickers that should feel like Claude Code / Codex.
pub trait BuffersHost: Send + Sync {
    fn create_buffer(&self, name: String, spec: PanelSpec) -> String;
    fn update_buffer(&self, id: &str, spec: PanelSpec) -> anyhow::Result<()>;
    fn remove_buffer(&self, id: &str);
    fn select_buffer(&self, id: Option<String>);
    fn list_buffers(&self) -> Vec<String>;
    fn current_buffer(&self) -> Option<String>;
}
pub type SharedBuffersHost = std::sync::Arc<dyn BuffersHost>;

/// Host side of `cord.goals` — list and assign goals to agents. Used for
/// `@1-6` / `@<id>-<id>` mentions in `cordanui-chat` and direct assign from TUI.
pub trait GoalsHost: Send + Sync {
    fn list_goals(&self) -> Vec<cordanui_schema::Goal>;
    fn assign_to_agent(&self, goal_id: &str, agent: Option<String>, model: Option<String>) -> anyhow::Result<()>;
    fn assign_range_to_agent(&self, start: &str, end: &str, agent: Option<String>, model: Option<String>) -> anyhow::Result<Vec<String>>;
    /// Dynamic form / data attribute: merge JSON patch into goals.metadata.
    /// Plugins use this to expose forms that mobile renders via data attribute.
    fn set_goal_data(&self, goal_id: &str, key: &str, value: serde_json::Value) -> anyhow::Result<()> {
        let _ = (goal_id, key, value);
        anyhow::bail!("set_goal_data not implemented by host");
    }
    /// List available provider models (live catalog or manifest fallback).
    fn list_models(&self) -> Vec<String> {
        Vec::new()
    }
}
pub type SharedGoalsHost = std::sync::Arc<dyn GoalsHost>;

/// A request plus the channel the host answers on.
///
/// Hosts that drop a `PendingUi` without responding cause the waiting
/// plugin call to resolve as cancelled — a safe default.
pub struct PendingUi {
    pub request: UiRequest,
    pub respond: oneshot::Sender<UiResponse>,
}

/// The host side of the `cord.ui.*` API. One instance serves every
/// loaded plugin; implementations should be cheap to clone via `Arc`.
pub trait UiHost: Send + Sync {
    fn submit(&self, pending: PendingUi);

    /// Non-blocking notification (`cord.ui.notify`). Fire-and-forget:
    /// no response channel. The default implementation drops the message,
    /// so hosts without a surface need no override.
    fn notify(&self, _level: UiLevel, _message: String) {}
}

/// Convenience alias for hosts sharing one bridge across runtimes.
pub type SharedUiHost = std::sync::Arc<dyn UiHost>;

/// A [`UiHost`] whose dialogs always fail with a fixed reason. Used by
/// hosts that have no UI surface attached.
#[derive(Debug, Clone)]
pub struct NoUiHost {
    pub reason: String,
}

impl UiHost for NoUiHost {
    fn submit(&self, pending: PendingUi) {
        let _ = pending
            .respond
            .send(UiResponse::Refused(self.reason.clone()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn titles_are_readable_from_every_variant() {
        let reqs = [
            UiRequest::Input {
                title: "in".into(),
                placeholder: None,
                prefill: None,
            },
            UiRequest::Confirm {
                title: "cf".into(),
                message: "m".into(),
            },
            UiRequest::Pick {
                title: "pk".into(),
                items: vec![],
            },
        ];
        assert!(reqs.iter().all(|r| !r.title().is_empty()));
    }

    #[test]
    fn no_ui_host_refuses_politely() {
        let host = NoUiHost {
            reason: "headless".into(),
        };
        let (tx, mut rx) = oneshot::channel();
        host.submit(PendingUi {
            request: UiRequest::Confirm {
                title: "t".into(),
                message: "m".into(),
            },
            respond: tx,
        });
        assert!(matches!(
            rx.try_recv().unwrap(),
            UiResponse::Refused(reason) if reason == "headless"
        ));
    }
}
