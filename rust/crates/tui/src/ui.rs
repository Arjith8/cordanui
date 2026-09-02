//! TUI rendering. Separated from app logic so the render path is pure —
//! it only reads `&App` and writes to a `Frame`.

use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Wrap},
    Frame,
};

use cordanui_schema::GoalStatus;

use crate::app::{ActivePluginModal, App, Mode, PluginModalKind, PluginPane};
use cordanui_plugin_runtime::UiRequest;

/// Status glyph + color for each goal status. Colors are addressed by
/// style-variable name (not fixed fields) so themes and `cord.*`
/// overrides can retarget them like anything else.
fn status_style(status: GoalStatus, c: &Palette) -> (&'static str, Color) {
    let var = match status {
        GoalStatus::Pending => "onSurfaceVariant",
        GoalStatus::InProgress => "primary",
        GoalStatus::Completed => "success",
        GoalStatus::AgentMode => "tertiary",
    };
    let glyph = match status {
        GoalStatus::Pending => "○",
        GoalStatus::InProgress => "◐",
        GoalStatus::Completed => "✓",
        GoalStatus::AgentMode => "⤴",
    };
    // Core vars always resolve; the fallback never triggers.
    (glyph, c.get(var).expect("core style vars always resolve"))
}

use crate::theme::Palette;

/// Render the full UI.
pub fn render(app: &mut App, frame: &mut Frame) {
    let size = frame.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // header
            Constraint::Length(1), // sheets bar
            Constraint::Min(1),    // goal list / plugin buffer
            Constraint::Length(3), // input/status bar
            Constraint::Length(1), // keybinding hint
        ])
        .split(size);

    render_header(app, frame, chunks[0]);
    render_sheets_bar(app, frame, chunks[1]);
    let active_buffer = app.active_buffer_id.lock().unwrap().clone();
    if let Some(buf_id) = active_buffer {
        let maybe_spec = app.plugin_buffers.lock().unwrap().get(&buf_id).cloned();
        if let Some(spec) = maybe_spec {
            render_plugin_buffer(&spec, app, frame, chunks[2]);
        } else {
            render_goal_list(app, frame, chunks[2]);
        }
    } else {
        render_goal_list(app, frame, chunks[2]);
    }
    render_input_bar(app, frame, chunks[3]);
    render_hint_bar(app, frame, chunks[4]);

    // Overlays
    if app.mode == Mode::Help {
        render_help_overlay(app, frame, &app.theme.colors);
    }
    if let Mode::ConfirmDelete { goal_id } = &app.mode {
        render_delete_confirm(frame, goal_id, &app.theme.colors);
    }

    if app.mode == Mode::ConfirmPurge {
        render_purge_confirm(app, frame, &app.theme.colors);
    }
    if let Mode::PluginManager { pane } = &app.mode {
        render_plugin_manager(app, pane.clone(), frame);
    }
    if app.mode == Mode::PluginHelp {
        render_plugin_help(app, frame);
    }
    if let Mode::PluginConfigure { plugin } = &app.mode {
        render_plugin_configure(app, plugin, frame);
    }
    if let Mode::AgentPicker { .. } = &app.mode {
        render_agent_picker(app, frame);
    }
    if let Mode::MovePicker { .. } = &app.mode {
        render_move_picker(app, frame);
    }
    if let Mode::SheetPicker = &app.mode {
        render_sheet_picker(app, frame);
    }
    if let Mode::AddSheet = &app.mode {
        render_add_sheet(app, frame);
    }
    if let Mode::ConfirmDeleteSheet { sheet_id } = &app.mode {
        render_confirm_delete_sheet(frame, sheet_id, &app.theme.colors);
    }
    if let Mode::AgentRunning { goal_id } = &app.mode {
        render_agent_running(app, goal_id, frame);
    }
    if let Mode::PluginModal = &app.mode {
        render_plugin_modal(app, frame);
    }
    if let Mode::PluginPanel = &app.mode {
        render_plugin_panel(app, frame);
    }
    if let Mode::Command = &app.mode {
        render_command_matches(app, frame);
    }
    if let Mode::GlobalConfig = &app.mode {
        render_global_config(app, frame);
    }
    if app.mode == Mode::Stats {
        render_stats(app, frame);
    }
    if let Mode::FullResult { goal_id, scroll } = &app.mode {
        render_full_result(app, goal_id, *scroll, frame);
    }
}

/// The command-line match list: up to 8 commands filtered by the input.
/// The selected entry is highlighted so the palette works as a picker
/// without typing — ↑/↓ moves, Enter runs.
fn render_command_matches(app: &App, frame: &mut Frame) {
    let c = &app.theme.colors;
    let matches = app.command_matches();
    if matches.is_empty() {
        return;
    }
    let selected = app.command_selected.min(matches.len().saturating_sub(1));
    let visible = 8usize;
    let rows = matches.len().min(visible);
    // Keep the selected entry in the visible window.
    let start = if matches.len() <= visible {
        0
    } else if selected < visible / 2 {
        0
    } else if selected + visible / 2 >= matches.len() {
        matches.len() - visible
    } else {
        selected - visible / 2
    };
    let area = centered_rect(60, 30, frame.area());
    let area = Rect {
        x: area.x,
        y: area.y.saturating_sub(20), // sit just above the status line
        width: area.width,
        height: rows as u16 + 2,
    };
    frame.render_widget(Clear, area);

    let lines: Vec<Line> = matches
        .iter()
        .enumerate()
        .skip(start)
        .take(visible)
        .map(|(i, cmd)| {
            let is_selected = i == selected;
            let prefix = if is_selected { "▶ " } else { "  " };
            let name_style = if is_selected {
                Style::default().fg(c.on_primary).bg(c.primary).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(c.primary)
            };
            let desc_style = if is_selected {
                Style::default().fg(c.on_primary).bg(c.primary)
            } else {
                Style::default().fg(c.outline_variant)
            };
            let line = Line::from(vec![
                Span::styled(format!("{prefix}{}", cmd.name), name_style),
                Span::styled(format!("  — {}", cmd.desc), desc_style),
            ]);
            line
        })
        .collect();
    frame.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .title(" Commands ")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(c.outline)),
        ),
        area,
    );
}

/// A plugin-owned panel (`cord.ui.show_panel`): the plugin returns a
/// widget tree each frame; we render it into a centered window and route
/// keys back to it.
fn render_plugin_panel(app: &App, frame: &mut Frame) {
    use cordanui_plugin_runtime::Widget;

    let Some(spec) = app.plugin_panel.as_ref() else {
        return;
    };
    let c = &app.theme.colors;
    let area = centered_rect(60, 70, frame.area());
    frame.render_widget(Clear, area);

    let block = Block::default()
        .title(format!(" {} ", spec.title))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(c.primary));

    // Flatten the widget tree into lines.
    let mut lines: Vec<Line> = Vec::new();
    fn flatten(w: &Widget, c: &Palette, out: &mut Vec<Line>) {
        match w {
            Widget::Text { content, fg, bold } => {
                let mut style = Style::default().fg(fg
                    .as_deref()
                    .and_then(|role| c.get(role))
                    .unwrap_or(c.on_background));
                if *bold {
                    style = style.add_modifier(Modifier::BOLD);
                }
                out.push(Line::from(Span::styled(content.clone(), style)));
            }
            Widget::List { items, highlight } => {
                for (i, item) in items.iter().enumerate() {
                    let style = if Some(i) == *highlight {
                        Style::default().fg(c.on_primary).bg(c.primary)
                    } else {
                        Style::default().fg(c.on_background)
                    };
                    out.push(Line::from(Span::styled(format!("  {item}"), style)));
                }
            }
            Widget::Column { children } => {
                for child in children {
                    flatten(child, c, out);
                }
            }
            Widget::Row { children } => {
                let mut cols: Vec<Vec<Line>> = Vec::new();
                for child in children {
                    let mut sub = Vec::new();
                    flatten(child, c, &mut sub);
                    cols.push(sub);
                }
                let max_h = cols.iter().map(|v| v.len()).max().unwrap_or(0);
                for i in 0..max_h {
                    let mut spans = Vec::new();
                    for (ci, col) in cols.iter().enumerate() {
                        if ci > 0 {
                            spans.push(Span::styled(" │ ", Style::default().fg(c.outline)));
                        }
                        if let Some(line) = col.get(i) {
                            spans.extend(line.spans.clone());
                        }
                    }
                    if !spans.is_empty() {
                        out.push(Line::from(spans));
                    }
                }
            }
        }
    }
    flatten(&(spec.draw)(), c, &mut lines);

    frame.render_widget(Paragraph::new(lines).block(block).wrap(Wrap { trim: false }), area);
}

