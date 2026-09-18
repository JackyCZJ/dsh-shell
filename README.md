# DSH Shell

A native desktop shell for the **DeepSeek Harness** web UI.

No system titlebar. The page fills the window edge to edge and the macOS traffic
lights float over it. The renderer is the platform's own webview — **WebKit** on
macOS, WebView2 on Windows, WebKitGTK on Linux — so nothing is bundled and the
binary stays ~2.7 MB.

```sh
git clone https://github.com/JackyCZJ/dsh-shell
cd dsh-shell
./scripts/make-app.sh          # builds and installs to /Applications
open -a "DSH Shell"
```

## Why a webview, not a native UI

This shell deliberately does **not** reimplement the interface.

DSH's web client is 55 plugins and roughly 204,000 lines: streaming markdown,
syntax highlighting, diffs, a file tree, settings, session history, document
preview. Reproducing that natively is a multi-month project that would then have
to track every upstream change.

The upstream Electron desktop app reached the same conclusion — its client code
is under 500 lines and it reuses the shipped web UI. This shell does the same, but
with the platform webview instead of a bundled Chromium, which removes the
runtime it was trying to avoid in the first place.

## Features

| | |
|---|---|
| **No system titlebar** | Transparent titlebar, full-size content, inset traffic lights |
| **Window dragging** | A drag strip over the caption area, since no OS drag region remains |
| **Native app menu** | Standard Edit menu, so ⌘C/⌘V/⌘X/⌘A work |
| **Tray icon** | Agent state (idle / working / failed) as a colour-coded whale, rasterised at 2x and antialiased |
| **Global hotkey** | **⌘⇧D** summons the window from anywhere |
| **Desktop notifications** | On turn completion and on failure |
| **Close to tray** | Closing hides; the host and session keep running |
| **Boot screen** | DSH's own loading state — whale, wordmark, spinner |
| **Light / dark** | Follows DSH's `ui-theme.preference`, then the OS |
| **Live settings** | Changes to the namespace update the chrome *and* the page, no restart |
| **Settings window** | Tray → Settings…; writes go through DSH, which persists them |
| **Follows DSH's language** | Chinese or English, from DSH's own `locale.preference` |
| **Single instance** | A second launch raises the running window instead of starting a second shell |
| **Remembers the window** | Size, position, and zoom come back on the next launch |
| **Dock reopen** | Clicking the dock icon restores a hidden or minimized window |
| **Dock badge + tray count** | Finished turns you have not looked at, as a red number on the dock icon and the same number beside the menu-bar icon; clears when the window is in front again |
| **Upgrading the DSH it runs** | Checks the registry once per launch (cached), and on request stages, verifies and swaps in a newer DSH — rolling itself back if the new one will not start. Nothing installs without a click |
| **The settings UI is DSH's own** | A client plugin renders the appearance, shortcut and update controls inside DSH's General settings; the shell's own window keeps them as a fallback for when DSH will not start |
| **A readable log** | Diagnostics go to `$DSH_HOME/cache/dsh-shell/dsh-shell.log`, not to a terminal a GUI app does not have |
| **Plugin bridge** | A real DSH Host plugin forwards `agent/*` events to the shell |

## Requirements

- **macOS 11+** (primary target; Windows and Linux compile but are untested)
- **Rust 1.98.1** — pinned in `rust-toolchain.toml`, rustup installs it
- **DeepSeek Harness** installed on your machine

### Prerequisite: Metal Toolchain

If the build fails with
`cannot execute tool 'metal' due to missing Metal Toolchain`:

```sh
xcodebuild -downloadComponent MetalToolchain   # ~1.5 GB, one time
xcrun -sdk macosx metal --version              # verify
```

## Install

```sh
./scripts/make-app.sh              # build + install to /Applications
./scripts/make-app.sh --no-install # assemble in ./dist only
```

