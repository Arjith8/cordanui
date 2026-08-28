//! Keybind configuration.
//!
//! Loaded from the `[keybinds]` section of `~/.config/cordanui/config.toml`
//! (the same file that holds `[turso]`). Every binding is optional — missing
//! entries fall back to the defaults:
//!
//! ```toml
//! [keybinds]
//! leader          = "ctrl+a"   # prefix key that arms command mode
//! new_goal        = "n"        # <leader> new goal
//! show_details    = "tab"      # <leader> toggle description + subgoals
//! cycle_status    = "tab"      # bare key: cycle pending → wip → done
//! help            = "h"        # <leader> open the help page
//! plugins         = "p"        # <leader> open the plugin manager
//! run_agent       = "r"        # <leader> run goal with an agent
//! commands        = ";"        # <leader> open the plugin-command line
//! global_config   = ","        # <leader> open the global settings page
//! sync            = "s"        # <leader> sync with Turso now
//! delete          = "d"        # bare key: delete selected goal (confirm)
//! edit_title      = "e"        # bare key: edit title
//! edit_description= "E"        # bare key: edit description
//! toggle_complete = "space"    # bare key: toggle complete
//! move_goal       = "m"        # bare key: move goal to new parent
//! sheets          = "b"        # <leader> + key: open sheet picker
//! ```
//!
//! Key syntax: lowercase key names joined by `+`, modifiers first
//! (`ctrl+a`, `shift+tab`, `alt+enter`, `space`, `f1`, plain chars).

use std::path::PathBuf;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// A parsed, matchable key binding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyBinding {
    pub code: KeyCode,
    pub modifiers: KeyModifiers,
}

impl KeyBinding {
    /// Parse a binding description like `"ctrl+shift+tab"` or `"n"`.
    /// Returns `None` for unparseable input (the caller keeps the default).
    pub fn parse(s: &str) -> Option<Self> {
        let mut code = None;
        let mut mods = KeyModifiers::NONE;

        for part in s.split('+').map(str::trim).filter(|p| !p.is_empty()) {
            let lower = part.to_ascii_lowercase();
            match lower.as_str() {
                "ctrl" | "control" => mods |= KeyModifiers::CONTROL,
                "alt" | "option" => mods |= KeyModifiers::ALT,
                "shift" => mods |= KeyModifiers::SHIFT,
                "super" | "cmd" | "meta" => mods |= KeyModifiers::SUPER,
                _ => {
                    if code.is_some() {
                        return None; // two non-modifier parts — invalid
                    }
                    code = Some(match lower.as_str() {
                        "esc" | "escape" => KeyCode::Esc,
                        "enter" | "return" => KeyCode::Enter,
                        "tab" => KeyCode::Tab,
                        "backtab" => KeyCode::BackTab,
                        "space" => KeyCode::Char(' '),
                        "backspace" => KeyCode::Backspace,
                        "delete" | "del" => KeyCode::Delete,
                        "insert" | "ins" => KeyCode::Insert,
                        "home" => KeyCode::Home,
                        "end" => KeyCode::End,
                        "pageup" => KeyCode::PageUp,
                        "pagedown" => KeyCode::PageDown,
                        "up" => KeyCode::Up,
                        "down" => KeyCode::Down,
                        "left" => KeyCode::Left,
                        "right" => KeyCode::Right,
                        f if f.len() == 2
                            && f.starts_with('f')
                            && f[1..].bytes().all(|b| b.is_ascii_digit()) =>
                        {
                            KeyCode::F(f[1..].parse().ok()?)
                        }
                        c if c.chars().count() == 1 => {
                            let ch = c.chars().next()?;
                            if ch.is_ascii_uppercase() {
                                // Uppercase chars imply shift so matching is
                                // case-sensitive but forgiving ("A" == "a"+shift).
                                mods |= KeyModifiers::SHIFT;
                                KeyCode::Char(ch)
                            } else {
                                KeyCode::Char(ch)
                            }
                        }
                        _ => return None,
                    });
                }
            }
        }

        Some(Self {
            code: code?,
            modifiers: mods,
        })
    }

