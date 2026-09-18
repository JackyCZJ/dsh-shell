# Auto-upgrading the DSH that the shell runs

How `dsh-shell` keeps the DSH it launches up to date — the design, the
measurements it rests on, and what is deliberately not automatic. Every number
below was produced on this machine, not recalled.

**Status: implemented.** `src/updater.rs` plus the Updates section of the
settings window. Detection runs once per launch and on demand; installing is
always a click. The four phases in "Phases" describe what shipped and what did
not.

## What "the DSH version" actually is

The shell does not contain DSH. `server.rs::resolve_launcher` finds a launcher,
in order:

1. `$DSH_BIN`, if set;
2. `Contents/Resources/bin/dsh` — the bundled runtime from `--bundle-runtime`;
3. well-known locations, of which `~/.bun/bin/dsh` is the one in use here.

**The installed app has no bundled runtime** — `Contents/Resources/` contains
only `deepseek.icns` — so the shell runs whatever `~/.bun/bin/dsh` resolves to,
which is a symlink into bun's global tree:

```
~/.bun/bin/dsh -> ../install/global/node_modules/@deepseek-ai/dsh/lib/bin.js
```

So "upgrading DSH" means replacing packages under
`~/.bun/install/global/node_modules/@deepseek-ai/`. The shell owns that
knowledge; nothing else in DSH has a reason to.

## Measurements

| Question | Answer |
|---|---|
| Is a version probe cheap? | `dsh --version` → `0.1.5-rc.2`, **58 ms** |
| How is an update discovered? | `GET https://registry.npmjs.org/@deepseek-ai%2Fdsh` → `dist-tags` (`latest`, `next`, `alpha`), per-version tarball + `integrity` |
| Fresh resolve consistent? | `bun install` in an empty dir with `^0.1.5-rc.1`: **231/231 packages at rc.2** |
| Does `bun install -g @…@0.1.5-rc.2` do the same? | **No.** It left **23 packages at rc.1** and 208 at rc.2 — a mixed tree, with bun printing `incorrect peer dependency "@deepseek-ai/dsh-settings@0.1.5-rc.1"` |
| Staging cost | install **8.2 s**, **279 MB**, after which `bun run <staged>/…/bin.js --version` answers correctly in **80 ms** |
| Relevant bun 1.4 flags | `--cwd=<val>` (stage anywhere), `--dry-run` |

Two conclusions fall out of this, and they are the whole design:

1. **In-place `bun install -g` must not be the mechanism.** It reuses the global
   lockfile and leaves a mixed-version tree, which is exactly the state this
   machine is in right now. The upgrade has to resolve fresh and then replace.
2. **A staged install is cheap and verifiable.** 279 MB and 8 seconds buys a tree
   that can be smoke-tested *before* it is swapped in, so a bad release or a
   partial download cannot break a working shell.

## A version range is not a version

The global `package.json` asks for `^0.1.5-rc.1`. In semver's ordering
`0.1.6-alpha.2 > 0.1.5-rc.2`, so that range is satisfied by an alpha. Any
mechanism that says "upgrade within the range" can silently move a user onto the
`alpha` channel. The upgrade path must therefore name an explicit target version
chosen from a dist-tag, never a range.

## Design

### Detection (safe, do this first)

- On launch, and at most once per `update.check_interval` (default 24 h), fetch
  the registry document and read `dist-tags[channel]`.
- Compare against `dsh --version` with a small semver comparison, including
  prerelease ordering — the shell cannot shell out to a semver library it does
  not ship, so this is ~60 lines and needs unit tests for prerelease ordering
  (`rc.2 > rc.1`, `rc.1 > alpha.2`, `1.0.0 > 1.0.0-rc.1`).
- Cache the result in `$DSH_HOME/cache/dsh-shell/update.json` so a flaky network
  never blocks a launch, and so the UI has something to show offline.
- Respect `update.channel`: `latest` (default) or `alpha`. Following `alpha`
  should be opt-in and visibly marked; the default must be `latest`.

Detection is *displayed*, never applied on its own.

### Application (explicit, staged, reversible)

```
stage:    mkdir $DSH_HOME/cache/dsh-shell/upgrade/<version>
          write a package.json pinning @deepseek-ai/dsh@<exact version>
          bun install --cwd <stage> --production        # fresh resolve, no lockfile reuse
verify:   bun run <stage>/node_modules/@deepseek-ai/dsh/lib/bin.js --version
          assert it equals <version>
apply:    mv global/@deepseek-ai            -> upgrade/backup-<old version>/
          mv <stage>/node_modules/@deepseek-ai -> global/@deepseek-ai
          re-point ~/.bun/bin/dsh -> ../install/global/node_modules/@deepseek-ai/dsh/lib/bin.js
verify:   dsh --version == <version>
          restart the host; if it fails to come up, restore backup-<old>/ and say so
cleanup:  drop the backup once the new version has served one successful launch
```

