//! Remembering where the window was.
//!
//! The window's size and position are *state*, not configuration: they are what
//! the user last did, not what they chose. They therefore live in the shell's
//! own state file rather than in DSH's settings document, so the settings
//! namespace stays limited to things a user would want to set deliberately.
//!
//! Losing this file costs nothing — the window opens at its default size — so
//! a corrupt or unreadable file is treated as absent rather than reported.
//!
//! Sizes and positions are stored in **logical** units, which is what makes the
//! file portable across displays of different densities: a window that occupies
//! half a 1440-point screen keeps doing so instead of doubling.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The size the window opens at when nothing has been saved.
pub const DEFAULT_WIDTH: f64 = 1280.0;
pub const DEFAULT_HEIGHT: f64 = 840.0;

/// The smallest window worth restoring.
///
/// Matches the window's own minimum. A saved value below it would be clamped by
/// the window anyway; rejecting it here keeps the file's meaning honest.
pub const MIN_WIDTH: f64 = 720.0;
pub const MIN_HEIGHT: f64 = 480.0;

/// A remembered window rectangle, in logical units.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct WindowState {
    pub width: f64,
    pub height: f64,
    /// Absent until the window has been positioned at least once.
    #[serde(default)]
    pub x: Option<f64>,
    #[serde(default)]
    pub y: Option<f64>,
    /// Whether the window was maximized when it was last hidden.
    #[serde(default)]
    pub maximized: bool,
}

impl Default for WindowState {
    fn default() -> Self {
        Self {
            width: DEFAULT_WIDTH,
            height: DEFAULT_HEIGHT,
            x: None,
            y: None,
            maximized: false,
        }
    }
}

impl WindowState {
    /// Whether these numbers are usable at all.
    ///
    /// Guards against a truncated write, a hand-edited file, or NaN — any of
    /// which would otherwise reach `with_inner_size` and produce a window that
    /// cannot be seen or resized.
    pub fn is_sane(&self) -> bool {
        let finite = |v: f64| v.is_finite();
        if !finite(self.width) || !finite(self.height) {
            return false;
        }
        if self.width < MIN_WIDTH || self.height < MIN_HEIGHT {
            return false;
        }
        // An absurd size means the file is wrong, not that the user has a
        // 100000-point display.
        if self.width > 20000.0 || self.height > 20000.0 {
            return false;
        }
        match (self.x, self.y) {
            (Some(x), Some(y)) => finite(x) && finite(y) && x.abs() <= 100_000.0 && y.abs() <= 100_000.0,
            // A half-known position is unusable, so the window is centred.
            (None, None) => true,
            _ => false,
        }
    }

    /// Whether the rectangle overlaps a monitor, so restoring it cannot put the
    /// window somewhere the user cannot reach.
    ///
    /// Monitors are `(x, y, width, height)` rectangles in logical units. When no
    /// monitor is known the position is trusted: the alternative is discarding a
    /// good position because the platform would not report its displays.
    pub fn is_reachable(&self, monitors: &[(f64, f64, f64, f64)]) -> bool {
        let (Some(x), Some(y)) = (self.x, self.y) else {
            // No saved position means the platform centres it, which is always
            // reachable.
            return true;
        };
        if monitors.is_empty() {
            return true;
        }
        // A window dragged almost entirely off the edge is still recoverable if
        // a strip remains, so require only a small overlap rather than full
        // containment.
        const KEEP: f64 = 80.0;
        monitors.iter().any(|&(mx, my, mw, mh)| {
            x + self.width > mx + KEEP
                && x < mx + mw - KEEP
                && y + self.height > my
                && y < my + mh
        })
    }

    /// Clamp the size to a monitor, so a window saved on a large display is not
    /// restored larger than the one it now opens on.
    pub fn clamped_to(&self, monitors: &[(f64, f64, f64, f64)]) -> Self {
        let Some(&(_, _, widest, tallest)) = monitors
            .iter()
            .max_by(|a, b| (a.2 * a.3).partial_cmp(&(b.2 * b.3)).unwrap_or(std::cmp::Ordering::Equal))
        else {
            return *self;
        };
        Self {
            width: self.width.min(widest).max(MIN_WIDTH),
            height: self.height.min(tallest).max(MIN_HEIGHT),
            ..*self
        }
    }
}

/// Where the window state file lives.
///
/// Under DSH's home so the shell keeps all of its own state in one tree, and
/// under `cache` because a lost window position is not worth preserving more
/// carefully than that.
pub fn path() -> Option<PathBuf> {
    state_path_from(
        std::env::var("DSH_SHELL_WINDOW_STATE").ok(),
        std::env::var("DSH_HOME").ok(),
        std::env::var("HOME").ok(),
    )
}