    /// Whether a received key event matches this binding. Modifier matching
    /// ignores SHIFT on printable characters (terminals report shifted chars
    /// inconsistently across platforms).
    pub fn matches(&self, key: KeyEvent) -> bool {
        let relevant = key.modifiers & !(KeyModifiers::SHIFT);
        let self_relevant = self.modifiers & !(KeyModifiers::SHIFT);
        self.code == key.code && self_relevant == relevant
    }

    /// Human-readable label for hints/help (e.g. `"C-a"`, `"tab"`).
    pub fn label(&self) -> String {
        let mut parts = Vec::new();
        if self.modifiers.contains(KeyModifiers::CONTROL) {
            parts.push("C");
        }
        if self.modifiers.contains(KeyModifiers::ALT) {
            parts.push("M");
        }
        if self.modifiers.contains(KeyModifiers::SUPER) {
            parts.push("Super");
        }
        let key = match self.code {
            KeyCode::Char(' ') => "space".to_string(),
            KeyCode::Char(c) => c.to_string(),
            KeyCode::F(n) => format!("F{n}"),
            other => format!("{other:?}").to_lowercase(),
        };
        parts.push(&key);
        parts.join("-")
    }
}

/// The configurable bindings.
#[derive(Debug, Clone)]
pub struct Keybinds {
    /// Prefix key that arms leader/command mode.
    pub leader: KeyBinding,
    /// `<leader> + this` starts adding a new root goal.
    pub new_goal: KeyBinding,
    /// `<leader> + this` toggles the selected goal's description + subgoals.
    pub show_details: KeyBinding,
    /// Bare key (no leader) cycling the selected goal's status.
    pub cycle_status: KeyBinding,
    /// `<leader> + this` opens the help page.
    pub help: KeyBinding,
    /// `<leader> + this` opens the plugin manager popup.
    pub plugins: KeyBinding,
    /// `<leader> + this` runs the selected goal through a provider plugin.
    pub run_agent: KeyBinding,
    /// `<leader> + this` opens the plugin-command line.
    pub commands: KeyBinding,
    /// `<leader> + this` opens the global settings page.
    pub global_config: KeyBinding,
    /// `<leader> + this` triggers an immediate replica sync.
    pub sync: KeyBinding,
    /// Bare key deleting the selected goal (with confirmation).
    pub delete: KeyBinding,
    /// Bare key editing the selected goal's title.
    pub edit_title: KeyBinding,
    /// Bare key editing the selected goal's description.
    pub edit_description: KeyBinding,
    /// Bare key toggling complete on the selected goal.
    pub toggle_complete: KeyBinding,
    /// Bare key moving the selected goal under another parent.
    pub move_goal: KeyBinding,
    /// <leader> + this opens the sheet (buffer) picker.
    pub sheets: KeyBinding,
}

impl Default for Keybinds {
    fn default() -> Self {
        Self {
            leader: KeyBinding::parse("ctrl+a").unwrap(),
            new_goal: KeyBinding::parse("n").unwrap(),
            show_details: KeyBinding::parse("tab").unwrap(),
            cycle_status: KeyBinding::parse("tab").unwrap(),
            help: KeyBinding::parse("h").unwrap(),
            plugins: KeyBinding::parse("p").unwrap(),
            run_agent: KeyBinding::parse("r").unwrap(),
            commands: KeyBinding::parse(";").unwrap(),
            global_config: KeyBinding::parse(",").unwrap(),
            sync: KeyBinding::parse("s").unwrap(),
            delete: KeyBinding::parse("d").unwrap(),
            edit_title: KeyBinding::parse("e").unwrap(),
            edit_description: KeyBinding::parse("E").unwrap(),
            toggle_complete: KeyBinding::parse("space").unwrap(),
            move_goal: KeyBinding::parse("m").unwrap(),
            sheets: KeyBinding::parse("b").unwrap(),
        }
    }
}

