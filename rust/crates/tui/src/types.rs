//! Shared types used by the TUI app state and its consumers.
//!
//! Extracted from `app.rs` to keep that file focused on logic rather than
//! type declarations.

use cordanui_schema::Goal;

// ─── interaction modes ───────────────────────────────────────────────────────

/// What the TUI is currently doing. Determines how input is handled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mode {
    /// Normal navigation mode.
    Normal,
    /// Adding a goal. `parent_id` is None for a root goal, Some for a subgoal.
    AddGoal { parent_id: Option<String> },
    /// Editing an existing goal's title.
    EditTitle { goal_id: String },
    /// Editing an existing goal's description.
    EditDescription { goal_id: String },
    /// Editing an existing goal's due date.
    EditDue { goal_id: String },
    /// Editing an existing goal's reminder time.
    EditReminder { goal_id: String },
    /// Editing an existing goal's repeat rule.
    EditRepeat { goal_id: String },
    /// Confirmation prompt for deleting a goal.
    ConfirmDelete { goal_id: String },
    /// Confirmation prompt for purging the entire database (danger zone
    /// row on the global settings page).
    ConfirmPurge,
    /// Help overlay.
    Help,
    /// Plugin manager popup (installed list + install input).
    PluginManager {
        /// Which pane has keyboard focus.
        pane: PluginPane,
    },
    /// Plugin manager's own help page.
    PluginHelp,
    /// Configure form for one plugin (declarative [ui] manifest section).
    PluginConfigure { plugin: String },
    /// Pick a provider+model to run the selected goal with.
    AgentPicker { goal_id: String },
    /// An agent run is streaming for this goal.
    AgentRunning { goal_id: String },
    /// Input for `@1-6` / `@<id>-<id>` range assign to agent (from goals page or chat).
    AssignRange,
    /// Pick a new parent for the selected goal (None = move to root).
    MovePicker { goal_id: String },
    /// Pick a sheet (buffer) to switch to, or create/delete.
    SheetPicker,
    /// Adding a new sheet (buffer).
    AddSheet,
    /// Confirm deleting a sheet.
    ConfirmDeleteSheet { sheet_id: String },
    /// A plugin requested a modal dialog via `cord.ui.*`. The payload
    /// lives in [`App::plugin_modal`] because it carries a non-comparable
    /// responder channel.
    PluginModal,
    /// A plugin owns the screen via `cord.ui.show_panel`. Payload in
    /// [`App::plugin_panel`].
    PluginPanel,
    /// The user-facing command line (`<leader>;`) listing every command
    /// registered by active Lua plugins via `plugin.commands`.
    Command,
    /// Host-level settings (Turso credentials) plus one entry per plugin
    /// that owns a configurator.
    GlobalConfig,
    /// Stats overlay.
    Stats,
}

// ─── plugin modal ────────────────────────────────────────────────────────────

/// A plugin-requested modal currently on screen.
#[derive(Debug)]
pub struct ActivePluginModal {
    pub request: cordanui_plugin_runtime::UiRequest,
    pub kind: PluginModalKind,
    pub respond: tokio::sync::oneshot::Sender<cordanui_plugin_runtime::UiResponse>,
}

/// Per-kind interactive state of an open plugin modal.
#[derive(Debug)]
pub enum PluginModalKind {
    Input {
        buffer: String,
        placeholder: Option<String>,
    },
    Confirm,
    Pick {
        selected: usize,
    },
    /// Multi-select: per-item toggles plus a highlight cursor.
    /// Enter submits the set, Esc cancels.
    MultiSelect {
        selected: Vec<bool>,
        cursor: usize,
    },
    /// Multi-line text: Enter inserts a newline, Ctrl+Enter submits.
    TextEditor {
        buffer: String,
        placeholder: Option<String>,
    },
}

// ─── plugin commands ─────────────────────────────────────────────────────────

/// A command registered by an installed Lua plugin.
#[derive(Debug, Clone)]
pub struct PluginCommand {
    pub plugin_name: String,
    pub name: String,
    pub desc: String,
}

// ─── help tabs ───────────────────────────────────────────────────────────────

/// One tab of the help page. Tab 0 is the built-in keybinds page
/// (`plugin: None`); every active plugin that ships `[[help]]` manifest
/// sections gets its own tab with the sections concatenated.
#[derive(Debug, Clone)]
pub struct HelpTab {
    /// Short label shown in the tab bar.
    pub title: String,
    /// Owning plugin id; `None` = built-in keybinds tab.
    pub plugin: Option<String>,
    /// Body text (plugin tabs only; empty for built-in).
    pub text: String,
}

