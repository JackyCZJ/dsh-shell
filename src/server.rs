//! Launching and supervising the DSH web server.
//!
//! The shell does not reimplement the host. It starts `dsh web`, waits for the
//! line that carries the authenticated URL, and points the webview at it.
//!
//! That startup line is the whole contract:
//!
//! ```text
//! dsh web: http://127.0.0.1:4180/?token=...
//! ```
//!
//! The token is minted per process and exchanged for a signed cookie on first
//! load, so the shell must read the URL rather than guess a port.

use std::path::PathBuf;
use std::process::Stdio;

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};

/// A running `dsh web` server.
pub struct DshServer {
    pub child: Child,
    pub url: String,
}

impl DshServer {
    /// Stop the server.
    ///
    /// Killing the child is enough: the URL is process-scoped, so the next
    /// launch mints a fresh token and there is no stale state to clean up.
    pub async fn shutdown(mut self) {
        let _ = self.child.kill().await;
    }
}

/// Resolve the `dsh` launcher to an absolute path.
///
/// A GUI-launched app inherits launchd's PATH, which on macOS is the bare
/// `/usr/bin:/bin:/usr/sbin:/sbin`. A `dsh` installed through bun, nvm, Homebrew,
/// or a version manager is therefore invisible — the app would start and then
/// silently fail to find its host. So an explicit path is preferred, and the
/// usual install locations are probed before falling back to PATH.
pub fn resolve_launcher(configured: &str) -> String {
    // 1. An explicit override always wins.
    if let Ok(explicit) = std::env::var("DSH_BIN") {
        if !explicit.trim().is_empty() {
            return explicit;
        }
    }

    // 2. A launcher shipped inside the app bundle, if present. This is what a
    //    distributed build would carry so users need nothing preinstalled.
    if let Ok(exe) = std::env::current_exe() {
        if let Some(macos_dir) = exe.parent() {
            // Contents/MacOS/<exe> -> Contents/Resources/bin/dsh
            if let Some(contents) = macos_dir.parent() {
                let bundled = contents.join("Resources").join("bin").join("dsh");
                if bundled.is_file() {
                    return bundled.to_string_lossy().into_owned();
                }
            }
        }
    }

    // 3. Well-known install locations.
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        for rel in [
            ".bun/bin/dsh",
            ".local/bin/dsh",
            ".volta/bin/dsh",
            "node_modules/.bin/dsh",
            ".npm-global/bin/dsh",
        ] {
            let candidate = home.join(rel);
            if candidate.is_file() {
                return candidate.to_string_lossy().into_owned();
            }
        }
        // 4. npm global prefixes under common version managers.
        for rel in [".nvm/versions/node", ".fnm/node-versions"] {
            if let Some(found) = find_dsh_under(&home.join(rel)) {
                return found;
            }
        }
    }

    // 5. Fall back to PATH, which works when launched from a terminal.
    configured.to_string()
}

/// Ensure the child can find a `node` interpreter.
///
/// `dsh` is a `#!/usr/bin/env node` script, so it needs `node` on PATH. A
/// GUI-launched app inherits launchd's bare PATH, where no version-manager node
/// exists — the launcher is found but immediately exits. Prepending the
/// launcher's own directory (and any node found nearby) is what makes a
/// Finder double-click work.
///
/// Public because anything that runs the same launcher needs it, not just the
/// host: the updater's `dsh --version` probe failed with exactly this exit 127
/// until it was given the same treatment.
pub fn augment_path_for(cmd: &mut Command, launcher: &str) {
    cmd.env("PATH", path_with_node(launcher));
}

/// The same PATH, for a synchronous `std::process::Command`.
///
/// As [`augment_path_for`], for the updater's probes.
pub fn augment_path_for_std(cmd: &mut std::process::Command, launcher: &str) {
    cmd.env("PATH", path_with_node(launcher));
}

/// PATH with the launcher's directory and any nearby `node` prepended.
pub fn path_with_node(launcher: &str) -> String {
    path_with_node_list(launcher, std::env::var("PATH").unwrap_or_default(), which_node())
}

