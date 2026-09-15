//! The shell's side of the plugin bridge.
//!
//! A Unix domain socket is listened on and newline-delimited JSON events are
//! read from the `dsh-plugin-shell-bridge` host plugin. The socket is the same
//! path the plugin resolves, so neither side needs configuration.
//!
//! This is deliberately a *pull* surface: the shell reads whatever arrives. If
//! DSH is not running, or the plugin is not installed, nothing happens and the
//! shell works exactly as before.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

/// One request from a plugin to the shell.
///
/// Separate from `BridgeEvent` because the direction differs: events are the
/// shell observing DSH, while these are a plugin asking the shell to do
/// something. Keeping them apart means a plugin cannot spoof an agent event
/// through the request path, and vice versa.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", tag = "method")]
pub enum ShellRequest {
    /// Post a desktop notification.
    Notify {
        #[serde(default)]
        title: Option<String>,
        body: String,
    },
    /// Bring the window to the front.
    FocusWindow,
    /// Hide the window to the tray.
    HideWindow,
    /// Set a short label shown in the tray tooltip.
    ///
    /// Deliberately not "set the tray colour": the icon encodes agent state,
    /// which the shell derives from real events. A plugin overriding that would
    /// make the tray lie.
    SetStatusLabel {
        #[serde(default)]
        text: Option<String>,
    },
}

/// A reply to a `ShellRequest`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ShellReply {
    pub id: String,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl ShellReply {
    pub fn ok(id: impl Into<String>) -> Self {
        ShellReply {
            id: id.into(),
            ok: true,
            error: None,
        }
    }

    pub fn err(id: impl Into<String>, error: impl Into<String>) -> Self {
        ShellReply {
            id: id.into(),
            ok: false,
            error: Some(error.into()),
        }
    }

    /// The wire form: one JSON object per line.
    pub fn to_line(&self) -> String {
        let mut line = serde_json::to_string(self).unwrap_or_else(|_| "{}".into());
        line.push('\n');
        line
    }
}

/// A line arriving from a plugin, which may be either an event or a request.
///
/// The two are distinguished by shape: requests carry `method` and `id`, events
/// carry `kind`. Parsing tries requests first so a malformed request is reported
/// rather than silently read as an unknown event.
#[derive(Debug, Clone)]
pub enum Incoming {
    Event(BridgeEvent),
    Request { id: String, request: ShellRequest },
    /// The plugin's answer to a request the shell made.
    Reply {
        id: String,
        ok: bool,
        error: Option<String>,
    },
    /// A line that looked like a request but could not be parsed.
    ///
    /// Carries the id when one was present so the caller can be told its request
    /// was invalid. Without this the plugin would wait out its full timeout with
    /// no explanation.
    BadRequest { id: Option<String>, error: String },
}

