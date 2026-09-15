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
| **Tray icon** | Agent state (idle / working / failed) as a colour-coded whale |
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
| `agent/turn-stopping` | Tray → idle, notification |
| `agent/request-error` | Tray → failed, notification with the message |

The shell works identically without the plugin: the bridge is optional by design,
and the plugin cannot break the host.

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
  deepseek.svg    DSH's official icon
  deepseek.icns   built from the SVG for the bundle
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

```sh
codesign --force --deep --options runtime \
  --sign "Developer ID Application: <name> (<team>)" "DSH Shell.app"
xcrun notarytool submit "DSH Shell.app.zip" --keychain-profile <profile> --wait
xcrun stapler staple "DSH Shell.app"
```

- Notarization requires a paid Developer ID and the hardened runtime.
- Add the `com.apple.security.network.client` entitlement — the webview connects
  to the loopback host.
- Re-sign after changing anything inside the bundle, or
  the signature is invalidated.

## Development

```sh
cargo run          # run from source
cargo test         # 40 tests
cargo clippy       # lints
./scripts/make-icon.sh   # regenerate the .icns from the SVG
```

The tests cover the parts that are easy to get subtly wrong: URL parsing, CSS
injection and escaping, the appearance preference parser, theme reload semantics,
SVG path flattening, and the menu structure that makes the clipboard work.

## Verified

Everything below was exercised against a real DSH install on macOS 26 (arm64):

| Part | Evidence |
|---|---|
| Builds | `cargo build` clean, zero warnings, 112 tests passing |
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
- **No auto-update.** A new build has to be installed by hand.
- **Language changes need a restart** for the tray and the app menu, which are
  built once at startup. The settings window picks up a change when reopened.

## Licence

MIT. The DeepSeek icon (`assets/deepseek.svg`) comes from the official
`@deepseek-ai/dsh-web-frontend` package and remains under its own terms.