/// As [`path_with_node`], with the environment supplied by the caller.
///
/// Pure on purpose: it touches nothing global. Tests share one process across
/// parallel threads, so a version that read the environment could not be tested
/// without rewriting state that every other test depends on.
pub fn path_with_node_list(launcher: &str, existing: String, node: Option<PathBuf>) -> String {
    let mut prepend: Vec<PathBuf> = Vec::new();

    // The directory holding the launcher usually holds `node` too: bun, volta,
    // and npm global prefixes all place both side by side.
    if let Some(dir) = std::path::Path::new(launcher).parent() {
        prepend.push(dir.to_path_buf());
    }

    // `node` may also live elsewhere; add its directory so `env node` resolves.
    if let Some(node) = node {
        if let Some(dir) = node.parent() {
            prepend.push(dir.to_path_buf());
        }
    }

    let mut parts: Vec<String> = prepend
        .into_iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect();
    parts.push(existing);
    // Filter empties so a leading colon cannot inject the cwd.
    parts
        .into_iter()
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join(":")
}

/// Find a `node` binary in the usual places, or on PATH.
pub fn which_node() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from)?;
    for rel in [
        ".local/bin/node",
        ".bun/bin/node",
        ".volta/bin/node",
        ".hermes/node/bin/node",
    ] {
        let candidate = home.join(rel);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    for rel in [".nvm/versions/node", ".fnm/node-versions"] {
        let root = home.join(rel);
        if let Ok(entries) = std::fs::read_dir(&root) {
            for entry in entries.flatten() {
                let candidate = entry.path().join("bin").join("node");
                if candidate.is_file() {
                    return Some(candidate);
                }
            }
        }
    }
    // Finally PATH, which is what a terminal-launched run has and a
    // Finder-launched one does not.
    let probe = std::process::Command::new("node")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
    if matches!(probe, Ok(status) if status.success()) {
        return Some(PathBuf::from("node"));
    }
    None
}

/// Search one level below a version-manager directory for a `bin/dsh`.
fn find_dsh_under(root: &std::path::Path) -> Option<String> {
    let entries = std::fs::read_dir(root).ok()?;
    for entry in entries.flatten() {
        let candidate = entry.path().join("bin").join("dsh");
        if candidate.is_file() {
            return Some(candidate.to_string_lossy().into_owned());
        }
    }
    None
}

/// Spawn `dsh web` and resolve its authenticated URL.
///
/// `dsh_program` is the *resolved* launcher path; `port` of 0 lets the OS choose,
/// which avoids collisions with an already-running GUI.
///
/// The caller resolves it through [`resolve_launcher`] and hands the result to
/// both this function and the updater, so the tree the shell runs and the tree
/// an upgrade replaces are guaranteed to be the same one.
pub async fn start(dsh_program: String, port: u16) -> Result<DshServer, String> {
    let launcher = dsh_program;
    tracing::info!(launcher = %launcher, "starting host");

    let mut command = Command::new(&launcher);
    augment_path_for(&mut command, &launcher);

    let mut child = command
        .arg("web")
        .arg("--no-open") // The shell owns the window; do not also open a browser.
        .arg("--port")
        .arg(port.to_string())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|err| format!("could not start `{launcher} web`: {err}"))?;

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "no stdout from dsh web".to_string())?;

    // Surface stderr in our own log so a host-side failure is visible rather
    // than swallowed.
    if let Some(stderr) = child.stderr.take() {
        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                tracing::debug!(target: "dsh", "{line}");
            }
        });
    }

    let mut lines = BufReader::new(stdout).lines();
    let deadline = std::time::Duration::from_secs(120);
    let started = std::time::Instant::now();

    while started.elapsed() < deadline {
        let line = match tokio::time::timeout(
            std::time::Duration::from_secs(120) - started.elapsed(),
            lines.next_line(),
        )
        .await
        {
            Ok(Ok(Some(line))) => line,
            Ok(Ok(None)) => return Err("dsh web exited before printing a URL".into()),
            Ok(Err(err)) => return Err(format!("reading dsh web output: {err}")),
            Err(_) => break,
        };

        if let Some(url) = extract_url(&line) {
            tracing::info!(%url, "dsh web ready");
            // Keep draining stdout for the life of the host. Abandoning the
            // reader would let the pipe buffer fill, and the host would block
            // forever on its next write — which is what a plugin logging to
            // stdout does. The lines are only useful as diagnostics.
            tokio::spawn(async move {
                while let Ok(Some(line)) = lines.next_line().await {
                    tracing::debug!(target: "dsh", "{line}");
                }
            });
            return Ok(DshServer { child, url });
        }
    }

    let _ = child.kill().await;
    Err(format!(
        "dsh web did not print a URL within {}s",
        deadline.as_secs()
    ))
}

