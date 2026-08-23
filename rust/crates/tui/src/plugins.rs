//! Plugin manager backend — GitHub repo lookup + install via `gh`/`git`.
//!
//! GitHub-only for now. A task runs on a background thread and reports
//! progress over an mpsc channel that the main loop drains between frames:
//!
//!   link / owner-repo  →  verify repo → clone (or pull) into the plugin
//!                         dir → validate `cordanui.toml` → installed
//!   free text          →  `gh search repos` → result list
//!
//! Every step emits [`TaskEvent::Log`] lines so the UI can show what's
//! happening instead of a bare spinner.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{channel, Receiver, Sender};

use serde::Deserialize;

/// One matching repository.
#[derive(Debug, Clone)]
pub struct RepoInfo {
    pub full_name: String,
    pub description: Option<String>,
    pub stars: u64,
}

impl RepoInfo {
    /// Short display line for the popup list.
    pub fn summary(&self) -> String {
        match &self.description {
            Some(d) if !d.is_empty() => {
                let d = truncate(d, 48);
                format!("{:>4}★  {} — {}", self.stars, self.full_name, d)
            }
            _ => format!("{:>4}★  {}", self.stars, self.full_name),
        }
    }
}

/// A theme pack found inside an installed plugin repo.
#[derive(Debug, Clone)]
pub struct ThemeFile {
    pub id: String,
    pub name: String,
    /// Serialized `colors` object, ready to store as the row's colors_json.
    pub colors_json: String,
}

/// What the background thread reports back.
pub enum TaskEvent {
    /// Progress line for the activity log.
    Log(String),
    /// Free-text search finished; candidate repos to choose from.
    Results(Vec<RepoInfo>),
    /// Exact lookup found nothing.
    NotFound(String),
    /// Terminal failure.
    Error(String),
    /// Repo cloned/updated and its manifest validated. Carries any theme
    /// packs found at the repo root so the host can import them.
    Installed {
        name: String,
        dir: String,
        themes: Vec<ThemeFile>,
    },}

/// Lifecycle of one task started from the plugin manager popup.
#[derive(Debug, Clone, Default)]
pub enum TaskState {
    #[default]
    Idle,
    /// In flight; carries the activity log so far.
    Working(Vec<String>),
    Results(Vec<RepoInfo>),
    NotFound(String),
    Error(String),
    Installed {
        name: String,
        dir: String,
        /// How many theme packs were imported (for the success line).
        theme_count: usize,
    },
}

// --- gh JSON shapes (only the fields we ask for) ---

#[derive(Deserialize)]
struct GhSearchRepo {
    #[serde(rename = "fullName")]
    full_name: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default, rename = "stargazersCount")]
    stargazers_count: u64,
}

/// Kick off a full task on a background thread.
pub fn spawn_plugin_task(query: &str) -> Receiver<TaskEvent> {
    let (tx, rx) = channel();
    let query = query.trim().to_string();
    std::thread::spawn(move || {
        let _ = run_task(&tx, &query);
    });
    rx
}

fn run_task(tx: &Sender<TaskEvent>, query: &str) {
    match extract_owner_repo(query) {
        // A GitHub link or `owner/repo` — verify, then fetch + install.
        Some(slug) => install_flow(tx, &slug),
        // Anything else — free-text repo search.
        None => forward(search_repos(query), tx),
    }
}

fn forward(outcome: SearchOutcome, tx: &Sender<TaskEvent>) {
    let _ = tx.send(match outcome {
        SearchOutcome::Results(r) => TaskEvent::Results(r),
        SearchOutcome::NotFound(q) => TaskEvent::NotFound(q),
        SearchOutcome::Error(e) => TaskEvent::Error(e),
    });
}

enum SearchOutcome {
    Results(Vec<RepoInfo>),
    NotFound(String),
    Error(String),
}

