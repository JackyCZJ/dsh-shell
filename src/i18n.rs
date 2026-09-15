//! Shell localisation.
//!
//! The shell follows DSH's own `locale.preference` from `settings.yaml`, the
//! same document it reads its configuration from. A user who has set DSH to
//! Chinese should not find the shell's tray, menus, and settings window in
//! English — and vice versa.
//!
//! Strings are held in a struct rather than looked up by key so a missing
//! translation is a compile error instead of a blank label at runtime.

use serde::Deserialize;

/// The languages the shell speaks. Mirrors DSH's own `LOCALE_IDS`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Locale {
    Zh,
    En,
}

impl Default for Locale {
    /// English, because a machine with no DSH home has no stated preference and
    /// English is the safer default for an unknown reader.
    fn default() -> Self {
        Locale::En
    }
}

impl Locale {
    /// Parse a DSH locale id, tolerating a region suffix (`zh-CN`, `en_US`).
    pub fn parse(id: &str) -> Option<Locale> {
        let primary = id
            .trim()
            .split(['-', '_'])
            .next()
            .unwrap_or("")
            .to_ascii_lowercase();
        match primary.as_str() {
            "zh" => Some(Locale::Zh),
            "en" => Some(Locale::En),
            _ => None,
        }
    }

    /// Read DSH's `locale.preference` from the settings document.
    pub fn from_settings_yaml(yaml: &str) -> Option<Locale> {
        let document: serde_norway::Value = serde_norway::from_str(yaml).ok()?;
        let preference = document
            .get("locale")?
            .get("preference")?
            .as_str()?;
        Locale::parse(preference)
    }

    pub fn strings(self) -> &'static Strings {
        match self {
            Locale::En => &EN,
            Locale::Zh => &ZH,
        }
    }

    /// The id the pages receive, matching DSH's own vocabulary.
    pub fn id(self) -> &'static str {
        match self {
            Locale::En => "en",
            Locale::Zh => "zh",
        }
    }
}

/// Every string the native chrome needs.
#[derive(Debug)]
pub struct Strings {
    // Tray
    pub show_window: &'static str,
    pub settings: &'static str,
    pub quit: &'static str,
    pub agent_prefix: &'static str,
    pub agent_idle: &'static str,
    pub agent_working: &'static str,
    pub agent_failed: &'static str,

    // Application menu
    pub menu_edit: &'static str,
    pub menu_view: &'static str,
    pub menu_window: &'static str,
    pub menu_about: &'static str,
    pub menu_hide: &'static str,
    pub menu_reload: &'static str,
    pub menu_close_window: &'static str,

    // Notifications
    pub notify_finished: &'static str,
    pub notify_failed: &'static str,
    pub notify_done: &'static str,
    pub notify_errored: &'static str,
    pub notify_stopped: &'static str,
    pub notification_app: &'static str,

    // Windows
    pub settings_window_title: &'static str,
}

static EN: Strings = Strings {
    show_window: "Show Window",
    settings: "Settings…",
    quit: "Quit",
    agent_prefix: "Agent",
    agent_idle: "idle",
    agent_working: "working",
    agent_failed: "failed",

    menu_edit: "Edit",
    menu_view: "View",
    menu_window: "Window",
    menu_about: "About {app}",
    menu_hide: "Hide {app}",
    menu_reload: "Reload",
    menu_close_window: "Close Window",

    notify_finished: "DSH: {session} finished",
    notify_failed: "DSH: {session} failed",
    notify_done: "The agent is done.",
    notify_errored: "The agent hit an error.",
    notify_stopped: "Stopped: {reason}",
    notification_app: "DSH Shell",

    settings_window_title: "DSH Shell Settings",
};