/// Pull the authenticated URL out of a startup line.
///
/// Kept separate from the read loop so it can be tested against the exact
/// strings the host prints.
pub fn extract_url(line: &str) -> Option<String> {
    let marker = "dsh web:";
    let idx = line.find(marker)?;
    let rest = line[idx + marker.len()..].trim();
    let url = rest.split_whitespace().next()?;
    if url.starts_with("http://") || url.starts_with("https://") {
        Some(url.to_string())
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_the_startup_url() {
        let line = "dsh web: http://127.0.0.1:4180/?token=abc123";
        assert_eq!(
            extract_url(line).as_deref(),
            Some("http://127.0.0.1:4180/?token=abc123")
        );
    }

    #[test]
    fn tolerates_noise_and_trailing_text() {
        assert_eq!(
            extract_url("2026-01-01 INFO dsh web: http://127.0.0.1:4180/?token=x (ready)").as_deref(),
            Some("http://127.0.0.1:4180/?token=x")
        );
    }

    #[test]
    fn resolves_a_launcher_even_with_an_empty_path() {
        // This is the Finder-launch case: launchd gives the app the bare
        // /usr/bin:/bin:/usr/sbin:/sbin, where `dsh` does not exist. Resolution
        // must still find it via a well-known home-directory location.
        let saved = std::env::var("DSH_BIN").ok();
        // SAFETY: single-threaded test process setup.
        unsafe { std::env::remove_var("DSH_BIN") };

        let resolved = resolve_launcher("dsh");
        assert!(
            resolved != "dsh",
            "expected an absolute path, got the bare name back"
        );
        assert!(
            std::path::Path::new(&resolved).is_file(),
            "resolved launcher does not exist: {resolved}"
        );

        if let Some(saved) = saved {
            unsafe { std::env::set_var("DSH_BIN", saved) };
        }
    }

    #[test]
    fn explicit_override_wins() {
        let saved = std::env::var("DSH_BIN").ok();
        unsafe { std::env::set_var("DSH_BIN", "/custom/path/to/dsh") };
        assert_eq!(resolve_launcher("dsh"), "/custom/path/to/dsh");
        match saved {
            Some(v) => unsafe { std::env::set_var("DSH_BIN", v) },
            None => unsafe { std::env::remove_var("DSH_BIN") },
        }
    }

    /// Reading the URL must not end stdout consumption.
    ///
    /// The reader is moved into a task rather than dropped, because a host that
    /// keeps writing (a plugin logging, for instance) would otherwise block once
    /// the pipe buffer filled. This test pins the *intent*; the compile-time
    /// guarantee is that `lines` is moved rather than dropped.
    #[test]
    fn the_url_line_is_recognised_before_any_other_output() {
        // Whatever else the host prints, the URL is what starts the shell.
        assert!(extract_url("dsh web: http://127.0.0.1:1/?token=a").is_some());
        // Plugin chatter must not be mistaken for it.
        assert!(extract_url("[dsh-plugin-shell-bridge] connected to shell").is_none());
    }

    #[test]
    fn ignores_unrelated_lines() {
        assert!(extract_url("just a log line").is_none());
        assert!(extract_url("dsh web: not-a-url").is_none());
    }
}

/// Compile a generated script with `node --check`.
///
/// Every page the shell builds embeds hand-written JavaScript inside a Rust
/// string literal, where the compiler cannot see a syntax error. This is the
/// only thing that can, and it runs on the real generated text rather than on
/// a copy of it.
///
/// Skipped rather than failed when no node is available: the check is a safety
/// net, not a dependency of the build.
#[cfg(test)]
pub(crate) fn assert_js_parses(script: &str, label: &str) {
    let Some(node) = which_node() else {
        eprintln!("no node found; skipping the parse check for {label}");
        return;
    };

    // Unique per call, not per process: the tests run in parallel threads of one
    // process, and a shared path meant two of them wrote this file at once — so
    // `node --check` sometimes parsed a half-written script and reported a
    // syntax error that was really a race.
    use std::sync::atomic::{AtomicU32, Ordering};
    static SEQ: AtomicU32 = AtomicU32::new(0);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("dsh-script-check-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    let path = dir.join(format!("{label}-{seq}.js"));
    std::fs::write(&path, script).expect("write the extracted script");

    let output = std::process::Command::new(&node)
        .arg("--check")
        .arg(&path)
        .output();
    let _ = std::fs::remove_file(&path);

    match output {
        Ok(output) => assert!(
            output.status.success(),
            "the {label} script does not parse:\n{}",
            String::from_utf8_lossy(&output.stderr)
        ),
        // Raced with something that removed it; not this test's business.
        Err(_) => eprintln!("could not run {}; skipping", node.display()),
    }
}