/// A plugin-requested dialog (`cord.ui.input/confirm/pick`), rendered as
/// a centered overlay. The host owns the widgets; plugins only ever see
/// the answer.
fn render_plugin_modal(app: &App, frame: &mut Frame) {
    let Some(modal) = app.plugin_modal.as_ref() else {
        return;
    };
    let ActivePluginModal { request, kind, .. } = modal;
    let c = &app.theme.colors;

    // Size the box to its content.
    let (pct_w, pct_h) = match kind {
        PluginModalKind::Pick { .. } | PluginModalKind::MultiSelect { .. } => (45, 40),
        PluginModalKind::TextEditor { .. } => (60, 50),
        _ => (50, 22),
    };
    let area = centered_rect(pct_w, pct_h, frame.area());
    frame.render_widget(Clear, area);

    let block = Block::default()
        .title(format!(" {} ", request.title()))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(c.primary));

    let lines: Vec<Line> = match (kind, request) {
        (
            PluginModalKind::Input {
                buffer,
                placeholder,
            },
            UiRequest::Input { .. },
        ) => {
            let shown = if buffer.is_empty() {
                placeholder.clone().unwrap_or_default()
            } else {
                buffer.clone()
            };
            vec![
                Line::from(""),
                Line::from(vec![
                    Span::styled("> ", Style::default().fg(c.primary)),
                    if buffer.is_empty() {
                        Span::styled(shown, Style::default().fg(c.outline_variant))
                    } else {
                        Span::styled(shown, Style::default().fg(c.on_background))
                    },
                    Span::styled("▏", Style::default().fg(c.primary)),
                ]),
            ]
        }
        (PluginModalKind::Confirm, UiRequest::Confirm { message, .. }) => vec![
            Line::from(""),
            Line::from(Span::styled(
                format!("  {message}"),
                Style::default().fg(c.on_background),
            )),
        ],
        (PluginModalKind::Pick { selected }, UiRequest::Pick { items, .. }) => items
            .iter()
            .enumerate()
            .map(|(i, item)| {
                let style = if i == *selected {
                    Style::default().fg(c.on_primary).bg(c.primary)
                } else {
                    Style::default().fg(c.on_background)
                };
                let marker = if i == *selected { "▸ " } else { "  " };
                Line::from(Span::styled(format!("  {marker}{item}"), style))
            })
            .collect(),
        (
            PluginModalKind::MultiSelect { selected, cursor },
            UiRequest::MultiSelect { items, .. },
        ) => items
            .iter()
            .zip(selected)
            .enumerate()
            .map(|(i, (item, on))| {
                let check = if *on { "[x]" } else { "[ ]" };
                let style = if i == *cursor {
                    Style::default().fg(c.on_primary).bg(c.primary)
                } else if *on {
                    Style::default().fg(c.success)
                } else {
                    Style::default().fg(c.on_background)
                };
                Line::from(Span::styled(format!("  {check} {item}"), style))
            })
            .collect(),
        (
            PluginModalKind::TextEditor {
                buffer,
                placeholder,
            },
            UiRequest::Text { .. },
        ) => {
            let body = if buffer.is_empty() {
                placeholder.clone().unwrap_or_default()
            } else {
                buffer.clone()
            };
            let style = if buffer.is_empty() {
                Style::default().fg(c.outline_variant)
            } else {
                Style::default().fg(c.on_background)
            };
            let mut out = vec![Line::from("")];
            for part in body.split('\n') {
                out.push(Line::from(Span::styled(part.to_string(), style)));
            }
            out
        }
        _ => vec![Line::from("")],
    };

    frame.render_widget(Paragraph::new(lines).block(block).wrap(Wrap { trim: false }), area);
}

fn render_header(app: &App, frame: &mut Frame, area: Rect) {
    let c = &app.theme.colors;
    let total = app.goals.len();
    let completed = app
        .goals
        .iter()
        .filter(|g| g.status == GoalStatus::Completed)
        .count();
    let pending = total - completed;

    let title = Span::styled(
        " cordanui ",
        Style::default().fg(c.primary).add_modifier(Modifier::BOLD),
    );
    let theme = Span::styled(
        format!(" · {} ", app.theme.name),
        Style::default().fg(c.outline_variant),
    );
    let stats = Span::styled(
        format!(" {} / {} done · {} pending", completed, total, pending),
        Style::default().fg(c.outline_variant),
    );
    let sync = {
        use crate::app::{format_ago, SyncStatus};
        let (text, style) = match &app.sync_status {
            SyncStatus::NotConfigured => (
                "sync off".to_string(),
                Style::default().fg(c.outline_variant),
            ),
            SyncStatus::Syncing => (
                "syncing…".to_string(),
                Style::default().fg(c.on_surface_variant),
            ),
            SyncStatus::Synced { at } => (
                format!("synced {}", format_ago(*at)),
                Style::default().fg(c.success),
            ),
            SyncStatus::Failed { at, error } => (
                format!(
                    "sync failed {} — {}",
                    format_ago(*at),
                    truncate_str(error, 40)
                ),
                Style::default().fg(c.error),
            ),
        };
        Span::styled(format!(" · {text}"), style)
    };

    let block = Block::default()
        .borders(Borders::BOTTOM)
        .border_style(Style::default().fg(c.outline));

    let line = Line::from(vec![title, theme, stats, sync]);
    let paragraph = Paragraph::new(line).block(block);
    frame.render_widget(&paragraph, area);
}

fn render_goal_list(app: &mut App, frame: &mut Frame, area: Rect) {
    let c = &app.theme.colors;
    let rows = app.flat_rows();
    let partial = app.partially_complete_ids();

    let mut items: Vec<ListItem> = rows
        .iter()
        .map(|row| {
            let indent = "  ".repeat(row.depth);
            let expand_icon = if row.has_children {
                if row.expanded {
                    "▾ "
                } else {
                    "▸ "
                }
            } else {
                "  "
            };
            let (status_glyph, status_color) = if partial.contains(&row.goal.id) {
                // Completed parent with unfinished children — green ringed circle.
                ("◎", c.success)
            } else {
                status_style(row.goal.status, c)
            };

            let title_style = if row.goal.status == GoalStatus::Completed {
                Style::default()
                    .fg(c.outline_variant)
                    .add_modifier(Modifier::CROSSED_OUT)
            } else {
                Style::default().fg(c.on_background)
            };

            let mut spans = vec![
                Span::raw(indent.clone()),
                Span::raw(expand_icon),
                Span::styled(
                    format!("{status_glyph} "),
                    Style::default().fg(status_color),
                ),
                Span::styled(row.goal.title.clone(), title_style),
            ];
            // Due / reminder / repeat badges after title.
            let now_iso = cordanui_schema::now_iso();
            if let Some(due) = &row.goal.due_at {
                let is_overdue = due < &now_iso && row.goal.status != GoalStatus::Completed;
                spans.push(Span::styled(
                    format!("  📅 {}", due),
                    if is_overdue {
                        Style::default().fg(c.error)
                    } else {
                        Style::default().fg(c.tertiary)
                    },
                ));
            }
            if let Some(remind) = &row.goal.remind_at {
                spans.push(Span::styled(
                    format!("  ⏰ {}", remind),
                    Style::default().fg(c.tertiary),
                ));
            }
            if let Some(repeat) = &row.goal.repeat_rule {
                if !repeat.is_empty() && repeat != "none" {
                    spans.push(Span::styled(
                        format!("  ↻ {}", repeat),
                        Style::default().fg(c.tertiary),
                    ));
                }
            }
            let line = Line::from(spans);

            // Description + agent result/progress is only shown while the row's detail view
            // (leader + show_details) is toggled on.
            let mut lines = vec![line];
            if app.detailed.as_deref() == Some(row.goal.id.as_str()) {
                let desc = row
                    .goal
                    .description
                    .clone()
                    .unwrap_or_else(|| "(no description)".to_string());
                lines.push(Line::from(vec![
                    Span::raw(""),
                    Span::styled(
                        format!("{indent}      {desc}"),
                        Style::default().fg(c.on_surface_variant),
                    ),
                ]));
                // Show agent progress/result when in agent_mode
                if let Some(status) = &row.goal.agent_status {
                    let status_str = match status.as_str() {
                        "queued" => "queued",
                        "running" => "running",
                        "completed" => "completed",
                        "failed" => "failed",
                        other => other,
                    };
                    let color = match status.as_str() {
                        "completed" => c.success,
                        "failed" => c.error,
                        "running" => c.primary,
                        _ => c.tertiary,
                    };
                    lines.push(Line::from(vec![
                        Span::raw(""),
                        Span::styled(
                            format!("{indent}      ↳ {status_str}"),
                            Style::default().fg(color).add_modifier(Modifier::BOLD),
                        ),
                    ]));
                    if let Some(prog) = &row.goal.agent_progress {
                        if let Ok(v) = serde_json::from_str::<serde_json::Value>(prog) {
                            let msg = v.get("message").and_then(|m| m.as_str()).unwrap_or(prog.as_str());
                            let detail = v.get("detail").and_then(|d| d.as_str()).unwrap_or("");
                            let text = if detail.is_empty() { msg.to_string() } else { format!("{msg} — {detail}") };
                            for part in text.split('\n').take(3) {
                                lines.push(Line::from(vec![
                                    Span::raw(""),
                                    Span::styled(
                                        format!("{indent}        {part}"),
                                        Style::default().fg(c.on_surface_variant),
                                    ),
                                ]));
                            }
                        }
                    }
                    if let Some(res) = &row.goal.agent_result {
                        if let Ok(v) = serde_json::from_str::<serde_json::Value>(res) {
                            if let Some(content) = v.get("content").and_then(|c| c.as_str()) {
                                for part in content.split('\n').take(8) {
                                    if part.trim().is_empty() { continue; }
                                    lines.push(Line::from(vec![
                                        Span::raw(""),
                                        Span::styled(
                                            format!("{indent}      {part}"),
                                            Style::default().fg(c.on_surface),
                                        ),
                                    ]));
                                }
                                if content.chars().count() > 800 {
                                    lines.push(Line::from(vec![
                                        Span::raw(""),
                                        Span::styled(
                                            format!("{indent}      … (truncated, {} chars)", content.chars().count()),
                                            Style::default().fg(c.outline_variant),
                                        ),
                                    ]));
                                }
                            }
                        }
                    }
                }
            }

            ListItem::new(Text::from(lines))
        })
        .collect();

    // Dummy row at the end for root creation: pointer-based creation requires
    // an explicit place to select for "create at root". When this row is
    // selected, leader+n creates a root goal (parent_id = None).
    {
        let dummy_line = Line::from(vec![
            Span::raw("  "),
            Span::styled("⊕  New root goal", Style::default().fg(c.outline_variant).add_modifier(Modifier::ITALIC)),
            Span::styled(
                "  (select here → leader+n)",
                Style::default().fg(c.outline_variant),
            ),
        ]);
        items.push(ListItem::new(Text::from(dummy_line)));
    }

    let block = Block::default().borders(Borders::NONE);

    let list = List::new(items)
        .block(block)
        .highlight_style(Style::default().bg(c.surface).add_modifier(Modifier::BOLD))
        .highlight_symbol("▶ ");

    // Reuse the App's ListState so selection + scroll offset persist across
    // frames and are tracked by the widget itself.
    frame.render_stateful_widget(list, area, &mut app.list_state);
}

