//! Agent runner — resolves the provider plugin for a queued task, invokes
//! it (binary subprocess or in-process Lua), streams progress back to the
//! shared DB, and writes the final result.
//!
//! The runner supports both plugin runtimes:
//! - **Binary plugins**: spawned via `cordanui_plugin_runtime::spawn::run_streaming`.
//! - **Lua plugins**: loaded in-process via `cordanui_plugin_runtime::LuaPlugin`.
//!
//! Provider selection: the runner reads the goal's `metadata` JSON for a
//! `provider` and `model` field (written by the TUI or mobile when the
//! user picks a provider). If absent, it uses the first active provider
//! plugin's first model.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use cordanui_plugin_runtime::{
    AgentEvent, AgentRunConfig, HostHooks, LuaPlugin, PluginManifest,
};
use cordanui_schema::AgentStatus as SchemaAgentStatus;
use cordanui_sync::Database;
use serde::Deserialize;
use tracing::{error, info, warn};

use crate::db;

/// The agent runner. Owns the database handle and caches loaded Lua
/// plugins across tasks (a Lua plugin's state survives one run; the
/// runtime is reused for subsequent runs of the same plugin).
pub struct AgentRunner {
    db: Database,
}

/// Parsed `metadata` JSON on a goal — tells the backend which
/// agent/provider and model to use. `agent` is the generic field written by
/// the TUI picker (works for both `provider` and `agent` plugins);
/// `provider` is kept for backward compat.
#[derive(Debug, Default, Deserialize)]
struct GoalMetadata {
    #[serde(default)]
    agent: Option<String>,
    #[serde(default)]
    provider: Option<String>,
    #[serde(default)]
    model: Option<String>,
}

/// A resolved provider plugin ready to run.
struct ResolvedProvider {
    plugin_name: String,
    model: String,
    dir: PathBuf,
    manifest: PluginManifest,
    config: Option<serde_json::Value>,
}

impl AgentRunner {
    pub fn new(db: Database) -> Result<Self> {
        Ok(Self { db })
    }

    /// Poll mode: sync, fetch queued tasks, process each one sequentially.
    pub async fn poll_loop(self: &Arc<Self>, interval_secs: u64) {
        loop {
            if let Err(e) = db::sync(&self.db) {
                warn!("pre-poll sync failed: {e:#}");
            }

            match db::get_queued_tasks(&self.db) {
                Ok(tasks) => {
                    for task_id in tasks {
                        self.process_task(task_id).await;
                    }
                }
                Err(e) => warn!("failed to query queued tasks: {e:#}"),
            }

            tokio::time::sleep(std::time::Duration::from_secs(interval_secs)).await;
        }
    }

    /// Wake mode: start an HTTP server that receives `POST /wake { task_id }`
    /// and triggers immediate task processing. The server also runs a
    /// background poll loop as a fallback.
    pub async fn serve(self: &Arc<Self>, port: u16) -> Result<()> {
        use axum::{extract::State, routing::post, Router};
        use serde::Deserialize;

        #[derive(Deserialize)]
        struct WakeRequest {
            task_id: String,
        }

        // The poll loop runs in the background as a safety net — a missed
        // wake call (mobile app killed, network blip) still gets picked up.
        let poll_runner = self.clone();
        tokio::spawn(async move {
            poll_runner.poll_loop(60).await;
        });

        let runner = self.clone();
        let app = Router::new()
            .route("/wake", post(move |State(runner): State<Arc<Self>>, req: axum::Json<WakeRequest>| {
                let task_id = req.0.task_id.clone();
                async move {
                    let response_id = task_id.clone();
                    info!(task_id = %task_id, "wake received");
                    runner.process_task(task_id).await;
                    axum::Json(serde_json::json!({"ok": true, "task_id": response_id}))
                }
            }))
            .route(
                "/health",
                axum::routing::get(|| async { "ok" }),
            )
            .with_state(self.clone());

        let addr = std::net::SocketAddr::from(([0, 0, 0, 0], port));
        info!(addr = %addr, "agent backend listening");
        let listener = tokio::net::TcpListener::bind(addr).await?;
        axum::serve(listener, app).await?;
        Ok(())
    }

