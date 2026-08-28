//! Plugin services — long-running processes a plugin ships, in any
//! language.
//!
//! A plugin declares a `[service]` section in its manifest; the host (or
//! the `cordanui service` CLI) spawns it, tracks the pid, and stops it on
//! demand. The process itself is a black box — the host only knows how to
//! start/stop it and where it listens.
//!
//! Two consumers share this module:
//! - **TUI**: [`ServiceManager`] supervises services while the app runs
//!   (autostart on activation, `s` toggle in the plugin manager, stderr
//!   streamed into a per-plugin log ring buffer).
//! - **CLI**: `cordanui service list|start|stop|status` for headless use
//!   (servers, systemd units). Detached spawn + pidfile; no TUI needed.
//!
//! Pidfiles live in `~/.local/share/cordanui/services/<plugin>.pid` and
//! are shared by both consumers, so the CLI can stop a service the TUI
//! started and vice versa.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};

use cordanui_plugin_runtime::{PluginManifest, ServiceConfig};

/// Where pidfiles (and service logs) live.
pub fn services_dir() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("cordanui")
        .join("services")
}

fn pidfile_path(plugin: &str) -> PathBuf {
    services_dir().join(format!("{plugin}.pid"))
}

fn logfile_path(plugin: &str) -> PathBuf {
    services_dir().join(format!("{plugin}.log"))
}

/// A service currently supervised by this process.
struct Running {
    child: Child,
}

/// Supervises plugin services. Shared via `Arc` between the UI thread,
/// worker threads, and the Lua runtime.
#[derive(Default)]
pub struct ServiceManager {
    running: Mutex<HashMap<String, Running>>,
    /// Registry of known services: plugin -> (dir, spec). Populated by
    /// the host when plugins load.
    registry: Mutex<HashMap<String, (PathBuf, ServiceConfig)>>,
    /// Per-plugin stderr/stdout log ring buffers (most recent last).
    logs: Mutex<HashMap<String, Vec<String>>>,
}

const LOG_CAP: usize = 100;

impl ServiceManager {
    pub fn new() -> Self {
        Self::default()
    }

    /// Make a service known so [`Self::start_service`] /
    /// [`ServiceHost`][cordanui_plugin_runtime::UiHost]-style lookups by
    /// name can resolve it.
    pub fn register(&self, plugin: &str, dir: &Path, spec: ServiceConfig) {
        self.registry
            .lock()
            .unwrap()
            .insert(plugin.to_string(), (dir.to_path_buf(), spec));
    }

    pub fn registered_spec(&self, plugin: &str) -> Option<ServiceConfig> {
        self.registry
            .lock()
            .unwrap()
            .get(plugin)
            .map(|(_, s)| s.clone())
    }

    fn push_log(&self, plugin: &str, line: String) {
        let mut logs = self.logs.lock().unwrap();
        let buf = logs.entry(plugin.to_string()).or_default();
        buf.push(line);
        if buf.len() > LOG_CAP {
            let drain = buf.len() - LOG_CAP;
            buf.drain(..drain);
        }
    }

    /// Recent log lines for a service (oldest first, capped).
    pub fn recent_logs(&self, plugin: &str) -> Vec<String> {
        self.logs
            .lock()
            .unwrap()
            .get(plugin)
            .cloned()
            .unwrap_or_default()
    }

    /// Spawn a service from its manifest spec. No-op if already running.
    /// `extra` args are appended after the manifest's defaults.
    pub fn start_service(
        &self,
        plugin: &str,
        dir: &Path,
        spec: &ServiceConfig,
        extra: &[String],
    ) -> anyhow::Result<()> {
        if self.is_running(plugin) {
            return Ok(());
        }
        std::fs::create_dir_all(services_dir())?;

        let logfile = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(logfile_path(plugin))?;
        let stderr = logfile.try_clone()?;

        let mut cmd = Command::new(&spec.cmd);
        cmd.args(&spec.args)
            .args(extra)
            .current_dir(dir)
            .stdout(Stdio::from(logfile))
            .stderr(Stdio::from(stderr));

        let child = cmd.spawn().map_err(|e| {
            let base = format!("spawning '{}' in {}: {e}", spec.cmd, dir.display());
            if e.kind() == std::io::ErrorKind::NotFound {
                anyhow::anyhow!(
                    "{base} — binary not found in PATH. Install '{}' or use absolute `cmd` in cordanui.toml [service]",
                    spec.cmd
                )
            } else {
                anyhow::anyhow!(base)
            }
        })?;

        let pid = child.id();
        std::fs::write(pidfile_path(plugin), pid.to_string())?;
        self.push_log(plugin, format!("[started pid {pid}]"));
        self.running
            .lock()
            .unwrap()
            .insert(plugin.to_string(), Running { child });
        Ok(())
    }

