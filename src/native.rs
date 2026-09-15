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
    settings_item: MenuItem,
    quit_item: MenuItem,
    pub state: AgentState,
    /// A short label contributed by a plugin, shown after the agent state.
    plugin_label: Option<String>,
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
        let settings_item = MenuItem::new("Settings…", true, None);
        let quit_item = MenuItem::new("Quit", true, None);

        menu.append_items(&[
            &state_item,
            &PredefinedMenuItem::separator(),
            &show_item,
            &settings_item,
            &PredefinedMenuItem::separator(),
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
            settings_item,
            quit_item,
            state: AgentState::Idle,
            plugin_label: None,
        })
    }

    /// Update the tray to reflect a new agent state.
    pub fn set_state(&mut self, state: AgentState) {
        if self.state == state {
            return;
        }
        self.state = state;
        self.refresh_label();
        let _ = self._icon.set_icon(Some(make_icon(state)));
    }

    /// Set or clear the plugin-contributed label.
    ///
    /// Only text is plugin-controlled: the icon colour stays derived from real
    /// agent state, so a plugin cannot make the tray misrepresent what the
    /// agent is doing.
    pub fn set_plugin_label(&mut self, label: Option<String>) {
        let label = sanitize_label(label);
        if self.plugin_label == label {
            return;
        }
        self.plugin_label = label;
        self.refresh_label();
    }

