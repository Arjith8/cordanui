//! App state for the TUI.
//!
//! Holds the DB connection, the flat goal list, the expanded-node set, the
//! selection index, and the current input mode (normal / inserting text /
//! editing). Input is handled inline in the TUI loop — a modal-style text
//! input field at the bottom of the screen.
//!
//! All shared types (`Mode`, `InputBuffer`, `FlatRow`, etc.) live in
//! [`crate::types`] and are re-exported from here so existing
//! `use crate::app::X` paths continue to work.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use anyhow::Context;
use cordanui_schema::{CreateGoalInput, Goal, GoalStatus, UpdateGoalInput};
use cordanui_sync::Database;
use ratatui::widgets::ListState;

use crate::db;
use crate::plugin_ui::PanelCommand;
use cordanui_plugin_runtime::ui::GoalsHost;
use cordanui_plugin_runtime::{UiRequest, UiResponse};

// Re-export all types so existing `use crate::app::X` paths keep working.
pub use crate::types::{
    format_ago, ActivePluginModal, AgentChoice, FlatRow, HelpTab, InputBuffer, Mode, PluginCall,
    PluginCommand, PluginCommandOutcome, PluginModalKind, PluginPane, SyncStatus, SYNC_INTERVAL,
};

/// The full TUI application state.
pub struct App {
    pub db: Database,
    pub keybinds: crate::config::Keybinds,
    /// Re-resolved whenever [`Self::styles`] reports changes.
    pub theme: crate::theme::Theme,
    pub styles: std::sync::Arc<crate::style::StyleBridge>,
    pub services: std::sync::Arc<crate::services::ServiceManager>,
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
    /// owning plugin), refreshed when states load.
    pub plugin_commands: Vec<PluginCommand>,
    /// active plugin that ships `[[help]]` manifest sections.
    pub help_tabs: Vec<HelpTab>,
    pub help_selected: usize,
    pub help_scroll: usize,
    /// In-flight plugin command result channel + guard.
    pub(crate) command_rx: Option<std::sync::mpsc::Receiver<PluginCommandOutcome>>,
    pub command_running: bool,
    pub command_selected: usize,
    /// Second database handle (same file) used by the sync worker, so
    /// network I/O never blocks the UI thread. `None` = sync not
    /// configured.
    pub(crate) sync_db: Option<std::sync::Arc<std::sync::Mutex<Database>>>,
    pub sync_status: SyncStatus,
    /// Set while a sync worker is running.
    pub(crate) sync_rx: Option<std::sync::mpsc::Receiver<Result<(), String>>>,
    pub(crate) sync_in_flight: bool,
    pub(crate) last_sync_attempt: Option<std::time::Instant>,
    pub goals: Vec<Goal>,
    pub expanded: HashSet<String>,
    pub detailed: Option<String>,
    /// Ratatui list state — tracks selection + scroll offset of the goal list.
    pub list_state: ListState,
    /// Whether the leader key (Ctrl+A) was just pressed and we're waiting
    /// for the command key.
    pub leader_pending: bool,
    pub mode: Mode,
    /// Text input buffer (reused across modes).
    pub input: InputBuffer,
    pub message: Option<String>,
    /// Plugin manager: in-flight task channel (background thread).
    pub plugin_rx: Option<std::sync::mpsc::Receiver<crate::plugins::TaskEvent>>,
    /// Plugin manager: current task state rendered in the popup.
    pub plugin_state: crate::plugins::TaskState,
    pub installed_plugins: Vec<db::PluginRow>,
    pub plugin_selected: usize,
    pub config_spec: Option<cordanui_plugin_runtime::UiSpec>,
    pub global_spec: Option<cordanui_plugin_runtime::UiSpec>,
    pub global_values: std::collections::BTreeMap<String, String>,
    pub global_plugin_entries: Vec<(String, String)>,
    pub config_values: std::collections::BTreeMap<String, String>,
    pub config_selected: usize,
    /// Configure form: in-progress edit buffer (None = not editing).
    pub config_editing: Option<String>,
    pub agent_choices: Vec<AgentChoice>,
    pub agent_selected: usize,
    /// In-flight agent run event channel.
    pub agent_rx: Option<std::sync::mpsc::Receiver<cordanui_plugin_runtime::AgentEvent>>,
    pub agent_log: Vec<String>,
    pub agent_goal: Option<String>,
    pub move_choices: Vec<(Option<String>, String)>,
    pub move_selected: usize,
    /// Goal sheets (buffers) for work/project separation.
    pub sheets: Vec<cordanui_schema::GoalSheet>,
    pub active_sheet_id: Arc<Mutex<Option<String>>>,
    pub sheet_picker_selected: usize,
    /// Plugin-controlled buffers: sheet_id -> PanelSpec (draw/on_key).
    /// When active_buffer_id is Some, that buffer's content is shown instead of goals.
    pub plugin_buffers: Arc<Mutex<HashMap<String, cordanui_plugin_runtime::PanelSpec>>>,
    pub active_buffer_id: Arc<Mutex<Option<String>>>,
    pub sheet_manager: Arc<crate::sheets::SheetManager>,
    pub buffer_manager: Arc<crate::buffers::BufferManager>,
    pub goals_host: Arc<AppGoalsHost>,
}

impl App {
    pub fn new(db: Database) -> anyhow::Result<Self> {
        let goals = db::get_all(&db)?;
        let sheets = db::list_sheets(&db).unwrap_or_default();
        let active_sheet: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let active_buffer: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let buffers: Arc<Mutex<HashMap<String, cordanui_plugin_runtime::PanelSpec>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let sheet_manager = Arc::new(crate::sheets::SheetManager::new(active_sheet.clone()));
        sheet_manager.attach_db(db.clone());
        let buffer_manager = Arc::new(crate::buffers::BufferManager::new(
            buffers.clone(),
            active_buffer.clone(),
        ));
        let styles = std::sync::Arc::new(crate::style::StyleBridge::new());
        let services = std::sync::Arc::new(crate::services::ServiceManager::new());
        let goals_host = Arc::new(AppGoalsHost::new(db.clone(), services.clone()));
        let theme = crate::theme::Theme::resolve(&db, &styles.session_snapshot());
        let plugin_ui = std::sync::Arc::new(crate::plugin_ui::PluginUiBridge::new());
        let mut list_state = ListState::default();
        list_state.select(Some(0));
        let mut app = Self {
            db,
            keybinds: crate::config::Keybinds::default(),
            theme,
            styles,
            services,
            plugin_ui,
            plugin_modal: None,
            plugin_panel: None,
            plugin_states: std::sync::Mutex::new(HashMap::new()),
            plugin_commands: Vec::new(),
            help_tabs: Vec::new(),
            help_selected: 0,
            help_scroll: 0,
            command_rx: None,
            command_running: false,
            command_selected: 0,
            sync_db: None,
            sync_status: SyncStatus::NotConfigured,
            sync_rx: None,
            sync_in_flight: false,
            last_sync_attempt: None,
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
            global_spec: None,
            global_values: Default::default(),
            global_plugin_entries: Vec::new(),
            config_selected: 0,
            config_editing: None,
            agent_choices: Vec::new(),
            agent_selected: 0,
            agent_rx: None,
            agent_log: Vec::new(),
            agent_goal: None,
            move_choices: Vec::new(),
            move_selected: 0,
            sheets,
            active_sheet_id: active_sheet,
            sheet_picker_selected: 0,
            plugin_buffers: buffers,
            active_buffer_id: active_buffer,
            sheet_manager,
            buffer_manager,
            goals_host,
        };
        Ok(app)
    }

