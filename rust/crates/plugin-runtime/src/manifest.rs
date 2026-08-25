//! Plugin manifest types and parsing.
//!
//! The manifest is a `cordanui.toml` file at the root of a plugin repo.
//! It declares the plugin's name, capabilities, build config, and provider
//! settings (if applicable).

use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// The top-level manifest structure.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginManifest {
    pub plugin: PluginMeta,
    /// Plugin runtime kind. `"binary"` (default) = subprocess CLI built
    /// from source per `[build]`. `"lua"` = embedded script (`main.lua`
    /// at the repo root) executed in-process by the host — no build step.
    #[serde(default)]
    pub runtime: Option<String>,
    #[serde(default)]
    pub capabilities: Capabilities,
    #[serde(default)]
    pub provider: Option<ProviderConfig>,
    #[serde(default)]
    pub build: Option<BuildConfig>,
    /// A long-running process the plugin ships (any language). Declared
    /// under `[service]`. The host can spawn/stop it (plugin manager,
    /// `cord.services.*`, `cordanui service` CLI).
    #[serde(default)]
    pub service: Option<ServiceConfig>,
    /// Declarative settings form. Authored as `[[field]]` entries at the
    /// manifest root (flattened into [`UiSpec`] here). The host renders
    /// these fields (plugin manager → Configure), stores values namespaced
    /// under `<plugin.name>.<field.key>`, and injects them back into every
    /// subprocess invocation as the request's `config` object.
    #[serde(default, flatten)]
    pub ui: Option<UiSpec>,
}

/// A declarative settings form: `[[field]]` entries in `cordanui.toml`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UiSpec {
    #[serde(default, rename = "field")]
    pub fields: Vec<UiField>,
}

/// One input the host renders for this plugin.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UiField {
    /// Setting key (namespaced by host to `<plugin>.<key>`).
    pub key: String,
    /// Human label shown in the form.
    #[serde(default)]
    pub label: String,
    /// text | secret | number | bool | select
    #[serde(default = "default_field_type")]
    pub r#type: String,
    #[serde(default)]
    pub required: bool,
    /// Default value (string form; bools/numbers are parsed per type).
    #[serde(default)]
    pub default: Option<String>,
    /// Choices for type = "select".
    #[serde(default)]
    pub options: Vec<String>,
}

fn default_field_type() -> String {
    "text".to_string()
}

impl UiSpec {
    /// Validate a spec: non-duplicate keys, known types, select needs
    /// options, defaults must be members of options. Returns human-readable
    /// problems; empty = valid.
    pub fn validate(&self) -> Vec<String> {
        let mut problems = Vec::new();
        let mut seen = std::collections::HashSet::new();
        if self.fields.is_empty() {
            problems.push("[ui] declared with no [[field]] entries".into());
        }
        for f in &self.fields {
            if f.key.trim().is_empty() {
                problems.push("field with empty key".into());
                continue;
            }
            if !seen.insert(f.key.clone()) {
                problems.push(format!("duplicate field key '{}'", f.key));
            }
            match f.r#type.as_str() {
                "text" | "secret" | "number" | "bool" => {}
                "select" if f.options.is_empty() => {
                    problems.push(format!("field '{}' is select but has no options", f.key));
                }
                "select" => {}
                other => problems.push(format!("field '{}' has unknown type '{other}'", f.key)),
            }
            if let Some(d) = &f.default {
                if f.r#type == "select" && !f.options.contains(d) {
                    problems.push(format!(
                        "field '{}' default '{d}' is not one of its options",
                        f.key
                    ));
                }
                if f.r#type == "bool" && d != "true" && d != "false" {
                    problems.push(format!("field '{}' bool default must be true/false", f.key));
                }
                if f.r#type == "number" && d.parse::<f64>().is_err() {
                    problems.push(format!("field '{}' number default is not numeric", f.key));
                }
            }
            if self.fields.len() > 32 {
                problems.push("more than 32 fields".into());
            }
        }
        problems
    }

    /// The effective starting value of a field: its default or "".
    pub fn initial_value(&self, key: &str) -> String {
        self.fields
            .iter()
            .find(|f| f.key == key)
            .and_then(|f| f.default.clone())
            .unwrap_or_default()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginMeta {
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub description: String,
    /// Mistake guard: `runtime` is a manifest-root key. If it shows up
    /// here, the author put it after the `[plugin]` header and TOML
    /// silently claimed it for this table — `validate()` reports it.
    #[serde(default)]
    pub runtime: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Capabilities {
    #[serde(default)]
    pub provider: bool,
    #[serde(default)]
    pub tool: bool,
    #[serde(default)]
    pub agent: bool,
    #[serde(default)]
    pub theme: bool,
    #[serde(default)]
    pub command: bool,
}

/// Configuration for a provider-capable plugin.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderConfig {
    pub models: Vec<String>,
    #[serde(default)]
    pub api_key_env: Option<String>,
}

/// A long-running process the plugin ships. The binary can be written in
/// any language; the host only knows how to spawn/stop it and where it
/// listens.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceConfig {
    /// Command to spawn, relative to the plugin directory.
    pub cmd: String,
    /// Default arguments; callers may append more.
    #[serde(default)]
    pub args: Vec<String>,
    /// Base URL for `cord.services.request` (e.g. "http://127.0.0.1:8081").
    #[serde(default)]
    pub addr: Option<String>,
    /// Optional readiness probe URL.
    #[serde(default)]
    pub health: Option<String>,
    /// Hint for TUI hosts: start the service when the plugin activates.
    #[serde(default)]
    pub autostart: bool,
}

