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

use crate::app::{App, Mode, PluginPane};

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
            Constraint::Min(1),    // goal list
            Constraint::Length(3), // input/status bar
            Constraint::Length(1), // keybinding hint
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
    if let Mode::AgentRunning { goal_id } = &app.mode {
        render_agent_running(app, goal_id, frame);
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

    let block = Block::default()
        .borders(Borders::BOTTOM)
        .border_style(Style::default().fg(c.outline));

    let line = Line::from(vec![title, theme, stats]);
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

            let line = Line::from(vec![
                Span::raw(indent.clone()),
                Span::raw(expand_icon),
                Span::styled(
                    format!("{status_glyph} "),
                    Style::default().fg(status_color),
                ),
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
                        Style::default().fg(c.on_surface_variant),
                    ),
                ]));
            }

            ListItem::new(Text::from(lines))
        })
        .collect();

    let block = Block::default().borders(Borders::NONE);

    let list = List::new(items)
        .block(block)
        .highlight_style(Style::default().bg(c.surface).add_modifier(Modifier::BOLD))
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
                Style::default().fg(c.outline_variant),
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
        Mode::EditTitle { .. } => (" Edit title: ".to_string(), app.input.text.clone()),
        Mode::EditDescription { .. } => (" Edit description: ".to_string(), app.input.text.clone()),
        Mode::ConfirmDelete { .. } => (" DELETE ".to_string(), String::new()),
        Mode::Help => (" HELP ".to_string(), String::new()),
        Mode::PluginManager { .. } | Mode::PluginHelp | Mode::PluginConfigure { .. } => {
            (" PLUGIN ".to_string(), app.input.text.clone())
        }
        Mode::AgentPicker { .. } | Mode::AgentRunning { .. } => {
            (" AGENT ".to_string(), String::new())
        }
    };

    let label_style = match &app.mode {
        Mode::Normal => Style::default().fg(c.outline_variant),
        Mode::AddGoal { .. } | Mode::EditTitle { .. } | Mode::EditDescription { .. } => {
            Style::default().fg(c.primary)
        }
        Mode::PluginManager { .. }
        | Mode::PluginHelp
        | Mode::PluginConfigure { .. }
        | Mode::AgentPicker { .. }
        | Mode::AgentRunning { .. } => Style::default().fg(c.primary),
        Mode::ConfirmDelete { .. } => Style::default().fg(c.error),
        Mode::Help => Style::default().fg(c.primary),
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
                | Mode::PluginManager { .. }
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
        Mode::PluginManager {
            pane: PluginPane::Install,
        } => "GitHub link / owner/repo / terms · Enter install · Esc back".into(),
        Mode::PluginManager {
            pane: PluginPane::List,
        } => "i install · ↑↓ select · Enter activate · d uninstall · ? help · Esc close".into(),
        Mode::PluginHelp => "Esc/q to close".into(),
        Mode::PluginConfigure { .. } => "↑↓ field · Enter edit · Enter save · Esc back".into(),
        Mode::AgentPicker { .. } => "↑↓ model · Enter run · Esc close".into(),
        Mode::AgentRunning { .. } => "streaming… Esc hides (run continues)".into(),
    };

    let line = Line::from(vec![Span::styled(
        format!(" {hint}"),
        Style::default().fg(c.outline_variant),
    )]);
    let paragraph = Paragraph::new(line);
    frame.render_widget(&paragraph, area);
}

fn render_help_overlay(app: &App, frame: &mut Frame, c: &Palette) {
    let area = centered_rect(70, 70, frame.area());
    let block = Block::default()
        .title(" Help — [keybinds] from ~/.config/cordanui/config.toml ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(c.primary));

    let mut lines = vec![
        Line::from(""),
        Line::from(vec![Span::styled(
            "  Configured keybinds",
            Style::default().fg(c.primary).add_modifier(Modifier::BOLD),
        )]),
        Line::from(Span::styled(
            "  (values marked ·default· come from the built-ins, others were set in config.toml)",
            Style::default().fg(c.outline_variant),
        )),
        Line::from(""),
    ];

    for entry in app.keybinds.entries() {
        let origin = if entry.is_default {
            "default"
        } else {
            "custom"
        };
        lines.push(Line::from(vec![
            Span::styled(
                format!("    {:<14}", entry.binding.label()),
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("{:<24}", entry.desc),
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
        Line::from("    Esc             cancel leader / input"),
        Line::from("    C-d / C-c       quit"),
        Line::from(""),
        Line::from(Span::styled(
            "  To rebind: edit [keybinds] in ~/.config/cordanui/config.toml",
            Style::default().fg(c.outline_variant),
        )),
    ]);

    let paragraph = Paragraph::new(lines)
        .block(block)
        .style(Style::default().fg(c.on_background))
        .wrap(Wrap { trim: false });
    frame.render_widget(&paragraph, area);
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

    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::from(Span::styled(
        "  ↑↓ select · Enter edit · Enter save · Esc back",
        Style::default().fg(c.outline_variant),
    )));
    lines.push(Line::from(""));

    for (i, f) in spec.fields.iter().enumerate() {
        let selected = i == app.config_selected;
        let cursor = if selected { "▶ " } else { "  " };
        let value = app.config_values.get(&f.key).cloned().unwrap_or_default();

        // While editing THIS field, show the live buffer.
        let shown = if selected && app.config_editing.is_some() {
            format!("{}│", app.config_editing.as_deref().unwrap_or(""))
        } else if f.r#type == "secret" && !value.is_empty() {
            "•".repeat(value.chars().count().min(24))
        } else if value.is_empty() {
            "(not set)".to_string()
        } else {
            value.clone()
        };
        let value_style = if selected && app.config_editing.is_some() {
            Style::default().fg(c.primary).add_modifier(Modifier::BOLD)
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
        lines.push(Line::from(vec![
            Span::styled(format!(" {cursor}"), Style::default().fg(c.primary)),
            Span::styled(format!("{:<24}", truncate_str(&ch.model, 23)), style),
            Span::styled(ch.plugin.clone(), Style::default().fg(c.outline_variant)),
        ]));
    }

    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
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
        Line::from("    Enter / a      activate · deactivate plugin"),
        Line::from("    d              uninstall (files + registry)"),
        Line::from("    i              open the install input box"),
        Line::from("    Enter          install from the input"),
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

/// Helper: centered rect for overlays.
fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let pop_h = area.height * percent_y / 100;
    let pop_w = area.width * percent_x / 100;
    let y = area.y + (area.height - pop_h) / 2;
    let x = area.x + (area.width - pop_w) / 2;
    Rect::new(x, y, pop_w, pop_h)
}