/// Recompute the tray text from the agent state and any plugin label.
    fn refresh_label(&mut self) {
        let text = match &self.plugin_label {
            Some(label) => format!("Agent: {} — {label}", self.state.label()),
            None => format!("Agent: {}", self.state.label()),
        };
        self.state_item.set_text(text.clone());
        let _ = self._icon.set_tooltip(Some(text));
    }

    pub fn show_item_id(&self) -> &tray_icon::menu::MenuId {
        self.show_item.id()
    }

    pub fn settings_item_id(&self) -> &tray_icon::menu::MenuId {
        self.settings_item.id()
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
    fn plugin_labels_are_sanitised() {
        // A plugin-supplied label must not break the tooltip layout or flood it.
        let long = "x".repeat(200);
        let cleaned = sanitize_label(Some(long));
        let cleaned = cleaned.expect("long label survives, truncated");
        assert!(cleaned.chars().count() <= 60, "label not truncated: {}", cleaned.len());

        // Newlines would break the single-line menu text.
        let multiline = sanitize_label(Some("a\nb\tc".into())).unwrap();
        assert!(!multiline.contains('\n') && !multiline.contains('\t'), "{multiline:?}");

        // Whitespace-only and empty labels clear the field rather than showing
        // an empty separator in the menu.
        assert_eq!(sanitize_label(Some("   ".into())), None);
        assert_eq!(sanitize_label(Some(String::new())), None);
        assert_eq!(sanitize_label(None), None);
    }

    #[test]
    fn generated_icon_has_the_expected_buffer() {
        let icon = make_icon(AgentState::Working);
        // Construction is the assertion: from_rgba rejects a wrong length.
        drop(icon);
    }

    #[test]
    fn parses_hotkey_strings() {
        let spec = HotkeySpec::parse("meta+shift+D").expect("parse");
        assert_eq!(spec.key, HotkeyKey::D);
        assert!(spec.mods.contains(&HotkeyMod::Meta));
        assert!(spec.mods.contains(&HotkeyMod::Shift));
    }

    #[test]
    fn hotkey_parsing_is_tolerant_of_spelling_and_order() {
        let a = HotkeySpec::parse("meta+shift+D").unwrap();
        // Case, whitespace, ordering, and the platform aliases all normalise to
        // the same shortcut, so a user copying from any convention works.
        for variant in [
            "META+SHIFT+D",
            " D + shift + meta ",
            "cmd+shift+d",
            "command+shift+d",
            "super+shift+d",
        ] {
            assert_eq!(
                HotkeySpec::parse(variant).unwrap(),
                a,
                "{variant} did not normalise to the same shortcut"
            );
        }
        assert_eq!(
            HotkeySpec::parse("alt+f4").unwrap(),
            HotkeySpec {
                mods: vec![HotkeyMod::Alt],
                key: HotkeyKey::F4
            }
        );
    }

    #[test]
    fn hotkey_requires_a_modifier() {
        // A bare letter as a global shortcut would swallow that key everywhere.
        let err = HotkeySpec::parse("D").unwrap_err();
        assert!(err.contains("no modifier"), "unexpected error: {err}");
    }

    #[test]
    fn hotkey_rejects_malformed_input() {
        for bad in [
            "",                 // empty
            "meta+",            // dangling separator
            "+d",               // leading separator
            "meta+meta+d",      // duplicate modifier
            "meta+nope",        // not a key we accept
            "meta+shift+d+x",   // two keys
        ] {
            assert!(
                HotkeySpec::parse(bad).is_err(),
                "{bad:?} should have been rejected"
            );
        }
    }

    #[test]
    fn hotkey_round_trips_through_its_canonical_form() {
        for text in ["meta+shift+D", "alt+F4", "control+space", "meta+1"] {
            let spec = HotkeySpec::parse(text).unwrap();
            let canonical = spec.to_string_canonical();
            assert_eq!(
                HotkeySpec::parse(&canonical).unwrap(),
                spec,
                "{text} did not round trip via {canonical}"
            );
        }
    }

    #[test]
    fn default_hotkey_is_the_documented_shortcut() {
        assert_eq!(HotkeySpec::default_spec().to_string_canonical(), "meta+shift+D");
    }

    #[test]
    fn the_summon_hotkey_does_not_collide_with_common_shortcuts() {
        use global_hotkey::hotkey::{Code, Modifiers};
        let hk = HotkeySpec::default_spec().to_hotkey();
        assert_eq!(hk.key, Code::KeyD);
        // Cmd-D alone is a common app shortcut ("Don't Save"); requiring Shift
        // keeps the default out of the way.
        //
        // `Modifiers::META` is what this crate accepts for the platform Command
        // key: global-hotkey maps `SUPER | META` onto the same native flag, so
        // either spelling registers Command on macOS.
        assert!(hk.mods.intersects(Modifiers::META | Modifiers::SUPER));
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
    Settings,
    Quit,
}

/// Map a menu event id to a command, if it is one of ours.
pub fn tray_command(event: &MenuEvent, tray: &Tray) -> Option<TrayCommand> {
    if event.id == *tray.show_item_id() {
        Some(TrayCommand::Show)
    } else if event.id == *tray.settings_item_id() {
        Some(TrayCommand::Settings)
    } else if event.id == *tray.quit_item_id() {
        Some(TrayCommand::Quit)
    } else {
        None
    }
}

/// Normalise a plugin-supplied tray label.
///
/// Flattened to one line and length-capped: a tooltip cannot show newlines, and
/// without a cap a plugin could flood the menu. Whitespace-only input clears the
/// label rather than leaving a stray separator.
pub fn sanitize_label(label: Option<String>) -> Option<String> {
    let flat = label?.replace(char::is_whitespace, " ");
    let trimmed = flat.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.chars().count() > 60 {
        let head: String = trimmed.chars().take(59).collect();
        Some(format!("{head}…"))
    } else {
        Some(trimmed.to_string())
    }
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

/// A registered global hotkey, re-targetable at runtime.
///
/// Holds the manager for the lifetime of the app: dropping it unregisters the
/// shortcut. Kept in one place so a live config change can swap the combination
/// without tearing the manager down.
pub struct HotKeyHandle {
    manager: global_hotkey::GlobalHotKeyManager,
    hotkey: global_hotkey::hotkey::HotKey,
    spec: HotkeySpec,
}

impl HotKeyHandle {
    /// The active shortcut.
    pub fn hotkey(&self) -> &global_hotkey::hotkey::HotKey {
        &self.hotkey
    }

    /// The active shortcut as written in the config.
    pub fn spec(&self) -> &HotkeySpec {
        &self.spec
    }

    /// Switch to a different combination.
    ///
    /// Registration happens **before** the old one is released, so a
    /// combination owned by another app is rejected without ever leaving the
    /// user without a working hotkey. On success the previous one is dropped.
    pub fn retarget(&mut self, spec: HotkeySpec) -> Result<(), String> {
        if spec == self.spec {
            return Ok(()); // Nothing to do; avoids a needless unregister window.
        }

        let candidate = spec.to_hotkey();
        self.manager
            .register(candidate)
            .map_err(|err| format!("{} is unavailable: {err}", spec.to_string_canonical()))?;

        // The new one is registered, so releasing the old one cannot leave a gap.
        let _ = self.manager.unregister(self.hotkey);
        self.hotkey = candidate;
        self.spec = spec;
        Ok(())
    }
}

/// Register the summon shortcut.
///
/// Returns `None` when registration fails — usually because another app already
/// owns the combination. Losing the hotkey must not cost the user the window,
/// so this is reported and the shell continues.
pub fn register_hotkey(spec: HotkeySpec) -> Option<HotKeyHandle> {
    use global_hotkey::GlobalHotKeyManager;

    let manager = match GlobalHotKeyManager::new() {
        Ok(manager) => manager,
        Err(err) => {
            tracing::warn!(%err, "global hotkey unavailable");
            return None;
        }
    };

    let hotkey = spec.to_hotkey();
    if let Err(err) = manager.register(hotkey) {
        tracing::warn!(
            shortcut = %spec.to_string_canonical(),
            %err,
            "could not register the global hotkey; it may be taken by another app"
        );
        return None;
    }

    tracing::info!(shortcut = %spec.to_string_canonical(), "global hotkey registered");
    Some(HotKeyHandle {
        manager,
        hotkey,
        spec,
    })
}

/// Whether an event matches the registered summon shortcut.
pub fn is_summon_event(
    event: &global_hotkey::GlobalHotKeyEvent,
    handle: &HotKeyHandle,
) -> bool {
    event.id == handle.hotkey().id()
}

/// A configurable keyboard shortcut, as written in the `dsh-shell` settings
/// namespace.
///
/// Stored as text (`"meta+shift+D"`) rather than a serialized enum so the file
/// stays readable and a typo produces a clear parse error instead of a silently
/// ignored field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HotkeySpec {
    pub mods: Vec<HotkeyMod>,
    pub key: HotkeyKey,
}

/// Modifier keys accepted in a shortcut string.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HotkeyMod {
    Meta,
    Shift,
    Alt,
    Control,
}

