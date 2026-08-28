//! Style variables — the shared vocabulary between the host UI, themes,
//! and plugins.
//!
//! Variable names follow the Compose/Material 3 role convention
//! (`background`, `onBackground`, `primary`, ...) so anyone who has used
//! Compose, Flutter, or CSS design tokens already knows them. There are no
//! widget-specific tokens like `statusWip`: statuses simply use the
//! standard roles (pending → `onSurfaceVariant`, in-progress → `primary`,
//! completed → `success`, agent mode → `tertiary`).
//!
//! Plugins may introduce new variables at runtime; anything not covered by
//! the active theme falls back to the plugin-declared default or
//! `onBackground`.

use std::sync::Arc;

/// The canonical style variables. These are always defined — every one has
/// a builtin default in both dark and light palettes.
pub const CORE_VARS: &[&str] = &[
    "background",
    "onBackground",
    "surface",
    "onSurface",
    "surfaceVariant",
    "onSurfaceVariant",
    "primary",
    "onPrimary",
    "secondary",
    "onSecondary",
    "tertiary",
    "onTertiary",
    "success",
    "onSuccess",
    "error",
    "onError",
    "outline",
    "outlineVariant",
];

/// Parse a color string into normalized `#rrggbb`.
///
/// Accepted forms: `#rgb`, `#rrggbb`, `rgb(r, g, b)`, `rgba(r, g, b, a)`
/// (alpha is accepted but dropped — terminals have no alpha channel).
/// A leading `#` is optional in hex forms.
pub fn parse_color(s: &str) -> Option<String> {
    let s = s.trim();
    let lowered = s.to_ascii_lowercase();

    if let Some(inner) = lowered
        .strip_prefix("rgba(")
        .and_then(|r| r.strip_suffix(')'))
        .or_else(|| {
            lowered
                .strip_prefix("rgb(")
                .and_then(|r| r.strip_suffix(')'))
        })
    {
        let parts: Vec<&str> = inner.split(',').map(str::trim).collect();
        if parts.len() < 3 {
            return None;
        }
        let mut out = String::from("#");
        for p in &parts[..3] {
            let v: u8 = p.parse().ok()?;
            out.push_str(&format!("{v:02x}"));
        }
        return Some(out);
    }

    let hex = s.trim_start_matches('#');
    match hex.len() {
        3 => {
            if !hex.chars().all(|c| c.is_ascii_hexdigit()) {
                return None;
            }
            let expanded: String = hex
                .chars()
                .flat_map(|c| [c.to_ascii_lowercase(); 2])
                .collect();
            Some(format!("#{expanded}"))
        }
        6 => {
            if !hex.chars().all(|c| c.is_ascii_hexdigit()) {
                return None;
            }
            Some(format!("#{}", hex.to_ascii_lowercase()))
        }
        _ => None,
    }
}

/// Host-side style storage that the embedded Lua runtime talks to when a
/// plugin calls `cord.g.style.*` / `cord.local.style.*`.
///
/// - **persistent** (`.g`) — stored at the database layer, synced to all
///   clients via Turso.
/// - **session** (`.local`) — held in memory by this client only.
pub trait StyleHost: Send + Sync {
    fn set(&self, persistent: bool, var: &str, hex: &str);
    fn clear(&self, persistent: bool, var: &str);
    fn clear_all(&self, persistent: bool);
    /// The currently effective value of a variable, if overridden.
    fn resolved(&self, var: &str) -> Option<String>;
}

/// Convenience alias so hosts can share one store across runtimes.
pub type SharedStyleHost = Arc<dyn StyleHost>;

/// A no-op store for hosts/tests without styling wired up.
#[derive(Debug, Default, Clone, Copy)]
pub struct NullStyleHost;

impl StyleHost for NullStyleHost {
    fn set(&self, _: bool, _: &str, _: &str) {}
    fn clear(&self, _: bool, _: &str) {}
    fn clear_all(&self, _: bool) {}
    fn resolved(&self, _: &str) -> Option<String> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_hex_forms() {
        assert_eq!(parse_color("#ff8800").as_deref(), Some("#ff8800"));
        assert_eq!(parse_color("FF8800").as_deref(), Some("#ff8800"));
        assert_eq!(parse_color("#f80").as_deref(), Some("#ff8800"));
        assert_eq!(parse_color("nope"), None);
        assert_eq!(parse_color("#12345"), None);
    }

    #[test]
    fn parses_functional_forms() {
        assert_eq!(parse_color("rgb(255, 136, 0)").as_deref(), Some("#ff8800"));
        assert_eq!(
            parse_color("rgba(255,136,0,0.5)").as_deref(),
            Some("#ff8800")
        );
        assert_eq!(parse_color("rgb(300,0,0)"), None);
    }
}
