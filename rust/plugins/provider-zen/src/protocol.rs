//! Protocol types — mirrors `crates/plugin-runtime/src/protocol.rs`.
//!
//! This is a local copy because provider plugins are standalone binaries
//! (not workspace members). They don't depend on the plugin-runtime crate;
//! they just need to produce/consume the same JSON shapes.
//!
//! The plugin-runtime crate is the authoritative source. If these types
//! drift, the host will fail to parse the plugin's output.

use serde::{Deserialize, Serialize};

// ---------- one-shot: complete ----------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompleteRequest {
    pub model: String,
    pub prompt: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompleteResponse {
    pub content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Usage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_tokens: Option<u32>,
}

// ---------- streaming: agent-run ----------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentRunConfig {
    pub task_id: String,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum AgentEvent {
    #[serde(rename = "progress")]
    Progress {
        message: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
    #[serde(rename = "result")]
    Result(AgentResult),
    #[serde(rename = "error")]
    Error {
        message: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentResult {
    pub content: String,
    #[serde(default)]
    pub files: Vec<AgentFile>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentFile {
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
}

// ---------- helpers ----------

/// Serialize an event as JSON and write it as a single line to stdout,
/// followed by a newline. This is the newline-delimited JSON protocol.
pub fn emit_event(event: &AgentEvent) {
    if let Ok(json) = serde_json::to_string(event) {
        println!("{json}");
        // Flush stdout so the host receives the line immediately. println!
        // uses a line-buffered handle when connected to a terminal, but our
        // stdout is a pipe — pipes are block-buffered by default in Rust.
        use std::io::Write;
        let _ = std::io::stdout().flush();
    }
}
