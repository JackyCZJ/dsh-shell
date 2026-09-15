//! Shell configuration, read from DSH's own settings document.
//!
//! The shell does **not** keep its own config file. Its settings live in
//! `$DSH_HOME/settings.yaml` under the `dsh-shell` namespace, registered by the
//! `dsh-plugin-shell-bridge` Host plugin. That is the same arrangement the
//! official DSH desktop app uses for its own `dsh-desktop` namespace, and it
//! means DSH owns validation and persistence while the shell only reads.
//!
//! Reads come from the file directly, so the shell can start and theme itself
//! before the Host is up. Live changes arrive either from the file watcher (a
//! hand edit) or as a pushed `settings` event from the plugin (a UI write).
//!
//! Palette values come from DSH's own boot-theme CSS, so the window chrome and
//! the page share one colour and the seam between them disappears.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

/// The namespace this crate reads from the settings document.
pub const SETTINGS_NAMESPACE: &str = "dsh-shell";

/// A color stored as `#rrggbb`.
///
/// Parsing is strict so a typo surfaces as a readable error rather than a
/// silently black window.
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
    pub fn css(&self) -> String {
        format!("#{:06x}", self.0)
    }
}

/// One palette. Field names match the settings document, which is camelCase.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub struct Palette {
    pub background: ColorHex,
    pub surface: ColorHex,
    pub surface_hover: ColorHex,
    pub border: ColorHex,
    pub text: ColorHex,
    pub text_muted: ColorHex,
    pub accent: ColorHex,
}

impl Default for Palette {
    /// The light palette, used to fill any key a hand-written section omits.
    fn default() -> Self {
        Palette::deepseek_light()
    }
}

impl Palette {
    /// The dark palette, taken from DSH's own dark boot theme.
    pub fn deepseek_dark() -> Palette {
        Palette {
            background: ColorHex(0x151517),
            surface: ColorHex(0x2c2c2e),
            surface_hover: ColorHex(0x3a3a3c),
            border: ColorHex(0x3f3f42),
            text: ColorHex(0xf9fafb),
            text_muted: ColorHex(0xadb2b8),
            accent: ColorHex(0x4176e6),
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
        }
    }
}

/// The shell's settings, mirroring the `dsh-shell` namespace.
///
/// Every field has a default, so a partial or absent section still yields a
/// usable theme rather than an error.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct Theme {
    /// The global shortcut that summons the window, e.g. `"meta+shift+D"`.
    pub hotkey: String,
    /// Height of the draggable caption strip, in CSS pixels.
    pub caption_height: u32,
    /// Inset of the traffic lights from the window's top-left.
    pub traffic_light_inset_x: f64,
    pub traffic_light_inset_y: f64,
    #[serde(default = "Palette::deepseek_light")]
    pub light: Palette,
    #[serde(default = "Palette::deepseek_dark")]
    pub dark: Palette,
    /// Extra CSS appended after the generated rules.
    pub custom_css: String,
}

impl Default for Theme {
    fn default() -> Self {
        Theme {
            hotkey: "meta+shift+D".to_string(),
            caption_height: 34,
            // Matches the default macOS inset so the buttons look native until
            // the user moves them.
            traffic_light_inset_x: 20.0,
            traffic_light_inset_y: 20.0,
            light: Palette::deepseek_light(),
            dark: Palette::deepseek_dark(),
            custom_css: String::new(),
        }
    }
}

impl Theme {
    /// Resolve the active palette for a resolved appearance.
    ///
    /// The caller decides whether it is dark; see `ThemeSource::is_dark`, which
    /// follows DSH's own preference before the OS.
    pub fn palette_for(&self, is_dark: bool) -> &Palette {
        if is_dark {
            &self.dark
        } else {
            &self.light
        }
    }

    /// Read DSH's own `ui-theme.preference` from the same document.
    ///
    /// Returns `None` for `system` or a missing key, which means the OS decides.
    /// This is the value DSH itself applies to the page, so the chrome follows it.
    pub fn dsh_preference_from_yaml(yaml: &str) -> Option<bool> {
        let document: serde_norway::Value = serde_norway::from_str(yaml).ok()?;
        let preference = document
            .get("ui-theme")?
            .get("preference")?
            .as_str()?
            .trim()
            .to_ascii_lowercase();
        match preference.as_str() {
            "dark" => Some(true),
            "light" => Some(false),
            _ => None,
        }
    }

