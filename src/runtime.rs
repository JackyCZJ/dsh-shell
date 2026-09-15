//! Where a running shell keeps its per-user runtime files.
//!
//! One directory holds everything the shell owns while it runs: the bridge
//! socket the Host plugin dials, the lock that turns a second launch into a
//! handoff, and the activation socket that carries that handoff. Keeping the
//! three together is what makes "is a shell already running?" a single
//! well-defined question.
//!
//! None of these files hold configuration. Configuration lives in DSH's own
//! settings document; these are process artifacts carrying no user intent, so
//! losing them costs nothing beyond a restart.

use std::path::{Path, PathBuf};

/// The bridge socket the Host plugin connects to.
///
/// `DSH_SHELL_SOCKET` overrides it. The plugin honours the same variable, so
/// setting it moves both ends together — which is what makes the bridge
/// testable without disturbing the real session.
pub fn socket_path() -> PathBuf {
    Env::from_process().socket_path(uid())
}

/// The single-instance lock file.
///
/// The file only needs to exist; what matters is the lock held *on* it, which
/// the kernel drops when the process dies. See [`crate::instance`].
pub fn lock_path() -> PathBuf {
    Env::from_process().lock_path(uid())
}

/// The activation socket a second launch dials to reach the running shell.
pub fn activate_path() -> PathBuf {
    Env::from_process().activate_path(uid())
}

/// Ensure the directory for `path` exists, so a bind or create cannot fail on a
/// missing parent.
pub fn ensure_parent(path: &Path) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
}

/// Remove the socket files, so a clean exit leaves nothing behind.
///
/// The lock file is deliberately **not** removed. Unlinking it would be a race:
/// a launch that opened the path just before the unlink would hold a lock on an
/// inode that no longer has a name, while a later launch would create a fresh
/// inode and lock that — leaving two shells each believing it was the only one.
/// An empty leftover file is harmless, and no lock is left held on it, because
/// the kernel releases the lock when the process dies.
pub fn cleanup() {
    for path in [socket_path(), activate_path()] {
        if path.exists() {
            let _ = std::fs::remove_file(&path);
        }
    }
}

/// The environment the path resolvers read.
///
/// Split out from the resolvers so the naming and precedence rules can be
/// tested without mutating the process environment, which is shared by every
/// test thread and would make the suite order-dependent.
#[derive(Debug, Default)]
struct Env {
    socket: Option<String>,
    lock: Option<String>,
    activate: Option<String>,
    run_dir: Option<String>,
}

impl Env {
    fn from_process() -> Self {
        Self {
            socket: std::env::var("DSH_SHELL_SOCKET").ok(),
            lock: std::env::var("DSH_SHELL_LOCK").ok(),
            activate: std::env::var("DSH_SHELL_ACTIVATE").ok(),
            run_dir: std::env::var("XDG_RUNTIME_DIR").ok(),
        }
    }

    /// `XDG_RUNTIME_DIR` is the right home on Linux: per-user, mode 0700, and
    /// cleared on logout. macOS has no equivalent, so the per-user temp
    /// directory stands in — same 0700 ownership, likewise not durable.
    ///
    /// An exported-but-empty variable is treated as unset: it would otherwise
    /// resolve to the filesystem root, putting one user's runtime files in a
    /// directory every user shares.
    fn run_dir(&self) -> PathBuf {
        self.run_dir
            .as_deref()
            .filter(|dir| !dir.trim().is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir)
    }

    fn socket_path(&self, uid: u32) -> PathBuf {
        match self.socket.as_deref() {
            Some(explicit) => PathBuf::from(explicit),
            None => self.run_dir().join(format!("dsh-shell-{uid}.sock")),
        }
    }

    fn lock_path(&self, uid: u32) -> PathBuf {
        match self.lock.as_deref() {
            Some(explicit) => PathBuf::from(explicit),
            None => self.run_dir().join(format!("dsh-shell-{uid}.lock")),
        }
    }

    fn activate_path(&self, uid: u32) -> PathBuf {
        match self.activate.as_deref() {
            Some(explicit) => PathBuf::from(explicit),
            None => {
                self.run_dir()
                    .join(format!("dsh-shell-{uid}.activate.sock"))
            }
        }
    }
}

/// The real user id, keeping one user's runtime files out of another's.
///
/// Unix-only, like the rest of the shell's inter-process plumbing: the bridge
/// socket and the single-instance lock are both Unix facilities, so a port to a
/// platform without them has to answer this question rather than inherit a
/// default that quietly gives every user the same paths.
#[cfg(unix)]
pub fn uid() -> u32 {
    // SAFETY: getuid takes no arguments and cannot fail.
    unsafe { getuid() }
}

#[cfg(unix)]
unsafe extern "C" {
    fn getuid() -> u32;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(socket: Option<&str>, lock: Option<&str>, activate: Option<&str>, run: Option<&str>) -> Env {
        Env {
            socket: socket.map(str::to_string),
            lock: lock.map(str::to_string),
            activate: activate.map(str::to_string),
            run_dir: run.map(str::to_string),
        }
    }

    #[test]
    fn each_runtime_file_has_its_own_name() {
        let env = env(None, None, None, Some("/run/user/501"));
        let paths = [
            env.socket_path(501),
            env.lock_path(501),
            env.activate_path(501),
        ];
        // Sharing a path would mean the lock and a socket fighting over one
        // inode — the very thing the lock exists to prevent.
        assert_ne!(paths[0], paths[1]);
        assert_ne!(paths[1], paths[2]);
        assert_ne!(paths[0], paths[2]);
    }

    #[test]
    fn runtime_files_are_namespaced_by_user() {
        let env = env(None, None, None, Some("/run/user"));
        assert!(env.socket_path(501).to_string_lossy().contains("501"));
        assert!(!env.socket_path(502).to_string_lossy().contains("501"));
    }

    #[test]
    fn an_explicit_path_wins_over_the_run_dir() {
        let env = env(Some("/tmp/a.sock"), Some("/tmp/a.lock"), Some("/tmp/a.act"), None);
        assert_eq!(env.socket_path(1), PathBuf::from("/tmp/a.sock"));
        assert_eq!(env.lock_path(1), PathBuf::from("/tmp/a.lock"));
        assert_eq!(env.activate_path(1), PathBuf::from("/tmp/a.act"));
    }

    #[test]
    fn an_empty_run_dir_variable_is_ignored() {
        let env = env(None, None, None, Some(""));
        // Not `PathBuf::from("")`, which would place runtime files in a
        // directory every user shares.
        assert_eq!(env.run_dir(), std::env::temp_dir());
    }

    #[test]
    fn a_blank_run_dir_variable_is_ignored() {
        let env = env(None, None, None, Some("   "));
        assert_eq!(env.run_dir(), std::env::temp_dir());
    }

    #[test]
    fn the_default_socket_path_fits_a_unix_socket() {
        std::env::remove_var("DSH_SHELL_SOCKET");
        let path = socket_path();
        // `sun_path` is 104 bytes on macOS and 108 on Linux; the smaller bound
        // is the one that matters, and a longer path fails at bind, not here.
        assert!(
            path.as_os_str().len() < 104,
            "socket path too long: {}",
            path.display()
        );
    }
}
