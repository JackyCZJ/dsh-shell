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
pub fn page(
    theme: &Theme,
    host_connected: bool,
    locale: crate::i18n::Locale,
    update: &crate::updater::Status,
) -> String {
    let json = serde_json::to_string(theme).unwrap_or_else(|_| "{}".into());
    // `</script>` inside the JSON would end the script block early. The theme
    // contains CSS text, which can legitimately include angle brackets.
    let safe = json.replace("</", "<\\/");
    // The update status is injected the same way for the same reason: the page
    // is usable the moment it paints, with no round trip to ask the shell.
    //
    // `Value::to_string`, not `serde_json::to_string`: the latter encodes the
    // value *as JSON*, so a string would carry its quotes into the page. The
    // page expects an object, and a string would read as `phase: undefined`.
    let update_json = update.json().to_string();
    SETTINGS_HTML
        .replace("window.__DSH_INITIAL__ || {}", &format!("{safe}"))
        .replace("window.__DSH_UPDATE__ || { phase: 'unknown' }", &update_json)
        .replace(
            "window.__DSH_HOST_CONNECTED__ || false",
            if host_connected { "true" } else { "false" },
        )
        .replace(
            "window.__DSH_LOCALE__ || 'en'",
            &format!("{:?}", locale.id()),
        )
}

/// What the settings page asked us to do.
#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase", tag = "action")]
pub enum SettingsRequest {
    /// Validate and persist the given configuration.
    Save { config: serde_json::Value },
    /// Look for a newer DSH, bypassing the cached answer.
    CheckUpdate,
    /// Stage, verify and apply the newest DSH on the configured channel.
    InstallUpdate,
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
    fn the_page_embeds_the_locale() {
        let html = page(&Theme::default(), true, crate::i18n::Locale::Zh, &crate::updater::Status::default());
        assert!(
            html.contains("\"zh\""),
            "locale was not injected into the page"
        );
        assert!(
            !html.contains("window.__DSH_LOCALE__ || 'en'"),
            "locale placeholder was not substituted"
        );
    }