    /// Extract the `dsh-shell` section from a DSH settings document.
    ///
    /// Returns the defaults when the document has no such section, which is the
    /// normal state before the plugin has registered anything.
    pub fn from_settings_yaml(yaml: &str) -> Result<Theme, String> {
        let document: serde_norway::Value =
            serde_norway::from_str(yaml).map_err(|err| format!("settings.yaml: {err}"))?;

        match document.get(SETTINGS_NAMESPACE) {
            // Defaults, not an error: a fresh install has no section yet and the
            // shell must still start.
            None => Ok(Theme::default()),
            Some(section) => serde_norway::from_value(section.clone())
                .map_err(|err| format!("{SETTINGS_NAMESPACE}: {err}")),
        }
    }

    /// The CSS injected into the web UI for the resolved palette.
    ///
    /// DSH's own theme system owns the real design tokens; this block exists so
    /// the shell can match the page to its chrome — background, and leaving room
    /// for the caption strip — without fighting it. It also mirrors DSH's own
    /// dark-mode marker so page and window agree on appearance.
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
            caption = self.caption_height,
            custom = self.custom_css,
        )
    }
}

/// Locate `$DSH_HOME/settings.yaml`.
///
/// `DSH_SETTINGS` overrides the whole path; otherwise `DSH_HOME`, then the
/// default `~/.dsh` location.
pub fn settings_path() -> Option<PathBuf> {
    if let Ok(explicit) = std::env::var("DSH_SETTINGS") {
        return Some(PathBuf::from(explicit));
    }
    if let Ok(home) = std::env::var("DSH_HOME") {
        return Some(PathBuf::from(home).join("settings.yaml"));
    }
    std::env::var("HOME")
        .ok()
        .map(|h| PathBuf::from(h).join(".dsh").join("settings.yaml"))
}

/// Reads the shell's settings from DSH's document, holding the last good value.
///
/// DSH owns the file; this type only ever reads. Writes go through the Host
/// plugin so DSH performs validation and preserves the rest of the document.
pub struct ThemeSource {
    pub path: PathBuf,
    /// Last successfully loaded theme, so a broken or partial document keeps the
    /// current look on screen instead of blanking the app.
    last_good: Arc<Mutex<Arc<Theme>>>,
    /// DSH's own `ui-theme.preference`, when it names light or dark.
    ///
    /// Read from the same document because it is what DSH applies to the page:
    /// the chrome must resolve appearance from it too, or the two disagree.
    dsh_prefers_dark: Arc<Mutex<Option<bool>>>,
}

