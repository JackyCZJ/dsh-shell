//! The shell's side of the plugin bridge.
//!
//! A Unix domain socket is listened on and newline-delimited JSON events are
//! read from the `dsh-plugin-shell-bridge` host plugin. The socket is the same
//! path the plugin resolves, so neither side needs configuration.
//!
//! This is deliberately a *pull* surface: the shell reads whatever arrives. If
//! DSH is not running, or the plugin is not installed, nothing happens and the
//! shell works exactly as before.

use std::io::{BufRead, BufReader};
use std::path::PathBuf;

use serde::Deserialize;

/// One event from the host plugin.
///
/// Unknown `kind` values are tolerated rather than rejected, so a newer plugin
/// can add events without breaking an older shell.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BridgeEvent {
    pub kind: String,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default)]
    pub reason: Option<String>,
}

/// Follow the host's snake_case field names.
impl BridgeEvent {
    /// A short label for tray tooltips and notification titles.
    pub fn summary(&self) -> String {
        match self.kind.as_str() {
            "status" => format!("status: {}", self.status.as_deref().unwrap_or("unknown")),
            "turn-stopping" => "turn finished".to_string(),
            "request-error" => "request failed".to_string(),
            "created" => "agent created".to_string(),
            "disposed" => "agent disposed".to_string(),
            other => other.to_string(),
        }
    }

    /// Whether this event warrants a desktop notification.
    ///
    /// Deliberately narrow: notifying on every status change would be noise,
    /// and the two cases a user actually wants to be interrupted for are a
    /// finished turn and a failure.
    pub fn wants_notification(&self) -> bool {
        matches!(self.kind.as_str(), "turn-stopping" | "request-error")
    }
}

/// Resolve the socket path, matching the plugin's own resolution.
pub fn socket_path() -> PathBuf {
    if let Ok(explicit) = std::env::var("DSH_SHELL_SOCKET") {
        return PathBuf::from(explicit);
    }
    let run_dir = std::env::var("XDG_RUNTIME_DIR")
        .ok()
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    // SAFETY: getuid cannot fail.
    let uid = unsafe { libc_getuid() };
    run_dir.join(format!("dsh-shell-{uid}.sock"))
}

#[cfg(unix)]
unsafe fn libc_getuid() -> u32 {
    unsafe extern "C" {
        fn getuid() -> u32;
    }
    unsafe { getuid() }
}

#[cfg(not(unix))]
unsafe fn libc_getuid() -> u32 {
    0
}

/// Parse one newline-delimited JSON line.
///
/// Separated from the socket loop so the wire format is testable.
pub fn parse_event(line: &str) -> Option<BridgeEvent> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }
    serde_json::from_str(trimmed).ok()
}

/// Listen for bridge events, invoking `on_event` for each one.
///
/// Runs until the process exits. A stale socket file from a previous crash is
/// removed before binding, otherwise the bind would fail forever.
pub fn listen<F>(on_event: F) -> std::io::Result<()>
where
    F: Fn(BridgeEvent) + Send + Clone + 'static,
{
    let path = socket_path();

    // A leftover socket from an unclean exit would make bind fail. Removing it
    // is safe: if another live process owned it, that process is gone, because
    // the shell holds exactly one listener per user.
    if path.exists() {
        let _ = std::fs::remove_file(&path);
    }
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    let listener = std::os::unix::net::UnixListener::bind(&path)?;
    tracing::info!(socket = %path.display(), "bridge listening");

    std::thread::Builder::new()
        .name("bridge-listen".into())
        .spawn(move || {
            for stream in listener.incoming() {
                let stream = match stream {
                    Ok(stream) => stream,
                    Err(err) => {
                        tracing::debug!(%err, "bridge accept failed");
                        continue;
                    }
                };
                let on_event = on_event.clone();
                // One thread per connection: the plugin is a single client, so
                // this is bounded in practice and keeps reads simple.
                std::thread::spawn(move || {
                    let reader = BufReader::new(stream);
                    for line in reader.lines() {
                        match line {
                            Ok(line) => {
                                if let Some(event) = parse_event(&line) {
                                    on_event(event);
                                }
                            }
                            Err(err) => {
                                tracing::debug!(%err, "bridge read ended");
                                break;
                            }
                        }
                    }
                });
            }
        })?;

    Ok(())
}

/// Remove the socket file, so a clean exit leaves nothing behind.
pub fn cleanup() {
    let path = socket_path();
    if path.exists() {
        let _ = std::fs::remove_file(&path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_status_event() {
        let event = parse_event(r#"{"kind":"status","status":"working","at":1}"#).expect("parse");
        assert_eq!(event.kind, "status");
        assert_eq!(event.status.as_deref(), Some("working"));
    }

    #[test]
    fn parses_snake_case_session_id() {
        // The plugin sends `sessionId`; serde must map it to `session_id`.
        let event = parse_event(r#"{"kind":"created","sessionId":"abc"}"#).expect("parse");
        assert_eq!(event.session_id.as_deref(), Some("abc"));
    }

    #[test]
    fn tolerates_unknown_kinds_and_empty_lines() {
        assert!(parse_event(r#"{"kind":"something-new"}"#).is_some());
        assert!(parse_event("").is_none());
        assert!(parse_event("not json").is_none());
    }

    #[test]
    fn only_interrupting_events_notify() {
        let notify = |kind: &str| {
            parse_event(&format!(r#"{{"kind":"{kind}"}}"#))
                .unwrap()
                .wants_notification()
        };
        assert!(notify("turn-stopping"));
        assert!(notify("request-error"));
        // Status changes are frequent; notifying on them would be noise.
        assert!(!notify("status"));
        assert!(!notify("created"));
    }

    #[test]
    fn socket_path_is_short_enough_for_unix_sockets() {
        // macOS caps socket paths near 104 bytes; a long home dir must not
        // silently produce an unusable path.
        let path = socket_path();
        assert!(
            path.as_os_str().len() < 100,
            "socket path too long: {}",
            path.display()
        );
    }
}
