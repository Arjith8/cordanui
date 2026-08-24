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
//!
//! Plugins with `runtime = "lua"` in their manifest skip the build step
//! entirely: they are Lua scripts (`main.lua`) executed in-process by the
//! embedded Lua runtime ([`LuaPlugin`]), calling the host through the
//! injected `cordanui.*` API.

pub mod lua;
pub mod manifest;
pub mod protocol;
pub mod spawn;
pub mod style;
pub mod ui;

pub use lua::LuaPlugin;
pub use manifest::{
    BuildConfig, PluginCapability, PluginManifest, ProviderConfig, UiField, UiSpec,
};
pub use protocol::{
    AgentEvent, AgentEventType, AgentResult, AgentRunConfig, CompleteRequest, CompleteResponse,
};
pub use spawn::{run_one_shot, run_streaming};
pub use style::{parse_color, NullStyleHost, StyleHost, CORE_VARS, LEGACY_ALIASES};
pub use ui::{NoUiHost, PendingUi, SharedUiHost, UiHost, UiLevel, UiRequest, UiResponse};