static ZH: Strings = Strings {
    show_window: "显示窗口",
    settings: "设置…",
    quit: "退出",
    agent_prefix: "代理",
    agent_idle: "空闲",
    agent_working: "工作中",
    agent_failed: "失败",

    menu_edit: "编辑",
    menu_view: "显示",
    menu_window: "窗口",
    menu_about: "关于 {app}",
    menu_hide: "隐藏 {app}",
    menu_reload: "重新加载",
    menu_close_window: "关闭窗口",

    notify_finished: "DSH：{session} 已完成",
    notify_failed: "DSH：{session} 失败",
    notify_done: "代理已完成。",
    notify_errored: "代理遇到错误。",
    notify_stopped: "已停止：{reason}",
    notification_app: "DSH Shell",

    settings_window_title: "DSH Shell 设置",
};

/// Fill a `{placeholder}` in a template.
///
/// A tiny substitution rather than a formatting library: the templates are
/// fixed strings in this file, so the only risk is a typo, and a missing
/// placeholder leaves the template visible rather than panicking.
pub fn fill(template: &str, key: &str, value: &str) -> String {
    template.replace(&format!("{{{key}}}"), value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_dsh_locale_ids() {
        assert_eq!(Locale::parse("zh"), Some(Locale::Zh));
        assert_eq!(Locale::parse("en"), Some(Locale::En));
        // Region suffixes are tolerated so `zh-CN` does not fall through to the
        // default and silently give a Chinese user an English shell.
        assert_eq!(Locale::parse("zh-CN"), Some(Locale::Zh));
        assert_eq!(Locale::parse("en_US"), Some(Locale::En));
        assert_eq!(Locale::parse(" zh "), Some(Locale::Zh));
        assert_eq!(Locale::parse("fr"), None);
        assert_eq!(Locale::parse(""), None);
    }

    #[test]
    fn reads_the_preference_from_a_real_document() {
        let document = "\
locale:
  preference: zh
ui-theme:
  preference: light
dsh-shell:
  hotkey: meta+shift+D
";
        assert_eq!(Locale::from_settings_yaml(document), Some(Locale::Zh));
    }

    #[test]
    fn a_missing_or_unknown_locale_is_not_an_error() {
        assert_eq!(Locale::from_settings_yaml("ui-theme:\n  preference: dark\n"), None);
        assert_eq!(Locale::from_settings_yaml(""), None);
        // A language the shell does not speak falls back rather than failing.
        assert_eq!(Locale::from_settings_yaml("locale:\n  preference: fr\n"), None);
    }

    #[test]
    fn both_locales_are_complete() {
        // Every field is a `&'static str` in a struct, so a missing translation
        // is a compile error. This asserts the two are genuinely different, so a
        // copy-paste that left English in the Chinese table is caught.
        let en = Locale::En.strings();
        let zh = Locale::Zh.strings();
        assert_ne!(en.quit, zh.quit);
        assert_ne!(en.show_window, zh.show_window);
        assert_ne!(en.menu_edit, zh.menu_edit);
        assert_ne!(en.notify_done, zh.notify_done);
        assert_ne!(en.settings_window_title, zh.settings_window_title);
    }

    #[test]
    fn placeholders_are_filled() {
        let en = Locale::En.strings();
        assert_eq!(
            fill(en.notify_finished, "session", "abc123"),
            "DSH: abc123 finished"
        );
        let zh = Locale::Zh.strings();
        assert_eq!(fill(zh.notify_finished, "session", "abc123"), "DSH：abc123 已完成");
    }

    #[test]
    fn every_template_keeps_its_placeholder() {
        // A translation that drops `{session}` or `{app}` would silently lose
        // the only identifying part of the message.
        for locale in [Locale::En, Locale::Zh] {
            let s = locale.strings();
            assert!(s.notify_finished.contains("{session}"), "{locale:?}");
            assert!(s.notify_failed.contains("{session}"), "{locale:?}");
            assert!(s.notify_stopped.contains("{reason}"), "{locale:?}");
            assert!(s.menu_about.contains("{app}"), "{locale:?}");
            assert!(s.menu_hide.contains("{app}"), "{locale:?}");
        }
    }
}