/// Install flow: straight to clone/pull — git's own errors tell us if a
/// repo doesn't exist, so there's nothing to validate up front.
fn install_flow(tx: &Sender<TaskEvent>, slug: &str) {
    let _ = tx.send(TaskEvent::Log(format!("Fetching {slug}…")));

    let dest = plugin_install_dir(slug);
    if dest.exists() {
        let _ = tx.send(TaskEvent::Log(
            "Already installed — pulling updates (--progress)…".into(),
        ));
        if let Err(e) = stream_git(
            tx,
            Command::new("git")
                .args(["-C"])
                .arg(&dest)
                .args(["pull", "--ff-only", "--progress"]),
        ) {
            let _ = tx.send(TaskEvent::Error(format!("update failed: {e}")));
        }
    } else {
        let _ = tx.send(TaskEvent::Log(format!(
            "Cloning into '{}'…",
            dest.to_string_lossy()
        )));
        let url = format!("https://github.com/{slug}.git");
        // --progress: git silences its progress stream when stderr is not a
        // TTY (our case — it's a pipe), this flag forces it back on.
        if let Err(e) = stream_git(
            tx,
            Command::new("git")
                .args(["clone", "--progress"])
                .arg(&url)
                .arg(&dest),
        ) {
            let _ = tx.send(TaskEvent::Error(format!("clone failed: {e}")));
            let _ = std::fs::remove_dir_all(&dest);
            return;
        }
        let _ = tx.send(TaskEvent::Log("Clone complete.".into()));
    }

    // Validate it actually is a cordanui plugin before declaring success.
    let manifest = match cordanui_plugin_runtime::PluginManifest::from_dir(&dest) {
        Ok(m) => m,
        Err(e) => {
            let _ = tx.send(TaskEvent::Error(format!(
                "not a valid cordanui plugin: {e}"
            )));
            return;
        }
    };

    // Collect theme packs (if any) for the host to import.
    let themes = scan_theme_files(&dest);
    if manifest.capabilities.theme && themes.is_empty() {
        let _ = tx.send(TaskEvent::Log(
            "  note: theme plugin but no <id>.json theme files found".into(),
        ));
    } else if !themes.is_empty() {
        let _ = tx.send(TaskEvent::Log(format!(
            "  {} theme pack{}: {}",
            themes.len(),
            if themes.len() == 1 { "" } else { "s" },
            themes.iter().map(|t| t.name.clone()).collect::<Vec<_>>().join(", ")
        )));
    }

    let _ = tx.send(TaskEvent::Log(format!(
        "Manifest OK — {} v{} ({})",
        manifest.plugin.name, manifest.plugin.version,
        caps_label(&manifest)
    )));
    let _ = tx.send(TaskEvent::Installed {
        name: manifest.plugin.name,
        dir: dest.to_string_lossy().into_owned(),
        themes,
    });
}

/// Scan a plugin repo root for theme artifacts (`<id>.json`).
/// Tolerates junk: files that don't match the contract are skipped.
pub fn scan_theme_files(dir: &Path) -> Vec<ThemeFile> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else {
            continue;
        };
        let (Some(id), Some(name), Some(colors)) = (
            v.get("id").and_then(|x| x.as_str()),
            v.get("name").and_then(|x| x.as_str()),
            v.get("colors"),
        ) else {
            continue;
        };
        if !colors.is_object() {
            continue;
        }
        out.push(ThemeFile {
            id: id.to_string(),
            name: name.to_string(),
            colors_json: colors.to_string(),
        });
    }
    out
}

fn caps_label(m: &cordanui_plugin_runtime::PluginManifest) -> String {
    let c = &m.capabilities;
    let mut v = Vec::new();
    if c.provider {
        v.push("provider");
    }
    if c.tool {
        v.push("tool");
    }
    if c.agent {
        v.push("agent");
    }
    if c.theme {
        v.push("theme");
    }
    if c.command {
        v.push("command");
    }
    if v.is_empty() {
        "no capabilities".into()
    } else {
        v.join(", ")
    }
}