/// Split out so the resolution order can be tested without touching the
/// process environment, which every test thread shares.
fn state_path_from(explicit: Option<String>, dsh_home: Option<String>, home: Option<String>) -> Option<PathBuf> {
    if let Some(explicit) = explicit {
        return Some(PathBuf::from(explicit));
    }
    let home = dsh_home
        .map(PathBuf::from)
        .or_else(|| home.map(|h| PathBuf::from(h).join(".dsh")))?;
    Some(home.join("cache").join("dsh-shell").join("window.json"))
}

/// Read the saved state, or the default when there is nothing usable to read.
pub fn load(path: &Path) -> WindowState {
    let Ok(text) = std::fs::read_to_string(path) else {
        return WindowState::default();
    };
    match serde_json::from_str::<WindowState>(&text) {
        Ok(state) if state.is_sane() => state,
        Ok(_) => {
            tracing::debug!(path = %path.display(), "saved window state is unusable; using the default");
            WindowState::default()
        }
        Err(err) => {
            tracing::debug!(%err, path = %path.display(), "could not read window state; using the default");
            WindowState::default()
        }
    }
}

/// Write the state, ignoring failures.
///
/// A window position is not worth an error path: if it cannot be saved the user
/// loses nothing but a convenience. It is written via a temporary file and a
/// rename so an interrupted write cannot leave a half-file behind.
pub fn save(path: &Path, state: &WindowState) {
    if !state.is_sane() {
        tracing::debug!("refusing to save an unusable window state");
        return;
    }
    let Some(parent) = path.parent() else {
        return;
    };
    if let Err(err) = std::fs::create_dir_all(parent) {
        tracing::debug!(%err, "could not create the window state directory");
        return;
    }

    let json = match serde_json::to_string_pretty(state) {
        Ok(json) => json,
        Err(err) => {
            tracing::debug!(%err, "could not encode the window state");
            return;
        }
    };

    let temp = path.with_extension("json.tmp");
    if let Err(err) = std::fs::write(&temp, json) {
        tracing::debug!(%err, "could not write the window state");
        return;
    }
    if let Err(err) = std::fs::rename(&temp, path) {
        tracing::debug!(%err, "could not replace the window state");
        let _ = std::fs::remove_file(&temp);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A single 1920x1080 display at the origin, in logical units.
    const ONE_SCREEN: &[(f64, f64, f64, f64)] = &[(0.0, 0.0, 1920.0, 1080.0)];

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "dsh-shell-window-{}-{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create scratch dir");
        dir.join("window.json")
    }

    #[test]
    fn a_round_trip_preserves_the_rectangle() {
        let path = scratch("round-trip");
        let state = WindowState {
            width: 1024.0,
            height: 700.0,
            x: Some(80.0),
            y: Some(40.0),
            maximized: true,
        };
        save(&path, &state);
        assert_eq!(load(&path), state);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn a_missing_file_yields_the_default() {
        let path = scratch("missing");
        assert_eq!(load(&path), WindowState::default());
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn a_corrupt_file_yields_the_default() {
        let path = scratch("corrupt");
        std::fs::write(&path, "{ this is not json").unwrap();
        // A truncated write must not stop the shell from opening.
        assert_eq!(load(&path), WindowState::default());
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn unknown_fields_do_not_break_loading() {
        let path = scratch("unknown-field");
        std::fs::write(
            &path,
            r#"{"width":900.0,"height":600.0,"x":10.0,"y":10.0,"futureField":1}"#,
        )
        .unwrap();
        let state = load(&path);
        assert_eq!(state.width, 900.0);
        assert_eq!(state.height, 600.0);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn a_degenerate_size_is_rejected() {
        // A zero or NaN size would produce a window that cannot be grabbed.
        for (width, height) in [(0.0, 600.0), (900.0, 0.0), (f64::NAN, 600.0), (-100.0, 600.0)] {
            let state = WindowState {
                width,
                height,
                ..WindowState::default()
            };
            assert!(!state.is_sane(), "{width}x{height} must be rejected");
        }
    }

    #[test]
    fn a_size_above_the_window_minimum_is_accepted() {
        let state = WindowState {
            width: MIN_WIDTH,
            height: MIN_HEIGHT,
            ..WindowState::default()
        };
        assert!(state.is_sane());
    }

    #[test]
    fn a_half_known_position_is_rejected() {
        let state = WindowState {
            x: Some(10.0),
            y: None,
            ..WindowState::default()
        };
        assert!(!state.is_sane());
    }

    #[test]
    fn an_offset_position_on_a_second_display_is_reachable() {
        let state = WindowState {
            x: Some(2000.0),
            y: Some(100.0),
            ..WindowState::default()
        };
        // The second display sits to the right of the primary one.
        let monitors = [(0.0, 0.0, 1920.0, 1080.0), (1920.0, 0.0, 2560.0, 1440.0)];
        assert!(state.is_reachable(&monitors));
    }

    #[test]
    fn a_position_on_a_disconnected_display_is_unreachable() {
        let state = WindowState {
            x: Some(9000.0),
            y: Some(100.0),
            ..WindowState::default()
        };
        // Restoring this would put the window where the user cannot reach it,
        // so it must be refused and the window centred instead.
        assert!(!state.is_reachable(ONE_SCREEN));
    }

    #[test]
    fn a_window_mostly_off_the_left_edge_is_still_reachable() {
        let state = WindowState {
            x: Some(-1100.0),
            y: Some(100.0),
            ..WindowState::default()
        };
        // Enough of the caption strip remains to drag it back.
        assert!(state.is_reachable(ONE_SCREEN));
    }

    #[test]
    fn a_window_far_off_the_left_edge_is_unreachable() {
        let state = WindowState {
            x: Some(-1400.0),
            y: Some(100.0),
            ..WindowState::default()
        };
        assert!(!state.is_reachable(ONE_SCREEN));
    }

    #[test]
    fn no_position_is_always_reachable() {
        assert!(WindowState::default().is_reachable(ONE_SCREEN));
        // And an unreported monitor list must not discard a good position.
        let placed = WindowState {
            x: Some(9000.0),
            y: Some(9000.0),
            ..WindowState::default()
        };
        assert!(placed.is_reachable(&[]));
    }

    #[test]
    fn a_size_larger_than_the_display_is_clamped() {
        let state = WindowState {
            width: 4000.0,
            height: 3000.0,
            ..WindowState::default()
        };
        let clamped = state.clamped_to(ONE_SCREEN);
        assert_eq!(clamped.width, 1920.0);
        assert_eq!(clamped.height, 1080.0);
        // The position is preserved; only the size was out of range.
        assert_eq!(clamped.x, state.x);
    }

    #[test]
    fn clamping_picks_the_largest_display() {
        let state = WindowState {
            width: 5000.0,
            height: 5000.0,
            ..WindowState::default()
        };
        let monitors = [(0.0, 0.0, 1920.0, 1080.0), (1920.0, 0.0, 2560.0, 1440.0)];
        let clamped = state.clamped_to(&monitors);
        assert_eq!(clamped.width, 2560.0);
        assert_eq!(clamped.height, 1440.0);
    }

    #[test]
    fn clamping_never_goes_below_the_window_minimum() {
        // A tiny saved size on a tiny reported monitor must still leave a
        // window the platform's own minimum will accept.
        let state = WindowState {
            width: 100.0,
            height: 100.0,
            ..WindowState::default()
        };
        let clamped = state.clamped_to(&[(0.0, 0.0, 50.0, 50.0)]);
        assert!(clamped.width >= MIN_WIDTH);
        assert!(clamped.height >= MIN_HEIGHT);
    }

    #[test]
    fn an_unusable_state_is_not_written() {
        let path = scratch("not-written");
        let bad = WindowState {
            width: f64::NAN,
            ..WindowState::default()
        };
        save(&path, &bad);
        assert!(!path.exists(), "a state that cannot be restored must not be saved");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn saving_replaces_rather_than_appends() {
        let path = scratch("replace");
        save(
            &path,
            &WindowState {
                width: 900.0,
                height: 600.0,
                ..WindowState::default()
            },
        );
        save(
            &path,
            &WindowState {
                width: 1000.0,
                height: 700.0,
                ..WindowState::default()
            },
        );
        let state = load(&path);
        assert_eq!(state.width, 1000.0);
        // The temporary file must not be left behind.
        assert!(!path.with_extension("json.tmp").exists());
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn the_explicit_path_wins() {
        let resolved = state_path_from(
            Some("/tmp/explicit.json".into()),
            Some("/home/u/.dsh".into()),
            Some("/home/u".into()),
        );
        assert_eq!(resolved, Some(PathBuf::from("/tmp/explicit.json")));
    }

    #[test]
    fn dsh_home_is_preferred_over_the_bare_home() {
        let resolved = state_path_from(None, Some("/custom/dsh".into()), Some("/home/u".into()));
        assert_eq!(
            resolved,
            Some(PathBuf::from("/custom/dsh/cache/dsh-shell/window.json"))
        );
    }

    #[test]
    fn the_bare_home_falls_back_to_dot_dsh() {
        let resolved = state_path_from(None, None, Some("/home/u".into()));
        assert_eq!(
            resolved,
            Some(PathBuf::from("/home/u/.dsh/cache/dsh-shell/window.json"))
        );
    }

    #[test]
    fn no_home_at_all_yields_no_path() {
        assert_eq!(state_path_from(None, None, None), None);
    }
}
