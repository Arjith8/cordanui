//! App state for the TUI.
//!
//! Holds the DB connection, the flat goal list, the expanded-node set, the
//! selection index, and the current input mode (normal / inserting text /
//! editing). Input is handled inline in the TUI loop — a modal-style text
//! input field at the bottom of the screen.

use std::collections::{HashMap, HashSet};

use anyhow::Context;
use cordanui_schema::{CreateGoalInput, Goal, GoalStatus, UpdateGoalInput};
use cordanui_sync::Database;

use crate::db;
use crate::plugin_ui::PanelCommand;
use cordanui_plugin_runtime::{UiRequest, UiResponse};

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
    /// Confirmation prompt for deleting a goal.
    ConfirmDelete { goal_id: String },
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
}

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

/// A command registered by an installed Lua plugin.
#[derive(Debug, Clone)]
pub struct PluginCommand {
    pub plugin_name: String,
    pub name: String,
    pub desc: String,
}

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

/// One selectable (provider plugin, model) pair in the agent picker.
#[derive(Debug, Clone)]
pub struct AgentChoice {
    pub plugin: String,
    pub model: String,
    pub binary: std::path::PathBuf,
    pub config: Option<serde_json::Value>,
}

/// Focus within the plugin manager popup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PluginPane {
    /// Browsing the installed-plugins list (default).
    List,
    /// Install overlay: typing a query / watching install progress.
    Install,
}

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

/// A visible row in the flattened tree. Owned (cloned from goals) so action
/// methods can mutate `self` without lifetime issues.
#[derive(Debug, Clone)]
pub struct FlatRow {
    pub goal: Goal,
    pub depth: usize,
    pub has_children: bool,
    pub expanded: bool,
}

use ratatui::widgets::ListState;

/// The full TUI application state.
pub struct App {
    pub db: Database,
    /// Configured key bindings ([keybinds] in config.toml).
    pub keybinds: crate::config::Keybinds,
    /// Resolved style palette (builtin ← theme ← global ← session).
    /// Re-resolved whenever [`Self::styles`] reports changes.
    pub theme: crate::theme::Theme,
    /// Live style overrides — the host side of `cord.g` / `cord["local"]`.
    pub styles: std::sync::Arc<crate::style::StyleBridge>,
    /// Host side of the `cord.ui.*` dialog API. Plugins submit requests
    /// here; the loop drains them into [`Self::plugin_modal`].
    pub plugin_ui: std::sync::Arc<crate::plugin_ui::PluginUiBridge>,
    /// The open plugin modal, if any. Always paired with
    /// [`Mode::PluginModal`].
    pub plugin_modal: Option<ActivePluginModal>,
    /// The open plugin panel, if any. Always paired with
    /// [`Mode::PluginPanel`].
    pub plugin_panel: Option<cordanui_plugin_runtime::PanelSpec>,
    /// Loaded Lua plugin states, keyed by manifest name. States are moved
    /// out to a worker thread while a command runs and re-inserted on
    /// completion.
    pub(crate) plugin_states: std::sync::Mutex<HashMap<String, cordanui_plugin_runtime::LuaPlugin>>,
    /// Every command exposed by loaded plugins (name + description +
    /// owning plugin), refreshed when states load.
    pub plugin_commands: Vec<PluginCommand>,
    /// In-flight plugin command result channel + guard.
    pub(crate) command_rx: Option<std::sync::mpsc::Receiver<PluginCommandOutcome>>,
    pub command_running: bool,
    /// All goals loaded from the DB.
    pub goals: Vec<Goal>,
    /// IDs of expanded nodes in the tree.
    pub expanded: HashSet<String>,
    /// ID of the goal whose details (description) are shown, if any.
    pub detailed: Option<String>,
    /// Ratatui list state — tracks selection + scroll offset of the goal list.
    pub list_state: ListState,
    /// Whether the leader key (Ctrl+A) was just pressed and we're waiting
    /// for the command key.
    pub leader_pending: bool,
    /// Current interaction mode.
    pub mode: Mode,
    /// Text input buffer (reused across modes).
    pub input: InputBuffer,
    /// Transient status message shown in the status bar.
    pub message: Option<String>,
    /// Plugin manager: in-flight task channel (background thread).
    pub plugin_rx: Option<std::sync::mpsc::Receiver<crate::plugins::TaskEvent>>,
    /// Plugin manager: current task state rendered in the popup.
    pub plugin_state: crate::plugins::TaskState,
    /// Installed plugins (most recent first).
    pub installed_plugins: Vec<db::PluginRow>,
    /// Selection index into `installed_plugins`.
    pub plugin_selected: usize,
    /// Configure form: the [ui] spec of the plugin being configured.
    pub config_spec: Option<cordanui_plugin_runtime::UiSpec>,
    /// Configure form: current editable values (bare field key → value).
    pub config_values: std::collections::BTreeMap<String, String>,
    /// Configure form: selected field index.
    pub config_selected: usize,
    /// Configure form: in-progress edit buffer (None = not editing).
    pub config_editing: Option<String>,
    /// Agent picker choices (provider × model) for the current picker.
    pub agent_choices: Vec<AgentChoice>,
    /// Agent picker selection index.
    pub agent_selected: usize,
    /// In-flight agent run event channel.
    pub agent_rx: Option<std::sync::mpsc::Receiver<cordanui_plugin_runtime::AgentEvent>>,
    /// Live log of the running agent's progress events.
    pub agent_log: Vec<String>,
    /// Goal the in-flight agent run belongs to (survives navigation).
    pub agent_goal: Option<String>,
}

impl App {
    pub fn new(db: Database) -> anyhow::Result<Self> {
        let goals = db::get_all(&db)?;
        let styles = std::sync::Arc::new(crate::style::StyleBridge::new());
        let theme = crate::theme::Theme::resolve(&db, &styles.session_snapshot());
        let plugin_ui = std::sync::Arc::new(crate::plugin_ui::PluginUiBridge::new());
        let mut list_state = ListState::default();
        if !goals.is_empty() {
            list_state.select(Some(0));
        }
        let mut app = Self {
            db,
            keybinds: crate::config::Keybinds::default(),
            theme,
            styles,
            plugin_ui,
            plugin_modal: None,
            plugin_panel: None,
            plugin_states: std::sync::Mutex::new(HashMap::new()),
            plugin_commands: Vec::new(),
            command_rx: None,
            command_running: false,
            goals,
            expanded: HashSet::new(),
            detailed: None,
            list_state,
            leader_pending: false,
            mode: Mode::Normal,
            input: InputBuffer::new(),
            message: None,
            plugin_rx: None,
            plugin_state: crate::plugins::TaskState::Idle,
            installed_plugins: Vec::new(),
            plugin_selected: 0,
            config_spec: None,
            config_values: Default::default(),
            config_selected: 0,
            config_editing: None,
            agent_choices: Vec::new(),
            agent_selected: 0,
            agent_rx: None,
            agent_log: Vec::new(),
            agent_goal: None,
        };
        Ok(app)
    }