/// A key accepted in a shortcut string.
///
/// Deliberately a whitelist rather than free text: an arbitrary key name would
/// have to map onto the platform's key codes anyway, and a wrong entry would
/// fail at registration with a less useful message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HotkeyKey {
    A, B, C, D, E, F, G, H, I, J, K, L, M,
    N, O, P, Q, R, S, T, U, V, W, X, Y, Z,
    Digit0, Digit1, Digit2, Digit3, Digit4,
    Digit5, Digit6, Digit7, Digit8, Digit9,
    Space, Enter, Escape, Tab,
    F1, F2, F3, F4, F5, F6, F7, F8, F9, F10, F11, F12,
}

impl HotkeySpec {
    /// The default: ⌘⇧D.
    ///
    /// Shift is required because ⌘D alone is a common in-app shortcut.
    pub fn default_spec() -> HotkeySpec {
        HotkeySpec {
            mods: vec![HotkeyMod::Meta, HotkeyMod::Shift],
            key: HotkeyKey::D,
        }
    }

    /// Parse `"meta+shift+D"`.
    ///
    /// Case-insensitive, tolerant of surrounding whitespace, and the key may
    /// appear in any position. At least one modifier is **required**: a bare
    /// letter as a global shortcut would swallow that key system-wide.
    pub fn parse(text: &str) -> Result<HotkeySpec, String> {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return Err("shortcut is empty".into());
        }