`make-app.sh` ad-hoc signs the bundle, which is enough to run locally.
Distribution to other machines needs a Developer ID and notarization — see
[Packaging](#packaging).

## Configuration

| Variable | Default | Meaning |
|---|---|---|
| `DSH_BIN` | auto-detected | Path to the `dsh` launcher |
| `DSH_PORT` | `0` | Port for `dsh web`; `0` lets the OS choose |
| `DSH_SETTINGS` | `$DSH_HOME/settings.yaml` | Settings document to read |
| `DSH_HOME` | `~/.dsh` | DSH home; locates `settings.yaml` |
| `DSH_SHELL_SOCKET` | `$TMPDIR/dsh-shell-<uid>.sock` | Bridge socket (the plugin honours it too) |
| `DSH_SHELL_LOCK` | `$TMPDIR/dsh-shell-<uid>.lock` | Single-instance lock file |
| `DSH_SHELL_ACTIVATE` | `$TMPDIR/dsh-shell-<uid>.activate.sock` | Handoff socket |
| `DSH_SHELL_WINDOW_STATE` | `$DSH_HOME/cache/dsh-shell/window.json` | Remembered window rectangle |
| `RUST_LOG` | `info` | Log filter |

## Configuration lives in DSH

The shell has **no config file of its own**. Its settings live in DSH's
`settings.yaml` under the `dsh-shell` namespace, registered by the Host plugin:

```yaml
dsh-shell:
  hotkey: meta+shift+D
  light:
    background: "#ffffff"
    surface: "#f5f6f7"
    surfaceHover: "#e9ebed"
    border: "#d9dcdf"
    text: "#0f1115"
    textMuted: "#81858c"
    accent: "#4176e6"
  dark:
    background: "#151517"
    surface: "#2c2c2e"
    surfaceHover: "#3a3a3c"
    border: "#3f3f42"
    text: "#f9fafb"
    textMuted: "#adb2b8"
    accent: "#4176e6"
  customCss: ""
```

This is the same arrangement the official DSH desktop app uses for its own
`dsh-desktop` namespace, and it means **DSH owns validation and persistence**.
The shell only reads.

Two ways to change it, both landing in the same document:

| | |
|---|---|
| **The shell's settings window** | Tray → **Settings…**. Writes go through the Host plugin, so DSH persists them and the rest of `settings.yaml` is preserved by DSH's own writer. |
| **Editing the document** | Any editor. The shell watches the file and applies changes live. |

You can also call the namespace directly from any DSH-side surface:

```js
ctx.settings.update('dsh-shell', { hotkey: 'meta+alt+K' })
```

### Applied live

Both palettes, the caption height, the traffic-light inset, the hotkey, and
`customCss` apply **without a restart and without losing the session**. The file
watcher covers hand edits; a change written through the namespace is pushed to
the shell so it lands immediately rather than waiting out the debounce.

A rejected value never leaves you without a working setting: an unavailable
hotkey keeps the previous shortcut, and a malformed document keeps the last good
theme rather than blanking the window.

### How light and dark are decided

There is **no shell-side appearance setting**, deliberately. The page follows
DSH's own `ui-theme.preference`, so the chrome resolves from the same value:

1. **DSH's `ui-theme.preference`** from `settings.yaml`.
2. **The operating system**, when DSH says `system` or says nothing.

A shell override could only ever make the two disagree — a dark caption bar over
a light page — which is the exact seam this theming exists to remove. To change
the theme, change it in DSH's Settings. The official desktop app takes the same
position: it mirrors DSH's preference and offers no override of its own.

Both palettes are taken from **DSH's own boot-theme CSS** (`#151517` dark,
`#ffffff` light), so the window chrome and the page share one colour.

The caption-strip height and the traffic-light inset are **not configurable**.
They are fixed chrome that has to line up with itself, and exposing them invited
a broken drag strip far more easily than it enabled anything useful. They live as
constants in `src/theme.rs`.

## Language

The shell follows DSH's own `locale.preference` from `settings.yaml`:

```yaml
locale:
  preference: zh   # or en
```

Everything the shell owns is localised: the tray menu, the application menu, the
settings window, the boot screen, and notifications. The strings live in
`src/i18n.rs` as a struct per language, so a missing translation is a compile
error rather than a blank label, and a test asserts the two tables differ.

macOS localises the standard Edit items itself, so Cut/Copy/Paste keep working
and appear in the system language whatever the shell is set to.

> **A language change applies to the tray and menus on restart.** They are built
> once at startup. The settings window picks the new language up when reopened.

## Keyboard

| Shortcut | Action |
|---|---|
| **⌘⇧D** | Summon the window (global, works when the app is hidden) |
| ⌘C / ⌘V / ⌘X / ⌘A | Standard clipboard |
| ⌘R | Reload the page |
| ⌘W | Hide the window to the tray |
| ⌘Q | Quit (stops the host) |

