//! App state for the TUI.
//!
//! Holds the DB connection, the flat goal list, the expanded-node set, the
//! selection index, and the current input mode (normal / inserting text /
//! editing). Input is handled inline in the TUI loop — a modal-style text
//! input field at the bottom of the screen.

use std::collections::{HashMap, HashSet};

use cordanui_schema::{CreateGoalInput, Goal, GoalStatus, UpdateGoalInput};
use cordanui_sync::Database;

use crate::db;

/// What the TUI is currently doing. Determines how input is handled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mode {
    /// Normal navigation mode.
    Normal,
    /// Adding a goal. `parent_id` is None for a root goal, Some for a subgoal.
    AddGoal {
        parent_id: Option<String>,
    },
    /// Editing an existing goal's title.
    EditTitle {
        goal_id: String,
    },
    /// Editing an existing goal's description.
    EditDescription {
        goal_id: String,
    },
    /// Confirmation prompt for deleting a goal.
    ConfirmDelete {
        goal_id: String,
    },
    /// Help overlay.
    Help,
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
    /// Resolved theme (from the shared `themes` table) used by the render path.
    pub theme: crate::theme::Theme,
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
}

impl App {
    pub fn new(db: Database) -> anyhow::Result<Self> {
        let goals = db::get_all(&db)?;
        let theme = crate::theme::Theme::load(&db);
        let mut list_state = ListState::default();
        if !goals.is_empty() {
            list_state.select(Some(0));
        }
        Ok(Self {
            db,
            keybinds: crate::config::Keybinds::default(),
            theme,
            goals,
            expanded: HashSet::new(),
            detailed: None,
            list_state,
            leader_pending: false,
            mode: Mode::Normal,
            input: InputBuffer::new(),
            message: None,
        })
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
            list.sort_by(|a, b| a.sort_order.cmp(&b.sort_order).then(a.created_at.cmp(&b.created_at)));
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
                    .map(|children| {
                        children
                            .iter()
                            .all(|c| all_done(c, by_parent, memo))
                    })
                    .unwrap_or(true);
            memo.insert(goal.id.clone(), done);
            done
        }

        let mut memo = HashMap::new();
        self.goals
            .iter()
            .filter(|g| {
                g.status == GoalStatus::Completed
                    && !all_done(g, &by_parent, &mut memo)
            })
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
