//! Native desktop affordances: tray icon, notifications, global hotkey.
//!
//! Each is optional and independent. A failure to create one is logged and the
//! shell keeps running: losing the tray must not cost you the window.
//!
//! All three must be created on the main thread, which is why they are
//! initialized from `main` rather than from a background worker.

use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

/// Agent state as reflected by the tray icon.
///
/// Kept to what the bridge actually reports, rather than inventing states the
/// host never emits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentState {
    /// No event seen yet, or the agent is present but idle.
    Idle,
    /// A turn is in flight.
    Working,
    /// The last turn failed.
    Failed,
}

impl AgentState {
    /// Map a bridge event onto a tray state.
    pub fn from_event(kind: &str, status: Option<&str>) -> Option<AgentState> {
        match kind {
            "status" => Some(match status.unwrap_or("") {
                "working" | "running" | "busy" | "streaming" => AgentState::Working,
                "failed" | "error" => AgentState::Failed,
                _ => AgentState::Idle,
            }),
            // A turn ending or failing both return the agent to rest, but the
            // failure stays visible until the next turn starts.
            "turn-stopping" => Some(AgentState::Idle),
            "request-error" => Some(AgentState::Failed),
            "created" => Some(AgentState::Idle),
            _ => None,
        }
    }

    fn color(self) -> [u8; 4] {
        match self {
            AgentState::Idle => [0x9a, 0x9d, 0xb0, 0xff],
            AgentState::Working => [0x4c, 0x7d, 0xff, 0xff],
            AgentState::Failed => [0xe0, 0x55, 0x61, 0xff],
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            AgentState::Idle => "idle",
            AgentState::Working => "working",
            AgentState::Failed => "failed",
        }
    }
}

/// Owns the tray icon and its menu.
pub struct Tray {
    _icon: TrayIcon,
    state_item: MenuItem,
    show_item: MenuItem,
    quit_item: MenuItem,
    pub state: AgentState,
}

impl Tray {
    /// Create the tray icon.
    ///
    /// `on_show` and `on_quit` are invoked from the tray's own event thread;
    /// the menu ids below are what the caller matches on.
    pub fn new() -> Result<Tray, String> {
        let menu = Menu::new();
        let state_item = MenuItem::new("Agent: idle", false, None);
        let show_item = MenuItem::new("Show Window", true, None);
        let quit_item = MenuItem::new("Quit", true, None);

        menu.append_items(&[
            &state_item,
            &PredefinedMenuItem::separator(),
            &show_item,
            &quit_item,
        ])
        .map_err(|err| format!("tray menu: {err}"))?;

        let icon = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_tooltip("DSH Shell")
            .with_icon(make_icon(AgentState::Idle))
            .build()
            .map_err(|err| format!("tray icon: {err}"))?;

        Ok(Tray {
            _icon: icon,
            state_item,
            show_item,
            quit_item,
            state: AgentState::Idle,
        })
    }

    /// Update the tray to reflect a new agent state.
    pub fn set_state(&mut self, state: AgentState) {
        if self.state == state {
            return;
        }
        self.state = state;
        self.state_item.set_text(format!("Agent: {}", state.label()));
        let _ = self._icon.set_icon(Some(make_icon(state)));
    }

    pub fn show_item_id(&self) -> &tray_icon::menu::MenuId {
        self.show_item.id()
    }

    pub fn quit_item_id(&self) -> &tray_icon::menu::MenuId {
        self.quit_item.id()
    }
}

/// Source geometry of the DeepSeek whale, as a 1-bit mask.
///
/// Embedded as the same path the app icon uses, rasterised at load time. Menu
/// bar icons need a template image — a monochrome mask the system tints to match
/// the bar — which is why the shape is baked to alpha rather than shipped as
/// colour art.
const WHALE_PATH: &str = include_str!("../assets/whale_path.txt");

/// Rasterise the whale at the given size, tinted for the agent state.
///
/// The mask is computed from the SVG path with a scanline fill, so the tray and
/// the Dock icon cannot drift apart.
fn make_icon(state: AgentState) -> Icon {
    const SIZE: u32 = 22;
    let [r, g, b, a] = state.color();
    let mask = whale_mask(SIZE);
    let mut rgba = Vec::with_capacity((SIZE * SIZE * 4) as usize);
    for value in mask {
        if value {
            rgba.extend_from_slice(&[r, g, b, a]);
        } else {
            rgba.extend_from_slice(&[0, 0, 0, 0]);
        }
    }
    Icon::from_rgba(rgba, SIZE, SIZE).expect("generated icon is valid")
}