impl Keybinds {
    /// Load bindings from `~/.config/cordanui/config.toml`. Falls back to
    /// defaults for a missing file, missing section, or any parse error —
    /// bad config must never prevent startup.
    pub fn load() -> Self {
        Self::from_toml_path(&config_file_path())
    }

    /// Same as [`load`] but from an explicit path (used by tests).
    pub fn from_toml_path(path: &PathBuf) -> Self {
        let defaults = Self::default();
        let Ok(contents) = std::fs::read_to_string(path) else {
            return defaults;
        };
        let Ok(parsed) = contents.parse::<toml::Value>() else {
            return defaults;
        };
        let Some(section) = parsed.get("keybinds") else {
            return defaults;
        };

        let get = |key: &str, fallback: &KeyBinding| -> KeyBinding {
            section
                .get(key)
                .and_then(|v| v.as_str())
                .and_then(KeyBinding::parse)
                .unwrap_or_else(|| fallback.clone())
        };

        Self {
            leader: get("leader", &defaults.leader),
            new_goal: get("new_goal", &defaults.new_goal),
            show_details: get("show_details", &defaults.show_details),
            cycle_status: get("cycle_status", &defaults.cycle_status),
            help: get("help", &defaults.help),
            plugins: get("plugins", &defaults.plugins),
            run_agent: get("run_agent", &defaults.run_agent),
            commands: get("commands", &defaults.commands),
            global_config: get("global_config", &defaults.global_config),
            sync: get("sync", &defaults.sync),
            delete: get("delete", &defaults.delete),
            edit_title: get("edit_title", &defaults.edit_title),
            edit_description: get("edit_description", &defaults.edit_description),
            toggle_complete: get("toggle_complete", &defaults.toggle_complete),
            move_goal: get("move_goal", &defaults.move_goal),
            sheets: get("sheets", &defaults.sheets),
        }
    }

    /// All bindings with their config key name, description, and whether the
    /// value was customized away from the default. Drives the help page.
    pub fn entries(&self) -> Vec<BindingEntry> {
        let d = Self::default();
        let mut v = Vec::new();
        let mut push =
            |name: &'static str, bind: &KeyBinding, def: &KeyBinding, desc: &'static str| {
                v.push(BindingEntry {
                    name,
                    binding: bind.clone(),
                    desc,
                    is_default: bind == def,
                });
            };
        push("leader", &self.leader, &d.leader, "arm leader mode");
        push(
            "new_goal",
            &self.new_goal,
            &d.new_goal,
            "<leader> + key — add a goal (subgoal if selection expanded)",
        );
        push(
            "show_details",
            &self.show_details,
            &d.show_details,
            "<leader> + key — toggle description + subgoals",
        );
        push(
            "cycle_status",
            &self.cycle_status,
            &d.cycle_status,
            "bare key — cycle pending → in progress → done",
        );
        push(
            "help",
            &self.help,
            &d.help,
            "<leader> + key — open this help page",
        );
        push(
            "plugins",
            &self.plugins,
            &d.plugins,
            "<leader> + key — open the plugin manager",
        );
        // `run_agent` is deliberately absent from the help page: agent runs
        // are a plugin-facilitated capability. The binding still works
        // (`<leader>r`) but only does something once the user has installed
        // an active provider plugin — advertising it out of the box just
        // produces "no active provider plugins" errors.
        push(
            "commands",
            &self.commands,
            &d.commands,
            "<leader> + key — open the plugin-command line",
        );
        push(
            "global_config",
            &self.global_config,
            &d.global_config,
            "<leader> + key — open the global settings page",
        );
        push(
            "sync",
            &self.sync,
            &d.sync,
            "<leader> + key — sync with Turso now",
        );
        push(
            "delete",
            &self.delete,
            &d.delete,
            "delete selected goal + subgoals (confirm)",
        );
        push(
            "edit_title",
            &self.edit_title,
            &d.edit_title,
            "edit selected goal title",
        );
        push(
            "edit_description",
            &self.edit_description,
            &d.edit_description,
            "edit selected goal description",
        );
        push(
            "toggle_complete",
            &self.toggle_complete,
            &d.toggle_complete,
            "toggle complete on selected goal",
        );
        push(
            "move_goal",
            &self.move_goal,
            &d.move_goal,
            "move selected goal under another parent / to root",
        );
        push(
            "sheets",
            &self.sheets,
            &d.sheets,
            "<leader> + key — open sheet (buffer) picker",
        );
        v
    }
}

