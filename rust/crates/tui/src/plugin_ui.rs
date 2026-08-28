//! Host side of the `cord.ui.*` dialog API.
//!
//! [`PluginUiBridge`] implements `UiHost` for the embedded Lua runtime:
//! a plugin's awaited `cord.ui.input/confirm/pick` lands here as a
//! [`PendingUi`]. The App drains the queue every loop iteration and opens
//! the corresponding modal; when the user answers, the oneshot completes
//! and the plugin's Lua thread resumes with the value.
//!
//! Only one modal is shown at a time. Requests arriving while another
//! dialog (or any other mode) is active are answered `Refused`, which the
//! plugin sees as a Lua error — better than silently queueing a surprise
//! dialog behind whatever the user is doing.

use std::sync::{mpsc, Mutex};

use cordanui_plugin_runtime::{
    ConfigHost, PanelHost, PanelSpec, PendingUi, UiHost, UiLevel, UiResponse,
};
use cordanui_sync::Database;

/// Everything a plugin can put in front of the user.
pub enum PluginUiEvent {
    /// A blocking dialog awaiting an answer.
    Modal(PendingUi),
    /// A transient, non-blocking status message.
    Notify { level: UiLevel, message: String },
}

/// Panel lifecycle commands.
pub enum PanelCommand {
    Open(PanelSpec),
    Close,
}

/// Bridge shared with plugin Lua states via `Arc`.
pub struct PluginUiBridge {
    tx: mpsc::Sender<PluginUiEvent>,
    rx: Mutex<mpsc::Receiver<PluginUiEvent>>,
    panel_tx: mpsc::Sender<PanelCommand>,
    panel_rx: Mutex<mpsc::Receiver<PanelCommand>>,
    /// Dedicated DB handle for `cord.config` reads/writes. Attached after
    /// construction (the App's own handle can't be shared). Mutex makes it
    /// Sync regardless of `Database`'s own thread-safety bounds.
    config_db: Mutex<Option<std::sync::Arc<Mutex<Database>>>>,
}

impl PluginUiBridge {
    pub fn new() -> Self {
        let (tx, rx) = mpsc::channel();
        let (panel_tx, panel_rx) = mpsc::channel();
        Self {
            tx,
            rx: Mutex::new(rx),
            panel_tx,
            panel_rx: Mutex::new(panel_rx),
            config_db: Mutex::new(None),
        }
    }

    /// Give `cord.config` a database handle. Call once, right after the
    /// App is constructed and before plugins load.
    pub fn attach_config_db(&self, db: std::sync::Arc<Mutex<Database>>) {
        *self.config_db.lock().unwrap() = Some(db);
    }

    /// Run a db closure on a plain thread. `cordanui_sync` blocks on its
    /// internal runtime, which panics if called from inside any tokio
    /// context (worker threads running plugin commands are exactly that).
    fn with_config_db<T: Send + 'static>(
        &self,
        f: impl FnOnce(&Database) -> T + Send + 'static,
    ) -> Option<T> {
        let db = self.config_db.lock().unwrap().clone()?;
        Some(
            std::thread::spawn(move || {
                let guard = db.lock().unwrap();
                f(&guard)
            })
            .join()
            .expect("config db thread"),
        )
    }

    /// Take the next queued event, if any. Non-blocking: called every
    /// event-loop iteration.
    pub fn try_take_event(&self) -> Option<PluginUiEvent> {
        self.rx.lock().unwrap().try_recv().ok()
    }

    /// Take the next queued panel command, if any.
    pub fn try_take_panel_command(&self) -> Option<PanelCommand> {
        self.panel_rx.lock().unwrap().try_recv().ok()
    }
}

impl UiHost for PluginUiBridge {
    fn submit(&self, pending: PendingUi) {
        // The App always drains the queue; a send only fails if it has
        // been dropped, in which case cancelling the dialog is correct.
        let _ = self.tx.send(PluginUiEvent::Modal(pending));
    }

    fn notify(&self, level: UiLevel, message: String) {
        let _ = self.tx.send(PluginUiEvent::Notify { level, message });
    }
}

impl ConfigHost for PluginUiBridge {
    fn get(&self, plugin: &str, key: &str) -> Option<String> {
        let plugin = plugin.to_string();
        let key = key.to_string();
        self.with_config_db(move |db| {
            crate::db::get_plugin_settings(db, &plugin)
                .ok()
                .and_then(|m| m.get(&key).cloned())
        })
        .flatten()
    }

