//! Theme tokens and hot reload for the shell.
//!
//! The theme drives two things:
//!
//! 1. The native chrome this shell owns — the window background and the caption
//!    strip that sits beside the traffic lights.
//! 2. The web UI inside the webview, via a CSS custom-property block injected
//!    on every load and re-injected on every reload.
//!
//! That second part is what makes hot reload worth having here: editing
//! `theme.json` restyles the real DSH interface without restarting the app or
//! losing the session.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

/// A color stored as `#rrggbb`.
///
/// Parsing is strict so a typo surfaces as a readable error at load time rather
/// than silently rendering black.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(try_from = "String", into = "String")]
pub struct ColorHex(pub u32);

impl TryFrom<String> for ColorHex {
    type Error = String;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        let hex = value.trim().trim_start_matches('#');
        if hex.len() != 6 {
            return Err(format!(
                "expected a 6-digit hex color like \"#1a1b26\", got {value:?}"
            ));
        }
        u32::from_str_radix(hex, 16)
            .map(ColorHex)
            .map_err(|_| format!("{value:?} is not valid hexadecimal"))
    }
}

impl From<ColorHex> for String {
    fn from(value: ColorHex) -> Self {
        format!("#{:06x}", value.0)
    }
}

impl ColorHex {
    /// `#rrggbb`, for embedding in CSS.
    pub fn css(&self) -> String {
        format!("#{:06x}", self.0)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Palette {
    /// Window and page background.
    pub background: ColorHex,
    /// Raised surfaces (cards, popovers).
    pub surface: ColorHex,
    /// Hover/selected surface.
    pub surface_hover: ColorHex,
    pub border: ColorHex,
    pub text: ColorHex,
    pub text_muted: ColorHex,
    pub accent: ColorHex,
    /// Height of the draggable caption strip, in CSS pixels.
    pub caption_height: u32,
}

impl Palette {
    /// The dark palette, taken from DSH's own dark boot theme.
    ///
    /// Sourced from the shipped CSS rather than eyeballed, so the window chrome
    /// matches the app's own background exactly and the seam disappears.
    pub fn deepseek_dark() -> Palette {
        Palette {
            background: ColorHex(0x151517),
            surface: ColorHex(0x2c2c2e),
            surface_hover: ColorHex(0x3a3a3c),
            border: ColorHex(0x3f3f42),
            text: ColorHex(0xf9fafb),
            text_muted: ColorHex(0xadb2b8),
            accent: ColorHex(0x4176e6),
            caption_height: 34,
        }
    }

    /// The light palette, taken from DSH's own light boot theme.
    pub fn deepseek_light() -> Palette {
        Palette {
            background: ColorHex(0xffffff),
            surface: ColorHex(0xf5f6f7),
            surface_hover: ColorHex(0xe9ebed),
            border: ColorHex(0xd9dcdf),
            text: ColorHex(0x0f1115),
            text_muted: ColorHex(0x81858c),
            accent: ColorHex(0x4176e6),
            caption_height: 34,
        }
    }
}

impl Default for Palette {
    fn default() -> Self {
        Palette::deepseek_light()
    }
}

/// Which palette the shell uses.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum Appearance {
    Light,
    Dark,
    /// Follow the operating system.
    #[default]
    System,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Theme {
    /// Which palette to use; `system` follows the OS.
    #[serde(default)]
    pub appearance: Appearance,
    /// Palette used when the resolved appearance is light.
    #[serde(default = "Palette::deepseek_light")]
    pub light: Palette,
    /// Palette used when the resolved appearance is dark.
    #[serde(default = "Palette::deepseek_dark")]
    pub dark: Palette,
    /// The global shortcut that summons the window, e.g. `"meta+shift+D"`.
    ///
    /// A string rather than a structured value so the file stays readable and a
    /// typo is reported as text. Validated on load; see `HotkeySpec::parse`.
    #[serde(default = "default_hotkey_text")]
    pub hotkey: String,
    /// Inset of the traffic lights from the window's top-left, in CSS pixels.
    pub traffic_light_inset: TrafficLightInset,
    /// Extra CSS appended to the injected block, for quick experiments.
    #[serde(default)]
    pub custom_css: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TrafficLightInset {
    pub x: f64,
    pub y: f64,
}

/// The shipped default shortcut.
fn default_hotkey_text() -> String {
    "meta+shift+D".to_string()
}

impl Default for Theme {
    fn default() -> Self {
        Theme {
            appearance: Appearance::System,
            hotkey: default_hotkey_text(),
            light: Palette::deepseek_light(),
            dark: Palette::deepseek_dark(),
            // Matches the default macOS inset so the buttons look native until
            // the user moves them.
            traffic_light_inset: TrafficLightInset { x: 20.0, y: 20.0 },
            custom_css: String::new(),
        }
    }
}

impl Theme {
    /// Resolve the active palette for a given system appearance.
    ///
    /// `Appearance::System` defers to the OS; the explicit variants pin it.
    pub fn palette_for(&self, system_is_dark: bool) -> &Palette {
        match self.appearance {
            Appearance::Light => &self.light,
            Appearance::Dark => &self.dark,
            Appearance::System => {
                if system_is_dark {
                    &self.dark
                } else {
                    &self.light
                }
            }
        }
    }

    /// The CSS injected into the web UI for the resolved palette.
    ///
    /// DSH's own theme system owns the real design tokens; this block exists so
    /// the shell can match the page to its chrome — background, and leaving room
    /// for the caption strip — without fighting it. It also mirrors DSH's own
    /// dark-mode marker so the page and the native window agree on appearance.
    /// `custom_css` is applied last so it always wins.
    pub fn injected_css(&self, system_is_dark: bool) -> String {
        let p = self.palette_for(system_is_dark);
        let scheme = if system_is_dark { "dark" } else { "light" };
        format!(
            r#":root {{
  color-scheme: {scheme};
  --dsh-shell-background: {bg};
  --dsh-shell-surface: {surface};
  --dsh-shell-border: {border};
  --dsh-shell-text: {text};
  --dsh-shell-muted: {muted};
  --dsh-shell-accent: {accent};
  --dsh-shell-caption-height: {caption}px;
}}
html, body {{
  background: {bg} !important;
}}
/* Reserve the caption strip so no DSH chrome hides under the traffic lights. */
body {{
  padding-top: var(--dsh-shell-caption-height);
  box-sizing: border-box;
}}

/* The window-drag region.
   With the system titlebar hidden, no OS drag area remains, so this strip
   forwards pointer presses to the shell. It is transparent, sits above the
   page, and covers only the reserved caption strip — never DSH's own UI. */
#__dsh_shell_drag {{
  position: fixed;
  top: 0;
  left: 0;
  right: 0;
  height: var(--dsh-shell-caption-height);
  z-index: 2147483647;
  -webkit-app-region: drag;
  user-select: none;
  -webkit-user-select: none;
}}
{custom}
"#,
            scheme = scheme,
            bg = p.background.css(),
            surface = p.surface.css(),
            border = p.border.css(),
            text = p.text.css(),
            muted = p.text_muted.css(),
            accent = p.accent.css(),
            caption = p.caption_height,
            custom = self.custom_css,
        )
    }

    pub fn parse(json: &str) -> Result<Theme, String> {
        serde_json::from_str(json).map_err(|err| err.to_string())
    }

    pub fn to_pretty_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_else(|_| "{}".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_round_trips() {
        let theme = Theme::default();
        let json = theme.to_pretty_json();
        let back = Theme::parse(&json).expect("reparse");
        assert_eq!(theme, back);
    }

    #[test]
    fn rejects_bad_colors() {
        let json = Theme::default()
            .to_pretty_json()
            .replace("#ffffff", "#zzz");
        assert!(Theme::parse(&json).is_err(), "bad hex must be rejected");
    }

    #[test]
    fn injected_css_carries_tokens_and_caption_height() {
        let mut theme = Theme::default();
        theme.light.accent = ColorHex(0xff0000);
        theme.light.caption_height = 40;
        theme.custom_css = "/* custom */".into();

        let css = theme.injected_css(false);
        assert!(css.contains("#ff0000"), "accent must appear: {css}");
        assert!(css.contains("--dsh-shell-caption-height: 40px"), "{css}");
        assert!(
            css.contains("padding-top: var(--dsh-shell-caption-height)"),
            "{css}"
        );
        // custom_css must be last so it can override the generated rules.
        assert!(css.trim_end().ends_with("/* custom */"), "{css}");
    }

    #[test]
    fn appearance_selects_the_matching_palette() {
        let mut theme = Theme::default();

        // Default is `system`, so the OS decides.
        assert_eq!(theme.appearance, Appearance::System);
        assert_eq!(
            theme.palette_for(true).background.0,
            0x151517,
            "system + dark must resolve to the DeepSeek dark background"
        );
        assert_eq!(
            theme.palette_for(false).background.0,
            0xffffff,
            "system + light must resolve to the DeepSeek light background"
        );

        // An explicit preference pins it regardless of the OS.
        theme.appearance = Appearance::Dark;
        assert_eq!(theme.palette_for(false).background.0, 0x151517);
        theme.appearance = Appearance::Light;
        assert_eq!(theme.palette_for(true).background.0, 0xffffff);
    }

    #[test]
    fn css_reports_the_resolved_color_scheme() {
        let theme = Theme::default();
        assert!(theme.injected_css(true).contains("color-scheme: dark"));
        assert!(theme.injected_css(false).contains("color-scheme: light"));
    }

    #[test]
    fn palettes_use_deepseek_source_colors() {
        // These come from DSH's own boot theme CSS. Regressing them would
        // reintroduce a visible seam between the window chrome and the page.
        let dark = Palette::deepseek_dark();
        assert_eq!(dark.background.0, 0x151517);
        assert_eq!(dark.text.0, 0xf9fafb);
        assert_eq!(dark.text_muted.0, 0xadb2b8);

        let light = Palette::deepseek_light();
        assert_eq!(light.background.0, 0xffffff);
        assert_eq!(light.text.0, 0x0f1115);
        assert_eq!(light.text_muted.0, 0x81858c);
    }

    #[test]
    fn reload_reports_only_real_changes() {
        let dir = std::env::temp_dir().join("dsh-shell-theme-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("theme.json");
        std::fs::write(&path, Theme::default().to_pretty_json()).unwrap();

        let src = ThemeSource::new(&path);
        assert!(src.reload().is_none(), "unchanged file must not report a change");

        let mut t = Theme::default();
        t.light.accent = ColorHex(0x00ff00);
        std::fs::write(&path, t.to_pretty_json()).unwrap();
        let got = src.reload().expect("change must be reported");
        assert_eq!(got.light.accent.0, 0x00ff00);

        // A broken edit must be rejected and the last good theme retained.
        std::fs::write(&path, "{ broken").unwrap();
        assert!(src.reload().is_none(), "broken file must be rejected");
        assert_eq!(src.current().light.accent.0, 0x00ff00, "last good retained");

        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// The live theme plus its file path, shared with the watcher.
pub struct ThemeSource {
    pub path: PathBuf,
    /// Last successfully loaded theme, so a broken edit keeps the current look
    /// on screen instead of blanking the app.
    pub last_good: Arc<Mutex<Arc<Theme>>>,
}

impl ThemeSource {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let source = ThemeSource {
            path,
            last_good: Arc::new(Mutex::new(Arc::new(Theme::default()))),
        };
        source.ensure_file_exists();
        let _ = source.reload();
        source
    }

    fn ensure_file_exists(&self) {
        if self.path.exists() {
            return;
        }
        if let Some(parent) = self.path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(&self.path, Theme::default().to_pretty_json());
    }

    /// Reload from disk. Returns `Some` only when the visible theme changed, so
    /// callers can skip redundant work.
    pub fn reload(&self) -> Option<Arc<Theme>> {
        let raw = match std::fs::read_to_string(&self.path) {
            Ok(raw) => raw,
            Err(err) => {
                tracing::warn!(path = %self.path.display(), %err, "theme unreadable; keeping last good");
                return None;
            }
        };

        match Theme::parse(&raw) {
            Ok(theme) => {
                let theme = Arc::new(theme);
                let mut last = self.last_good.lock().unwrap();
                if last.as_ref() == &*theme {
                    return None; // No visible change.
                }
                tracing::info!(
                    appearance = ?theme.appearance,
                    light_bg = %theme.light.background.css(),
                    dark_bg = %theme.dark.background.css(),
                    "theme reloaded"
                );
                *last = theme.clone();
                Some(theme)
            }
            Err(err) => {
                tracing::warn!(path = %self.path.display(), %err, "theme rejected; keeping last good");
                None
            }
        }
    }

    pub fn current(&self) -> Arc<Theme> {
        self.last_good.lock().unwrap().clone()
    }

    pub fn clone_for_watcher(&self) -> ThemeSource {
        ThemeSource {
            path: self.path.clone(),
            last_good: self.last_good.clone(),
        }
    }
}

/// Watch `theme.json`, invoking `on_change` on a dedicated thread.
///
/// The callback runs on the watcher thread, so the caller decides how to reach
/// the UI thread — this module deliberately does not assume a UI toolkit.
pub fn spawn_watcher<F>(source: ThemeSource, on_change: F) -> Option<notify::RecommendedWatcher>
where
    F: Fn(Arc<Theme>) + Send + 'static,
{
    use notify::{RecursiveMode, Watcher};

    let (tx, rx) = std::sync::mpsc::channel::<()>();
    let watch_dir = source
        .path
        .parent()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));

    let mut watcher = match notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        match res {
            Ok(event) => {
                // Editors save via temp file + rename, so match on the final
                // path rather than the event kind.
                if event.paths.iter().any(|p| p.ends_with("theme.json")) {
                    let _ = tx.send(());
                }
            }
            Err(err) => tracing::warn!(%err, "theme watcher error"),
        }
    }) {
        Ok(w) => w,
        Err(err) => {
            tracing::warn!(%err, "could not start theme watcher; hot reload disabled");
            return None;
        }
    };

    if let Err(err) = watcher.watch(&watch_dir, RecursiveMode::NonRecursive) {
        tracing::warn!(path = %watch_dir.display(), %err, "could not watch theme directory");
        return None;
    }

    std::thread::Builder::new()
        .name("theme-watch".into())
        .spawn(move || {
            // Blocking receives belong on this thread, never on the UI thread.
            while rx.recv().is_ok() {
                // Debounce: one save can emit several events.
                std::thread::sleep(std::time::Duration::from_millis(80));
                while rx.try_recv().is_ok() {}
                if let Some(theme) = source.reload() {
                    on_change(theme);
                }
            }
        })
        .expect("spawn theme watch thread");

    Some(watcher)
}