impl ThemeSource {
    /// Open the settings document, loading the current value.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let source = ThemeSource {
            path,
            last_good: Arc::new(Mutex::new(Arc::new(Theme::default()))),
            dsh_prefers_dark: Arc::new(Mutex::new(None)),
        };
        let _ = source.reload();
        source
    }

    /// Re-read the document. Returns `Some` only when the value changed, so
    /// callers can skip redundant work.
    pub fn reload(&self) -> Option<Arc<Theme>> {
        let raw = match std::fs::read_to_string(&self.path) {
            Ok(raw) => raw,
            Err(err) => {
                // A missing file is normal before DSH has ever run; keep the
                // defaults rather than warning on every start.
                if err.kind() != std::io::ErrorKind::NotFound {
                    tracing::warn!(path = %self.path.display(), %err, "settings unreadable");
                }
                return None;
            }
        };

        // DSH's preference is read even when our own section is unchanged, since
        // it decides which palette is active.
        let preference = Theme::dsh_preference_from_yaml(&raw);
        let preference_changed = {
            let mut current = self.dsh_prefers_dark.lock().unwrap();
            let changed = *current != preference;
            *current = preference;
            changed
        };

        match Theme::from_settings_yaml(&raw) {
            Ok(theme) => {
                let theme = Arc::new(theme);
                let mut last = self.last_good.lock().unwrap();
                if last.as_ref() == &*theme && !preference_changed {
                    return None; // No visible change.
                }
                tracing::info!(
                    dsh_prefers_dark = ?preference,
                    light_bg = %theme.light.background.css(),
                    dark_bg = %theme.dark.background.css(),
                    "settings reloaded"
                );
                *last = theme.clone();
                Some(theme)
            }
            Err(err) => {
                tracing::warn!(path = %self.path.display(), %err, "settings rejected; keeping last good");
                None
            }
        }
    }

    /// Apply a theme pushed by the Host plugin.
    ///
    /// Used when DSH persists a change: the write may land inside the watcher's
    /// debounce, and going through here keeps the in-memory value canonical.
    pub fn apply_pushed(&self, theme: Theme) -> Option<Arc<Theme>> {
        let theme = Arc::new(theme);
        let mut last = self.last_good.lock().unwrap();
        if last.as_ref() == &*theme {
            return None;
        }
        *last = theme.clone();
        Some(theme)
    }

    pub fn current(&self) -> Arc<Theme> {
        self.last_good.lock().unwrap().clone()
    }

    /// Whether the shell should render dark.
    ///
    /// DSH's `ui-theme.preference` wins when it is explicit; the OS is consulted
    /// only when DSH says `system` or says nothing.
    pub fn is_dark(&self) -> bool {
        self.dsh_prefers_dark
            .lock()
            .unwrap()
            .unwrap_or_else(crate::native::system_is_dark)
    }

    pub fn clone_for_watcher(&self) -> ThemeSource {
        ThemeSource {
            path: self.path.clone(),
            last_good: self.last_good.clone(),
            dsh_prefers_dark: self.dsh_prefers_dark.clone(),
        }
    }
}

/// Watch the settings document, invoking `on_change` on a dedicated thread.
///
/// The callback runs on the watcher thread; the caller decides how to reach the
/// UI thread. Blocking waits belong there, never on the UI thread.
pub fn spawn_watcher<F>(source: ThemeSource, on_change: F) -> Option<notify::RecommendedWatcher>
where
    F: Fn(Arc<Theme>) + Send + 'static,
{
    use notify::{RecursiveMode, Watcher};

    let (tx, rx) = std::sync::mpsc::channel::<()>();
    let watch_dir = source
        .path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));

    let file_name = source
        .path
        .file_name()
        .map(|n| n.to_os_string())
        .unwrap_or_else(|| "settings.yaml".into());

    let mut watcher = match notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        match res {
            Ok(event) => {
                // Editors and DSH both save via temp file + rename, so match on
                // the final path rather than the event kind.
                if event.paths.iter().any(|p| p.ends_with(&file_name)) {
                    let _ = tx.send(());
                }
            }
            Err(err) => tracing::warn!(%err, "settings watcher error"),
        }
    }) {
        Ok(w) => w,
        Err(err) => {
            tracing::warn!(%err, "could not start the settings watcher; hot reload disabled");
            return None;
        }
    };

    if let Err(err) = watcher.watch(&watch_dir, RecursiveMode::NonRecursive) {
        tracing::warn!(path = %watch_dir.display(), %err, "could not watch the settings directory");
        return None;
    }

    std::thread::Builder::new()
        .name("settings-watch".into())
        .spawn(move || {
            while rx.recv().is_ok() {
                // Debounce: one save can emit several filesystem events, and DSH
                // writes the whole document.
                std::thread::sleep(std::time::Duration::from_millis(120));
                while rx.try_recv().is_ok() {}
                if let Some(theme) = source.reload() {
                    on_change(theme);
                }
            }
        })
        .expect("spawn settings watch thread");

    Some(watcher)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A document shaped the way DSH writes it: other namespaces plus ours.
    const DOCUMENT: &str = r##"
ui-theme:
  preference: light
llm-pi-ai:
  providers:
    v2ex:
      apiKeyEnv: V2EX_API_KEY
