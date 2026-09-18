//! Upgrading the DSH that this shell launches.
//!
//! The shell holds no DSH of its own: `server::resolve_launcher` finds one on
//! disk, which for a normal install is `~/.bun/bin/dsh` pointing into bun's
//! global tree. Keeping that install current is this module's whole job.
//!
//! # Why nothing here uses `bun install -g` in place
//!
//! Measured on a real install: `bun install -g @deepseek-ai/dsh@0.1.5-rc.2`
//! left **23 packages at the old version** and 208 at the new one, because bun
//! reuses the global lockfile's resolutions. The same request resolved in an
//! empty directory produced **231 at the new version and a consistent tree**.
//! So the upgrade resolves fresh into a staging directory, proves the result can
//! start, and only then replaces the live tree.
//!
//! # Why the whole `@deepseek-ai` scope is swapped, not a package list
//!
//! The scope is not all DSH. It also holds third-party dependencies — `cordis`,
//! `schemastery`, `cosmokit`, the `cordis-plugin-*` set, `node-addon-system*` —
//! ten of them on the machine this was written against. A fresh resolve pulls
//! those back at identical versions, so swapping the scope directory wholesale
//! is both the simplest rule and the correct dependency graph; a per-package
//! merge would instead leave behind every package the new version dropped.
//!
//! # Why it is safe to stage
//!
//! Staging costs 279 MB and about 8 seconds, and the staged launcher answers
//! `--version` in 80 ms. That buys the right to verify before committing, and
//! the previous scope is renamed aside rather than deleted, so a rollback is a
//! rename instead of a re-download.
//!
//! # What this deliberately does not do
//!
//! Nothing applies an upgrade on its own. Detection is automatic and cached;
//! application is always a direct response to a click. This project publishes
//! release candidates, and an unattended apply could move someone onto a broken
//! prerelease while they slept.

use std::path::{Path, PathBuf};

/// How long a cached registry answer is trusted before a check goes out again.
///
/// A day is long enough that the check is a non-event and short enough that a
/// user who leaves the app open still learns about a release promptly. It is a
/// constant rather than a setting on purpose: the intervals worth supporting are
/// "frequently" and "never", and those are already the manual button and not
/// opening the window.
pub const CHECK_INTERVAL: std::time::Duration = std::time::Duration::from_secs(24 * 60 * 60);

/// Directory name holding the previous scope during an upgrade.
const BACKUP_DIR: &str = "previous";

/// The npm scope every DSH package lives under.
const SCOPE: &str = "@deepseek-ai";

/// The package whose version *is* the DSH version.
const ROOT_PACKAGE: &str = "dsh";

// ---------------------------------------------------------------- versions

/// A parsed semantic version, enough of one to order DSH releases.
///
/// Hand-written rather than taking a `semver` dependency: the only question
/// asked here is "is B newer than A", and getting prerelease ordering wrong is
/// the one way to answer it incorrectly. See [`Version::cmp`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Version {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
    /// Dot-separated prerelease identifiers; empty for a release version.
    pub prerelease: Vec<String>,
}

impl Version {
    /// Parse `1.2.3`, `1.2.3-rc.1`, `1.2.3-alpha.2`. Build metadata (`+x`) is
    /// ignored, as semver says it must not affect precedence.
    pub fn parse(text: &str) -> Option<Version> {
        let text = text.trim();
        let text = text.split('+').next().unwrap_or(text);
        let (core, prerelease) = match text.split_once('-') {
            Some((core, pre)) => (
                core,
                pre.split('.')
                    .map(|part| part.to_string())
                    .collect::<Vec<_>>(),
            ),
            None => (text, Vec::new()),
        };

        let mut numbers = core.split('.');
        let major = numbers.next()?.parse().ok()?;
        let minor = numbers.next()?.parse().ok()?;
        let patch = numbers.next()?.parse().ok()?;
        if numbers.next().is_some() {
            return None;
        }
        if prerelease.iter().any(|part| part.is_empty()) {
            return None;
        }

        Some(Version {
            major,
            minor,
            patch,
            prerelease,
        })
    }
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)?;
        if !self.prerelease.is_empty() {
            write!(f, "-{}", self.prerelease.join("."))?;
        }
        Ok(())
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Version {
    /// Semver precedence, including the rule that a prerelease sorts *below* its
    /// release (`1.0.0-rc.1 < 1.0.0`) — the rule that decides whether a user on
    /// `0.1.5-rc.2` is offered `0.1.6-alpha.2`.
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        use std::cmp::Ordering;

        let core = (self.major, self.minor, self.patch).cmp(&(other.major, other.minor, other.patch));
        if core != Ordering::Equal {
            return core;
        }

        match (self.prerelease.is_empty(), other.prerelease.is_empty()) {
            (true, true) => Ordering::Equal,
            // A version with no prerelease outranks one with any.
            (true, false) => Ordering::Greater,
            (false, true) => Ordering::Less,
            (false, false) => compare_prerelease(&self.prerelease, &other.prerelease),
        }
    }
}

/// Compare prerelease identifier lists, per semver rule 11.
fn compare_prerelease(left: &[String], right: &[String]) -> std::cmp::Ordering {
    use std::cmp::Ordering;

    for (a, b) in left.iter().zip(right.iter()) {
        let ordering = match (a.parse::<u64>(), b.parse::<u64>()) {
            // Numeric identifiers compare numerically and rank below alphanumeric.
            (Ok(x), Ok(y)) => x.cmp(&y),
            (Ok(_), Err(_)) => Ordering::Less,
            (Err(_), Ok(_)) => Ordering::Greater,
            (Err(_), Err(_)) => a.cmp(b),
        };
        if ordering != Ordering::Equal {
            return ordering;
        }
    }
    // A larger set of identifiers wins when all shared ones are equal.
    left.len().cmp(&right.len())
}

// ------------------------------------------------------------ install layout

/// Where the DSH that this shell runs actually lives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Install {
    /// The path to *execute* to run this DSH — the launcher as the shell found
    /// it, symlink and all, because that symlink is what carries the executable
    /// bit. Resolving it would leave a `.js` file that cannot be spawned; the
    /// shell would try to execute JavaScript as a program.
    pub launcher: PathBuf,
    /// The launcher with symlinks resolved, used only to read the layout off.
    pub resolved: PathBuf,
    /// The `node_modules/@deepseek-ai` directory holding the running packages.
    pub scope: PathBuf,
    /// The global prefix (`~/.bun/install/global`), where a staged tree is built
    /// beside the live one so the final rename stays on one filesystem.
    pub prefix: PathBuf,
    /// The package manager to stage with, resolved once.
    pub bun: Option<PathBuf>,
}

impl Install {
    /// Locate the install behind `launcher`, or `None` when the layout is not
    /// one this module understands — a bundled runtime or a source checkout,
    /// both of which have their own update story.
    ///
    /// The shape being looked for is
    /// `<prefix>/node_modules/@deepseek-ai/dsh/lib/bin.js`.
    pub fn discover(launcher: &Path) -> Option<Install> {
        let resolved = resolve_symlinks(launcher);
        // Build the scope as an owned path first: keeping a borrow of `resolved`
        // alive here would stop it from being moved into the struct.
        let scope = resolved
            .parent() // .../dsh/lib
            .and_then(Path::parent) // .../dsh
            .filter(|dsh| dsh.file_name().is_some_and(|n| n == ROOT_PACKAGE))?
            .parent() // .../@deepseek-ai
            .filter(|scope| scope.file_name().is_some_and(|n| n == SCOPE))?
            .to_path_buf();
        let prefix = scope.parent()?.parent()?.to_path_buf();

        Some(Install {
            bun: resolve_bun(launcher, &prefix),
            launcher: launcher.to_path_buf(),
            resolved,
            scope,
            prefix,
        })
    }