/// Rasterise the whale silhouette into a boolean mask.
///
/// A point-in-polygon test on the path's outline. Doing this at runtime keeps
/// the tray in step with `assets/deepseek.svg` without a build-time rasteriser
/// or a committed PNG that could go stale.
fn whale_mask(size: u32) -> Vec<bool> {
    let polygons = whale_polygons();
    let mut mask = vec![false; (size * size) as usize];
    let scale = size as f32 / 50.0; // The SVG viewBox is 50x50.
    for y in 0..size {
        for x in 0..size {
            // Sample at pixel centres.
            let px = (x as f32 + 0.5) / scale;
            let py = (y as f32 + 0.5) / scale;
            if polygons.iter().any(|poly| point_in_polygon(px, py, poly)) {
                mask[(y * size + x) as usize] = true;
            }
        }
    }
    mask
}

/// The whale outline, as flattened polygons.
///
/// The path is built from cubic Béziers, so its raw coordinate list is control
/// points, not an outline — filling those directly would distort the silhouette.
/// Each curve is therefore subdivided into short line segments before filling.
fn whale_polygons() -> Vec<Vec<(f32, f32)>> {
    flatten_path(WHALE_PATH)
}

/// Number of segments per cubic curve.
///
/// At a 22px tray icon one SVG unit is well under a pixel, so 12 segments per
/// curve is comfortably past the point where more would be visible.
const CURVE_SEGMENTS: usize = 12;

/// Flatten an SVG path of absolute `M`/`L`/`C`/`Z` commands into polygons.
fn flatten_path(path: &str) -> Vec<Vec<(f32, f32)>> {
    let mut polygons = Vec::new();
    let mut current: Vec<(f32, f32)> = Vec::new();
    let mut cursor = (0.0f32, 0.0f32);
    let mut start = (0.0f32, 0.0f32);

    for (command, args) in tokenize_path(path) {
        match command {
            'M' => {
                if current.len() >= 3 {
                    polygons.push(std::mem::take(&mut current));
                } else {
                    current.clear();
                }
                if args.len() >= 2 {
                    cursor = (args[0], args[1]);
                    start = cursor;
                    current.push(cursor);
                }
            }
            'L' => {
                let mut i = 0;
                while i + 1 < args.len() {
                    cursor = (args[i], args[i + 1]);
                    current.push(cursor);
                    i += 2;
                }
            }
            'C' => {
                let mut i = 0;
                while i + 5 < args.len() {
                    let c1 = (args[i], args[i + 1]);
                    let c2 = (args[i + 2], args[i + 3]);
                    let end = (args[i + 4], args[i + 5]);
                    for step in 1..=CURVE_SEGMENTS {
                        let t = step as f32 / CURVE_SEGMENTS as f32;
                        current.push(cubic_at(cursor, c1, c2, end, t));
                    }
                    cursor = end;
                    i += 6;
                }
            }
            'Z' => {
                if current.len() >= 3 {
                    polygons.push(std::mem::take(&mut current));
                } else {
                    current.clear();
                }
                cursor = start;
            }
            _ => {}
        }
    }
    if current.len() >= 3 {
        polygons.push(current);
    }
    polygons
}

/// Evaluate a cubic Bézier at `t`.
fn cubic_at(p0: (f32, f32), p1: (f32, f32), p2: (f32, f32), p3: (f32, f32), t: f32) -> (f32, f32) {
    let u = 1.0 - t;
    let (a, b, c, d) = (u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t);
    (
        a * p0.0 + b * p1.0 + c * p2.0 + d * p3.0,
        a * p0.1 + b * p1.1 + c * p2.1 + d * p3.1,
    )
}

