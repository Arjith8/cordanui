//! cordanui agents backend — binary entry point.

use cordanui_agents::Config;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let config = Config::from_env()?;

    tracing::info!(
        port = config.port,
        plugin_dir = %config.plugin_dir.display(),
        provider_plugin = %config.provider_plugin,
        provider_model = ?config.provider_model,
        auth_enabled = config.auth_token.is_some(),
        "cordanui agents backend starting"
    );

    cordanui_agents::serve(config).await
}