macOS routes ⌘C/⌘V/⌘X/⌘A **through the Edit menu**, not to the focused view. A
windowed app with no menu bar therefore has no clipboard shortcuts at all,
however complete its webview is. The menu in `src/menu.rs` exists to give the
system something to bind them to.

## The plugin bridge

The shell is a participant in DSH's plugin ecosystem, not just a window around
it. `dsh-plugin-shell-bridge` is a **Host plugin** installed with `dsh plugin`,
whose lifecycle the Loader owns:

```sh
dsh plugin --profile web add ./dsh-plugin-shell-bridge
```

It subscribes to `agent/*` events and forwards them over a Unix socket; the shell
turns them into tray state and notifications.

| Event | Effect |
|---|---|
| `agent/status` | Tray colour: idle / working / failed |
| `agent/turn-stopping` | Tray → idle, notification, dock + tray count |
| `agent/request-error` | Tray → failed, notification with the message |

The shell works identically without the plugin: the bridge is optional by design,
and the plugin cannot break the host.

That last claim is a property to maintain, not an accident, and two ways of
losing it are worth knowing:

- **A listener on a cordis `waterfall` must call `next()`.** `agent/request-error`
  is a waterfall and `dsh-llm-retry` is a listener on it; a listener that returns
  without forwarding "vetoes the rest of the chain, including the built-in
  behavior", which silently disables retries for the whole host. Observing must
  never change what is observed.
- **Agent events carry `payload.agent`, not a `sessionId`.** Every agent-subject
  event is dispatched through a fused carrier that injects the subject, so the
  identity is `agent.id` and a top-level `sessionId` is always `undefined`. That
  mistake does not break anything — the notification still appears — which is
  exactly why it went unnoticed: it only means the shell cannot name the session.
  `agent/request-error` likewise nests its text under `failure.message`.

The payload shape is pinned by tests against the emit sites, so guessing at a
field fails the suite rather than quietly degrading a notification.

## Architecture

```
┌─────────────────────────────┐
│ dsh Host (Node)             │
│  └─ cordis row: shell-bridge│──┐  newline-delimited JSON
└─────────────────────────────┘  │  over a Unix socket
                                 ▼
                    ┌────────────────────────┐
                    │ dsh-shell (Rust)       │
                    │  window · tray · menu  │
                    │         │              │
                    │         ▼              │
                    │   webview → dsh web    │
                    └────────────────────────┘
```

```
src/
  main.rs    window, traffic lights, webview, event loop, CSS injection
  menu.rs    the application menu (what makes ⌘C/⌘V work)
  native.rs  tray, notifications, global hotkey, appearance detection
  server.rs  spawns `dsh web`, parses its URL, kills it on exit
  bridge.rs  the shell's side of the plugin bridge
  instance.rs single-instance lock and the handoff to a running shell
  runtime.rs where the shell's per-user runtime files live
  settings.rs the settings window
  theme.rs   tokens, settings.yaml reading, watcher
  window_state.rs the remembered window rectangle
assets/
  boot.html       the loading screen
  deepseek.svg    DSH's official whale; also the source of the tray glyph
  app-icon.png    the app icon's source artwork (1024x1024)
  deepseek.icns   the squircle-masked icon built from it
# configuration lives in DSH's settings.yaml
```

### Runtime files

A running shell owns three files in `$XDG_RUNTIME_DIR` (on macOS, the per-user
temp directory), all namespaced by uid:

| File | Purpose |
|---|---|
| `dsh-shell-<uid>.sock` | the bridge the Host plugin dials |
| `dsh-shell-<uid>.lock` | the `flock` that makes a second launch a handoff |
| `dsh-shell-<uid>.activate.sock` | how that second launch asks the first to raise its window |

The lock file is deliberately never deleted. Only the lock on it matters, and
unlinking it would be a race: a launch that had just opened the path would hold
a lock on an inode with no name, while a later launch would create a fresh inode
and lock that — leaving two shells each believing it was the only one.
`DSH_SHELL_SOCKET`, `DSH_SHELL_LOCK`, and `DSH_SHELL_ACTIVATE` override the
paths, which is how the handoff is tested without disturbing a live session.

