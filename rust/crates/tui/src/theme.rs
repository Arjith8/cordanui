//! Theme resolution for the TUI.
//!
//! Themes live in the shared `themes` + `settings` tables (see
//! `agent-docs/theme-system-spec.md` — same contract as the mobile client).
//! Resolution order:
//!
//! 1. `settings.theme_mode = 'explicit'` → the row matching
//!    `settings.selected_theme_id`.
//! 2. Otherwise (`'system'`, default) → the `builtin-dark` theme. Terminals
//!    are predominantly dark and the TUI cannot query the OS scheme, so the
//!    "system" slot resolves to dark here.
//!
//! `colors_json` may contain any subset of the canonical tokens; missing
//! tokens fall back to the builtin dark palette. Any DB error (missing
//! table on an un-migrated database, corrupt JSON, …) degrades gracefully
//! to builtin dark — theming must never prevent startup.

use cordanui_sync::{Database, Value};
use ratatui::style::Color;

/// The canonical styling tokens. Field names match the mobile client's
/// `ThemeColors` (snake_cased) and double as `colors_json` keys.
#[derive(Debug, Clone)]
pub struct ThemeColors {
    pub bg: Color,
    pub surface: Color,
    pub border: Color,
    pub tree_line: Color,
    pub text: Color,
    pub text_dim: Color,
    pub text_faint: Color,
    pub accent: Color,
    pub on_accent: Color,
    pub danger: Color,
    pub status_pending: Color,
    pub status_wip: Color,
    pub status_done: Color,
    pub status_agent: Color,
}

/// Builtin palettes — kept in lockstep with the mobile app's
/// `src/theme/types.ts`.
fn builtin_dark() -> ThemeColors {
    ThemeColors {
        bg: rgb(0x0f172a),
        surface: rgb(0x1e293b),
        border: rgb(0x1f2937),
        tree_line: rgb(0x334155),
        text: rgb(0xf9fafb),
        text_dim: rgb(0x9ca3af),
        text_faint: rgb(0x6b7280),
        accent: rgb(0x3b82f6),
        on_accent: rgb(0xffffff),
        danger: rgb(0xef4444),
        status_pending: rgb(0x9ca3af),
        status_wip: rgb(0x3b82f6),
        status_done: rgb(0x22c55e),
        status_agent: rgb(0xa855f7),
    }
}

fn builtin_light() -> ThemeColors {
    ThemeColors {
        bg: rgb(0xf8fafc),
        surface: rgb(0xffffff),
        border: rgb(0xe2e8f0),
        tree_line: rgb(0xcbd5e1),
        text: rgb(0x0f172a),
        text_dim: rgb(0x475569),
        text_faint: rgb(0x94a3b8),
        accent: rgb(0x2563eb),
        on_accent: rgb(0xffffff),
        danger: rgb(0xdc2626),
        status_pending: rgb(0x64748b),
        status_wip: rgb(0x2563eb),
        status_done: rgb(0x16a34a),
        status_agent: rgb(0x9333ea),
    }
}

const BUILTIN_DARK_ID: &str = "builtin-dark";
const BUILTIN_LIGHT_ID: &str = "builtin-light";

const SEED_BUILTINS_SQL: &str = "INSERT OR IGNORE INTO themes \
     (id, name, source, colors_json) VALUES (?, ?, 'builtin', ?)";

// Full token maps so rows are interchangeable with the mobile client's
// seeds (same keys as ThemeColors camelCased).
const DARK_COLORS_JSON: &str = "{\"bg\":\"#0f172a\",\"surface\":\"#1e293b\",\"border\":\"#1f2937\",\
    \"treeLine\":\"#334155\",\"text\":\"#f9fafb\",\"textDim\":\"#9ca3af\",\"textFaint\":\"#6b7280\",\
    \"accent\":\"#3b82f6\",\"onAccent\":\"#ffffff\",\"danger\":\"#ef4444\",\"statusPending\":\"#9ca3af\",\
    \"statusWip\":\"#3b82f6\",\"statusDone\":\"#22c55e\",\"statusAgent\":\"#a855f7\"}";