Notes that matter:

- The swap keeps `@deepseek-ai/dsh` at the same relative path, so the existing
  `~/.bun/bin/dsh` symlink keeps resolving. Re-pointing it is belt-and-braces.
- The old tree is moved aside rather than deleted, which is what makes rollback
  a rename instead of a 279 MB re-download. Keep one generation.
- Only apply when no turn is running. An upgrade kills the host, and killing it
  mid-turn is worse than being one release behind.
- `bun` is resolved the same way `dsh` is: prefer the sibling of the launcher
  (`dirname $(readlink -f ~/.bun/bin/dsh)/../../../..`), then `$PATH`. If no bun
  and no npm is found, the feature reports "cannot upgrade automatically" rather
  than failing at the last step.

### Where the code lives, and why

The *mechanism* belongs in the shell; the *UI* belongs in DSH. That split is
deliberate rather than a compromise:

- The upgrade path must not depend on the thing it is repairing. If a DSH release
  breaks the profile — and the bridge plugin is already version sensitive — then
  the shell, a separate signed binary that always starts, is precisely the
  component that can still fix it. So `src/updater.rs` stays in Rust.
- A settings window the shell draws itself is a second UI in an app that already
  has one, and it showed: the update section sat in a window that looked nothing
  like DSH's own settings. So the DSH-facing UI is a client plugin, rendered
  inside DSH's General settings.

Both exist. The shell window keeps its Updates section as the fallback — it is
the one that still works when DSH will not start — and the plugin row is the one
a user actually sees.

### How the plugin half talks to the shell

The bridge plugin is plain JavaScript, and DSH's `ctx.remote.<service>` surface is
generated from TypeScript decorators into a `typert.remote-client` module. A
plugin with no build step therefore **cannot** expose a remote service, which is
why the row does not call `ctx.remote.desktopShell`.

It uses an HTTP route on `ctx.webServer` instead:

```
GET  /dsh-shell-update/status    the last published status
POST /dsh-shell-update/check     forward a check to the shell, return the result
POST /dsh-shell-update/install   forward an install request
```

Three properties make that the right seam:

- It is same-origin with the page, so the browser half is a plain `fetch`.
- Route matching is longest-prefix, so the route wins over the authenticated
  fallback that serves the app, without needing a session token.
- The shell's socket reply carries only `ok` and an error string. Rather than
  widen that protocol for one caller, the shell *publishes* its status to
  `$DSH_HOME/cache/dsh-shell/status.json` and the plugin reads it. That file is
  an output and the registry cache is an input, and they are separate paths
  because sharing one silently destroyed the cache on every publish.

The client bundle is hand-written (`lib/client.js`), which works because DSH
resolves a package's `./client` export directly: there is no build pipeline to
miss for a `link:`-installed plugin.

### Verifying the two halves separately

`curl` covers the route, and is worth doing before touching any UI:

```sh
curl -s localhost:PORT/dsh-shell-update/status
curl -s -X POST localhost:PORT/dsh-shell-update/check
```

The client bundle is checked by executing it against a fake module loader and a
fake slot registry (`test/client.test.mjs`). That catches a bundle which never
registers, or registers under the wrong slot — whose only symptom otherwise is a
settings page that silently shows nothing.

### The configuration moved too

The update row was the first thing to move, but the same argument applied to the
whole shell configuration, so the plugin also renders the appearance and shortcut
form: the summon shortcut, both palettes, the custom CSS, and the upgrade
channel. The shell's own settings window keeps all of it as the fallback.

Two endpoints carry it, on the same route:

```
GET  /dsh-shell-update/config    the resolved `dsh-shell` section
POST /dsh-shell-update/config    a patch, merged
```

The write goes through `settings.update` — the same call the shell's own
`setConfig` makes — so there is one writer and one validator rather than a second
path that could disagree.

Three things about this are worth keeping:

- **Only changed fields are sent.** The write is a merge, so resending an
  untouched field is harmless; sending one the shell did not resolve is not. An
  older document has no `updateChannel`, and a form that helpfully supplied a
  default would overwrite the value the shell is actually using.