impl ServiceConfig {
    /// The URL `cord.services.request` should address: `addr` if set,
    /// otherwise the health probe's origin.
    pub fn base_url(&self) -> Option<String> {
        if let Some(addr) = &self.addr {
            return Some(addr.trim_end_matches('/').to_string());
        }
        self.health.as_deref().and_then(|h| {
            let without = h
                .strip_prefix("http://")
                .or_else(|| h.strip_prefix("https://"))?;
            let origin = without.split('/').next()?;
            Some(format!("http://{origin}"))
        })
    }
}

/// How to build the plugin binary.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuildConfig {
    #[serde(default = "default_build_cmd")]
    pub cmd: String,
    #[serde(default)]
    pub bin: Option<String>,
}

fn default_build_cmd() -> String {
    "cargo build --release".to_string()
}

/// Convenience: what kind of plugin is this?
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginCapability {
    Provider,
    Tool,
    Agent,
    Theme,
    Command,
}

impl PluginManifest {
    /// The name of a Lua plugin's entry script (at the plugin repo root).
    pub const LUA_ENTRY: &'static str = "main.lua";

    /// True if this plugin runs on the embedded Lua runtime.
    pub fn is_lua(&self) -> bool {
        self.runtime.as_deref() == Some("lua")
    }

    /// The entry script path for a Lua plugin, relative to its directory.
    pub fn entry_point(&self, plugin_dir: &Path) -> std::path::PathBuf {
        plugin_dir.join(Self::LUA_ENTRY)
    }

    /// Parse a `cordanui.toml` from a plugin directory.
    pub fn from_dir(dir: &Path) -> Result<Self> {
        let manifest_path = dir.join("cordanui.toml");
        let contents = std::fs::read_to_string(&manifest_path)
            .with_context(|| format!("reading manifest at {}", manifest_path.display()))?;
        Self::from_str(&contents)
    }

    /// Parse manifest from a TOML string.
    pub fn from_str(toml_str: &str) -> Result<Self> {
        toml::from_str(toml_str).context("parsing cordanui.toml")
    }

    /// Validate manifest-level invariants. Returns human-readable
    /// problems; empty = valid.
    pub fn validate(&self) -> Vec<String> {
        let mut problems = Vec::new();
        match self.runtime.as_deref() {
            None | Some("binary") | Some("lua") => {}
            Some(other) => problems.push(format!(
                "unknown runtime '{other}' (expected \"binary\" or \"lua\")"
            )),
        }
        if self.plugin.runtime.is_some() {
            problems.push(
                "`runtime` must be at the manifest root (before any [section]), not under [plugin]"
                    .into(),
            );
        }
        if let Some(service) = &self.service {
            if service.cmd.trim().is_empty() {
                problems.push("[service] cmd must not be empty".into());
            }
            for url_field in [&service.addr, &service.health] {
                if let Some(url) = url_field {
                    if !url.starts_with("http://") && !url.starts_with("https://") {
                        problems.push(format!(
                            "[service] url '{url}' must start with http:// or https://"
                        ));
                    }
                }
            }
        }
        if let Some(ui) = &self.ui {
            // An absent/empty form is valid — plugins don't need settings.
            if !ui.fields.is_empty() {
                problems.extend(ui.validate());
            }
        }
        problems
    }

