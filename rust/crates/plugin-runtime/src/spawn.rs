//! Subprocess spawning — runs plugin binaries and handles the JSON-over-stdio
//! protocol.
//!
//! Two invocation modes:
//! - `run_one_shot`: sends a JSON request on stdin, reads one JSON response
//!   from stdout.
//! - `run_streaming`: sends a JSON config on stdin, reads newline-delimited
//!   JSON events from stdout, invoking a callback for each event.

use std::path::Path;
use std::process::Stdio;

use anyhow::{Context, Result};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;

use crate::protocol::{AgentEvent, AgentRunConfig, CompleteRequest, CompleteResponse};

/// Run a one-shot `complete` invocation.
///
/// Spawns the plugin binary with `complete` subcommand, writes the request
/// as JSON to stdin, reads a single JSON response from stdout.
pub async fn run_one_shot(
    binary: &Path,
    request: &CompleteRequest,
) -> Result<CompleteResponse> {
    let request_json = serde_json::to_string(request).context("serializing request")?;

    let mut child = Command::new(binary)
        .arg("complete")
        .arg("--model")
        .arg(&request.model)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("spawning plugin binary: {}", binary.display()))?;

    // Write request to stdin
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(request_json.as_bytes()).await?;
        stdin.shutdown().await?;
    }

    // Read stdout
    let mut stdout = child.stdout.take().context("no stdout from plugin")?;
    let mut output = String::new();
    stdout.read_to_string(&mut output).await?;

    // Read stderr for error context
    let mut stderr = child.stderr.take().context("no stderr from plugin")?;
    let mut stderr_output = String::new();
    stderr.read_to_string(&mut stderr_output).await?;

    let status = child.wait().await?;

    if !status.success() {
        anyhow::bail!(
            "plugin exited with status {status}\nstderr: {stderr_output}"
        );
    }

    let response: CompleteResponse =
        serde_json::from_str(output.trim()).context(format!(
            "parsing plugin response. stdout: {output}\nstderr: {stderr_output}"
        ))?;

    Ok(response)
}

/// Run a streaming `agent-run` invocation.
///
/// Spawns the plugin binary with `agent-run --task-id <id>` subcommand,
/// writes the config as JSON to stdin, then reads newline-delimited JSON
/// events from stdout. The `on_event` callback is called for each event.
///
/// Returns the final `AgentEvent` (Result or Error), or an error if the
/// plugin exits without sending one.
pub async fn run_streaming<F>(
    binary: &Path,
    config: &AgentRunConfig,
    mut on_event: F,
) -> Result<AgentEvent>
where
    F: FnMut(&AgentEvent),
{
    let config_json = serde_json::to_string(config).context("serializing config")?;

    let mut child = Command::new(binary)
        .arg("agent-run")
        .arg("--task-id")
        .arg(&config.task_id)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("spawning plugin binary: {}", binary.display()))?;

    // Write config to stdin
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(config_json.as_bytes()).await?;
        stdin.shutdown().await?;
    }

    // Read stdout line by line
    let stdout = child.stdout.take().context("no stdout from plugin")?;
    let reader = BufReader::new(stdout);
    let mut lines = reader.lines();

    let mut final_event: Option<AgentEvent> = None;

    while let Ok(Some(line)) = lines.next_line().await {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        match serde_json::from_str::<AgentEvent>(line) {
            Ok(event) => {
                let is_terminal = matches!(
                    event,
                    AgentEvent::Result { .. } | AgentEvent::Error { .. }
                );
                on_event(&event);
                if is_terminal {
                    final_event = Some(event);
                    break;
                }
            }
            Err(e) => {
                tracing::warn!("failed to parse plugin output line: {line}\nerror: {e}");
            }
        }
    }

    // Wait for the process to exit
    let status = child.wait().await?;

    if let Some(event) = final_event {
        return Ok(event);
    }

    // Collect stderr for the error message
    let mut stderr_output = String::new();
    if let Some(mut stderr) = child.stderr.take() {
        let _ = stderr.read_to_string(&mut stderr_output).await;
    }

    if !status.success() {
        anyhow::bail!(
            "plugin exited with status {status} without a terminal event\nstderr: {stderr_output}"
        );
    }

    anyhow::bail!("plugin stream ended without a result or error event")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// Create a fake plugin binary (a shell script) that outputs a given
    /// sequence of JSON lines.
    fn make_fake_plugin(name: &str, script: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join("cordanui-plugin-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        let mut file = std::fs::File::create(&path).unwrap();
        writeln!(file, "#!/bin/sh").unwrap();
        writeln!(file, "{script}").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&path).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&path, perms).unwrap();
        }
        path
    }

    #[tokio::test]
    async fn test_run_one_shot() {
        let script = r#"cat > /dev/null; echo '{"content":"hello","usage":null}'"#;
        let binary = make_fake_plugin("fake-complete", script);

        let request = CompleteRequest {
            model: "test-model".to_string(),
            prompt: "say hello".to_string(),
            system: None,
            max_tokens: None,
            temperature: None,
        };

        let response = run_one_shot(&binary, &request).await.unwrap();
        assert_eq!(response.content, "hello");
    }

    #[tokio::test]
    async fn test_run_streaming() {
        let script = r#"cat > /dev/null; printf '{"type":"progress","message":"working"}\n{"type":"result","content":"done","files":[],"usage":null}\n'"#;
        let binary = make_fake_plugin("fake-stream", script);

        let config = AgentRunConfig {
            task_id: "test-task".to_string(),
            title: "Test".to_string(),
            description: None,
            model: None,
            config: None,
        };

        let mut events = Vec::new();
        let result = run_streaming(&binary, &config, |e| {
            events.push(e.clone());
        })
        .await
        .unwrap();

        assert_eq!(events.len(), 2);
        assert!(matches!(events[0], AgentEvent::Progress { .. }));
        match result {
            AgentEvent::Result(r) => assert_eq!(r.content, "done"),
            _ => panic!("expected Result"),
        }
    }
}
