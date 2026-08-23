//! cordanui plugin runtime — manifest parsing, subprocess spawning, and the
//! JSON-over-stdio protocol between the host (TUI or agent backend) and
//! plugin binaries.
//!
//! Plugins are Rust CLIs. The host spawns them as subprocesses, writes JSON
//! to stdin, and reads line-delimited JSON from stdout. Two invocation modes:
//!
//! - **one-shot**: `plugin complete --model X < input.json` → single JSON
//!   object on stdout.
//! - **streaming**: `plugin agent-run --task-id X < config.json` →
//!   newline-delimited JSON events (progress / result) on stdout.

pub mod manifest;
pub mod protocol;
pub mod spawn;

pub use manifest::{PluginManifest, PluginCapability, ProviderConfig, BuildConfig};
pub use protocol::{
    CompleteRequest, CompleteResponse,
    AgentRunConfig, AgentEvent, AgentEventType, AgentResult,
};
pub use spawn::{run_one_shot, run_streaming};