fn render_input_bar(app: &App, frame: &mut Frame, area: Rect) {
    let c = &app.theme.colors;
    // Leader indicator — shown prominently while waiting for a command key.
    let leader_span = if app.leader_pending {
        Span::styled(
            format!(" LEADER ({}) ", app.keybinds.leader.label()),
            Style::default()
                .fg(c.on_primary)
                .bg(c.primary)
                .add_modifier(Modifier::BOLD),
        )
    } else {
        Span::raw("")
    };

    let (label, text) = match &app.mode {
        Mode::Normal => {
            let msg = app.message.clone().unwrap_or_default();
            let label = if msg.is_empty() {
                " NORMAL ".to_string()
            } else {
                format!(" {} ", msg)
            };
            (label, String::new())
        }
        Mode::AddGoal { parent_id } => {
            let prompt = match parent_id {
                Some(_) => " Add subgoal: ",
                None => " Add goal: ",
            };
            (prompt.to_string(), app.input.text.clone())
        }
        Mode::FullResult { .. } => (" Full result ".to_string(), String::new()),
        Mode::EditTitle { .. } => (" Edit title: ".to_string(), app.input.text.clone()),
        Mode::EditDescription { .. } => (" Edit description: ".to_string(), app.input.text.clone()),
        Mode::EditDue { .. } => (" Due: ".to_string(), app.input.text.clone()),
        Mode::EditReminder { .. } => (" Remind: ".to_string(), app.input.text.clone()),
        Mode::EditRepeat { .. } => (" Repeat: ".to_string(), app.input.text.clone()),
        Mode::ConfirmDelete { .. } => (" DELETE ".to_string(), String::new()),
        Mode::ConfirmPurge => (" CONFIRM PURGE ".to_string(), String::new()),
        Mode::Help => (" HELP ".to_string(), String::new()),
        Mode::PluginManager { .. } | Mode::PluginHelp | Mode::PluginConfigure { .. } => {
            if let Some(msg) = &app.message {
                (format!(" {} ", msg), String::new())
            } else {
                (" PLUGIN ".to_string(), app.input.text.clone())
            }
        }
        Mode::AgentPicker { .. } | Mode::AgentRunning { .. } => {
            (" AGENT ".to_string(), String::new())
        }
        Mode::MovePicker { .. } => (" MOVE ".to_string(), String::new()),
        Mode::SheetPicker => (" SHEET ".to_string(), String::new()),
        Mode::AddSheet => (" New sheet: ".to_string(), app.input.text.clone()),
        Mode::ConfirmDeleteSheet { .. } => (" DELETE SHEET ".to_string(), String::new()),
        Mode::PluginModal => {
            let text = app.plugin_modal_text().unwrap_or_default();
            (" PLUGIN DIALOG ".to_string(), text.to_string())
        }
        Mode::PluginPanel => (" PLUGIN PANEL ".to_string(), String::new()),
        Mode::Command => (" COMMAND ".to_string(), app.input.text.clone()),
        Mode::AssignRange => (" Assign @1-6: ".to_string(), app.input.text.clone()),
        Mode::GlobalConfig => {
            let text = if app.config_editing.is_some() {
                app.config_editing.clone().unwrap_or_default()
            } else {
                String::new()
            };
            (" GLOBAL SETTINGS ".to_string(), text)
        }
        Mode::Stats => (" STATS ".to_string(), String::new()),
    };

    let label_style = match &app.mode {
        Mode::Normal => Style::default().fg(c.outline_variant),
        Mode::AddGoal { .. }
        | Mode::EditTitle { .. }
        | Mode::EditDescription { .. }
        | Mode::EditDue { .. }
        | Mode::EditReminder { .. }
        | Mode::EditRepeat { .. } => {
            Style::default().fg(c.primary)
        }
        Mode::PluginManager { .. }
        | Mode::PluginHelp
        | Mode::PluginConfigure { .. }
        | Mode::AgentPicker { .. }
        | Mode::MovePicker { .. }
        | Mode::SheetPicker
        | Mode::AddSheet
        | Mode::AgentRunning { .. }
        | Mode::PluginModal
        | Mode::PluginPanel
        | Mode::FullResult { .. } => Style::default().fg(c.primary),
        Mode::Command | Mode::GlobalConfig | Mode::AssignRange => Style::default().fg(c.secondary),
        Mode::ConfirmDelete { .. } | Mode::ConfirmDeleteSheet { .. } => Style::default().fg(c.error),
        Mode::ConfirmPurge => Style::default().fg(c.error),
        Mode::Help | Mode::Stats | Mode::FullResult { .. } => Style::default().fg(c.primary),
    };

    let block = Block::default()
        .borders(Borders::TOP)
        .border_style(Style::default().fg(c.outline));

    let line = Line::from(vec![
        leader_span,
        Span::styled(label, label_style),
        Span::styled(text, Style::default().fg(c.on_background)),
        if matches!(
            app.mode,
            Mode::AddGoal { .. }
                | Mode::EditTitle { .. }
                | Mode::EditDescription { .. }
                | Mode::EditDue { .. }
                | Mode::EditReminder { .. }
                | Mode::EditRepeat { .. }
                | Mode::PluginManager { .. }
                | Mode::AddSheet
        ) {
            Span::styled("│", Style::default().fg(c.on_surface_variant))
        } else {
            Span::raw("")
        },
    ]);

    let paragraph = Paragraph::new(line).block(block);
    frame.render_widget(&paragraph, area);
}