/// Split a path into `(command, numbers)` pairs.
///
/// Each command letter opens a new group; every number after it belongs to that
/// group until the next letter. Getting this wrong makes the flattener read
/// control points as if they were endpoints, which silently produces an empty
/// or distorted silhouette.
fn tokenize_path(path: &str) -> Vec<(char, Vec<f32>)> {
    let mut groups: Vec<(char, Vec<f32>)> = Vec::new();
    let mut buf = String::new();
    let mut command: Option<char> = None;

    // Push the buffered number into the group for the *current* command.
    fn flush_number(
        buf: &mut String,
        command: Option<char>,
        groups: &mut Vec<(char, Vec<f32>)>,
    ) {
        if buf.is_empty() {
            return;
        }
        if let Ok(value) = buf.parse::<f32>() {
            if let Some(c) = command {
                match groups.last_mut() {
                    Some(last) if last.0 == c => last.1.push(value),
                    _ => groups.push((c, vec![value])),
                }
            }
        }
        buf.clear();
    }

    for ch in path.chars() {
        match ch {
            '0'..='9' | '.' => buf.push(ch),
            // A sign starts a new number, except inside an exponent.
            '-' | '+' => {
                if !buf.is_empty() && !buf.ends_with('e') && !buf.ends_with('E') {
                    flush_number(&mut buf, command, &mut groups);
                }
                buf.push(ch);
            }
            'e' | 'E' => buf.push(ch),
            _ if ch.is_ascii_alphabetic() => {
                // Close the previous number under the previous command, then
                // switch to the new one.
                flush_number(&mut buf, command, &mut groups);
                command = Some(ch.to_ascii_uppercase());
            }
            // Whitespace and commas separate numbers; without flushing here a
            // value like "48.8354 10.0479" would concatenate into an
            // unparseable token and be silently dropped.
            ' ' | '\t' | '\n' | '\r' | ',' => {
                flush_number(&mut buf, command, &mut groups);
            }
            _ => {} // Anything unrecognised.
        }
    }
    flush_number(&mut buf, command, &mut groups);
    groups
}

/// Even-odd point-in-polygon test.
fn point_in_polygon(x: f32, y: f32, poly: &[(f32, f32)]) -> bool {
    let mut inside = false;
    let mut j = poly.len().wrapping_sub(1);
    for i in 0..poly.len() {
        let (xi, yi) = poly[i];
        let (xj, yj) = poly[j];
        if (yi > y) != (yj > y) && x < (xj - xi) * (y - yi) / (yj - yi) + xi {
            inside = !inside;
        }
        j = i;
    }
    inside
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_status_strings_to_states() {
        assert_eq!(
            AgentState::from_event("status", Some("working")),
            Some(AgentState::Working)
        );
        assert_eq!(
            AgentState::from_event("status", Some("idle")),
            Some(AgentState::Idle)
        );
        // An unrecognised status must degrade to idle, not panic.
        assert_eq!(
            AgentState::from_event("status", Some("something-new")),
            Some(AgentState::Idle)
        );
        assert_eq!(AgentState::from_event("status", None), Some(AgentState::Idle));
    }

    #[test]
    fn error_state_persists_until_the_next_turn() {
        assert_eq!(
            AgentState::from_event("request-error", None),
            Some(AgentState::Failed)
        );
        assert_eq!(
            AgentState::from_event("turn-stopping", None),
            Some(AgentState::Idle)
        );
    }

    #[test]
    fn unrelated_events_do_not_change_the_tray() {
        assert_eq!(AgentState::from_event("disposed", None), None);
        assert_eq!(AgentState::from_event("unknown-kind", None), None);
    }

    #[test]
    fn reads_dsh_theme_preference() {
        // The real shape of a DSH settings document.
        let yaml = "ui-onboarding:\n  welcomeNoticeVersion: 1\nui-theme:\n  preference: light\npet:\n  visible: true\n";
        assert_eq!(parse_theme_preference(yaml), Some(false));

        let yaml_dark = "ui-theme:\n  preference: dark\n";
        assert_eq!(parse_theme_preference(yaml_dark), Some(true));
    }

    #[test]
    fn system_preference_defers_to_the_os() {
        // `system` must not be reported as an answer: the caller consults the OS.
        assert_eq!(
            parse_theme_preference("ui-theme:\n  preference: system\n"),
            None
        );
        // A missing key is likewise not an answer.
        assert_eq!(parse_theme_preference("ui-theme:\n  fontSize: 14\n"), None);
        assert_eq!(parse_theme_preference(""), None);
    }

    #[test]
    fn a_same_named_key_elsewhere_is_not_mistaken_for_the_theme() {
        // Only a `preference` directly under `ui-theme` may count.
        let yaml = "locale:\n  preference: dark\nui-theme:\n  fontSize: 14\n";
        assert_eq!(
            parse_theme_preference(yaml),
            None,
            "a preference under another top-level key must be ignored"
        );
    }

    #[test]
    fn commented_out_preferences_are_ignored() {
        let yaml = "ui-theme:\n  # preference: dark\n  preference: light\n";
        assert_eq!(parse_theme_preference(yaml), Some(false));
    }

    #[test]
    fn quoted_values_are_accepted() {
        assert_eq!(
            parse_theme_preference("ui-theme:\n  preference: \"dark\"\n"),
            Some(true)
        );
        assert_eq!(
            parse_theme_preference("ui-theme:\n  preference: 'light'\n"),
            Some(false)
        );
    }

    #[test]
    fn whale_path_flattens_into_usable_polygons() {
        let polys = whale_polygons();
        assert!(!polys.is_empty(), "path produced no geometry");
        // The whale is drawn as several subpaths.
        assert!(polys.len() >= 2, "expected multiple subpaths, got {}", polys.len());
        let total: usize = polys.iter().map(|p| p.len()).sum();
        // Bézier flattening must yield far more points than control points.
        assert!(total > 200, "too few flattened points: {total}");
    }

    #[test]
    fn flattened_points_stay_inside_the_viewbox() {
        // A parsing slip would send coordinates far outside 0..50 and the icon
        // would silently render as an empty square.
        for poly in whale_polygons() {
            for (x, y) in poly {
                assert!(
                    (-1.0..=51.0).contains(&x) && (-1.0..=51.0).contains(&y),
                    "point outside the viewBox: ({x}, {y})"
                );
            }
        }
    }

    #[test]
    fn the_whale_actually_covers_pixels() {
        // Guards the whole rasterisation chain: if the fill or the winding were
        // wrong, the mask would come back empty and the tray would show nothing.
        let mask = whale_mask(22);
        let filled = mask.iter().filter(|v| **v).count();
        let total = mask.len();
        assert!(filled > 0, "whale rendered as empty");
        // A whale is a solid silhouette: expect a meaningful fraction filled,
        // but not the entire square.
        assert!(
            filled > total / 20 && filled < total * 9 / 10,
            "implausible coverage: {filled}/{total}"
        );
    }

    #[test]
    fn generated_icon_has_the_expected_buffer() {
        let icon = make_icon(AgentState::Working);
        // Construction is the assertion: from_rgba rejects a wrong length.
        drop(icon);
    }

    #[test]
    fn the_summon_hotkey_does_not_collide_with_common_shortcuts() {
        use global_hotkey::hotkey::{Code, Modifiers};
        let hk = default_hotkey();
        assert_eq!(hk.key, Code::KeyD);
        // Cmd-D alone is a common app shortcut ("Don't Save"); requiring Shift
        // keeps the default out of the way.
        assert!(hk.mods.contains(Modifiers::SUPER));
        assert!(hk.mods.contains(Modifiers::SHIFT));
    }
}

