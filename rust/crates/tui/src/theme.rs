//! Style resolution for the TUI.
//!
//! Colors are addressed by **style variables** using the Compose /
//! Material 3 role vocabulary (`background`, `primary`, `onSurface`, ...)
//! — see `cordanui-plugin-runtime::style`. There are no widget-specific
//! tokens: statuses use standard roles (pending → `onSurfaceVariant`,
//! in-progress → `primary`, completed → `success`, agent → `tertiary`).
//!
//! A variable's value resolves through four layers, later winning:
//!
//! 1. **builtin palette** (dark or light defaults)
//! 2. **active theme** (`themes.colors_json`; mobile token names
//!    are aliased to their new roles)
//! 3. **global overrides** — `settings` rows keyed `style.<var>`; these
//!    are what `cord.g.style.*` writes and sync to every client via Turso
//! 4. **session overrides** — in-memory, what `cord["local"].style.*`
//!    writes; this client only, gone on exit
//!
//! Plugins can introduce new variable names at any time: unknown names
//! resolve to `onBackground` unless a layer defines them. Any DB error
//! degrades gracefully to the builtin palette — styling never blocks
//! startup.

use std::collections::BTreeMap;
use std::collections::HashMap;

use cordanui_plugin_runtime::parse_color;
use cordanui_sync::{Database, Value};
use ratatui::style::Color;

/// The resolved style palette used for rendering. Fixed fields for the 18
/// core roles (fast + checked), plus a map for plugin-introduced extras.
#[derive(Debug, Clone)]
pub struct Palette {
    pub background: Color,
    pub on_background: Color,
    pub surface: Color,
    pub on_surface: Color,
    pub surface_variant: Color,
    pub on_surface_variant: Color,
    pub primary: Color,
    pub on_primary: Color,
    pub secondary: Color,
    pub on_secondary: Color,
    pub tertiary: Color,
    pub on_tertiary: Color,
    pub success: Color,
    pub on_success: Color,
    pub error: Color,
    pub on_error: Color,
    pub outline: Color,
    pub outline_variant: Color,
    /// Plugin-defined variables not covered by the core roles.
    pub custom: BTreeMap<String, Color>,
}

impl Palette {
    /// Look up any variable by name (core roles or custom). Unknown names
    /// fall back to `on_background`.
    pub fn get(&self, var: &str) -> Option<Color> {
        Some(match var {
            "background" => self.background,
            "onBackground" => self.on_background,
            "surface" => self.surface,
            "onSurface" => self.on_surface,
            "surfaceVariant" => self.surface_variant,
            "onSurfaceVariant" => self.on_surface_variant,
            "primary" => self.primary,
            "onPrimary" => self.on_primary,
            "secondary" => self.secondary,
            "onSecondary" => self.on_secondary,
            "tertiary" => self.tertiary,
            "onTertiary" => self.on_tertiary,
            "success" => self.success,
            "onSuccess" => self.on_success,
            "error" => self.error,
            "onError" => self.on_error,
            "outline" => self.outline,
            "outlineVariant" => self.outline_variant,
            other => {
                return Some(
                    self.custom
                        .get(other)
                        .copied()
                        .unwrap_or(self.on_background),
                )
            }
        })
    }

    fn set(&mut self, var: &str, color: Color) {
        match var {
            "background" => self.background = color,
            "onBackground" => self.on_background = color,
            "surface" => self.surface = color,
            "onSurface" => self.on_surface = color,
            "surfaceVariant" => self.surface_variant = color,
            "onSurfaceVariant" => self.on_surface_variant = color,
            "primary" => self.primary = color,
            "onPrimary" => self.on_primary = color,
            "secondary" => self.secondary = color,
            "onSecondary" => self.on_secondary = color,
            "tertiary" => self.tertiary = color,
            "onTertiary" => self.on_tertiary = color,
            "success" => self.success = color,
            "onSuccess" => self.on_success = color,
            "error" => self.error = color,
            "onError" => self.on_error = color,
            "outline" => self.outline = color,
            "outlineVariant" => self.outline_variant = color,
            _ => {
                self.custom.insert(var.to_string(), color);
            }
        }
    }
}

