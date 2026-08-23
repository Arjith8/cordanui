//! The executor: ties together the database, plugin manifest, and plugin
//! subprocess spawning.
//!
//! Flow:
//! 1. Read the goal from DB by task_id.
//! 2. Validate it's in agent_mode.
//! 3. Find the provider plugin binary in plugin_dir.
//! 4. Set agent_status = running in DB.
//! 5. Spawn the plugin with `agent-run --task-id <id>`.
//! 6. For each progress event, write agent_progress to DB.
//! 7. On result, write agent_result + mark completed.
//! 8. On error, write failure.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use cordanui_plugin_runtime::{AgentEvent, AgentRunConfig, PluginManifest};
use cordanui_schema::Goal;
use cordanui_sync::Database;
use tokio::sync::Mutex;

use crate::config::Config;
use crate::db;

/// The outcome of an execution.
#[derive(Debug)]
pub struct ExecutionResult {
    pub task_id: String,
    pub success: bool,
    pub message: String,
}

/// The executor holds shared state: the config and a DB connection (wrapped
/// in a mutex since Database is not Sync — it owns a tokio runtime).
pub struct Executor {
    config: Config,
    db: Arc<Mutex<Database>>,
}

impl Executor {
    pub fn new(config: Config, db: Database) -> Self {
        Self {
            config,
            db: Arc::new(Mutex::new(db)),
        }
    }

    /// Execute a task: read the goal, run the provider plugin, write results
    /// back to the DB.
    pub async fn execute(&self, task_id: &str) -> Result<ExecutionResult> {
        // 1. Read the goal from DB
        let goal = {
            let db = self.db.lock().await;
            db::get_goal(&db, task_id)
        }?;

        let goal = match goal {
            Some(g) => g,
            None => {
                return Ok(ExecutionResult {
                    task_id: task_id.to_string(),
                    success: false,
                    message: format!("goal not found: {task_id}"),
                });
            }
        };

        tracing::info!(
            task_id = %task_id,
            title = %goal.title,
            "executing task"
        );

        // 2. Find the provider plugin
        let (manifest, binary) = match self.resolve_plugin() {
            Ok(v) => v,
            Err(e) => {
                let msg = format!("failed to resolve provider plugin: {e}");
                tracing::error!(task_id = %task_id, "{msg}");
                let db = self.db.lock().await;
                let _ = db::write_failure(&db, task_id, &msg);
                return Ok(ExecutionResult {
                    task_id: task_id.to_string(),
                    success: false,
                    message: msg,
                });
            }
        };

        // 3. Set agent_status = running
        {
            let db = self.db.lock().await;
            db::set_agent_running(&db, task_id)?;
        }

        // 4. Build the agent run config
        let model = self
            .config
            .provider_model
            .clone()
            .or_else(|| {
                manifest
                    .provider
                    .as_ref()
                    .and_then(|p| p.models.first().cloned())
            })
            .unwrap_or_default();

        let run_config = AgentRunConfig {
            task_id: task_id.to_string(),
            title: goal.title.clone(),
            description: goal.description.clone(),
            model: if model.is_empty() { None } else { Some(model) },
            config: None,
        };

        // 5. Run the plugin
        let db_clone = Arc::clone(&self.db);

        let on_event = |event: &AgentEvent| {
            match event {
                AgentEvent::Progress { message, .. } => {
                    tracing::info!(task_id = %task_id, "progress: {message}");
                    let progress_json = serde_json::json!({
                        "message": message,
                        "timestamp": cordanui_schema::now_iso(),
                    })
                    .to_string();

                    let db = db_clone.clone();
                    let progress = progress_json.clone();
                    let task_id_owned = task_id.to_string();
                    tokio::spawn(async move {
                        let db = db.lock().await;
                        if let Err(e) = db::write_progress(&db, &task_id_owned, &progress) {
                            tracing::warn!("failed to write progress: {e}");
                        }
                    });
                }
                AgentEvent::Result { .. } => {
                    tracing::info!(task_id = %task_id, "received result event");
                }
                AgentEvent::Error { message, .. } => {
                    tracing::warn!(task_id = %task_id, "plugin error: {message}");
                }
            }
        };

        match cordanui_plugin_runtime::run_streaming(&binary, &run_config, on_event).await {
            Ok(AgentEvent::Result(result)) => {
                let result_json = serde_json::to_string(&result)
                    .context("serializing agent result")?;
                let db = self.db.lock().await;
                db::write_result(&db, task_id, &result_json)?;
                tracing::info!(task_id = %task_id, "task completed successfully");
                Ok(ExecutionResult {
                    task_id: task_id.to_string(),
                    success: true,
                    message: result.content,
                })
            }
            Ok(AgentEvent::Error { message, detail }) => {
                let full_msg = match detail {
                    Some(d) => format!("{message}: {d}"),
                    None => message,
                };
                let db = self.db.lock().await;
                let _ = db::write_failure(&db, task_id, &full_msg);
                Ok(ExecutionResult {
                    task_id: task_id.to_string(),
                    success: false,
                    message: full_msg,
                })
            }
            Ok(AgentEvent::Progress { .. }) => {
                let msg = "plugin stream ended on a progress event (expected result or error)";
                let db = self.db.lock().await;
                let _ = db::write_failure(&db, task_id, msg);
                Ok(ExecutionResult {
                    task_id: task_id.to_string(),
                    success: false,
                    message: msg.to_string(),
                })
            }
            Err(e) => {
                let msg = format!("plugin execution failed: {e}");
                tracing::error!(task_id = %task_id, "{msg}");
                let db = self.db.lock().await;
                let _ = db::write_failure(&db, task_id, &msg);
                Ok(ExecutionResult {
                    task_id: task_id.to_string(),
                    success: false,
                    message: msg,
                })
            }
        }
    }

    /// Resolve the provider plugin: find its directory, parse its manifest,
    /// and locate its binary.
    fn resolve_plugin(&self) -> Result<(PluginManifest, PathBuf)> {
        let plugin_dir = self.config.plugin_dir.join(&self.config.provider_plugin);

        if !plugin_dir.exists() {
            anyhow::bail!(
                "plugin directory not found: {} (expected at {})",
                self.config.provider_plugin,
                plugin_dir.display()
            );
        }

        let manifest = PluginManifest::from_dir(&plugin_dir).context(format!(
            "parsing manifest for plugin: {}",
            self.config.provider_plugin
        ))?;

        if !manifest.capabilities.provider {
            anyhow::bail!(
                "plugin {} does not declare the 'provider' capability",
                self.config.provider_plugin
            );
        }

        let binary = manifest.binary_path(&plugin_dir);

        if !binary.exists() {
            anyhow::bail!(
                "plugin binary not found at {} — has it been built?",
                binary.display()
            );
        }

        Ok((manifest, binary))
    }
}

/// Read a goal from the DB (for the health/inspection endpoint).
pub async fn get_goal(db: &Arc<Mutex<Database>>, id: &str) -> Result<Option<Goal>> {
    let db = db.lock().await;
    db::get_goal(&db, id)
}