fn render_hint_bar(app: &App, frame: &mut Frame, area: Rect) {
    let c = &app.theme.colors;
    let hint: String = match &app.mode {
        Mode::Normal => {
            let n = app.keybinds.new_goal.label();
            let d = app.keybinds.show_details.label();
            let h = app.keybinds.help.label();
            let due = app.keybinds.edit_due.label();
            let rem = app.keybinds.edit_reminder.label();
            let rep = app.keybinds.edit_repeat.label();
            if app.leader_pending {
                format!(
                    "leader active — {n} new goal (subgoal if expanded) · {d} details/subgoals · {h} help · Esc cancel"
                )
            } else {
                format!(
                    "leader · leader+{n} new · leader+{d} details · leader+{h} help · {due} due · {rem} remind · {rep} repeat"
                )
            }
        }
        Mode::AddGoal { .. } => "Enter to save · Esc to cancel".into(),
        Mode::EditTitle { .. } => "Enter to save · Esc to cancel".into(),
        Mode::EditDescription { .. } => "Enter to save · Esc to cancel".into(),
        Mode::EditDue { .. } => "Enter to save due date (empty to clear) · Esc to cancel".into(),
        Mode::EditReminder { .. } => "Enter to save reminder (empty to clear) · Esc to cancel".into(),
        Mode::EditRepeat { .. } => "Enter to save (none/daily/weekly/monthly/yearly) · Esc to cancel".into(),
        Mode::ConfirmDelete { .. } => "y to confirm · n/Esc to cancel".into(),
        Mode::ConfirmPurge => "y to purge · n/Esc to cancel".into(),
        Mode::Help => {
            let tabs = if app.help_tabs.len() > 1 {
                " · ←/→ tab · j/k scroll"
            } else {
                " · j/k scroll"
            };
            format!("Esc/q to close{tabs}")
        }
        Mode::PluginManager {
            pane: PluginPane::Install,
        } => "GitHub link / owner/repo / terms · Enter install · Esc back".into(),
        Mode::PluginManager {
            pane: PluginPane::List,
        } => "i install · ↑↓ select · Enter activate · u update · s service · d uninstall · ? help · Esc close".into(),
        Mode::PluginHelp => "Esc/q to close".into(),
        Mode::PluginConfigure { .. } => "↑↓ field · Enter edit · Enter save · Esc back".into(),
        Mode::AgentPicker { .. } => "↑↓ model · Enter run · Esc close".into(),
        Mode::MovePicker { .. } => "↑↓ parent · Enter move · Esc cancel".into(),
        Mode::SheetPicker => "↑↓ sheet · Enter select · n new · d delete · Esc close".into(),
        Mode::AddSheet => "Enter create · Esc cancel".into(),
        Mode::ConfirmDeleteSheet { .. } => "y to confirm · n/Esc to cancel".into(),
        Mode::AgentRunning { .. } => "streaming… Esc hides (run continues)".into(),
        Mode::PluginPanel => "plugin panel — keys go to the plugin".into(),
        Mode::Command => "↑↓ select · type to filter · Enter run · Esc close".into(),
        Mode::AssignRange => "Enter assign (e.g. @1-6 or @id-id) · Esc cancel".into(),
        Mode::GlobalConfig => {
            let field_count = app.global_spec.as_ref().map(|s| s.fields.len()).unwrap_or(0);
            if app.config_editing.is_some() {
                "Enter save · Esc cancel edit".into()
            } else if app.config_selected < field_count {
                "Enter edit · ↑↓ row · Esc close".into()
            } else {
                "Enter open plugin settings · ↑↓ row · Esc close".into()
            }
        }
        Mode::Stats => "Esc/q to close".into(),
        Mode::FullResult { .. } => "j/k scroll · Esc/q close".into(),
        Mode::PluginModal => match app.plugin_modal.as_ref().map(|m| &m.kind) {
            Some(PluginModalKind::Input { .. }) => "type · Enter submit · Esc cancel".into(),
            Some(PluginModalKind::Confirm) => "y confirm · n/Esc cancel".into(),
            Some(PluginModalKind::Pick { .. }) => "↑↓ select · Enter pick · Esc cancel".into(),
            Some(PluginModalKind::MultiSelect { .. }) => {
                "↑↓ move · space toggle · Enter submit · Esc cancel".into()
            }
            Some(PluginModalKind::TextEditor { .. }) => {
                "Enter submit · Shift+Enter newline · Esc cancel".into()
            }
            None => String::new(),
        },
    };

    let line = Line::from(vec![Span::styled(
        format!(" {hint}"),
        Style::default().fg(c.on_surface).add_modifier(Modifier::BOLD),
    )]);
    let paragraph = Paragraph::new(line);
    frame.render_widget(&paragraph, area);
}

fn render_help_overlay(app: &App, frame: &mut Frame, c: &Palette) {
    let area = centered_rect(70, 70, frame.area());
    let tab_hint = if app.help_tabs.len() > 1 {
        " \u{2190}/\u{2192} switch tab"
    } else {
        ""
    };
    let block = Block::default()
        .title(format!(
            " Help{} \u{2014} [keybinds] from ~/.config/cordanui/config.toml ",
            tab_hint
        ))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(c.primary));

    let mut lines = vec![Line::from("")];

    // --- tab bar ---
    {
        let mut spans = vec![Span::raw("  ")];
        for (i, tab) in app.help_tabs.iter().enumerate() {
            if i == app.help_selected {
                spans.push(Span::styled(
                    format!("[ {} ]", tab.title),
                    Style::default().fg(c.primary).add_modifier(Modifier::BOLD),
                ));
            } else {
                spans.push(Span::styled(
                    format!("  {}  ", tab.title),
                    Style::default().fg(c.outline_variant),
                ));
            }
            spans.push(Span::raw(" "));
        }
        lines.push(Line::from(spans));
        lines.push(Line::from(""));
    }

    // --- active tab content ---
    match app.help_tabs.get(app.help_selected).map(|t| t.plugin.as_deref()) {
        Some(None) | None => render_help_keybinds_tab(app, c, &mut lines),
        Some(Some(_)) => {
            if let Some(tab) = app.help_tabs.get(app.help_selected) {
                let raw: Vec<&str> = tab.text.lines().collect();
                for (i, line) in raw.iter().enumerate() {
                    let next_is_rule = raw
                        .get(i + 1)
                        .map(|n| !n.is_empty() && n.chars().all(|ch| ch == '-'))
                        .unwrap_or(false);
                    if !line.is_empty() && next_is_rule {
                        // Section heading (the rule line under it is skipped).
                        lines.push(Line::from(Span::styled(
                            format!("  {line}"),
                            Style::default().fg(c.primary).add_modifier(Modifier::BOLD),
                        )));
                    } else if !line.is_empty() && line.chars().all(|ch| ch == '-') {
                        continue;
                    } else if line.is_empty() {
                        lines.push(Line::from(""));
                    } else {
                        lines.push(Line::from(Span::styled(
                            format!("  {line}"),
                            Style::default().fg(c.on_background),
                        )));
                    }
                }
            }
        }
    }

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "  To rebind: edit [keybinds] in ~/.config/cordanui/config.toml",
        Style::default().fg(c.outline_variant),
    )));

    let paragraph = Paragraph::new(lines)
        .block(block)
        .style(Style::default().fg(c.on_background))
        .scroll((app.help_scroll as u16, 0))
        .wrap(Wrap { trim: false });
    frame.render_widget(&paragraph, area);
}