/// Run a git command, streaming its stderr to the activity log. Returns
/// `Err` with the last meaningful error line on failure.
fn stream_git(tx: &Sender<TaskEvent>, cmd: &mut Command) -> Result<(), String> {
    cmd.stdout(Stdio::null()).stderr(Stdio::piped());
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(_) => return Err("git not found".into()),
    };
    let mut last_error: Option<String> = None;
    if let Some(mut stderr) = child.stderr.take() {
        // Frames arrive \r-terminated (progress rewrites) or \n-terminated
        // (regular messages). Byte-wise scan so progress shows live.
        let mut buf = Vec::new();
        while let Some(frame) = next_frame(&mut stderr, &mut buf) {
            let line = frame.trim();
            if line.is_empty() {
                continue;
            }
            // Forward everything — every step should be visible.
            let _ = tx.send(TaskEvent::Log(format!("  {}", truncate(line, 60))));
            if line.contains("fatal") || line.contains("error") {
                last_error = Some(line.to_string());
            }
        }
    }
    match child.wait() {
        Ok(s) if s.success() => Ok(()),
        Ok(_) => Err(last_error.unwrap_or_else(|| "git exited with an error".into())),
        Err(_) => Err("git was terminated".into()),
    }
}

/// Pull the next `\r`/`\n`-delimited frame from a stream. `buf` carries the
/// unconsumed bytes between calls; returns `None` on EOF with no partial
/// frame left.
fn next_frame<R: std::io::Read>(r: &mut R, buf: &mut Vec<u8>) -> Option<String> {
    loop {
        while let Some(pos) = buf.iter().position(|&b| b == b'\n' || b == b'\r') {
            let seg: Vec<u8> = buf.drain(..pos).collect();
            buf.remove(0); // drop the delimiter itself
            let s = String::from_utf8_lossy(&seg).to_string();
            if !s.trim().is_empty() {
                return Some(s);
            }
        }
        let mut chunk = [0u8; 1024];
        match r.read(&mut chunk) {
            Ok(0) => {
                // EOF — flush any trailing partial frame.
                if buf.is_empty() {
                    return None;
                }
                let s = String::from_utf8_lossy(buf).to_string();
                buf.clear();
                return Some(s);
            }
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
            Err(_) => return None,
        }
    }
}

/// Where an installed plugin lives.
fn plugin_install_dir(full_name: &str) -> PathBuf {
    let repo = full_name.rsplit('/').next().unwrap_or(full_name);
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("cordanui")
        .join("plugins")
        .join(repo)
}

/// `gh search repos <terms> --limit 5 --json …`
fn search_repos(terms: &str) -> SearchOutcome {
    let output = Command::new("gh")
        .args([
            "search",
            "repos",
            terms,
            "--limit",
            "5",
            "--json",
            "fullName,description,stargazersCount",
        ])
        .output();

    let Ok(output) = output else {
        return SearchOutcome::Error(
            "gh CLI not found — install the GitHub CLI to search repos".into(),
        );
    };

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return SearchOutcome::Error(format!("gh search failed: {}", truncate(&stderr, 80)));
    }

    match serde_json::from_slice::<Vec<GhSearchRepo>>(&output.stdout) {
        Ok(repos) if repos.is_empty() => SearchOutcome::NotFound(terms.to_string()),
        Ok(repos) => SearchOutcome::Results(
            repos
                .into_iter()
                .map(|r| RepoInfo {
                    full_name: r.full_name,
                    description: r.description.filter(|d| !d.is_empty()),
                    stars: r.stargazers_count,
                })
                .collect(),
        ),
        Err(e) => SearchOutcome::Error(format!("unexpected gh output: {e}")),
    }
}

