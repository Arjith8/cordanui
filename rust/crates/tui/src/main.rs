//! cordanui TUI — main entry point.
//!
//! Phase 1: local goal tracker. Goal tree, add/edit/complete/delete/reorder,
//! local SQLite. No sync, no plugins, no agent mode.
//!
//! The agent backend is an optional external component — the TUI has no
//! dependency on it; plugins communicate over the JSON-stdio protocol in
//! `crates/plugin-runtime`.

mod app;
mod config;
mod db;
pub mod plugin_ui;
mod plugins;
mod services;
mod style;
mod theme;
mod ui;

use std::io::{self, stdout};

use crossterm::{
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyModifiers,
    },
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{backend::CrosstermBackend, Terminal};

use crate::app::Mode;

fn main() -> anyhow::Result<()> {
    // Headless service management: `cordanui service list|start|stop|status`
    // runs without the TUI (servers, systemd units).
    let argv: Vec<String> = std::env::args().skip(1).collect();
    if argv.first().map(String::as_str) == Some("service") {
        return services::cli_run(&argv[1..]);
    }

    // Keybinds from the [keybinds] section of config.toml.
    let keybinds = config::Keybinds::load();

    // Open DB — once. Clones share the same underlying handles; repeated
    // `Database::open` calls each redo schema setup and (with a dead Turso
    // host) each pay ~1s of failed replica handshake.
    let db = db::open()?;
    let mut app = app::App::new(db.clone())?;
    app.attach_plugin_config_db(db.clone());
    // Sync worker handle: only when credentials are configured AND the
    // database actually opened in replica mode. If Turso is unreachable,
    // `Database::open` degrades to local-only instead of failing — the TUI
    // keeps working offline and the status line says so.
    if cordanui_sync::SyncConfig::load()
        .map(|c| c.is_sync_enabled())
        .unwrap_or(false)
    {
        if db.is_sync_enabled() {
            app.attach_sync_db(db.clone());
        } else {
            let msg = "turso unreachable at startup — running local-only (edits won't sync)";
            app.record_error(
                "sync",
                msg,
                Some("check [turso] url/token in ~/.config/cordanui/config.toml, then restart"),
            );
            app.set_message(msg);
        }
    }
    app.load_plugin_states();
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
        // Drain all plugin queues BEFORE drawing so worker results,
        // style commits, dialogs, panels, and notifications are visible
        // this frame — not on the next keypress. (event::poll below can
        // idle up to 250ms; drains must not wait on input.)
        app.poll_plugin_search()?;
        app.apply_style_updates()?;
        app.poll_plugin_ui_requests();
        app.poll_plugin_panel();
        app.poll_command_results();
        app.poll_sync();

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
            Mode::PluginManager { pane } => {
                if handle_plugin_manager_key(app, key, pane)? {
                    break;
                }
            }
            Mode::PluginHelp => handle_plugin_help_key(app, key),
            Mode::PluginConfigure { plugin } => handle_configure_key(app, key, &plugin)?,
            Mode::AgentPicker { .. } => handle_agent_picker_key(app, key)?,
            Mode::PluginModal => handle_plugin_modal_key(app, key),
            Mode::PluginPanel => handle_plugin_panel_key(app, key),
            Mode::Command => handle_command_key(app, key),
            Mode::GlobalConfig => handle_global_config_key(app, key)?,
            Mode::AgentRunning { .. } => handle_agent_running_key(app, key),
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
            KeyCode::Esc => {}                     // cancel leader
            KeyCode::Char('q') => return Ok(true), // <leader>q — quit
            _ if binds.new_goal.matches(key) => {
                // If the selected goal is expanded (leader + show_details),
                // add a subgoal under it; otherwise a new root goal.
                let parent_id = match app.selected_row() {
                    Some(row) if app.expanded.contains(&row.goal.id) => Some(row.goal.id.clone()),
                    _ => None,
                };
                app.start_add_goal(parent_id);
            }
            _ if binds.show_details.matches(key) => app.toggle_details(),
            _ if binds.help.matches(key) => app.mode = Mode::Help,
            _ if binds.plugins.matches(key) => app.open_plugin_manager()?,
            _ if binds.run_agent.matches(key) => {
                if let Some(row) = app.selected_row() {
                    app.open_agent_picker(row.goal.id.clone())?;
                }
            }
            _ if binds.commands.matches(key) => app.open_command_mode(),
            _ if binds.global_config.matches(key) => app.open_global_config(),
            _ if binds.sync.matches(key) => app.request_sync(),
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

fn handle_plugin_manager_key(
    app: &mut app::App,
    key: KeyEvent,
    pane: app::PluginPane,
) -> anyhow::Result<bool> {
    use app::PluginPane;

    // C-d / C-c quit from anywhere (terminal is restored by the run loop).
    if key.modifiers.contains(KeyModifiers::CONTROL)
        && matches!(key.code, KeyCode::Char('c') | KeyCode::Char('d'))
    {
        return Ok(true);
    }

    match pane {
        // Typing an install query.
        // Install overlay: type a query, Enter installs, Esc returns.
        PluginPane::Install => match key.code {
            KeyCode::Esc => {
                app.input.clear();
                app.mode = Mode::PluginManager {
                    pane: PluginPane::List,
                };
            }
            KeyCode::Enter => {
                if !app.input.text.trim().is_empty() {
                    app.start_plugin_search();
                }
            }
            KeyCode::Backspace => app.input.backspace(),
            KeyCode::Left => app.input.move_left(),
            KeyCode::Right => app.input.move_right(),
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                app.input.clear()
            }
            KeyCode::Char(c) => {
                if c.is_control() {
                    return Ok(false);
                }
                app.input.push_char(c);
            }
            _ => {}
        },

        // Installed-plugins list.
        PluginPane::List => match key.code {
            KeyCode::Esc => app.cancel(),
            KeyCode::Char('?') => app.mode = Mode::PluginHelp,
            // Open the install input overlay.
            KeyCode::Char('i') | KeyCode::Char('n') => app.start_install_mode(),
            KeyCode::Up | KeyCode::Char('k') => {
                if app.plugin_selected > 0 {
                    app.plugin_selected -= 1;
                }
            }
            KeyCode::Down | KeyCode::Char('j') => {
                let max = app.installed_plugins.len().saturating_sub(1);
                if app.plugin_selected < max {
                    app.plugin_selected += 1;
                }
            }
            // Activate / deactivate.
            KeyCode::Enter | KeyCode::Char('a') | KeyCode::Char(' ') => {
                app.toggle_plugin_active()?
            }
            // Uninstall (files + registry row).
            KeyCode::Char('d') | KeyCode::Delete => app.uninstall_selected_plugin()?,
            KeyCode::Char('u') => app.update_all_plugins(),
            // Configure (declarative [ui] settings form, or the plugin's
            // own page when it defines plugin.configure).
            KeyCode::Char('c') => app.open_configure()?,
            // Start/stop the selected plugin's [service] process.
            KeyCode::Char('s') => app.toggle_selected_service()?,
            _ => {}
        },
    }
    Ok(false)
}

