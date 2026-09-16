//! Somewhere a human can actually read the shell's diagnostics.
//!
//! The shell is a GUI app, and a GUI app's stdout goes nowhere: launched from
//! Finder there is no terminal attached, and macOS does not route a plain
//! process's stdout into the unified log either. Logging only to stdout
//! therefore makes the shell undiagnosable in exactly the situation that
//! matters — a real user, on a real machine, with something going wrong. That
//! is not hypothetical: tracking down "the Dock badge never appears" meant
//! guessing, because every record the shell wrote was invisible.
//!
//! So the same records also go to `$DSH_HOME/cache/dsh-shell/dsh-shell.log`.
//! Under DSH's home, so the shell's state stays in one tree, and under `cache`
//! because a log is not worth preserving more carefully than that.
//!
//! The file is truncated on every launch. One run's log is what a bug report
//! needs, and a GUI app can run for weeks — appending would grow without bound
//! and bury the interesting run under everything since.

use std::path::PathBuf;

/// Where the shell's own log lives.
///
/// `DSH_SHELL_LOG` overrides the whole path, which is what lets a test run
/// write somewhere disposable instead of into the user's real log.
pub fn path() -> Option<PathBuf> {
    path_from(
        std::env::var("DSH_SHELL_LOG").ok(),
        std::env::var("DSH_HOME").ok(),
        std::env::var("HOME").ok(),
    )
}

/// Split out so the precedence can be tested without mutating the process
/// environment, which every test thread shares.
fn path_from(
    explicit: Option<String>,
    dsh_home: Option<String>,
    home: Option<String>,
) -> Option<PathBuf> {
    if let Some(explicit) = explicit {
        // An exported-but-empty variable would resolve to the filesystem root;
        // treating it as unset keeps a stray `DSH_SHELL_LOG=` harmless.
        if explicit.trim().is_empty() {
            return None;
        }
        return Some(PathBuf::from(explicit));
    }
    let home = dsh_home
        .filter(|dir| !dir.trim().is_empty())
        .map(PathBuf::from)
        .or_else(|| home.map(|h| PathBuf::from(h).join(".dsh")))?;
    Some(home.join("cache").join("dsh-shell").join("dsh-shell.log"))
}

/// Open the log for writing, truncating whatever was there.
///
/// Returns `None` when there is nowhere to write — a shell that cannot write a
/// log must still run, so a failure here is reported and shrugged off rather
/// than being fatal.
pub fn open(path: &std::path::Path) -> Option<std::fs::File> {
    crate::runtime::ensure_parent(path);
    match std::fs::File::create(path) {
        Ok(file) => Some(file),
        Err(err) => {
            // Reported on stderr rather than through `tracing`, because
            // `tracing` is what is being set up and may not exist yet.
            eprintln!(
                "dsh-shell: could not open the log at {}: {err}",
                path.display()
            );
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_log_sits_beside_the_window_state() {
        let path = path_from(None, Some("/home/u/.dsh".into()), None).unwrap();
        // Same directory as `window_state::path`: shell state lives in one tree.
        assert_eq!(
            path,
            PathBuf::from("/home/u/.dsh/cache/dsh-shell/dsh-shell.log")
        );
    }

    #[test]
    fn an_explicit_path_wins() {
        assert_eq!(
            path_from(Some("/tmp/x.log".into()), Some("/home/u/.dsh".into()), None).unwrap(),
            PathBuf::from("/tmp/x.log")
        );
    }

    #[test]
    fn a_blank_explicit_path_is_ignored() {
        // Not `PathBuf::from("")`, which would try to create a file at the root.
        assert_eq!(path_from(Some("  ".into()), None, None), None);
    }

    #[test]
    fn home_is_the_fallback_when_dsh_home_is_unset() {
        let path = path_from(None, None, Some("/home/u".into())).unwrap();
        assert_eq!(
            path,
            PathBuf::from("/home/u/.dsh/cache/dsh-shell/dsh-shell.log")
        );
    }

    #[test]
    fn no_home_means_no_log() {
        assert_eq!(path_from(None, None, None), None);
    }
}
