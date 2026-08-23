//! provider-zen — a cordanui provider plugin for OpenCode Zen.
//!
//! Accesses GPT, Claude, Gemini, Qwen, DeepSeek, Grok, Kimi, and more via
//! the Zen gateway at `https://opencode.ai/zen/v1/chat/completions`.
//!
//! Auth: `OPENCODE_API_KEY` environment variable (Bearer token).
//!
//! ## protocol
//!
//! The plugin is spawned by the host (cordanui-agents or the TUI) as a
//! subprocess. Two subcommands:
//!
//! - `complete --model <model>` — reads a `CompleteRequest` JSON object
//!   from stdin, writes a `CompleteResponse` JSON object to stdout.
//! - `agent-run --task-id <id>` — reads an `AgentRunConfig` JSON object
//!   from stdin, writes newline-delimited `AgentEvent` JSON objects to
//!   stdout (progress events, then a terminal result or error event).
//!
//! See `crates/plugin-runtime/src/protocol.rs` for the exact types.

mod protocol;
mod zen;

use std::io::Read;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "provider-zen", about = "OpenCode Zen provider for cordanui")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// One-shot completion: read CompleteRequest from stdin, write
    /// CompleteResponse to stdout.
    Complete {
        #[arg(long)]
        model: String,
    },
    /// Streaming agent run: read AgentRunConfig from stdin, write
    /// newline-delimited AgentEvents to stdout.
    AgentRun {
        #[arg(long)]
        task_id: String,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    // Read all of stdin (the JSON request/config).
    let mut input = String::new();
    std::io::stdin()
        .read_to_string(&mut input)
        .context("reading stdin")?;

    // Validate API key is present before doing anything else.
    let api_key = std::env::var("OPENCODE_API_KEY")
        .context("OPENCODE_API_KEY environment variable is not set")?;

    match cli.command {
        Command::Complete { model } => {
            let request: protocol::CompleteRequest =
                serde_json::from_str(&input).context("parsing CompleteRequest from stdin")?;

            let response = zen::complete_blocking(&api_key, &model, &request)
                .context("Zen API request failed")?;

            let json = serde_json::to_string(&response).context("serializing response")?;
            println!("{json}");
        }
        Command::AgentRun { task_id } => {
            let config: protocol::AgentRunConfig =
                serde_json::from_str(&input).context("parsing AgentRunConfig from stdin")?;

            // Emit a progress event immediately so the host knows we're alive.
            protocol::emit_event(&protocol::AgentEvent::Progress {
                message: "Starting agent run...".to_string(),
                detail: Some(format!("model: {}", config.model.as_deref().unwrap_or("default"))),
            });

            let rt = tokio::runtime::Runtime::new()?;
            rt.block_on(async {
                zen::agent_run_streaming(&api_key, &task_id, &config)
                    .await
                    .map_err(|e| {
                        protocol::emit_event(&protocol::AgentEvent::Error {
                            message: "Agent run failed".to_string(),
                            detail: Some(e.to_string()),
                        });
                        e
                    })
            })?;
        }
    }

    Ok(())
}