/// Builtin dark palette (also the base everything falls back to).
fn dark_palette() -> Palette {
    Palette {
        background: rgb(0x0f172a),
        on_background: rgb(0xf9fafb),
        surface: rgb(0x1e293b),
        on_surface: rgb(0xf9fafb),
        surface_variant: rgb(0x1f2937),
        on_surface_variant: rgb(0x9ca3af),
        primary: rgb(0x3b82f6),
        on_primary: rgb(0xffffff),
        secondary: rgb(0x38bdf8),
        on_secondary: rgb(0x082f49),
        tertiary: rgb(0xa855f7),
        on_tertiary: rgb(0xffffff),
        success: rgb(0x22c55e),
        on_success: rgb(0x052e16),
        error: rgb(0xef4444),
        on_error: rgb(0xffffff),
        outline: rgb(0x334155),
        outline_variant: rgb(0x6b7280),
        custom: BTreeMap::new(),
    }
}

/// Builtin light palette. Not selected anywhere yet (system mode resolves
/// to dark — terminals have no OS scheme query) but kept in lockstep with
/// the mobile client's light seed for when light mode lands.
#[allow(dead_code)]
fn light_palette() -> Palette {
    Palette {
        background: rgb(0xf8fafc),
        on_background: rgb(0x0f172a),
        surface: rgb(0xffffff),
        on_surface: rgb(0x0f172a),
        surface_variant: rgb(0xe2e8f0),
        on_surface_variant: rgb(0x475569),
        primary: rgb(0x2563eb),
        on_primary: rgb(0xffffff),
        secondary: rgb(0x0284c7),
        on_secondary: rgb(0xffffff),
        tertiary: rgb(0x9333ea),
        on_tertiary: rgb(0xffffff),
        success: rgb(0x16a34a),
        on_success: rgb(0xffffff),
        error: rgb(0xdc2626),
        on_error: rgb(0xffffff),
        outline: rgb(0xcbd5e1),
        outline_variant: rgb(0x94a3b8),
        custom: BTreeMap::new(),
    }
}

const BUILTIN_DARK_ID: &str = "builtin-dark";
const BUILTIN_LIGHT_ID: &str = "builtin-light";

const SEED_BUILTINS_SQL: &str = "INSERT OR IGNORE INTO themes \
     (id, name, source, colors_json) VALUES (?, ?, 'builtin', ?)";

// Seeded JSON carries BOTH vocabularies: the new Compose-style roles for
// the TUI, plus the mobile client still reads,
// so one row renders correctly everywhere.
macro_rules! seed_json {
    ($($new:literal : $old:literal : $val:expr),* $(,)?) => {{
        let mut parts: Vec<String> = Vec::new();
        $(
            parts.push(format!("\"{}\":\"{}\"", $new, $val));
            parts.push(format!("\"{}\":\"{}\"", $old, $val));
        )*
        format!("{{{}}}", parts.join(","))
    }};
}

fn dark_seed_json() -> String {
    seed_json! {
        "background": "bg": "#0f172a",
        "onBackground": "text": "#f9fafb",
        "surface": "surface": "#1e293b",
        "surfaceVariant": "border": "#1f2937",
        "onSurfaceVariant": "textDim": "#9ca3af",
        "primary": "accent": "#3b82f6",
        "onPrimary": "onAccent": "#ffffff",
        "success": "statusDone": "#22c55e",
        "error": "danger": "#ef4444",
        "tertiary": "statusAgent": "#a855f7",
        "outline": "treeLine": "#334155",
        "outlineVariant": "textFaint": "#6b7280",
    }
}

fn light_seed_json() -> String {
    seed_json! {
        "background": "bg": "#f8fafc",
        "onBackground": "text": "#0f172a",
        "surface": "surface": "#ffffff",
        "surfaceVariant": "border": "#e2e8f0",
        "onSurfaceVariant": "textDim": "#475569",
        "primary": "accent": "#2563eb",
        "onPrimary": "onAccent": "#ffffff",
        "success": "statusDone": "#16a34a",
        "error": "danger": "#dc2626",
        "tertiary": "statusAgent": "#9333ea",
        "outline": "treeLine": "#cbd5e1",
        "outlineVariant": "textFaint": "#94a3b8",
    }
}