/// The built-in "keybinds" tab content: configured bindings + fixed keys.
/// (The `run_agent` binding is intentionally absent — agent runs are a
/// plugin-facilitated feature; see config.rs::entries.)
fn render_help_keybinds_tab(app: &App, c: &Palette, lines: &mut Vec<Line>) {
    lines.push(Line::from(vec![Span::styled(
        "  Configured keybinds",
        Style::default().fg(c.primary).add_modifier(Modifier::BOLD),
    )]));
    lines.push(Line::from(Span::styled(
        "  (values marked \u{b7}default\u{b7} come from the built-ins, others were set in config.toml)",
        Style::default().fg(c.outline_variant),
    )));
    lines.push(Line::from(""));

    for entry in app.keybinds.entries() {
        let origin = if entry.is_default { "default" } else { "custom" };
        lines.push(Line::from(vec![
            Span::styled(
                format!("    {:<14}", entry.binding.label()),
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("{:<46}", entry.desc),
                Style::default().fg(c.on_background),
            ),
            Span::styled(
                format!("{} ({origin})", entry.name),
                if entry.is_default {
                    Style::default().fg(c.outline_variant)
                } else {
                    Style::default().fg(c.success)
                },
            ),
        ]));
    }

    lines.extend([
        Line::from(""),
        Line::from(vec![Span::styled(
            "  Fixed keys",
            Style::default().fg(c.primary).add_modifier(Modifier::BOLD),
        )]),
        Line::from("    j / k, arrows   move selection"),
        Line::from("    <leader>q       quit"),
        Line::from("    C-d / C-c       quit (always active)"),
        Line::from("    Esc             cancel leader / clear message"),
    ]);
}
fn render_delete_confirm(frame: &mut Frame, goal_id: &str, c: &Palette) {
    let area = centered_rect(50, 25, frame.area());
    frame.render_widget(Clear, area);
    let block = Block::default()
        .title(" Confirm Delete ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(c.error));

    let lines = vec![
        Line::from(""),
        Line::from("  Delete this goal and all its subgoals?"),
        Line::from(""),
        Line::from(vec![Span::styled(
            format!("  id: {goal_id}"),
            Style::default().fg(c.outline_variant),
        )]),
        Line::from(""),
        Line::from(vec![
            Span::styled(
                "  y",
                Style::default().fg(c.error).add_modifier(Modifier::BOLD),
            ),
            Span::raw(" to confirm · "),
            Span::styled("n", Style::default().fg(c.outline_variant)),
            Span::raw(" to cancel"),
        ]),
    ];

    let paragraph = Paragraph::new(lines)
        .block(block)
        .style(Style::default().fg(c.on_background));
    frame.render_widget(&paragraph, area);
}

/// Spinner frames for in-progress popups (advanced on a 200ms tick).
const SPINNER: [&str; 6] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴"];

/// Plugin manager popup.
///
/// - List pane: installed plugins only, nothing else competing for space.
/// - Install pane (`i`): an accent-bordered input box overlays the top with
///   live git progress streaming beneath it; on success the popup returns
///   to the list, which now contains the new entry.
fn render_plugin_manager(app: &mut App, pane: PluginPane, frame: &mut Frame) {
    let c = &app.theme.colors;
    let area = centered_rect(76, 72, frame.area());
    frame.render_widget(Clear, area);

    let outer = Block::default()
        .title(" Plugin Manager ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(c.primary));
    let inner = outer.inner(area);
    frame.render_widget(&outer, area);

    match pane {
        PluginPane::Install => {
            // Input box on top…
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Length(3), Constraint::Min(1)])
                .split(inner);
            let input_block = Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(c.primary).add_modifier(Modifier::BOLD))
                .title(Span::styled(
                    " install — GitHub link / owner/repo / terms ",
                    Style::default().fg(c.primary),
                ));
            let placeholder = if app.input.text.is_empty() {
                Span::styled(
                    "https://github.com/…",
                    Style::default().fg(c.outline_variant),
                )
            } else {
                Span::raw("")
            };
            let line = Line::from(vec![
                Span::styled(
                    format!(" {}", app.input.text.clone()),
                    Style::default().fg(c.on_background),
                ),
                placeholder,
                Span::styled("│", Style::default().fg(c.primary)),
            ]);
            frame.render_widget(Paragraph::new(line).block(input_block), chunks[0]);

            // …live task status below it.
            let mut lines = Vec::new();
            push_task_status(&app.plugin_state, c, &mut lines);
            if lines.is_empty() {
                lines.push(Line::from(Span::styled(
                    "  Enter to start the install — Esc to cancel.",
                    Style::default().fg(c.outline_variant),
                )));
            }
            frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), chunks[1]);
        }

        PluginPane::List => {
            // Last task outcome (if any) shown briefly above the list.
            let mut lines = Vec::new();
            push_task_status(&app.plugin_state, c, &mut lines);
            if !lines.is_empty() {
                lines.push(Line::from(""));
            }

            if app.installed_plugins.is_empty() {
                lines.push(Line::from(Span::styled(
                    "  Nothing installed yet. Press i to install a plugin.",
                    Style::default().fg(c.outline_variant),
                )));
            } else {
                for (i, p) in app.installed_plugins.iter().enumerate() {
                    let selected = i == app.plugin_selected;
                    let cursor = if selected { "▶ " } else { "  " };
                    let style = if selected {
                        Style::default()
                            .fg(c.on_background)
                            .add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(c.on_background)
                    };
                    let state = if p.active { "[active]" } else { "[     ]" };
                    lines.push(Line::from(vec![
                        Span::styled(format!(" {cursor}"), Style::default().fg(c.primary)),
                        Span::styled(format!("{:<18}", truncate_str(&p.id, 17)), style),
                        Span::styled(
                            format!("{:>8} ", state),
                            if p.active {
                                Style::default().fg(c.success)
                            } else {
                                Style::default().fg(c.outline_variant)
                            },
                        ),
                        Span::styled(
                            truncate_str(&p.source, 34),
                            Style::default().fg(c.outline_variant),
                        ),
                    ]));
                }
            }

            frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
        }
    }
}

/// Render the current task state as log lines into `lines`.
fn push_task_status(
    state: &crate::plugins::TaskState,
    c: &Palette,
    lines: &mut Vec<Line<'static>>,
) {
    match state {
        crate::plugins::TaskState::Idle => {}
        crate::plugins::TaskState::Working(log) => {
            let f = SPINNER[(std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() / 200 % (SPINNER.len() as u128))
                .unwrap_or(0) as usize)];
            lines.push(Line::from(Span::styled(
                format!(" {f} working…"),
                Style::default().fg(c.primary).add_modifier(Modifier::BOLD),
            )));
            for entry in log
                .iter()
                .rev()
                .take(8)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
            {
                lines.push(Line::from(Span::styled(
                    format!("   {entry}"),
                    Style::default().fg(c.on_surface_variant),
                )));
            }
        }
        crate::plugins::TaskState::Installed {
            name,
            dir,
            theme_count,
        } => {
            lines.push(Line::from(Span::styled(
                format!(
                    " ✓ Installed {name}{}",
                    if *theme_count > 0 {
                        format!(
                            " (+{} theme{})",
                            theme_count,
                            if *theme_count == 1 { "" } else { "s" }
                        )
                    } else {
                        String::new()
                    }
                ),
                Style::default().fg(c.success).add_modifier(Modifier::BOLD),
            )));
            lines.push(Line::from(Span::styled(
                format!("   at {dir}"),
                Style::default().fg(c.outline_variant),
            )));
        }
        crate::plugins::TaskState::Results(repos) => {
            for r in repos {
                lines.push(Line::from(Span::styled(
                    format!(" {}", r.summary()),
                    Style::default().fg(c.on_background),
                )));
            }
        }
        crate::plugins::TaskState::NotFound(q) => {
            lines.push(Line::from(Span::styled(
                format!(" No repo found for '{q}'."),
                Style::default().fg(c.error),
            )));
        }
        crate::plugins::TaskState::Error(e) => {
            lines.push(Line::from(Span::styled(
                format!(" Error: {e}"),
                Style::default().fg(c.error),
            )));
        }
    }
}

/// Configure form for a plugin's declarative [ui] settings.
fn render_plugin_configure(app: &App, plugin: &str, frame: &mut Frame) {
    let c = &app.theme.colors;
    let area = centered_rect(64, 60, frame.area());
    frame.render_widget(Clear, area);

    let outer = Block::default()
        .title(format!(" Configure — {plugin} "))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(c.primary));
    let inner = outer.inner(area);
    frame.render_widget(&outer, area);

    let Some(spec) = &app.config_spec else { return };

    let any_select = spec
        .fields
        .iter()
        .any(|f| f.r#type == "select" && !f.options.is_empty());
    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::from(Span::styled(
        if any_select {
            "  ↑↓ field · Tab cycle select · Enter edit · Esc back"
        } else {
            "  ↑↓ select · Enter edit · Enter save · Esc back"
        },
        Style::default().fg(c.outline_variant),
    )));
    lines.push(Line::from(""));

    for (i, f) in spec.fields.iter().enumerate() {
        let selected = i == app.config_selected;
        let cursor = if selected { "▶ " } else { "  " };
        let value = app.config_values.get(&f.key).cloned().unwrap_or_default();

        // While editing THIS field, show the live buffer.
        let is_select = f.r#type == "select" && !f.options.is_empty();
        let shown = if selected && app.config_editing.is_some() {
            format!("{}│", app.config_editing.as_deref().unwrap_or(""))
        } else if is_select {
            // Cycle affordance: ◂ value ▸
            format!(
                "◂ {} ▸",
                if value.is_empty() {
                    "(not set)"
                } else {
                    &value
                }
            )
        } else if f.r#type == "secret" && !value.is_empty() {
            "•".repeat(value.chars().count().min(24))
        } else if value.is_empty() {
            "(not set)".to_string()
        } else {
            value.clone()
        };
        let value_style = if selected && app.config_editing.is_some() {
            Style::default().fg(c.primary).add_modifier(Modifier::BOLD)
        } else if is_select && selected {
            Style::default()
                .fg(c.secondary)
                .add_modifier(Modifier::BOLD)
        } else if value.is_empty() {
            Style::default().fg(c.outline_variant)
        } else {
            Style::default().fg(c.on_background)
        };

        let mut marks = Vec::new();
        if f.required {
            marks.push("*");
        }
        marks.push(f.r#type.as_str());

        lines.push(Line::from(vec![
            Span::styled(format!(" {cursor}"), Style::default().fg(c.primary)),
            Span::styled(
                format!("{:<14}", truncate_str(&f.label, 13)),
                if selected {
                    Style::default()
                        .fg(c.on_background)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(c.on_background)
                },
            ),
            Span::styled(shown, value_style),
            Span::styled(
                format!("  {}", marks.join(" · ")),
                Style::default().fg(c.outline_variant),
            ),
        ]));
    }

    let _ = plugin;
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
}