dsh-shell:
  appearance: dark
  hotkey: meta+alt+K
  captionHeight: 40
  trafficLightInsetX: 12.5
  trafficLightInsetY: 30
  light:
    background: "#fafafa"
    surface: "#f0f0f0"
    surfaceHover: "#e0e0e0"
    border: "#cccccc"
    text: "#111111"
    textMuted: "#777777"
    accent: "#ff0000"
  dark:
    background: "#101010"
    surface: "#202020"
    surfaceHover: "#303030"
    border: "#404040"
    text: "#eeeeee"
    textMuted: "#999999"
    accent: "#00ff00"
  customCss: "body { opacity: 1; }"
"##;

    #[test]
    fn reads_the_namespace_from_a_real_document() {
        let theme = Theme::from_settings_yaml(DOCUMENT).expect("parse");
        assert_eq!(theme.hotkey, "meta+alt+K");
        assert_eq!(theme.caption_height, 40);
        assert_eq!(theme.traffic_light_inset_x, 12.5);
        assert_eq!(theme.traffic_light_inset_y, 30.0);
        assert_eq!(theme.light.accent.0, 0xff0000);
        assert_eq!(theme.dark.accent.0, 0x00ff00);
        assert_eq!(theme.light.surface_hover.0, 0xe0e0e0);
        assert_eq!(theme.dark.text_muted.0, 0x999999);
        assert_eq!(theme.custom_css, "body { opacity: 1; }");
    }

    #[test]
    fn other_namespaces_do_not_interfere() {
        // The document is shared with every other DSH plugin; reading must pick
        // out only our section.
        let theme = Theme::from_settings_yaml(DOCUMENT).unwrap();
        assert_eq!(theme.hotkey, "meta+alt+K", "read another namespace?");
    }

    #[test]
    fn a_missing_section_yields_defaults_not_an_error() {
        // A fresh install has no section until the plugin registers one, and the
        // shell must still start.
        let theme = Theme::from_settings_yaml("ui-theme:\n  preference: dark\n").expect("parse");
        assert_eq!(theme, Theme::default());
    }

    #[test]
    fn an_empty_document_yields_defaults() {
        assert_eq!(Theme::from_settings_yaml("").unwrap(), Theme::default());
    }

    #[test]
    fn a_partial_section_fills_in_defaults() {
        // Every field is defaulted, so a hand-written section with one key works.
        let theme =
            Theme::from_settings_yaml("dsh-shell:\n  hotkey: control+alt+J\n").expect("parse");
        assert_eq!(theme.hotkey, "control+alt+J");
        assert_eq!(theme.caption_height, Theme::default().caption_height);
        assert_eq!(theme.light, Palette::deepseek_light());
    }

    #[test]
    fn a_bad_colour_is_rejected_with_the_namespace_named() {
        let bad = "dsh-shell:\n  light:\n    accent: \"#zzz\"\n";
        let err = Theme::from_settings_yaml(bad).unwrap_err();
        assert!(err.contains(SETTINGS_NAMESPACE), "unhelpful error: {err}");
    }

    #[test]
    fn malformed_yaml_is_reported() {
        assert!(Theme::from_settings_yaml("dsh-shell: [broken").is_err());
    }

    #[test]
    fn palette_selection_follows_the_resolved_appearance() {
        let theme = Theme::default();
        assert_eq!(theme.palette_for(true).background.0, 0x151517);
        assert_eq!(theme.palette_for(false).background.0, 0xffffff);
    }

    #[test]
    fn the_wire_field_names_are_exactly_what_the_settings_page_uses() {
        // The settings page reads and writes these names. A rename here without
        // updating the page silently drops the field: serde ignores unknown keys
        // and the struct default fills the gap, producing a half-wrong palette
        // with no error. Pinning the names makes that a test failure instead.
        let value = serde_json::to_value(Theme::default()).expect("serialize");
        for key in [
            "hotkey",
            "captionHeight",
            "trafficLightInsetX",
            "trafficLightInsetY",
            "customCss",
            "light",
            "dark",
        ] {
            assert!(value.get(key).is_some(), "theme is missing {key}: {value}");
        }

        let palette = value["light"].as_object().expect("palette object");
        let mut names: Vec<&str> = palette.keys().map(|k| k.as_str()).collect();
        names.sort_unstable();
        assert_eq!(
            names,
            [
                "accent",
                "background",
                "border",
                "surface",
                "surfaceHover",
                "text",
                "textMuted",
            ],
            "palette field names changed; update assets/settings.html to match"
        );
    }

    #[test]
    fn a_snake_case_palette_key_is_ignored_rather_than_misread() {
        // This is the exact shape of the bug that produced a half-light dark
        // palette: `surface_hover` was sent, silently ignored, and the default
        // filled in a light colour. Pin the behaviour so the page-side test and
        // the wire-name test above are the guard.
        let config = serde_json::json!({
            "dark": { "surfaceHover": "#3a3a3c", "surface_hover": "#ff0000" },
        });
        let theme: Theme = serde_json::from_value(config).expect("parse");
        assert_eq!(theme.dark.surface_hover.0, 0x3a3a3c, "camelCase must win");
    }

    #[test]
    fn dsh_preference_is_read_from_the_same_document() {
        // This is what makes chrome and page agree: both follow DSH.
        assert_eq!(
            Theme::dsh_preference_from_yaml("ui-theme:\n  preference: dark\n"),
            Some(true)
        );
        assert_eq!(
            Theme::dsh_preference_from_yaml("ui-theme:\n  preference: light\n"),
            Some(false)
        );
        // `system` is not an answer; the OS decides.
        assert_eq!(
            Theme::dsh_preference_from_yaml("ui-theme:\n  preference: system\n"),
            None
        );
        assert_eq!(Theme::dsh_preference_from_yaml(""), None);
        assert_eq!(Theme::dsh_preference_from_yaml("dsh-shell: {}\n"), None);
    }

    #[test]
    fn injected_css_carries_tokens_and_caption_height() {
        let mut theme = Theme::default();
        theme.light.accent = ColorHex(0xff0000);
        theme.caption_height = 40;
        theme.custom_css = "/* custom */".into();

        let css = theme.injected_css(false);
        assert!(css.contains("#ff0000"));
        assert!(css.contains("--dsh-shell-caption-height: 40px"));
        assert!(css.contains("padding-top: var(--dsh-shell-caption-height)"));
        assert!(css.trim_end().ends_with("/* custom */"));
    }

    #[test]
    fn css_reports_the_resolved_color_scheme() {
        let theme = Theme::default();
        assert!(theme.injected_css(true).contains("color-scheme: dark"));
        assert!(theme.injected_css(false).contains("color-scheme: light"));
    }

    #[test]
    fn reload_reports_only_real_changes() {
        let dir = std::env::temp_dir().join("dsh-shell-settings-reload");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.yaml");
        std::fs::write(&path, "dsh-shell:\n  hotkey: meta+shift+D\n").unwrap();

        let src = ThemeSource::new(&path);
        assert!(src.reload().is_none(), "unchanged document must not report");

        std::fs::write(&path, "dsh-shell:\n  hotkey: meta+alt+K\n").unwrap();
        let got = src.reload().expect("change must be reported");
        assert_eq!(got.hotkey, "meta+alt+K");

        // A broken write must be rejected and the last good value retained.
        std::fs::write(&path, "dsh-shell: [broken").unwrap();
        assert!(src.reload().is_none(), "broken document must be rejected");
        assert_eq!(src.current().hotkey, "meta+alt+K");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_document_keeps_the_defaults() {
        let src = ThemeSource::new("/nonexistent/does/not/exist/settings.yaml");
        assert!(src.reload().is_none());
        assert_eq!(src.current().hotkey, Theme::default().hotkey);
    }

    #[test]
    fn a_pushed_theme_is_applied_without_touching_disk() {
        let src = ThemeSource::new("/nonexistent/does/not/exist/settings.yaml");
        let mut pushed = Theme::default();
        pushed.hotkey = "meta+alt+Z".into();

        let applied = src.apply_pushed(pushed.clone()).expect("must apply");
        assert_eq!(applied.hotkey, "meta+alt+Z");
        assert_eq!(src.current().hotkey, "meta+alt+Z");
        // The same value must not report a change twice.
        assert!(src.apply_pushed(pushed).is_none());
    }
}
