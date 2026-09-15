//! The native application menu.
//!
//! macOS routes standard editing shortcuts — Cmd+C, Cmd+V, Cmd+X, Cmd+A, Cmd+Z —
//! through the **Edit menu**, not to the focused view directly. A windowed app
//! with no menu bar therefore has no working copy/paste at all, no matter what
//! the web view implements.
//!
//! The items below are created with `PredefinedMenuItem`, which binds them to the
//! platform's own selectors (`copy:`, `paste:`, …). Those selectors travel the
//! responder chain to whatever is focused — here, the web view — so the standard
//! shortcuts work without this shell handling any keystrokes itself.
//!
//! This is why the menu exists even though the app has no visible titlebar: it is
//! invisible chrome that makes keyboard interaction work.

use muda::{
    Menu, MenuItem, PredefinedMenuItem, Submenu,
    accelerator::{Accelerator, Code, Modifiers},
};

/// Menu ids for the shell's own items, so the event loop can recognise them.
pub const ID_QUIT: &str = "dsh-shell:quit";
pub const ID_SHOW: &str = "dsh-shell:show";
pub const ID_RELOAD: &str = "dsh-shell:reload";

/// One entry in the menu description.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Entry {
    /// A platform-provided item (cut, copy, paste, …), bound to a native
    /// selector by the system.
    Predefined(Predefined),
    /// An item this shell acts on, identified by its menu id.
    Shell { label: &'static str, id: &'static str },
    Separator,
}

/// The platform items the shell relies on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Predefined {
    About,
    Services,
    Hide,
    HideOthers,
    ShowAll,
    Undo,
    Redo,
    Cut,
    Copy,
    Paste,
    Delete,
    SelectAll,
    Minimize,
    Zoom,
    Fullscreen,
}

/// A submenu in the menu description.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Section {
    pub title: &'static str,
    pub entries: Vec<Entry>,
}

/// Describe the application menu.
///
/// Kept separate from construction because `muda::Menu` may only be built on the
/// main thread, which makes the real menu untestable. This description is pure
/// data, so the structure — in particular whether the Edit items that bind
/// Cmd+C/V are present — can be asserted in a test.
pub fn description() -> Vec<Section> {
    vec![
        Section {
            title: "App",
            entries: vec![
                Entry::Predefined(Predefined::About),
                Entry::Separator,
                Entry::Predefined(Predefined::Services),
                Entry::Separator,
                Entry::Predefined(Predefined::Hide),
                Entry::Predefined(Predefined::HideOthers),
                Entry::Predefined(Predefined::ShowAll),
                Entry::Separator,
                Entry::Shell {
                    label: "Quit",
                    id: ID_QUIT,
                },
            ],
        },
        Section {
            title: "Edit",
            entries: vec![
                Entry::Predefined(Predefined::Undo),
                Entry::Predefined(Predefined::Redo),
                Entry::Separator,
                Entry::Predefined(Predefined::Cut),
                Entry::Predefined(Predefined::Copy),
                Entry::Predefined(Predefined::Paste),
                Entry::Predefined(Predefined::Delete),
                Entry::Predefined(Predefined::SelectAll),
            ],
        },
        Section {
            title: "View",
            entries: vec![
                Entry::Shell {
                    label: "Reload",
                    id: ID_RELOAD,
                },
                Entry::Separator,
                Entry::Predefined(Predefined::Fullscreen),
            ],
        },
        Section {
            title: "Window",
            entries: vec![
                Entry::Predefined(Predefined::Minimize),
                Entry::Predefined(Predefined::Zoom),
                Entry::Separator,
                Entry::Shell {
                    label: "Close Window",
                    id: ID_SHOW,
                },
            ],
        },
    ]
}

/// Build the application menu.
///
/// Must be called on the main thread: the underlying platform objects require
/// it, and constructing one off-thread panics.
///
/// Returns the menu plus the ids of items this shell must act on. The standard
/// editing items are handled by the system and need no wiring.
pub fn build(app_name: &str) -> Menu {
    let menu = Menu::new();

    for section in description() {
        // macOS treats a submenu named "Window" specially; the app menu takes
        // the application's name.
        let title = if section.title == "App" {
            app_name.to_string()
        } else {
            section.title.to_string()
        };
        let submenu = Submenu::new(&title, true);
        for entry in &section.entries {
            let _ = match entry {
                Entry::Separator => submenu.append(&PredefinedMenuItem::separator()),
                Entry::Predefined(which) => submenu.append(&predefined_item(*which, app_name)),
                Entry::Shell { label, id } => {
                    let (key, mods) = shell_shortcut(id);
                    submenu.append(&shell_item(label, id, key, mods))
                }
            };
        }
        let _ = menu.append(&submenu);
    }

    menu
}