The window rectangle is *state*, not configuration, so it lives outside DSH's
settings document, in `$DSH_HOME/cache/dsh-shell/window.json`. Deleting it only
reopens the window at its default size. It is written about twice a second while
you drag rather than at exit, so a window lost to a crash still reopens where it
was; a position that no longer lands on an attached display is discarded rather
than applied off-screen.

### Upgrading the DSH it runs

The shell carries no DSH of its own — it runs whatever `resolve_launcher` finds,
which for a normal install is `~/.bun/bin/dsh` pointing into bun's global tree.
Keeping that current is `src/updater.rs`, driven from the Updates section of the
settings window and from **Check for DSH Updates…** in the tray menu.

- **Detection is automatic, applying is not.** One check per launch, satisfied
  from a cached answer (`$DSH_HOME/cache/dsh-shell/update.json`) until it is 24
  hours old, so a launch never waits on the network. Nothing is ever installed
  without a click: this project publishes release candidates, and an unattended
  apply could move someone onto a broken prerelease overnight.
- **The channel is a setting** (`updateChannel`, `latest` or `alpha`). A range is
  not a version: `^0.1.5-rc.1` is satisfied by `0.1.6-alpha.2`, so the target is
  always an explicit version taken from a dist-tag.
- **Upgrading never uses `bun install -g` in place.** Measured: that left 23
  packages at the old version and 208 at the new one, because bun reuses the
  global lockfile's resolutions. A fresh resolve in an empty directory produced
  231 consistent ones. So the new tree is resolved into a staging directory, the
  staged launcher is run to prove it starts, and only then is it swapped in.
- **The swap keeps the previous tree.** Both renames are inside one filesystem,
  so each is atomic; the old tree is moved aside rather than deleted, making a
  rollback a rename instead of a 280 MB download.
- **A bad release undoes itself.** `apply` cannot know whether the new tree boots
  — the old version is still live in memory — so it writes a note naming the
  target. The next launch that cannot start a host treats that as the verdict:
  restore the previous tree, clear the note, try once more. Once, so a failure
  unrelated to the upgrade reports its own error instead of cycling.

The last step is a restart, because the running host is the old version. The
settings window says so; the shell does not restart itself, since on a machine
where the shell hosts a DSH session that would kill the session.

Two mistakes here are worth not repeating, both of which failed while looking
like something else. Polling a child's exit status while its stdout is piped
deadlocks once the child fills the 64 KB pipe buffer — the registry document is
142 KB, so every check "timed out" on a working network; the reader now runs on
its own thread. And canonicalising the launcher's symlink yields a `lib/bin.js`
that is not executable, so `Install` keeps the path to *run* and the resolved
path to *read the layout from* separately.

### Two threading rules

Both were learned the hard way and are worth knowing before extending this:

1. **Never block the winit event loop.** Tray, hotkey, bridge, and menu events
   arrive on their own threads and cannot wake it. With `ControlFlow::Wait` they
   sat unread until unrelated input ticked the loop, so notifications appeared
   only when the mouse moved. The loop polls every 100 ms.

2. **`wry::WebView` is `!Send`.** It wraps main-thread-only AppKit objects, so a
   background thread must never touch it. The theme watcher publishes over a
   channel and the event loop applies the change; the compiler enforces this.

3. **Keep draining the host's stdout.** The launcher stops reading once it has
   the URL, so the reader is moved into a task rather than dropped. A host that
   keeps writing — a plugin logging, for instance — would otherwise block forever
   once the pipe buffer filled.

## Packaging

### Distributing to machines without DSH

The app resolves a `dsh` launcher itself, so a machine without DSH cannot run it.
`--bundle-runtime` ships one:

```sh
scripts/make-app.sh --bundle-runtime          # also installs to /Applications
scripts/make-app.sh --bundle-runtime --no-install
scripts/make-app.sh --bundle-runtime --dsh-version 0.1.5-rc.1
```

It installs `@deepseek-ai/dsh` with **bun** into `Contents/Resources/runtime/`,
copies the bun binary beside it, and writes `Contents/Resources/bin/dsh` — the
exact path `server.rs` already prefers. The bundle goes from 3.9 MB to **214 MB**:

| Component | Size |
|---|---|
| bun runtime | 63 MB |
| `node_modules` (after pruning) | ~147 MB |
| shell binary + icon | 3.9 MB |

