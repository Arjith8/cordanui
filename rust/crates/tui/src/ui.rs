//! TUI rendering. Separated from app logic so the render path is pure —
//! it only reads `&App` and writes to a `Frame`.

use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, List, ListItem, Paragraph, Wrap},
    Frame,
};

use cordanui_schema::GoalStatus;

use crate::app::{App, Mode};

/// Status glyph + color for each goal status.
fn status_style<'a>(status: GoalStatus, c: &'a ThemeColors) -> (&'static str, Color) {
    match status {
        GoalStatus::Pending => ("○", c.status_pending),
        GoalStatus::InProgress => ("◐", c.status_wip),
        GoalStatus::Completed => ("✓", c.status_done),
        GoalStatus::AgentMode => ("⤴", c.status_agent),
    }
}

use crate::theme::ThemeColors;

/// Render the full UI.
pub fn render(app: &mut App, frame: &mut Frame) {
    let size = frame.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),  // header
            Constraint::Min(1),    // goal list
            Constraint::Length(3),  // input/status bar
            Constraint::Length(1),  // keybinding hint
        ])
        .split(size);

    render_header(app, frame, chunks[0]);
    render_goal_list(app, frame, chunks[1]);
    render_input_bar(app, frame, chunks[2]);
    render_hint_bar(app, frame, chunks[3]);

    // Overlays
    if app.mode == Mode::Help {
        render_help_overlay(app, frame, &app.theme.colors);
    }
    if let Mode::ConfirmDelete { goal_id } = &app.mode {
        render_delete_confirm(frame, goal_id, &app.theme.colors);
    }
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
        Style::default()
            .fg(c.accent)
            .add_modifier(Modifier::BOLD),
    );
    let stats = Span::styled(
        format!(" {} / {} done · {} pending ", completed, total, pending),
        Style::default().fg(c.text_faint),
    );

    let block = Block::default()
        .borders(Borders::BOTTOM)
        .border_style(Style::default().fg(c.border));

    let line = Line::from(vec![title, stats]);
    let paragraph = Paragraph::new(line).block(block);
    frame.render_widget(&paragraph, area);
}

fn render_goal_list(app: &mut App, frame: &mut Frame, area: Rect) {
    let c = &app.theme.colors;
    let rows = app.flat_rows();
    let partial = app.partially_complete_ids();

    let items: Vec<ListItem> = rows
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
                ("◎", c.status_done)
            } else {
                status_style(row.goal.status, c)
            };

            let title_style = if row.goal.status == GoalStatus::Completed {
                Style::default().fg(c.text_faint).add_modifier(Modifier::CROSSED_OUT)
            } else {
                Style::default().fg(c.text)
            };

            let line = Line::from(vec![
                Span::raw(indent.clone()),
                Span::raw(expand_icon),
                Span::styled(format!("{status_glyph} "), Style::default().fg(status_color)),
                Span::styled(row.goal.title.clone(), title_style),
            ]);

            // Description is only shown while the row's detail view
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
                        Style::default().fg(c.text_dim),
                    ),
                ]));
            }

            ListItem::new(Text::from(lines))
        })
        .collect();

    let block = Block::default().borders(Borders::NONE);

    let list = List::new(items)
        .block(block)
        .highlight_style(
            Style::default()
                .bg(c.surface)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▶ ");

    // Reuse the App's ListState so selection + scroll offset persist across
    // frames and are tracked by the widget itself.
    frame.render_stateful_widget(list, area, &mut app.list_state);

    // Empty state
    if rows.is_empty() {
        let msg = Paragraph::new(Text::from(vec![
            Line::from(""),
            Line::from(""),
            Line::styled(
                format!(
                    "  No goals yet. Press {}+{} to add one.",
                    app.keybinds.leader.label(),
                    app.keybinds.new_goal.label()
                ),
                Style::default().fg(c.text_faint),
            ),
        ]));
        frame.render_widget(&msg, area);
    }
}