fn handle_plugin_help_key(app: &mut app::App, key: KeyEvent) {
    match key.code {
        KeyCode::Esc
        | KeyCode::Enter
        | KeyCode::Char('q')
        | KeyCode::Char('?')
        | KeyCode::Char('h') => {
            app.mode = Mode::PluginManager {
                pane: app::PluginPane::List,
            };
        }
        _ => {}
    }
}

/// Keys inside a plugin's configure form.
///
/// Navigation: ↑/↓ move between fields. Enter (or any printable key) on a
/// field starts editing that field's value; Enter commits, Esc cancels the
/// edit. Esc with no active edit returns to the plugin list.
fn handle_configure_key(app: &mut app::App, key: KeyEvent, plugin: &str) -> anyhow::Result<()> {
    use crossterm::event::KeyModifiers;

    // Editing a field value.
    if let Some(_) = &app.config_editing {
        match key.code {
            KeyCode::Enter => app.commit_config_field(plugin)?,
            KeyCode::Esc => app.config_editing = None,
            KeyCode::Backspace => {
                if let Some(buf) = &mut app.config_editing {
                    if let Some(prev) = buf.char_indices().last().map(|(i, _)| i) {
                        buf.truncate(prev);
                    }
                }
            }
            KeyCode::Char(c) if !c.is_control() => {
                if let Some(buf) = &mut app.config_editing {
                    buf.push(c);
                }
            }
            _ => {}
        }
        return Ok(());
    }

    let Some(spec) = app.config_spec.clone() else {
        return Ok(());
    };
    let on_select = app.config_selected_is_select();

    match key.code {
        KeyCode::Esc | KeyCode::Char('q') => {
            app.mode = Mode::PluginManager {
                pane: app::PluginPane::List,
            };
        }
        KeyCode::Up | KeyCode::Char('k') => {
            if app.config_selected > 0 {
                app.config_selected -= 1;
            }
        }
        KeyCode::Down | KeyCode::Char('j') => {
            if app.config_selected + 1 < spec.fields.len() {
                app.config_selected += 1;
            }
        }
        // Select fields cycle through their options instead of free-text
        // editing. Tab / Shift+Tab in either direction; Enter = forward.
        KeyCode::Tab => app.cycle_config_field(plugin, 1)?,
        KeyCode::BackTab => app.cycle_config_field(plugin, -1)?,
        KeyCode::Enter | KeyCode::Char(' ') if on_select => app.cycle_config_field(plugin, 1)?,
        // Start editing: seed the buffer with the current value (secrets
        // too — the user is already past any shoulder-surfers here).
        KeyCode::Enter | KeyCode::Char(' ') => {
            let key_name = spec.fields[app.config_selected].key.clone();
            app.config_editing = Some(
                app.config_values
                    .get(&key_name)
                    .cloned()
                    .unwrap_or_default(),
            );
        }
        KeyCode::Char(c) if !c.is_control() && !on_select => {
            app.config_editing = Some(c.to_string());
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
        KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc | KeyCode::Char('q') => app.cancel(),
        _ => {}
    }
    Ok(())
}

/// Keys in the command line. Enter runs the first matching command on a
/// worker thread; its result surfaces via the status line.
fn handle_command_key(app: &mut app::App, key: KeyEvent) {
    match key.code {
        KeyCode::Esc => app.cancel(),
        KeyCode::Enter => {
            let cmd = app.command_matches().into_iter().next();
            if let Some(cmd) = cmd {
                app.execute_plugin_command(&cmd);
            } else {
                app.set_message("no matching command");
            }
        }
        KeyCode::Backspace => {
            app.input.backspace();
        }
        KeyCode::Char(c) if !c.is_control() => app.input.push_char(c),
        _ => {}
    }
}

/// Keys in the global settings page. Rows are the host Sync fields
/// followed by plugin-owned configurator entries; Enter on a field edits
/// it, Enter on a plugin entry runs that plugin's configurator.
fn handle_global_config_key(app: &mut app::App, key: KeyEvent) -> anyhow::Result<()> {
    // Editing a field value (same buffer pattern as the plugin form).
    if app.config_editing.is_some() {
        match key.code {
            KeyCode::Enter => app.commit_global_field()?,
            KeyCode::Esc => app.config_editing = None,
            KeyCode::Backspace => {
                if let Some(buf) = &mut app.config_editing {
                    buf.pop();
                }
            }
            KeyCode::Char(c) if !c.is_control() => {
                if let Some(buf) = &mut app.config_editing {
                    buf.push(c);
                }
            }
            _ => {}
        }
        return Ok(());
    }

    let field_count = app
        .global_spec
        .as_ref()
        .map(|s| s.fields.len())
        .unwrap_or(0);
    let total = app.global_row_count();
    let on_field = app.config_selected < field_count;

    match key.code {
        KeyCode::Esc | KeyCode::Char('q') => app.cancel(),
        KeyCode::Up | KeyCode::Char('k') => {
            if app.config_selected > 0 {
                app.config_selected -= 1;
            }
        }
        KeyCode::Down | KeyCode::Char('j') => {
            if app.config_selected + 1 < total {
                app.config_selected += 1;
            }
        }
        KeyCode::Enter | KeyCode::Char(' ') if on_field => {
            let key_name = app
                .global_spec
                .as_ref()
                .and_then(|s| s.fields.get(app.config_selected))
                .map(|f| f.key.clone())
                .unwrap_or_default();
            app.config_editing = Some(
                app.global_values
                    .get(&key_name)
                    .cloned()
                    .unwrap_or_default(),
            );
        }
        KeyCode::Enter | KeyCode::Char(' ') => {
            // Plugin entry: run its configurator (worker thread; the
            // panel/dialog it opens is answered via the normal loop).
            let idx = app.config_selected.saturating_sub(field_count);
            if let Some((name, _)) = app.global_plugin_entries.get(idx).cloned() {
                app.spawn_plugin_call(&name, app::PluginCall::Configure);
            }
        }
        _ => {}
    }
    Ok(())
}

/// Keys in a plugin-owned panel (`cord.ui.show_panel`). Keys are forwarded
/// to the plugin's `on_key` by name; an unhandled Esc closes the panel.
fn handle_plugin_panel_key(app: &mut app::App, key: KeyEvent) {
    let name = match key.code {
        KeyCode::Enter => "enter".to_string(),
        KeyCode::Esc => "esc".to_string(),
        KeyCode::Tab => "tab".to_string(),
        KeyCode::BackTab => "shift+tab".to_string(),
        KeyCode::Backspace => "backspace".to_string(),
        KeyCode::Up => "up".to_string(),
        KeyCode::Down => "down".to_string(),
        KeyCode::Left => "left".to_string(),
        KeyCode::Right => "right".to_string(),
        KeyCode::Home => "home".to_string(),
        KeyCode::End => "end".to_string(),
        KeyCode::PageUp => "pageup".to_string(),
        KeyCode::PageDown => "pagedown".to_string(),
        KeyCode::Char(c) => {
            let base = c.to_string();
            if key.modifiers.contains(KeyModifiers::CONTROL) {
                format!("ctrl+{base}")
            } else if key.modifiers.contains(KeyModifiers::SHIFT) && c.is_ascii_lowercase() {
                c.to_ascii_uppercase().to_string()
            } else {
                base
            }
        }
        _ => return,
    };

    let handled = app
        .plugin_panel
        .as_ref()
        .map(|spec| (spec.on_key)(&name))
        .unwrap_or(false);
    if !handled && name == "esc" {
        app.close_plugin_panel();
    }
}

/// Keys in a plugin-requested modal (`cord.ui.input/confirm/pick`).
/// All real logic lives on `App` methods so tests can drive dialogs
/// without synthesizing key events.
fn handle_plugin_modal_key(app: &mut app::App, key: KeyEvent) {
    use app::PluginModalKind;
    use cordanui_plugin_runtime::UiRequest;

    let kind = app.plugin_modal.as_ref().map(|m| match &m.kind {
        PluginModalKind::Input { .. } | PluginModalKind::TextEditor { .. } => "text",
        PluginModalKind::Confirm => "confirm",
        PluginModalKind::Pick { .. } => "pick",
        PluginModalKind::MultiSelect { .. } => "multi",
    });

    match (key.code, key.modifiers, kind) {
        // Text editor: Ctrl+Enter submits; plain Enter adds a newline.
        (KeyCode::Enter, m, Some("text")) if m.contains(KeyModifiers::CONTROL) => {
            app.submit_plugin_modal()
        }
        (KeyCode::Char(c), _, Some("input") | Some("text")) if !c.is_control() => {
            app.plugin_modal_push_char(c)
        }
        (KeyCode::Backspace, _, Some("input") | Some("text")) => app.plugin_modal_backspace(),
        (KeyCode::Enter, m, Some("text")) if !m.contains(KeyModifiers::CONTROL) => {
            app.plugin_modal_newline()
        }
        (KeyCode::Enter, _, _) => app.submit_plugin_modal(),
        (KeyCode::Esc, _, _) => {
            // First Esc clears typed text; an empty box cancels.
            if matches!(kind, Some("input") | Some("text"))
                && app.plugin_modal_text().is_some_and(|t| !t.is_empty())
            {
                if let Some(app::ActivePluginModal {
                    kind: PluginModalKind::Input { buffer, .. },
                    ..
                }) = &mut app.plugin_modal
                {
                    buffer.clear();
                }
            } else {
                app.cancel_plugin_modal();
            }
        }
        // Confirm: y/n aliases.
        (KeyCode::Char('y') | KeyCode::Char('Y'), _, Some("confirm")) => app.submit_plugin_modal(),
        (KeyCode::Char('n') | KeyCode::Char('N'), _, Some("confirm")) => app.cancel_plugin_modal(),
        // Pick / multiselect: cursor movement.
        (KeyCode::Up | KeyCode::Char('k'), _, Some("pick") | Some("multi")) => {
            app.plugin_modal_move_selection(-1)
        }
        (KeyCode::Down | KeyCode::Char('j'), _, Some("pick") | Some("multi")) => {
            app.plugin_modal_move_selection(1)
        }
        // Multiselect: space toggles the highlighted item.
        (KeyCode::Char(' '), _, Some("multi")) => app.plugin_modal_toggle_current(),
        _ => {}
    }
}

/// Keys in the provider/model picker.
fn handle_agent_picker_key(app: &mut app::App, key: KeyEvent) -> anyhow::Result<()> {
    match key.code {
        KeyCode::Esc | KeyCode::Char('q') => app.mode = Mode::Normal,
        KeyCode::Up | KeyCode::Char('k') => {
            if app.agent_selected > 0 {
                app.agent_selected -= 1;
            }
        }
        KeyCode::Down | KeyCode::Char('j') => {
            if app.agent_selected + 1 < app.agent_choices.len() {
                app.agent_selected += 1;
            }
        }
        KeyCode::Enter => {
            if let Mode::AgentPicker { goal_id } = &app.mode {
                let goal_id = goal_id.clone();
                app.start_agent_run(goal_id)?;
            }
        }
        _ => {}
    }
    Ok(())
}

/// Keys while an agent streams. Esc leaves the view — the run keeps going
/// in the background and lands in the DB when done.
fn handle_agent_running_key(app: &mut app::App, key: KeyEvent) {
    if key.code == KeyCode::Esc {
        app.mode = Mode::Normal;
    }
}