    /// Process a single task: mark running, resolve provider, invoke it,
    /// stream progress, write result.
    pub async fn process_task(self: &Arc<Self>, task_id: String) {
        // Sync first to get the latest state from mobile/TUI.
        if let Err(e) = db::sync(&self.db) {
            warn!(task_id = %task_id, "pre-task sync failed: {e:#}");
        }

        // Re-read the goal to get its current state.
        let goal = match db::get_goal(&self.db, &task_id) {
            Ok(Some(g)) => g,
            Ok(None) => {
                warn!(task_id = %task_id, "task not found (deleted?)");
                return;
            }
            Err(e) => {
                error!(task_id = %task_id, "failed to read task: {e:#}");
                return;
            }
        };

        // Guard: only process queued tasks.
        if goal.agent_status != Some(SchemaAgentStatus::Queued) {
            info!(task_id = %task_id, "skipping (not queued)");
            return;
        }

        // Mark running.
        if let Err(e) = db::set_running(&self.db, &task_id) {
            error!(task_id = %task_id, "failed to set running: {e:#}");
            return;
        }

        // Resolve the provider.
        let provider = match self.resolve_provider(&goal) {
            Ok(p) => p,
            Err(e) => {
                error!(task_id = %task_id, "provider resolution failed: {e:#}");
                let _ = db::set_result(
                    &self.db,
                    &task_id,
                    SchemaAgentStatus::Failed,
                    Some(&format!("no provider available: {e}")),
                );
                let _ = db::sync(&self.db);
                return;
            }
        };

        // Run it.
        let cfg = AgentRunConfig {
            task_id: task_id.clone(),
            title: goal.title.clone(),
            description: goal.description.clone(),
            model: if provider.model.is_empty() {
                None
            } else {
                Some(provider.model.clone())
            },
            config: provider.config.clone(),
        };

        let db_handle = self.db.clone();
        let task_id_for_events = task_id.clone();
        let on_event = move |event: &AgentEvent| {
            Self::handle_event(&db_handle, &task_id_for_events, event);
        };

        let result = if provider.manifest.is_lua() {
            self.run_lua_provider(&provider, &cfg, on_event).await
        } else {
            self.run_binary_provider(&provider, &cfg, on_event).await
        };

        // Write the final result.
        match result {
            Ok(AgentEvent::Result(r)) => {
                // Allow plugins to declare mobile FE changes declaratively:
                // - `mobile.json` → merged as `{ "mobile": <parsed> }` (widget tree for mobile's PluginCard)
                // - `__metadata__.json` → raw patch merged into metadata
                // This is how a plugin "on its own" creates changes to mobile FE.
                for file in &r.files {
                    let path = file.path.trim();
                    if let Some(content) = file.content.as_deref() {
                        if path == "__metadata__.json" || path.ends_with("/__metadata__.json") {
                            if let Ok(patch) = serde_json::from_str::<serde_json::Value>(content) {
                                let patch = if patch.is_object() { patch } else { serde_json::json!({ "value": patch }) };
                                let _ = db::merge_metadata(&self.db, &task_id, patch);
                            }
                        } else if path == "mobile.json" || path.ends_with("/mobile.json") {
                            if let Ok(val) = serde_json::from_str::<serde_json::Value>(content) {
                                // Wrap as { "mobile": <val> } if not already wrapped
                                let patch = if val.get("mobile").is_some() || val.get("card").is_some() || val.get("widgets").is_some() {
                                    serde_json::json!({ "mobile": val.get("mobile").cloned().unwrap_or(val.clone()) })
                                } else {
                                    serde_json::json!({ "mobile": val })
                                };
                                let _ = db::merge_metadata(&self.db, &task_id, patch);
                            }
                        }
                    }
                }
                let result_json =
                    serde_json::to_string(&serde_json::json!({
                        "content": r.content,
                        "files": r.files,
                    }))
                    .unwrap_or_else(|_| r.content.clone());
                let _ = db::set_result(
                    &self.db,
                    &task_id,
                    SchemaAgentStatus::Completed,
                    Some(&result_json),
                );
                info!(task_id = %task_id, "task completed");
            }
            Ok(AgentEvent::Error { message, detail }) => {
                let err_msg = match detail {
                    Some(d) => format!("{message}: {d}"),
                    None => message,
                };
                let _ = db::set_result(
                    &self.db,
                    &task_id,
                    SchemaAgentStatus::Failed,
                    Some(&err_msg),
                );
                error!(task_id = %task_id, "task failed: {err_msg}");
            }
            Ok(_) => {}
            Err(e) => {
                let _ = db::set_result(
                    &self.db,
                    &task_id,
                    SchemaAgentStatus::Failed,
                    Some(&format!("agent run error: {e:#}")),
                );
                error!(task_id = %task_id, "task error: {e:#}");
            }
        }

        // Push the result to Turso so other clients see it.
        if let Err(e) = db::sync(&self.db) {
            warn!(task_id = %task_id, "post-task sync failed: {e:#}");
        }
    }