Bun rather than Node: 63 MB against Node's 113 MB, and it is a single static
binary with nothing to sign beyond itself. It must be launched as `bun run dsh`,
**not** `bun <path>/lib/bin.js` — the latter cannot resolve the plugin tree. The
shim does that, and `exec`s, so the shell still supervises the pid it started.

Three things the build does that are not obvious:

- **Pruning.** DSH resolves plugins through the *dependency closure* of its
  install, so packages cannot simply be deleted. Dropping `*.d.ts`, `*.map` and
  `*.pdb` removes 130 MB that nothing reads at runtime and cannot affect
  resolution. TypeScript *sources* are kept: only another 7 MB, and a package
  whose runtime entry is `.ts` would break without them.
- **The install anchor.** DSH writes `$DSH_HOME/profiles/node_modules` as links
  into whichever install launched it. Bundling at `Contents/Resources/runtime`
  is what makes a fresh machine's profile point into the app instead of at a
  global install that is not there. Verified against an empty `DSH_HOME`: the
  profile initialises and `@deepseek-ai/dsh-base` links to
  `…/DSH Shell.app/Contents/Resources/runtime/node_modules/…`, and the UI answers
  `200` with `<title>DeepSeek Harness</title>`.
- **A self-check.** The script runs the bundled launcher's `--version` before
  signing, so it cannot produce a bundle that fails to start.

Two things it does not yet handle. No lockfile is written — bun 1.4 does not emit
one for this layout — so transitive versions are re-resolved on each build. And
the shell's own bridge plugin still lives in the user's profile, so a fresh
machine gets a working app without tray integration until it is installed.

Node also works if you would rather not depend on bun: install
`@deepseek-ai/dsh` into the same directory with npm or pnpm and point the shim at
`node`. That path is untested here.

### Why the launcher needs resolving at all

`dsh` is installed by bun, nvm, Homebrew, or a version manager — none of which are
on the PATH a GUI-launched app receives. macOS gives apps launched from Finder the
bare `/usr/bin:/bin:/usr/sbin:/sbin`, and `launchctl getenv PATH` is empty.

Worse, `dsh` is a `#!/usr/bin/env node` script, so finding `dsh` is not enough:
it also needs a `node` on PATH.

Measured on the development machine:

```
$ launchctl getenv PATH
                       # empty -> the bare default applies
$ PATH=/usr/bin:/bin:/usr/sbin:/sbin command -v dsh
                       # not found
$ PATH=/usr/bin:/bin:/usr/sbin:/sbin command -v node
                       # not found
```

The shell therefore resolves the launcher in `server.rs`:

1. `DSH_BIN`, when set explicitly.
2. `Contents/Resources/bin/dsh` inside the app bundle.
3. `$HOME/{.bun,.local,.volta}/bin/dsh` and npm global prefixes.
4. `$HOME/.nvm/versions/node/*/bin/dsh`, `$HOME/.fnm/node-versions/*/bin/dsh`.
5. `dsh` from PATH, correct when launched from a terminal.

It then prepends the launcher's and `node`'s directories to the child's PATH, so
`#!/usr/bin/env node` resolves. Verified with
`env -i PATH=/usr/bin:/bin:/usr/sbin:/sbin`, which reproduces a Finder
double-click.

### Signing and notarization

`make-app.sh` signs with the first codesigning identity it finds, or with
`CODESIGN_IDENTITY` when set, and falls back to ad-hoc. **This is not only about
Gatekeeper: notifications need a real signature.** An ad-hoc app has no Team ID,
so `usernotificationsd` cannot read its code-signing record — it logs
`Couldn't get record to check entitlement key` and refuses every
`requestAuthorization`. The app then never appears in System Settings >
Notifications and every notification is dropped without a word. Everything else
works ad-hoc, which is why this is easy to miss.

Signing with an Apple Development certificate is enough for notifications on the
machine that owns it. Distributing to other machines still needs a Developer ID
and notarization:

```sh
xcrun notarytool submit "DSH Shell.app.zip" --keychain-profile <profile> --wait
xcrun stapler staple "DSH Shell.app"
```

- Notarization requires a paid Developer ID and the hardened runtime, which the
  script applies only for a `Developer ID` identity.
- Add the `com.apple.security.network.client` entitlement — the webview connects
  to the loopback host.
- Re-sign after changing anything inside the bundle, or
  the signature is invalidated.
