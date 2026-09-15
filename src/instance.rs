//! Single-instance ownership of the shell.
//!
//! Two shells racing would each start a host and each bind the same bridge
//! socket path, so a second launch must reach the first instead of competing
//! with it. The lock is an advisory `flock` on a file rather than a PID file:
//! the kernel drops it when the process dies, however it dies. A PID file would
//! go stale on a `SIGKILL` and then block every later launch until someone
//! deleted it by hand — and worse, a recycled pid would make a dead shell look
//! alive.
//!
//! A second launch asks the running shell to show its window before exiting, so
//! launching the app while it sits in the tray does what the user expects
//! rather than appearing to do nothing. macOS routes a *bundle* relaunch to the
//! running instance on its own, but that covers neither a direct binary launch
//! nor the other platforms.
//!
//! Like [`crate::bridge`], this module is Unix-only: the handoff rides a Unix
//! socket, and `flock` is the lock. Rather than carry a fallback that silently
//! provides no single-instance protection at all, the crate declines to build
//! where these are missing.

use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::sync::mpsc::{channel, Receiver};
use std::time::Duration;

/// How long a second launch waits for the running shell to acknowledge.
///
/// The handoff is between two local processes with the responding work already
/// done, so this only has to outlast scheduling. It exists so a shell that is
/// alive but wedged cannot hang the new launch forever.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(3);

/// Asks the running shell to show its window.
const SHOW_REQUEST: &str = "show";

/// What the running shell answers once it has taken the request.
const SHOW_ACK: &str = "ok";

/// How this launch relates to an already-running shell.
pub enum Claim {
    /// No shell was running. This process owns the slot and should run one.
    ///
    /// The lock is held for as long as `guard` lives; show requests arrive on
    /// `activations`.
    Owned {
        guard: Guard,
        activations: Receiver<()>,
    },
    /// Another shell owned the slot and has been asked to show its window.
    HandedOff,
}

/// Holds the single-instance lock.
///
/// There is nothing to release explicitly: closing the file drops the lock, so
/// no exit path can forget to do it.
#[derive(Debug)]
pub struct Guard {
    /// Never read. Its only job is to stay open for the process's lifetime.
    _lock: std::fs::File,
}

/// Claim the slot, handing off to a running shell if there is one.
///
/// The paths are passed in rather than resolved here so the handoff can be
/// exercised against a scratch directory.
pub fn claim(lock_path: &Path, activate_path: &Path) -> std::io::Result<Claim> {
    // Two attempts, because the holder can die between our failed lock and our
    // handoff. The retry then legitimately acquires a lock nobody holds.
    for _ in 0..2 {
        match try_lock(lock_path)? {
            Some(guard) => {
                let activations = listen(activate_path)?;
                return Ok(Claim::Owned { guard, activations });
            }
            // A failed handoff means the holder vanished; loop and retry the
            // lock rather than reporting a failure the user cannot act on.
            None => {
                if hand_off(activate_path).is_ok() {
                    return Ok(Claim::HandedOff);
                }
            }
        }
    }

    Err(std::io::Error::new(
        std::io::ErrorKind::WouldBlock,
        format!(
            "another shell holds {} and did not answer on {}",
            lock_path.display(),
            activate_path.display()
        ),
    ))
}

/// Take the lock without blocking, or report that someone else holds it.
fn try_lock(path: &Path) -> std::io::Result<Option<Guard>> {
    use std::os::unix::io::AsRawFd;

    crate::runtime::ensure_parent(path);
    let file = std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(path)?;

    // `LOCK_EX` because exactly one shell may own the slot; `LOCK_NB` so a
    // second launch learns the answer instead of waiting on the first.
    const LOCK_EX: i32 = 2;
    const LOCK_NB: i32 = 4;
    // SAFETY: `file` holds a valid open descriptor for the duration of the call.
    let locked = unsafe { flock(file.as_raw_fd(), LOCK_EX | LOCK_NB) } == 0;
    if locked {
        return Ok(Some(Guard { _lock: file }));
    }

    // `WouldBlock` is the expected "someone else owns it"; anything else is a
    // real failure and must not be mistaken for a running shell.
    let err = std::io::Error::last_os_error();
    if err.kind() == std::io::ErrorKind::WouldBlock {
        return Ok(None);
    }
    Err(err)
}

#[cfg(unix)]
unsafe extern "C" {
    fn flock(fd: i32, operation: i32) -> i32;
}

