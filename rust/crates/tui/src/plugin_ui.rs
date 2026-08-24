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

use cordanui_plugin_runtime::{PanelHost, PanelSpec, PendingUi, UiHost, UiLevel, UiResponse};

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
        }
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

impl PanelHost for PluginUiBridge {
    fn open_panel(&self, spec: PanelSpec) {
        let _ = self.panel_tx.send(PanelCommand::Open(spec));
    }

    fn close_panel(&self) {
        let _ = self.panel_tx.send(PanelCommand::Close);
    }
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
            let plugin = LuaPlugin::load(&plug_dir, "asker", None, HostHooks::new().with_styles(styles).with_ui(ui)).unwrap();

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
            let plugin = LuaPlugin::load(&plug_dir, "asker", None, HostHooks::new().with_styles(styles).with_ui(ui)).unwrap();

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
            let plugin = LuaPlugin::load(&plug_dir, "asker", None, HostHooks::new().with_styles(styles).with_ui(ui)).unwrap();

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

        let db_dir = std::env::temp_dir()
            .join(format!("cordanui-plugin-ui-app-{}", cordanui_schema::new_id()));
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

}
