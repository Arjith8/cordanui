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
pub struct PanelSpec {
    pub title: String,
    pub draw: Box<dyn Fn() -> Widget + Send>,
    pub on_key: Box<dyn Fn(&str) -> bool + Send>,
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
