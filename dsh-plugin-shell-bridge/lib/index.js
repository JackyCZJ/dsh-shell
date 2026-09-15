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
 * The service name this plugin publishes.
 *
 * **Must not collide with a DSH core service.** Publishing under a name DSH
 * already owns silently replaces that service for the whole process: an earlier
 * version used `shell`, which is DSH's bash executor, and the overwrite made
 * every profile fail to boot with
 * "the mounted bash executor does not confine (no sandboxMode)".
 *
 * `desktopShell` follows the convention the official desktop app uses for its
 * own capabilities (`desktopProfiles`, `desktopPnpm`).
 */
export const SERVICE_NAME = 'desktopShell'

/**
 * Service names owned by DSH.
 *
 * Checked before publishing so a future rename cannot clobber core
 * functionality again. This list is the set of names DSH plugins register via
 * `super(ctx, "...")`; it is a safety net, not an API.
 */
export const RESERVED_SERVICE_NAMES = new Set([
  'agentDefaultModel', 'agentLoop', 'agentPresets', 'agents', 'approval',
  'attachments', 'authorization', 'clientModules', 'codeRuntime', 'commands',
  'compaction', 'connection', 'cordisInspect', 'credentials',
  'deepseekLlmApiExtensions', 'directoryPicker', 'dynamicCordisRunner',
  'fileReferences', 'fileUploads', 'fs', 'goals', 'hmr', 'invariants', 'jobs',
  'llm', 'messageFeedback', 'permissionPresets', 'planMode', 'pluginInventory',
  'sandbox', 'sandboxPolicy', 'sessionFeedback', 'sessionPersistence',
  'sessionProjectionCache', 'sessionProjections', 'sessionQuery',
  'sessionReferenceResolver', 'sessions', 'sessionTelemetry', 'sessionTitle',
  'settings', 'shell', 'shellEnv', 'skills', 'spillStore', 'storage',
  'subagents', 'subprocess', 'systemPrompt', 'terminals', 'timer', 'tokenMeter',
  'toolResultPruner', 'tools', 'typert', 'typertGateway', 'userQuestions', 'web',
  'webhookRuntime', 'webServer', 'workflowEngine', 'workspaceFiles',
  'workspaceRegistry',
])

/**
 * Requests the shell can be asked to perform.
 *
 * Kept as constants so a typo fails here rather than on the wire.
 */
export const ShellMethod = {
  Notify: 'notify',
  FocusWindow: 'focusWindow',
  HideWindow: 'hideWindow',
  SetStatusLabel: 'setStatusLabel',
}

/**
 * A resilient, non-blocking connection to the shell.
 *
 * Owns reconnection, the bounded queue, and every failure path. Callers only
 * ever call `send`, which cannot throw.
 */
class ShellLink {
  /** Monotonic id source, so replies can be matched to requests. */
  static nextId = 0

  constructor(log, socketPath) {
    this.log = log
    this.socketPath = socketPath
    this.socket = null
    this.queue = []
    this.backoff = RECONNECT_MIN_MS
    this.stopped = false
    /** In-flight requests, keyed by id, awaiting the shell's reply. */
    this.pending = new Map()
    this.buffer = ''
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

    // Replies arrive on the same socket. Lines are buffered because a single
    // TCP read can carry a partial line or several at once.
    socket.on('data', (chunk) => {
      this.buffer += chunk.toString('utf8')
      let index
      while ((index = this.buffer.indexOf('\n')) >= 0) {
        const line = this.buffer.slice(0, index)
        this.buffer = this.buffer.slice(index + 1)
        if (!line.trim()) continue
        try {
          this.settle(JSON.parse(line))
        } catch {
          // A non-JSON line is not a reply; ignore it rather than crashing.
        }
      }
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

  /** Fail every in-flight request, e.g. when the connection drops. */
  failPending(reason) {
    for (const [, waiter] of this.pending) {
      clearTimeout(waiter.timer)
      waiter.reject(new Error(reason))
    }
    this.pending.clear()
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

  /**
   * Ask the shell to do something, resolving with its reply.
   *
   * Resolves to `{ ok, error }` rather than rejecting on a shell-side failure,
   * so a caller can branch without a try/catch. Rejects only on timeout.
   */
  request(method, params = {}, { timeoutMs = 5000 } = {}) {
    const id = `req-${++ShellLink.nextId}`
    return new Promise((resolve, reject) => {
      if (this.stopped) {
        reject(new Error('shell link is closed'))
        return
      }
      const timer = setTimeout(() => {
        this.pending.delete(id)
        reject(new Error(`shell did not reply to ${method} within ${timeoutMs}ms`))
      }, timeoutMs)
      // Do not hold the process open waiting for a reply.
      timer.unref?.()

      this.pending.set(id, { resolve, reject, timer, method })
      this.send({ id, method, ...params })
    })
  }

  /** Route one reply line to its waiting caller, if any. */
  settle(reply) {
    const waiter = this.pending.get(reply?.id)
    if (!waiter) return
    this.pending.delete(reply.id)
    clearTimeout(waiter.timer)
    waiter.resolve({ ok: reply.ok === true, error: reply.error })
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
    this.failPending('shell link disposed')
    this.drop()
    this.queue.length = 0
  }
}

/**
 * Cordis plugin entry point.
 *
 * @param {import('@deepseek-ai/cordis').Context} ctx
 */
/**
 * The service other plugins consume.
 *
 * Published on `ctx.shell` so a plugin declares a dependency rather than
 * discovering the socket path itself. Every method resolves to
 * `{ ok, error }` — a shell-side failure is a value, not an exception, because
 * the shell is optional and its absence must not break the caller.
 */
function createShellService(link) {
  const call = (method, params) => {
    // Absence of a shell is known synchronously, so do not make the caller wait
    // out the request timeout to learn it.
    if (!link.socket) {
      return Promise.resolve({ ok: false, error: 'no shell connected' })
    }
    return link.request(method, params).catch((error) => ({
      ok: false,
      error: error.message,
    }))
  }

  return {
    /** Whether a shell is currently connected. */
    get connected() {
      return link.socket != null
    },
    /** Post a desktop notification. */
    notify: (body, title) => call(ShellMethod.Notify, { body, title }),
    /** Bring the shell window to the front. */
    focusWindow: () => call(ShellMethod.FocusWindow),
    /** Hide the shell window to the tray. */
    hideWindow: () => call(ShellMethod.HideWindow),
    /** Set or clear the short label shown beside the agent state. */
    setStatusLabel: (text) => call(ShellMethod.SetStatusLabel, { text }),
  }
}

export function apply(ctx) {
  const log = ctx.logger ?? console
  const startedAt = Date.now()
  console.log(`[dsh-plugin-shell-bridge] apply() called; socket=${socketPath()}`)
  const link = new ShellLink(log, socketPath())

  ctx.on('dispose', () => link.dispose())

  // Refuse to start rather than clobber a service DSH owns. Failing loudly here
  // is far better than the silent corruption a collision causes: the whole
  // profile stops booting, and the error surfaces far from the real cause.
  if (RESERVED_SERVICE_NAMES.has(SERVICE_NAME)) {
    throw new Error(
      `dsh-plugin-shell-bridge: refusing to publish "${SERVICE_NAME}" because ` +
        `DSH registers a service with that name; publishing would replace it ` +
        `process-wide. Rename SERVICE_NAME.`,
    )
  }

  // Publish the shell service. Consumers declare `inject: [SERVICE_NAME]` and
  // then call `ctx.desktopShell.*`.
  ctx.provide(SERVICE_NAME, createShellService(link))

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