    /// The path to run for *staged* trees.
    ///
    /// A staged tree has no `bin` symlink pointing into it, so asking the
    /// launcher inside the staging directory directly is wrong for the same
    /// reason as above. If the tree cannot be resolved to a package-manager
    /// shim, running it through its sibling `bun` is equivalent — the shim
    /// itself does nothing but spawn that same binary with the same script.
    fn staged_command(&self, version: &Version) -> Result<(PathBuf, Vec<String>), String> {
        let script = self.staged_launcher(version);
        if !script.is_file() {
            return Err(format!("nothing to run at {}", script.display()));
        }
        // Prefer the shim when the tree has one, so staging is exercised the
        // same way a real install runs.
        if let Some(shim) = self.bun.as_ref().and_then(|bun| {
            bun.parent().map(|bin| bin.join(ROOT_PACKAGE)).filter(|p| p.is_file())
        }) {
            return Ok((shim, Vec::new()));
        }
        let bun = self
            .bun
            .as_ref()
            .ok_or("no package manager available to run the staged tree")?;
        Ok((bun.clone(), vec!["run".into(), script.to_string_lossy().into_owned()]))
    }

    /// Where a staged tree for `version` is built.
    ///
    /// Under the install prefix rather than the shell's own cache, because the
    /// final step renames this directory over the live scope and `rename` only
    /// works within one filesystem.
    pub fn stage_dir(&self, version: &Version) -> PathBuf {
        self.prefix.join("dsh-shell-staging").join(version.to_string())
    }

    /// Where the previous scope is parked during an upgrade.
    pub fn backup_dir(&self) -> PathBuf {
        self.prefix.join("dsh-shell-staging").join(BACKUP_DIR)
    }

    /// The staged equivalent of [`Install::launcher`].
    pub fn staged_launcher(&self, version: &Version) -> PathBuf {
        self.stage_dir(version)
            .join("node_modules")
            .join(SCOPE)
            .join(ROOT_PACKAGE)
            .join("lib")
            .join("bin.js")
    }

    /// The staged scope directory, ready to be renamed into place.
    pub fn staged_scope(&self, version: &Version) -> PathBuf {
        self.stage_dir(version).join("node_modules").join(SCOPE)
    }
}

/// Follow symlinks without requiring the path to exist as a whole.
fn resolve_symlinks(path: &Path) -> PathBuf {
    match std::fs::canonicalize(path) {
        Ok(real) => real,
        // A launcher that is not there yet still has to produce a sensible
        // answer so the UI can say what it would upgrade.
        Err(_) => path.to_path_buf(),
    }
}

/// Find the package manager to stage with.
///
/// Preferred order: a `bun` sitting beside the launcher (which is how bun
/// installs itself globally), `bun` on `PATH`, then a sibling of the live
/// install. `None` disables upgrading rather than failing at the last step.
fn resolve_bun(launcher: &Path, prefix: &Path) -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    // `~/.bun/bin/dsh` -> `~/.bun/bin/bun`
    if let Some(bin) = launcher.parent() {
        candidates.push(bin.join("bun"));
    }
    // `<prefix>/node_modules/@deepseek-ai/dsh/lib/bin.js` climbs to `<prefix>`.
    candidates.push(prefix.join("node_modules").join(".bin").join("bun"));
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        candidates.push(home.join(".bun").join("bin").join("bun"));
    }

    for candidate in &candidates {
        if candidate.is_file() {
            return Some(candidate.clone());
        }
    }
    // Any `bun` on PATH, via the same probe the launcher search uses.
    let probe = std::process::Command::new("bun")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
    if matches!(probe, Ok(status) if status.success()) {
        return Some(PathBuf::from("bun"));
    }
    None
}

// ------------------------------------------------------------ running things

/// Run `program args…` with a timeout, returning trimmed stdout.
///
/// A thread drains stdout while the parent polls for exit. That is not
/// incidental: polling `try_wait` while the child writes into a `piped` stdout
/// deadlocks as soon as the child fills the pipe buffer (~64 KB), because it
/// blocks on a write the parent never reads. The registry document is 142 KB, so
/// the first version of this function timed out on every single check and made
/// a healthy network look unreachable.
fn run_with_timeout(
    program: &Path,
    args: &[&str],
    timeout: std::time::Duration,
) -> Result<String, String> {
    run_with_timeout_hinted(program, args, timeout, None)
}

/// As [`run_with_timeout`], with a launcher whose `node` must also be findable.
///
/// The hint matters for the same reason it does when the host is spawned: a
/// package manager's `dsh` is a `#!/usr/bin/env node` script and a GUI app's
/// PATH has no `node`, so the probe fails with exit 127, `env: node: No such
/// file or directory`, unless the launcher's own directory is prepended. The
/// host already got this treatment; the update check did not, and reported a
/// healthy install as broken.
fn run_with_timeout_hinted(
    program: &Path,
    args: &[&str],
    timeout: std::time::Duration,
    path_hint: Option<&Path>,
) -> Result<String, String> {
    let mut command = std::process::Command::new(program);
    command
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    if let Some(hint) = path_hint {
        crate::server::augment_path_for_std(&mut command, &hint.to_string_lossy());
    }
    drive_to_exit(command, program, timeout)
}

/// Spawn a prepared command, drain it, and report its stdout.
///
/// Shared by both runners so that the deadlock fix below exists in exactly one
/// place: polling `try_wait` while the child writes into a `piped` stdout
/// deadlocks as soon as the child fills the pipe buffer, so a thread must always
/// be reading.
fn drive_to_exit(
    mut command: std::process::Command,
    program: &Path,
    timeout: std::time::Duration,
) -> Result<String, String> {
    let mut child = command
        .spawn()
        .map_err(|err| format!("could not run {}: {err}", program.display()))?;

    let stdout = child.stdout.take().expect("stdout was piped");
    let stderr = child.stderr.take().expect("stderr was piped");
    let reader = std::thread::spawn(move || {
        use std::io::Read;
        // Read both streams to the end: stopping at EOF on one while the other
        // fills would reintroduce the deadlock this thread exists to prevent.
        let mut out = Vec::new();
        let mut err = Vec::new();
        let _ = std::io::BufReader::new(stdout).read_to_end(&mut out);
        let _ = std::io::BufReader::new(stderr).read_to_end(&mut err);
        (out, err)
    });

    let deadline = std::time::Instant::now() + timeout;
    let mut timed_out = false;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    timed_out = true;
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(25));
            }
            Err(err) => {
                let _ = child.kill();
                return Err(format!("waiting for {}: {err}", program.display()));
            }
        }
    }

    let (out, err) = reader
        .join()
        .map_err(|_| format!("the output reader for {} panicked", program.display()))?;
    let status = child
        .try_wait()
        .ok()
        .flatten()
        .ok_or_else(|| format!("{} did not report an exit status", program.display()))?;

    if timed_out {
        return Err(format!("{} timed out", program.display()));
    }
    if !status.success() {
        let detail = String::from_utf8_lossy(&err);
        let detail = detail
            .lines()
            .rev()
            .find(|line| !line.trim().is_empty())
            .unwrap_or("")
            .trim()
            .to_string();
        return Err(format!(
            "{} exited {}: {detail}",
            program.display(),
            status.code().unwrap_or(-1)
        ));
    }

    Ok(String::from_utf8_lossy(&out).trim().to_string())
}

