# dsh-plugin-shell-bridge

A DeepSeek Harness **Host plugin** that forwards agent events to the native
`dsh-shell` desktop shell.

This is what makes the shell a participant in DSH's plugin ecosystem rather than
an external window: it is installed with `dsh plugin`, its lifecycle is owned by
the Loader, and uninstalling it leaves the host untouched.

```
┌─────────────────────────────┐
│ dsh Host (Node)             │
│  └─ cordis row: shell-bridge│──┐  newline-delimited JSON
└─────────────────────────────┘  │  over a Unix socket
                                 ▼
                    ┌────────────────────────┐
                    │ dsh-shell (Rust)       │
                    │  tray · notifications  │
                    └────────────────────────┘
```

## Install

```sh
dsh plugin --profile web add /path/to/dsh-plugin-shell-bridge
```

The plugin registers one Loader row through `cordis.patch.yml`. Confirm it is in
the composed tree:

```sh
dsh --profile web --dump-config | grep shell-bridge
```

## Events forwarded

| Event | Shell behaviour |
|---|---|
| `agent/created` | tray → idle |
| `agent/status` | tray reflects `working` / `idle` / `failed` |
| `agent/turn-stopping` | tray → idle, desktop notification |
| `agent/request-error` | tray → failed, desktop notification with the message |
| `agent/disposed` | recorded in the shell log |

Notifications are deliberately limited to the two events a user wants to be
interrupted by. Notifying on every status change would be noise.

## Transport

A Unix domain socket at `$XDG_RUNTIME_DIR/dsh-shell-<uid>.sock`, or `$TMPDIR` when
`XDG_RUNTIME_DIR` is unset. Both sides resolve this identically; `DSH_SHELL_SOCKET`
overrides it.

A socket is used rather than stdout because stdout carries the host's own
protocol traffic in some profiles, and interleaving would corrupt it.

## Design constraints

1. **Never break the host.** DSH must boot and behave identically whether the
   shell is running, absent, or crashing. Every side effect is best-effort and
   swallows its own errors; listener bodies are individually wrapped so a bug
   here can never fail an agent turn.
2. **No reliance on internal packages.** Only Node built-ins and the public `ctx`
   API are used, matching how other third-party plugins in this ecosystem work.
   That keeps the plugin working across DSH releases.
3. **Bounded buffering.** If the shell is not reading, events are dropped rather
   than queued without limit. A stalled reader must not grow the host's memory.
4. **Minimal payloads.** Events are trimmed to what the shell renders. It has no
   business holding full prompts or model output.

## Verified

- `apply()` runs in the real host and the plugin connects to the shell socket
  (**7 socket FDs held by the host process**).
- Events sent over the socket are received and processed by the shell.
- The host boots identically when the shell is not running.

## Debugging

The plugin logs to **stdout of the host process**, which for a shell-launched
host is not captured in the shell's own log file. To watch it, run the host
standalone and read its output:

```sh
dsh web --no-open --port 0
```

Look for:

```
[dsh-plugin-shell-bridge] apply() called; socket=...
[dsh-plugin-shell-bridge] connected to shell
```

`ctx.logger` is not guaranteed to implement `info`, so this plugin uses
`console.log` for its traces — an earlier version logged through `ctx.logger` and
the messages were silently dropped, which made a working plugin look absent.