- Changing the signing identity changes the Team ID, and the system treats that
  as a different app: the notification permission has to be granted again.

## Development

```sh
cargo run          # run from source
cargo test         # 167 tests
cargo clippy       # lints
./scripts/make-icon.py   # regenerate the .icns from assets/app-icon.png
```

The tests cover the parts that are easy to get subtly wrong: URL parsing, CSS
injection and escaping, the appearance preference parser, theme reload semantics,
SVG path flattening, and the menu structure that makes the clipboard work.

### The app icon

`scripts/make-icon.py` does the whole job with the standard library: it decodes
`assets/app-icon.png`, area-averages it down, masks it to the macOS icon shape,
writes the ten PNGs `iconutil` wants, and runs `iconutil`. It replaced a
`sips`-based script that rendered `assets/deepseek.svg`, which could not draw the
mask.

The mask's geometry is measured, not recalled. Extracting the `AppIcon.icns`
from `Notes`, `Music`, `Calculator` and `Reminders` with `iconutil -c iconset`
and thresholding the alpha gives identical numbers for all four:

- the icon body spans **206/256** of the canvas, which is Apple's 824/1024 grid;
- the mid-edge runs are full width, so the shape is a flat-edged rounded
  rectangle — a superellipse such as `|x|^5 + |y|^5 = 1` is **not** the shape,
  and curves its edges inward where the real icons stay flat;
- the corner's implied circular radius is **0.228–0.246** of the body across a
  10× range of the arc. A superellipse's implied radius drifts far more than
  that over the same range, which is what rules it out.

`0.235 × body` is 185.6px on a 1024 canvas, against the 185.4 Apple documents
for the macOS grid. The final check is area: the system icons fill **0.6173** of
the canvas (0.6199 by coverage-weighted area), and the generated mask fills
**0.6169** (0.6199). An earlier superellipse version of this script filled
0.6086 *and* had transparent pixels where the real icons are opaque at the
mid-edges — which is how the error was caught, by probing the mask rather than
eyeballing the result.

Two guards now exist because the script got this wrong twice in one sitting, both
times silently:

- `resample` takes the source's channel count and raises when the buffer length
  disagrees with it. It once hardcoded 3 and was handed the 4-channel masked
  master, which is not an error in Python — it just walked the buffer with the
  wrong stride and returned a short, meaningless image.
- `self_check` inflates each PNG it just wrote and asserts the scanline length is
  `height * (1 + width * 4)`, so a short buffer cannot be papered over by a
  decoder that is happy to read what is there.

The lesson worth keeping: a preview that *looks* like the artwork is not evidence
that the mask is right. Both bugs survived an "it looks fine" inspection, and both
were caught by measuring the output — mid-edge widths and area fill — against the
system icons.

## Verified

Everything below was exercised against a real DSH install on macOS 26 (arm64):

| Part | Evidence |
|---|---|
| Builds | `cargo build` clean, 167 tests passing |
| Window | Hidden titlebar, inset traffic lights, drag strip |
| Renders DSH | Full web UI — sidebar, conversations, composer, cost meter |
| Host supervision | Spawns `dsh web --no-open`, parses its authenticated URL |
| Clean shutdown | The host process dies with the window; no orphans |
| **Clipboard** | ⌘V pasted `PASTE_TEST_12345`; ⌘A then ⌘C copied it back |
| **Hotkey** | Hidden → ⌘⇧D → summoned, session preserved |
| **Close to tray** | Closing hides; host survives |
| **Theme reload** | Background changed with the PID unchanged |
| **Boot screen** | Visible during startup, then transitions to the app |
| **Bundle** | `make-app.sh` output launches through `open` |
| **Finder launch** | Starts its host under a minimal PATH |
| **Single instance** | Second launch exited `0`, logged the handoff, and left exactly one host running |
| **Handoff raises** | Window minimized → second launch → un-minimized, not merely focused |
| **Window state** | Resized to 1000×640 at (300, 120), `kill -9`'d, relaunched at exactly that rectangle |
| **Dock reopen** | Window minimized → `open -a` (the reopen event) → `AXMinimized` `true` → `false` |
| **App icon** | All ten `.icns` sizes re-decoded through CoreGraphics: alpha corners `0`, mid-edge runs exactly 412/512 and 824/1024, area fill 0.6169 against the system icons' 0.6173 |
| **DSH upgrade path** | An ignored test stages `0.1.5-rc.2` from the live registry, applies it, runs the launcher at its final path, and rolls back; a sandboxed app copy wrote `update.json` from the real registry |
| **Upgrade rollback** | A fake live tree whose launcher exits non-zero: the log shows `the upgraded DSH would not start; rolling back`, the previous tree is restored, and the note is cleared |
| **Notifications** | A banner appeared with the shell's title, body, and whale icon; the previous backend delivered nothing at all |