/// The version a launcher reports.
/// The version a launcher reports.
///
/// `launcher` doubles as the PATH hint: its directory is where the `node` that
/// runs it lives, and a GUI app has none of that on PATH.
pub fn version_of(launcher: &Path) -> Result<Version, String> {
    version_of_hinted(launcher, &[], Some(launcher))
}

/// The version a `(program, leading args)` pair reports.
///
/// The leading-args form exists because a staged tree has no executable shim:
/// proving it starts means running `<bun> run <staged>/bin.js --version`, which
/// is the same thing the installed shim does. `hint` supplies the directory to
/// prepend to PATH — for a staged tree that is the `bun` that will run it.
pub fn version_of_hinted(
    program: &Path,
    leading: &[String],
    hint: Option<&Path>,
) -> Result<Version, String> {
    let mut args: Vec<&str> = leading.iter().map(String::as_str).collect();
    args.push("--version");
    let out = run_with_timeout_hinted(program, &args, std::time::Duration::from_secs(60), hint)?;
    // A launcher prints exactly the version; take the last line to tolerate a
    // warning printed before it.
    let line = out.lines().last().unwrap_or("").trim();
    Version::parse(line).ok_or_else(|| format!("unreadable version from launcher: {line:?}"))
}

/// Convenience for the `(program, args)` shape [`Install::staged_command`] returns.
fn version_of_script(
    command: &(PathBuf, Vec<String>),
    hint: Option<&Path>,
) -> Result<Version, String> {
    version_of_hinted(&command.0, &command.1, hint)
}

// ------------------------------------------------------------------ registry

/// What the registry says is available.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Registry {
    pub latest: Version,
    pub next: Option<Version>,
    pub alpha: Option<Version>,
}

impl Registry {
    /// The version a channel name selects.
    pub fn channel(&self, channel: Channel) -> Version {
        match channel {
            Channel::Latest => self.latest.clone(),
            Channel::Alpha => self.alpha.clone().unwrap_or_else(|| self.latest.clone()),
        }
    }

    fn parse(document: &str) -> Result<Registry, String> {
        let json: serde_json::Value =
            serde_json::from_str(document).map_err(|err| format!("registry reply: {err}"))?;
        let tags = json
            .get("dist-tags")
            .and_then(|v| v.as_object())
            .ok_or("registry reply has no dist-tags")?;
        let read = |name: &str| -> Option<Version> {
            tags.get(name)
                .and_then(|v| v.as_str())
                .and_then(Version::parse)
        };
        Ok(Registry {
            latest: read("latest").ok_or("registry reply has no latest tag")?,
            next: read("next"),
            alpha: read("alpha"),
        })
    }
}

/// Which release line to follow.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Channel {
    /// The `latest` dist-tag. What everyone should be on.
    #[default]
    Latest,
    /// The `alpha` dist-tag, for someone deliberately tracking new work.
    Alpha,
}

impl Channel {
    pub fn as_str(self) -> &'static str {
        match self {
            Channel::Latest => "latest",
            Channel::Alpha => "alpha",
        }
    }

    pub fn parse(text: &str) -> Option<Channel> {
        match text.trim().to_ascii_lowercase().as_str() {
            "latest" | "stable" => Some(Channel::Latest),
            "alpha" => Some(Channel::Alpha),
            _ => None,
        }
    }
}

/// Registry URL for the DSH package document.
const REGISTRY_URL: &str = "https://registry.npmjs.org/@deepseek-ai%2Fdsh";

/// Fetch the registry document.
///
/// Uses `curl` rather than an HTTP client dependency. The shell already
/// supervises child processes and this is one GET, so pulling a TLS stack into
/// the binary to avoid a dependency that ships with macOS and every Linux
/// distribution is the wrong trade. A machine without `curl` simply cannot
/// check automatically.
fn fetch_registry(timeout: std::time::Duration) -> Result<Registry, String> {
    let out = run_with_timeout(
        Path::new("curl"),
        &["-sSfL", "--max-time", "20", REGISTRY_URL],
        timeout,
    )
    .map_err(|err| format!("could not reach the registry: {err}"))?;
    Registry::parse(&out)
}

// -------------------------------------------------------------- cached state

/// Where the cached *registry answer* lives. Written beside the shell's log.
pub fn cache_path() -> Option<PathBuf> {
    crate::logfile::path().map(|log| log.with_file_name("registry.json"))
}

/// Where the current *status* is published for other readers.
///
/// Deliberately not the same file as the registry cache: one is an input to the
/// updater and the other is its output, and sharing a path meant publishing the
/// status silently destroyed the cache — turning every later launch into the
/// network round trip the cache exists to avoid.
pub fn status_path() -> Option<PathBuf> {
    crate::logfile::path().map(|log| log.with_file_name("status.json"))
}

/// Publish `status` where anything else can read it.
///
/// The plugin bridge's reply carries only a boolean and an error string, so it
/// has no room for a payload; this file is how the plugin learns the result of a
/// check without the wire protocol growing a second shape.
pub fn publish(status: &Status) {
    let Some(path) = status_path() else { return };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Err(err) = std::fs::write(
        &path,
        serde_json::to_string_pretty(&status.json()).unwrap_or_else(|_| "{}".into()),
    ) {
        tracing::warn!(%err, "could not publish the update status");
    }
}

/// Persisted check result, so a launch is never delayed by the network.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Cached {
    /// Unix seconds when the answer was fetched.
    pub checked_at: u64,
    pub latest: String,
    pub next: Option<String>,
    pub alpha: Option<String>,
}

impl Cached {
    pub fn from_registry(registry: &Registry, now: u64) -> Cached {
        Cached {
            checked_at: now,
            latest: registry.latest.to_string(),
            next: registry.next.as_ref().map(|v| v.to_string()),
            alpha: registry.alpha.as_ref().map(|v| v.to_string()),
        }
    }

    pub fn registry(&self) -> Option<Registry> {
        Some(Registry {
            latest: Version::parse(&self.latest)?,
            next: self.next.as_deref().and_then(Version::parse),
            alpha: self.alpha.as_deref().and_then(Version::parse),
        })
    }

    pub fn is_fresh(&self, now: u64, interval: std::time::Duration) -> bool {
        now.saturating_sub(self.checked_at) < interval.as_secs()
    }

    pub fn read(path: &Path) -> Option<Cached> {
        let text = std::fs::read_to_string(path).ok()?;
        serde_json::from_str(&text).ok()
    }

    pub fn write(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let text = serde_json::to_string_pretty(self)
            .map_err(std::io::Error::other)?;
        std::fs::write(path, text)
    }
}

// ------------------------------------------------------------------- staging

