//! cordanui TUI — main entry point.
//!
//! Phase 1: local goal tracker. Goal tree, add/edit/complete/delete/reorder,
//! local SQLite. No sync, no plugins, no agent mode.
//!
//! The agent backend is now a separate optional component at
//! `plugins/cordanui-agents/` — the TUI has no dependency on it.

mod app;
mod config;
mod db;
mod theme;
mod ui;

use std::io::{self, stdout};

use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{backend::CrosstermBackend, Terminal};

use crate::app::Mode;

fn main() -> anyhow::Result<()> {
    // Keybinds from the [keybinds] section of config.toml.
    let keybinds = config::Keybinds::load();

    // Open DB
    let db = db::open()?;
    let mut app = app::App::new(db)?;
    app.keybinds = keybinds;

    // Setup terminal
    enable_raw_mode()?;
    let mut stdout = stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // Run the app
    let result = run(&mut terminal, &mut app);

    // Restore terminal
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;

    result
}

fn run(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut app::App,
) -> anyhow::Result<()> {
    loop {
        terminal.draw(|f| ui::render(app, f))?;

        if !event::poll(std::time::Duration::from_millis(250))? {
            continue;
        }

        let Event::Key(key) = event::read()? else {
            continue;
        };

        let mode = app.mode.clone();
        match mode {
            Mode::Help => handle_help_key(app, key)?,
            Mode::ConfirmDelete { .. } => handle_confirm_delete_key(app, key)?,
            Mode::Normal => {
                if handle_normal_key(app, key)? {
                    break;
                }
            }
            _ => handle_input_key(app, key)?,
        }

        // Clear transient message after any key in normal mode
        if app.mode == Mode::Normal && !app.leader_pending && app.message.is_some() {
            app.clear_message();
        }
    }

    Ok(())
}

/// Handle a key in normal mode. tmux-style leader input, fully configurable
/// via `[keybinds]` in config.toml:
///
///   <leader>              arm the leader (indicator appears in the status bar)
///   <leader><new_goal>    add a new goal
///   <leader><show_details> toggle description + subgoals of the selection
///   <cycle_status>        bare: cycle pending → in progress → done
///   Esc                   cancel leader / clear message
///   C-c                   quit
///
/// Bare j/k and arrow keys navigate without the prefix for convenience.
fn handle_normal_key(app: &mut app::App, key: KeyEvent) -> anyhow::Result<bool> {
    let binds = app.keybinds.clone();

    // Hardcoded safety exits — never intercepted by the leader.
    if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
        return Ok(true);
    }
    if key.code == KeyCode::Char('d') && key.modifiers.contains(KeyModifiers::CONTROL) {
        return Ok(true);
    }

    // Leader arming.
    if binds.leader.matches(key) {
        app.leader_pending = true;
        return Ok(false);
    }

    // Leader is armed — this key is the command.
    if app.leader_pending {
        app.leader_pending = false;
        match key.code {
            KeyCode::Esc => {} // cancel leader
            KeyCode::Char('q') => return Ok(true), // <leader>q — quit
            _ if binds.new_goal.matches(key) => {
                // If the selected goal is expanded (leader + show_details),
                // add a subgoal under it; otherwise a new root goal.
                let parent_id = match app.selected_row() {
                    Some(row) if app.expanded.contains(&row.goal.id) => {
                        Some(row.goal.id.clone())
                    }
                    _ => None,
                };
                app.start_add_goal(parent_id);
            }
            _ if binds.show_details.matches(key) => app.toggle_details(),
            _ if binds.help.matches(key) => app.mode = Mode::Help,
            _ => {
                app.set_message(&format!("unknown leader command ({})", key_label(&key)));
            }
        }
        return Ok(false);
    }

    // Configured bare key: cycle the selected goal's status.
    if binds.cycle_status.matches(key) {
        app.cycle_status()?;
        return Ok(false);
    }

    // No leader — bare navigation keys only.
    match key.code {
        KeyCode::Char('j') | KeyCode::Down => app.move_down(),
        KeyCode::Char('k') | KeyCode::Up => app.move_up(),
        KeyCode::Esc => app.leader_pending = false,
        _ => {}
    }
    Ok(false)
}

fn key_label(key: &KeyEvent) -> String {
    match key.code {
        KeyCode::Char(c) => c.to_string(),
        other => format!("{other:?}").to_lowercase(),
    }
}

fn handle_input_key(app: &mut app::App, key: KeyEvent) -> anyhow::Result<()> {
    match key.code {
        KeyCode::Esc => app.cancel(),
        KeyCode::Enter => match &app.mode {
            Mode::AddGoal { .. } => app.commit_add_goal()?,
            Mode::EditTitle { .. } => app.commit_edit_title()?,
            Mode::EditDescription { .. } => app.commit_edit_description()?,
            _ => {}
        },
        KeyCode::Backspace => app.input.backspace(),
        KeyCode::Left => app.input.move_left(),
        KeyCode::BackTab => app.input.move_start(),
        KeyCode::Right => app.input.move_right(),
        KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => app.input.clear(),
        KeyCode::Char(c) => {
            if c.is_control() {
                return Ok(());
            }
            app.input.push_char(c);
        }
        _ => {}
    }
    Ok(())
}

fn handle_help_key(app: &mut app::App, key: KeyEvent) -> anyhow::Result<()> {
    match key.code {
        KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('?') | KeyCode::Enter => {
            app.mode = Mode::Normal;
        }
        _ => {}
    }
    Ok(())
}

fn handle_confirm_delete_key(app: &mut app::App, key: KeyEvent) -> anyhow::Result<()> {
    match key.code {
        KeyCode::Char('y') | KeyCode::Char('Y') => app.confirm_delete()?,
        KeyCode::Char('n')
        | KeyCode::Char('N')
        | KeyCode::Esc
        | KeyCode::Char('q') => app.cancel(),
        _ => {}
    }
    Ok(())
}
