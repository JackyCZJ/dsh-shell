/**
 * dsh-plugin-shell-bridge — Host plugin forwarding DSH agent events to the
 * native desktop shell.
 *
 * Design constraints, in priority order:
 *
 *  1. **Never break the host.** The shell is optional: DSH must boot and run
 *     identically whether the shell is running, absent, or crashing. Every
 *     side effect here is best-effort and swallows its own errors.
 *
 *  2. **No hard dependency on internal packages.** This plugin imports only
 *     Node built-ins and the public `ctx` API, the way third-party plugins in
 *     this ecosystem do. That keeps it working across DSH releases.
 *
 *  3. **Bounded buffering.** If the shell is not reading, events are dropped
 *     rather than queued without limit — a stalled reader must never grow the
 *     host's memory.
 *
 * Transport: a newline-delimited JSON stream over a Unix domain socket. A
 * socket is used rather than stdout because stdout belongs to the host's own
 * protocol traffic in some profiles, and mixing the two would corrupt it.
 */

import net from 'node:net'
import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'

export const name = 'shell-bridge'

/** Reconnection backoff, in milliseconds. */
const RECONNECT_MIN_MS = 500
const RECONNECT_MAX_MS = 10_000

/** Cap on queued events while the shell is disconnected. */
const MAX_QUEUE = 256

/**
 * Resolve the socket path the shell listens on.
 *
 * Kept short: macOS caps Unix socket paths at ~104 bytes, and a deep home
 * directory plus a long file name can exceed that.
 */
export function socketPath(env = process.env) {
  if (env.DSH_SHELL_SOCKET) return env.DSH_SHELL_SOCKET
  const runDir = env.XDG_RUNTIME_DIR || os.tmpdir()
  return path.join(runDir, `dsh-shell-${process.getuid?.() ?? 0}.sock`)
}

/**
 * Strip a session event down to what the shell needs.
 *
 * The shell renders notifications and a tray state; it has no business holding
 * full prompts or model output. Trimming here also keeps the socket cheap.
 */
function summarize(kind, payload) {
  switch (kind) {
    case 'status':
      return { kind, status: payload?.status ?? payload?.state ?? 'unknown' }
    case 'turn-stopping':
      return { kind, sessionId: payload?.sessionId, reason: payload?.reason }
    case 'request-error':
      return {
        kind,
        sessionId: payload?.sessionId,
        // Errors are the one place free text is genuinely useful to a user.
        message: truncate(payload?.error?.message ?? payload?.message, 300),
      }
    case 'created':
      return { kind, sessionId: payload?.sessionId }
    case 'disposed':
      return { kind, sessionId: payload?.sessionId }
    default:
      return { kind }
  }
}

function truncate(text, limit) {
  if (typeof text !== 'string') return undefined
  const flat = text.replace(/\s+/g, ' ').trim()
  return flat.length <= limit ? flat : `${flat.slice(0, limit - 1)}…`
}

/**
 * A resilient, non-blocking connection to the shell.
 *
 * Owns reconnection, the bounded queue, and every failure path. Callers only
 * ever call `send`, which cannot throw.
 */
class ShellLink {
  constructor(log, socketPath) {
    this.log = log
    this.socketPath = socketPath
    this.socket = null
    this.queue = []
    this.backoff = RECONNECT_MIN_MS
    this.stopped = false
    this.connect()
  }

  connect() {
    if (this.stopped) return

    const socket = net.createConnection(this.socketPath)
    socket.setNoDelay(true)

    socket.on('connect', () => {
      this.socket = socket
      this.backoff = RECONNECT_MIN_MS
      console.log('[dsh-plugin-shell-bridge] connected to shell')
      this.flush()
    })

    socket.on('error', (error) => {
      // Expected whenever the shell is not running, so log once per distinct
      // reason rather than on every retry.
      if (this.lastError !== error.code) {
        this.lastError = error.code
        console.log(
          `[dsh-plugin-shell-bridge] shell socket unavailable (${error.code ?? error.message}); will retry`,
        )
      }
      this.drop()
    })

    socket.on('close', () => {
      this.drop()
      this.scheduleReconnect()
    })
  }

  drop() {
    if (this.socket) {
      this.socket.removeAllListeners()
      this.socket.destroy()
      this.socket = null
    }
  }

  scheduleReconnect() {
    if (this.stopped) return
    const delay = this.backoff;
    this.backoff = Math.min(this.backoff * 2, RECONNECT_MAX_MS)
    const timer = setTimeout(() => this.connect(), delay)
    // Do not hold the process open for a reconnection attempt.
    timer.unref?.()
  }

  flush() {
    if (!this.socket) return
    while (this.queue.length > 0 && this.socket.writable) {
      const line = this.queue.shift()
      this.socket.write(line)
    }
  }

  /** Queue one event. Never throws, never blocks. */
  send(event) {
    if (this.stopped) return
    let line
    try {
      line = `${JSON.stringify(event)}\n`
    } catch {
      return // An unserializable payload must not take the host down.
    }

    if (this.socket?.writable) {
      this.socket.write(line)
      return
    }

    // Drop the oldest rather than growing without bound: a stale status update
    // is worth less than the host's memory.
    if (this.queue.length >= MAX_QUEUE) this.queue.shift()
    this.queue.push(line)
  }

  dispose() {
    this.stopped = true
    this.drop()
    this.queue.length = 0
  }
}

/**
 * Cordis plugin entry point.
 *
 * @param {import('@deepseek-ai/cordis').Context} ctx
 */
export function apply(ctx) {
  const log = ctx.logger ?? console
  const startedAt = Date.now()
  console.log(`[dsh-plugin-shell-bridge] apply() called; socket=${socketPath()}`)
  const link = new ShellLink(log, socketPath())

  ctx.on('dispose', () => link.dispose())

  // Agent lifecycle and status drive the tray icon.
  for (const [event, kind] of [
    ['agent/created', 'created'],
    ['agent/disposed', 'disposed'],
    ['agent/status', 'status'],
    ['agent/turn-stopping', 'turn-stopping'],
    ['agent/request-error', 'request-error'],
  ]) {
    // Listener failures are isolated here rather than relying on the host to
    // catch them, so a bug in this plugin can never fail an agent turn.
    ctx.on(event, (payload) => {
      try {
        link.send({ ...summarize(kind, payload), at: Date.now() })
      } catch (error) {
        log.debug?.(`[shell-bridge] dropped ${kind}: ${error?.message ?? error}`)
      }
    })
  }
}