/// Create the staged tree for `version` and prove it starts.
///
/// Runs `bun install` into a directory under the install prefix, with a
/// `package.json` that names exactly one dependency and no lockfile, so the
/// resolution is fresh — the property whose absence is what makes an in-place
/// global install produce a mixed tree.
pub fn stage(install: &Install, version: &Version) -> Result<PathBuf, String> {
    let bun = install
        .bun
        .as_ref()
        .ok_or("no package manager found to stage with")?;

    let dir = install.stage_dir(version);
    // Start clean: a half-finished previous attempt must not be upgraded into
    // something that looks complete.
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).map_err(|err| format!("creating {}: {err}", dir.display()))?;

    let manifest = format!(
        "{{\n  \"name\": \"dsh-shell-staging\",\n  \"private\": true,\n  \"dependencies\": {{ \"{SCOPE}/{ROOT_PACKAGE}\": \"{version}\" }}\n}}\n"
    );
    std::fs::write(dir.join("package.json"), manifest)
        .map_err(|err| format!("writing the staging manifest: {err}"))?;

    run_with_timeout(
        bun,
        &[
            "install",
            "--cwd",
            &dir.to_string_lossy(),
            "--production",
        ],
        std::time::Duration::from_secs(600),
    )
    .map_err(|err| format!("staging {version} failed: {err}"))?;

    // Prove the staged tree can actually run before anything touches the live
    // one. This is the entire reason staging exists. It is skippable so the
    // swap machinery can be tested without a 280 MB install; nothing in the
    // shipping path sets that variable.
    if std::env::var_os("DSH_SHELL_UPDATER_SKIP_SMOKE").is_some() {
        tracing::warn!("staged smoke test skipped by DSH_SHELL_UPDATER_SKIP_SMOKE");
        return Ok(dir);
    }
    // The staged tree is run by the `bun` that installed it, so that is what
    // needs to be on PATH for the smoke test.
    let reported = version_of_script(&install.staged_command(version)?, install.bun.as_deref())
        .map_err(|err| format!("the staged {version} could not start: {err}"))?;
    if reported != *version {
        return Err(format!(
            "the staged tree reports {reported}, expected {version}"
        ));
    }

    Ok(dir)
}

// ------------------------------------------------------------------ applying

/// Replace the live scope with a staged one, keeping the old one aside.
///
/// The two renames are the only moment the install is not in a known state.
/// Both are within one filesystem by construction, so each is atomic and the
/// window between them is a rename's worth of time.
pub fn apply(install: &Install, version: &Version) -> Result<(), String> {
    let staged = install.staged_scope(version);
    if !staged.is_dir() {
        return Err(format!(
            "nothing staged at {}; run the staging step first",
            staged.display()
        ));
    }

    let backup = install.backup_dir();
    let _ = std::fs::remove_dir_all(&backup);

    if let Some(parent) = backup.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|err| format!("creating {}: {err}", parent.display()))?;
    }

    std::fs::rename(&install.scope, &backup).map_err(|err| {
        format!(
            "could not move {} aside: {err}",
            install.scope.display()
        )
    })?;

    if let Err(err) = std::fs::rename(&staged, &install.scope) {
        // Put the old tree back before reporting, so a failed apply leaves a
        // working shell rather than a missing one.
        let _ = std::fs::rename(&backup, &install.scope);
        return Err(format!("could not install the staged tree: {err}"));
    }

    Ok(())
}

/// Put the previous scope back, undoing [`apply`].
pub fn rollback(install: &Install) -> Result<(), String> {
    let backup = install.backup_dir();
    if !backup.is_dir() {
        return Err("there is no previous version to roll back to".into());
    }
    let failed = install
        .prefix
        .join("dsh-shell-staging")
        .join("failed-at");
    let _ = std::fs::remove_dir_all(&failed);

    std::fs::rename(&install.scope, &failed)
        .map_err(|err| format!("could not move the failed tree aside: {err}"))?;
    std::fs::rename(&backup, &install.scope)
        .map_err(|err| format!("could not restore the previous version: {err}"))?;
    Ok(())
}

/// Drop a rolled-back or superseded staging directory.
pub fn discard_stage(install: &Install, version: &Version) {
    let _ = std::fs::remove_dir_all(install.stage_dir(version));
}

// ----------------------------------------------------- the pending-upgrade note

/// A note that an upgrade was applied but has not yet proven itself.
///
/// [`apply`] cannot know whether the new tree actually runs: the shell is
/// already running the old one in memory, and the only honest test is the next
/// launch. So the target version is written down, and the next launch that
/// cannot start a host uses it to decide to put the old tree back.
///
/// The version is stored rather than a bare flag so a launch that somehow runs
/// a different version than intended does not clear a note it did not satisfy.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Pending {
    pub target: String,
}

/// Where the note lives — beside the backup it would restore, so the two cannot
/// get separated.
pub fn pending_path(install: &Install) -> PathBuf {
    install
        .prefix
        .join("dsh-shell-staging")
        .join("pending.json")
}

/// Record that `version` was applied and is on trial.
pub fn record_pending(install: &Install, version: &Version) -> Result<(), String> {
    let path = pending_path(install);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|err| format!("creating {}: {err}", parent.display()))?;
    }
    let note = serde_json::to_string(&Pending {
        target: version.to_string(),
    })
    .map_err(|err| format!("encoding the pending note: {err}"))?;
    std::fs::write(&path, note).map_err(|err| format!("writing {}: {err}", path.display()))
}

/// The version an applied-but-unproven upgrade is on trial for.
pub fn pending(install: &Install) -> Option<Version> {
    let text = std::fs::read_to_string(pending_path(install)).ok()?;
    let note: Pending = serde_json::from_str(&text).ok()?;
    Version::parse(&note.target)
}

/// The upgrade proved itself; stop watching it.
pub fn clear_pending(install: &Install) {
    let _ = std::fs::remove_file(pending_path(install));
}

// -------------------------------------------------------------- check result

/// The state of an upgrade, as the settings page shows it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Phase {
    /// Nothing known yet.
    #[default]
    Unknown,
    /// A check is running.
    Checking,
    /// Up to date on the selected channel.
    Current,
    /// A newer version exists and can be installed.
    Available,
    /// Staging, or applying a staged tree.
    Working,
    /// Installed; the host has to restart to pick it up.
    RestartRequired,
    /// Something failed; the string is shown to the user.
    Failed(String),
    /// This install cannot upgrade itself, with the reason.
    Unsupported(String),
}

impl Phase {
    pub fn as_str(&self) -> &'static str {
        match self {
            Phase::Unknown => "unknown",
            Phase::Checking => "checking",
            Phase::Current => "current",
            Phase::Available => "available",
            Phase::Working => "working",
            Phase::RestartRequired => "restartRequired",
            Phase::Failed(_) => "failed",
            Phase::Unsupported(_) => "unsupported",
        }
    }
}

/// Everything the settings page needs to render the update section.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Status {
    pub current: Option<Version>,
    pub target: Option<Version>,
    pub channel: Channel,
    pub phase: Phase,
    /// A message to show alongside the phase (a failure reason, or the reason
    /// an install is unsupported).
    pub message: Option<String>,
    /// When the last successful check happened, unix seconds.
    pub checked_at: Option<u64>,
}

impl Status {
    pub fn json(&self) -> serde_json::Value {
        // A phase that carries its own reason wins over `message`: the reason is
        // why the phase is what it is, and dropping it would show "failed" with
        // nothing to act on.
        let message = match &self.phase {
            Phase::Failed(reason) | Phase::Unsupported(reason) => Some(reason.clone()),
            _ => self.message.clone(),
        };
        serde_json::json!({
            "current": self.current.as_ref().map(|v| v.to_string()),
            "target": self.target.as_ref().map(|v| v.to_string()),
            "channel": self.channel.as_str(),
            "phase": self.phase.as_str(),
            "message": message,
            "checkedAt": self.checked_at,
            "busy": matches!(self.phase, Phase::Checking | Phase::Working),
        })
    }
}