    /// Resolve which agent/provider plugin and model to use for a goal.
    /// Priority:
    /// 1. `metadata.agent` or `metadata.provider` + `metadata.model` on the
    ///    goal (user choice from the TUI picker or mobile assignment).
    /// 2. First active `agent` or `provider` plugin (default).
    /// `agent` plugins may have no models — in that case model is empty.
    fn resolve_provider(&self, goal: &cordanui_schema::Goal) -> Result<ResolvedProvider> {
        let metadata = goal
            .metadata
            .as_deref()
            .and_then(|s| serde_json::from_str::<GoalMetadata>(s).ok())
            .unwrap_or_default();
        let wanted = metadata
            .agent
            .as_deref()
            .or(metadata.provider.as_deref())
            .map(|s| s.to_string());

        let plugins = db::list_plugins(&self.db).context("listing plugins")?;

        // First pass: try to match the wanted plugin if specified.
        // Second pass: fallback to first eligible plugin.
        for pass in 0..2 {
            for row in &plugins {
                if !row.active {
                    continue;
                }
                let dir = PathBuf::from(&row.dir);
                let manifest = PluginManifest::from_dir(&dir)
                    .with_context(|| format!("reading manifest for {}", row.id))?;
                if !manifest.capabilities.provider && !manifest.capabilities.agent {
                    continue;
                }

                // In pass 0, only consider the wanted plugin.
                if pass == 0 {
                    if let Some(w) = &wanted {
                        if &manifest.plugin.name != w {
                            continue;
                        }
                    } else {
                        continue; // no wanted -> skip pass 0
                    }
                }

                // Validate that we can actually run this plugin.
                // Provider plugins without models are still runnable as generic agents
                // (single entry), but pure provider plugins with an empty models list
                // and no agent capability are skipped.
                let has_provider_models = manifest
                    .provider
                    .as_ref()
                    .map(|p| !p.models.is_empty())
                    .unwrap_or(false);
                if manifest.capabilities.provider && !manifest.capabilities.agent && !has_provider_models {
                    continue;
                }

                // Pick the model: user's choice if it's in the list, else first model if any,
                // else empty string for pure agent plugins.
                let model = metadata
                    .model
                    .as_deref()
                    .filter(|m| {
                        manifest
                            .provider
                            .as_ref()
                            .map(|pr| pr.models.iter().any(|x| x == *m))
                            .unwrap_or(false)
                    })
                    .map(|s| s.to_string())
                    .or_else(|| {
                        manifest
                            .provider
                            .as_ref()
                            .and_then(|pr| pr.models.first().cloned())
                    })
                    .unwrap_or_default();

                // Collect settings.
                let mut values = db::get_plugin_settings(&self.db, &manifest.plugin.name)?;
                if let Some(ui) = &manifest.ui {
                    for f in &ui.fields {
                        if let Some(d) = &f.default {
                            values.entry(f.key.clone()).or_insert_with(|| d.clone());
                        }
                    }
                }
                let config = db::settings_to_config(&values);

                return Ok(ResolvedProvider {
                    plugin_name: manifest.plugin.name.clone(),
                    model,
                    dir,
                    manifest,
                    config,
                });
            }
        }

        anyhow::bail!("no active agent/provider plugin found");
    }

