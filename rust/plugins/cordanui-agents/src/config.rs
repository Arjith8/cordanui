//! Configuration for the agent backend. All settings come from environment
//! variables, with sensible defaults.

use std::path::PathBuf;

use anyhow::Result;

#[derive(Debug, Clone)]
pub struct Config {
    /// Port to listen on.
    pub port: u16,
    /// Shared secret for auth. If set, requests must include
    /// `Authorization: Bearer <token>`.
    pub auth_token: Option<String>,
    /// Directory where installed plugins live.
    pub plugin_dir: PathBuf,
    /// Which provider plugin to use.
    pub provider_plugin: String,
    /// Which model to use (passed to the plugin).
    pub provider_model: Option<String>,
    /// Database path. If None, uses the default local path.
    pub db_path: Option<PathBuf>,
}

impl Config {
    /// Load configuration from environment variables.
    pub fn from_env() -> Result<Self> {
        let port = std::env::var("CORDANUI_PORT")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(3737);

        let auth_token = std::env::var("CORDANUI_AUTH_TOKEN").ok().filter(|s| !s.is_empty());

        let plugin_dir = std::env::var("CORDANUI_PLUGIN_DIR")
            .ok()
            .map(PathBuf::from)
            .unwrap_or_else(default_plugin_dir);

        let provider_plugin = std::env::var("CORDANUI_PROVIDER_PLUGIN")
            .unwrap_or_else(|_| "provider-claude".to_string());

        let provider_model = std::env::var("CORDANUI_PROVIDER_MODEL")
            .ok()
            .filter(|s| !s.is_empty());

        let db_path = std::env::var("CORDANUI_DB_PATH")
            .ok()
            .map(PathBuf::from);

        Ok(Self {
            port,
            auth_token,
            plugin_dir,
            provider_plugin,
            provider_model,
            db_path,
        })
    }
}

fn default_plugin_dir() -> PathBuf {
    let base = dirs::data_dir()
        .or_else(|| dirs::home_dir().map(|h| h.join(".local").join("share")))
        .expect("cannot determine data directory");
    base.join("cordanui").join("plugins")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_defaults() {
        let dir = default_plugin_dir();
        assert!(dir.to_string_lossy().contains("cordanui"));
        assert!(dir.to_string_lossy().contains("plugins"));
    }
}