/// One row of the help page: a binding's name, current key, and origin.
#[derive(Debug, Clone)]
pub struct BindingEntry {
    /// Config key under `[keybinds]`.
    pub name: &'static str,
    /// Currently active binding.
    pub binding: KeyBinding,
    /// What the binding does.
    pub desc: &'static str,
    /// Whether it matches the builtin default.
    pub is_default: bool,
}

/// `~/.config/cordanui/config.toml` (honors XDG_CONFIG_HOME via `dirs`).
fn config_file_path() -> PathBuf {
    let base = dirs::config_dir().unwrap_or_else(|| PathBuf::from("."));
    base.join("cordanui").join("config.toml")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_plain_char() {
        let b = KeyBinding::parse("n").unwrap();
        assert_eq!(b.code, KeyCode::Char('n'));
        assert_eq!(b.modifiers, KeyModifiers::NONE);
    }

    #[test]
    fn parses_ctrl_combo() {
        let b = KeyBinding::parse("ctrl+a").unwrap();
        assert_eq!(b.code, KeyCode::Char('a'));
        assert!(b.modifiers.contains(KeyModifiers::CONTROL));
    }

    #[test]
    fn parses_special_keys() {
        assert_eq!(KeyBinding::parse("tab").unwrap().code, KeyCode::Tab);
        assert_eq!(KeyBinding::parse("space").unwrap().code, KeyCode::Char(' '));
        assert_eq!(KeyBinding::parse("esc").unwrap().code, KeyCode::Esc);
        assert_eq!(KeyBinding::parse("f2").unwrap().code, KeyCode::F(2));
    }

    #[test]
    fn rejects_garbage() {
        assert!(KeyBinding::parse("").is_none());
        assert!(KeyBinding::parse("ctrl+alt+x+y").is_none());
        assert!(KeyBinding::parse("notakey!!").is_none());
        assert!(KeyBinding::parse("ctrl").is_none());
    }

    #[test]
    fn matches_events() {
        let b = KeyBinding::parse("ctrl+a").unwrap();
        let ev = KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL);
        assert!(b.matches(ev));
        let other = KeyEvent::new(KeyCode::Char('b'), KeyModifiers::CONTROL);
        assert!(!b.matches(other));
        let no_mod = KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE);
        assert!(!b.matches(no_mod));
    }

    #[test]
    fn loads_from_toml_section() {
        let dir = std::env::temp_dir().join(format!(
            "cordanui-keybind-test-{}",
            cordanui_schema::new_id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        std::fs::write(&path, "[keybinds]\nleader = \"ctrl+g\"\nnew_goal = \"m\"\n").unwrap();

        let k = Keybinds::from_toml_path(&path);
        assert_eq!(k.leader.code, KeyCode::Char('g'));
        assert!(k.leader.modifiers.contains(KeyModifiers::CONTROL));
        assert_eq!(k.new_goal.code, KeyCode::Char('m'));
        // Unset entries keep defaults.
        assert_eq!(k.show_details, Keybinds::default().show_details);
    }

    #[test]
    fn falls_back_on_bad_values() {
        let dir = std::env::temp_dir().join(format!(
            "cordanui-keybind-test-{}",
            cordanui_schema::new_id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        std::fs::write(&path, "[keybinds]\nleader = \"@@garbage@@\"\n").unwrap();
        let k = Keybinds::from_toml_path(&path);
        assert_eq!(k.leader, Keybinds::default().leader);
    }
}
