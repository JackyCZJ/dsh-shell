//! The shell-owned settings window.
//!
//! The `dsh-shell` settings namespace has enough fields (two palettes, layout,
//! hotkey) that hand-editing the document is not a reasonable way to configure
//! the shell. This module opens a second window with a form and submits changes
//! to the Host, which persists them through DSH's settings service.
//!
//! DSH owns persistence: the form's write becomes a request to the Host plugin,
//! which writes through the settings namespace. The shell then picks the change
//! up from the document, so a UI edit and a hand edit converge on one path and
//! DSH keeps ownership of validation.
//!
//! Validation lives in Rust rather than in the page: the same rules that guard a
//! hand-edited file must guard a typed one, or the UI becomes a way to write a
//! config the loader would reject.

use crate::theme::Theme;

/// The settings page, with a placeholder for the live theme.
const SETTINGS_HTML: &str = include_str!("../assets/settings.html");

/// Build the settings page with the current theme embedded.
///
/// The theme is injected as JSON into the page rather than fetched, so the form
/// is populated on first paint with no round trip. `host_connected` tells the
/// page whether a save can succeed at all: writes go through the Host, so with
/// no Host attached the Save button must explain itself rather than fail.
pub fn page(theme: &Theme, host_connected: bool) -> String {
    let json = serde_json::to_string(theme).unwrap_or_else(|_| "{}".into());
    // `</script>` inside the JSON would end the script block early. The theme
    // contains CSS text, which can legitimately include angle brackets.
    let safe = json.replace("</", "<\\/");
    SETTINGS_HTML
        .replace("window.__DSH_INITIAL__ || {}", &format!("{safe}"))
        .replace(
            "window.__DSH_HOST_CONNECTED__ || false",
            if host_connected { "true" } else { "false" },
        )
}

/// What the settings page asked us to do.
#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase", tag = "action")]
pub enum SettingsRequest {
    /// Validate and persist the given configuration.
    Save { config: serde_json::Value },
}

/// The outcome reported back to the page.
pub struct SaveOutcome {
    pub ok: bool,
    pub error: Option<String>,
}

impl SaveOutcome {
    /// The script that reports this outcome to the page.
    pub fn to_script(&self) -> String {
        let error = self
            .error
            .as_ref()
            .map(|e| serde_json::to_string(e).unwrap_or_else(|_| "null".into()))
            .unwrap_or_else(|| "null".into());
        format!(
            "window.__dshSettings && window.__dshSettings.saved({}, {});",
            self.ok, error
        )
    }
}

/// Parse a message from the settings page.
pub fn parse_request(body: &str) -> Result<SettingsRequest, String> {
    serde_json::from_str(body).map_err(|err| format!("malformed settings message: {err}"))
}