/// Parse one line into either an event or a request.
pub fn parse_incoming(line: &str) -> Option<Incoming> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }

    #[derive(Deserialize)]
    struct Envelope {
        #[serde(default)]
        id: Option<String>,
        #[serde(default)]
        method: Option<String>,
        #[serde(default)]
        ok: Option<bool>,
    }

    if let Ok(envelope) = serde_json::from_str::<Envelope>(trimmed) {
        // A reply carries `ok` and an id but no method: it answers a request the
        // shell made rather than starting one.
        if envelope.method.is_none() {
            if let (Some(id), Some(ok)) = (envelope.id.clone(), envelope.ok) {
                #[derive(Deserialize)]
                struct ReplyBody {
                    #[serde(default)]
                    error: Option<String>,
                }
                let error = serde_json::from_str::<ReplyBody>(trimmed)
                    .ok()
                    .and_then(|b| b.error);
                return Some(Incoming::Reply { id, ok, error });
            }
        }

        if let Some(method) = envelope.method {
            return match serde_json::from_str::<ShellRequest>(trimmed) {
                Ok(request) => Some(Incoming::Request {
                    id: envelope.id.unwrap_or_default(),
                    request,
                }),
                Err(err) => {
                    tracing::warn!(%err, %method, "malformed shell request");
                    Some(Incoming::BadRequest {
                        id: envelope.id,
                        error: err.to_string(),
                    })
                }
            };
        }
    }

    parse_event(trimmed).map(Incoming::Event)
}

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
    /// Present on a `settings` event: the resolved `dsh-shell` section DSH just
    /// persisted. Carried as raw JSON so this crate does not have to know the
    /// settings shape; the theme module parses it.
    #[serde(default)]
    pub config: Option<serde_json::Value>,
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

    /// Present the pushed settings as a theme, if this is a settings event.
    pub fn as_theme(&self) -> Option<crate::theme::Theme> {
        if self.kind != "settings" {
            return None;
        }
        let config = self.config.as_ref()?;
        serde_json::from_value(config.clone()).ok()
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

/// A handle for sending requests to the connected Host plugin.
///
/// The plugin dials the shell, so the shell writes back on the same connection.
/// One connection is expected at a time; a reconnect replaces it.
#[derive(Clone, Default)]
pub struct PluginLink {
    inner: Arc<Mutex<PluginLinkInner>>,
}

#[derive(Default)]
struct PluginLinkInner {
    stream: Option<std::os::unix::net::UnixStream>,
    /// Requests awaiting a reply, keyed by id.
    pending: HashMap<String, std::sync::mpsc::Sender<(bool, Option<String>)>>,
    next_id: u64,
}

impl PluginLink {
    /// Whether a plugin is currently connected.
    pub fn is_connected(&self) -> bool {
        self.inner.lock().unwrap().stream.is_some()
    }

    /// Send a request and block until the plugin answers, or the timeout expires.
    ///
    /// Blocking is acceptable only because callers run this off the UI thread;
    /// the settings window does so on a worker.
    pub fn call(
        &self,
        method: &str,
        params: serde_json::Value,
        timeout: std::time::Duration,
    ) -> Result<(bool, Option<String>), String> {
        let (id, rx) = {
            let mut inner = self.inner.lock().unwrap();

            inner.next_id += 1;
            let id = format!("shell-{}", inner.next_id);

            if inner.stream.is_none() {
                return Err("the DSH host is not connected".into());
            }

            let (tx, rx) = std::sync::mpsc::channel();
            inner.pending.insert(id.clone(), tx);

            let stream = inner
                .stream
                .as_mut()
                .ok_or_else(|| "the DSH host is not connected".to_string())?;

            let mut message = serde_json::Map::new();
            message.insert("id".into(), serde_json::Value::String(id.clone()));
            message.insert("method".into(), serde_json::Value::String(method.into()));
            if let serde_json::Value::Object(fields) = params {
                for (k, v) in fields {
                    message.insert(k, v);
                }
            }
            let mut line = serde_json::to_string(&serde_json::Value::Object(message))
                .map_err(|err| format!("could not encode the request: {err}"))?;
            line.push('\n');

            if let Err(err) = stream.write_all(line.as_bytes()) {
                inner.pending.remove(&id);
                return Err(format!("could not reach the DSH host: {err}"));
            }
            (id, rx)
        };

        match rx.recv_timeout(timeout) {
            Ok(outcome) => Ok(outcome),
            Err(_) => {
                // Drop the waiter so a late reply is ignored rather than leaking.
                self.inner.lock().unwrap().pending.remove(&id);
                Err("the DSH host did not answer in time".into())
            }
        }
    }

    /// Resolve a waiter from a reply line.
    fn settle(&self, id: &str, ok: bool, error: Option<String>) {
        let waiter = self.inner.lock().unwrap().pending.remove(id);
        if let Some(tx) = waiter {
            let _ = tx.send((ok, error));
        }
    }

    /// Clear the connection after a drop, failing any outstanding calls.
    fn disconnect(&self) {
        let mut inner = self.inner.lock().unwrap();
        inner.stream = None;
        inner.pending.clear();
    }
}

/// Listen for bridge traffic, invoking `on_event` for events and `on_request`
/// for requests.
///
/// A stale socket file from a previous crash is removed before binding,
/// otherwise the bind would fail forever.
///
/// Each connection is served on its own thread. Requests are answered on the
/// asking connection, so a reply always reaches the plugin that made the call.
pub fn listen<E, R>(link: PluginLink, on_event: E, on_request: R) -> std::io::Result<()>
where
    E: Fn(BridgeEvent) + Send + Clone + 'static,
    R: Fn(ShellRequest) -> Result<(), String> + Send + Clone + 'static,
{
    let path = crate::runtime::socket_path();

    // A leftover socket from an unclean exit would make bind fail, so it is
    // cleared first. That is safe because the single-instance lock is already
    // held: `crate::instance::claim` runs before this and guarantees no other
    // live shell owns the path, so the file can only be a dead process's
    // remains. Without that lock this removal would let a second shell steal
    // the first one's socket.
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
                let on_request = on_request.clone();
                let link = link.clone();
                std::thread::spawn(move || {
                    // Split so the reader can block on lines while the same
                    // thread writes replies back.
                    let writer = match stream.try_clone() {
                        Ok(writer) => writer,
                        Err(err) => {
                            tracing::debug!(%err, "bridge stream clone failed");
                            return;
                        }
                    };
                    // Publish this connection so the shell can send requests on
                    // it. A reconnect replaces whatever was there.
                    if let Ok(publish) = stream.try_clone() {
                        link.inner.lock().unwrap().stream = Some(publish);
                    }
                    let reader = BufReader::new(stream);
                    for line in reader.lines() {
                        let line = match line {
                            Ok(line) => line,
                            Err(err) => {
                                tracing::debug!(%err, "bridge read ended");
                                break;
                            }
                        };
                        match parse_incoming(&line) {
                            Some(Incoming::Event(event)) => on_event(event),
                            Some(Incoming::Reply { id, ok, error }) => {
                                link.settle(&id, ok, error);
                            }
                            Some(Incoming::Request { id, request }) => {
                                // An unknown id means the plugin sent no id;
                                // still answer so a caller waiting on a reply
                                // is not left hanging.
                                let reply = match on_request(request) {
                                    Ok(()) => ShellReply::ok(id),
                                    Err(err) => ShellReply::err(id, err),
                                };
                                let mut writer = &writer;
                                if let Err(err) = writer.write_all(reply.to_line().as_bytes()) {
                                    tracing::debug!(%err, "bridge reply failed");
                                    break;
                                }
                            }
                            Some(Incoming::BadRequest { id, error }) => {
                                // Only answer when the caller supplied an id to
                                // match on; otherwise there is nobody waiting.
                                if let Some(id) = id {
                                    let mut writer = &writer;
                                    let reply = ShellReply::err(id, format!("invalid request: {error}"));
                                    if let Err(err) = writer.write_all(reply.to_line().as_bytes()) {
                                        tracing::debug!(%err, "bridge reply failed");
                                        break;
                                    }
                                }
                            }
                            None => {}
                        }
                    }
                    link.disconnect();
                });
            }
        })?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_settings_event_carries_a_theme() {
        let line = r#"{"kind":"settings","config":{"hotkey":"meta+alt+K"}}"#;
        let event = parse_event(line).expect("parse");
        let theme = event.as_theme().expect("settings event must yield a theme");
        assert_eq!(theme.hotkey, "meta+alt+K");
        // Omitted fields fall back to defaults rather than failing.
        assert_eq!(theme.light, crate::theme::Palette::deepseek_light());
    }

    #[test]
    fn a_non_settings_event_yields_no_theme() {
        // Agent events must not be mistaken for configuration.
        let event = parse_event(r#"{"kind":"status","status":"working"}"#).unwrap();
        assert!(event.as_theme().is_none());
    }

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
    fn parses_a_notify_request() {
        let line = r#"{"id":"1","method":"notify","body":"hello","title":"DSH"}"#;
        match parse_incoming(line) {
            Some(Incoming::Request { id, request }) => {
                assert_eq!(id, "1");
                match request {
                    ShellRequest::Notify { title, body } => {
                        assert_eq!(title.as_deref(), Some("DSH"));
                        assert_eq!(body, "hello");
                    }
                    other => panic!("expected Notify, got {other:?}"),
                }
            }
            other => panic!("expected a request, got {other:?}"),
        }
    }

    #[test]
    fn title_is_optional_on_notify() {
        let line = r#"{"id":"1","method":"notify","body":"hello"}"#;
        assert!(matches!(
            parse_incoming(line),
            Some(Incoming::Request {
                request: ShellRequest::Notify { title: None, .. },
                ..
            })
        ));
    }

    #[test]
    fn an_event_is_not_mistaken_for_a_request() {
        // Events carry `kind` and no `method`, so they must route to the event
        // path — otherwise agent updates would be dropped as bad requests.
        match parse_incoming(r#"{"kind":"status","status":"working"}"#) {
            Some(Incoming::Event(event)) => assert_eq!(event.kind, "status"),
            other => panic!("expected an event, got {other:?}"),
        }
    }

    #[test]
    fn a_malformed_request_is_reported_back_not_read_as_an_event() {
        // `method` present but the payload is wrong: it must not silently become
        // an unknown event, and the caller must be told so it does not hang
        // until its timeout.
        match parse_incoming(r#"{"id":"7","method":"notify"}"#) {
            Some(Incoming::BadRequest { id, error }) => {
                assert_eq!(id.as_deref(), Some("7"));
                assert!(!error.is_empty(), "no explanation for the caller");
            }
            other => panic!("expected BadRequest, got {other:?}"),
        }

        // An unknown method is also reported rather than dropped.
        assert!(matches!(
            parse_incoming(r#"{"id":"8","method":"no-such-method"}"#),
            Some(Incoming::BadRequest { .. })
        ));
    }

    #[test]
    fn a_bad_request_without_an_id_is_still_not_an_event() {
        // Nothing to reply to, but it must not be misread as a bridge event.
        match parse_incoming(r#"{"method":"notify"}"#) {
            Some(Incoming::BadRequest { id, .. }) => assert!(id.is_none()),
            other => panic!("expected BadRequest, got {other:?}"),
        }
    }

    #[test]
    fn replies_serialise_as_one_line() {
        let line = ShellReply::ok("42").to_line();
        assert!(line.ends_with('\n'), "reply must be newline-terminated");
        assert_eq!(line.matches('\n').count(), 1, "reply must be a single line");
        let parsed: serde_json::Value = serde_json::from_str(line.trim()).expect("valid json");
        assert_eq!(parsed["id"], "42");
        assert_eq!(parsed["ok"], true);
        // A successful reply omits the error field entirely.
        assert!(parsed.get("error").is_none());

        let err = ShellReply::err("43", "nope").to_line();
        let parsed: serde_json::Value = serde_json::from_str(err.trim()).unwrap();
        assert_eq!(parsed["ok"], false);
        assert_eq!(parsed["error"], "nope");
    }

    #[test]
    fn socket_path_is_short_enough_for_unix_sockets() {
        // macOS caps socket paths near 104 bytes; a long home dir must not
        // silently produce an unusable path.
        let path = crate::runtime::socket_path();
        assert!(
            path.as_os_str().len() < 100,
            "socket path too long: {}",
            path.display()
        );
    }
}