    pub fn selected_index(&self) -> usize {
        self.list_state.selected().unwrap_or(0)
    }

    pub fn reload(&mut self) -> anyhow::Result<()> {
        self.goals = db::get_all(&self.db)?;
        let max = self.flat_len_with_dummy().saturating_sub(1);
        let sel = self.selected_index().min(max);
        // When there are no real goals, the dummy at 0 is the only row.
        self.list_state.select(Some(sel));
        Ok(())
    }

    pub fn flat_rows(&self) -> Vec<FlatRow> {
        if self.active_buffer_id.lock().unwrap().is_some() {
            return Vec::new();
        }
        let active_sheet = self.active_sheet_id.lock().unwrap().clone();
        let filtered: Vec<&Goal> = if let Some(active) = active_sheet.as_deref() {
            self.goals
                .iter()
                .filter(|g| g.sheet_id.as_deref() == Some(active))
                .collect()
        } else {
            self.goals.iter().collect()
        };
        let mut by_parent: HashMap<Option<String>, Vec<&Goal>> = HashMap::new();
        for g in filtered {
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

    pub fn selected_row(&self) -> Option<FlatRow> {
        self.flat_rows().get(self.selected_index()).cloned()
    }

    pub fn is_dummy_selected(&self) -> bool {
        self.selected_index() == self.flat_rows().len()
    }

    pub fn flat_len_with_dummy(&self) -> usize {
        self.flat_rows().len() + 1
    }

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
        let max = self.flat_len_with_dummy().saturating_sub(1);
        let cur = self.selected_index();
        if cur < max {
            self.list_state.select(Some(cur + 1));
        }
    }

    pub fn select_first(&mut self) {
        self.list_state.select(Some(0));
    }

    pub fn select_last(&mut self) {
        let max = self.flat_len_with_dummy().saturating_sub(1);
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
    /// If the goal has a repeat_rule and would become Completed, instead
    /// reschedule it to the next interval and keep it pending.
    pub fn cycle_status(&mut self) -> anyhow::Result<()> {
        let Some(row) = self.selected_row() else {
            return Ok(());
        };
        let next = match row.goal.status {
            GoalStatus::Pending => GoalStatus::InProgress,
            GoalStatus::InProgress => GoalStatus::Completed,
            _ => GoalStatus::Pending,
        };
        if next == GoalStatus::Completed {
            if let Some(repeat) = row.goal.repeat_rule.clone().filter(|r| !r.is_empty() && r != "none") {
                if let Some(next_due) = Self::next_due_for_repeat(&repeat) {
                    db::update(
                        &self.db,
                        &row.goal.id,
                        UpdateGoalInput {
                            status: Some(GoalStatus::Pending),
                            due_at: Some(Some(next_due.clone())),
                            completed_at: Some(None),
                            ..Default::default()
                        },
                    )?;
                    self.reload()?;
                    self.set_message(&format!("repeated: next due {}", next_due));
                    return Ok(());
                }
            }
        }
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

    fn next_due_for_repeat(repeat_rule: &str) -> Option<String> {
        let now = chrono::Utc::now();
        let next = match repeat_rule {
            "daily" => now + chrono::Duration::days(1),
            "weekly" => now + chrono::Duration::days(7),
            "monthly" => now + chrono::Duration::days(30),
            "yearly" => now + chrono::Duration::days(365),
            _ => return None,
        };
        Some(next.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
    }

    pub fn toggle_complete(&mut self) -> anyhow::Result<()> {
        if let Some(row) = self.selected_row() {
            let id = row.goal.id.clone();
            if row.goal.status == GoalStatus::Completed {
                db::uncomplete(&self.db, &id)?;
                self.reload()?;
                self.set_message("toggled complete");
            } else if let Some(repeat) = row.goal.repeat_rule.clone().filter(|r| !r.is_empty() && r != "none") {
                if let Some(next_due) = Self::next_due_for_repeat(&repeat) {
                    db::update(
                        &self.db,
                        &id,
                        UpdateGoalInput {
                            status: Some(GoalStatus::Pending),
                            due_at: Some(Some(next_due.clone())),
                            completed_at: Some(None),
                            ..Default::default()
                        },
                    )?;
                    self.reload()?;
                    self.set_message(&format!("repeated: next due {}", next_due));
                } else {
                    db::complete(&self.db, &id)?;
                    self.reload()?;
                    self.set_message("toggled complete");
                }
            } else {
                db::complete(&self.db, &id)?;
                self.reload()?;
                self.set_message("toggled complete");
            }
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
        let sheet_id = self.active_sheet_id.lock().unwrap().clone();
        let sort_order = db::next_sort_order_in_sheet(
            &self.db,
            parent_id.as_deref(),
            sheet_id.as_deref(),
        )?;
        let input = CreateGoalInput {
            title,
            description: None,
            parent_id: parent_id.clone(),
            sheet_id,
            sort_order: Some(sort_order),
            due_at: None,
            remind_at: None,
            repeat_rule: None,
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

    pub fn start_edit_due(&mut self) {
        if let Some(row) = self.selected_row() {
            self.input.text = row.goal.due_at.clone().unwrap_or_default();
            self.input.cursor = self.input.text.len();
            self.mode = Mode::EditDue {
                goal_id: row.goal.id.clone(),
            };
        }
    }

    pub fn commit_edit_due(&mut self) -> anyhow::Result<()> {
        let goal_id = match &self.mode {
            Mode::EditDue { goal_id } => goal_id.clone(),
            _ => return Ok(()),
        };
        let text = self.input.text.trim().to_string();
        let val = if text.is_empty() { None } else { Some(text) };
        db::update(
            &self.db,
            &goal_id,
            UpdateGoalInput {
                due_at: Some(val),
                ..Default::default()
            },
        )?;
        self.reload()?;
        self.set_message("due date updated");
        self.mode = Mode::Normal;
        Ok(())
    }

    pub fn start_edit_reminder(&mut self) {
        if let Some(row) = self.selected_row() {
            self.input.text = row.goal.remind_at.clone().unwrap_or_default();
            self.input.cursor = self.input.text.len();
            self.mode = Mode::EditReminder {
                goal_id: row.goal.id.clone(),
            };
        }
    }

    pub fn commit_edit_reminder(&mut self) -> anyhow::Result<()> {
        let goal_id = match &self.mode {
            Mode::EditReminder { goal_id } => goal_id.clone(),
            _ => return Ok(()),
        };
        let text = self.input.text.trim().to_string();
        let val = if text.is_empty() { None } else { Some(text) };
        db::update(
            &self.db,
            &goal_id,
            UpdateGoalInput {
                remind_at: Some(val),
                ..Default::default()
            },
        )?;
        self.reload()?;
        self.set_message("reminder updated");
        self.mode = Mode::Normal;
        Ok(())
    }

    pub fn start_edit_repeat(&mut self) {
        if let Some(row) = self.selected_row() {
            self.input.text = row.goal.repeat_rule.clone().unwrap_or_default();
            self.input.cursor = self.input.text.len();
            self.mode = Mode::EditRepeat {
                goal_id: row.goal.id.clone(),
            };
        }
    }

    pub fn commit_edit_repeat(&mut self) -> anyhow::Result<()> {
        let goal_id = match &self.mode {
            Mode::EditRepeat { goal_id } => goal_id.clone(),
            _ => return Ok(()),
        };
        let text = self.input.text.trim().to_lowercase();
        let val: Option<String> = if text.is_empty() || text == "none" {
            None
        } else if ["daily", "weekly", "monthly", "yearly"].contains(&text.as_str()) {
            Some(text)
        } else {
            self.set_message("repeat must be one of: none, daily, weekly, monthly, yearly");
            return Ok(());
        };
        db::update(
            &self.db,
            &goal_id,
            UpdateGoalInput {
                repeat_rule: Some(val),
                ..Default::default()
            },
        )?;
        self.reload()?;
        self.set_message("repeat rule updated");
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

    pub fn request_purge(&mut self) {
        if self.config_editing.is_some() {
            return;
        }
        self.mode = Mode::ConfirmPurge;
    }

    pub fn confirm_purge(&mut self) -> anyhow::Result<()> {
        if self.mode != Mode::ConfirmPurge {
            return Ok(());
        }
        db::purge_all(&self.db)?;
        self.reload()?;
        self.theme = crate::theme::Theme::resolve(&self.db, &self.styles.session_snapshot());
        let _ = self.reload_installed_plugins();
        self.set_message("database purged");
        self.mode = Mode::Normal;
        Ok(())
    }

    pub fn open_plugin_manager(&mut self) -> anyhow::Result<()> {
        self.input.clear();
        self.reload_installed_plugins()?;
        self.plugin_selected = 0;
        self.mode = Mode::PluginManager {
            pane: PluginPane::List,
        };
        Ok(())
    }

    pub fn start_install_mode(&mut self) {
        self.input.clear();
        self.mode = Mode::PluginManager {
            pane: PluginPane::Install,
        };
    }

    /// `c` on a selected plugin: load its [ui] spec + stored settings and
    /// open the configure form. Plugins without a [ui] section just report
    /// that there's nothing to configure.
    pub fn attach_plugin_config_db(&mut self, db: Database) {
        self.plugin_ui
            .attach_config_db(std::sync::Arc::new(std::sync::Mutex::new(db)));
    }

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

    pub fn open_global_config(&mut self) {
        use cordanui_plugin_runtime::{UiField, UiSpec};
        let (url, token) = cordanui_sync::read_turso_credentials();
        let spec = UiSpec {
            fields: vec![
                UiField {
                    key: "turso_url".into(),
                    label: "Turso URL".into(),
                    r#type: "text".into(),
                    required: false,
                    default: None,
                    options: vec![],
                },
                UiField {
                    key: "turso_token".into(),
                    label: "Turso token".into(),
                    r#type: "secret".into(),
                    required: false,
                    default: None,
                    options: vec![],
                },
            ],
        };
        let mut values = std::collections::BTreeMap::new();
        values.insert("turso_url".into(), url.unwrap_or_default());
        values.insert("turso_token".into(), token.unwrap_or_default());

        // Plugins that own a configurator are listed automatically — the
        // extension point for this page.
        self.global_plugin_entries.clear();
        let states = self.plugin_states.lock().unwrap();
        for row in &self.installed_plugins {
            if !row.active {
                continue;
            }
            if states.contains_key(&row.id) {
                let desc = cordanui_plugin_runtime::PluginManifest::from_dir(std::path::Path::new(
                    &row.dir,
                ))
                .map(|m| m.plugin.description)
                .unwrap_or_else(|_| "configure".into());
                self.global_plugin_entries.push((row.id.clone(), desc));
            }
        }
        drop(states);

        self.global_spec = Some(spec);
        self.global_values = values;
        self.config_selected = 0;
        self.config_editing = None;
        self.mode = Mode::GlobalConfig;
    }

    pub fn global_row_count(&self) -> usize {
        self.global_spec
            .as_ref()
            .map(|s| s.fields.len())
            .unwrap_or(0)
            + self.global_plugin_entries.len()
            + 1
    }

    pub fn commit_global_field(&mut self) -> anyhow::Result<()> {
        let (Some(spec), Some(buf)) = (&self.global_spec, &self.config_editing) else {
            return Ok(());
        };
        let Some(field) = spec.fields.get(self.config_selected) else {
            return Ok(());
        };
        let value = buf.trim().to_string();
        self.global_values.insert(field.key.clone(), value.clone());

        let url = self
            .global_values
            .get("turso_url")
            .cloned()
            .unwrap_or_default();
        let token = self
            .global_values
            .get("turso_token")
            .cloned()
            .unwrap_or_default();
        if url.is_empty() != token.is_empty() {
            self.set_message("turso url and token must both be set (or both empty for local-only)");
            return Ok(());
        }
        if !url.is_empty() {
            cordanui_sync::write_turso_credentials(&url, &token)?;
            self.set_message("saved — restart to apply sync");
        } else {
            self.set_message("saved (local-only mode)");
        }
        self.config_editing = None;
        Ok(())
    }

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

    /// Leader + run_agent: collect (active agent/provider × model) choices
    /// and open the picker for the selected goal. Any plugin with
    /// `capabilities.provider` or `capabilities.agent` that can handle
    /// `agent-run` is eligible. Provider plugins expand to one entry per
    pub fn open_agent_picker(&mut self, goal_id: String) -> anyhow::Result<()> {
        self.reload_installed_plugins()?;
        let mut choices = Vec::new();

        for p in self.installed_plugins.iter().filter(|p| p.active) {
            let dir = std::path::PathBuf::from(&p.dir);
            let Ok(manifest) = cordanui_plugin_runtime::PluginManifest::from_dir(&dir) else {
                continue;
            };
            if !manifest.capabilities.provider && !manifest.capabilities.agent {
                continue;
            }
            let is_lua = manifest.is_lua();
            let binary = manifest.binary_path(&dir);
            if !is_lua && !binary.exists() {
                continue;
            }
            let mut values = db::get_plugin_settings(&self.db, &manifest.plugin.name)?;
            // Unsaved fields fall back to their manifest defaults so
            // plugins see the authored behavior out of the box (e.g.
            // `open_picker_on_start = "false"`) instead of nil.
            if let Some(ui) = &manifest.ui {
                for f in &ui.fields {
                    if let Some(d) = &f.default {
                        values.entry(f.key.clone()).or_insert_with(|| d.clone());
                    }
                }
            }
            let config = db::settings_to_config(&values);

            // Provider plugins with models expand per-model; everything else
            // (pure agent, provider without models) is a single choice.
            if manifest.capabilities.provider {
                if let Some(provider) = &manifest.provider {
                    if !provider.models.is_empty() {
                        for model in &provider.models {
                            choices.push(AgentChoice {
                                plugin: manifest.plugin.name.clone(),
                                model: Some(model.clone()),
                                binary: binary.clone(),
                                is_lua,
                                config: config.clone(),
                            });
                        }
                        continue;
                    }
                }
            }
            // Pure agent or model-less provider: single entry.
            choices.push(AgentChoice {
                plugin: manifest.plugin.name.clone(),
                model: None,
                binary: binary.clone(),
                is_lua,
                config: config.clone(),
            });
        }

        if choices.is_empty() {
            self.set_message("no active agent/provider plugins (build the binary or install a Lua plugin)");
            return Ok(());
        }

        self.agent_choices = choices;
        self.agent_selected = 0;
        self.mode = Mode::AgentPicker { goal_id };
        Ok(())
    }

    pub fn start_assign_range(&mut self) {
        self.input.clear();
        self.input.text = "@".to_string();
        self.input.cursor = 1;
        self.mode = Mode::AssignRange;
    }

    pub fn commit_assign_range(&mut self) -> anyhow::Result<()> {
        let raw = self.input.text.trim().to_string();
        self.mode = Mode::Normal;
        self.input.clear();
        if raw.is_empty() || raw == "@" {
            self.set_message("assign cancelled — empty range");
            return Ok(());
        }
        // Strip leading @, split on - or .. or space
        let trimmed = raw.trim_start_matches('@').trim().to_string();
        let (start, end) = if let Some((s, e)) = trimmed.split_once('-') {
            (s.trim().to_string(), e.trim().to_string())
        } else if let Some((s, e)) = trimmed.split_once("..") {
            (s.trim().to_string(), e.trim().to_string())
        } else {
            // Single @id or @1
            let single = trimmed.clone();
            (single.clone(), single)
        };
        // Use goals_host to assign (handles numeric 1-based and dotted IDs)
        let ids = self
            .goals_host
            .assign_range_to_agent(&start, &end, None, None)?;
        if ids.is_empty() {
            self.set_message("assign: no goals matched range");
        } else {
            self.reload()?;
            self.set_message(&format!("assigned {} goal(s) @{}-{} to agent", ids.len(), start, end));
        }
        Ok(())
    }

    pub fn start_agent_run(&mut self, goal_id: String) -> anyhow::Result<()> {
        let Some(choice) = self.agent_choices.get(self.agent_selected).cloned() else {
            return Ok(());
        };
        let Some(goal) = self.goals.iter().find(|g| g.id == goal_id).cloned() else {
            return Ok(());
        };
        let title = goal.title.clone();
        let description = goal.description.clone();
        let existing_metadata = goal.metadata.clone();

        db::update(
            &self.db,
            &goal_id,
            UpdateGoalInput {
                agent_status: Some(Some(cordanui_schema::AgentStatus::Running)),
                ..Default::default()
            },
        )?;
        self.reload()?;

        // Persist which agent was chosen in the goal's metadata so the
        // backend (and mobile) can see it after sync, and so a future
        // restart can recover the choice. Keep any existing metadata keys.
        {
            let existing: serde_json::Value = existing_metadata
                .as_deref()
                .and_then(|s| serde_json::from_str(s).ok())
                .unwrap_or(serde_json::Value::Object(Default::default()));
            let mut obj = existing
                .as_object()
                .cloned()
                .unwrap_or_default();
            obj.insert("agent".into(), serde_json::Value::String(choice.plugin.clone()));
            if let Some(m) = &choice.model {
                obj.insert("model".into(), serde_json::Value::String(m.clone()));
            }
            let _ = db::update(
                &self.db,
                &goal_id,
                UpdateGoalInput {
                    metadata: Some(Some(serde_json::Value::Object(obj).to_string())),
                    ..Default::default()
                },
            );
            self.reload()?;
        }

        let (tx, rx) = std::sync::mpsc::channel();
        let cfg = cordanui_plugin_runtime::AgentRunConfig {
            task_id: goal_id.clone(),
            title,
            description,
            model: choice.model.clone(),
            config: choice.config.clone(),
        };
        let binary = choice.binary.clone();
        let plugin_name = choice.plugin.clone();
        let plugin_config = choice.config.clone();
        let is_lua = choice.is_lua;
        // Resolve plugin dir for Lua runs (needed to load main.lua).
        let plugin_dir = self
            .installed_plugins
            .iter()
            .find(|p| p.id == plugin_name)
            .map(|p| std::path::PathBuf::from(&p.dir));

        std::thread::spawn(move || {
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
            let result: anyhow::Result<()> = rt.block_on(async {
                if is_lua {
                    let dir = plugin_dir.ok_or_else(|| anyhow::anyhow!("plugin dir not found for {plugin_name}"))?;
                    let plugin = cordanui_plugin_runtime::LuaPlugin::load(
                        &dir,
                        &plugin_name,
                        plugin_config.clone(),
                        cordanui_plugin_runtime::HostHooks::new(),
                    )?;
                    let tx_lua = tx.clone();
                    let ev = plugin
                        .agent_run(&cfg, move |ev| {
                            let _ = tx_lua.send(ev.clone());
                        })
                        .await?;
                    // Lua agent_run returns the terminal event; if it is an error
                    // that wasn't already sent via the callback (callback already
                    // forwards all events), forward it. Success case already sent.
                    if let cordanui_plugin_runtime::AgentEvent::Error { message, detail } = ev {
                        let _ = tx.send(cordanui_plugin_runtime::AgentEvent::Error { message, detail });
                    }
                    Ok(())
                } else {
                    use cordanui_plugin_runtime::spawn::run_streaming;
                    let tx_bin = tx.clone();
                    run_streaming(&binary, &cfg, move |ev| {
                        let _ = tx_bin.send(ev.clone());
                    })
                    .await?;
                    Ok(())
                }
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
        let label = match &choice.model {
            Some(m) => format!("{} — {}", choice.plugin, m),
            None => choice.plugin.clone(),
        };
        self.agent_log.push(label);
        self.set_message("agent running");
        self.mode = Mode::AgentRunning { goal_id };
        Ok(())
    }

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
                    // Plugin-declared mobile FE changes: files named mobile.json / __metadata__.json
                    // are merged into metadata so mobile's PluginCard renders them.
                    for file in &r.files {
                        if let Some(content) = file.content.as_deref() {
                            if file.path == "__metadata__.json" || file.path.ends_with("/__metadata__.json") {
                                if let Ok(patch) = serde_json::from_str::<serde_json::Value>(content) {
                                    if let Some(goal_id) = self.agent_goal.clone() {
                                        let _ = db::merge_metadata(&self.db, &goal_id, patch);
                                    }
                                }
                            } else if file.path == "mobile.json" || file.path.ends_with("/mobile.json") {
                                if let Ok(val) = serde_json::from_str::<serde_json::Value>(content) {
                                    let patch = if val.get("mobile").is_some() {
                                        serde_json::json!({ "mobile": val.get("mobile").cloned().unwrap_or(val.clone()) })
                                    } else {
                                        serde_json::json!({ "mobile": val })
                                    };
                                    if let Some(goal_id) = self.agent_goal.clone() {
                                        let _ = db::merge_metadata(&self.db, &goal_id, patch);
                                    }
                                }
                            }
                        }
                    }
                    let result_json = serde_json::to_string(&serde_json::json!({
                        "content": r.content,
                        "files": r.files
                    }))
                    .unwrap_or(r.content.clone());
                    self.finish_agent_run("completed", result_json)?;
                    break;
                }
                Ok(cordanui_plugin_runtime::AgentEvent::Error { message, detail }) => {
                    let text = match &detail {
                        Some(d) => format!("{message}: {d}"),
                        None => message.clone(),
                    };
                    self.record_error("agent", &message, detail.as_deref());
                    self.finish_agent_run("failed", text)?;
                    break;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    self.agent_rx = Some(rx);
                    break;
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.record_error("agent", "agent thread died", None);
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

    // ---------- move (reparent) ----------

    fn descendant_ids(&self, root: &str) -> HashSet<String> {
        let mut out = HashSet::new();
        let mut stack = vec![root.to_string()];
        let mut by_parent: HashMap<Option<String>, Vec<String>> = HashMap::new();
        for g in &self.goals {
            by_parent.entry(g.parent_id.clone()).or_default().push(g.id.clone());
        }
        while let Some(cur) = stack.pop() {
            if let Some(children) = by_parent.get(&Some(cur.clone())) {
                for child in children {
                    if out.insert(child.clone()) {
                        stack.push(child.clone());
                    }
                }
            }
        }
        out.remove(root);
        out
    }

    pub fn open_move_picker(&mut self, goal_id: String) -> anyhow::Result<()> {
        let Some(goal) = self.goals.iter().find(|g| g.id == goal_id).cloned() else {
            return Ok(());
        };
        let descendants = self.descendant_ids(&goal.id);
        let mut choices: Vec<(Option<String>, String)> = Vec::new();
        // Root entry first.
        choices.push((None, "∅  (root)".to_string()));
        for g in &self.goals {
            if g.id == goal.id || descendants.contains(&g.id) {
                continue;
            }
            let glyph = match g.status {
                cordanui_schema::GoalStatus::Pending => "○",
                cordanui_schema::GoalStatus::InProgress => "◐",
                cordanui_schema::GoalStatus::Completed => "✓",
                cordanui_schema::GoalStatus::AgentMode => "⤴",
            };
            choices.push((Some(g.id.clone()), format!("{glyph} {}", g.title)));
        }
        if choices.is_empty() {
            self.set_message("nowhere to move");
            return Ok(());
        }
        // Preselect current parent if present.
        let cur_parent = goal.parent_id.clone();
        let sel = choices
            .iter()
            .position(|(id, _)| *id == cur_parent)
            .unwrap_or(0);
        self.move_choices = choices;
        self.move_selected = sel;
        self.mode = Mode::MovePicker { goal_id };
        Ok(())
    }

    pub fn confirm_move(&mut self) -> anyhow::Result<()> {
        let goal_id = match &self.mode {
            Mode::MovePicker { goal_id } => goal_id.clone(),
            _ => return Ok(()),
        };
        let Some((new_parent, label)) = self.move_choices.get(self.move_selected).cloned() else {
            return Ok(());
        };
        // No-op if same parent.
        let cur = self.goals.iter().find(|g| g.id == goal_id).and_then(|g| g.parent_id.clone());
        if cur == new_parent {
            self.set_message("already there");
            self.mode = Mode::Normal;
            return Ok(());
        }
        let new_sheet_id = if let Some(pid) = &new_parent {
            self.goals
                .iter()
                .find(|g| &g.id == pid)
                .and_then(|g| g.sheet_id.clone())
                .or_else(|| self.active_sheet_id.lock().unwrap().clone())
        } else {
            self.active_sheet_id.lock().unwrap().clone()
        };
        let sort = crate::db::next_sort_order_in_sheet(
            &self.db,
            new_parent.as_deref(),
            new_sheet_id.as_deref(),
        )?;
        crate::db::update(
            &self.db,
            &goal_id,
            cordanui_schema::UpdateGoalInput {
                parent_id: Some(new_parent.clone()),
                sheet_id: Some(new_sheet_id.clone()),
                sort_order: Some(sort),
                ..Default::default()
            },
        )?;
        self.reload()?;
        if let Some(pid) = &new_parent {
            self.expanded.insert(pid.clone());
        }
        // Select the moved goal.
        if let Some(idx) = self.flat_rows().iter().position(|r| r.goal.id == goal_id) {
            self.list_state.select(Some(idx));
        }
        self.set_message(&format!("moved to {}", label));
        self.mode = Mode::Normal;
        Ok(())
    }

    // ---------- sheets (buffers) ----------

    pub fn reload_sheets(&mut self) -> anyhow::Result<()> {
        self.sheets = db::list_sheets(&self.db).unwrap_or_default();
        // If active sheet was deleted, fall back to None (All).
        if let Some(active) = self.active_sheet_id.lock().unwrap().clone().as_ref().map(|s| s.clone()) {
            if !self.sheets.iter().any(|s| s.id == *active) {
                *self.active_sheet_id.lock().unwrap() = None;
                *self.active_buffer_id.lock().unwrap() = None;
            }
        }
        Ok(())
    }

    pub fn open_sheet_picker(&mut self) -> anyhow::Result<()> {
        self.reload_sheets()?;
        self.sheet_picker_selected = 0;
        // Preselect active sheet/buffer.
        let active_buffer_clone = self.active_buffer_id.lock().unwrap().clone();
        if let Some(active) = &active_buffer_clone {
            let mut buf_ids: Vec<String> = self.plugin_buffers.lock().unwrap().keys().cloned().collect();
            buf_ids.sort();
            let pos = buf_ids.iter().position(|k| k == active).unwrap_or(0);
            let idx = self.sheets.len() + 1 + pos;
            self.sheet_picker_selected = idx;
        } else if let Some(active) = self.active_sheet_id.lock().unwrap().clone().as_ref() {
            if let Some(pos) = self.sheets.iter().position(|s| &s.id == active) {
                self.sheet_picker_selected = pos + 1; // +1 for "All"
            }
        }
        self.mode = Mode::SheetPicker;
        Ok(())
    }

    pub fn select_sheet_at_picker(&mut self) -> anyhow::Result<()> {
        // 0 = All (no sheet), 1..sheets.len() = sheets, rest = plugin buffers (sorted)
        let idx = self.sheet_picker_selected;
        if idx == 0 {
            *self.active_sheet_id.lock().unwrap() = None;
            *self.active_buffer_id.lock().unwrap() = None;
            self.set_message("sheet: All");
        } else if idx <= self.sheets.len() {
            let sheet = self.sheets[idx - 1].clone();
            *self.active_sheet_id.lock().unwrap() = Some(sheet.id.clone());
            *self.active_buffer_id.lock().unwrap() = None;
            self.set_message(&format!("sheet: {}", sheet.name));
        } else {
            let buf_idx = idx - self.sheets.len() - 1;
            let mut buf_ids: Vec<String> = self.plugin_buffers.lock().unwrap().keys().cloned().collect();
            buf_ids.sort();
            if let Some(buf_id) = buf_ids.get(buf_idx).cloned() {
                *self.active_buffer_id.lock().unwrap() = Some(buf_id.clone());
                *self.active_sheet_id.lock().unwrap() = None;
                self.set_message(&format!("buffer: {}", buf_id));
            }
        }
        self.mode = Mode::Normal;
        self.reload()?;
        Ok(())
    }

    pub fn start_add_sheet(&mut self) {
        self.input.clear();
        self.mode = Mode::AddSheet;
    }

    pub fn commit_add_sheet(&mut self) -> anyhow::Result<()> {
        let name = self.input.text.trim().to_string();
        if name.is_empty() {
            self.set_message("empty name — cancelled");
            self.mode = Mode::Normal;
            return Ok(());
        }
        let sheet = db::create_sheet(&self.db, &name)?;
        self.sheets.push(sheet.clone());
        *self.active_sheet_id.lock().unwrap() = Some(sheet.id.clone());
        *self.active_buffer_id.lock().unwrap() = None;
        self.mode = Mode::Normal;
        self.reload()?;
        self.set_message(&format!("sheet '{}' created", name));
        Ok(())
    }

    pub fn start_delete_sheet(&mut self) -> anyhow::Result<()> {
        let active_sheet = self.active_sheet_id.lock().unwrap().clone();
        let sheet_id = if let Some(active) = active_sheet.as_ref() {
            active.clone()
        } else if self.sheet_picker_selected > 0 && self.sheet_picker_selected <= self.sheets.len() {
            self.sheets[self.sheet_picker_selected - 1].id.clone()
        } else {
            self.set_message("no sheet selected to delete");
            return Ok(());
        };
        self.mode = Mode::ConfirmDeleteSheet { sheet_id };
        Ok(())
    }

    pub fn confirm_delete_sheet(&mut self) -> anyhow::Result<()> {
        let sheet_id = match &self.mode {
            Mode::ConfirmDeleteSheet { sheet_id } => sheet_id.clone(),
            _ => return Ok(()),
        };
        db::delete_sheet(&self.db, &sheet_id)?;
        // Move goals in that sheet to None (or delete?) — for now, orphan them to All.
        // We do not delete goals, just their sheet assignment is now dangling; they will appear in All.
        // Optionally we could UPDATE goals SET sheet_id = NULL WHERE sheet_id = ?.
        let _ = self.db.execute(
            "UPDATE goals SET sheet_id = NULL, updated_at = ? WHERE sheet_id = ?",
            vec![
                cordanui_sync::Value::from(cordanui_schema::now_iso()),
                cordanui_sync::Value::from(sheet_id.clone()),
            ],
        );
        // Mark dirty for sync? The above direct execute doesn't mark. Do it manually.
        for g in self.goals.iter().filter(|g| g.sheet_id.as_deref() == Some(sheet_id.as_str())) {
            let _ = self.db.mark_dirty("goals", &g.id);
        }
        self.reload_sheets()?;
        *self.active_sheet_id.lock().unwrap() = None;
        self.reload()?;
        self.set_message("sheet deleted");
        self.mode = Mode::Normal;
        Ok(())
    }

    /// Plugin API: create a buffer (sheet-like) that a plugin controls.
    /// Returns the buffer id. The buffer appears in the sheet picker and when
    /// selected, its PanelSpec is rendered instead of goals.
    pub fn create_plugin_buffer(&mut self, name: String, spec: cordanui_plugin_runtime::PanelSpec) -> String {
        let id = format!("buffer:{}", name);
        self.plugin_buffers.lock().unwrap().insert(id.clone(), spec);
        // Optionally also create a sheet entry for persistence? For now, buffer is ephemeral.
        id
    }

    pub fn set_plugin_buffer(&mut self, id: &str, spec: cordanui_plugin_runtime::PanelSpec) {
        self.plugin_buffers.lock().unwrap().insert(id.to_string(), spec);
    }

    pub fn remove_plugin_buffer(&mut self, id: &str) {
        self.plugin_buffers.lock().unwrap().remove(id);
        if self.active_buffer_id.lock().unwrap().as_deref() == Some(id) {
            *self.active_buffer_id.lock().unwrap() = None;
        }
    }

    pub fn toggle_selected_service(&mut self) -> anyhow::Result<()> {
        let Some(p) = self.installed_plugins.get(self.plugin_selected) else {
            return Ok(());
        };
        let dir = std::path::PathBuf::from(&p.dir);
        let manifest = cordanui_plugin_runtime::PluginManifest::from_dir(&dir)?;
        let Some(spec) = manifest.service else {
            self.set_message("this plugin declares no [service]");
            return Ok(());
        };
        self.services.register(&p.id, &dir, spec.clone());
        if self.services.is_running(&p.id) {
            self.services.stop_service(&p.id)?;
            self.set_message(&format!("{} service stopped", p.id));
        } else {
            if let Err(e) = self.services.start_registered(&p.id, &[]) {
                let hint = if e.to_string().contains("No such file or directory") {
                    format!(" — '{}' not found in PATH. Install it (e.g. `curl -fsSL https://bun.sh/install | bash` or `brew install oven-sh/bun/bun`) and ensure TUI's PATH includes it", spec.cmd)
                } else {
                    String::new()
                };
                let msg = format!("✖ failed to start {}: {e:#}{hint}", p.id);
                self.record_error("service", "service start failed", Some(&msg));
                self.set_message(&msg);
                return Ok(());
            }
            self.set_message(&format!("{} service started", p.id));
        }
        Ok(())
    }

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

    pub fn reload_installed_plugins(&mut self) -> anyhow::Result<()> {
        self.installed_plugins = db::list_plugins(&self.db)?;
        let max = self.installed_plugins.len().saturating_sub(1);
        self.plugin_selected = self.plugin_selected.min(max);
        Ok(())
    }

    /// Announce agent capability to other clients (mobile) by writing or
    /// clearing `agent.url` in the synced `settings` table.
    ///
    /// When the TUI has at least one active provider plugin with a built
    /// binary (or a Lua provider), it writes a non-empty `agent.url` so
    /// mobile shows the "assign to agent" UI. When no provider is available,
    /// it writes an empty string to hide the UI on other clients.
    ///
    /// The URL itself is the agent backend's wake endpoint — read from the
    pub fn announce_agent_capability(&mut self) -> anyhow::Result<()> {
        let has_provider = self.installed_plugins.iter().any(|p| {
            if !p.active {
                return false;
            }
            let dir = std::path::PathBuf::from(&p.dir);
            let Ok(manifest) = cordanui_plugin_runtime::PluginManifest::from_dir(&dir) else {
                return false;
            };
            if !manifest.capabilities.provider && !manifest.capabilities.agent {
                return false;
            }
            // Lua plugins are always "built" (no build step). Binary plugins
            // need the compiled binary to exist.
            if manifest.is_lua() {
                return true;
            }
            manifest.binary_path(&dir).exists()
        });

        let url = if has_provider {
            // The agent backend URL. Read from settings if a user has
            // configured it, else use the default. This is the wake-and-point
            // endpoint that mobile POSTs to when assigning a task.
            match db::get_setting(&self.db, "agent.url") {
                Some(u) if !u.is_empty() => u,
                _ => "http://localhost:8081".to_string(),
            }
        } else {
            String::new()
        };

        db::set_setting(&self.db, "agent.url", &url)?;
        Ok(())
    }

    /// Activate/deactivate the selected plugin. Activating a theme-capable
    pub fn toggle_plugin_active(&mut self) -> anyhow::Result<()> {
        let Some(p) = self.installed_plugins.get(self.plugin_selected) else {
            return Ok(());
        };
        let id = p.id.clone();
        let dir = std::path::PathBuf::from(&p.dir);
        let activating = !p.active;

        db::set_plugin_active(&self.db, &id, activating)?;
        if !activating {
            // Deactivation stops any service the plugin declared — a
            // plugin shouldn't keep running after it's turned off.
            let _ = self.services.stop_service(&p.id);
        }

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
        self.reload_installed_plugins()?;
        // Re-announce: activating/deactivating a provider changes whether
        // mobile should show the agent UI.
        self.announce_agent_capability()
    }

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
        self.reload_installed_plugins()?;
        self.announce_agent_capability()
    }

    pub fn start_plugin_search(&mut self) {
        let query = self.input.text.trim().to_string();
        if query.is_empty() {
            return;
        }
        self.plugin_rx = Some(crate::plugins::spawn_plugin_task(&query));
        self.plugin_state = crate::plugins::TaskState::Working(Vec::new());
    }

    pub fn poll_plugin_ui_requests(&mut self) {
        // Drain all pending notifies first — they are fire-and-forget and would
        // otherwise be lost when a command's return string overwrites the status
        // line on the next `poll_command_results` tick (e.g. cordanui-chat open
        // notifying "backend not active" then returning "chat opened").
        let mut pending_modal: Option<crate::plugin_ui::PluginUiEvent> = None;
        while let Some(event) = self.plugin_ui.try_take_event() {
            if let crate::plugin_ui::PluginUiEvent::Notify { level, message } = event {
                let prefixed = match level {
                    cordanui_plugin_runtime::UiLevel::Info => message.clone(),
                    cordanui_plugin_runtime::UiLevel::Warn => format!("⚠ {message}"),
                    cordanui_plugin_runtime::UiLevel::Error => format!("✖ {message}"),
                };
                self.set_message(&prefixed);
                // Dump plugin-raised warnings/errors to the shared errors table
                // like every other subsystem (sync, agent, service).
                if level != cordanui_plugin_runtime::UiLevel::Info {
                    self.record_error("plugin", &message, None);
                }
            } else {
                pending_modal = Some(event);
                break;
            }
        }
        let Some(event) = pending_modal else {
            return;
        };
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

    pub fn answer_plugin_modal(&mut self, response: UiResponse) {
        if let Some(modal) = self.plugin_modal.take() {
            let _ = modal.respond.send(response);
        }
        if self.mode == Mode::PluginModal {
            self.cancel();
        }
    }

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

    pub fn plugin_modal_push_char(&mut self, c: char) {
        if let Some(ActivePluginModal { kind, .. }) = &mut self.plugin_modal {
            match kind {
                PluginModalKind::Input { buffer, .. }
                | PluginModalKind::TextEditor { buffer, .. } => buffer.push(c),
                _ => {}
            }
        }
    }

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

    pub fn plugin_modal_newline(&mut self) {
        if let Some(ActivePluginModal {
            kind: PluginModalKind::TextEditor { buffer, .. },
            ..
        }) = &mut self.plugin_modal
        {
            buffer.push('\n');
        }
    }

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

    pub fn close_plugin_panel(&mut self) {
        self.plugin_panel = None;
        if self.mode == Mode::PluginPanel {
            self.cancel();
        }
    }

    pub fn load_plugin_states(&mut self) -> Vec<String> {
        let mut problems = Vec::new();
        let Ok(plugins) = db::list_plugins(&self.db) else {
            return problems;
        };
        {
            let mut states = self.plugin_states.lock().unwrap();
            states.clear();
        }
        self.plugin_commands.clear();

        for row in plugins {
            if !row.active {
                continue;
            }
            let dir = std::path::PathBuf::from(&row.dir);
            let Ok(manifest) = cordanui_plugin_runtime::PluginManifest::from_dir(&dir) else {
                continue;
            };
            // Register any [service] so `cord.services.is_running/start("cordanui-agents")`
            // resolves even before manual `s` — needed for `cordanui-chat` auto-start
            // `main.lua:6` (otherwise "no service registered").
            if let Some(service) = &manifest.service {
                self.services
                    .register(&manifest.plugin.name, &dir, service.clone());
            }
            if !manifest.is_lua() {
                continue;
            }
            // Settings collected from the plugin's Configure form, with
            // manifest defaults filled in for never-saved fields so
            // plugins observe their authored behavior at load time.
            let mut stored =
                db::get_plugin_settings(&self.db, &manifest.plugin.name).unwrap_or_default();
            if let Some(ui) = &manifest.ui {
                for f in &ui.fields {
                    if let Some(d) = &f.default {
                        stored.entry(f.key.clone()).or_insert_with(|| d.clone());
                    }
                }
            }
            let config = db::settings_to_config(&stored);
            let name = manifest.plugin.name.clone();
            match cordanui_plugin_runtime::LuaPlugin::load(
                &dir,
                &name,
                config,
                crate::plugin_ui::plugin_runtime_hooks(
                    &self.styles,
                    &self.plugin_ui,
                    &self.services,
                    &self.sheet_manager,
                    &self.buffer_manager,
                    &self.goals_host,
                ),
            ) {
                Ok(state) => {
                    for cmd in state.list_commands() {
                        self.plugin_commands.push(PluginCommand {
                            plugin_name: name.clone(),
                            name: cmd.name,
                            desc: cmd.desc,
                        });
                    }
                    self.plugin_states
                        .lock()
                        .unwrap()
                        .insert(name, state);
                }
                Err(e) => problems.push(format!("{name}: {e:#}")),
            }
        }
        self.plugin_commands.sort_by(|a, b| a.name.cmp(&b.name));
        // Dump every plugin load problem to the shared errors table like sync/agent/service.
        for p in &problems {
            self.record_error("plugin", "plugin failed to load", Some(p));
        }
        problems
    }

    pub fn open_command_mode(&mut self) {
        let problems = self.load_plugin_states();
        self.input.clear();
        self.command_selected = 0;
        if let Some(first) = problems.first() {
            self.set_message(&format!("✖ {first}"));
        } else if self.plugin_commands.is_empty() {
            self.set_message(
                "no plugin commands (need an active runtime=\"lua\" plugin defining plugin.commands)",
            );
        }
        self.mode = Mode::Command;
    }

    pub fn move_command_selection(&mut self, delta: i32) {
        let len = self.command_matches().len();
        if len == 0 {
            self.command_selected = 0;
            return;
        }
        let cur = self.command_selected as i32;
        let next = (cur + delta).clamp(0, len as i32 - 1);
        self.command_selected = next as usize;
    }

    pub fn clamp_command_selection(&mut self) {
        let len = self.command_matches().len();
        if len == 0 {
            self.command_selected = 0;
        } else if self.command_selected >= len {
            self.command_selected = len - 1;
        }
    }

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

    pub fn execute_plugin_command(&mut self, cmd: &PluginCommand) {
        self.spawn_plugin_call(&cmd.plugin_name, PluginCall::Command(cmd.name.clone()));
    }

    pub fn spawn_plugin_call(&mut self, plugin_name: &str, call: PluginCall) {
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

    pub fn poll_command_results(&mut self) {
        let Some(rx) = &self.command_rx else {
            return;
        };
        match rx.try_recv() {
            Ok(outcome) => {
                let plugin_name = outcome.plugin_name.clone();
                self.command_rx = None;
                self.command_running = false;
                self.plugin_states
                    .lock()
                    .unwrap()
                    .insert(outcome.plugin_name, outcome.state);
                match outcome.result {
                    Ok(Some(msg)) => self.set_message(&msg),
                    Ok(None) => self.set_message("done"),
                    Err(e) => {
                        self.record_error(
                            "plugin",
                            "plugin command failed",
                            Some(&format!("{plugin_name}: {e:#}")),
                        );
                        self.set_message(&format!("✖ {e:#}"));
                    }
                }
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.command_rx = None;
                self.command_running = false;
            }
        }
    }

    pub fn attach_sync_db(&mut self, db: Database) {
        self.sync_db = Some(std::sync::Arc::new(std::sync::Mutex::new(db)));
        self.sync_status = SyncStatus::Syncing;
        self.last_sync_attempt = None;
    }

    pub fn request_sync(&mut self) {
        if self.sync_db.is_none() {
            // No credentials configured — sync is not active. Surface it in
            // the errors view too — a transient status line is easy to miss.
            db::log_error(
                &self.db,
                "sync",
                "sync requested but sync is not active",
                Some("no [turso] credentials configured — add them in the global settings page to enable sync"),
            );
            self.set_message("sync is not active — set [turso] credentials to enable");
            return;
        }
        // Due immediately — even if a sync is currently running, the next
        // poll fires another right after it lands.
        self.last_sync_attempt = None;
        if self.sync_in_flight {
            self.set_message("sync already running");
            return;
        }
        self.set_message("syncing…");
    }

    pub fn poll_sync(&mut self) {
        if let Some(rx) = &self.sync_rx {
            match rx.try_recv() {
                Ok(Ok(())) => {
                    self.sync_status = SyncStatus::Synced {
                        at: std::time::Instant::now(),
                    };
                    self.sync_in_flight = false;
                    self.sync_rx = None;
                }
                Ok(Err(e)) => {
                    self.record_error("sync", "sync failed", Some(&e));
                    self.sync_status = SyncStatus::Failed {
                        at: std::time::Instant::now(),
                        error: e,
                    };
                    self.sync_in_flight = false;
                    self.sync_rx = None;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => return,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.sync_in_flight = false;
                    self.sync_rx = None;
                }
            }
        }
        if self.sync_in_flight {
            return;
        }
        let due = self
            .last_sync_attempt
            .map(|t| t.elapsed() >= SYNC_INTERVAL)
            .unwrap_or(true);
        if !due {
            return;
        }
        let Some(db) = self.sync_db.clone() else {
            return;
        };
        self.last_sync_attempt = Some(std::time::Instant::now());
        self.sync_in_flight = true;
        self.sync_status = SyncStatus::Syncing;
        let (tx, rx) = std::sync::mpsc::channel();
        self.sync_rx = Some(rx);
        std::thread::spawn(move || {
            let result = db.lock().unwrap().sync().map_err(|e| format!("{e:#}"));
            let _ = tx.send(result);
        });
    }

    pub fn record_error(&mut self, context: &str, message: &str, detail: Option<&str>) {
        db::log_error(&self.db, context, message, detail);
    }

    pub fn open_help(&mut self) {
        let _ = self.reload_installed_plugins();

        let mut tabs = vec![HelpTab {
            title: "keybinds".into(),
            plugin: None,
            text: String::new(),
        }];

        for p in &self.installed_plugins {
            if !p.active {
                continue;
            }
            let dir = std::path::PathBuf::from(&p.dir);
            let Ok(manifest) = cordanui_plugin_runtime::PluginManifest::from_dir(&dir) else {
                continue;
            };
            if manifest.help.is_empty() {
                continue;
            }
            // Concatenate sections: heading, rule, wrapped body.
            let mut text = String::new();
            for s in &manifest.help {
                text.push_str(&s.title);
                text.push('\n');
                text.push_str(&"-".repeat(s.title.len().min(60)));
                text.push('\n');
                text.push_str(s.text.trim());
                text.push_str("\n\n");
            }
            tabs.push(HelpTab {
                title: manifest.plugin.name.clone(),
                plugin: Some(p.id.clone()),
                text,
            });
        }

        self.help_tabs = tabs;
        self.help_selected = 0;
        self.help_scroll = 0;
        self.mode = Mode::Help;
    }

    pub fn cycle_help_tab(&mut self, delta: i32) {
        if self.help_tabs.is_empty() {
            return;
        }
        let n = self.help_tabs.len() as i32;
        let next = (self.help_selected as i32 + delta).rem_euclid(n);
        self.help_selected = next as usize;
        self.help_scroll = 0;
    }

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
                    self.record_error("plugin", "plugin task failed", Some(&e));
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
                    for f in &failed {
                        self.record_error("plugin", "plugin update failed", Some(f));
                    }
                    for p in &problems {
                        self.record_error("plugin", "plugin failed to load after update", Some(p));
                    }
                    self.set_message(&msg);
                    self.plugin_state = crate::plugins::TaskState::Idle;
                    self.reload_installed_plugins()?;
                    break;
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

/// Host for `cord.goals` — list and assign goals to agents from Lua (e.g. `@1-6` in chat).
pub struct AppGoalsHost {
    db: Database,
    services: Arc<crate::services::ServiceManager>,
}

impl AppGoalsHost {
    pub fn new(db: Database, services: Arc<crate::services::ServiceManager>) -> Self {
        Self { db, services }
    }
}

impl cordanui_plugin_runtime::ui::GoalsHost for AppGoalsHost {
    fn list_goals(&self) -> Vec<Goal> {
        crate::db::get_all(&self.db).unwrap_or_default()
    }

    fn assign_to_agent(
        &self,
        goal_id: &str,
        agent: Option<String>,
        model: Option<String>,
    ) -> anyhow::Result<()> {
        // Verify goal exists
        let goal = crate::db::get(&self.db, goal_id)?
            .ok_or_else(|| anyhow::anyhow!("goal not found: {goal_id}"))?;
        // Optionally merge agent/model into metadata (like TUI's start_agent_run)
        if agent.is_some() || model.is_some() {
            let existing: serde_json::Value = goal
                .metadata
                .as_deref()
                .and_then(|s| serde_json::from_str(s).ok())
                .unwrap_or(serde_json::Value::Object(Default::default()));
            let mut obj = existing.as_object().cloned().unwrap_or_default();
            if let Some(a) = agent {
                obj.insert("agent".into(), serde_json::Value::String(a));
            }
            if let Some(m) = model {
                obj.insert("model".into(), serde_json::Value::String(m));
            }
            let _ = crate::db::update(
                &self.db,
                goal_id,
                UpdateGoalInput {
                    metadata: Some(Some(serde_json::Value::Object(obj).to_string())),
                    ..Default::default()
                },
            );
        }
        // Queue for agent backend (mobile/TUI poll will pick up `agent_mode/queued`)
        let ts = cordanui_schema::now_iso();
        self.db.execute(
            "UPDATE goals SET status='agent_mode', agent_status='queued', agent_result=NULL, agent_progress=NULL, updated_at=? WHERE id=? AND deleted_at IS NULL",
            vec![cordanui_sync::Value::from(ts), cordanui_sync::Value::from(goal_id)],
        )?;
        self.db.mark_dirty("goals", goal_id)?;
        Ok(())
    }

    fn assign_range_to_agent(
        &self,
        start: &str,
        end: &str,
        agent: Option<String>,
        model: Option<String>,
    ) -> anyhow::Result<Vec<String>> {
        let all = self.list_goals();
        let mut sorted = all;
        sorted.sort_by(|a, b| a.sort_order.cmp(&b.sort_order).then(a.created_at.cmp(&b.created_at)));
        if sorted.is_empty() {
            return Ok(Vec::new());
        }
        // `@1-6` / `@<sno>-<sno>` — sno = 1-based visible serial (sort_order order); numeric try first, uuid/dotted id as fallback
        let start_trim = start.trim_start_matches('@');
        let end_trim = end.trim_start_matches('@');
        let s_num = start_trim.parse::<usize>().ok();
        let e_num = end_trim.parse::<usize>().ok();
        let (lo, hi) = if let (Some(s), Some(e)) = (s_num, e_num) {
            let s0 = (s.saturating_sub(1)).min(sorted.len() - 1);
            let e0 = (e.saturating_sub(1)).min(sorted.len() - 1);
            if s0 <= e0 { (s0, e0) } else { (e0, s0) }
        } else {
            // sno-as-uuid fallback — find positions, allow prefix/suffix match for dotted hierarchy
            let find_idx = |id: &str| {
                sorted
                    .iter()
                    .position(|g| g.id == id || g.id.ends_with(id) || id.ends_with(&g.id) || g.id.contains(id))
                    .unwrap_or(0)
            };
            let s_idx = find_idx(start_trim);
            let e_idx = find_idx(end_trim);
            if s_idx <= e_idx { (s_idx, e_idx) } else { (e_idx, s_idx) }
        };
        let mut assigned = Vec::new();
        for g in sorted.iter().skip(lo).take(hi - lo + 1) {
            self.assign_to_agent(&g.id, agent.clone(), model.clone())?;
            assigned.push(g.id.clone());
        }
        Ok(assigned)
    }
}
