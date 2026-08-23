//! OpenCode Zen API client.
//!
//! Zen exposes an OpenAI-compatible `/chat/completions` endpoint that works
//! with most models (GPT, Claude, Gemini, Qwen, DeepSeek, Grok, Kimi). We use
//! it for both one-shot completions and streaming agent runs.
//!
//! Base URL: `https://opencode.ai/zen/v1`
//! Auth: `Authorization: Bearer <OPENCODE_API_KEY>`
//!
//! For `complete`: non-streaming request, single response.
//! For `agent-run`: streaming request (SSE), reads chunks, emits progress
//! events with accumulated content, then a final result event.

use anyhow::{anyhow, Context, Result};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};

use crate::protocol::{
    AgentEvent, AgentResult, AgentRunConfig, CompleteRequest, CompleteResponse, Usage,
};

const ZEN_BASE_URL: &str = "https://opencode.ai/zen/v1";
const DEFAULT_MODEL: &str = "gpt-5.4";

// ---------- OpenAI-compatible request/response types ----------

#[derive(Debug, Serialize)]
struct ChatRequest {
    model: String,
    messages: Vec<ChatMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    stream: bool,
}

#[derive(Debug, Serialize)]
struct ChatMessage {
    role: String,
    content: String,
}

#[derive(Debug, Deserialize)]
struct ChatResponse {
    choices: Vec<ChatChoice>,
    #[serde(default)]
    usage: Option<OpenAIUsage>,
}

#[derive(Debug, Deserialize)]
struct ChatChoice {
    message: ChatChoiceMessage,
}

#[derive(Debug, Deserialize)]
struct ChatChoiceMessage {
    content: String,
}

#[derive(Debug, Deserialize)]
struct OpenAIUsage {
    #[serde(default)]
    prompt_tokens: u32,
    #[serde(default)]
    completion_tokens: u32,
    #[serde(default)]
    total_tokens: Option<u32>,
}

// ---------- SSE streaming types ----------

#[derive(Debug, Deserialize)]
struct StreamChunk {
    choices: Vec<StreamChoice>,
}

#[derive(Debug, Deserialize)]
struct StreamChoice {
    delta: StreamDelta,
}

#[derive(Debug, Deserialize)]
struct StreamDelta {
    #[serde(default)]
    content: Option<String>,
}

// ---------- public API ----------

/// One-shot completion. Sends a non-streaming request to Zen, returns the
/// full response. Uses `reqwest::blocking::Client` since this is called from
/// a synchronous context.
pub fn complete_blocking(
    api_key: &str,
    model: &str,
    request: &CompleteRequest,
) -> Result<CompleteResponse> {
    let client = reqwest::blocking::Client::new();
    let model = if model.is_empty() { DEFAULT_MODEL } else { model };

    let mut messages = Vec::new();
    if let Some(system) = &request.system {
        messages.push(ChatMessage {
            role: "system".to_string(),
            content: system.clone(),
        });
    }
    messages.push(ChatMessage {
        role: "user".to_string(),
        content: request.prompt.clone(),
    });

    let body = ChatRequest {
        model: model.to_string(),
        messages,
        max_tokens: request.max_tokens,
        temperature: request.temperature,
        stream: false,
    };

    let resp = client
        .post(format!("{ZEN_BASE_URL}/chat/completions"))
        .bearer_auth(api_key)
        .json(&body)
        .send()
        .context("sending request to Zen API")?;

    let status = resp.status();
    if !status.is_success() {
        let text = resp.text().unwrap_or_default();
        return Err(anyhow!("Zen API error {status}: {text}"));
    }

    let chat: ChatResponse = resp.json().context("parsing Zen response")?;

    let content = chat
        .choices
        .into_iter()
        .next()
        .ok_or_else(|| anyhow!("Zen API returned no choices"))?
        .message
        .content;

    let usage = chat.usage.map(|u| Usage {
        prompt_tokens: u.prompt_tokens,
        completion_tokens: u.completion_tokens,
        total_tokens: u.total_tokens,
    });

    Ok(CompleteResponse { content, usage })
}

/// Streaming agent run. Sends a streaming request to Zen, reads SSE chunks,
/// emits progress events with accumulated content, then a final result
/// event.
pub async fn agent_run_streaming(
    api_key: &str,
    _task_id: &str,
    config: &AgentRunConfig,
) -> Result<()> {
    let model = config
        .model
        .as_deref()
        .filter(|m| !m.is_empty())
        .unwrap_or(DEFAULT_MODEL);

    let client = reqwest::Client::new();

    let system_prompt = build_system_prompt(config);
    let user_prompt = build_user_prompt(config);

    let messages = vec![
        ChatMessage {
            role: "system".to_string(),
            content: system_prompt,
        },
        ChatMessage {
            role: "user".to_string(),
            content: user_prompt,
        },
    ];

    let body = ChatRequest {
        model: model.to_string(),
        messages,
        max_tokens: None,
        temperature: Some(0.7),
        stream: true,
    };

    crate::protocol::emit_event(&AgentEvent::Progress {
        message: format!("Calling Zen API (model: {model})..."),
        detail: None,
    });

    let resp = client
        .post(format!("{ZEN_BASE_URL}/chat/completions"))
        .bearer_auth(api_key)
        .json(&body)
        .send()
        .await
        .context("sending streaming request to Zen API")?;

    let status = resp.status();
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        return Err(anyhow!("Zen API error {status}: {text}"));
    }

    // Read the SSE stream: reqwest's bytes_stream() gives us chunks. We
    // accumulate them in a buffer and process complete lines as they arrive.
    let mut full_response = String::new();
    let mut stream = resp.bytes_stream();
    let mut buffer = String::new();

    while let Some(chunk_result) = stream.next().await {
        let chunk = chunk_result.context("reading stream chunk")?;
        buffer.push_str(&String::from_utf8_lossy(&chunk));

        // Process complete lines from the buffer.
        while let Some(newline_pos) = buffer.find('\n') {
            let line = buffer[..newline_pos].to_string();
            buffer.drain(..=newline_pos);

            let line = line.trim();
            if line.is_empty() || !line.starts_with("data: ") {
                continue;
            }

            let data = &line[6..];
            if data == "[DONE]" {
                continue;
            }

            if let Ok(chunk) = serde_json::from_str::<StreamChunk>(data) {
                if let Some(choice) = chunk.choices.into_iter().next() {
                    if let Some(content) = choice.delta.content {
                        if !content.is_empty() {
                            full_response.push_str(&content);
                            crate::protocol::emit_event(&AgentEvent::Progress {
                                message: format!("Received {} chars...", full_response.len()),
                                detail: Some(content),
                            });
                        }
                    }
                }
            }
        }
    }

    // Emit the final result.
    crate::protocol::emit_event(&AgentEvent::Result(AgentResult {
        content: full_response,
        files: vec![],
        usage: None,
    }));

    Ok(())
}

// ---------- helpers ----------

fn build_system_prompt(config: &AgentRunConfig) -> String {
    let mut prompt = "You are an AI agent working on a goal/task. \
        Analyze the task and provide a thorough, actionable response. \
        If the task is a goal that needs to be broken down, provide a clear plan. \
        If the task is something you can complete directly, do so."
        .to_string();

    if let Some(desc) = &config.description {
        if !desc.is_empty() {
            prompt.push_str(&format!("\n\nTask description: {desc}"));
        }
    }

    prompt
}

fn build_user_prompt(config: &AgentRunConfig) -> String {
    format!("Task: {}\n\nPlease work on completing this task.", config.title)
}