/// Validate a submitted configuration.
///
/// Validation mirrors what a hand-edited document must satisfy: colours must be
/// six-digit hex, the hotkey must parse, and numeric fields must be in range. A
/// submission that fails is rejected whole.
///
/// The shell checks before sending so an obviously wrong entry is reported
/// immediately; DSH validates again when it persists and remains the authority.
pub fn validate(config: &serde_json::Value) -> Result<Theme, String> {
    let theme: Theme = serde_json::from_value(config.clone())
        .map_err(|err| format!("invalid configuration: {err}"))?;

    // Colour components are validated by `ColorHex` during deserialization, but
    // the numeric fields need bounds the type cannot express.
    if !(16..=96).contains(&theme.caption_height) {
        return Err(format!(
            "captionHeight must be between 16 and 96, got {}",
            theme.caption_height
        ));
    }

    if !(0.0..=400.0).contains(&theme.traffic_light_inset_x)
        || !(0.0..=400.0).contains(&theme.traffic_light_inset_y)
    {
        return Err("trafficLightInsetX/Y must be between 0 and 400".into());
    }

    // Reject a shortcut the shell could not register, rather than writing a file
    // that silently falls back to the default on the next start.
    crate::native::HotkeySpec::parse(&theme.hotkey)
        .map_err(|err| format!("invalid shortcut: {err}"))?;

    if theme.custom_css.len() > 64 * 1024 {
        return Err("custom CSS is capped at 64 KB".into());
    }

    Ok(theme)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_config() -> serde_json::Value {
        serde_json::to_value(Theme::default()).expect("serialize default")
    }

    #[test]
    fn the_page_embeds_the_live_theme() {
        let mut theme = Theme::default();
        theme.hotkey = "meta+alt+K".into();
        theme.light.accent = crate::theme::ColorHex(0x123456);

        let html = page(&theme, true);
        assert!(html.contains("meta+alt+K"), "hotkey missing from the page");
        assert!(html.contains("#123456"), "accent missing from the page");
        assert!(
            html.contains("__DSH_HOST_CONNECTED__ || true") || html.contains("= true"),
            "connection flag was not substituted"
        );
        // The placeholder must be gone, or the page would fall back to {}.
        assert!(
            !html.contains("window.__DSH_INITIAL__ || {}"),
            "placeholder was not substituted"
        );
    }

    #[test]
    fn a_theme_containing_a_script_tag_cannot_break_out() {
        // custom_css is free text and can contain "</script>".
        let mut theme = Theme::default();
        theme.custom_css = "/* </script><script>alert(1)</script> */".into();
        let html = page(&theme, false);
        assert!(
            !html.contains("</script><script>alert(1)"),
            "a script tag in custom_css escaped the injection block"
        );
    }

    #[test]
    fn accepts_the_default_configuration() {
        let theme = validate(&valid_config()).expect("default must validate");
    }

    #[test]
    fn rejects_a_bad_colour() {
        let mut config = valid_config();
        config["light"]["accent"] = serde_json::json!("#zzzzzz");
        let err = validate(&config).unwrap_err();
        assert!(err.contains("invalid configuration"), "unexpected: {err}");
    }

    #[test]
    fn rejects_an_unparseable_hotkey() {
        let mut config = valid_config();
        config["hotkey"] = serde_json::json!("nonsense");
        let err = validate(&config).unwrap_err();
        assert!(err.contains("invalid shortcut"), "unexpected: {err}");
    }

    #[test]
    fn rejects_a_hotkey_without_a_modifier() {
        // Must be caught here, not silently downgraded to the default.
        let mut config = valid_config();
        config["hotkey"] = serde_json::json!("D");
        let err = validate(&config).unwrap_err();
        assert!(err.contains("no modifier"), "unexpected: {err}");
    }

    #[test]
    fn rejects_out_of_range_layout_values() {
        let mut config = valid_config();
        config["captionHeight"] = serde_json::json!(4);
        assert!(validate(&config).unwrap_err().contains("captionHeight"));

        let mut config = valid_config();
        config["trafficLightInsetX"] = serde_json::json!(9000.0);
        assert!(validate(&config).unwrap_err().contains("trafficLightInsetX"));
    }

    #[test]
    fn rejects_oversized_custom_css() {
        let mut config = valid_config();
        config["customCss"] = serde_json::json!("x".repeat(70 * 1024));
        assert!(validate(&config).unwrap_err().contains("64 KB"));
    }

    #[test]
    fn rejects_a_structurally_wrong_message() {
        assert!(parse_request("not json").is_err());
        assert!(parse_request(r#"{"action":"nope"}"#).is_err());
        assert!(parse_request(r#"{"action":"save","config":{}}"#).is_ok());
    }

    #[test]
    fn a_valid_configuration_survives_a_json_round_trip() {
        // The settings page sends JSON, which is what validate() consumes; the
        // result is what gets forwarded to the Host to persist.
        let config = serde_json::to_value(Theme::default()).expect("serialize");
        let theme = validate(&config).expect("default must validate");
        assert_eq!(theme, Theme::default());

        // And back out again, so the shell can push the canonical form to the page.
        let again = serde_json::to_value(&theme).unwrap();
        assert_eq!(validate(&again).unwrap(), theme);
    }

    #[test]
    fn the_wire_names_are_camel_case() {
        // The settings document is camelCase, so the JSON crossing the wire must
        // be too. A snake_case name would silently fail to persist.
        let json = serde_json::to_value(Theme::default()).unwrap();
        for key in ["captionHeight", "trafficLightInsetX", "customCss"] {
            assert!(json.get(key).is_some(), "missing {key} in {json}");
        }
        assert!(json["light"].get("surfaceHover").is_some());

        // And it must parse back from those names.
        let config = serde_json::json!({
            "captionHeight": 44,
            "trafficLightInsetX": 11.0,
            "customCss": "/* x */",
            "light": { "surfaceHover": "#abcdef" },
        });
        let theme = validate(&config).expect("camelCase config must validate");
        assert_eq!(theme.caption_height, 44);
        assert_eq!(theme.traffic_light_inset_x, 11.0);
        assert_eq!(theme.light.surface_hover.0, 0xabcdef);
        // Omitted fields fall back to defaults rather than failing.
        assert_eq!(theme.hotkey, Theme::default().hotkey);
    }
}