## Known gaps

- **macOS is the only platform that builds.** The bridge and the
  single-instance handoff are both Unix-socket based, so Windows needs a
  different transport before it can compile at all — the README previously
  claimed Windows built, which was wrong. Linux shares the Unix facilities and
  should build, but has never been run: neither target is installed here, so
  neither is checked. Only macOS has actually been exercised.
- **Shipped builds need a DSH install** unless built with `--bundle-runtime`,
  which is automated but unverified on a machine that has never had DSH. It is
  proven only against an empty `DSH_HOME` on a machine that already had bun.
- **Not notarized.** Local use only until a Developer ID is applied. A bundled
  runtime makes this harder, not easier: the bun binary and every native module
  under `node_modules` is another Mach-O that has to be signed.
- **No auto-update of the app itself.** The shell can upgrade the *DSH* it runs,
  but a new build of the shell still has to be installed by hand — a hand-built
  local bundle has no channel to update from.
- **An applied DSH upgrade needs a restart** to take effect, and the shell will
  not restart itself: where it hosts a DSH session, doing so ends that session.
  It says so in the settings window instead. A *bad* upgrade is still caught —
  the next launch rolls it back if no host starts.
- **Language changes need a restart** for the tray and the app menu, which are
  built once at startup. The settings window picks up a change when reopened.
- **An unsigned (ad-hoc) build cannot notify**, by design of the platform. See
  the signing section. `make-app.sh` warns when it falls back to ad-hoc.

### Debugging notifications

A note for whoever looks at this next, because it cost a long detour:

- **`defaults read com.apple.ncprefs` is not a reliable list of apps that can
  notify on current macOS.** Apps that had just shown the permission prompt —
  one of which had already delivered a notification — do not appear in it, even
  after `killall cfprefsd`. Treating its absence as "not registered" sends you
  chasing signatures and bundle identifiers that are not the problem.
- **The permission prompt is not instant.** It can arrive several seconds after
  launch, behind the app's own window. Concluding "no prompt appeared" too early
  is easy.
- Failure is logged with the `NSError` on the `warn` level, so read the log
  rather than guessing. See below for where it is.
- **A shell-script launcher breaks notifications.** With a wrapper as
  `CFBundleExecutable`, `usernotificationsd` reports *"Couldn't get record to
  check entitlement key"* and refuses the request, however the bundle is signed.
  The shipped app has no wrapper, so this only bites when testing.

## The log

```
$DSH_HOME/cache/dsh-shell/dsh-shell.log
```

The shell is a GUI app, so its stdout goes nowhere: launched from Finder there
is no terminal attached, and macOS does not route a plain process's stdout into
the unified log. Everything is therefore *also* written here, at `debug` rather
than the terminal's `info`, and the file is truncated on each launch so it holds
the run you are asking about rather than everything since.

`DSH_SHELL_LOG` moves it, `DSH_SHELL_LOG_LEVEL` changes the filter, and
`RUST_LOG` still governs stdout.

Two things it is worth reading it for, because both are otherwise invisible:

- **The host's own output.** The shell captures `dsh web`'s stdout and stderr
  and forwards it under the `dsh` target, so the bridge plugin's
  `[dsh-plugin-shell-bridge] connected to shell` lands in the file. Without that
  line there is no way to tell a plugin that is not loaded from one whose
  messages are not arriving.
- **The badge decision.** Every finished turn logs the inputs — whether AppKit
  says the app is active, whether the window is visible, and what the tracked
  focus flag thought. A badge that does not appear is otherwise unarguable.

### Debugging the badges