    /// Index into `flat_rows()` of the currently selected row.
    pub fn selected_index(&self) -> usize {
        self.list_state.selected().unwrap_or(0)
    }

    /// Reload goals from the DB.
    pub fn reload(&mut self) -> anyhow::Result<()> {
        self.goals = db::get_all(&self.db)?;
        let max = self.flat_rows().len().saturating_sub(1);
        let sel = self.selected_index().min(max);
        if self.goals.is_empty() {
            self.list_state.select(None);
        } else {
            self.list_state.select(Some(sel));
        }

        Ok(())
    }

    /// Build the flattened tree for rendering. Returns owned `FlatRow`s so
    /// callers can hold them across `&mut self` calls.
    pub fn flat_rows(&self) -> Vec<FlatRow> {
        let mut by_parent: HashMap<Option<String>, Vec<&Goal>> = HashMap::new();
        for g in &self.goals {
            by_parent.entry(g.parent_id.clone()).or_default().push(g);
        }
        for list in by_parent.values_mut() {
            list.sort_by(|a, b| {
                a.sort_order
                    .cmp(&b.sort_order)
                    .then(a.created_at.cmp(&b.created_at))
            });
        }

        let mut rows = Vec::new();
        let roots: Vec<&Goal> = by_parent.get(&None).cloned().unwrap_or_default();
        self.walk(&roots, 0, &mut rows, &by_parent);
        rows
    }

    fn walk(
        &self,
        nodes: &[&Goal],
        depth: usize,
        rows: &mut Vec<FlatRow>,
        by_parent: &HashMap<Option<String>, Vec<&Goal>>,
    ) {
        for node in nodes {
            let children: Vec<&Goal> = by_parent
                .get(&Some(node.id.clone()))
                .cloned()
                .unwrap_or_default();
            let has_children = !children.is_empty();
            let expanded = self.expanded.contains(&node.id);
            rows.push(FlatRow {
                goal: (*node).clone(),
                depth,
                has_children,
                expanded,
            });
            if has_children && expanded {
                self.walk(&children, depth + 1, rows, by_parent);
            }
        }
    }

    /// The currently selected row, if any (owned, so no borrow conflicts).
    pub fn selected_row(&self) -> Option<FlatRow> {
        self.flat_rows().get(self.selected_index()).cloned()
    }

    /// IDs of goals marked completed whose subtree is not fully completed —
    /// rendered with a green ringed circle instead of the normal check.
    pub fn partially_complete_ids(&self) -> HashSet<String> {
        let mut by_parent: HashMap<Option<String>, Vec<&Goal>> = HashMap::new();
        for g in &self.goals {
            by_parent.entry(g.parent_id.clone()).or_default().push(g);
        }

        // Memoized: does this goal's whole subtree count as done?
        fn all_done(
            goal: &Goal,
            by_parent: &HashMap<Option<String>, Vec<&Goal>>,
            memo: &mut HashMap<String, bool>,
        ) -> bool {
            if let Some(done) = memo.get(&goal.id) {
                return *done;
            }
            let done = goal.status == GoalStatus::Completed
                && by_parent
                    .get(&Some(goal.id.clone()))
                    .map(|children| children.iter().all(|c| all_done(c, by_parent, memo)))
                    .unwrap_or(true);
            memo.insert(goal.id.clone(), done);
            done
        }

        let mut memo = HashMap::new();
        self.goals
            .iter()
            .filter(|g| g.status == GoalStatus::Completed && !all_done(g, &by_parent, &mut memo))
            .map(|g| g.id.clone())
            .collect()
    }

    // ---------- actions ----------

    pub fn move_up(&mut self) {
        let cur = self.selected_index();
        if cur > 0 {
            self.list_state.select(Some(cur - 1));
        }
    }

    pub fn move_down(&mut self) {
        let max = self.flat_rows().len().saturating_sub(1);
        let cur = self.selected_index();
        if cur < max {
            self.list_state.select(Some(cur + 1));
        }
    }

    /// Jump to the first / last visible row.
    pub fn select_first(&mut self) {
        self.list_state.select(Some(0));
    }

    pub fn select_last(&mut self) {
        let max = self.flat_rows().len().saturating_sub(1);
        self.list_state.select(Some(max));
    }

    pub fn toggle_expand(&mut self) {
        if let Some(row) = self.selected_row() {
            if row.has_children {
                if self.expanded.contains(&row.goal.id) {
                    self.expanded.remove(&row.goal.id);
                } else {
                    self.expanded.insert(row.goal.id.clone());
                }
            }
        }
    }

    /// Leader + show_details: toggle the selected goal's detail view —
    /// reveals its description inline and expands its subgoals.
    pub fn toggle_details(&mut self) {
        let Some(row) = self.selected_row() else {
            return;
        };
        let id = row.goal.id.clone();
        if self.detailed.as_deref() == Some(id.as_str()) {
            // Collapsing — hide the description again.
            self.detailed = None;
            self.expanded.remove(&id);
        } else {
            self.detailed = Some(id.clone());
            // Mark expanded even if it currently has no children — this is
            // what makes <leader>+new_goal add a subgoal under it later.
            self.expanded.insert(id);
        }
    }

    /// Bare cycle_status key: pending → in progress → completed → pending.
    /// Skips `AgentMode` (reserved for the agent backend).
    pub fn cycle_status(&mut self) -> anyhow::Result<()> {
        let Some(row) = self.selected_row() else {
            return Ok(());
        };
        let next = match row.goal.status {
            GoalStatus::Pending => GoalStatus::InProgress,
            GoalStatus::InProgress => GoalStatus::Completed,
            _ => GoalStatus::Pending,
        };
        let completed_at = match next {
            GoalStatus::Completed => Some(Some(cordanui_schema::now_iso())),
            _ => Some(None),
        };
        db::update(
            &self.db,
            &row.goal.id,
            UpdateGoalInput {
                status: Some(next),
                completed_at,
                ..Default::default()
            },
        )?;
        self.reload()?;
        self.set_message(match next {
            GoalStatus::Pending => "status: pending",
            GoalStatus::InProgress => "status: in progress",
            GoalStatus::Completed => "status: done",
            GoalStatus::AgentMode => "status: agent",
        });
        Ok(())
    }