fn render_input_bar(app: &App, frame: &mut Frame, area: Rect) {
    let c = &app.theme.colors;
    // Leader indicator — shown prominently while waiting for a command key.
    let leader_span = if app.leader_pending {
        Span::styled(
            format!(" LEADER ({}) ", app.keybinds.leader.label()),
            Style::default()
                .fg(c.on_accent)
                .bg(c.accent)
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
        Mode::EditTitle { .. } => (" Edit title: ".to_string(), app.input.text.clone()),
        Mode::EditDescription { .. } => {
            (" Edit description: ".to_string(), app.input.text.clone())
        }
        Mode::ConfirmDelete { .. } => (" DELETE ".to_string(), String::new()),
        Mode::Help => (" HELP ".to_string(), String::new()),
    };

    let label_style = match &app.mode {
        Mode::Normal => Style::default().fg(c.text_faint),
        Mode::AddGoal { .. } | Mode::EditTitle { .. } | Mode::EditDescription { .. } => {
            Style::default().fg(c.accent)
        }
        Mode::ConfirmDelete { .. } => Style::default().fg(c.danger),
        Mode::Help => Style::default().fg(c.accent),
    };

    let block = Block::default()
        .borders(Borders::TOP)
        .border_style(Style::default().fg(c.border));

    let line = Line::from(vec![
        leader_span,
        Span::styled(label, label_style),
        Span::styled(text, Style::default().fg(c.text)),
        if matches!(app.mode, Mode::AddGoal { .. } | Mode::EditTitle { .. } | Mode::EditDescription { .. }) {
            Span::styled("│", Style::default().fg(c.text_dim))
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
            if app.leader_pending {
                format!(
                    "leader active — {n} new goal (subgoal if expanded) · {d} details/subgoals · {h} help · Esc cancel"
                )
            } else {
                format!("leader · leader+{n} new · leader+{d} details · leader+{h} help")
            }
        }
        Mode::AddGoal { .. } => "Enter to save · Esc to cancel".into(),
        Mode::EditTitle { .. } => "Enter to save · Esc to cancel".into(),
        Mode::EditDescription { .. } => "Enter to save · Esc to cancel".into(),
        Mode::ConfirmDelete { .. } => "y to confirm · n/Esc to cancel".into(),
        Mode::Help => "Esc/q to close help".into(),
    };

    let line = Line::from(vec![Span::styled(
        format!(" {hint}"),
        Style::default().fg(c.text_faint),
    )]);
    let paragraph = Paragraph::new(line);
    frame.render_widget(&paragraph, area);
}

fn render_help_overlay(app: &App, frame: &mut Frame, c: &ThemeColors) {
    let area = centered_rect(70, 70, frame.area());
    let block = Block::default()
        .title(" Help — [keybinds] from ~/.config/cordanui/config.toml ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(c.accent));

    let mut lines = vec![
        Line::from(""),
        Line::from(vec![Span::styled(
            "  Configured keybinds",
            Style::default().fg(c.accent).add_modifier(Modifier::BOLD),
        )]),
        Line::from(Span::styled(
            "  (values marked ·default· come from the built-ins, others were set in config.toml)",
            Style::default().fg(c.text_faint),
        )),
        Line::from(""),
    ];

    for entry in app.keybinds.entries() {
        let origin = if entry.is_default { "default" } else { "custom" };
        lines.push(Line::from(vec![
            Span::styled(
                format!("    {:<14}", entry.binding.label()),
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::styled(format!("{:<24}", entry.desc), Style::default().fg(c.text)),
            Span::styled(
                format!("{} ({origin})", entry.name),
                if entry.is_default {
                    Style::default().fg(c.text_faint)
                } else {
                    Style::default().fg(c.status_done)
                },
            ),
        ]));
    }

    lines.extend([
        Line::from(""),
        Line::from(vec![Span::styled(
            "  Fixed keys",
            Style::default().fg(c.accent).add_modifier(Modifier::BOLD),
        )]),
        Line::from("    j / k, arrows   move selection"),
        Line::from("    Esc             cancel leader / input"),
        Line::from("    C-d / C-c       quit"),
        Line::from(""),
        Line::from(Span::styled(
            "  To rebind: edit [keybinds] in ~/.config/cordanui/config.toml",
            Style::default().fg(c.text_faint),
        )),
    ]);

    let paragraph = Paragraph::new(lines)
        .block(block)
        .style(Style::default().fg(c.text))
        .wrap(Wrap { trim: false });
    frame.render_widget(&paragraph, area);
}

fn render_delete_confirm(frame: &mut Frame, goal_id: &str, c: &ThemeColors) {
    let area = centered_rect(50, 25, frame.area());
    let block = Block::default()
        .title(" Confirm Delete ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(c.danger));

    let lines = vec![
        Line::from(""),
        Line::from("  Delete this goal and all its subgoals?"),
        Line::from(""),
        Line::from(vec![Span::styled(
            format!("  id: {goal_id}"),
            Style::default().fg(c.text_faint),
        )]),
        Line::from(""),
        Line::from(vec![
            Span::styled("  y", Style::default().fg(c.danger).add_modifier(Modifier::BOLD)),
            Span::raw(" to confirm · "),
            Span::styled("n", Style::default().fg(c.text_faint)),
            Span::raw(" to cancel"),
        ]),
    ];

    let paragraph = Paragraph::new(lines).block(block).style(Style::default().fg(c.text));
    frame.render_widget(&paragraph, area);
}

/// Helper: centered rect for overlays.
fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let pop_h = area.height * percent_y / 100;
    let pop_w = area.width * percent_x / 100;
    let y = area.y + (area.height - pop_h) / 2;
    let x = area.x + (area.width - pop_w) / 2;
    Rect::new(x, y, pop_w, pop_h)
}