    /// Run a binary (subprocess) provider plugin.
    async fn run_binary_provider<F>(
        &self,
        provider: &ResolvedProvider,
        cfg: &AgentRunConfig,
        on_event: F,
    ) -> Result<AgentEvent>
    where
        F: FnMut(&AgentEvent) + Send + 'static,
    {
        let binary = provider.manifest.binary_path(&provider.dir);
        if !binary.exists() {
            anyhow::bail!(
                "provider binary not found: {}. Did the TUI build it?",
                binary.display()
            );
        }
        info!(plugin = %provider.plugin_name, binary = %binary.display(), "running binary provider");
        cordanui_plugin_runtime::spawn::run_streaming(&binary, cfg, on_event)
            .await
            .context("binary plugin run failed")
    }

    /// Run a Lua (in-process) provider plugin.
    async fn run_lua_provider<F>(
        &self,
        provider: &ResolvedProvider,
        cfg: &AgentRunConfig,
        on_event: F,
    ) -> Result<AgentEvent>
    where
        F: FnMut(&AgentEvent) + Send + 'static,
    {
        info!(plugin = %provider.plugin_name, "running Lua provider");
        let plugin = LuaPlugin::load(
            &provider.dir,
            &provider.plugin_name,
            provider.config.clone(),
            HostHooks::new(), // backend has no UI; providers don't need it
        )
        .with_context(|| format!("loading Lua plugin {}", provider.plugin_name))?;
        plugin.agent_run(cfg, on_event).await
    }

    /// Handle a streaming event from the provider: write progress to the DB.
    fn handle_event(db: &Database, task_id: &str, event: &AgentEvent) {
        match event {
            AgentEvent::Progress { message, detail } => {
                let progress = serde_json::json!({
                    "message": message,
                    "detail": detail,
                });
                if let Err(e) =
                    db::set_progress(db, task_id, &progress.to_string())
                {
                    warn!(task_id = %task_id, "failed to write progress: {e:#}");
                }
            }
            // Result and Error are handled by the caller (process_task).
            _ => {}
        }
    }
}

// Re-export AgentStatus for convenience in main.rs type references.
pub use cordanui_schema::AgentStatus;

#[cfg(test)]
mod tests {
    use super::*;
    use cordanui_sync::SyncConfig;

    fn test_db() -> Database {
        let dir = std::env::temp_dir().join(format!(
            "cordanui-runner-test-{}",
            cordanui_schema::new_id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let config = SyncConfig {
            db_path: dir.join("test.db"),
            ..Default::default()
        };
        Database::open(&config).unwrap()
    }

    #[test]
    fn resolve_provider_with_no_plugins_fails() {
        let db = test_db();
        let runner = AgentRunner::new(db).unwrap();

        let goal = cordanui_schema::Goal {
            id: "test".into(),
            title: "test".into(),
            description: None,
            status: cordanui_schema::GoalStatus::AgentMode,
            parent_id: None,
            sort_order: 0,
            created_at: "2024-01-01T00:00:00Z".into(),
            updated_at: "2024-01-01T00:00:00Z".into(),
            completed_at: None,
            agent_status: Some(SchemaAgentStatus::Queued),
            agent_result: None,
            agent_progress: None,
            metadata: None,
        };

        assert!(runner.resolve_provider(&goal).is_err());
    }

    #[test]
    fn parse_goal_metadata() {
        let json = r#"{"provider":"provider-zen","model":"grok-code"}"#;
        let meta: GoalMetadata = serde_json::from_str(json).unwrap();
        assert_eq!(meta.provider.as_deref(), Some("provider-zen"));
        assert_eq!(meta.model.as_deref(), Some("grok-code"));
    }

    #[test]
    fn empty_metadata_is_default() {
        let meta: GoalMetadata = serde_json::from_str("{}").unwrap();
        assert!(meta.provider.is_none());
        assert!(meta.model.is_none());
    }
}