const LIGHT_COLORS_JSON: &str = "{\"bg\":\"#f8fafc\",\"surface\":\"#ffffff\",\"border\":\"#e2e8f0\",\
    \"treeLine\":\"#cbd5e1\",\"text\":\"#0f172a\",\"textDim\":\"#475569\",\"textFaint\":\"#94a3b8\",\
    \"accent\":\"#2563eb\",\"onAccent\":\"#ffffff\",\"danger\":\"#dc2626\",\"statusPending\":\"#64748b\",\
    \"statusWip\":\"#2563eb\",\"statusDone\":\"#16a34a\",\"statusAgent\":\"#9333ea\"}";

/// A resolved theme ready for rendering.
#[derive(Debug, Clone)]
pub struct Theme {
    pub name: String,
    pub colors: ThemeColors,
}

impl Theme {
    /// The palette used when nothing else is available.
    pub fn builtin_dark() -> Theme {
        Theme { name: "Cordanui Dark".into(), colors: builtin_dark() }
    }

    /// Load the active theme from the DB, seeding builtins if needed.
    /// Never fails: any error falls back to builtin dark.
    pub fn load(db: &Database) -> Theme {
        Self::load_inner(db).unwrap_or_else(|_| Self::builtin_dark())
    }

    fn load_inner(db: &Database) -> anyhow::Result<Theme> {
        // Seed builtins (idempotent).
        db.execute(
            SEED_BUILTINS_SQL,
            vec![
                Value::from(BUILTIN_DARK_ID),
                Value::from("Cordanui Dark"),
                Value::from(DARK_COLORS_JSON),
            ],
        )?;
        db.execute(
            SEED_BUILTINS_SQL,
            vec![
                Value::from(BUILTIN_LIGHT_ID),
                Value::from("Cordanui Light"),
                Value::from(LIGHT_COLORS_JSON),
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

        // Resolve which row is active.
        let explicit_row = if mode == "explicit" {
            match selected_id.as_deref() {
                Some(id) => db.query_first(
                    "SELECT name, colors_json FROM themes WHERE id = ?",
                    vec![Value::from(id)],
                )?,
                None => None,
            }
        } else {
            None
        };

        match explicit_row {
            Some(row) => {
                let name = value_text(row.first());
                let colors_json =
                    row.get(1).map(|v| value_text(Some(&v))).unwrap_or_default();
                Ok(Theme { name, colors: merge_colors(&colors_json)? })
            }
            None => {
                // System mode: terminals have no OS scheme; resolve to dark.
                let row = db.query_first(
                    "SELECT name, colors_json FROM themes WHERE id = ?",
                    vec![Value::from(BUILTIN_DARK_ID)],
                )?;
                let name = row
                    .as_ref()
                    .map(|r| value_text(r.first()))
                    .unwrap_or_else(|| "Cordanui Dark".into());
                let colors_json = row
                    .as_ref()
                    .and_then(|r| r.get(1))
                    .map(|v| value_text(Some(v)))
                    .unwrap_or_default();
                Ok(Theme { name, colors: merge_colors(&colors_json)? })
            }
        }
    }
}

/// Overlay a partial token map (JSON object of hex strings) onto the dark
/// defaults. Unknown keys are ignored.
fn merge_colors(colors_json: &str) -> anyhow::Result<ThemeColors> {
    let mut colors = builtin_dark();
    let map: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str(colors_json).unwrap_or_default();

    let pick = |key: &str| -> Option<Color> {
        map.get(key)
            .and_then(|v| v.as_str())
            .and_then(parse_hex)
    };

    if let Some(c) = pick("bg") { colors.bg = c; }
    if let Some(c) = pick("surface") { colors.surface = c; }
    if let Some(c) = pick("border") { colors.border = c; }
    if let Some(c) = pick("treeLine") { colors.tree_line = c; }
    if let Some(c) = pick("text") { colors.text = c; }
    if let Some(c) = pick("textDim") { colors.text_dim = c; }
    if let Some(c) = pick("textFaint") { colors.text_faint = c; }
    if let Some(c) = pick("accent") { colors.accent = c; }
    if let Some(c) = pick("onAccent") { colors.on_accent = c; }
    if let Some(c) = pick("danger") { colors.danger = c; }
    if let Some(c) = pick("statusPending") { colors.status_pending = c; }
    if let Some(c) = pick("statusWip") { colors.status_wip = c; }
    if let Some(c) = pick("statusDone") { colors.status_done = c; }
    if let Some(c) = pick("statusAgent") { colors.status_agent = c; }

    Ok(colors)
}

/// Parse `#rgb` / `#rrggbb` into a ratatui RGB color. Returns `None` for
/// anything else so bad values fall through to defaults.
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