// ─── sync status ─────────────────────────────────────────────────────────────

/// Live sync state for the status bar.
#[derive(Debug, Clone, PartialEq)]
pub enum SyncStatus {
    /// No Turso credentials configured.
    NotConfigured,
    /// A sync is in flight.
    Syncing,
    /// Last sync succeeded; `at` drives the "2m ago" delta.
    Synced { at: std::time::Instant },
    /// Last sync failed (offline, bad token, ...).
    Failed {
        at: std::time::Instant,
        error: String,
    },
}

impl Default for SyncStatus {
    fn default() -> Self {
        Self::NotConfigured
    }
}

// ─── plugin worker types ─────────────────────────────────────────────────────

/// What a worker thread should invoke on a plugin state.
#[derive(Debug, Clone)]
pub enum PluginCall {
    Command(String),
    Configure,
}

/// Sent back from the worker thread when a plugin command finishes.
pub struct PluginCommandOutcome {
    pub plugin_name: String,
    pub state: cordanui_plugin_runtime::LuaPlugin,
    pub result: anyhow::Result<Option<String>>,
}

// ─── agent picker ────────────────────────────────────────────────────────────

/// One selectable agent/provider choice in the agent picker.
/// For `provider` plugins this expands to one entry per model; for pure
/// `agent` plugins there is a single entry with `model = None`.
#[derive(Debug, Clone)]
pub struct AgentChoice {
    pub plugin: String,
    pub model: Option<String>,
    pub binary: std::path::PathBuf,
    pub is_lua: bool,
    pub config: Option<serde_json::Value>,
}

// ─── plugin manager ──────────────────────────────────────────────────────────

/// Focus within the plugin manager popup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PluginPane {
    /// Browsing the installed-plugins list (default).
    List,
    /// Install overlay: typing a query / watching install progress.
    Install,
}

// ─── input buffer ────────────────────────────────────────────────────────────

/// The text input buffer used in AddGoal / EditTitle / EditDescription modes.
#[derive(Debug, Clone, Default)]
pub struct InputBuffer {
    pub text: String,
    pub cursor: usize,
}

impl InputBuffer {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn clear(&mut self) {
        self.text.clear();
        self.cursor = 0;
    }

    pub fn push_char(&mut self, c: char) {
        self.text.insert(self.cursor, c);
        self.cursor += c.len_utf8();
    }

    pub fn backspace(&mut self) {
        if self.cursor > 0 {
            let prev = self.text[..self.cursor]
                .char_indices()
                .last()
                .map(|(i, _)| i)
                .unwrap_or(0);
            self.text.replace_range(prev..self.cursor, "");
            self.cursor = prev;
        }
    }

    pub fn move_left(&mut self) {
        if self.cursor > 0 {
            let prev = self.text[..self.cursor]
                .char_indices()
                .last()
                .map(|(i, _)| i)
                .unwrap_or(0);
            self.cursor = prev;
        }
    }

    pub fn move_right(&mut self) {
        if self.cursor < self.text.len() {
            let next = self.text[self.cursor..]
                .char_indices()
                .nth(1)
                .map(|(i, _)| self.cursor + i)
                .unwrap_or(self.text.len());
            self.cursor = next;
        }
    }

    pub fn move_start(&mut self) {
        self.cursor = 0;
    }

    pub fn move_end(&mut self) {
        self.cursor = self.text.len();
    }
}

// ─── flat tree row ───────────────────────────────────────────────────────────

/// A visible row in the flattened tree. Owned (cloned from goals) so action
/// methods can mutate `self` without lifetime issues.
#[derive(Debug, Clone)]
pub struct FlatRow {
    pub goal: Goal,
    pub depth: usize,
    pub has_children: bool,
    pub expanded: bool,
}

// ─── sync helpers ────────────────────────────────────────────────────────────

/// How often the push/pull sync runs (when credentials are configured).
pub const SYNC_INTERVAL: std::time::Duration = std::time::Duration::from_secs(300);

/// Human delta for the status bar ("just now", "4m ago", "2h ago").
pub fn format_ago(at: std::time::Instant) -> String {
    let secs = at.elapsed().as_secs();
    if secs < 60 {
        "just now".to_string()
    } else if secs < 3600 {
        format!("{}m ago", secs / 60)
    } else {
        format!("{}h ago", secs / 3600)
    }
}
