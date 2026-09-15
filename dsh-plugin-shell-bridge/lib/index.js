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
 *
 * Configuration is owned by DSH, not by a file the shell reads itself: the
 * plugin registers a `dsh-shell` settings namespace and every write travels
 * through `ctx.settings`. That is the same arrangement the official desktop app
 * uses for its `dsh-desktop` namespace, and it means DSH validates and persists
 * the section while the rest of `settings.yaml` stays untouched.
 */

import net from 'node:net'
import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import { createRequire } from 'node:module'
import { pathToFileURL } from 'node:url'

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

/** Recursively freeze a plain value, so no consumer can mutate shared defaults. */
function deepFreeze(value) {
  if (value && typeof value === 'object' && !Object.isFrozen(value)) {
    Object.freeze(value)
    for (const child of Object.values(value)) deepFreeze(child)
  }
  return value
}

/** Whether a value is a plain data object, the only shape `setConfig` accepts. */
function isPlainObject(value) {
  if (typeof value !== 'object' || value === null || Array.isArray(value)) return false
  const proto = Object.getPrototypeOf(value)
  return proto === Object.prototype || proto === null
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
 *
 * `setConfig` is the one entry that flows the *other* way: the shell asks the
 * plugin to persist a settings write, rather than the plugin asking the shell
 * to act. It lives here so both directions share one spelling of the method.
 */
export const ShellMethod = {
  Notify: 'notify',
  FocusWindow: 'focusWindow',
  HideWindow: 'hideWindow',
  SetStatusLabel: 'setStatusLabel',
  SetConfig: 'setConfig',
}

/**
 * The settings namespace DSH owns for this shell.
 *
 * Mirrors the shell's own `theme::SETTINGS_NAMESPACE`; the two must agree or
 * the shell reads a section nobody writes.
 */
export const SETTINGS_NAMESPACE = 'dsh-shell'

/**
 * The default `dsh-shell` section, and the value used whenever the settings
 * service is absent.
 *
 * This is also the composition `base` passed to `installSection`: schema
 * defaults alone would suffice for resolution, but an explicit base documents
 * the intended starting point and is what the shell receives if settings ever
 * detaches. The numbers are the shell's own compiled defaults, so a hand-edited
 * or partial section still yields a usable window.
 */
export const SHELL_SETTINGS_DEFAULTS = deepFreeze({
  hotkey: 'meta+shift+D',
  light: {
    background: '#ffffff',
    surface: '#f5f6f7',
    surfaceHover: '#e9ebed',
    border: '#d9dcdf',
    text: '#0f1115',
    textMuted: '#81858c',
    accent: '#4176e6',
  },
  dark: {
    background: '#151517',
    surface: '#2c2c2e',
    surfaceHover: '#3a3a3c',
    border: '#3f3f42',
    text: '#f9fafb',
    textMuted: '#adb2b8',
    accent: '#4176e6',
  },
  customCss: '',
})

/**
 * Load the schemastery instance the host itself validates with.
 *
 * A third-party plugin installed by `link:` outside the profile cannot resolve
 * `@deepseek-ai/schemastery` with an ordinary `import`: the specifier resolves
 * from this file's real directory, which is not below the profile's
 * `node_modules`. `ctx.baseUrl` is the profile URL, so a `createRequire`
 * anchored there finds the installation's shared fallback — and therefore the
 * copy the settings service uses. The remaining anchors cover hosts that do not
 * expose `ctx.baseUrl` (a bare `node --test`, or a packaged launcher).
 *
 * Returns `undefined` rather than throwing: settings is optional, and a plugin
 * that cannot build its schema must still boot and serve the shell.
 *
 * @param {{ baseUrl?: unknown }} [ctx] - plugin context, for the profile anchor.
 * @returns {any | undefined} the schemastery namespace, or undefined when absent.
 */
export function loadSchemastery(ctx) {
  const anchors = []
  if (ctx?.baseUrl) anchors.push(String(ctx.baseUrl))
  const home = process.env.DSH_HOME || path.join(os.homedir(), '.dsh')
  // The probe file need not exist; only its directory drives resolution.
  anchors.push(pathToFileURL(path.join(home, 'profiles', 'shell-bridge-anchor.js')).href)
  try {
    anchors.push(pathToFileURL(fs.realpathSync(process.argv[1])).href)
  } catch {
    // Embedded runtimes have no useful argv[1]; this anchor is only a fallback.
  }
  anchors.push(import.meta.url)

  for (const anchor of anchors) {
    try {
      const loaded = createRequire(anchor)('@deepseek-ai/schemastery')
      const z = loaded?.default ?? loaded
      if (typeof z?.object === 'function') return z
    } catch {
      // Try the next anchor; an unresolvable schemastery is not fatal.
    }
  }
  return undefined
}

/**
 * Build the `dsh-shell` schema against the host's schemastery instance.
 *
 * Every field is defaulted so a partial or hand-written section resolves to a
 * complete document. Defaults are repeated in the schema (not only in the
 * composition base) because a configuration surface renders schema defaults,
 * and a fresh install must show sensible values there too.
 *
 * @param {any} z - the schemastery namespace.
 * @returns {any} the namespace schema.
 */
export function defineShellSettingsSchema(z) {
  const palette = () =>
    z.object({
      background: z.string().default(SHELL_SETTINGS_DEFAULTS.light.background),
      surface: z.string().default(SHELL_SETTINGS_DEFAULTS.light.surface),
      surfaceHover: z.string().default(SHELL_SETTINGS_DEFAULTS.light.surfaceHover),
      border: z.string().default(SHELL_SETTINGS_DEFAULTS.light.border),
      text: z.string().default(SHELL_SETTINGS_DEFAULTS.light.text),
      textMuted: z.string().default(SHELL_SETTINGS_DEFAULTS.light.textMuted),
      accent: z.string().default(SHELL_SETTINGS_DEFAULTS.light.accent),
    })

  return z.object({
    hotkey: z.string().default(SHELL_SETTINGS_DEFAULTS.hotkey),
    // Copies, not the frozen originals: a schema default is data the resolver
    // may hand back, and the shared constant must stay the single source.
    light: palette().default({ ...SHELL_SETTINGS_DEFAULTS.light }),
    dark: palette().default({ ...SHELL_SETTINGS_DEFAULTS.dark }),
    customCss: z.string().default(SHELL_SETTINGS_DEFAULTS.customCss),
  })
}

/**
 * Answer one request the shell made over the socket.
 *
 * Only `setConfig` exists here. The write goes through `ctx.settings` so DSH
 * performs schema validation and its own writer preserves every other
 * namespace in the document; this function never touches the file itself.
 *
 * Resolves to `{ ok, error }` rather than throwing, because the caller turns
 * the result straight into a wire reply.
 *
 * @param {any} ctx - plugin context, read lazily for the settings service.
 * @param {{ method?: string, config?: unknown }} request - the shell's request.
 * @returns {Promise<{ ok: boolean, error?: string }>} the reply body.
 */
export async function handleShellRequest(ctx, request) {
  if (request?.method !== ShellMethod.SetConfig) {
    return { ok: false, error: `unknown method: ${String(request?.method)}` }
  }

  // Read the service at call time: it may mount after `apply()` ran, and it may
  // unmount while the plugin keeps running.
  const settings = ctx?.get?.('settings')
  if (settings === undefined || settings === null) {
    return { ok: false, error: 'the DSH settings service is not mounted' }
  }
  if (!isPlainObject(request.config)) {
    return { ok: false, error: 'setConfig requires a config object' }
  }

  try {
    // Merge (not replace): the shell may send a partial section, and a merge is
    // also correct for a full one — every key it carries overwrites the user
    // layer, while keys it omits keep whatever the user already had.
    await settings.update(SETTINGS_NAMESPACE, request.config)
    return { ok: true }
  } catch (error) {
    // A validation failure is a normal outcome for a UI write, so report it as
    // the reply's error rather than an exception.
    return { ok: false, error: error?.message ?? String(error) }
  }
}

/**
 * Register the `dsh-shell` namespace and keep the shell's live copy current.
 *
 * `installSection` is the optional-service contract: while a settings provider
 * is mounted our namespace resolves from it, and when none is, `setSource`
 * hands back the composition defaults, so the shell is never left themeless.
 * The registration is deliberately best-effort — a schema mismatch in a stored
 * section fails registration, and the host must still boot.
 *
 * @param {any} ctx - plugin context; also the registration owner.
 * @param {any} settingsCtx - the context injected with `settings`.
 * @param {ShellLink} link - transport used to push the resolved value.
 * @param {any} log - the plugin's logger, for debug output.
 */
function installShellSettings(ctx, settingsCtx, link, log) {
  const z = loadSchemastery(ctx)
  if (z === undefined) {
    console.log(
      '[dsh-plugin-shell-bridge] schemastery not resolvable; dsh-shell settings stay static',
    )
    return
  }

  const settings = settingsCtx.settings
  // The authoritative value: the settings scope while mounted, defaults after.
  let source = () => SHELL_SETTINGS_DEFAULTS

  try {
    settings.installSection(
      ctx,
      SETTINGS_NAMESPACE,
      defineShellSettingsSchema(z),
      SHELL_SETTINGS_DEFAULTS,
      {
        setSource: (current) => {
          source = current
        },
        // Fired on every committed change and once at attach, which is also how
        // a shell that connected late receives the current value.
        onChange: () => {
          try {
            link.send({ kind: 'settings', config: source() })
          } catch (error) {
            log?.debug?.(`[shell-bridge] dropped settings push: ${error?.message ?? error}`)
          }
        },
      },
    )
    console.log(
      `[dsh-plugin-shell-bridge] settings namespace "${SETTINGS_NAMESPACE}" registered`,
    )
  } catch (error) {
    console.log(
      `[dsh-plugin-shell-bridge] settings registration failed: ${error?.message ?? error}`,
    )
  }
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
    /** Handler for requests the shell sends back; see {@link setRequestHandler}. */
    this.onRequest = null
    this.buffer = ''
    this.connect()
  }

  /**
   * Install the handler for requests arriving from the shell.
   *
   * The shell pushes settings writes over the same socket it reads events from,
   * so requests and replies share one connection and are told apart by shape.
   */
  setRequestHandler(handler) {
    this.onRequest = handler
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

    // Requests and replies arrive on the same socket. Lines are buffered
    // because a single TCP read can carry a partial line or several at once.
    socket.on('data', (chunk) => {
      this.buffer += chunk.toString('utf8')
      let index
      while ((index = this.buffer.indexOf('\n')) >= 0) {
        const line = this.buffer.slice(0, index)
        this.buffer = this.buffer.slice(index + 1)
        if (!line.trim()) continue
        let message
        try {
          message = JSON.parse(line)
        } catch {
          // A non-JSON line is not protocol traffic; ignore it rather than
          // crashing.
          continue
        }
        // The shell's own rule: a `method` starts a request, an `ok` answers
        // one. Applying it here keeps both directions on one wire shape.
        if (typeof message?.method === 'string') this.answer(message)
        else this.settle(message)
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

  /**
   * Answer one request the shell made.
   *
   * A handler failure becomes a failed reply rather than an exception: a bad
   * write has to reach the shell as a message, and must never reach the host's
   * event loop as a rejection.
   */
  answer(request) {
    const handler = this.onRequest
    Promise.resolve()
      .then(() =>
        handler
          ? handler(request)
          : { ok: false, error: `unknown method: ${String(request.method)}` },
      )
      .catch((error) => ({ ok: false, error: error?.message ?? String(error) }))
      .then((result) => {
        // Without an id there is nobody to answer; the shell always sends one.
        if (request.id === undefined || request.id === null) return
        const reply = { id: request.id, ok: result?.ok === true }
        if (!reply.ok) reply.error = String(result?.error ?? 'request failed')
        this.send(reply)
      })
      .catch(() => {
        // `send` and the reply assembly cannot throw; this only keeps a bug in
        // the chain from surfacing as an unhandled rejection.
      })
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

  // The shell writes its configuration back through the same socket. The write
  // path is resolved lazily inside the handler, so it works whether settings
  // mounted before or after this row.
  link.setRequestHandler((request) => handleShellRequest(ctx, request))

  // Register the namespace through the optional-service wiring: with no
  // settings provider the plugin boots unchanged, the service is simply absent,
  // and the shell is told its writes are unavailable.
  ctx.inject(['settings'], (settingsCtx) => {
    installShellSettings(ctx, settingsCtx, link, log)
  })
}
