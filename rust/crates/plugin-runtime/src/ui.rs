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