/// Decide what to show from a current version, a registry answer and a channel.
///
/// Split out from the network code because this is the part with the branching
/// that is easy to get wrong — a user on a prerelease, a channel whose tag is
/// missing, a registry older than the install.
pub fn resolve(
    current: &Version,
    registry: &Registry,
    channel: Channel,
    checked_at: u64,
) -> Status {
    let target = registry.channel(channel);
    let phase = if target > *current {
        Phase::Available
    } else {
        Phase::Current
    };
    Status {
        current: Some(current.clone()),
        target: Some(target),
        channel,
        phase,
        message: None,
        checked_at: Some(checked_at),
    }
}

/// Unix seconds now.
pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// ------------------------------------------------------------ the entry point

/// Check for an update, using the cache when it is fresh.
///
/// `force` skips the cache, which is what the "Check now" button asks for.
/// Returns the status plus the installation it resolved, so a later apply does
/// not have to rediscover anything.
pub fn check(launcher: &Path, channel: Channel, force: bool) -> (Status, Option<Install>) {
    let Some(install) = Install::discover(launcher) else {
        return (
            Status {
                channel,
                phase: Phase::Unsupported(format!(
                    "{} is not a package-manager install, so it cannot upgrade itself",
                    launcher.display()
                )),
                ..Status::default()
            },
            None,
        );
    };

    let current = match version_of(&install.launcher) {
        Ok(version) => version,
        Err(err) => {
            return (
                Status {
                    channel,
                    phase: Phase::Failed(err),
                    ..Status::default()
                },
                Some(install),
            )
        }
    };

    let cache = cache_path();
    let cached = cache.as_deref().and_then(Cached::read);

    // A fresh cache is authoritative: it is what keeps a launch off the network.
    if !force {
        if let Some(cached) = cached.as_ref().filter(|c| c.is_fresh(now(), CHECK_INTERVAL)) {
            if let Some(registry) = cached.registry() {
                return (
                    resolve(&current, &registry, channel, cached.checked_at),
                    Some(install),
                );
            }
        }
    }

    match fetch_registry(std::time::Duration::from_secs(30)) {
        Ok(registry) => {
            if let Some(path) = cache.as_deref() {
                if let Err(err) = Cached::from_registry(&registry, now()).write(path) {
                    tracing::warn!(%err, "could not cache the registry answer");
                }
            }
            (resolve(&current, &registry, channel, now()), Some(install))
        }
        Err(err) => {
            // Serve a stale cache rather than nothing: a snapshot from yesterday
            // is still a useful answer, and the message says how old it is.
            if let Some(registry) = cached.as_ref().and_then(|c| c.registry()) {
                let mut status = resolve(&current, &registry, channel, cached.map_or(0, |c| c.checked_at));
                status.message = Some(format!("{err}; showing the last known result"));
                return (status, Some(install));
            }
            (
                Status {
                    current: Some(current),
                    channel,
                    phase: Phase::Failed(err),
                    ..Status::default()
                },
                Some(install),
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(text: &str) -> Version {
        Version::parse(text).unwrap_or_else(|| panic!("{text} should parse"))
    }

    // -------------------------------------------------------------- parsing

    #[test]
    fn parses_release_and_prerelease_versions() {
        assert_eq!(v("0.1.5"), Version { major: 0, minor: 1, patch: 5, prerelease: vec![] });
        assert_eq!(v("0.1.5-rc.2"), Version {
            major: 0, minor: 1, patch: 5,
            prerelease: vec!["rc".into(), "2".into()],
        });
        // Build metadata must not affect precedence.
        assert_eq!(v("1.2.3+build.9"), v("1.2.3"));
    }

    #[test]
    fn rejects_things_that_are_not_versions() {
        for bad in ["", "1.2", "1.2.3.4", "x.y.z", "1.2.3-", "v1.2.3"] {
            assert!(Version::parse(bad).is_none(), "{bad:?} should not parse");
        }
    }

    // ------------------------------------------------------------- ordering

    #[test]
    fn orders_release_versions() {
        assert!(v("0.1.6") > v("0.1.5"));
        assert!(v("0.2.0") > v("0.1.9"));
        assert!(v("1.0.0") > v("0.99.99"));
        assert!(v("0.1.5") == v("0.1.5"));
    }

    #[test]
    fn a_prerelease_sorts_below_its_release() {
        // The rule that decides whether an upgrade is offered at all.
        assert!(v("1.0.0-rc.1") < v("1.0.0"));
        assert!(v("0.1.5-rc.2") < v("0.1.5"));
    }

    #[test]
    fn orders_prerelease_identifiers_by_the_semver_rules() {
        assert!(v("0.1.5-rc.2") > v("0.1.5-rc.1"));
        assert!(v("0.1.5-rc.10") > v("0.1.5-rc.9"), "numeric identifiers compare as numbers");
        // Numeric identifiers rank below alphanumeric ones.
        assert!(v("1.0.0-alpha.1") < v("1.0.0-alpha.beta"));
        assert!(v("1.0.0-alpha") < v("1.0.0-beta"));
        // The canonical semver chain.
        let chain = ["1.0.0-alpha", "1.0.0-alpha.1", "1.0.0-alpha.beta", "1.0.0-beta",
                     "1.0.0-beta.2", "1.0.0-beta.11", "1.0.0-rc.1", "1.0.0"];
        for pair in chain.windows(2) {
            assert!(v(pair[0]) < v(pair[1]), "{} should be < {}", pair[0], pair[1]);
        }
    }

    #[test]
    fn an_alpha_of_a_later_version_outranks_an_rc_of_an_earlier_one() {
        // This is why a version *range* must never be the upgrade target: on
        // this ordering `^0.1.5-rc.1` is satisfied by 0.1.6-alpha.2.
        assert!(v("0.1.6-alpha.2") > v("0.1.5-rc.2"));
    }

    // -------------------------------------------------------------- channels

    #[test]
    fn a_channel_selects_its_tag() {
        let registry = Registry {
            latest: v("0.1.5-rc.2"),
            next: Some(v("0.1.5-rc.2")),
            alpha: Some(v("0.1.6-alpha.2")),
        };
        assert_eq!(registry.channel(Channel::Latest), v("0.1.5-rc.2"));
        assert_eq!(registry.channel(Channel::Alpha), v("0.1.6-alpha.2"));
    }

    #[test]
    fn a_channel_with_no_tag_falls_back_to_latest() {
        let registry = Registry { latest: v("0.1.5-rc.2"), next: None, alpha: None };
        assert_eq!(registry.channel(Channel::Alpha), v("0.1.5-rc.2"));
    }

    #[test]
    fn parses_channel_names() {
        assert_eq!(Channel::parse("latest"), Some(Channel::Latest));
        assert_eq!(Channel::parse("stable"), Some(Channel::Latest));
        assert_eq!(Channel::parse("Alpha"), Some(Channel::Alpha));
        assert_eq!(Channel::parse("beta"), None);
        assert_eq!(Channel::default(), Channel::Latest);
    }

    // -------------------------------------------------------------- resolve

    fn registry() -> Registry {
        Registry {
            latest: v("0.1.5-rc.2"),
            next: Some(v("0.1.5-rc.2")),
            alpha: Some(v("0.1.6-alpha.2")),
        }
    }

    #[test]
    fn an_older_install_is_offered_the_newer_one() {
        let status = resolve(&v("0.1.5-rc.1"), &registry(), Channel::Latest, 100);
        assert_eq!(status.phase, Phase::Available);
        assert_eq!(status.target, Some(v("0.1.5-rc.2")));
        assert_eq!(status.checked_at, Some(100));
    }

    #[test]
    fn an_up_to_date_install_is_not_offered_anything() {
        let status = resolve(&v("0.1.5-rc.2"), &registry(), Channel::Latest, 100);
        assert_eq!(status.phase, Phase::Current);
    }

    #[test]
    fn a_newer_local_install_is_not_downgraded() {
        // A user running a build ahead of the registry must not be "upgraded"
        // backwards onto the published tag.
        let status = resolve(&v("0.1.7"), &registry(), Channel::Latest, 100);
        assert_eq!(status.phase, Phase::Current);
    }

    #[test]
    fn the_alpha_channel_offers_the_alpha() {
        let status = resolve(&v("0.1.5-rc.2"), &registry(), Channel::Alpha, 100);
        assert_eq!(status.phase, Phase::Available);
        assert_eq!(status.target, Some(v("0.1.6-alpha.2")));
    }

    #[test]
    fn the_latest_channel_does_not_offer_the_alpha() {
        // The default channel must never pull someone onto a prerelease they
        // did not ask for.
        let status = resolve(&v("0.1.5-rc.1"), &registry(), Channel::Latest, 100);
        assert_eq!(status.target, Some(v("0.1.5-rc.2")));
    }

    // ---------------------------------------------------------------- status

    #[test]
    fn status_serialises_for_the_page() {
        let status = Status {
            current: Some(v("0.1.5-rc.1")),
            target: Some(v("0.1.5-rc.2")),
            channel: Channel::Latest,
            phase: Phase::Available,
            message: None,
            checked_at: Some(7),
        };
        let json = status.json();
        assert_eq!(json["current"], "0.1.5-rc.1");
        assert_eq!(json["target"], "0.1.5-rc.2");
        assert_eq!(json["phase"], "available");
        assert_eq!(json["channel"], "latest");
        assert_eq!(json["busy"], false);
        assert!(json["message"].is_null());
    }

    #[test]
    fn a_busy_phase_disables_the_controls() {
        for phase in [Phase::Checking, Phase::Working] {
            let status = Status { phase, ..Status::default() };
            assert_eq!(status.json()["busy"], true, "{:?} should be busy", status.phase);
        }
        for phase in [Phase::Available, Phase::Current, Phase::RestartRequired] {
            let status = Status { phase, ..Status::default() };
            assert_eq!(status.json()["busy"], false);
        }
    }

    #[test]
    fn a_failed_phase_keeps_its_reason() {
        let status = Status {
            phase: Phase::Failed("curl is missing".into()),
            ..Status::default()
        };
        assert_eq!(status.json()["phase"], "failed");
        assert_eq!(status.json()["message"], "curl is missing");
    }

    // ---------------------------------------------------------------- caching

    #[test]
    fn a_fresh_cache_is_trusted_and_a_stale_one_is_not() {
        let cached = Cached {
            checked_at: 1_000,
            latest: "0.1.5-rc.2".into(),
            next: None,
            alpha: Some("0.1.6-alpha.2".into()),
        };
        let day = CHECK_INTERVAL.as_secs();
        assert!(cached.is_fresh(1_000 + day - 1, CHECK_INTERVAL));
        assert!(!cached.is_fresh(1_000 + day, CHECK_INTERVAL));
        // A clock that moved backwards must not make the cache stale forever.
        assert!(cached.is_fresh(500, CHECK_INTERVAL));
    }

    #[test]
    fn a_cache_round_trips_through_json() {
        let registry = registry();
        let cached = Cached::from_registry(&registry, 42);
        let text = serde_json::to_string(&cached).unwrap();
        let back: Cached = serde_json::from_str(&text).unwrap();
        assert_eq!(back.registry().unwrap(), registry);
        assert_eq!(back.checked_at, 42);
    }

    #[test]
    fn a_cache_with_a_broken_version_is_ignored_rather_than_trusted() {
        let cached = Cached {
            checked_at: 1,
            latest: "not-a-version".into(),
            next: None,
            alpha: None,
        };
        assert!(cached.registry().is_none());
    }

    // ------------------------------------------------------------ registry doc

    #[test]
    fn reads_the_registry_document() {
        let document = r#"{
            "name": "@deepseek-ai/dsh",
            "dist-tags": { "latest": "0.1.5-rc.2", "next": "0.1.5-rc.2", "alpha": "0.1.6-alpha.2" },
            "versions": { "0.1.5-rc.2": { "dist": { "tarball": "https://example.invalid/x.tgz" } } }
        }"#;
        let registry = Registry::parse(document).unwrap();
        assert_eq!(registry.latest, v("0.1.5-rc.2"));
        assert_eq!(registry.alpha, Some(v("0.1.6-alpha.2")));
    }

    #[test]
    fn a_registry_document_without_latest_is_an_error() {
        assert!(Registry::parse(r#"{"dist-tags":{"next":"1.0.0"}}"#).is_err());
        assert!(Registry::parse("not json").is_err());
        assert!(Registry::parse("{}").is_err());
    }

    // ----------------------------------------------------------- install path

    #[test]
    fn discovers_a_bun_global_install() {
        let launcher = PathBuf::from("/home/u/.bun/bin/dsh");
        // The launcher is a symlink in reality; discovery works on the resolved
        // path, so feed it the real location.
        let real = PathBuf::from(
            "/home/u/.bun/install/global/node_modules/@deepseek-ai/dsh/lib/bin.js",
        );
        let install = Install::discover(&real).expect("should recognise the layout");
        assert_eq!(install.prefix, PathBuf::from("/home/u/.bun/install/global"));
        assert_eq!(
            install.scope,
            PathBuf::from("/home/u/.bun/install/global/node_modules/@deepseek-ai")
        );
        assert_eq!(install.launcher, real);
        let _ = launcher;
    }

    #[test]
    fn refuses_a_layout_it_does_not_understand() {
        // A bundled runtime and a source checkout both look like this, and both
        // have their own update story.
        for path in [
            "/Applications/DSH Shell.app/Contents/Resources/bin/dsh",
            "/Users/x/src/dsh-shell/target/release/dsh-shell",
            "/usr/local/bin/dsh",
        ] {
            assert!(
                Install::discover(Path::new(path)).is_none(),
                "{path} should not be treated as an install"
            );
        }
    }

    #[test]
    fn staging_paths_sit_inside_the_install_prefix() {
        let install = Install {
            launcher: PathBuf::from("/p/node_modules/@deepseek-ai/dsh/lib/bin.js"),
            resolved: PathBuf::from("/p/node_modules/@deepseek-ai/dsh/lib/bin.js"),
            scope: PathBuf::from("/p/node_modules/@deepseek-ai"),
            prefix: PathBuf::from("/p"),
            bun: None,
        };
        let version = v("0.1.5-rc.2");
        let stage = install.stage_dir(&version);
        // Same filesystem as the live scope, or the final rename is not atomic
        // and may fail outright across devices.
        assert!(stage.starts_with("/p"), "{stage:?} should be under the prefix");
        assert_eq!(install.backup_dir(), PathBuf::from("/p/dsh-shell-staging/previous"));
        assert_eq!(
            install.staged_launcher(&version),
            PathBuf::from("/p/dsh-shell-staging/0.1.5-rc.2/node_modules/@deepseek-ai/dsh/lib/bin.js")
        );
        assert_eq!(
            install.staged_scope(&version),
            PathBuf::from("/p/dsh-shell-staging/0.1.5-rc.2/node_modules/@deepseek-ai")
        );
    }

    #[test]
    fn a_staged_scope_and_its_launcher_agree() {
        // If these two ever disagree, apply() would move one tree into place
        // while the smoke test validated a different one.
        let install = Install {
            launcher: PathBuf::from("/p/node_modules/@deepseek-ai/dsh/lib/bin.js"),
            resolved: PathBuf::from("/p/node_modules/@deepseek-ai/dsh/lib/bin.js"),
            scope: PathBuf::from("/p/node_modules/@deepseek-ai"),
            prefix: PathBuf::from("/p"),
            bun: None,
        };
        let version = v("1.2.3");
        let scope = install.staged_scope(&version);
        let launcher = install.staged_launcher(&version);
        assert!(launcher.starts_with(&scope), "{launcher:?} should be inside {scope:?}");
    }

    #[test]
    fn version_display_round_trips() {
        for text in ["0.1.5", "0.1.5-rc.2", "1.0.0-alpha.beta"] {
            assert_eq!(v(text).to_string(), text);
        }
    }

    // ------------------------------------------------------- the swap on disk
    //
    // These build a fake install layout and exercise the real filesystem work.
    // `apply` and `rollback` are the only operations that can lose a working
    // installation, so they are tested directly rather than only reasoned about.

    /// A throwaway install laid out exactly as bun's global tree is, with
    /// `packages` standing in for the live scope's contents.
    fn fake_install(packages: &[&str]) -> (tempdir::TempDir, Install) {
        let root = tempdir::TempDir::new("dsh-updater-test");
        let prefix = root.path().join("global");
        let scope = prefix.join("node_modules").join(SCOPE);
        std::fs::create_dir_all(&scope).unwrap();
        for package in packages {
            std::fs::write(scope.join(package), package.as_bytes()).unwrap();
        }
        // Something that is not `dsh` inside the scope, standing in for the
        // third-party packages that live there in reality.
        std::fs::create_dir_all(scope.join("cordis")).unwrap();
        std::fs::write(scope.join("cordis").join("package.json"), "{\"version\":\"4.0.2\"}")
            .unwrap();

        let install = Install {
            launcher: scope.join(ROOT_PACKAGE).join("lib").join("bin.js"),
            resolved: scope.join(ROOT_PACKAGE).join("lib").join("bin.js"),
            scope,
            prefix,
            bun: None,
        };
        (root, install)
    }

    /// Stand in for a staged tree: create the scope with the given contents.
    fn fake_stage(install: &Install, version: &Version, packages: &[&str]) {
        let staged = install.staged_scope(version);
        std::fs::create_dir_all(&staged).unwrap();
        for package in packages {
            std::fs::write(staged.join(package), package.as_bytes()).unwrap();
        }
    }

    fn scope_contains(install: &Install, package: &str) -> bool {
        install.scope.join(package).is_file()
    }

    #[test]
    fn apply_replaces_the_scope_and_keeps_the_old_one() {
        let (_root, install) = fake_install(&["dsh", "dsh-old-only"]);
        let version = v("0.1.5-rc.2");
        fake_stage(&install, &version, &["dsh", "dsh-new-only"]);

        apply(&install, &version).expect("apply should succeed");

        // The new tree is live...
        assert!(scope_contains(&install, "dsh"));
        assert!(scope_contains(&install, "dsh-new-only"));
        // ...and a package the new version dropped is gone, which a per-package
        // merge would have left behind.
        assert!(!scope_contains(&install, "dsh-old-only"));
        // ...and the old tree is parked, not deleted.
        assert!(install.backup_dir().join("dsh-old-only").is_file());
        // The staging directory itself is consumed by the rename.
        assert!(!install.staged_scope(&version).exists());
    }

    #[test]
    fn rollback_restores_exactly_what_was_there() {
        let (_root, install) = fake_install(&["dsh", "dsh-old-only"]);
        let version = v("0.1.5-rc.2");
        fake_stage(&install, &version, &["dsh", "dsh-new-only"]);

        apply(&install, &version).unwrap();
        rollback(&install).expect("rollback should succeed");

        assert!(scope_contains(&install, "dsh"));
        assert!(scope_contains(&install, "dsh-old-only"), "the old package is back");
        assert!(!scope_contains(&install, "dsh-new-only"), "the new package is gone");
        // A rollback consumes the backup, so a second one has nothing to do.
        assert!(rollback(&install).is_err());
    }

    #[test]
    fn apply_refuses_when_nothing_was_staged() {
        let (_root, install) = fake_install(&["dsh"]);
        let err = apply(&install, &v("0.1.5-rc.2")).unwrap_err();
        assert!(err.contains("nothing staged"), "unexpected error: {err}");
        // The live tree must be untouched by a refused apply.
        assert!(scope_contains(&install, "dsh"));
    }

    #[test]
    fn a_second_apply_does_not_accumulate_backups() {
        let (_root, install) = fake_install(&["dsh", "first"]);
        let first = v("0.1.5-rc.1");
        fake_stage(&install, &first, &["dsh", "second"]);
        apply(&install, &first).unwrap();

        let second = v("0.1.5-rc.2");
        fake_stage(&install, &second, &["dsh", "third"]);
        apply(&install, &second).unwrap();

        // The backup holds the tree that was live until a moment ago — the
        // first staged tree — and not the original, nor both generations.
        assert!(install.backup_dir().join("second").is_file());
        assert!(!install.backup_dir().join("first").exists());
        assert!(scope_contains(&install, "third"));
    }

    #[test]
    fn rollback_after_a_second_apply_returns_to_the_middle_version() {
        let (_root, install) = fake_install(&["dsh", "v1"]);
        let first = v("0.1.5-rc.1");
        fake_stage(&install, &first, &["dsh", "v2"]);
        apply(&install, &first).unwrap();
        let second = v("0.1.5-rc.2");
        fake_stage(&install, &second, &["dsh", "v3"]);
        apply(&install, &second).unwrap();

        rollback(&install).unwrap();
        assert!(scope_contains(&install, "v2"), "one generation back, not two");
    }

    #[test]
    fn discard_removes_a_staged_tree() {
        let (_root, install) = fake_install(&["dsh"]);
        let version = v("0.1.5-rc.2");
        fake_stage(&install, &version, &["dsh"]);
        assert!(install.staged_scope(&version).exists());
        discard_stage(&install, &version);
        assert!(!install.staged_scope(&version).exists());
        // The live scope is not what was discarded.
        assert!(scope_contains(&install, "dsh"));
    }

    // ------------------------------------------------- the pending-upgrade note

    #[test]
    fn a_pending_note_round_trips_and_clears() {
        let (_root, install) = fake_install(&["dsh"]);
        let target = v("0.1.5-rc.2");

        // Nothing is pending before an upgrade.
        assert_eq!(pending(&install), None);

        record_pending(&install, &target).unwrap();
        assert_eq!(pending(&install), Some(target));

        clear_pending(&install);
        assert_eq!(pending(&install), None);
    }

    #[test]
    fn a_pending_note_lives_beside_the_backup_it_would_restore() {
        // If these ever diverge, a launch could decide to roll back and find
        // nothing to roll back to.
        let (_root, install) = fake_install(&["dsh"]);
        assert_eq!(pending_path(&install).parent(), install.backup_dir().parent());
    }

    #[test]
    fn a_corrupt_pending_note_is_ignored_rather_than_acted_on() {
        // A note that cannot be read must not cause a rollback of a version
        // that is in fact fine.
        let (_root, install) = fake_install(&["dsh"]);
        let path = pending_path(&install);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        for junk in ["", "not json", "{\"target\":\"nonsense\"}"] {
            std::fs::write(&path, junk).unwrap();
            assert_eq!(pending(&install), None, "{junk:?} should be ignored");
        }
    }

    #[test]
    fn clearing_a_note_that_is_not_there_is_harmless() {
        let (_root, install) = fake_install(&["dsh"]);
        clear_pending(&install);
        assert_eq!(pending(&install), None);
    }

    #[test]
    fn a_second_upgrade_replaces_the_pending_note() {
        let (_root, install) = fake_install(&["dsh"]);
        record_pending(&install, &v("0.1.5-rc.1")).unwrap();
        record_pending(&install, &v("0.1.5-rc.2")).unwrap();
        assert_eq!(pending(&install), Some(v("0.1.5-rc.2")));
    }

    // ------------------------------------------------- the PATH the GUI lacks

    #[test]
    fn the_launcher_directory_comes_first_on_the_augmented_path() {
        // The bug this guards: a Finder-launched app inherits launchd's PATH,
        // which has no `node`, and `dsh` is a `#!/usr/bin/env node` script — so
        // the probe died with exit 127, "env: node: No such file or directory",
        // and reported a healthy install as broken.
        //
        // Asserted on the pure PATH builder, which reads and writes nothing
        // global. A spawn-based version of this test was written first and
        // removed: it needed the child to resolve `env node`, and the only
        // faithful way to arrange that was to rewrite the process environment,
        // which races every other test that spawns a subprocess. The runtime
        // behaviour is covered where it is actually reachable — the isolated
        // host probe and the installed app both report their version.
        let path = crate::server::path_with_node_list(
            "/opt/custom/bin/dsh",
            "/usr/bin:/bin".to_string(),
            None,
        );
        assert_eq!(
            path, "/opt/custom/bin:/usr/bin:/bin",
            "the launcher's own directory must be prepended"
        );

        // A `node` found elsewhere is added too, without displacing the launcher.
        let path = crate::server::path_with_node_list(
            "/opt/custom/bin/dsh",
            "/usr/bin:/bin".to_string(),
            Some(PathBuf::from("/hermes/node/bin/node")),
        );
        let parts: Vec<&str> = path.split(':').collect();
        assert_eq!(parts[0], "/opt/custom/bin");
        assert_eq!(parts[1], "/hermes/node/bin");
        assert!(parts.contains(&"/usr/bin"), "the existing PATH must survive");

        // An empty existing PATH must not leave a leading colon, which would put
        // the working directory on the search path.
        assert_eq!(
            crate::server::path_with_node_list("/opt/bin/dsh", String::new(), None),
            "/opt/bin"
        );
    }

    #[test]
    fn the_path_augmentation_puts_the_launcher_directory_first() {
        // Order matters: a stale `node` further along PATH must not win.
        let augmented = crate::server::path_with_node("/opt/custom/bin/dsh");
        let first = augmented.split(':').next().unwrap_or_default();
        assert_eq!(first, "/opt/custom/bin");
        // The existing PATH survives, so nothing else the launcher needs is lost.
        assert!(augmented.contains("/usr/bin") || std::env::var("PATH").unwrap_or_default().is_empty());
    }

    /// The whole staged path against a real registry: install, verify, swap.
    ///
    /// Ignored by default because it downloads ~280 MB and takes about ten
    /// seconds. Run it when the staging or apply code changes:
    ///
    /// ```sh
    /// cargo test --release -- --ignored --nocapture a_real_staged_upgrade
    /// ```
    ///
    /// It deliberately builds its own fake prefix, so it can never touch the
    /// install the developer is running.
    #[test]
    #[ignore = "downloads ~280 MB from the registry"]
    fn a_real_staged_upgrade_installs_verifies_and_swaps() {
        let Some(bun) = resolve_bun(Path::new("/nonexistent/dsh"), Path::new("/nonexistent")) else {
            eprintln!("skipping: no bun on this machine");
            return;
        };

        let root = tempdir::TempDir::new("dsh-updater-live");
        let prefix = root.path().join("global");
        let scope = prefix.join("node_modules").join(SCOPE);
        // A live tree that is obviously not the new one, so the swap is visible.
        std::fs::create_dir_all(&scope).unwrap();
        std::fs::write(scope.join("dsh"), b"old").unwrap();

        let install = Install {
            launcher: scope.join(ROOT_PACKAGE).join("lib").join("bin.js"),
            resolved: scope.join(ROOT_PACKAGE).join("lib").join("bin.js"),
            scope: scope.clone(),
            prefix: prefix.clone(),
            bun: Some(bun),
        };

        // Stage the version the registry currently calls latest.
        let registry = fetch_registry(std::time::Duration::from_secs(60))
            .expect("registry should be reachable for this test");
        let target = registry.latest.clone();
        eprintln!("staging {target} into {}", install.stage_dir(&target).display());

        stage(&install, &target).expect("staging should succeed and verify");

        // The smoke test inside `stage` already ran the staged launcher; this
        // asserts the tree it left behind is the real thing.
        let staged_packages = std::fs::read_dir(install.staged_scope(&target))
            .expect("staged scope should exist")
            .count();
        assert!(
            staged_packages > 100,
            "expected a full scope, found {staged_packages} entries"
        );
        assert!(
            install.staged_scope(&target).join("cordis").is_dir(),
            "the scope's third-party packages should be staged too"
        );

        apply(&install, &target).expect("apply should succeed");
        assert!(scope.join(ROOT_PACKAGE).join("lib").join("bin.js").is_file());
        assert!(scope.join("cordis").is_dir(), "third-party packages come along");

        // The staged tree now lives at the real path. Verify it the way a real
        // install runs — through a package-manager shim, never by executing
        // `bin.js` directly, which is not an executable and which the
        // launcher/resolved split exists to keep out of the spawn path.
        let shim = install.bun.as_ref().unwrap().parent().unwrap().join(ROOT_PACKAGE);
        assert!(
            shim.is_file(),
            "the test needs a shim at {} to verify the live tree",
            shim.display()
        );
        let reported = version_of(&shim).expect("the live launcher should run");
        assert_eq!(reported, target);

        // And rolling back puts the placeholder back.
        rollback(&install).expect("rollback should succeed");
        assert_eq!(std::fs::read(scope.join("dsh")).unwrap(), b"old");
    }
}

/// A minimal temporary-directory helper.
///
/// Written rather than depended on so the test suite gains no crate for
/// something this small; it removes its directory on drop so a failed test does
/// not leave a fake install in the temp folder.
#[cfg(test)]
mod tempdir {
    use std::path::{Path, PathBuf};

    pub struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        pub fn new(prefix: &str) -> TempDir {
            // A counter keeps two tests in the same process from colliding; the
            // process id separates concurrent test binaries.
            use std::sync::atomic::{AtomicU32, Ordering};
            static COUNTER: AtomicU32 = AtomicU32::new(0);
            let unique = format!(
                "{prefix}-{}-{}",
                std::process::id(),
                COUNTER.fetch_add(1, Ordering::Relaxed)
            );
            let path = std::env::temp_dir().join(unique);
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).expect("create temp dir");
            TempDir { path }
        }

        pub fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }
}