    /// The list of capabilities this plugin declares.
    pub fn capabilities_list(&self) -> Vec<PluginCapability> {
        let mut caps = Vec::new();
        if self.capabilities.provider {
            caps.push(PluginCapability::Provider);
        }
        if self.capabilities.tool {
            caps.push(PluginCapability::Tool);
        }
        if self.capabilities.agent {
            caps.push(PluginCapability::Agent);
        }
        if self.capabilities.theme {
            caps.push(PluginCapability::Theme);
        }
        if self.capabilities.command {
            caps.push(PluginCapability::Command);
        }
        caps
    }

    /// Resolve the binary path for this plugin, relative to its directory.
    /// Falls back to `target/release/<plugin_name>` if `bin` is not set.
    pub fn binary_path(&self, plugin_dir: &Path) -> std::path::PathBuf {
        let bin = self
            .build
            .as_ref()
            .and_then(|b| b.bin.clone())
            .unwrap_or_else(|| format!("target/release/{}", self.plugin.name));
        plugin_dir.join(bin)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_MANIFEST: &str = r#"
[plugin]
name = "provider-claude"
version = "0.1.0"
description = "Anthropic Claude provider"

[capabilities]
provider = true

[provider]
models = ["claude-sonnet-4-5", "claude-opus-4-1"]
api_key_env = "ANTHROPIC_API_KEY"

[build]
cmd = "cargo build --release"
bin = "target/release/provider-claude"
"#;

    #[test]
    fn parse_manifest() {
        let m = PluginManifest::from_str(SAMPLE_MANIFEST).unwrap();
        assert_eq!(m.plugin.name, "provider-claude");
        assert_eq!(m.plugin.version, "0.1.0");
        assert!(m.capabilities.provider);
        assert!(!m.capabilities.tool);

        let provider = m.provider.unwrap();
        assert_eq!(
            provider.models,
            vec!["claude-sonnet-4-5", "claude-opus-4-1"]
        );
        assert_eq!(provider.api_key_env.as_deref(), Some("ANTHROPIC_API_KEY"));

        let build = m.build.unwrap();
        assert_eq!(build.cmd, "cargo build --release");
        assert_eq!(build.bin.as_deref(), Some("target/release/provider-claude"));
    }

    #[test]
    fn parse_minimal_manifest() {
        let toml = r#"
[plugin]
name = "echo"
version = "0.1.0"
"#;
        let m = PluginManifest::from_str(toml).unwrap();
        assert_eq!(m.plugin.name, "echo");
        assert!(!m.capabilities.provider);
        assert!(m.provider.is_none());
        assert!(m.build.is_none());
    }

    #[test]
    fn lua_runtime_manifest() {
        // `runtime` is a manifest-root key — it must come before any
        // [section] header or TOML silently attaches it to [plugin].
        let toml = r#"
runtime = "lua"

[plugin]
name = "provider-zen"
version = "0.1.0"

[capabilities]
provider = true
"#;
        let m = PluginManifest::from_str(toml).unwrap();
        assert!(m.is_lua());
        assert!(m.build.is_none()); // no build step for script plugins
        assert!(m.validate().is_empty());

        let dir = Path::new("/plugins/provider-zen");
        assert_eq!(
            m.entry_point(dir),
            Path::new("/plugins/provider-zen/main.lua")
        );

        // unknown runtime is rejected by validate()
        let bad: PluginManifest = toml::from_str(
            r#"runtime = "wasm"
[plugin]
name = "x"
version = "0.1"
"#,
        )
        .unwrap();
        assert_eq!(
            bad.validate(),
            vec!["unknown runtime 'wasm' (expected \"binary\" or \"lua\")"]
        );
    }

    #[test]
    fn service_manifest_parses_and_validates() {
        let toml = r#"
[plugin]
name = "cordanui-agents"
version = "0.1.0"

[service]
cmd = "./target/release/cordanui-agents"
args = ["--port", "8081"]
addr = "http://127.0.0.1:8081"
health = "http://127.0.0.1:8081/health"
autostart = true
"#;
        let m = PluginManifest::from_str(toml).unwrap();
        let service = m.service.clone().expect("service present");
        assert_eq!(service.cmd, "./target/release/cordanui-agents");
        assert_eq!(service.args, vec!["--port", "8081"]);
        assert!(service.autostart);
        assert_eq!(service.base_url().as_deref(), Some("http://127.0.0.1:8081"));
        assert!(m.validate().is_empty());

        // health-only: base_url derives the origin
        let m2 = PluginManifest::from_str(
            "[plugin]\nname = \"x\"\nversion = \"0.1\"\n\n[service]\ncmd = \"./srv\"\nhealth = \"http://1.2.3.4:9/api/health\"\n",
        )
        .unwrap();
        assert_eq!(
            m2.service.unwrap().base_url().as_deref(),
            Some("http://1.2.3.4:9")
        );

        // bad url is caught
        let bad: PluginManifest = toml::from_str(
            "[plugin]\nname = \"x\"\nversion = \"0.1\"\n\n[service]\ncmd = \"./srv\"\naddr = \"ftp://nope\"\n",
        )
        .unwrap();
        assert_eq!(bad.validate().len(), 1);
    }

    #[test]
    fn runtime_under_plugin_section_is_caught() {
        // Common authoring mistake: runtime after the [plugin] header.
        let m: PluginManifest = toml::from_str(
            r#"[plugin]
name = "x"
version = "0.1"
runtime = "lua"
"#,
        )
        .unwrap();
        assert!(!m.is_lua());
        assert_eq!(
            m.validate(),
            vec![
                "`runtime` must be at the manifest root (before any [section]), not under [plugin]"
            ]
        );
    }

    #[test]
    fn binary_path_default() {
        let m = PluginManifest::from_str(
            r#"
[plugin]
name = "provider-claude"
version = "0.1.0"
"#,
        )
        .unwrap();
        let dir = Path::new("/home/user/.local/share/cordanui/plugins/provider-claude");
        let bin = m.binary_path(dir);
        assert_eq!(
            bin,
            Path::new("/home/user/.local/share/cordanui/plugins/provider-claude/target/release/provider-claude")
        );
    }

    const UI_MANIFEST: &str = r#"
[plugin]
name = "provider-x"
version = "0.1.0"

[[field]]
key = "api_key"
label = "API Key"
type = "secret"
required = true

[[field]]
key = "base_url"
type = "text"
default = "https://example.com/v1"

[[field]]
key = "model"
type = "select"
options = ["a", "b"]
default = "a"

[[field]]
key = "stream"
type = "bool"
default = "true"
"#;

    #[test]
    fn parse_ui_spec() {
        let m = PluginManifest::from_str(UI_MANIFEST).unwrap();
        let ui = m.ui.expect("ui section present");
        assert_eq!(ui.fields.len(), 4);
        assert!(ui.validate().is_empty());
        assert_eq!(ui.fields[0].r#type, "secret");
        assert!(ui.fields[0].required);
        assert_eq!(ui.initial_value("base_url"), "https://example.com/v1");
        assert_eq!(ui.initial_value("api_key"), "");
    }

    #[test]
    fn ui_validation_catches_problems() {
        let bad: PluginManifest = toml::from_str(
            r#"
[plugin]
name = "x"
version = "0.1"

[[field]]
key = "dup"
type = "text"

[[field]]
key = "dup"
type = "wat"

[[field]]
key = "sel"
type = "select"
"#,
        )
        .unwrap();
        let ui = bad.ui.unwrap();
        let problems = ui.validate();
        assert_eq!(problems.len(), 3); // duplicate, unknown type, select w/o options
    }
}