/// Accept activation requests and forward them to the UI thread.
///
/// The listener is bound only once the lock is held, which is what makes it
/// safe to clear a leftover socket file first: no live shell can own that path
/// while we hold the lock.
fn listen(path: &Path) -> std::io::Result<Receiver<()>> {
    use std::os::unix::net::UnixListener;

    crate::runtime::ensure_parent(path);
    if path.exists() {
        let _ = std::fs::remove_file(path);
    }
    let listener = UnixListener::bind(path)?;
    let (tx, rx) = channel();

    std::thread::Builder::new()
        .name("instance-activate".into())
        .spawn(move || {
            for stream in listener.incoming() {
                let stream = match stream {
                    Ok(stream) => stream,
                    Err(err) => {
                        tracing::debug!(%err, "activation accept failed");
                        continue;
                    }
                };

                let mut line = String::new();
                // Scoped so the borrow ends before the socket is written to.
                let read = {
                    let mut reader = BufReader::new(&stream);
                    reader.read_line(&mut line)
                };
                if let Err(err) = read {
                    tracing::debug!(%err, "activation read failed");
                    continue;
                }
                if line.trim() != SHOW_REQUEST {
                    tracing::debug!(line = %line.trim(), "ignoring unknown activation");
                    continue;
                }

                // A send failure means the event loop is gone, so the shell is
                // shutting down and the thread has nothing left to do.
                if tx.send(()).is_err() {
                    break;
                }
                let mut writer = &stream;
                let _ = writer.write_all(format!("{SHOW_ACK}\n").as_bytes());
            }
        })?;

    Ok(rx)
}

/// Ask the running shell to show its window, and wait for it to agree.
fn hand_off(path: &Path) -> std::io::Result<()> {
    use std::os::unix::net::UnixStream;

    let mut stream = UnixStream::connect(path)?;
    stream.set_read_timeout(Some(HANDSHAKE_TIMEOUT))?;
    stream.set_write_timeout(Some(HANDSHAKE_TIMEOUT))?;
    stream.write_all(format!("{SHOW_REQUEST}\n").as_bytes())?;
    stream.flush()?;

    let mut line = String::new();
    let mut reader = BufReader::new(&stream);
    reader.read_line(&mut line)?;
    if line.trim() == SHOW_ACK {
        return Ok(());
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        format!("unexpected activation reply: {}", line.trim()),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// A scratch directory per test, so no two tests share a lock or socket.
    struct Scratch {
        dir: PathBuf,
    }

    impl Scratch {
        fn new() -> Self {
            static NEXT: AtomicU32 = AtomicU32::new(0);
            let n = NEXT.fetch_add(1, Ordering::Relaxed);
            let dir = std::env::temp_dir().join(format!(
                "dsh-shell-instance-{}-{n}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("create scratch dir");
            Self { dir }
        }

        /// Short enough to fit `sun_path`, which caps the socket path length.
        fn lock(&self) -> PathBuf {
            self.dir.join("l")
        }

        fn activate(&self) -> PathBuf {
            self.dir.join("a")
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    #[test]
    fn the_first_claim_owns_the_slot() {
        let scratch = Scratch::new();
        let claim = claim(&scratch.lock(), &scratch.activate()).expect("claim");
        assert!(matches!(claim, Claim::Owned { .. }));
    }

    #[test]
    fn a_second_claim_hands_off_and_activates_the_first() {
        let scratch = Scratch::new();
        let Claim::Owned { guard, activations } =
            claim(&scratch.lock(), &scratch.activate()).expect("first claim")
        else {
            panic!("the first claim must own the slot");
        };

        let second = claim(&scratch.lock(), &scratch.activate()).expect("second claim");
        assert!(
            matches!(second, Claim::HandedOff),
            "a second launch must hand off, not start a second shell"
        );

        // The handoff is only useful if it actually reaches the running shell,
        // which is what would raise the window.
        activations
            .recv_timeout(Duration::from_secs(5))
            .expect("the running shell must be told to show its window");

        drop(guard);
    }

    #[test]
    fn the_slot_is_free_once_the_guard_is_dropped() {
        let scratch = Scratch::new();
        let Claim::Owned { guard, .. } =
            claim(&scratch.lock(), &scratch.activate()).expect("first claim")
        else {
            panic!("the first claim must own the slot");
        };
        drop(guard);

        // The lock file itself is never unlinked, so a successful reclaim
        // proves the kernel released the lock rather than the file vanishing.
        assert!(
            scratch.lock().exists(),
            "the lock file must outlive the guard"
        );
        let again = claim(&scratch.lock(), &scratch.activate()).expect("reclaim");
        assert!(matches!(again, Claim::Owned { .. }));
    }

    #[test]
    fn a_handoff_with_no_listener_reports_a_failure() {
        let scratch = Scratch::new();
        // Hold the lock without ever binding the activation socket, which is
        // the window between locking and listening during a real startup.
        let guard = try_lock(&scratch.lock()).expect("lock").expect("free");
        let err = claim(&scratch.lock(), &scratch.activate())
            .err()
            .expect("a handoff nobody answers must be reported, not assumed");
        assert_eq!(err.kind(), std::io::ErrorKind::WouldBlock);
        drop(guard);
    }

    #[test]
    fn a_claim_recovers_when_the_holder_died_mid_handoff() {
        let scratch = Scratch::new();
        let Claim::Owned { guard, .. } =
            claim(&scratch.lock(), &scratch.activate()).expect("first claim")
        else {
            panic!("the first claim must own the slot");
        };
        // Simulate the holder dying after the second launch's lock attempt but
        // before its handoff: the lock is free, the stale socket is not.
        drop(guard);

        let reclaim = claim(&scratch.lock(), &scratch.activate()).expect("reclaim");
        assert!(
            matches!(reclaim, Claim::Owned { .. }),
            "a launch must not be stranded by a socket file its owner left behind"
        );
    }
}