    pub fn toggle_complete(&mut self) -> anyhow::Result<()> {
        if let Some(row) = self.selected_row() {
            let id = row.goal.id.clone();
            if row.goal.status == GoalStatus::Completed {
                db::uncomplete(&self.db, &id)?;
            } else {
                db::complete(&self.db, &id)?;
            }
            self.reload()?;
            self.set_message("toggled complete");
        }
        Ok(())
    }

    pub fn start_add_goal(&mut self, parent_id: Option<String>) {
        self.input.clear();
        self.mode = Mode::AddGoal { parent_id };
    }

    pub fn commit_add_goal(&mut self) -> anyhow::Result<()> {
        let title = self.input.text.trim().to_string();
        if title.is_empty() {
            self.set_message("empty title — cancelled");
            self.mode = Mode::Normal;
            return Ok(());
        }
        let parent_id = match &self.mode {
            Mode::AddGoal { parent_id } => parent_id.clone(),
            _ => None,
        };
        let sort_order = db::next_sort_order(&self.db, parent_id.as_deref())?;
        let input = CreateGoalInput {
            title,
            description: None,
            parent_id: parent_id.clone(),
            sort_order: Some(sort_order),
        };
        let created = db::create(&self.db, input)?;
        if let Some(pid) = &parent_id {
            self.expanded.insert(pid.clone());
        }
        self.reload()?;
        if let Some(idx) = self
            .flat_rows()
            .iter()
            .position(|r| r.goal.id == created.id)
        {
            self.list_state.select(Some(idx));
        }
        self.mode = Mode::Normal;
        self.set_message("goal added");
        Ok(())
    }

    pub fn start_edit_title(&mut self) {
        if let Some(row) = self.selected_row() {
            self.input.text = row.goal.title.clone();
            self.input.cursor = self.input.text.len();
            self.mode = Mode::EditTitle {
                goal_id: row.goal.id.clone(),
            };
        }
    }

    pub fn commit_edit_title(&mut self) -> anyhow::Result<()> {
        let title = self.input.text.trim().to_string();
        let goal_id = match &self.mode {
            Mode::EditTitle { goal_id } => goal_id.clone(),
            _ => return Ok(()),
        };
        if !title.is_empty() {
            db::update(
                &self.db,
                &goal_id,
                UpdateGoalInput {
                    title: Some(title),
                    ..Default::default()
                },
            )?;
            self.reload()?;
            self.set_message("title updated");
        }
        self.mode = Mode::Normal;
        Ok(())
    }

    pub fn start_edit_description(&mut self) {
        if let Some(row) = self.selected_row() {
            self.input.text = row.goal.description.clone().unwrap_or_default();
            self.input.cursor = self.input.text.len();
            self.mode = Mode::EditDescription {
                goal_id: row.goal.id.clone(),
            };
        }
    }

    pub fn commit_edit_description(&mut self) -> anyhow::Result<()> {
        let goal_id = match &self.mode {
            Mode::EditDescription { goal_id } => goal_id.clone(),
            _ => return Ok(()),
        };
        let desc = self.input.text.trim();
        let desc_val = if desc.is_empty() {
            None
        } else {
            Some(desc.to_string())
        };
        db::update(
            &self.db,
            &goal_id,
            UpdateGoalInput {
                description: Some(desc_val),
                ..Default::default()
            },
        )?;
        self.reload()?;
        self.set_message("description updated");
        self.mode = Mode::Normal;
        Ok(())
    }

    pub fn start_delete(&mut self) {
        if let Some(row) = self.selected_row() {
            self.mode = Mode::ConfirmDelete {
                goal_id: row.goal.id.clone(),
            };
        }
    }

    pub fn confirm_delete(&mut self) -> anyhow::Result<()> {
        let goal_id = match &self.mode {
            Mode::ConfirmDelete { goal_id } => goal_id.clone(),
            _ => return Ok(()),
        };
        db::delete(&self.db, &goal_id)?;
        self.reload()?;
        self.set_message("goal deleted");
        self.mode = Mode::Normal;
        Ok(())
    }

    pub fn cancel(&mut self) {
        self.mode = Mode::Normal;
        self.input.clear();
    }

    /// Leader + plugins: open the plugin manager popup (list focused).
    pub fn open_plugin_manager(&mut self) -> anyhow::Result<()> {
        self.input.clear();
        self.reload_installed_plugins()?;
        self.plugin_selected = 0;
        self.mode = Mode::PluginManager {
            pane: PluginPane::List,
        };
        Ok(())
    }

    /// `i` from the list: open the install input overlay.
    pub fn start_install_mode(&mut self) {
        self.input.clear();
        self.mode = Mode::PluginManager {
            pane: PluginPane::Install,
        };
    }

    /// `c` on a selected plugin: load its [ui] spec + stored settings and
    /// open the configure form. Plugins without a [ui] section just report
    /// that there's nothing to configure.
    /// Give `cord.config` its database handle. Call once after
    /// construction (the App's own handle is moved, not shareable).
    pub fn attach_plugin_config_db(&mut self, db: Database) {
        self.plugin_ui
            .attach_config_db(std::sync::Arc::new(std::sync::Mutex::new(db)));
    }

    /// Configure the selected plugin. A Lua plugin that defines
    /// `plugin.configure` owns the whole page — we only invoke it (it may
    /// open panels/dialogs and persists via `cord.config`). Everything
    /// else falls back to the declarative `[[field]]` form.
    pub fn open_configure(&mut self) -> anyhow::Result<()> {
        let Some(p) = self.installed_plugins.get(self.plugin_selected) else {
            return Ok(());
        };
        let dir = std::path::PathBuf::from(&p.dir);
        let manifest = match cordanui_plugin_runtime::PluginManifest::from_dir(&dir) {
            Ok(m) => m,
            Err(e) => {
                self.set_message(&format!("cannot read manifest: {e}"));
                return Ok(());
            }
        };

        // Custom configurator takes precedence.
        let has_custom = self
            .plugin_states
            .lock()
            .unwrap()
            .get(&manifest.plugin.name)
            .map(|s| s.has_configure())
            .unwrap_or(false);
        if has_custom {
            self.spawn_plugin_call(&manifest.plugin.name, PluginCall::Configure);
            return Ok(());
        }

        let Some(spec) = manifest.ui else {
            self.set_message("this plugin has nothing to configure");
            return Ok(());
        };
        let problems = spec.validate();
        if !problems.is_empty() {
            self.set_message(&format!("bad plugin [ui]: {}", problems[0]));
            return Ok(());
        }

        // Existing stored values win; otherwise fall back to defaults.
        let mut values = db::get_plugin_settings(&self.db, &p.id)?;
        for f in &spec.fields {
            values
                .entry(f.key.clone())
                .or_insert_with(|| spec.initial_value(&f.key));
        }

        self.config_spec = Some(spec);
        self.config_values = values;
        self.config_selected = 0;
        self.config_editing = None;
        self.mode = Mode::PluginConfigure {
            plugin: p.id.clone(),
        };
        Ok(())
    }