/// Global settings: host-owned Sync section (Turso credentials) plus one
/// entry per plugin that owns a configurator — the plugin extension point
/// for this page.
fn render_global_config(app: &App, frame: &mut Frame) {
    let c = &app.theme.colors;
    let area = centered_rect(64, 60, frame.area());
    frame.render_widget(Clear, area);
    let outer = Block::default()
        .title(" Global Settings ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(c.primary));
    let inner = outer.inner(area);
    frame.render_widget(&outer, area);

    let Some(spec) = &app.global_spec else { return };
    let mut lines: Vec<Line> = Vec::new();

    lines.push(Line::from(Span::styled(
        "  Sync",
        Style::default()
            .fg(c.secondary)
            .add_modifier(Modifier::BOLD),
    )));
    for (i, f) in spec.fields.iter().enumerate() {
        let selected = app.config_selected == i;
        let cursor = if selected { "▶ " } else { "  " };
        let raw = app.global_values.get(&f.key).cloned().unwrap_or_default();
        let shown = if app.config_selected == i && app.config_editing.is_some() {
            format!("{}│", app.config_editing.as_deref().unwrap_or(""))
        } else if f.r#type == "secret" && !raw.is_empty() {
            "•".repeat(raw.chars().count().min(24))
        } else if raw.is_empty() {
            "(not set — local-only)".to_string()
        } else {
            raw.clone()
        };
        let style = if app.config_selected == i && app.config_editing.is_some() {
            Style::default().fg(c.primary).add_modifier(Modifier::BOLD)
        } else if raw.is_empty() {
            Style::default().fg(c.outline_variant)
        } else {
            Style::default().fg(c.on_background)
        };
        lines.push(Line::from(vec![
            Span::styled(format!(" {cursor}"), Style::default().fg(c.primary)),
            Span::styled(
                format!("{:<14}", truncate_str(&f.label, 13)),
                if selected {
                    Style::default()
                        .fg(c.on_background)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(c.on_background)
                },
            ),
            Span::styled(shown, style),
        ]));
    }

    if !app.global_plugin_entries.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "  Plugins",
            Style::default()
                .fg(c.secondary)
                .add_modifier(Modifier::BOLD),
        )));
        for (i, (name, desc)) in app.global_plugin_entries.iter().enumerate() {
            let selected = app.config_selected == spec.fields.len() + i;
            let cursor = if selected { "▶ " } else { "  " };
            let style = if selected {
                Style::default().fg(c.on_primary).bg(c.primary)
            } else {
                Style::default().fg(c.on_background)
            };
            lines.push(Line::from(vec![
                Span::styled(format!(" {cursor}"), Style::default().fg(c.primary)),
                Span::styled(format!("{:<14}", truncate_str(name, 13)), style),
                Span::styled(
                    desc.clone(),
                    if selected {
                        Style::default().fg(c.on_primary).bg(c.primary)
                    } else {
                        Style::default().fg(c.outline_variant)
                    },
                ),
            ]));
        }
    }

    // --- danger zone: purge row (last selectable row) ---
    {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "  Danger zone",
            Style::default().fg(c.error).add_modifier(Modifier::BOLD),
        )));
        let purge_idx = spec.fields.len() + app.global_plugin_entries.len();
        let selected = app.config_selected == purge_idx;
        let cursor = if selected { "▶ " } else { "  " };
        lines.push(Line::from(vec![
            Span::styled(
                format!(" {cursor}"),
                if selected {
                    Style::default().fg(c.error).add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(c.outline_variant)
                },
            ),
            Span::styled(
                "Purge database",
                if selected {
                    Style::default().fg(c.error).add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(c.on_background)
                },
            ),
            Span::styled(
                "  wipe ALL data — goals, themes, settings, plugins",
                Style::default().fg(c.outline_variant),
            ),
        ]));
    }

    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
}

/// Purge confirmation dialog (danger zone). Requires an explicit `y`.
fn render_purge_confirm(app: &App, frame: &mut Frame, c: &Palette) {
    let area = centered_rect(56, 30, frame.area());
    frame.render_widget(Clear, area);
    let block = Block::default()
        .title(" Confirm Purge ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(c.error));

    let lines = vec![
        Line::from(""),
        Line::from("  Delete ALL data and start fresh?"),
        Line::from(""),
        Line::from(vec![Span::styled(
            "  goals · themes · settings · plugins · error log",
            Style::default().fg(c.error),
        )]),
        Line::from(""),
        Line::from(Span::styled(
            "  This cannot be undone.",
            Style::default().fg(c.outline_variant),
        )),
        Line::from(""),
        Line::from(vec![
            Span::styled(
                "  y",
                Style::default().fg(c.error).add_modifier(Modifier::BOLD),
            ),
            Span::raw(" to purge · "),
            Span::styled("n", Style::default().fg(c.outline_variant)),
            Span::raw(" to cancel"),
        ]),
    ];

    let paragraph = Paragraph::new(lines)
        .block(block)
        .style(Style::default().fg(c.on_background));
    frame.render_widget(&paragraph, area);
}

/// Agent picker: choose a provider × model for the selected goal.
fn render_agent_picker(app: &App, frame: &mut Frame) {
    let c = &app.theme.colors;
    let area = centered_rect(56, 60, frame.area());
    frame.render_widget(Clear, area);
    let outer = Block::default()
        .title(" Run with agent ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(c.primary));
    let inner = outer.inner(area);
    frame.render_widget(&outer, area);

    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::from(Span::styled(
        "  Active provider models",
        Style::default().fg(c.outline_variant),
    )));
    lines.push(Line::from(""));

    if app.agent_choices.is_empty() {
        lines.push(Line::from(Span::styled(
            "  No active provider plugins.",
            Style::default().fg(c.error),
        )));
    }
    for (i, ch) in app.agent_choices.iter().enumerate() {
        let selected = i == app.agent_selected;
        let cursor = if selected { "▶ " } else { "  " };
        let style = if selected {
            Style::default()
                .fg(c.on_background)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(c.on_background)
        };
        let model_label = ch.model.as_deref().unwrap_or("—");
        lines.push(Line::from(vec![
            Span::styled(format!(" {cursor}"), Style::default().fg(c.primary)),
            Span::styled(format!("{:<24}", truncate_str(model_label, 23)), style),
            Span::styled(ch.plugin.clone(), Style::default().fg(c.outline_variant)),
        ]));
    }

    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
}

fn render_move_picker(app: &App, frame: &mut Frame) {
    let c = &app.theme.colors;
    let area = centered_rect(60, 60, frame.area());
    frame.render_widget(Clear, area);
    let outer = Block::default()
        .title(" Move to… ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(c.primary));
    let inner = outer.inner(area);
    frame.render_widget(&outer, area);

    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::from(Span::styled(
        "  Choose new parent (∅ = root)",
        Style::default().fg(c.outline_variant),
    )));
    lines.push(Line::from(""));

    for (i, (_, label)) in app.move_choices.iter().enumerate() {
        let selected = i == app.move_selected;
        let cursor = if selected { "▶ " } else { "  " };
        let style = if selected {
            Style::default()
                .fg(c.on_background)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(c.on_background)
        };
        lines.push(Line::from(vec![
            Span::styled(format!(" {cursor}"), Style::default().fg(c.primary)),
            Span::styled(truncate_str(label, 50), style),
        ]));
    }

    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
}

fn render_sheets_bar(app: &App, frame: &mut Frame, area: Rect) {
    let c = &app.theme.colors;
    let mut spans = Vec::new();
    // "All" tab
    let all_active = app.active_sheet_id.lock().unwrap().is_none() && app.active_buffer_id.lock().unwrap().is_none();
    spans.push(Span::styled(
        " All ",
        if all_active {
            Style::default().fg(c.on_primary).bg(c.primary).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(c.outline_variant)
        },
    ));
    spans.push(Span::raw(" "));
    for sheet in &app.sheets {
        let active_sheet = app.active_sheet_id.lock().unwrap().clone();
        let active = active_sheet.as_deref() == Some(sheet.id.as_str());
        spans.push(Span::styled(
            format!(" {} ", sheet.name),
            if active {
                Style::default().fg(c.on_primary).bg(c.primary).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(c.on_background)
            },
        ));
        spans.push(Span::raw(" "));
    }
    // Plugin buffers as sheets
    let mut buf_ids: Vec<String> = app.plugin_buffers.lock().unwrap().keys().cloned().collect();
    buf_ids.sort();
    for buf_id in buf_ids {
        let active_buf = app.active_buffer_id.lock().unwrap().clone();
        let active = active_buf.as_deref() == Some(buf_id.as_str());
        let label = buf_id.strip_prefix("buffer:").unwrap_or(&buf_id);
        spans.push(Span::styled(
            format!(" {} ", label),
            if active {
                Style::default().fg(c.on_primary).bg(c.tertiary).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(c.tertiary)
            },
        ));
        spans.push(Span::raw(" "));
    }
    let line = Line::from(spans);
    let block = Block::default().borders(Borders::BOTTOM).border_style(Style::default().fg(c.outline));
    frame.render_widget(Paragraph::new(line).block(block), area);
}

fn render_sheet_picker(app: &App, frame: &mut Frame) {
    let c = &app.theme.colors;
    let area = centered_rect(60, 60, frame.area());
    frame.render_widget(Clear, area);
    let outer = Block::default()
        .title(" Sheets — buffers ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(c.primary));
    let inner = outer.inner(area);
    frame.render_widget(&outer, area);
    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::from(Span::styled(
        "  All = show all goals · sheets isolate goals · buffers are plugin UIs",
        Style::default().fg(c.outline_variant),
    )));
    lines.push(Line::from(""));
    // Build combined list: All + sheets + buffers
    let mut entries: Vec<(String, String, bool)> = Vec::new(); // (id, label, is_buffer)
    entries.push(("__all__".to_string(), "∅  All".to_string(), false));
    for s in &app.sheets {
        entries.push((s.id.clone(), format!("▭  {}", s.name), false));
    }
    let mut buf_ids: Vec<String> = app.plugin_buffers.lock().unwrap().keys().cloned().collect();
    buf_ids.sort();
    for bid in buf_ids {
        let label = bid.strip_prefix("buffer:").unwrap_or(&bid);
        entries.push((bid.clone(), format!("◈  {} (plugin)", label), true));
    }
    for (i, (_, label, is_buf)) in entries.iter().enumerate() {
        let selected = i == app.sheet_picker_selected;
        let cursor = if selected { "▶ " } else { "  " };
        let style = if selected {
            Style::default().fg(c.on_background).add_modifier(Modifier::BOLD)
        } else if *is_buf {
            Style::default().fg(c.tertiary)
        } else {
            Style::default().fg(c.on_background)
        };
        lines.push(Line::from(vec![
            Span::styled(format!(" {cursor}"), Style::default().fg(c.primary)),
            Span::styled(truncate_str(label, 40), style),
        ]));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "  n new sheet · d delete sheet · Enter select · Esc close",
        Style::default().fg(c.outline_variant),
    )));
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
}