- **The schema needed the key added.** `updateChannel` existed on the shell's
  `Theme` but not in the plugin's `dsh-shell` schema, so a write of it was
  dropped and the shell's copy silently reverted to the default.
- **The form validates what the schema does not.** The schema says `hotkey` is a
  string, so `"D"` is accepted and then fails on the shell side of the socket,
  where it falls back to the default with no error anywhere. The browser refuses
  a shortcut with no modifier, and a colour that is not six-digit hex, so the
  user sees why.

### What the user sees

- DSH's own General settings: the appearance/shortcut form, then the installed
  DSH version, the channel, a "Check Now" button, and an "Upgrade to X" button
  when one exists.
- The shell's settings window: the same, as the fallback when DSH will not start.
- The tray menu: "Check for DSH Updates…" plus a row showing the version or the
  available update.
- Never a silent install. This project's own release flow publishes release
  candidates, so an unattended apply could move someone onto a broken prerelease
  overnight.

## Phases

| Phase | Scope | State |
|---|---|---|
| 1 | Detection + settings/tray display, cached, with semver tests | **Shipped** — once per launch (cached, so normally a file read) and on demand |
| 2 | Staged install + verify, behind an explicit button, with backup | **Shipped** — `stage()` resolves fresh, smoke-tests the result, keeps the old tree aside |
| 3 | `apply` + rollback on a failed launch | **Shipped** — `apply()`, then a pending note; the next launch that cannot start a host restores the previous tree and retries once |
| 4 | Unattended apply on launch | **Not shipped, deliberately.** Nothing installs itself |

Phase 3 needed the failure test it was specified with, and it is worth
describing because the mechanism is not obvious: `apply` runs while the old
version is still live in memory, so it *cannot* know whether the new tree boots.
`apply` therefore writes a note naming the target (`pending.json`, beside the
backup it would restore), and the next launch treats a failure to start a host as
a verdict on that upgrade: restore the previous tree, clear the note, try once
more — once, so a failure unrelated to the upgrade still reports its own error
instead of cycling. A successful start clears the note.

Verified by running the real shell against a fake install whose live launcher
exits non-zero: the log shows `the upgraded DSH would not start; rolling back`,
the live tree comes back as the previous version, the note is gone, and the bad
tree is parked in `dsh-shell-staging/failed-at`.

## Also verified

- **The swap itself, for real**: an `--ignored` test stages `0.1.5-rc.2` from the
  live registry (279 MB), asserts the staged scope has the third-party packages
  (`cordis` et al.) as well as the DSH ones, applies it, runs the launcher at its
  final path, and rolls back. `cargo test -- --ignored`.
- **The registry check**: a sandboxed copy of the app, pointed at a fake install
  that reports `0.1.5-alpha.1`, wrote `update.json` with
  `latest: 0.1.5-rc.2, alpha: 0.1.6-alpha.2`.

## Two bugs this found, both of which would have shipped

Recorded because neither was visible from reading the code, and both failed in
ways that looked like something else:

1. **Polling `try_wait` with a piped stdout deadlocks.** The registry document is
   142 KB; the child blocks writing once the 64 KB pipe buffer fills, so it never
   exits and every check "timed out". The network was fine. `run_with_timeout`
   now drains stdout on its own thread.
2. **Resolving the launcher's symlink breaks the process you spawn.** The shell
   runs `~/.bun/bin/dsh`, a symlink to a `lib/bin.js` that is *not* executable.
   Canonicalising it produced a path that cannot be spawned — the shell would
   have tried to execute JavaScript as a program. `Install` now keeps the
   launcher to run (the symlink) and the resolved path to read the layout from.

## Open questions

- **The shell's own updates.** This covers the DSH version only. The app bundle
  itself still has no update path, and a hand-built local bundle has no channel
  to update from.
- **Applying without a restart.** We run the old DSH in memory while the new one
  is on disk, so the last step is a restart. Doing better means restarting the
  host in place, which on this machine would kill whatever session the shell is
  hosting — so it is left to the user, who is told to restart.
- **Linux/Windows.** The staged swap is plain filesystem work and portable; the
  launcher resolution and `~/.bun/bin/dsh` symlink are not.
- **`--bundle-runtime` builds.** Those carry their own DSH under
  `Contents/Resources/runtime/` and would stage into the bundle, which then
  invalidates its signature. `Install::discover` refuses that layout — the scope
  is not `node_modules/@deepseek-ai` — so such a build reports "cannot upgrade
  itself" rather than corrupting its own signature. Re-signing and supporting it
  is undecided.