- **The Dock badge is governed by notification authorization, not by
  `NSDockTile`.** `UNAuthorizationOptions` carries `UNAuthorizationOptionBadge`
  as its own option (`UNUserNotificationCenter.h`), and macOS 13 added
  `setBadgeCount:withCompletionHandler:` (`API_AVAILABLE(macos(13.0))`). This
  app originally requested only `Alert | Sound`, so badge authorization was
  never granted and *`NSDockTile.badgeLabel` did nothing at all* — silently.
  The system reports the state, and the log prints it once per launch:

  ```
  INFO dsh_shell::native: notification authorization badge=UNNotificationSetting(0)
       alert=UNNotificationSetting(2) sound=UNNotificationSetting(2)
  ```

  `0` is not-supported, `2` is enabled. Badge was `0` throughout the whole
  investigation; alert and sound were already `2`, which is exactly why
  notifications and the menu-bar number worked while the Dock stayed plain.
  The shell now requests `Badge` too and sets the count through
  `setBadgeCount:`, which also reports a refusal on its completion handler —
  a refused badge is otherwise indistinguishable from one that never happened.
  macOS 11 and 12 keep the `NSDockTile` path, chosen at runtime with
  `respondsToSelector:`.
- **The tray number and the Dock badge are separate mechanisms.** The tray is
  plain AppKit text on a status item and needs no permission; the Dock is
  UserNotifications and does. One working while the other does not is the
  expected shape of a missing badge authorization, not a bug in either.
- **`tray-icon` 0.25 cannot clear a title with `None`.** Its macOS
  `set_title_inner` reads `if let Some(title) = title` and does nothing
  otherwise, so `set_title(None)` is a silent no-op: the number stays beside the
  icon after the count returns to zero. Pass `Some("")` instead. This is why the
  tray count did not disappear when the window came back in front, while the
  Dock badge did.
- **Do not infer "is the user looking at the app" from tao's `Focused` events.**
  The tracked flag is initialised to `true` — an assumption that the window was
  focused when it opened — and only moves when a transition is actually
  delivered. A LaunchServices-launched instance was observed logging
  `tracked_focus=true active=false` while it sat behind another app, which is
  exactly the state in which the badge must appear. It is now asked of AppKit
  directly (`NSApplication.isActive`), which has no state to go stale.
- **A process with no app bundle aborts when it touches
  `UNUserNotificationCenter`.** Not an error — an ObjC exception, which Rust
  cannot catch. `prepare_notifications` and `notify_macos` both check for a
  bundle first, which is what makes `cargo run` usable at all.

### Two false leads, for whoever is next

Neither of these was the cause, and both cost real time. They are recorded so
they can be dismissed quickly rather than re-investigated.

- **Duplicate LaunchServices registrations for one bundle id.** Building into
  `dist/`, copying the bundle around, and launching those copies registers the
  same `CFBundleIdentifier` against several paths — 7 in one case, including
  deleted and Trash-ed copies. That is genuinely worth cleaning
  (`lsregister -u <path>`, then `lsregister -f` the installed app; `-kill` no
  longer exists on macOS 26), but badges still did not appear afterwards.
  `make-app.sh` now unregisters its staging copy after installing, so the
  hazard does not rebuild itself.
- **Code signing.** Team-signed and ad-hoc builds were compared side by side
  with two identical minimal apps and two copies of this one: both signings
  badge. The signature is not a factor here.

### A note on verifying any of this

**`screencapture -R` returned frozen frames here.** A region capture was taken,
an app was launched and quit so that its Dock icon came and went, and a second
region capture of the same rectangle was byte-identical in every pixel. Every
conclusion drawn from diffing region captures in that window was worthless, and
several were drawn. Full-screen captures (`screencapture -x`) stayed live, and
cropping them with `sips -c <h> <w> --cropOffset <top> <left>` is the method
that held up. Confirm the tool before trusting the measurement — the same
lesson as `defaults read com.apple.ncprefs` above.

The other half of that lesson: a 2400-pixel-wide preview read by eye will
happily show you a badge that is not there. Verify pixel-level claims with
something that measures pixels.

## Licence

MIT. The DeepSeek icon (`assets/deepseek.svg`) comes from the official
`@deepseek-ai/dsh-web-frontend` package and remains under its own terms.

`assets/app-icon.png` — the artwork the app icon is built from — is **not** mine
and is not covered by the MIT grant. It is a crop of someone else's illustration,
kept in the repository only as the icon's source so `scripts/make-icon.py` can be
re-run; drop in your own 1024×1024 PNG and regenerate if you fork this.
