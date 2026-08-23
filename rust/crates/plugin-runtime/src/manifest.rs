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
    #[serde(default)]
    pub capabilities: Capabilities,
    #[serde(default)]
    pub provider: Option<ProviderConfig>,
    #[serde(default)]
    pub build: Option<BuildConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginMeta {
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub description: String,
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
        assert_eq!(provider.models, vec!["claude-sonnet-4-5", "claude-opus-4-1"]);
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
}