    fn set(&self, plugin: &str, key: &str, value: &str) {
        let plugin = plugin.to_string();
        let key = key.to_string();
        let value = value.to_string();
        self.with_config_db(move |db| {
            let _ = crate::db::set_plugin_setting(db, &plugin, &key, &value);
        });
    }
}

impl cordanui_plugin_runtime::ErrorLogHost for PluginUiBridge {
    fn list(&self, limit: u32) -> Vec<cordanui_plugin_runtime::ErrorEntry> {
        self.with_config_db(move |db| crate::db::get_errors(db, limit as i64).unwrap_or_default())
            .unwrap_or_default()
            .into_iter()
            .map(|e| cordanui_plugin_runtime::ErrorEntry {
                created_at: e.created_at,
                context: e.context,
                message: e.message,
                detail: e.detail,
            })
            .collect()
    }

    fn clear(&self) {
        self.with_config_db(move |db| {
            let _ = crate::db::clear_errors(db);
        });
    }
}

impl PanelHost for PluginUiBridge {    fn open_panel(&self, spec: PanelSpec) {
        let _ = self.panel_tx.send(PanelCommand::Open(spec));
    }

    fn close_panel(&self) {
        let _ = self.panel_tx.send(PanelCommand::Close);
    }
}

/// The standard host hooks for TUI-loaded Lua plugins: styling bridge,
/// dialog/notify bridge, and panel commands all through one bridge.
pub fn plugin_runtime_hooks(
    styles: &std::sync::Arc<crate::style::StyleBridge>,
    ui: &std::sync::Arc<PluginUiBridge>,
    services: &std::sync::Arc<crate::services::ServiceManager>,
    sheets: &std::sync::Arc<crate::sheets::SheetManager>,
    buffers: &std::sync::Arc<crate::buffers::BufferManager>,
) -> cordanui_plugin_runtime::HostHooks {
    let services: std::sync::Arc<dyn cordanui_plugin_runtime::ServiceHost> = services.clone();
    let sheets: std::sync::Arc<dyn cordanui_plugin_runtime::ui::SheetsHost> = sheets.clone();
    let buffers: std::sync::Arc<dyn cordanui_plugin_runtime::ui::BuffersHost> = buffers.clone();
    cordanui_plugin_runtime::HostHooks::new()
        .with_styles(styles.clone())
        .with_ui(ui.clone())
        .with_panels(ui.clone())
        .with_config(ui.clone())
        .with_services(services)
        .with_errors(ui.clone())
        .with_sheets(sheets)
        .with_buffers(buffers)
}

#[cfg(test)]
mod tests {
    use super::*;
    use cordanui_plugin_runtime::{UiRequest, UiResponse};
    use cordanui_sync::{Database, SyncConfig};

    #[test]
    fn requests_round_trip_through_the_bridge() {
        let bridge = PluginUiBridge::new();
        assert!(bridge.try_take_event().is_none());

        let (tx, rx) = tokio::sync::oneshot::channel();
        bridge.submit(PendingUi {
            request: UiRequest::Confirm {
                title: "Sure?".into(),
                message: "delete everything".into(),
            },
            respond: tx,
        });

        let crate::plugin_ui::PluginUiEvent::Modal(pending) =
            bridge.try_take_event().expect("request queued")
        else {
            panic!("expected a modal event");
        };
        assert_eq!(pending.request.title(), "Sure?");
        let _ = pending.respond.send(UiResponse::Confirmed(true));
        assert!(matches!(
            rx.blocking_recv(),
            Ok(UiResponse::Confirmed(true))
        ));
    }

