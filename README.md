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
| **Live theme reload** | Edit `theme.json` and the chrome *and* the page update, no restart |
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
| `DSH_THEME` | `./theme.json` | Theme file to load and watch |
| `RUST_LOG` | `info` | Log filter |

## Theming, live

`theme.json` carries a light and a dark palette. Both are taken from **DSH's own
boot theme CSS**, so the window chrome and the page share one colour and the seam
between them disappears.

```json
{
  "appearance": "system",
  "light": { "background": "#ffffff", "text": "#0f1115" },
  "dark":  { "background": "#151517", "text": "#f9fafb" }
}
```

Appearance resolves in this order:

1. **`appearance` in `theme.json`** — `light`, `dark`, or `system`.
2. **DSH's own `ui-theme.preference`** from `$DSH_HOME/settings.yaml`.
3. **The operating system**, when DSH is set to `system`.

Step 2 matters: DSH applies its preference to the page, so a shell that followed
the OS instead would render a dark caption bar over a light page whenever the two
disagreed. Resolution happens in one function, so the native window colour and the
injected CSS cannot drift apart.

Because the file is watched, edits apply to **both the native chrome and the page**
with no restart and no loss of session:

```sh
jq '.appearance="dark"' theme.json > t && mv t theme.json
```

Change `custom_css` for one-off tweaks — it is appended last, so it overrides the
generated rules:

```json
"custom_css": "body { --dsh-content-font-size: 15px; }"
```

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
  theme.rs   tokens, JSON load, watcher
assets/
  boot.html       the loading screen
  deepseek.svg    DSH's official icon
  deepseek.icns   built from the SVG for the bundle
theme.json        the hot-reloadable document
```

### Two threading rules

Both were learned the hard way and are worth knowing before extending this:

1. **Never block the winit event loop.** Tray, hotkey, bridge, and menu events
   arrive on their own threads and cannot wake it. With `ControlFlow::Wait` they
   sat unread until unrelated input ticked the loop, so notifications appeared
   only when the mouse moved. The loop polls every 100 ms.

2. **`wry::WebView` is `!Send`.** It wraps main-thread-only AppKit objects, so a
   background thread must never touch it. The theme watcher publishes over a
   channel and the event loop applies the change; the compiler enforces this.

## Packaging

### Distributing to machines without DSH

The app resolves a `dsh` launcher itself, so a machine without DSH cannot run it.
To ship something self-contained, add the runtime under `Contents/Resources/`:

| Component | Size |
|---|---|
| Node runtime (official darwin-arm64 tarball) | ~50 MB |
| `@deepseek-ai/dsh` packages | ~305 MB |
| `dsh-shell` binary | ~2.7 MB |

Place `dsh` at `Contents/Resources/bin/dsh`; the resolver picks it up with no user
configuration. The official Electron app avoids this weight by reusing Electron's
embedded Node, which a webview-based shell does not have.

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
- Re-sign after changing anything inside the bundle (including `theme.json`), or
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
| Builds | `cargo build` clean, zero warnings, 40 tests passing |
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

## Known gaps

- **Windows and Linux are untested.** The code compiles for them and uses the
  platform webview, but only macOS has been run. The app menu is attached
  app-globally on macOS only.
- **Shipped builds need a DSH install.** Bundling the runtime is documented above
  but not automated.
- **Not notarized.** Local use only until a Developer ID is applied.

## Licence

MIT. The DeepSeek icon (`assets/deepseek.svg`) comes from the official
`@deepseek-ai/dsh-web-frontend` package and remains under its own terms.