fn render_add_sheet(app: &App, frame: &mut Frame) {
    let c = &app.theme.colors;
    let area = centered_rect(50, 22, frame.area());
    frame.render_widget(Clear, area);
    let block = Block::default()
        .title(" New sheet ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(c.primary));
    let inner = block.inner(area);
    frame.render_widget(&block, area);
    let line = Line::from(vec![
        Span::styled("> ", Style::default().fg(c.primary)),
        Span::styled(app.input.text.clone(), Style::default().fg(c.on_background)),
        Span::styled("│", Style::default().fg(c.primary)),
    ]);
    frame.render_widget(Paragraph::new(vec![Line::from(""), line]), inner);
}

fn render_confirm_delete_sheet(frame: &mut Frame, sheet_id: &str, c: &Palette) {
    let area = centered_rect(50, 25, frame.area());
    frame.render_widget(Clear, area);
    let block = Block::default()
        .title(" Confirm Delete Sheet ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(c.error));
    let lines = vec![
        Line::from(""),
        Line::from("  Delete this sheet? Goals become orphaned to All."),
        Line::from(""),
        Line::from(vec![Span::styled(
            format!("  id: {sheet_id}"),
            Style::default().fg(c.outline_variant),
        )]),
        Line::from(""),
        Line::from(vec![
            Span::styled("  y", Style::default().fg(c.error).add_modifier(Modifier::BOLD)),
            Span::raw(" to confirm · "),
            Span::styled("n", Style::default().fg(c.outline_variant)),
            Span::raw(" to cancel"),
        ]),
    ];
    frame.render_widget(Paragraph::new(lines).block(block).style(Style::default().fg(c.on_background)), area);
}

fn render_plugin_buffer(spec: &cordanui_plugin_runtime::PanelSpec, app: &App, frame: &mut Frame, area: Rect) {
    let c = &app.theme.colors;
    // no rect/border — requested: causes hangs and not needed for diff sheet
    let inner = area;
    // Reuse PanelSpec draw logic similar to plugin_panel
    use cordanui_plugin_runtime::Widget;
    let mut lines: Vec<Line> = Vec::new();
    fn flatten(w: &Widget, c: &Palette, out: &mut Vec<Line>) {
        match w {
            Widget::Text { content, fg, bold } => {
                let mut style = Style::default().fg(fg
                    .as_deref()
                    .and_then(|role| c.get(role))
                    .unwrap_or(c.on_background));
                if *bold {
                    style = style.add_modifier(Modifier::BOLD);
                }
                out.push(Line::from(Span::styled(content.clone(), style)));
            }
            Widget::List { items, highlight } => {
                for (i, item) in items.iter().enumerate() {
                    let style = if Some(i) == *highlight {
                        Style::default().fg(c.on_primary).bg(c.primary)
                    } else {
                        Style::default().fg(c.on_background)
                    };
                    out.push(Line::from(Span::styled(format!("  {item}"), style)));
                }
            }
            Widget::Column { children } => {
                for child in children {
                    flatten(child, c, out);
                }
            }
            Widget::Row { children } => {
                let mut cols: Vec<Vec<Line>> = Vec::new();
                for child in children {
                    let mut sub = Vec::new();
                    flatten(child, c, &mut sub);
                    cols.push(sub);
                }
                let max_h = cols.iter().map(|v| v.len()).max().unwrap_or(0);
                for i in 0..max_h {
                    let mut spans = Vec::new();
                    for (ci, col) in cols.iter().enumerate() {
                        if ci > 0 {
                            spans.push(Span::styled(" │ ", Style::default().fg(c.outline)));
                        }
                        if let Some(line) = col.get(i) {
                            spans.extend(line.spans.clone());
                        }
                    }
                    if !spans.is_empty() {
                        out.push(Line::from(spans));
                    }
                }
            }
        }
    }
    flatten(&(spec.draw)(), c, &mut lines);
    if lines.is_empty() {
        lines.push(Line::from(Span::styled(
            "  (plugin buffer — no content)",
            Style::default().fg(c.outline_variant),
        )));
    }
    frame.render_widget(Paragraph::new(lines).block(Block::default()).wrap(Wrap { trim: false }), inner);
}

/// Live agent run view: spinner + progress log for the running goal.
fn render_agent_running(app: &App, goal_id: &str, frame: &mut Frame) {
    let c = &app.theme.colors;
    let area = centered_rect(64, 55, frame.area());
    frame.render_widget(Clear, area);
    let title = app
        .goals
        .iter()
        .find(|g| g.id == goal_id)
        .map(|g| truncate_str(&g.title, 30))
        .unwrap_or_else(|| "?".into());
    let outer = Block::default()
        .title(format!(" Agent — {title} "))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(c.primary));
    let inner = outer.inner(area);
    frame.render_widget(&outer, area);

    let f = SPINNER[(std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() / 200 % (SPINNER.len() as u128))
        .unwrap_or(0) as usize)];

    let mut lines: Vec<Line> = vec![Line::from(Span::styled(
        format!(" {f} streaming…  (Esc hides; the run continues)"),
        Style::default().fg(c.primary).add_modifier(Modifier::BOLD),
    ))];
    lines.push(Line::from(""));
    for entry in app
        .agent_log
        .iter()
        .rev()
        .take(12)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
    {
        lines.push(Line::from(Span::styled(
            format!("   {entry}"),
            Style::default().fg(c.on_surface_variant),
        )));
    }

    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
}

/// Truncate a string on char boundaries with an ellipsis.
fn truncate_str(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let cut: String = s.chars().take(max.saturating_sub(1)).collect();
    format!("{cut}…")
}