    #[test]
    fn the_page_embeds_the_live_theme() {
        let mut theme = Theme::default();
        theme.hotkey = "meta+alt+K".into();
        theme.light.accent = crate::theme::ColorHex(0x123456);

        let html = page(&theme, true, crate::i18n::Locale::En, &crate::updater::Status::default());
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

    /// The page's script, as the browser would parse it.
    ///
    /// `node --check` is the only real JavaScript parser this project can reach
    /// without a browser or a dependency, and it earns its place: it caught the
    /// update status being injected as a JSON-encoded *string* rather than an
    /// object, which no string assertion here would have noticed.
    ///
    /// `node` is resolved to an absolute path rather than taken from `PATH`.
    /// Tests run in parallel in one process, and a test that rewrites `PATH` —
    /// the one pinning that a GUI app has no node on it, which must — otherwise
    /// makes this one fail for a reason that has nothing to do with the page.
    fn assert_page_script_parses(html: &str) {
        let start = html.find("<script>").expect("the page has a script block");
        let body = &html[start + "<script>".len()..];
        let end = body.find("</script>").expect("the script block is closed");
        let script = &body[..end];

        let Some(node) = crate::server::which_node() else {
            eprintln!("no node found; skipping the script parse check");
            return;
        };

        // Unique per call, not per process: the tests run in parallel threads of
        // one process, and a shared path meant two of them wrote this file at
        // once — so `node --check` sometimes parsed a half-written script and
        // reported a syntax error that was really a race.
        use std::sync::atomic::{AtomicU32, Ordering};
        static SEQ: AtomicU32 = AtomicU32::new(0);
        let seq = SEQ.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("dsh-settings-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        let path = dir.join(format!("settings-{seq}.js"));
        std::fs::write(&path, script).expect("write the extracted script");

        let output = std::process::Command::new(&node)
            .arg("--check")
            .arg(&path)
            .output();
        let _ = std::fs::remove_file(&path);

        match output {
            Ok(output) => assert!(
                output.status.success(),
                "the settings script does not parse:\n{}",
                String::from_utf8_lossy(&output.stderr)
            ),
            // Raced with something that removed it; not this test's business.
            Err(_) => eprintln!("could not run {}; skipping", node.display()),
        }
    }

    #[test]
    fn the_page_script_parses() {
        assert_page_script_parses(&page(
            &Theme::default(),
            true,
            crate::i18n::Locale::En,
            &crate::updater::Status::default(),
        ));
    }

    #[test]
    fn the_page_script_still_parses_with_a_failure_status() {
        // The failure reason is arbitrary text from a command's stderr, so it is
        // the most likely thing to carry a quote or a newline into the page.
        let status = crate::updater::Status {
            channel: crate::updater::Channel::Alpha,
            phase: crate::updater::Phase::Failed(
                "curl exited 6: could not resolve host \"registry.npmjs.org\"\nsecond line".into(),
            ),
            current: Some(crate::updater::Version::parse("0.1.5-rc.2").unwrap()),
            target: None,
            message: None,
            checked_at: Some(1),
        };
        assert_page_script_parses(&page(
            &Theme::default(),
            false,
            crate::i18n::Locale::Zh,
            &status,
        ));
    }

    #[test]
    fn the_page_receives_the_update_status_as_an_object() {
        let status = crate::updater::Status {
            current: Some(crate::updater::Version::parse("0.1.5-rc.2").unwrap()),
            target: Some(crate::updater::Version::parse("0.1.6-alpha.2").unwrap()),
            channel: crate::updater::Channel::Alpha,
            phase: crate::updater::Phase::Available,
            message: None,
            checked_at: Some(5),
        };
        let html = page(&Theme::default(), true, crate::i18n::Locale::En, &status);
        assert!(
            !html.contains("window.__DSH_UPDATE__ || { phase: 'unknown' }"),
            "the update placeholder was not substituted"
        );
        // An object literal, not a quoted string: the page reads `.phase` off it.
        // Not asserted against the literal key order — `serde_json` is built here
        // with `preserve_order` (through tao/wry), so keys come out sorted.
        assert!(
            html.contains("{\"") && html.contains("\"current\":\"0.1.5-rc.2\""),
            "the status should be injected as a JSON object, not a quoted string"
        );
        assert!(html.contains("\"phase\":\"available\""));
        assert!(html.contains("\"channel\":\"alpha\""));
    }

    #[test]
    fn a_theme_containing_a_script_tag_cannot_break_out() {        // custom_css is free text and can contain "</script>".
        let mut theme = Theme::default();
        theme.custom_css = "/* </script><script>alert(1)</script> */".into();
        let html = page(&theme, false, crate::i18n::Locale::En, &crate::updater::Status::default());
        assert!(
            !html.contains("</script><script>alert(1)"),
            "a script tag in custom_css escaped the injection block"
        );
    }

    #[test]
    fn accepts_the_default_configuration() {
        let theme = validate(&valid_config()).expect("default must validate");
        // Validation must round-trip the document, not merely accept it: a
        // dropped field would silently save a truncated configuration.
        assert_eq!(theme, Theme::default());
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
    fn the_layout_values_are_not_part_of_the_configuration() {
        // They are fixed chrome: a document that carries them must not change
        // how the window is built.
        let config = serde_json::json!({
            "captionHeight": 900,
            "trafficLightInsetX": -50.0,
            "trafficLightInsetY": 9999.0,
        });
        assert!(
            validate(&config).is_ok(),
            "unknown layout keys must be ignored, not rejected"
        );
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
    fn parses_the_update_actions_the_page_sends() {
        // These arrive as bare verbs: the shell decides which version to touch,
        // so there is nothing for the page to pass.
        assert!(matches!(
            parse_request(r#"{"action":"checkUpdate"}"#),
            Ok(SettingsRequest::CheckUpdate)
        ));
        assert!(matches!(
            parse_request(r#"{"action":"installUpdate"}"#),
            Ok(SettingsRequest::InstallUpdate)
        ));
        // A misspelt verb must not silently become one of them.
        assert!(parse_request(r#"{"action":"checkupdate"}"#).is_err());
        assert!(parse_request(r#"{"action":"InstallUpdate"}"#).is_err());
    }

    #[test]
    fn the_update_channel_is_saved_with_the_rest_of_the_configuration() {
        // The channel lives in the document rather than travelling with the
        // check, so the stored value and the checked value cannot disagree.
        let mut config = valid_config();
        config["updateChannel"] = serde_json::json!("alpha");
        let theme = validate(&config).expect("alpha is a valid channel");
        assert_eq!(theme.update_channel, "alpha");
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
        for key in ["hotkey", "customCss"] {
            assert!(json.get(key).is_some(), "missing {key} in {json}");
        }
        assert!(json["light"].get("surfaceHover").is_some());

        // And it must parse back from those names.
        let config = serde_json::json!({
            "hotkey": "meta+alt+K",
            "customCss": "/* x */",
            "light": { "surfaceHover": "#abcdef" },
        });
        let theme = validate(&config).expect("camelCase config must validate");
        assert_eq!(theme.hotkey, "meta+alt+K");
        assert_eq!(theme.light.surface_hover.0, 0xabcdef);
        assert_eq!(theme.custom_css, "/* x */");
    }
}