        let mut mods = Vec::new();
        let mut key = None;

        for part in trimmed.split('+') {
            let token = part.trim();
            if token.is_empty() {
                return Err(format!("empty component in {trimmed:?}"));
            }
            let lower = token.to_ascii_lowercase();
            let modifier = match lower.as_str() {
                "meta" | "cmd" | "command" | "super" | "win" => Some(HotkeyMod::Meta),
                "shift" => Some(HotkeyMod::Shift),
                "alt" | "option" | "opt" => Some(HotkeyMod::Alt),
                "control" | "ctrl" => Some(HotkeyMod::Control),
                _ => None,
            };
            match modifier {
                Some(m) => {
                    if mods.contains(&m) {
                        return Err(format!("duplicate modifier {:?} in {trimmed:?}", m));
                    }
                    mods.push(m);
                }
                None => {
                    if key.is_some() {
                        return Err(format!("more than one key in {trimmed:?}"));
                    }
                    key = Some(parse_key(&lower)?);
                }
            }
        }

        let key = key.ok_or_else(|| format!("no key in {trimmed:?}"))?;
        if mods.is_empty() {
            return Err(format!(
                "{trimmed:?} has no modifier; a global shortcut needs at least one"
            ));
        }
        // Normalise modifier order so two spellings of the same shortcut compare
        // equal. `retarget` relies on that comparison to skip no-op changes.
        mods.sort_by_key(|m| match m {
            HotkeyMod::Control => 0,
            HotkeyMod::Alt => 1,
            HotkeyMod::Shift => 2,
            HotkeyMod::Meta => 3,
        });
        Ok(HotkeySpec { mods, key })
    }

    /// Render back to the canonical string form.
    pub fn to_string_canonical(&self) -> String {
        let mut parts: Vec<String> = self
            .mods
            .iter()
            .map(|m| {
                match m {
                    HotkeyMod::Meta => "meta",
                    HotkeyMod::Shift => "shift",
                    HotkeyMod::Alt => "alt",
                    HotkeyMod::Control => "control",
                }
                .to_string()
            })
            .collect();
        parts.push(key_token(self.key).to_string());
        parts.join("+")
    }

    /// Convert to the platform hotkey type.
    pub fn to_hotkey(&self) -> global_hotkey::hotkey::HotKey {
        use global_hotkey::hotkey::{HotKey, Modifiers};
        let mut mods = Modifiers::empty();
        for m in &self.mods {
            mods |= match m {
                HotkeyMod::Meta => Modifiers::META,
                HotkeyMod::Shift => Modifiers::SHIFT,
                HotkeyMod::Alt => Modifiers::ALT,
                HotkeyMod::Control => Modifiers::CONTROL,
            };
        }
        HotKey::new(Some(mods), code_for(self.key))
    }
}

/// The canonical text for a key, matching what `parse_key` accepts.
fn key_token(key: HotkeyKey) -> &'static str {
    use HotkeyKey::*;
    match key {
        A => "A", B => "B", C => "C", D => "D", E => "E", F => "F",
        G => "G", H => "H", I => "I", J => "J", K => "K", L => "L",
        M => "M", N => "N", O => "O", P => "P", Q => "Q", R => "R",
        S => "S", T => "T", U => "U", V => "V", W => "W", X => "X",
        Y => "Y", Z => "Z",
        Digit0 => "0", Digit1 => "1", Digit2 => "2", Digit3 => "3",
        Digit4 => "4", Digit5 => "5", Digit6 => "6", Digit7 => "7",
        Digit8 => "8", Digit9 => "9",
        Space => "Space", Enter => "Enter", Escape => "Escape", Tab => "Tab",
        F1 => "F1", F2 => "F2", F3 => "F3", F4 => "F4",
        F5 => "F5", F6 => "F6", F7 => "F7", F8 => "F8",
        F9 => "F9", F10 => "F10", F11 => "F11", F12 => "F12",
    }
}