/// Extract `owner/repo` from any of:
///   https://github.com/<o>/<r>[/…]   http variant
///   github.com/<o>/<r>[/…]
///   <o>/<r>
/// Returns `None` when the input isn't repo-shaped (→ treated as a search).
fn extract_owner_repo(q: &str) -> Option<String> {
    let q = q.trim();

    // Strip scheme + optional www., then require the host to be github.
    let after_host = if let Some(rest) = q
        .strip_prefix("https://")
        .or_else(|| q.strip_prefix("http://"))
    {
        let rest = rest.strip_prefix("www.").unwrap_or(rest);
        let (host, tail) = rest.split_once('/')?;
        if !host.eq_ignore_ascii_case("github.com") {
            return None;
        }
        tail
    } else if let Some(rest) = q.strip_prefix("github.com/") {
        rest
    } else {
        // No host part — must be a bare `owner/repo`.
        q
    };

    let mut segs = after_host.split('/');
    let owner = segs.next()?.trim();
    let repo = segs.next()?.trim();
    if owner.is_empty() || repo.is_empty() {
        return None;
    }
    // Trailing segments (/tree/main, .git suffix) are tolerated and dropped.
    let repo = repo.trim_end_matches(".git");
    if repo.is_empty() {
        return None;
    }
    Some(format!("{owner}/{repo}"))
}

fn truncate(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    let cut: String = s.chars().take(max_chars.saturating_sub(1)).collect();
    format!("{cut}…")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routes_exact_vs_search() {
        // GitHub URLs and bare slugs resolve to owner/repo…
        assert_eq!(
            extract_owner_repo("https://github.com/Arjith8/rosepine"),
            Some("Arjith8/rosepine".into())
        );
        assert_eq!(
            extract_owner_repo("http://www.github.com/o/r/tree/main"),
            Some("o/r".into())
        );
        assert_eq!(extract_owner_repo("github.com/o/r.git"), Some("o/r".into()));
        assert_eq!(extract_owner_repo("o/r"), Some("o/r".into()));
        assert_eq!(extract_owner_repo("  o/r  "), Some("o/r".into()));
        // …non-GitHub hosts, bare words, and junk fall back to search.
        assert_eq!(extract_owner_repo("https://gitlab.com/o/r"), None);
        assert_eq!(extract_owner_repo("just terms"), None);
        assert_eq!(extract_owner_repo("github.com"), None);
        assert_eq!(extract_owner_repo("github.com/onlyowner"), None);
    }

    #[test]
    fn parses_search_json_shape() {
        let raw = br#"[{"fullName":"foo/bar","description":"hi","stargazersCount":12}]"#;
        let repos: Vec<GhSearchRepo> = serde_json::from_slice(raw).unwrap();
        assert_eq!(repos[0].full_name, "foo/bar");
        assert_eq!(repos[0].stargazers_count, 12);
    }

    #[test]
    fn summaries_truncate() {
        let r = RepoInfo {
            full_name: "foo/bar".into(),
            description: Some("a very long description that keeps going way past the limit!!".into()),
            stars: 3,
        };
        let s = r.summary();
        assert!(s.contains('…'));
        assert!(s.starts_with("   3★"));
    }

    #[test]
    fn install_dir_uses_repo_name() {
        let d = plugin_install_dir("Arjith8/rosepine");
        assert!(d.ends_with("cordanui/plugins/rosepine"));
    }

    #[test]
    fn scans_theme_files_and_skips_junk() {
        let dir = std::env::temp_dir().join(format!("cordanui-theme-scan-{}", cordanui_schema::new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("my-theme.json"),
            r##"{"id":"my-theme","name":"My Theme","colors":{"bg":"#000000","accent":"#ff0000"}}"##,
        )
        .unwrap();
        std::fs::write(dir.join("not-a-theme.json"), r#"{"hello":"world"}"#).unwrap();
        std::fs::write(dir.join("broken.json"), "not json at all").unwrap();

        let themes = scan_theme_files(&dir);
        assert_eq!(themes.len(), 1);
        assert_eq!(themes[0].id, "my-theme");
        assert_eq!(themes[0].name, "My Theme");
        // colors_json is the serialized colors object
        assert!(themes[0].colors_json.contains("\"bg\":\"#000000\""));
    }
}