    /// Commit the in-progress edit for the selected field.
    /// Cycle a `select` field through its options (Tab / Shift+Tab).
    /// Saves immediately — selects have no free-text edit step.
    pub fn cycle_config_field(&mut self, plugin: &str, delta: i32) -> anyhow::Result<()> {
        let Some(spec) = &self.config_spec else {
            return Ok(());
        };
        let Some(field) = spec.fields.get(self.config_selected) else {
            return Ok(());
        };
        if field.r#type != "select" || field.options.is_empty() {
            return Ok(());
        }

        let current = self
            .config_values
            .get(&field.key)
            .cloned()
            .or_else(|| field.default.clone())
            .unwrap_or_default();
        let len = field.options.len() as i64;
        let cur_idx = field
            .options
            .iter()
            .position(|o| *o == current)
            .map(|i| i as i64)
            .unwrap_or(0);
        let next = ((cur_idx + delta as i64).rem_euclid(len)) as usize;
        let value = field.options[next].clone();

        db::set_plugin_setting(&self.db, plugin, &field.key, &value)?;
        self.config_values.insert(field.key.clone(), value.clone());
        self.set_message(&format!("{} = {}", field.key, value));
        Ok(())
    }

    /// True if the field under the cursor is a cycleable select.
    pub fn config_selected_is_select(&self) -> bool {
        self.config_spec
            .as_ref()
            .and_then(|s| s.fields.get(self.config_selected))
            .map(|f| f.r#type == "select" && !f.options.is_empty())
            .unwrap_or(false)
    }

    pub fn commit_config_field(&mut self, plugin: &str) -> anyhow::Result<()> {
        let (Some(spec), Some(buf)) = (&self.config_spec, &self.config_editing) else {
            return Ok(());
        };
        let Some(field) = spec.fields.get(self.config_selected) else {
            return Ok(());
        };

        // Per-type validation before saving.
        let value = buf.trim().to_string();
        if field.required && value.is_empty() {
            self.set_message(&format!("'{}' is required", field.key));
            return Ok(());
        }
        if field.r#type == "number" && !value.is_empty() && value.parse::<f64>().is_err() {
            self.set_message(&format!("'{}' must be numeric", field.key));
            return Ok(());
        }
        if field.r#type == "select" && !value.is_empty() && !field.options.contains(&value) {
            self.set_message(&format!(
                "'{}' must be one of: {}",
                field.key,
                field.options.join(", ")
            ));
            return Ok(());
        }

        db::set_plugin_setting(&self.db, plugin, &field.key, &value)?;
        self.config_values.insert(field.key.clone(), value);
        self.config_editing = None;
        // Move to the next field for fast entry.
        let max = spec.fields.len().saturating_sub(1);
        if self.config_selected < max {
            self.config_selected += 1;
        }
        self.set_message("saved");
        Ok(())
    }

    // ---------- agent runs ----------

    /// Leader + run_agent: collect (active provider × model) choices and
    /// open the picker for the selected goal.
    pub fn open_agent_picker(&mut self, goal_id: String) -> anyhow::Result<()> {
        self.reload_installed_plugins()?;
        let mut choices = Vec::new();

        for p in self.installed_plugins.iter().filter(|p| p.active) {
            let dir = std::path::PathBuf::from(&p.dir);
            let Ok(manifest) = cordanui_plugin_runtime::PluginManifest::from_dir(&dir) else {
                continue;
            };
            if !manifest.capabilities.provider {
                continue;
            }
            let binary = manifest.binary_path(&dir);
            if !binary.exists() {
                continue;
            }
            let Some(provider) = &manifest.provider else {
                continue;
            };
            let values = db::get_plugin_settings(&self.db, &manifest.plugin.name)?;
            let config = db::settings_to_config(&values);

            for model in &provider.models {
                choices.push(AgentChoice {
                    plugin: manifest.plugin.name.clone(),
                    model: model.clone(),
                    binary: binary.clone(),
                    config: config.clone(),
                });
            }
        }

        if choices.is_empty() {
            self.set_message("no active provider plugins with a built binary");
            return Ok(());
        }

        self.agent_choices = choices;
        self.agent_selected = 0;
        self.mode = Mode::AgentPicker { goal_id };
        Ok(())
    }

    /// Spawn the chosen provider in a background thread and mark the goal
    /// as running.
    pub fn start_agent_run(&mut self, goal_id: String) -> anyhow::Result<()> {
        let Some(choice) = self.agent_choices.get(self.agent_selected).cloned() else {
            return Ok(());
        };
        let Some(goal) = self.goals.iter().find(|g| g.id == goal_id) else {
            return Ok(());
        };
        let title = goal.title.clone();
        let description = goal.description.clone();

        db::update(
            &self.db,
            &goal_id,
            UpdateGoalInput {
                agent_status: Some(Some(cordanui_schema::AgentStatus::Running)),
                ..Default::default()
            },
        )?;
        self.reload()?;

        let (tx, rx) = std::sync::mpsc::channel();
        let cfg = cordanui_plugin_runtime::AgentRunConfig {
            task_id: goal_id.clone(),
            title,
            description,
            model: Some(choice.model.clone()),
            config: choice.config.clone(),
        };
        let binary = choice.binary.clone();

        std::thread::spawn(move || {
            use cordanui_plugin_runtime::spawn::run_streaming;
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build();
            let Ok(rt) = rt else {
                let _ = tx.send(cordanui_plugin_runtime::AgentEvent::Error {
                    message: "failed to start runtime".into(),
                    detail: None,
                });
                return;
            };
            let result = rt.block_on(async {
                run_streaming(&binary, &cfg, |ev| {
                    let _ = tx.send(ev.clone());
                })
                .await
            });
            if let Err(e) = result {
                let _ = tx.send(cordanui_plugin_runtime::AgentEvent::Error {
                    message: "plugin invocation failed".into(),
                    detail: Some(e.to_string()),
                });
            }
        });

        self.agent_rx = Some(rx);
        self.agent_goal = Some(goal_id.clone());
        self.agent_log.clear();
        self.agent_log
            .push(format!("{} — {}", choice.plugin, choice.model));
        self.set_message("agent running");
        self.mode = Mode::AgentRunning { goal_id };
        Ok(())
    }

    /// Drain in-flight agent events (non-blocking), called every loop
    /// iteration regardless of mode so completion lands even if the user
    /// navigated away.
    pub fn poll_agent_events(&mut self) -> anyhow::Result<()> {
        if self.agent_rx.is_none() {
            return Ok(());
        }
        let rx = self.agent_rx.take().unwrap();
        loop {
            match rx.try_recv() {
                Ok(cordanui_plugin_runtime::AgentEvent::Progress { message, detail }) => {
                    self.agent_log.push(match detail {
                        Some(d) => format!("{message} — {d}"),
                        None => message,
                    });
                    if self.agent_log.len() > 60 {
                        self.agent_log.drain(..self.agent_log.len() - 40);
                    }
                    // Best-effort live progress on the goal row.
                    if let Some(goal_id) = self.agent_goal.clone() {
                        let last = self.agent_log.last().cloned().unwrap_or_default();
                        let _ = db::update(
                            &self.db,
                            &goal_id,
                            UpdateGoalInput {
                                agent_progress: Some(Some(last)),
                                ..Default::default()
                            },
                        );
                    }
                }
                Ok(cordanui_plugin_runtime::AgentEvent::Result(r)) => {
                    self.finish_agent_run("completed", r.content)?;
                    break;
                }
                Ok(cordanui_plugin_runtime::AgentEvent::Error { message, detail }) => {
                    let text = match detail {
                        Some(d) => format!("{message}: {d}"),
                        None => message,
                    };
                    self.finish_agent_run("failed", text)?;
                    break;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    self.agent_rx = Some(rx);
                    break;
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.finish_agent_run("failed", "agent thread died".into())?;
                    break;
                }
            }
        }
        Ok(())
    }

    fn finish_agent_run(&mut self, status: &str, content: String) -> anyhow::Result<()> {
        // The run belongs to the tracked goal, wherever the user is now.
        let Some(goal_id) = self.agent_goal.take() else {
            self.agent_rx = None;
            return Ok(());
        };

        let status_val = match status {
            "completed" => cordanui_schema::AgentStatus::Completed,
            _ => cordanui_schema::AgentStatus::Failed,
        };
        db::update(
            &self.db,
            &goal_id,
            UpdateGoalInput {
                agent_status: Some(Some(status_val)),
                agent_result: Some(Some(content.clone())),
                ..Default::default()
            },
        )?;
        self.reload()?;
        self.set_message(&format!("agent {status}"));
        self.agent_rx = None;
        if matches!(self.mode, Mode::AgentRunning { .. }) {
            self.mode = Mode::Normal;
        }
        Ok(())
    }

    /// Pull + rebuild every installed plugin on a worker thread
    /// (`u` in the plugin manager). Lua plugins are pull-only — no build.
    pub fn update_all_plugins(&mut self) {
        if self.plugin_rx.is_some() {
            self.set_message("a plugin task is already running");
            return;
        }
        let installed: Vec<(String, String)> = self
            .installed_plugins
            .iter()
            .map(|p| (p.id.clone(), p.dir.clone()))
            .collect();
        if installed.is_empty() {
            self.set_message("nothing installed to update");
            return;
        }
        self.plugin_state = crate::plugins::TaskState::Working(vec![format!(
            "updating {} plugin{}…",
            installed.len(),
            if installed.len() == 1 { "" } else { "s" }
        )]);
        self.plugin_rx = Some(crate::plugins::spawn_update_all_task(installed));
    }

    /// Re-read the plugins registry from the DB.
    pub fn reload_installed_plugins(&mut self) -> anyhow::Result<()> {
        self.installed_plugins = db::list_plugins(&self.db)?;
        let max = self.installed_plugins.len().saturating_sub(1);
        self.plugin_selected = self.plugin_selected.min(max);
        Ok(())
    }

    /// Activate/deactivate the selected plugin. Activating a theme-capable
    /// plugin applies its first theme pack live; deactivating reverts to
    /// builtin dark.
    pub fn toggle_plugin_active(&mut self) -> anyhow::Result<()> {
        let Some(p) = self.installed_plugins.get(self.plugin_selected) else {
            return Ok(());
        };
        let id = p.id.clone();
        let dir = std::path::PathBuf::from(&p.dir);
        let activating = !p.active;

        db::set_plugin_active(&self.db, &id, activating)?;

        // Theme-capable plugins get special handling.
        let is_theme_plugin = std::fs::read_to_string(dir.join("cordanui.toml"))
            .ok()
            .and_then(|t| cordanui_plugin_runtime::PluginManifest::from_str(&t).ok())
            .map(|m| m.capabilities.theme)
            .unwrap_or(false);

        let mut msg = if activating {
            format!("{id} activated")
        } else {
            format!("{id} deactivated")
        };

        if is_theme_plugin {
            if activating {
                let themes = crate::plugins::scan_theme_files(&dir);
                if let Some(t) = themes.first() {
                    db::set_active_theme(&self.db, &t.id)?;
                    self.theme =
                        crate::theme::Theme::resolve(&self.db, &self.styles.session_snapshot());
                    msg = format!("{id} activated — theme '{}' applied", t.name);
                } else {
                    msg = format!("{id} activated (no theme packs found)");
                }
            } else {
                db::clear_theme_selection(&self.db)?;
                self.theme =
                    crate::theme::Theme::resolve(&self.db, &self.styles.session_snapshot());
                msg = format!("{id} deactivated — reverted to builtin dark");
            }
        }

        self.set_message(&msg);
        self.reload_installed_plugins()
    }

    /// Uninstall the selected plugin: delete files + registry row.
    pub fn uninstall_selected_plugin(&mut self) -> anyhow::Result<()> {
        let Some(p) = self.installed_plugins.get(self.plugin_selected) else {
            return Ok(());
        };
        let dir = std::path::PathBuf::from(&p.dir);
        if dir.exists() {
            std::fs::remove_dir_all(&dir).with_context(|| format!("removing {}", dir.display()))?;
        }
        db::remove_plugin_row(&self.db, &p.id)?;
        self.set_message("plugin uninstalled");
        self.reload_installed_plugins()
    }

    /// Dispatch a plugin task for whatever is in the input buffer:
    /// verify + install a GitHub repo, or run a free-text search.
    pub fn start_plugin_search(&mut self) {
        let query = self.input.text.trim().to_string();
        if query.is_empty() {
            return;
        }
        self.plugin_rx = Some(crate::plugins::spawn_plugin_task(&query));
        self.plugin_state = crate::plugins::TaskState::Working(Vec::new());
    }

    /// Drain queued `cord.ui.*` requests and open the first as a modal.
    /// Called every loop iteration. A request arriving while anything else
    /// is on screen is refused — plugins get a clean error instead of a
    /// surprise dialog.
    pub fn poll_plugin_ui_requests(&mut self) {
        let Some(event) = self.plugin_ui.try_take_event() else {
            return;
        };
        // Notifications never block and never get refused.
        if let crate::plugin_ui::PluginUiEvent::Notify { level, message } = &event {
            let prefixed = match level {
                cordanui_plugin_runtime::UiLevel::Info => message.clone(),
                cordanui_plugin_runtime::UiLevel::Warn => format!("⚠ {message}"),
                cordanui_plugin_runtime::UiLevel::Error => format!("✖ {message}"),
            };
            self.set_message(&prefixed);
            return;
        }
        let crate::plugin_ui::PluginUiEvent::Modal(pending) = event else {
            return;
        };
        if self.mode != Mode::Normal || self.plugin_modal.is_some() {
            let _ = pending
                .respond
                .send(cordanui_plugin_runtime::UiResponse::Refused(
                    "another dialog is open".into(),
                ));
            return;
        }
        let kind = match &pending.request {
            UiRequest::Input { placeholder, .. } => PluginModalKind::Input {
                buffer: String::new(),
                placeholder: placeholder.clone(),
            },
            UiRequest::Confirm { .. } => PluginModalKind::Confirm,
            UiRequest::Pick { items, .. } => PluginModalKind::Pick { selected: 0 },
            UiRequest::MultiSelect {
                items, preselected, ..
            } => PluginModalKind::MultiSelect {
                selected: items
                    .iter()
                    .enumerate()
                    .map(|(i, _)| preselected.contains(&i))
                    .collect(),
                cursor: 0,
            },
            UiRequest::Text {
                placeholder,
                prefill,
                ..
            } => PluginModalKind::TextEditor {
                buffer: prefill.clone().unwrap_or_default(),
                placeholder: placeholder.clone(),
            },
        };
        self.plugin_modal = Some(ActivePluginModal {
            kind,
            request: pending.request,
            respond: pending.respond,
        });
        self.mode = Mode::PluginModal;
    }

    /// Answer the open plugin modal and close it.
    pub fn answer_plugin_modal(&mut self, response: UiResponse) {
        if let Some(modal) = self.plugin_modal.take() {
            let _ = modal.respond.send(response);
        }
        if self.mode == Mode::PluginModal {
            self.cancel();
        }
    }

    /// The text currently typed into an open input modal.
    pub fn plugin_modal_text(&self) -> Option<&str> {
        match &self.plugin_modal {
            Some(ActivePluginModal {
                kind:
                    PluginModalKind::Input { buffer, .. } | PluginModalKind::TextEditor { buffer, .. },
                ..
            }) => Some(buffer),
            _ => None,
        }
    }

    /// Feed a character into an open input modal.
    pub fn plugin_modal_push_char(&mut self, c: char) {
        if let Some(ActivePluginModal { kind, .. }) = &mut self.plugin_modal {
            match kind {
                PluginModalKind::Input { buffer, .. }
                | PluginModalKind::TextEditor { buffer, .. } => buffer.push(c),
                _ => {}
            }
        }
    }

    /// Remove the last character from an open input modal.
    pub fn plugin_modal_backspace(&mut self) {
        if let Some(ActivePluginModal { kind, .. }) = &mut self.plugin_modal {
            match kind {
                PluginModalKind::Input { buffer, .. }
                | PluginModalKind::TextEditor { buffer, .. } => {
                    buffer.pop();
                }
                _ => {}
            }
        }
    }

    /// Move the cursor in an open pick/multiselect modal.
    pub fn plugin_modal_move_selection(&mut self, delta: i32) {
        let len = match &self.plugin_modal {
            Some(m) => match &m.request {
                UiRequest::Pick { items, .. } | UiRequest::MultiSelect { items, .. } => items.len(),
                _ => return,
            },
            None => return,
        };
        let bump = |cursor: &mut usize| {
            *cursor = ((*cursor as i64) + (delta as i64)).clamp(0, len as i64 - 1) as usize;
        };
        if let Some(modal) = &mut self.plugin_modal {
            match &mut modal.kind {
                PluginModalKind::Pick { selected } => bump(selected),
                PluginModalKind::MultiSelect { cursor, .. } => bump(cursor),
                _ => {}
            }
        }
    }

    /// Toggle the highlighted item in an open multiselect modal (space).
    pub fn plugin_modal_toggle_current(&mut self) {
        if let Some(ActivePluginModal {
            kind: PluginModalKind::MultiSelect { selected, cursor },
            ..
        }) = &mut self.plugin_modal
        {
            if let Some(flag) = selected.get_mut(*cursor) {
                *flag = !*flag;
            }
        }
    }

    /// Insert a newline in an open text-editor modal (plain Enter).
    pub fn plugin_modal_newline(&mut self) {
        if let Some(ActivePluginModal {
            kind: PluginModalKind::TextEditor { buffer, .. },
            ..
        }) = &mut self.plugin_modal
        {
            buffer.push('\n');
        }
    }

    /// Submit the open modal with its current state (Enter).
    pub fn submit_plugin_modal(&mut self) {
        let Some(modal) = &self.plugin_modal else {
            return;
        };
        let response = match &modal.kind {
            PluginModalKind::Input { buffer, .. } => {
                let text = buffer.trim().to_string();
                UiResponse::Text(if text.is_empty() { None } else { Some(text) })
            }
            PluginModalKind::Confirm => UiResponse::Confirmed(true),
            PluginModalKind::Pick { selected } => UiResponse::Choice(Some(*selected)),
            PluginModalKind::MultiSelect { selected, .. } => UiResponse::Choices(Some(
                selected
                    .iter()
                    .enumerate()
                    .filter(|(_, on)| **on)
                    .map(|(i, _)| i)
                    .collect(),
            )),
            PluginModalKind::TextEditor { buffer, .. } => {
                UiResponse::Text(Some(buffer.trim_end().to_string()))
            }
        };
        self.answer_plugin_modal(response);
    }

    /// Cancel the open modal (Esc / 'n').
    pub fn cancel_plugin_modal(&mut self) {
        let is_confirm = matches!(
            self.plugin_modal.as_ref().map(|m| &m.kind),
            Some(PluginModalKind::Confirm)
        );
        let response = match self.plugin_modal.as_ref().map(|m| &m.kind) {
            Some(PluginModalKind::Confirm) => UiResponse::Confirmed(false),
            Some(PluginModalKind::MultiSelect { .. }) => UiResponse::Choices(None),
            _ => UiResponse::Text(None),
        };
        self.answer_plugin_modal(response);
    }

    /// Take the next queued `show_panel` / `close_panel` command.
    /// Called every loop iteration after [`Self::poll_plugin_ui_requests`].
    pub fn poll_plugin_panel(&mut self) {
        match self.plugin_ui.try_take_panel_command() {
            Some(PanelCommand::Open(spec)) => {
                // One panel at a time; a second replaces the first (the
                // old spec is dropped, which is the documented behavior).
                self.plugin_panel = Some(spec);
                self.mode = Mode::PluginPanel;
            }
            Some(PanelCommand::Close) => self.close_plugin_panel(),
            None => {}
        }
    }

    /// Close the plugin panel (Esc pass-through or `cord.ui.close_panel`).
    pub fn close_plugin_panel(&mut self) {
        self.plugin_panel = None;
        if self.mode == Mode::PluginPanel {
            self.cancel();
        }
    }

    /// Load (or reload) every active Lua-runtime plugin's state and
    /// rebuild the command registry. Called at startup; install/activate
    /// flows can call it again to pick up new commands.
    pub fn load_plugin_states(&mut self) -> Vec<String> {
        let mut problems = Vec::new();
        let Ok(plugins) = db::list_plugins(&self.db) else {
            return problems;
        };
        let mut states = self.plugin_states.lock().unwrap();
        states.clear();
        self.plugin_commands.clear();

        for row in plugins {
            if !row.active {
                continue;
            }
            let dir = std::path::PathBuf::from(&row.dir);
            let Ok(manifest) = cordanui_plugin_runtime::PluginManifest::from_dir(&dir) else {
                continue;
            };
            if !manifest.is_lua() {
                continue;
            }
            // Settings collected from the plugin's Configure form.
            let config = db::settings_to_config(
                &db::get_plugin_settings(&self.db, &manifest.plugin.name).unwrap_or_default(),
            );
            let name = manifest.plugin.name.clone();
            match cordanui_plugin_runtime::LuaPlugin::load(
                &dir,
                &name,
                config,
                crate::plugin_ui::plugin_runtime_hooks(&self.styles, &self.plugin_ui),
            ) {
                Ok(state) => {
                    for cmd in state.list_commands() {
                        self.plugin_commands.push(PluginCommand {
                            plugin_name: name.clone(),
                            name: cmd.name,
                            desc: cmd.desc,
                        });
                    }
                    states.insert(name, state);
                }
                Err(e) => problems.push(format!("{name}: {e:#}")),
            }
        }
        self.plugin_commands.sort_by(|a, b| a.name.cmp(&b.name));
        problems
    }

    /// Open the command line over loaded plugin commands. Refreshes
    /// plugin states first so freshly installed/activated plugins work
    /// without a restart; load problems surface on the status line
    /// (stderr is invisible inside a TUI session).
    pub fn open_command_mode(&mut self) {
        let problems = self.load_plugin_states();
        self.input.clear();
        if let Some(first) = problems.first() {
            self.set_message(&format!("✖ {first}"));
        } else if self.plugin_commands.is_empty() {
            self.set_message(
                "no plugin commands (need an active runtime=\"lua\" plugin defining plugin.commands)",
            );
        }
        self.mode = Mode::Command;
    }

    /// Commands matching the current input text (substring, case-insensitive).
    pub fn command_matches(&self) -> Vec<PluginCommand> {
        let q = self.input.text.trim().to_lowercase();
        self.plugin_commands
            .iter()
            .filter(|c| {
                q.is_empty()
                    || c.name.to_lowercase().contains(&q)
                    || c.desc.to_lowercase().contains(&q)
            })
            .cloned()
            .collect()
    }

    /// Run the named command on a worker thread. Its Lua state is moved
    /// out of the cache until it finishes — dialogs/panels it opens are
    /// answered through the normal event loop while we keep drawing.
    pub fn execute_plugin_command(&mut self, cmd: &PluginCommand) {
        self.spawn_plugin_call(&cmd.plugin_name, PluginCall::Command(cmd.name.clone()));
    }

    /// Shared worker spawn for commands and custom configure pages.
    fn spawn_plugin_call(&mut self, plugin_name: &str, call: PluginCall) {
        if self.command_running {
            self.set_message("a command is already running");
            return;
        }
        let Some(mut state) = self.plugin_states.lock().unwrap().remove(plugin_name) else {
            self.set_message("plugin not loaded");
            return;
        };
        self.command_running = true;
        // Leave any input mode so dialogs can open (they require Normal).
        self.cancel();
        let plugin_name = plugin_name.to_string();
        let (tx, rx) = std::sync::mpsc::channel();
        self.command_rx = Some(rx);
        std::thread::spawn(move || {
            // cordanui-sync's blocking DB API must stay off this thread,
            // so the worker gets its own single-purpose runtime.
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            let result = rt.block_on(async {
                match &call {
                    PluginCall::Command(name) => state.call_command(name).await,
                    PluginCall::Configure => state.call_configure().await,
                }
            });
            let _ = tx.send(PluginCommandOutcome {
                plugin_name,
                state,
                result,
            });
        });
    }

    /// Drain finished command outcomes. Non-blocking.
    pub fn poll_command_results(&mut self) {
        let Some(rx) = &self.command_rx else {
            return;
        };
        match rx.try_recv() {
            Ok(outcome) => {
                self.command_rx = None;
                self.command_running = false;
                self.plugin_states
                    .lock()
                    .unwrap()
                    .insert(outcome.plugin_name, outcome.state);
                match outcome.result {
                    Ok(Some(msg)) => self.set_message(&msg),
                    Ok(None) => self.set_message("done"),
                    Err(e) => self.set_message(&format!("✖ {e:#}")),
                }
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.command_rx = None;
                self.command_running = false;
            }
        }
    }

    /// Commit any pending style changes and re-resolve the palette if
    /// something changed. Called every loop iteration so `cord.g` /
    /// `cord["local"]` restyles land within a frame or two.
    pub fn apply_style_updates(&mut self) -> anyhow::Result<()> {
        if !self.styles.dirty() {
            return Ok(());
        }
        for op in self.styles.drain_pending() {
            match op {
                crate::style::PendingStyle::Set { var, hex } => {
                    db::set_style_override(&self.db, &var, &hex)?;
                }
                crate::style::PendingStyle::Clear { var } => {
                    db::clear_style_override(&self.db, &var)?;
                }
                crate::style::PendingStyle::ClearAll => {
                    db::clear_all_style_overrides(&self.db)?;
                }
            }
        }
        self.theme = crate::theme::Theme::resolve(&self.db, &self.styles.session_snapshot());
        self.styles.clear_dirty();
        Ok(())
    }

    /// Drain the in-flight plugin task (non-blocking). Called every loop
    /// iteration; `Log` events accumulate in the activity log so the popup
    /// can show live progress (resolving → cloning % → manifest check).
    pub fn poll_plugin_search(&mut self) -> anyhow::Result<()> {
        if self.plugin_rx.is_none() {
            return Ok(());
        }
        let rx = self.plugin_rx.take().unwrap();
        loop {
            match rx.try_recv() {
                Ok(crate::plugins::TaskEvent::Log(line)) => {
                    match &mut self.plugin_state {
                        crate::plugins::TaskState::Working(log) => {
                            log.push(line);
                            // Keep the log bounded.
                            if log.len() > 60 {
                                log.drain(..log.len() - 40);
                            }
                        }
                        _ => {
                            self.plugin_state = crate::plugins::TaskState::Working(vec![line]);
                        }
                    }
                    // Keep draining; the task may still be running.
                }
                Ok(crate::plugins::TaskEvent::Results(r)) => {
                    self.plugin_state = crate::plugins::TaskState::Results(r);
                    break;
                }
                Ok(crate::plugins::TaskEvent::NotFound(q)) => {
                    self.plugin_state = crate::plugins::TaskState::NotFound(q);
                    break;
                }
                Ok(crate::plugins::TaskEvent::Error(e)) => {
                    self.plugin_state = crate::plugins::TaskState::Error(e);
                    break;
                }
                Ok(crate::plugins::TaskEvent::Updated { name, themes }) => {
                    // Re-import theme packs under the plugin's existing
                    // source (no re-registration — it's already installed).
                    let source = self
                        .installed_plugins
                        .iter()
                        .find(|p| p.id == name)
                        .map(|p| p.source.clone())
                        .unwrap_or_else(|| "plugin".into());
                    for t in &themes {
                        let _ = db::upsert_theme(&self.db, &t.id, &t.name, &source, &t.colors_json);
                    }
                    if let crate::plugins::TaskState::Working(log) = &mut self.plugin_state {
                        log.push(format!(
                            "{name} updated{}",
                            if themes.is_empty() {
                                String::new()
                            } else {
                                format!(
                                    " (+{} theme{})",
                                    themes.len(),
                                    if themes.len() == 1 { "" } else { "s" }
                                )
                            }
                        ));
                    }
                }
                Ok(crate::plugins::TaskEvent::UpdateFinished { updated, failed }) => {
                    let problems = self.load_plugin_states();
                    let mut msg = if failed.is_empty() {
                        format!(
                            "updated {updated} plugin{}",
                            if updated == 1 { "" } else { "s" }
                        )
                    } else {
                        format!("updated {updated}, failed: {}", failed.join("; "))
                    };
                    if !problems.is_empty() {
                        msg.push_str(&format!(" — ⚠ {}", problems.join("; ")));
                    }
                    self.set_message(&msg);
                    self.plugin_state = crate::plugins::TaskState::Idle;
                    self.reload_installed_plugins()?;
                }
                Ok(crate::plugins::TaskEvent::Installed { name, dir, themes }) => {
                    // Register it (most-recent-first) and refresh the list
                    // so the page updates on the next frame.
                    let source = self.input.text.trim().to_string();
                    let _ = db::add_plugin(&self.db, &name, &source, &dir);
                    // Import any theme packs into the themes table so they
                    // can be activated immediately.
                    for t in &themes {
                        let _ = db::upsert_theme(&self.db, &t.id, &t.name, &source, &t.colors_json);
                    }
                    self.reload_installed_plugins()?;
                    let msg = if themes.is_empty() {
                        format!("installed {name}")
                    } else {
                        format!(
                            "installed {name} (+{} theme{})",
                            themes.len(),
                            if themes.len() == 1 { "" } else { "s" }
                        )
                    };
                    self.set_message(&msg);
                    self.plugin_state = crate::plugins::TaskState::Installed {
                        name,
                        dir,
                        theme_count: themes.len(),
                    };
                    // Done — hand control back to the list.
                    if matches!(
                        self.mode,
                        Mode::PluginManager {
                            pane: PluginPane::Install
                        }
                    ) {
                        self.mode = Mode::PluginManager {
                            pane: PluginPane::List,
                        };
                        self.input.clear();
                    }
                    break;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    // Still running — put the channel back.
                    self.plugin_rx = Some(rx);
                    break;
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.plugin_state = crate::plugins::TaskState::Error("task thread died".into());
                    break;
                }
            }
        }
        Ok(())
    }

    pub fn set_message(&mut self, msg: &str) {
        self.message = Some(msg.to_string());
    }

    pub fn clear_message(&mut self) {
        self.message = None;
    }

    /// Move the selected goal up within its sibling group (swap sort_order).
    pub fn reorder_up(&mut self) -> anyhow::Result<()> {
        let row = match self.selected_row() {
            Some(r) => r,
            None => return Ok(()),
        };
        let id = row.goal.id.clone();
        let parent_id = row.goal.parent_id.clone();
        let mut siblings: Vec<Goal> = self
            .goals
            .iter()
            .filter(|g| g.parent_id == parent_id)
            .cloned()
            .collect();
        siblings.sort_by_key(|g| (g.sort_order, g.created_at.clone()));
        if let Some(i) = siblings.iter().position(|g| g.id == id) {
            if i > 0 {
                let a = siblings[i].clone();
                let b = siblings[i - 1].clone();
                db::update(
                    &self.db,
                    &a.id,
                    UpdateGoalInput {
                        sort_order: Some(b.sort_order),
                        ..Default::default()
                    },
                )?;
                db::update(
                    &self.db,
                    &b.id,
                    UpdateGoalInput {
                        sort_order: Some(a.sort_order),
                        ..Default::default()
                    },
                )?;
                self.reload()?;
                self.move_up();
                self.set_message("reordered up");
            }
        }
        Ok(())
    }

    /// Move the selected goal down within its sibling group (swap sort_order).
    pub fn reorder_down(&mut self) -> anyhow::Result<()> {
        let row = match self.selected_row() {
            Some(r) => r,
            None => return Ok(()),
        };
        let id = row.goal.id.clone();
        let parent_id = row.goal.parent_id.clone();
        let mut siblings: Vec<Goal> = self
            .goals
            .iter()
            .filter(|g| g.parent_id == parent_id)
            .cloned()
            .collect();
        siblings.sort_by_key(|g| (g.sort_order, g.created_at.clone()));
        if let Some(i) = siblings.iter().position(|g| g.id == id) {
            if i < siblings.len() - 1 {
                let a = siblings[i].clone();
                let b = siblings[i + 1].clone();
                db::update(
                    &self.db,
                    &a.id,
                    UpdateGoalInput {
                        sort_order: Some(b.sort_order),
                        ..Default::default()
                    },
                )?;
                db::update(
                    &self.db,
                    &b.id,
                    UpdateGoalInput {
                        sort_order: Some(a.sort_order),
                        ..Default::default()
                    },
                )?;
                self.reload()?;
                self.move_down();
                self.set_message("reordered down");
            }
        }
        Ok(())
    }
}