/// Map a parsed key onto the platform key code.
fn code_for(key: HotkeyKey) -> global_hotkey::hotkey::Code {
    use global_hotkey::hotkey::Code;
    match key {
        HotkeyKey::A => Code::KeyA, HotkeyKey::B => Code::KeyB,
        HotkeyKey::C => Code::KeyC, HotkeyKey::D => Code::KeyD,
        HotkeyKey::E => Code::KeyE, HotkeyKey::F => Code::KeyF,
        HotkeyKey::G => Code::KeyG, HotkeyKey::H => Code::KeyH,
        HotkeyKey::I => Code::KeyI, HotkeyKey::J => Code::KeyJ,
        HotkeyKey::K => Code::KeyK, HotkeyKey::L => Code::KeyL,
        HotkeyKey::M => Code::KeyM, HotkeyKey::N => Code::KeyN,
        HotkeyKey::O => Code::KeyO, HotkeyKey::P => Code::KeyP,
        HotkeyKey::Q => Code::KeyQ, HotkeyKey::R => Code::KeyR,
        HotkeyKey::S => Code::KeyS, HotkeyKey::T => Code::KeyT,
        HotkeyKey::U => Code::KeyU, HotkeyKey::V => Code::KeyV,
        HotkeyKey::W => Code::KeyW, HotkeyKey::X => Code::KeyX,
        HotkeyKey::Y => Code::KeyY, HotkeyKey::Z => Code::KeyZ,
        HotkeyKey::Digit0 => Code::Digit0, HotkeyKey::Digit1 => Code::Digit1,
        HotkeyKey::Digit2 => Code::Digit2, HotkeyKey::Digit3 => Code::Digit3,
        HotkeyKey::Digit4 => Code::Digit4, HotkeyKey::Digit5 => Code::Digit5,
        HotkeyKey::Digit6 => Code::Digit6, HotkeyKey::Digit7 => Code::Digit7,
        HotkeyKey::Digit8 => Code::Digit8, HotkeyKey::Digit9 => Code::Digit9,
        HotkeyKey::Space => Code::Space, HotkeyKey::Enter => Code::Enter,
        HotkeyKey::Escape => Code::Escape, HotkeyKey::Tab => Code::Tab,
        HotkeyKey::F1 => Code::F1, HotkeyKey::F2 => Code::F2,
        HotkeyKey::F3 => Code::F3, HotkeyKey::F4 => Code::F4,
        HotkeyKey::F5 => Code::F5, HotkeyKey::F6 => Code::F6,
        HotkeyKey::F7 => Code::F7, HotkeyKey::F8 => Code::F8,
        HotkeyKey::F9 => Code::F9, HotkeyKey::F10 => Code::F10,
        HotkeyKey::F11 => Code::F11, HotkeyKey::F12 => Code::F12,
    }
}

/// Parse a single key token.
fn parse_key(token: &str) -> Result<HotkeyKey, String> {
    use HotkeyKey::*;
    let key = match token {
        "a" => A, "b" => B, "c" => C, "d" => D, "e" => E, "f" => F,
        "g" => G, "h" => H, "i" => I, "j" => J, "k" => K, "l" => L,
        "m" => M, "n" => N, "o" => O, "p" => P, "q" => Q, "r" => R,
        "s" => S, "t" => T, "u" => U, "v" => V, "w" => W, "x" => X,
        "y" => Y, "z" => Z,
        "0" => Digit0, "1" => Digit1, "2" => Digit2, "3" => Digit3,
        "4" => Digit4, "5" => Digit5, "6" => Digit6, "7" => Digit7,
        "8" => Digit8, "9" => Digit9,
        "space" => Space, "enter" | "return" => Enter,
        "escape" | "esc" => Escape, "tab" => Tab,
        "f1" => F1, "f2" => F2, "f3" => F3, "f4" => F4,
        "f5" => F5, "f6" => F6, "f7" => F7, "f8" => F8,
        "f9" => F9, "f10" => F10, "f11" => F11, "f12" => F12,
        other => return Err(format!("unknown key {other:?}")),
    };
    Ok(key)
}