/// Post a desktop notification.
///
/// Failures are logged, never returned: a missing notification daemon should
/// not disturb the shell.
pub fn notify(summary: &str, body: &str) {
    if let Err(err) = notify_rust::Notification::new()
        .summary(summary)
        .body(body)
        .appname("DSH Shell")
        .timeout(notify_rust::Timeout::Milliseconds(6000))
        .show()
    {
        tracing::debug!(%err, "notification failed");
    }
}

/// Menu events surfaced to the caller.
pub enum TrayCommand {
    Show,
    Quit,
}

/// Map a menu event id to a command, if it is one of ours.
pub fn tray_command(event: &MenuEvent, tray: &Tray) -> Option<TrayCommand> {
    if event.id == *tray.show_item_id() {
        Some(TrayCommand::Show)
    } else if event.id == *tray.quit_item_id() {
        Some(TrayCommand::Quit)
    } else {
        None
    }
}

/// Read DSH's own theme preference from `$DSH_HOME/settings.yaml`.
///
/// This — not the OS — is authoritative for the page: DSH resolves
/// `ui-theme.preference` and applies it to the document. The shell must match
/// that or the window chrome and the page disagree.
///
/// Returns `None` when the file or key is absent, so the caller can fall back
/// to the OS setting.
pub fn dsh_theme_preference() -> Option<bool> {
    let home = std::env::var("DSH_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|_| std::env::var("HOME").map(|h| std::path::PathBuf::from(h).join(".dsh")))
        .ok()?;
    let raw = std::fs::read_to_string(home.join("settings.yaml")).ok()?;
    parse_theme_preference(&raw)
}

/// Extract `ui-theme.preference` from the settings document.
///
/// A deliberately small hand-rolled scan rather than a YAML dependency: the
/// shell only needs this one value. The scan is line-based and only accepts the
/// value when it sits directly under the `ui-theme` key, so a same-named key
/// elsewhere cannot be mistaken for it.
pub fn parse_theme_preference(yaml: &str) -> Option<bool> {
    let mut in_theme_block = false;
    for line in yaml.lines() {
        let trimmed = line.trim_end();
        if trimmed.trim().is_empty() || trimmed.trim_start().starts_with('#') {
            continue;
        }
        let indent = trimmed.len() - trimmed.trim_start().len();

        if indent == 0 {
            // A new top-level key ends any block we were inside.
            in_theme_block = trimmed.trim_start().starts_with("ui-theme:");
            continue;
        }
        if !in_theme_block {
            continue;
        }
        let body = trimmed.trim_start();
        if let Some(value) = body.strip_prefix("preference:") {
            let value = value.trim().trim_matches(|c| c == '"' || c == '\'');
            return match value.to_ascii_lowercase().as_str() {
                "dark" => Some(true),
                "light" => Some(false),
                // `system` is not an answer: the caller must consult the OS.
                _ => None,
            };
        }
    }
    None
}

/// Resolve the appearance the shell should use.
///
/// DSH's preference wins when it is explicit; `system` (or an unreadable
/// setting) defers to the OS.
pub fn resolved_is_dark() -> bool {
    dsh_theme_preference().unwrap_or_else(system_is_dark)
}

/// Read the operating system's current appearance.
#[cfg(target_os = "macos")]
pub fn system_is_dark() -> bool {
    use std::process::Command;
    // `defaults read -g AppleInterfaceStyle` prints "Dark" in dark mode and
    // exits non-zero when the key is absent (light mode).
    Command::new("defaults")
        .args(["read", "-g", "AppleInterfaceStyle"])
        .output()
        .map(|out| {
            String::from_utf8_lossy(&out.stdout)
                .trim()
                .eq_ignore_ascii_case("Dark")
        })
        .unwrap_or(false)
}

#[cfg(not(target_os = "macos"))]
pub fn system_is_dark() -> bool {
    // Other platforms are not wired up yet; light is the safer default because
    // a wrong dark theme is more jarring than a wrong light one.
    false
}

/// A registered global hotkey.
///
/// Held for the lifetime of the app: dropping the manager unregisters the
/// shortcut, so it must outlive the event loop.
pub struct HotKeyHandle {
    _manager: global_hotkey::GlobalHotKeyManager,
    pub hotkey: global_hotkey::hotkey::HotKey,
}

/// The shortcut that summons the window.
///
/// Chosen to avoid collisions with common system and app shortcuts:
/// Cmd-Shift-D is not taken by macOS by default, and "D" matches DSH.
pub fn default_hotkey() -> global_hotkey::hotkey::HotKey {
    use global_hotkey::hotkey::{Code, HotKey, Modifiers};
    HotKey::new(Some(Modifiers::SUPER | Modifiers::SHIFT), Code::KeyD)
}

/// Register the summon shortcut.
///
/// Returns `None` when registration fails — usually because another app already
/// owns the combination. Losing the hotkey must not cost the user the window,
/// so this is reported and the shell continues.
pub fn register_hotkey(spec: Option<global_hotkey::hotkey::HotKey>) -> Option<HotKeyHandle> {
    use global_hotkey::GlobalHotKeyManager;

    let manager = match GlobalHotKeyManager::new() {
        Ok(manager) => manager,
        Err(err) => {
            tracing::warn!(%err, "global hotkey unavailable");
            return None;
        }
    };

    let hotkey = spec.unwrap_or_else(default_hotkey);
    if let Err(err) = manager.register(hotkey) {
        tracing::warn!(
            %err,
            "could not register the global hotkey; it may be taken by another app"
        );
        return None;
    }

    tracing::info!(?hotkey, "global hotkey registered");
    Some(HotKeyHandle {
        _manager: manager,
        hotkey,
    })
}

/// Whether an event matches the registered summon shortcut.
pub fn is_summon_event(
    event: &global_hotkey::GlobalHotKeyEvent,
    handle: &HotKeyHandle,
) -> bool {
    event.id == handle.hotkey.id()
}