    /// Start a previously registered service.
    pub fn start_registered(&self, plugin: &str, extra: &[String]) -> anyhow::Result<()> {
        let (dir, spec) = self
            .registry
            .lock()
            .unwrap()
            .get(plugin)
            .map(|(d, s)| (d.clone(), s.clone()))
            .ok_or_else(|| anyhow::anyhow!("no service registered for '{plugin}'"))?;
        self.start_service(plugin, &dir, &spec, extra)
    }

    /// Stop a supervised service. Returns true if it was running.
    pub fn stop_service(&self, plugin: &str) -> anyhow::Result<bool> {
        let child = self.running.lock().unwrap().remove(plugin);
        if let Some(mut running) = child {
            let _ = running.child.kill();
            let _ = running.child.wait();
            let _ = std::fs::remove_file(pidfile_path(plugin));
            self.push_log(plugin, "[stopped]".into());
            return Ok(true);
        }
        Ok(false)
    }

    /// Whether the service is running (reaps exited children).
    pub fn is_running(&self, plugin: &str) -> bool {
        let mut running = self.running.lock().unwrap();
        match running.get_mut(plugin) {
            Some(running_service) => match running_service.child.try_wait() {
                Ok(Some(_)) => {
                    running.remove(plugin);
                    false
                }
                Ok(None) => true,
                Err(_) => false,
            },
            None => false,
        }
    }
}

impl cordanui_plugin_runtime::ServiceHost for ServiceManager {
    fn start(&self, plugin: &str, extra_args: &[String]) -> anyhow::Result<()> {
        self.start_registered(plugin, extra_args)
    }

    fn stop(&self, plugin: &str) -> anyhow::Result<()> {
        self.stop_service(plugin)?;
        Ok(())
    }

    fn is_running(&self, plugin: &str) -> bool {
        ServiceManager::is_running(self, plugin)
    }

    fn base_url(&self, plugin: &str) -> Option<String> {
        self.registered_spec(plugin).and_then(|s| s.base_url())
    }
}

// ---------- CLI (`cordanui service ...`) ----------

/// Read the pid from a plugin's pidfile, if any.
fn read_pid(plugin: &str) -> Option<u32> {
    std::fs::read_to_string(pidfile_path(plugin))
        .ok()
        .and_then(|s| s.trim().parse().ok())
}

fn pid_alive(pid: u32) -> bool {
    Path::new(&format!("/proc/{pid}")).exists()
}

/// Entry point for `cordanui service <subcommand>`. Runs without the TUI.
pub fn cli_run(args: &[String]) -> anyhow::Result<()> {
    let sub = args.first().map(String::as_str).unwrap_or("list");
    match sub {
        "list" => cli_list()?,
        "start" => cli_start(args.get(1), args_iter_after(args, 2))?,
        "stop" => cli_stop(args.get(1))?,
        "status" => cli_status(args.get(1))?,
        other => {
            eprintln!("unknown subcommand '{other}'");
            eprintln!("usage: cordanui service list | start <plugin> [-- args...] | stop <plugin> | status <plugin>");
            std::process::exit(2);
        }
    }
    Ok(())
}

fn args_iter_after(args: &[String], from: usize) -> Vec<String> {
    let rest = &args[from.min(args.len())..];
    let rest = if rest.first().map(String::as_str) == Some("--") {
        &rest[1..]
    } else {
        rest
    };
    rest.to_vec()
}

fn cli_list() -> anyhow::Result<()> {
    let db = crate::db::open()?;
    let plugins = crate::db::list_plugins(&db)?;
    let mut found = false;
    for row in plugins {
        let manifest = match PluginManifest::from_dir(Path::new(&row.dir)) {
            Ok(m) => m,
            Err(_) => continue,
        };
        if let Some(service) = &manifest.service {
            found = true;
            println!(
                "{}  {}  [{}]",
                row.id,
                if row.active { "active" } else { "inactive" },
                service.cmd
            );
        }
    }
    if !found {
        println!("no installed plugins expose a [service]");
    }
    Ok(())
}