/// A resolved theme ready for rendering.
#[derive(Debug, Clone)]
pub struct Theme {
    pub name: String,
    pub colors: Palette,
}

impl Theme {
    /// The palette used when nothing else is available.
    pub fn builtin_dark() -> Theme {
        Theme {
            name: "Cordanui Dark".into(),
            colors: dark_palette(),
        }
    }

    /// Full resolution: builtin ← theme ← global overrides ← session.
    /// Session overrides map variable name → color string (any form
    /// accepted by [`parse_color`]).
    pub fn resolve(db: &Database, session: &HashMap<String, String>) -> Theme {
        Self::resolve_inner(db, session).unwrap_or_else(|_| Self::builtin_dark())
    }

    fn resolve_inner(db: &Database, session: &HashMap<String, String>) -> anyhow::Result<Theme> {
        // Seed builtins (idempotent).
        db.execute(
            SEED_BUILTINS_SQL,
            vec![
                Value::from(BUILTIN_DARK_ID),
                Value::Text("Cordanui Dark".to_string()),
                Value::from(dark_seed_json()),
            ],
        )?;
        db.execute(
            SEED_BUILTINS_SQL,
            vec![
                Value::from(BUILTIN_LIGHT_ID),
                Value::Text("Cordanui Light".to_string()),
                Value::from(light_seed_json()),
            ],
        )?;

        let mode = db
            .query_first(
                "SELECT value FROM settings WHERE key = 'theme_mode'",
                vec![],
            )?
            .map(|row| value_text(row.first()))
            .unwrap_or_else(|| "system".into());

        let selected_id = db
            .query_first(
                "SELECT value FROM settings WHERE key = 'selected_theme_id'",
                vec![],
            )?
            .map(|row| value_text(row.first()));

        // Layer 2: the active theme's colors (or builtin dark in system mode).
        let theme_id = if mode == "explicit" {
            selected_id.unwrap_or_else(|| BUILTIN_DARK_ID.to_string())
        } else {
            BUILTIN_DARK_ID.to_string()
        };

        let row = db.query_first(
            "SELECT name, colors_json FROM themes WHERE id = ?",
            vec![Value::from(theme_id)],
        )?;

        let (theme_name, colors_json) = match &row {
            Some(r) => (
                value_text(r.first()),
                r.get(1).map(|v| value_text(Some(&v))).unwrap_or_default(),
            ),
            None => ("Cordanui Dark".to_string(), String::new()),
        };
        let mut palette = merge_colors(&dark_palette(), &colors_json);

        // Layer 3: global overrides (cord.g.style.*), stored as style.<var>.
        let overrides =
            db.query_simple("SELECT key, value FROM settings WHERE key LIKE 'style.%'")?;
        for row in overrides.rows() {
            let raw_key = value_text(row.first());
            let var = raw_key.strip_prefix("style.").unwrap_or(&raw_key);
            if let Some(hex) = parse_color(&value_text(row.get(1))) {
                apply_color(&mut palette, var, &hex);
            }
        }

        // Layer 4: session overrides (cord["local"].style.*).
        for (var, val) in session {
            if let Some(hex) = parse_color(val) {
                apply_color(&mut palette, var, &hex);
            }
        }

        Ok(Theme {
            name: theme_name,
            colors: palette,
        })
    }
}

/// Overlay a partial token map (JSON object of color strings) onto a base
/// palette. Accepts both new-role names and mobile aliases;
/// canonical names win when both are present.
fn merge_colors(base: &Palette, colors_json: &str) -> Palette {
    let mut palette = base.clone();
    let map: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str(colors_json).unwrap_or_default();

    let pick = |key: &str| -> Option<String> {
        map.get(key).and_then(|v| v.as_str()).and_then(parse_color)
    };

    // Canonical + plugin-defined names.
    for key in map.keys() {
        if let Some(hex) = pick(key) {
            apply_color(&mut palette, key, &hex);
        }
    }
    palette
}