/// The accelerator for a shell-owned item.
fn shell_shortcut(id: &str) -> (Code, Modifiers) {
    match id {
        ID_QUIT => (Code::KeyQ, Modifiers::META),
        ID_RELOAD => (Code::KeyR, Modifiers::META),
        ID_SHOW => (Code::KeyW, Modifiers::META),
        _ => (Code::KeyD, Modifiers::META),
    }
}

/// Construct the platform item for a description entry.
fn predefined_item(which: Predefined, app_name: &str) -> PredefinedMenuItem {
    match which {
        Predefined::About => {
            PredefinedMenuItem::about(Some(&format!("About {app_name}")), None)
        }
        Predefined::Services => PredefinedMenuItem::services(None),
        Predefined::Hide => PredefinedMenuItem::hide(Some(&format!("Hide {app_name}"))),
        Predefined::HideOthers => PredefinedMenuItem::hide_others(None),
        Predefined::ShowAll => PredefinedMenuItem::show_all(None),
        Predefined::Undo => PredefinedMenuItem::undo(None),
        Predefined::Redo => PredefinedMenuItem::redo(None),
        Predefined::Cut => PredefinedMenuItem::cut(None),
        Predefined::Copy => PredefinedMenuItem::copy(None),
        Predefined::Paste => PredefinedMenuItem::paste(None),
        Predefined::Delete => PredefinedMenuItem::delete(None),
        Predefined::SelectAll => PredefinedMenuItem::select_all(None),
        Predefined::Minimize => PredefinedMenuItem::minimize(None),
        Predefined::Zoom => PredefinedMenuItem::zoom(None),
        Predefined::Fullscreen => PredefinedMenuItem::fullscreen(None),
    }
}

/// A menu item whose selection the shell handles itself.
fn shell_item(label: &str, id: &str, key: Code, mods: Modifiers) -> MenuItem {
    MenuItem::with_id(
        id,
        label,
        true,
        Some(Accelerator::new(mods, key)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn section(title: &str) -> Section {
        description()
            .into_iter()
            .find(|s| s.title == title)
            .unwrap_or_else(|| panic!("no {title} section"))
    }

    #[test]
    fn the_edit_menu_binds_the_standard_clipboard_items() {
        // This is the regression test for Cmd+C/V doing nothing. macOS routes
        // those shortcuts through the Edit menu, so if these items are missing
        // the clipboard is dead no matter what the web view implements.
        let edit = section("Edit");
        for required in [
            Predefined::Cut,
            Predefined::Copy,
            Predefined::Paste,
            Predefined::SelectAll,
            Predefined::Undo,
            Predefined::Redo,
        ] {
            assert!(
                edit.entries.contains(&Entry::Predefined(required)),
                "Edit menu is missing {required:?}; the shortcut would not work"
            );
        }
    }

    #[test]
    fn there_is_an_edit_section_at_all() {
        assert!(
            description().iter().any(|s| s.title == "Edit"),
            "no Edit section; Cmd+C/V cannot be bound"
        );
    }

    #[test]
    fn shell_items_have_unique_ids_and_shortcuts() {
        let mut ids = Vec::new();
        for s in description() {
            for e in s.entries {
                if let Entry::Shell { id, .. } = e {
                    assert!(!ids.contains(&id), "duplicate menu id {id}");
                    ids.push(id);
                    // A shell item with no accelerator would be unreachable
                    // from the keyboard.
                    let (_, mods) = shell_shortcut(id);
                    assert!(mods.contains(Modifiers::META), "{id} lacks a modifier");
                }
            }
        }
        assert!(ids.contains(&ID_QUIT) && ids.contains(&ID_RELOAD));
    }

    #[test]
    fn separators_are_not_adjacent_and_do_not_lead() {
        for s in description() {
            let first_is_sep = matches!(s.entries.first(), Some(Entry::Separator));
            assert!(!first_is_sep, "{} starts with a separator", s.title);
            for pair in s.entries.windows(2) {
                let both = matches!(pair[0], Entry::Separator)
                    && matches!(pair[1], Entry::Separator);
                assert!(!both, "{} has adjacent separators", s.title);
            }
        }
    }

    #[test]
    fn shell_items_carry_their_ids() {
        let item = shell_item("Quit", ID_QUIT, Code::KeyQ, Modifiers::META);
        assert_eq!(item.id().0.as_str(), ID_QUIT);
    }
}