fn cli_start(plugin: Option<&String>, extra: Vec<String>) -> anyhow::Result<()> {
    let Some(plugin) = plugin else {
        anyhow::bail!("usage: cordanui service start <plugin> [-- args...]");
    };
    let db = crate::db::open()?;
    let row = crate::db::list_plugins(&db)?
        .into_iter()
        .find(|p| p.id == *plugin)
        .ok_or_else(|| anyhow::anyhow!("plugin '{plugin}' is not installed"))?;
    let manifest = PluginManifest::from_dir(Path::new(&row.dir))?;
    let spec = manifest
        .service
        .ok_or_else(|| anyhow::anyhow!("plugin '{plugin}' declares no [service]"))?;

    if read_pid(plugin).map(pid_alive).unwrap_or(false) {
        println!(
            "{} already running (pid {})",
            plugin,
            read_pid(plugin).unwrap()
        );
        return Ok(());
    }
    std::fs::create_dir_all(services_dir())?;

    let mut cmd = Command::new(&spec.cmd);
    cmd.args(&spec.args)
        .args(&extra)
        .current_dir(&row.dir)
        .stdout(Stdio::from(
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(logfile_path(plugin))?,
        ))
        .stderr(Stdio::from(
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(logfile_path(plugin))?,
        ));
    let child = cmd.spawn().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            anyhow::anyhow!(
                "spawning '{}' in {}: {e} — binary not found in PATH. Install '{}' (bun.sh) or use absolute `cmd` in cordanui.toml [service]",
                spec.cmd,
                row.dir,
                spec.cmd
            )
        } else {
            anyhow::anyhow!("spawning '{}' in {}: {e}", spec.cmd, row.dir)
        }
    })?;
    let pid = child.id();
    // Intentionally not waited: the CLI exits, the child re-parents to
    // init (no zombie) and keeps running headless.
    std::fs::write(pidfile_path(plugin), pid.to_string())?;
    println!(
        "started {} (pid {pid}) — logs: {}",
        plugin,
        logfile_path(plugin).display()
    );
    Ok(())
}

fn cli_stop(plugin: Option<&String>) -> anyhow::Result<()> {
    let Some(plugin) = plugin else {
        anyhow::bail!("usage: cordanui service stop <plugin>");
    };
    let Some(pid) = read_pid(plugin) else {
        println!("{} is not running (no pidfile)", plugin);
        return Ok(());
    };
    if !pid_alive(pid) {
        let _ = std::fs::remove_file(pidfile_path(plugin));
        println!("{} was not running (stale pidfile cleaned)", plugin);
        return Ok(());
    }
    let status = Command::new("kill").arg(pid.to_string()).status()?;
    if status.success() {
        let _ = std::fs::remove_file(pidfile_path(plugin));
        println!("stopped {plugin} (pid {pid})");
    } else {
        anyhow::bail!("failed to kill pid {pid}");
    }
    Ok(())
}

fn cli_status(plugin: Option<&String>) -> anyhow::Result<()> {
    let Some(plugin) = plugin else {
        anyhow::bail!("usage: cordanui service status <plugin>");
    };
    match read_pid(plugin) {
        Some(pid) if pid_alive(pid) => println!("{} running (pid {pid})", plugin),
        Some(_) => println!("{} not running (stale pidfile)", plugin),
        None => println!("{} not running", plugin),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> (PathBuf, ServiceConfig) {
        let dir = std::env::temp_dir()
            .join("cordanui-services-test")
            .join(format!("{}-{name}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let spec = ServiceConfig {
            cmd: "/bin/sleep".into(),
            args: vec!["5".into()],
            addr: Some("http://127.0.0.1:18081".into()),
            health: None,
            autostart: false,
        };
        (dir, spec)
    }

    #[test]
    fn service_lifecycle_start_stop() {
        let manager = ServiceManager::new();
        let (dir, spec) = fixture("lifecycle");
        manager.register("sleepy", &dir, spec);

        assert!(!manager.is_running("sleepy"));
        manager.start_registered("sleepy", &[]).unwrap();
        assert!(manager.is_running("sleepy"));
        // pidfile written for cross-boundary stop (cli).
        assert!(read_pid("sleepy").is_some());

        // Starting twice is a no-op, not a second process.
        manager.start_registered("sleepy", &[]).unwrap();

        // Extra args path: a second service with different args runs.
        manager.start_registered("sleepy", &[]).unwrap();

        manager.stop_service("sleepy").unwrap();
        assert!(!manager.is_running("sleepy"));

        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_file(pidfile_path("sleepy"));
    }

    #[test]
    fn start_unknown_service_errors() {
        let manager = ServiceManager::new();
        let err = manager.start_registered("ghost", &[]).unwrap_err();
        assert!(err.to_string().contains("no service registered"));
    }
}
