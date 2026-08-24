//! JSON-over-stdio protocol types.
//!
//! The host and plugin communicate via JSON. Two protocols:
//!
//! 1. **one-shot complete**: host sends `CompleteRequest` on stdin (one
//!    JSON object), plugin writes `CompleteResponse` to stdout (one JSON
//!    object).
//!
//! 2. **streaming agent-run**: host sends `AgentRunConfig` on stdin (one
//!    JSON object), plugin writes newline-delimited `AgentEvent`s to
//!    stdout. `AgentEvent::Progress` events stream as the plugin works,
//!    terminated by a single `AgentEvent::Result` or `AgentEvent::Error`.

use serde::{Deserialize, Serialize};

// ---------- one-shot: complete ----------

/// Request sent to a provider plugin for a one-shot completion.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompleteRequest {
    /// The model to use (e.g. "claude-sonnet-4-5").
    pub model: String,
    /// The prompt / instruction.
    pub prompt: String,
    /// Optional system prompt.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system: Option<String>,
    /// Optional max tokens.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    /// Optional temperature.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    /// Plugin settings collected by the host from its declarative `[ui]`
    /// form (namespaced keys stripped to bare field keys).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config: Option<serde_json::Value>,
}

/// Response from a provider plugin for a one-shot completion.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CompleteResponse {
    /// The generated content.
    pub content: String,
    /// Token usage (if available).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Usage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_tokens: Option<u32>,
}

// ---------- streaming: agent-run ----------

/// Configuration sent to a plugin for a streaming agent run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentRunConfig {
    /// The task/goal ID from the database.
    pub task_id: String,
    /// The goal title (human-readable).
    pub title: String,
    /// The goal description (what the agent should do).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// The model to use (if the plugin supports model selection).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Any plugin-specific configuration.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config: Option<serde_json::Value>,
}

/// A single streaming event from an agent run. Newline-delimited JSON on
/// stdout.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum AgentEvent {
    /// Progress update — the plugin is working.
    #[serde(rename = "progress")]
    Progress {
        message: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
    /// Final result — the agent is done.
    #[serde(rename = "result")]
    Result(AgentResult),
    /// Error — the agent failed.
    #[serde(rename = "error")]
    Error {
        message: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
}

/// Discriminator for event type (used internally for matching).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentEventType {
    Progress,
    Result,
    Error,
}

impl AgentEvent {
    pub fn event_type(&self) -> AgentEventType {
        match self {
            Self::Progress { .. } => AgentEventType::Progress,
            Self::Result { .. } => AgentEventType::Result,
            Self::Error { .. } => AgentEventType::Error,
        }
    }
}

/// The final result of an agent run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentResult {
    /// The main content / output of the agent.
    pub content: String,
    /// Optional files produced (paths or contents).
    #[serde(default)]
    pub files: Vec<AgentFile>,
    /// Optional token usage.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentFile {
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serialize_progress_event() {
        let event = AgentEvent::Progress {
            message: "Searching the web...".to_string(),
            detail: None,
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains(r#""type":"progress""#));
        assert!(json.contains(r#""message":"Searching the web...""#));
    }

    #[test]
    fn deserialize_result_event() {
        let json = r#"{"type":"result","content":"done","files":[],"usage":null}"#;
        let event: AgentEvent = serde_json::from_str(json).unwrap();
        match event {
            AgentEvent::Result(r) => assert_eq!(r.content, "done"),
            _ => panic!("expected Result variant"),
        }
    }

    #[test]
    fn deserialize_error_event() {
        let json = r#"{"type":"error","message":"API key missing"}"#;
        let event: AgentEvent = serde_json::from_str(json).unwrap();
        match event {
            AgentEvent::Error { message, .. } => assert_eq!(message, "API key missing"),
            _ => panic!("expected Error variant"),
        }
    }

    #[test]
    fn roundtrip_complete_request() {
        let req = CompleteRequest {
            model: "claude-sonnet-4-5".to_string(),
            prompt: "Write a haiku about goals".to_string(),
            system: Some("You are a poet".to_string()),
            max_tokens: Some(100),
            temperature: Some(0.7),
            config: None,
        };
        let json = serde_json::to_string(&req).unwrap();
        let back: CompleteRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(back.model, req.model);
        assert_eq!(back.prompt, req.prompt);
    }
}