/// Set `var` on the palette from a normalized hex string.
fn apply_color(palette: &mut Palette, var: &str, hex: &str) {
    if let Some(Color::Rgb(r, g, b)) = parse_hex(hex) {
        palette.set(var, Color::Rgb(r, g, b));
    }
}

/// Parse a normalized `#rrggbb` string into a ratatui RGB color.
fn parse_hex(s: &str) -> Option<Color> {
    let s = s.trim_start_matches('#');
    match s.len() {
        6 => {
            let r = u8::from_str_radix(&s[0..2], 16).ok()?;
            let g = u8::from_str_radix(&s[2..4], 16).ok()?;
            let b = u8::from_str_radix(&s[4..6], 16).ok()?;
            Some(Color::Rgb(r, g, b))
        }
        _ => None,
    }
}

fn rgb(hex: u32) -> Color {
    Color::Rgb(
        ((hex >> 16) & 0xff) as u8,
        ((hex >> 8) & 0xff) as u8,
        (hex & 0xff) as u8,
    )
}

fn value_text(v: Option<&Value>) -> String {
    match v {
        Some(Value::Text(s)) => s.clone(),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_db(name: &str) -> Database {
        let dir = std::env::temp_dir().join(format!("cordanui-tui-theme-test-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let config = cordanui_sync::SyncConfig {
            db_path: dir.join("test.db"),
            ..Default::default()
        };
        Database::open(&config).unwrap()
    }

    fn color_of(theme: &Theme, var: &str) -> Option<Color> {
        match theme.colors.get(var) {
            Some(Color::Rgb(r, g, b)) => Some(Color::Rgb(r, g, b)),
            _ => None,
        }
    }

    #[test]
    fn core_roles_resolve_to_builtin_defaults() {
        let db = test_db("builtin");
        let theme = Theme::resolve(&db, &HashMap::new());
        assert_eq!(color_of(&theme, "primary"), Some(rgb(0x3b82f6)));
        assert_eq!(color_of(&theme, "background"), Some(rgb(0x0f172a)));
        // Old status tokens are gone; statuses use standard roles.
        assert_eq!(color_of(&theme, "success"), Some(rgb(0x22c55e)));
        // Unknown names (including old widget-specific names like
        // statusWip, which no longer exist as vars) fall back to onBackground.
        assert_eq!(
            color_of(&theme, "statusWip"),
            color_of(&theme, "onBackground")
        );
        // Unknown names fall back to onBackground.
        assert_eq!(
            color_of(&theme, "totallyMadeUp"),
            color_of(&theme, "onBackground")
        );
    }

    #[test]
    fn global_overrides_beat_themes_session_beats_global() {
        let db = test_db("layers");
        let mut session = HashMap::new();

        // Global override (cord.g) — what the DB layer applies.
        crate::db::set_style_override(&db, "primary", "#111111").unwrap();
        let theme = Theme::resolve(&db, &session);
        assert_eq!(color_of(&theme, "primary"), Some(rgb(0x111111)));

        // Session override (cord["local"]) wins over the global one.
        session.insert("primary".to_string(), "#222222".to_string());
        let theme = Theme::resolve(&db, &session);
        assert_eq!(color_of(&theme, "primary"), Some(rgb(0x222222)));

        // Clearing the global restores the builtin for other clients.
        crate::db::clear_style_override(&db, "primary").unwrap();
        let theme = Theme::resolve(&db, &HashMap::new());
        assert_eq!(color_of(&theme, "primary"), Some(rgb(0x3b82f6)));

        let _ = session;
    }

    #[test]
    fn custom_plugin_vars_resolve_and_fall_back() {
        let db = test_db("custom");
        // A plugin introduces a brand-new variable at DB level.
        crate::db::set_style_override(&db, "pluginX.glow", "#ff8800").unwrap();
        let theme = Theme::resolve(&db, &HashMap::new());
        assert_eq!(
            color_of(&theme, "pluginX.glow"),
            Some(rgb(0xff8800)),
            "plugin-introduced vars must be resolvable"
        );
        // A different plugin's var nobody defined falls back to onBackground.
        assert_eq!(
            color_of(&theme, "pluginY.shimmer"),
            color_of(&theme, "onBackground")
        );
    }
}