/// The plugin manager's own help page.
fn render_plugin_help(app: &App, frame: &mut Frame) {
    let c = &app.theme.colors;
    let area = centered_rect(55, 55, frame.area());
    frame.render_widget(Clear, area);
    let block = Block::default()
        .title(" Plugin Manager Help ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(c.primary));

    let l = app.keybinds.leader.label();
    let p = app.keybinds.plugins.label();
    let lines = vec![
        Line::from(""),
        Line::from(vec![Span::styled(
            format!("  Open: {l}+{p}"),
            Style::default().fg(c.primary).add_modifier(Modifier::BOLD),
        )]),
        Line::from(""),
        Line::from("  ↑ / ↓, j / k     move selection"),
        Line::from("    Enter / a / ␣  activate · deactivate plugin"),
        Line::from("    d              uninstall (files + registry)"),
        Line::from("    i / n          open the install input box"),
        Line::from("    Enter          install from the input"),
        Line::from("    u              update all plugins (re-clone + build)"),
        Line::from("    c              configure the selected plugin"),
        Line::from("    s              start / stop the selected [service]"),
        Line::from("    ?              this help"),
        Line::from("    Esc            close"),
        Line::from(""),
        Line::from(Span::styled(
            "  Installs clone the repo into",
            Style::default().fg(c.outline_variant),
        )),
        Line::from(Span::styled(
            "  ~/.local/share/cordanui/plugins/<repo>",
            Style::default().fg(c.outline_variant),
        )),
        Line::from(Span::styled(
            "  and register it active, most recent first.",
            Style::default().fg(c.outline_variant),
        )),
    ];

    let paragraph = Paragraph::new(lines)
        .block(block)
        .style(Style::default().fg(c.on_background));
    frame.render_widget(&paragraph, area);
}

fn render_stats(app: &App, frame: &mut Frame) {
    let c = &app.theme.colors;
    let area = centered_rect(70, 75, frame.area());
    frame.render_widget(Clear, area);
    let block = Block::default()
        .title(" Stats ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(c.primary));
    let inner = block.inner(area);
    frame.render_widget(&block, area);

    let s = app.stats_snapshot();
    let total = s.total as f64;

    let pct = |n: usize| if total > 0.0 { n as f64 / total * 100.0 } else { 0.0 };
    let bar = |n: usize, width: usize| {
        let p = pct(n);
        let filled = ((p / 100.0) * width as f64).round() as usize;
        format!("{}{}", "█".repeat(filled), "░".repeat(width.saturating_sub(filled)))
    };

    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::from(Span::styled(
        format!("  Total goals: {}", s.total),
        Style::default().fg(c.primary).add_modifier(Modifier::BOLD),
    )));
    lines.push(Line::from(""));

    // Status breakdown with bars
    let bw = 14usize;
    let status_rows = [
        ("pending", s.pending, c.get("onSurfaceVariant").unwrap_or(c.outline_variant)),
        ("in progress", s.in_progress, c.primary),
        ("completed", s.completed, c.success),
        ("agent mode", s.agent_mode, c.tertiary),
    ];
    for (label, cnt, col) in status_rows {
        lines.push(Line::from(vec![
            Span::styled(format!("  {label:<12}"), Style::default().fg(c.on_background)),
            Span::styled(format!("{cnt:>4}  "), Style::default().fg(col).add_modifier(Modifier::BOLD)),
            Span::styled(bar(cnt, bw), Style::default().fg(col)),
            Span::styled(format!(" {:>5.1}%", pct(cnt)), Style::default().fg(c.outline_variant)),
        ]));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(vec![
        Span::styled("  overdue      ", Style::default().fg(c.on_background)),
        Span::styled(format!("{:>4}", s.overdue), Style::default().fg(c.error).add_modifier(Modifier::BOLD)),
        Span::styled(format!("  due today {:>4}  due week {:>4}  no due {:>4}  remind {:>4}", s.due_today, s.due_week, s.no_due, s.remind_set), Style::default().fg(c.outline_variant)),
    ]));
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled("  Repeat", Style::default().fg(c.secondary).add_modifier(Modifier::BOLD))));
    lines.push(Line::from(vec![
        Span::styled(format!("    none {:>4}  daily {:>4}  weekly {:>4}  monthly {:>4}  yearly {:>4}", s.repeat_none, s.repeat_daily, s.repeat_weekly, s.repeat_monthly, s.repeat_yearly), Style::default().fg(c.on_background)),
    ]));
    lines.push(Line::from(""));
    lines.push(Line::from(vec![
        Span::styled("  Sheets", Style::default().fg(c.secondary).add_modifier(Modifier::BOLD)),
        Span::styled(format!("  count: {}", s.sheets_count), Style::default().fg(c.outline_variant)),
        Span::styled(format!("  avg children: {:.1}", s.avg_children), Style::default().fg(c.outline_variant)),
    ]));
    for (name, cnt) in &s.sheet_distribution {
        lines.push(Line::from(vec![
            Span::styled(format!("    {:<20}", truncate_str(name, 20)), Style::default().fg(c.on_background)),
            Span::styled(format!("{cnt:>4}  "), Style::default().fg(c.primary)),
            Span::styled(bar(*cnt, bw), Style::default().fg(c.primary)),
            Span::styled(format!(" {:>5.1}%", pct(*cnt)), Style::default().fg(c.outline_variant)),
        ]));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled("  Agent", Style::default().fg(c.secondary).add_modifier(Modifier::BOLD))));
    lines.push(Line::from(vec![
        Span::styled(format!("    queued {:>4}  running {:>4}  completed {:>4}  failed {:>4}", s.agent_queued, s.agent_running, s.agent_completed, s.agent_failed), Style::default().fg(c.on_background)),
    ]));

    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
}

fn render_full_result(app: &App, goal_id: &str, scroll: usize, frame: &mut Frame) {
    let c = &app.theme.colors;
    let area = centered_rect(85, 85, frame.area());
    frame.render_widget(Clear, area);
    let goal = app.goals.iter().find(|g| g.id == goal_id);
    let title = goal.map(|g| g.title.clone()).unwrap_or_else(|| goal_id.to_string());
    let block = Block::default()
        .title(format!(" Full result — {} ", truncate_str(&title, 40)))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(c.primary));
    let inner = block.inner(area);
    frame.render_widget(&block, area);

    let Some(g) = goal else {
        frame.render_widget(Paragraph::new("Goal not found").style(Style::default().fg(c.error)), inner);
        return;
    };

    let mut lines: Vec<Line> = Vec::new();
    // Header with status
    if let Some(st) = &g.agent_status {
        let st_str = format!("{st:?}").to_lowercase();
        lines.push(Line::from(Span::styled(format!("Status: {st_str}"), Style::default().fg(c.tertiary).add_modifier(Modifier::BOLD))));
        lines.push(Line::from(""));
    }
    // Full agent_result content (untruncated)
    if let Some(res) = &g.agent_result {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(res) {
            if let Some(content) = v.get("content").and_then(|c| c.as_str()) {
                for line in content.split('\n') {
                    lines.push(Line::from(Span::styled(line.to_string(), Style::default().fg(c.on_surface))));
                }
            } else {
                for line in res.split('\n') {
                    lines.push(Line::from(Span::styled(line.to_string(), Style::default().fg(c.on_surface))));
                }
            }
        }
    } else if let Some(prog) = &g.agent_progress {
        lines.push(Line::from(Span::styled("Progress:", Style::default().fg(c.primary).add_modifier(Modifier::BOLD))));
        for line in prog.split('\n') {
            lines.push(Line::from(Span::styled(line.to_string(), Style::default().fg(c.on_surface_variant))));
        }
    } else {
        lines.push(Line::from(Span::styled("(no result yet)", Style::default().fg(c.outline_variant))));
    }

    let total = lines.len();
    let height = inner.height as usize;
    let scroll = scroll.min(total.saturating_sub(height).max(0));
    let visible = lines.into_iter().skip(scroll).take(height).collect::<Vec<_>>();

    let footer = format!(" {}/{} lines — j/k scroll, Esc/q close ", scroll + visible.len().min(height), total);
    let inner_with_footer = Rect::new(inner.x, inner.y, inner.width, inner.height.saturating_sub(1));
    frame.render_widget(Paragraph::new(visible).wrap(Wrap { trim: false }), inner_with_footer);
    // footer
    let footer_area = Rect::new(inner.x, inner.y + inner.height - 1, inner.width, 1);
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(footer, Style::default().fg(c.outline_variant)))),
        footer_area,
    );
}

/// Helper: centered rect for overlays.
fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let pop_h = area.height * percent_y / 100;
    let pop_w = area.width * percent_x / 100;
    let y = area.y + (area.height - pop_h) / 2;
    let x = area.x + (area.width - pop_w) / 2;
    Rect::new(x, y, pop_w, pop_h)
}