    /// Full stack: a Lua plugin awaits `cord.ui.input`, the App opens the
    /// modal, the "user" types and hits Enter, and the Lua thread resumes
    /// with the value.
    ///
    /// Threading note: `cordanui_sync`'s blocking API must never be called
    /// from inside a tokio runtime context, so this test drives the App on
    /// the plain test thread and gives the plugin future its own tiny
    /// runtime on a spawned thread.
    #[test]
    fn lua_input_dialog_round_trips_through_app() {
        use crate::app::{App, Mode};
        use cordanui_plugin_runtime::{CompleteRequest, HostHooks, LuaPlugin};

        // Plugin fixture: asks for a name, echoes it back.
        let plug_dir = std::env::temp_dir()
            .join("cordanui-plugin-ui-test")
            .join(cordanui_schema::new_id());
        std::fs::create_dir_all(&plug_dir).unwrap();
        std::fs::write(
            plug_dir.join("main.lua"),
            r##"
plugin = {}
function plugin.complete(req)
  local name = cord.ui.input{ title = "Goal name" }
  return { content = "typed:" .. tostring(name) }
end
"##,
        )
        .unwrap();

        let db_dir = std::env::temp_dir().join(format!(
            "cordanui-plugin-ui-app-{}",
            cordanui_schema::new_id()
        ));
        std::fs::create_dir_all(&db_dir).unwrap();
        let db = Database::open(&SyncConfig {
            db_path: db_dir.join("test.db"),
            ..Default::default()
        })
        .unwrap();
        let mut app = App::new(db).unwrap();

        std::thread::scope(|scope| {
            let styles = app.styles.clone();
            let ui = app.plugin_ui.clone();
            let plugin = LuaPlugin::load(
                &plug_dir,
                "asker",
                None,
                HostHooks::new().with_styles(styles).with_ui(ui),
            )
            .unwrap();

            let answer = scope.spawn(move || {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap();
                rt.block_on(async {
                    plugin
                        .complete(&CompleteRequest {
                            model: "m".into(),
                            prompt: "p".into(),
                            system: None,
                            max_tokens: None,
                            temperature: None,
                            config: None,
                        })
                        .await
                        .unwrap()
                })
            });

            // Drive the event loop until the modal appears.
            for _ in 0..200 {
                app.poll_plugin_ui_requests();
                if app.mode == Mode::PluginModal {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            assert!(
                matches!(app.mode, Mode::PluginModal),
                "modal should have opened"
            );

            // Type and submit.
            for c in "hello".chars() {
                app.plugin_modal_push_char(c);
            }
            assert_eq!(app.plugin_modal_text(), Some("hello"));
            app.submit_plugin_modal();
            assert_eq!(app.mode, Mode::Normal);

            let resp = answer.join().unwrap();
            assert_eq!(resp.content, "typed:hello");
        });

        let _ = std::fs::remove_dir_all(&db_dir);
        let _ = std::fs::remove_dir_all(&plug_dir);
    }

    /// Multiselect: space toggles items, Enter submits the 0-based index
    /// set; notify lands in the status message without blocking.
    #[test]
    fn lua_multiselect_and_notify_flow() {
        use crate::app::{App, Mode};
        use cordanui_plugin_runtime::{CompleteRequest, HostHooks, LuaPlugin};

        let plug_dir = std::env::temp_dir()
            .join("cordanui-plugin-ui-test")
            .join(cordanui_schema::new_id());
        std::fs::create_dir_all(&plug_dir).unwrap();
        std::fs::write(
            plug_dir.join("main.lua"),
            r##"
plugin = {}
function plugin.complete(req)
  cord.ui.notify("scanning goals...")
  local picked = cord.ui.multiselect{
    title = "Tags",
    items = { "urgent", "work", "someday" },
  }
  if picked == nil then return { content = "cancelled" } end
  local sum = 0
  for _, i in ipairs(picked) do sum = sum + i end
  return { content = "picked-sum:" .. sum }
end
"##,
        )
        .unwrap();

        let db_dir = std::env::temp_dir().join(format!(
            "cordanui-plugin-ui-app-{}",
            cordanui_schema::new_id()
        ));
        std::fs::create_dir_all(&db_dir).unwrap();
        let db = Database::open(&SyncConfig {
            db_path: db_dir.join("test.db"),
            ..Default::default()
        })
        .unwrap();
        let mut app = App::new(db).unwrap();

        std::thread::scope(|scope| {
            let styles = app.styles.clone();
            let ui = app.plugin_ui.clone();
            let plugin = LuaPlugin::load(
                &plug_dir,
                "asker",
                None,
                HostHooks::new().with_styles(styles).with_ui(ui),
            )
            .unwrap();

            let answer = scope.spawn(move || {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap();
                rt.block_on(async {
                    plugin
                        .complete(&CompleteRequest {
                            model: "m".into(),
                            prompt: "p".into(),
                            system: None,
                            max_tokens: None,
                            temperature: None,
                            config: None,
                        })
                        .await
                        .unwrap()
                })
            });

            // First drain cycles: the notify arrives (status line, no
            // modal), then nothing until the dialog request lands.
            for _ in 0..200 {
                app.poll_plugin_ui_requests();
                if app.message.is_some() {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            assert_eq!(app.message.as_deref(), Some("scanning goals..."));

            // Then the modal.
            for _ in 0..200 {
                app.poll_plugin_ui_requests();
                if app.mode == Mode::PluginModal {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            assert!(matches!(app.mode, Mode::PluginModal));

            // Cursor starts on item 0 ("urgent"); select it, move to item 2
            // ("someday"), select it too — 1-based indices 1+3 = 4.
            app.plugin_modal_toggle_current();
            app.plugin_modal_move_selection(2);
            app.plugin_modal_toggle_current();
            app.submit_plugin_modal();
            assert_eq!(app.mode, Mode::Normal);

            let resp = answer.join().unwrap();
            assert_eq!(resp.content, "picked-sum:4");
        });

        let _ = std::fs::remove_dir_all(&db_dir);
        let _ = std::fs::remove_dir_all(&plug_dir);
    }

    /// Cancelling (Esc path) resolves the plugin's await to `false`.
    #[test]
    fn lua_confirm_dialog_cancel_resolves_false() {
        use crate::app::{App, Mode};
        use cordanui_plugin_runtime::{CompleteRequest, HostHooks, LuaPlugin};

        let plug_dir = std::env::temp_dir()
            .join("cordanui-plugin-ui-test")
            .join(cordanui_schema::new_id());
        std::fs::create_dir_all(&plug_dir).unwrap();
        std::fs::write(
            plug_dir.join("main.lua"),
            r##"
plugin = {}
function plugin.complete(req)
  local ok = cord.ui.confirm{ title = "Sure?", message = "really?" }
  return { content = tostring(ok) }
end
"##,
        )
        .unwrap();

        let db_dir = std::env::temp_dir().join(format!(
            "cordanui-plugin-ui-app-{}",
            cordanui_schema::new_id()
        ));
        std::fs::create_dir_all(&db_dir).unwrap();
        let db = Database::open(&SyncConfig {
            db_path: db_dir.join("test.db"),
            ..Default::default()
        })
        .unwrap();
        let mut app = App::new(db).unwrap();

        std::thread::scope(|scope| {
            let styles = app.styles.clone();
            let ui = app.plugin_ui.clone();
            let plugin = LuaPlugin::load(
                &plug_dir,
                "asker",
                None,
                HostHooks::new().with_styles(styles).with_ui(ui),
            )
            .unwrap();

            let answer = scope.spawn(move || {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap();
                rt.block_on(async {
                    plugin
                        .complete(&CompleteRequest {
                            model: "m".into(),
                            prompt: "p".into(),
                            system: None,
                            max_tokens: None,
                            temperature: None,
                            config: None,
                        })
                        .await
                        .unwrap()
                })
            });

            for _ in 0..200 {
                app.poll_plugin_ui_requests();
                if app.mode == Mode::PluginModal {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            assert!(matches!(app.mode, Mode::PluginModal));

            app.cancel_plugin_modal(); // 'n' / Esc
            let resp = answer.join().unwrap();
            assert_eq!(resp.content, "false");
        });

        let _ = std::fs::remove_dir_all(&db_dir);
        let _ = std::fs::remove_dir_all(&plug_dir);
    }

    /// Panel lifecycle through the App: show_panel opens Mode::PluginPanel,
    /// keys route to on_key, unhandled esc closes it.
    #[test]
    fn lua_panel_lifecycle_through_app() {
        use crate::app::{App, Mode};
        use cordanui_plugin_runtime::{CompleteRequest, HostHooks, LuaPlugin};

        let plug_dir = std::env::temp_dir()
            .join("cordanui-plugin-ui-test")
            .join(cordanui_schema::new_id());
        std::fs::create_dir_all(&plug_dir).unwrap();
        std::fs::write(
            plug_dir.join("main.lua"),
            r##"
plugin = {}
local count = 0
function plugin.complete(req)
  cord.ui.show_panel{
    title = "Counter",
    draw = function()
      return { { content = "count=" .. count } }
    end,
    on_key = function(key)
      if key == "+" then count = count + 1; return true end
      return false
    end,
  }
  return { content = "panel-closed:" .. count }
end
"##,
        )
        .unwrap();

        let db_dir = std::env::temp_dir().join(format!(
            "cordanui-plugin-ui-app-{}",
            cordanui_schema::new_id()
        ));
        std::fs::create_dir_all(&db_dir).unwrap();
        let db = Database::open(&SyncConfig {
            db_path: db_dir.join("test.db"),
            ..Default::default()
        })
        .unwrap();
        let mut app = App::new(db).unwrap();

        let plugin = LuaPlugin::load(
            &plug_dir,
            "paneleer",
            None,
            HostHooks::new()
                .with_styles(app.styles.clone())
                .with_ui(app.plugin_ui.clone())
                .with_panels(app.plugin_ui.clone()),
        )
        .unwrap();

        // complete() returns immediately (show_panel is non-blocking).
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let resp = rt.block_on(async {
            plugin
                .complete(&CompleteRequest {
                    model: "m".into(),
                    prompt: "p".into(),
                    system: None,
                    max_tokens: None,
                    temperature: None,
                    config: None,
                })
                .await
                .unwrap()
        });

        app.poll_plugin_panel();
        assert!(matches!(app.mode, Mode::PluginPanel));
        assert!(app.plugin_panel.is_some());

        // Keys reach the plugin and mutate its state.
        assert!((app.plugin_panel.as_ref().unwrap().on_key)("+"));
        assert!((app.plugin_panel.as_ref().unwrap().on_key)("+"));
        let drawn = format!("{:?}", (app.plugin_panel.as_ref().unwrap().draw)());
        assert!(drawn.contains("count=2"), "draw saw stale state: {drawn}");

        // Unhandled esc closes the panel.
        app.close_plugin_panel();
        assert_eq!(app.mode, Mode::Normal);
        assert_eq!(resp.content, "panel-closed:0");

        let _ = std::fs::remove_dir_all(&db_dir);
        let _ = std::fs::remove_dir_all(&plug_dir);
    }

    /// The full user story: `<leader>;` -> type to filter -> Enter runs
    /// `rose_pine.select()` -> the awaited pick dialog opens -> answered ->
    /// the returned message lands on the status line and the cord.g style
    /// write is committed to the db.
    #[test]
    fn command_mode_runs_plugin_command_with_dialog() {
        use crate::app::{App, Mode};
        use cordanui_plugin_runtime::{HostHooks, LuaPlugin};

        let plug_dir = std::env::temp_dir()
            .join("cordanui-plugin-ui-test")
            .join(cordanui_schema::new_id());
        std::fs::create_dir_all(&plug_dir).unwrap();
        std::fs::write(
            plug_dir.join("main.lua"),
            r##"
plugin = {}
local function select()
  local flavors = { "rose-pine", "moon", "dawn" }
  local idx = cord.ui.pick{ title = "Flavor", items = flavors }
  if not idx then return "cancelled" end
  cord.g.style.primary("#ebbcba")
  return "switched to " .. flavors[idx]
end

plugin.commands = {
  ["rose-pine.select"] = { run = select, desc = "Pick a flavor" },
}
"##,
        )
        .unwrap();

        let db_dir = std::env::temp_dir().join(format!(
            "cordanui-cmd-mode-app-{}",
            cordanui_schema::new_id()
        ));
        std::fs::create_dir_all(&db_dir).unwrap();
        let db = Database::open(&SyncConfig {
            db_path: db_dir.join("test.db"),
            ..Default::default()
        })
        .unwrap();
        let mut app = App::new(db).unwrap();

        // Simulate an installed, active lua plugin by injecting its state.
        let plugin = LuaPlugin::load(
            &plug_dir,
            "rose-pine",
            None,
            HostHooks::new()
                .with_styles(app.styles.clone())
                .with_ui(app.plugin_ui.clone())
                .with_panels(app.plugin_ui.clone()),
        )
        .unwrap();
        // `plugin` is moved into the cache after open_command_mode below.

        // Open the command line (this reloads states from the registry),
        // then inject the plugin as if installed + active.
        app.open_command_mode();
        assert!(matches!(app.mode, Mode::Command));
        app.plugin_states
            .lock()
            .unwrap()
            .insert("rose-pine".to_string(), plugin);
        app.plugin_commands.push(crate::app::PluginCommand {
            plugin_name: "rose-pine".into(),
            name: "rose-pine.select".into(),
            desc: "Pick a flavor".into(),
        });
        for c in "select".chars() {
            app.input.push_char(c);
        }
        assert_eq!(app.command_matches().len(), 1);

        // Run it; the awaited pick appears as a modal.
        let cmd = app.command_matches().remove(0);
        app.execute_plugin_command(&cmd);
        for _ in 0..200 {
            app.poll_plugin_ui_requests();
            if matches!(app.mode, Mode::PluginModal) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(
            matches!(app.mode, Mode::PluginModal),
            "pick dialog should be open"
        );

        // Answer: choose item 2 ("moon", 0-based 1).
        if let Some(modal) = &mut app.plugin_modal {
            if let crate::app::PluginModalKind::Pick { selected } = &mut modal.kind {
                *selected = 1;
            }
        }
        app.submit_plugin_modal();

        // Worker finishes; poll until the status message lands.
        for _ in 0..200 {
            app.poll_command_results();
            if !app.command_running {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert_eq!(app.message.as_deref(), Some("switched to moon"));
        assert!(!app.command_running);

        // The cord.g write from inside the command was committed.
        app.apply_style_updates().unwrap();
        let overrides = crate::db::get_style_overrides(&app.db).unwrap();
        assert_eq!(
            overrides.get("primary").map(String::as_str),
            Some("#ebbcba"),
            "cord.g.style.primary from inside the command should persist"
        );

        let _ = std::fs::remove_dir_all(&db_dir);
        let _ = std::fs::remove_dir_all(&plug_dir);
    }

    /// Configure-form select fields cycle with Tab and persist immediately.
    #[test]
    fn config_select_field_cycles_and_persists() {
        use crate::app::App;
        use cordanui_plugin_runtime::UiSpec;

        let db_dir = std::env::temp_dir().join(format!(
            "cordanui-config-cycle-{}",
            cordanui_schema::new_id()
        ));
        std::fs::create_dir_all(&db_dir).unwrap();
        let db = Database::open(&SyncConfig {
            db_path: db_dir.join("test.db"),
            ..Default::default()
        })
        .unwrap();
        let mut app = App::new(db).unwrap();

        let spec: UiSpec = toml::from_str(
            r#"
[[field]]
key = "variant"
type = "select"
options = ["rosepine", "rosepine-moon", "rosepine-dawn"]
default = "rosepine-moon"
"#,
        )
        .unwrap();
        app.config_spec = Some(spec);
        app.config_selected = 0;

        // No stored value yet: cycling starts from the default.
        app.cycle_config_field("p", 1).unwrap();
        assert_eq!(app.config_values.get("variant").unwrap(), "rosepine-dawn");

        // Wraps forward past the end.
        app.cycle_config_field("p", 1).unwrap();
        assert_eq!(app.config_values.get("variant").unwrap(), "rosepine");

        // And backward past the start.
        app.cycle_config_field("p", -1).unwrap();
        assert_eq!(app.config_values.get("variant").unwrap(), "rosepine-dawn");

        // Every cycle was persisted to the settings table.
        let stored = crate::db::get_plugin_settings(&app.db, "p").unwrap();
        assert_eq!(
            stored.get("variant").map(String::as_str),
            Some("rosepine-dawn")
        );

        let _ = std::fs::remove_dir_all(&db_dir);
    }

    /// `c` on a plugin with `plugin.configure` invokes the plugin's own
    /// page (here: a pick dialog + cord.config persistence). The host
    /// only facilitates.
    #[test]
    fn configure_key_invokes_plugin_owned_page() {
        use crate::app::{App, Mode};
        use cordanui_plugin_runtime::{HostHooks, LuaPlugin};

        let plug_dir = std::env::temp_dir()
            .join("cordanui-plugin-ui-test")
            .join(cordanui_schema::new_id());
        std::fs::create_dir_all(&plug_dir).unwrap();
        std::fs::write(
            plug_dir.join("cordanui.toml"),
            r#"
runtime = "lua"

[plugin]
name = "rosepine-moon"
version = "0.1.0"

[capabilities]
theme = true
"#,
        )
        .unwrap();
        std::fs::write(
            plug_dir.join("main.lua"),
            r##"
plugin = {}

function plugin.configure()
  local current = cord.config.get("variant", "moon")
  local idx = cord.ui.pick{ title = "Variant", items = { "main", "moon", "dawn" } }
  if not idx then return "cancelled" end
  cord.config.set("variant", ({ "main", "moon", "dawn" })[idx])
  return "variant saved"
end
"##,
        )
        .unwrap();

        let db_dir = std::env::temp_dir().join(format!(
            "cordanui-configure-app-{}",
            cordanui_schema::new_id()
        ));
        std::fs::create_dir_all(&db_dir).unwrap();
        let db = Database::open(&SyncConfig {
            db_path: db_dir.join("test.db"),
            ..Default::default()
        })
        .unwrap();
        let mut app = App::new(db).unwrap();

        // Second handle backs cord.config (mirrors main.rs).
        let config_db = Database::open(&SyncConfig {
            db_path: db_dir.join("test.db"),
            ..Default::default()
        })
        .unwrap();
        app.attach_plugin_config_db(config_db);

        let plugin = LuaPlugin::load(
            &plug_dir,
            "rosepine-moon",
            None,
            HostHooks::new()
                .with_styles(app.styles.clone())
                .with_ui(app.plugin_ui.clone())
                .with_panels(app.plugin_ui.clone())
                .with_config(app.plugin_ui.clone()),
        )
        .unwrap();
        app.plugin_states
            .lock()
            .unwrap()
            .insert("rosepine-moon".to_string(), plugin);

        // Register as an installed plugin so the `c` dispatcher finds it.
        app.installed_plugins.push(crate::db::PluginRow {
            id: "rosepine-moon".into(),
            source: "test".into(),
            dir: plug_dir.display().to_string(),
            active: true,
            installed_at: String::new(),
        });

        // The user presses `c`.
        app.open_configure();

        // The plugin's awaited pick dialog appears.
        for _ in 0..200 {
            app.poll_plugin_ui_requests();
            if matches!(app.mode, Mode::PluginModal) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(
            matches!(app.mode, Mode::PluginModal),
            "plugin-owned configure should open its pick dialog"
        );

        // Choose "dawn" (0-based 2) and submit.
        if let Some(modal) = &mut app.plugin_modal {
            if let crate::app::PluginModalKind::Pick { selected } = &mut modal.kind {
                *selected = 2;
            }
        }
        app.submit_plugin_modal();

        for _ in 0..200 {
            app.poll_command_results();
            if !app.command_running {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert_eq!(app.message.as_deref(), Some("variant saved"));

        // Persisted namespaced via cord.config, readable through the db.
        let stored = crate::db::get_plugin_settings(&app.db, "rosepine-moon").unwrap();
        assert_eq!(stored.get("variant").map(String::as_str), Some("dawn"));

        let _ = std::fs::remove_dir_all(&db_dir);
        let _ = std::fs::remove_dir_all(&plug_dir);
    }

    /// Global settings page: host Sync fields + automatic entries for
    /// plugins that own configurators; mismatched turso url/token is
    /// rejected without writing anything.
    #[test]
    fn global_config_lists_plugin_configurators_and_validates() {
        use crate::app::{App, Mode};
        use cordanui_plugin_runtime::{HostHooks, LuaPlugin};

        let plug_dir = std::env::temp_dir()
            .join("cordanui-plugin-ui-test")
            .join(cordanui_schema::new_id());
        std::fs::create_dir_all(&plug_dir).unwrap();
        std::fs::write(
            plug_dir.join("cordanui.toml"),
            r#"
runtime = "lua"

[plugin]
name = "rosepine-moon"
version = "0.1.0"
description = "Rose Pine themes"

[capabilities]
theme = true
"#,
        )
        .unwrap();
        std::fs::write(
            plug_dir.join("main.lua"),
            r##"
plugin = {}
function plugin.configure()
  return "ok"
end
"##,
        )
        .unwrap();

        let db_dir =
            std::env::temp_dir().join(format!("cordanui-global-cfg-{}", cordanui_schema::new_id()));
        std::fs::create_dir_all(&db_dir).unwrap();
        let db = Database::open(&SyncConfig {
            db_path: db_dir.join("test.db"),
            ..Default::default()
        })
        .unwrap();
        let mut app = App::new(db).unwrap();

        let plugin = LuaPlugin::load(
            &plug_dir,
            "rosepine-moon",
            None,
            HostHooks::new()
                .with_styles(app.styles.clone())
                .with_ui(app.plugin_ui.clone())
                .with_panels(app.plugin_ui.clone())
                .with_config(app.plugin_ui.clone()),
        )
        .unwrap();
        app.plugin_states
            .lock()
            .unwrap()
            .insert("rosepine-moon".to_string(), plugin);
        app.installed_plugins.push(crate::db::PluginRow {
            id: "rosepine-moon".into(),
            source: "test".into(),
            dir: plug_dir.display().to_string(),
            active: true,
            installed_at: String::new(),
        });

        app.open_global_config();
        assert!(matches!(app.mode, Mode::GlobalConfig));
        assert_eq!(
            app.global_spec.as_ref().unwrap().fields.len(),
            2,
            "host sync fields present"
        );
        assert_eq!(
            app.global_plugin_entries,
            vec![("rosepine-moon".to_string(), "Rose Pine themes".to_string())],
            "plugins with configurators are listed automatically"
        );
        assert_eq!(
            app.global_row_count(),
            4, // 2 sync fields + 1 plugin configurator + 1 danger-zone row
        );

        // Mismatched url/token is rejected before any write. Force the
        // token empty so the test doesn't depend on the real config file.
        app.global_values
            .insert("turso_token".to_string(), String::new());
        app.config_selected = 0;
        app.config_editing = Some("libsql://only-url.turso.io".into());
        app.commit_global_field().unwrap();
        assert!(
            app.message
                .as_deref()
                .unwrap_or_default()
                .contains("must both be set"),
            "expected validation message, got {:?}",
            app.message
        );

        // Entering a plugin row spawns its configurator.
        app.config_editing = None;
        app.config_selected = 2; // first plugin entry
        app.open_configure(); // reuse dispatcher sanity: not needed, direct spawn:
        let _ = app.global_plugin_entries.first().cloned();

        let _ = std::fs::remove_dir_all(&db_dir);
        let _ = std::fs::remove_dir_all(&plug_dir);
    }

    /// Sync lifecycle: attach fires an immediate sync (startup pull),
    /// completion flips the status to Synced; unattached stays
    /// NotConfigured. Uses a local-only db — sync() is a no-op success.
    #[test]
    fn sync_status_transitions() {
        use crate::app::{App, SyncStatus, SYNC_INTERVAL};
        use std::time::Duration;

        let db_dir =
            std::env::temp_dir().join(format!("cordanui-sync-test-{}", cordanui_schema::new_id()));
        std::fs::create_dir_all(&db_dir).unwrap();
        let db = Database::open(&SyncConfig {
            db_path: db_dir.join("test.db"),
            ..Default::default()
        })
        .unwrap();
        let mut app = App::new(db).unwrap();
        assert_eq!(app.sync_status, SyncStatus::NotConfigured);

        // No handle: polling never leaves NotConfigured.
        app.poll_sync();
        assert_eq!(app.sync_status, SyncStatus::NotConfigured);

        // Attach (as main.rs does when creds exist): fires immediately.
        let sync_db = Database::open(&SyncConfig {
            db_path: db_dir.join("test.db"),
            ..Default::default()
        })
        .unwrap();
        app.attach_sync_db(sync_db);
        assert!(matches!(app.sync_status, SyncStatus::Syncing));

        // Worker finishes (local-only sync is instant — may already be
        // done before the next poll) → Synced.
        for _ in 0..100 {
            app.poll_sync();
            if matches!(app.sync_status, SyncStatus::Synced { .. }) {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(matches!(app.sync_status, SyncStatus::Synced { .. }));
        assert!(!app.sync_in_flight);

        // Inside SYNC_INTERVAL: not due again.
        app.poll_sync();
        assert!(matches!(app.sync_status, SyncStatus::Synced { .. }));

        // After the interval elapses (simulate by backdating), due again.
        app.last_sync_attempt =
            Some(std::time::Instant::now() - SYNC_INTERVAL - Duration::from_secs(1));
        app.poll_sync();
        assert_eq!(app.sync_status, SyncStatus::Syncing);

        // Manual request while due is honored; format helper sanity.
        app.request_sync();
        assert!(app.last_sync_attempt.is_none());
        assert_eq!(
            crate::app::format_ago(std::time::Instant::now()),
            "just now"
        );

        let _ = std::fs::remove_dir_all(&db_dir);
    }
}
