//! Live style overrides — the host side of the `cord.g` / `cord["local"]`
//! styling API.
//!
//! [`StyleBridge`] implements `StyleHost` for the embedded Lua runtime.
//! Global (`.g`) changes are queued as pending operations and committed to
//! the `settings` table by the event loop ([`StyleBridge::drain_pending`]
//! is called from `App::apply_style_updates`), so they persist and sync to
//! every client through Turso. Session (`.local`) changes take effect
//! immediately in-memory and die with the process.
//!
//! Any change marks the bridge dirty; the event loop re-resolves the
//! palette on the next tick, which makes restyling visible instantly.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use cordanui_plugin_runtime::StyleHost;

/// One queued persistent change, to be committed by the App.
#[derive(Debug)]
pub enum PendingStyle {
    Set { var: String, hex: String },
    Clear { var: String },
    ClearAll,
}

/// Shared style state: session overrides + a queue of pending DB writes.
#[derive(Default)]
pub struct StyleBridge {
    session: Mutex<HashMap<String, String>>,
    pending: Mutex<Vec<PendingStyle>>,
    dirty: AtomicBool,
}

impl StyleBridge {
    pub fn new() -> Self {
        Self::default()
    }

    /// Session overrides as name → color string, for theme resolution.
    pub fn session_snapshot(&self) -> HashMap<String, String> {
        self.session.lock().unwrap().clone()
    }

    /// True if something changed since the last resolve.
    pub fn dirty(&self) -> bool {
        self.dirty.load(Ordering::Relaxed)
    }

    /// Mark changes as consumed by a re-resolve.
    pub fn clear_dirty(&self) {
        self.dirty.store(false, Ordering::Relaxed);
    }

    /// Take all queued persistent changes.
    pub fn drain_pending(&self) -> Vec<PendingStyle> {
        std::mem::take(&mut *self.pending.lock().unwrap())
    }

    fn mark_dirty(&self) {
        self.dirty.store(true, Ordering::Relaxed);
    }
}

impl StyleHost for StyleBridge {
    fn set(&self, persistent: bool, var: &str, hex: &str) {
        if persistent {
            self.pending.lock().unwrap().push(PendingStyle::Set {
                var: var.to_string(),
                hex: hex.to_string(),
            });
        } else {
            self.session
                .lock()
                .unwrap()
                .insert(var.to_string(), hex.to_string());
        }
        self.mark_dirty();
    }

    fn clear(&self, persistent: bool, var: &str) {
        if persistent {
            self.pending.lock().unwrap().push(PendingStyle::Clear {
                var: var.to_string(),
            });
        } else {
            self.session.lock().unwrap().remove(var);
        }
        self.mark_dirty();
    }

    fn clear_all(&self, persistent: bool) {
        if persistent {
            self.pending.lock().unwrap().push(PendingStyle::ClearAll);
        } else {
            self.session.lock().unwrap().clear();
        }
        self.mark_dirty();
    }

    /// The effective override for a variable, if any. Only session
    /// overrides are visible here — global ones live in the DB and are
    /// picked up by theme resolution.
    fn resolved(&self, var: &str) -> Option<String> {
        self.session.lock().unwrap().get(var).cloned()
    }
}
