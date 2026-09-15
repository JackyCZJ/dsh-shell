// Tests for the DSH-owned `dsh-shell` settings namespace.
//
// DSH, not the shell, owns this configuration: the plugin registers a namespace
// with the settings service and every write is persisted by DSH. That means two
// things must hold, and both are pinned here:
//
//   1. the schema resolves a fresh install to the documented section, and
//   2. a `setConfig` request from the shell reaches the settings service and is
//      answered with `{ ok, error }` rather than by throwing.

import assert from 'node:assert/strict'
import net from 'node:net'
import os from 'node:os'
import path from 'node:path'
import { rm } from 'node:fs/promises'
import { test } from 'node:test'

import {
  SETTINGS_NAMESPACE,
  SHELL_SETTINGS_DEFAULTS,
  ShellMethod,
  apply,
  defineShellSettingsSchema,
  handleShellRequest,
  loadSchemastery,
} from '../lib/index.js'

// Point the transport at a path with no listener. A real shell may be running on
// the machine executing these tests, and a successful connection would hold the
// test process open. The socket test below supplies its own path, then restores
// this one.
process.env.DSH_SHELL_SOCKET = path.join(
  os.tmpdir(),
  `dsh-shell-bridge-tests-${process.pid}.sock`,
)

/** The section exactly as it is documented, independent of the implementation. */
const DOCUMENTED = {
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
}

/**
 * A stand-in for the Cordis context that records the settings calls the plugin
 * makes. `settings` is optional so a test can exercise the absent-provider path.
 */
function fakeContext({ settings } = {}) {
  const record = {
    provided: [],
    events: [],
    injected: [],
    installs: [],
    updates: [],
    handlers: {},
  }
  const settingsCtx = {
    settings,
    on() {},
    get(name) {
      return name === 'settings' ? settings : undefined
    },
  }
  const ctx = {
    logger: { debug() {}, info() {}, warn() {} },
    on(event, handler) {
      ;(record.handlers[event] ??= []).push(handler)
    },
    provide(name, service) {
      record.provided.push({ name, service })
    },
    get(name) {
      return name === 'settings' ? settings : undefined
    },
    inject(deps, callback) {
      record.injected.push(deps)
      callback(settingsCtx)
    },
    /** Run the handlers registered for `dispose`, so tests can tear the link down. */
    fire(event) {
      for (const handler of record.handlers[event] ?? []) handler()
    },
  }
  return { ctx, record }
}

/** A minimal settings service: `installSection` records, `update` records. */
function fakeSettings(record = {}) {
  record.installs ??= []
  record.updates ??= []
  return {
    installSection(owner, ns, schema, entry, hooks) {
      record.installs.push({ ns, entry, schema })
      // The real installer hands the resolved value back through `setSource`
      // and announces it once through `onChange`.
      hooks.setSource(() => entry)
      hooks.onChange()
    },
    async update(ns, patch) {
      if (record.updateError) throw new Error(record.updateError)
      record.updates.push({ ns, patch })
    },
  }
}

/** Wait for one newline-delimited message matching `predicate`. */
function waitFor(connection, predicate, label) {
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error(`timed out waiting for ${label}`)), 5000)
    timer.unref?.()
    let buffer = ''
    const onData = (chunk) => {
      buffer += chunk
      let index
      while ((index = buffer.indexOf('\n')) >= 0) {
        const line = buffer.slice(0, index)
        buffer = buffer.slice(index + 1)
        if (!line.trim()) continue
        let message
        try {
          message = JSON.parse(line)
        } catch {
          continue
        }
        if (!predicate(message)) continue
        clearTimeout(timer)
        connection.off('data', onData)
        resolve(message)
      }
    }
    connection.setEncoding('utf8')
    connection.on('data', onData)
  })
}

test('the namespace and defaults match the documented section', () => {
  assert.equal(SETTINGS_NAMESPACE, 'dsh-shell')
  assert.deepEqual(SHELL_SETTINGS_DEFAULTS, DOCUMENTED)
})

test('the schema resolves a fresh or partial section to the documented defaults', () => {
  const z = loadSchemastery({})
  assert.ok(z, 'schemastery was not resolvable; run this test where DSH is installed')

  const schema = defineShellSettingsSchema(z)

  assert.deepEqual(schema({}), DOCUMENTED, 'an empty section must resolve to the defaults')
  assert.deepEqual(
    schema({ hotkey: 'meta+alt+K' }),
    { ...DOCUMENTED, hotkey: 'meta+alt+K' },
    'a partial section must fill in the remaining defaults',
  )
  assert.deepEqual(
    schema({ light: { accent: '#ff0000' } }),
    { ...DOCUMENTED, light: { ...DOCUMENTED.light, accent: '#ff0000' } },
    'a partial palette must fill in the remaining colours',
  )

  // DSH owns validation, so the schema must actually refuse bad values rather
  // than silently coercing them.
  assert.throws(() => schema({ light: { accent: 5 } }))
})

test('apply registers the dsh-shell section through installSection', () => {
  const settingsRecord = {}
  const { ctx, record } = fakeContext({ settings: fakeSettings(settingsRecord) })
  apply(ctx)
  try {
    assert.equal(record.injected[0]?.[0], 'settings', 'settings must be optional-injected')
    assert.equal(settingsRecord.installs.length, 1, 'expected exactly one registered section')
    assert.equal(settingsRecord.installs[0].ns, 'dsh-shell')
    assert.deepEqual(
      settingsRecord.installs[0].entry,
      DOCUMENTED,
      'defaults are the composition base',
    )
  } finally {
    ctx.fire('dispose')
  }
})

test('setConfig is routed to the settings service as a merge', async () => {
  const record = {}
  const { ctx } = fakeContext({ settings: fakeSettings(record) })
  apply(ctx)
  try {
    const config = { hotkey: 'meta+alt+K', light: { accent: '#ff0000' } }
    const result = await handleShellRequest(ctx, {
      id: 'shell-1',
      method: ShellMethod.SetConfig,
      config,
    })

    assert.deepEqual(result, { ok: true })
    assert.deepEqual(
      record.updates,
      [{ ns: 'dsh-shell', patch: config }],
      'the write must reach settings.update for our namespace',
    )
  } finally {
    ctx.fire('dispose')
  }
})

test('a rejected write is reported as { ok: false, error }, not thrown', async () => {
  const record = { updateError: 'appearance: expected one of system | light | dark' }
  const { ctx } = fakeContext({ settings: fakeSettings(record) })
  apply(ctx)
  try {
    const result = await handleShellRequest(ctx, {
      id: 'shell-2',
      method: ShellMethod.SetConfig,
      config: { appearance: 'chartreuse' },
    })

    assert.equal(result.ok, false)
    assert.match(result.error, /appearance/)
  } finally {
    ctx.fire('dispose')
  }
})

test('setConfig without a settings provider is refused, not thrown', async () => {
  const { ctx } = fakeContext()
  apply(ctx)
  try {
    const result = await handleShellRequest(ctx, {
      id: 'shell-3',
      method: ShellMethod.SetConfig,
      config: { appearance: 'dark' },
    })

    assert.equal(result.ok, false)
    assert.match(result.error, /settings service/i)
  } finally {
    ctx.fire('dispose')
  }
})

test('a non-object config is refused before reaching settings', async () => {
  const record = {}
  const { ctx } = fakeContext({ settings: fakeSettings(record) })
  apply(ctx)
  try {
    const result = await handleShellRequest(ctx, {
      id: 'shell-4',
      method: ShellMethod.SetConfig,
      config: 'dark',
    })

    assert.equal(result.ok, false)
    assert.deepEqual(record.updates, [])
  } finally {
    ctx.fire('dispose')
  }
})

test('an unknown shell method is reported back', async () => {
  const { ctx } = fakeContext()
  apply(ctx)
  try {
    const result = await handleShellRequest(ctx, { id: 'shell-5', method: 'no-such-method' })
    assert.equal(result.ok, false)
    assert.match(result.error, /unknown method/)
  } finally {
    ctx.fire('dispose')
  }
})

// End-to-end over a real Unix socket: this is the test that proves the routing
// lives in the transport, not only in the exported helper.
test('a setConfig request from the shell is persisted and answered on the wire', async () => {
  const record = {}
  const { ctx } = fakeContext({ settings: fakeSettings(record) })
  const socket = path.join(os.tmpdir(), `dsh-shell-bridge-test-${process.pid}.sock`)
  await rm(socket, { force: true })

  const server = net.createServer()
  await new Promise((resolve, reject) => {
    server.once('error', reject)
    server.listen(socket, resolve)
  })

  const previous = process.env.DSH_SHELL_SOCKET
  process.env.DSH_SHELL_SOCKET = socket
  try {
    apply(ctx)

    const connection = await new Promise((resolve, reject) => {
      const timer = setTimeout(() => reject(new Error('the plugin did not connect')), 5000)
      timer.unref?.()
      server.once('connection', (socket) => {
        clearTimeout(timer)
        resolve(socket)
      })
    })

    // The plugin pushes the resolved section on attach, and answers the write
    // separately; both are read off the one connection.
    const pushed = waitFor(connection, (m) => m.kind === 'settings', 'the settings push')
    const reply = waitFor(connection, (m) => typeof m.ok === 'boolean', 'the setConfig reply')

    connection.write(
      `${JSON.stringify({
        id: 'shell-1',
        method: ShellMethod.SetConfig,
        config: { appearance: 'dark' },
      })}\n`,
    )

    assert.deepEqual(
      await pushed,
      { kind: 'settings', config: DOCUMENTED },
      'the shell must receive the full resolved section as a settings event',
    )
    assert.deepEqual(await reply, { id: 'shell-1', ok: true })
    assert.deepEqual(record.updates, [{ ns: 'dsh-shell', patch: { appearance: 'dark' } }])
  } finally {
    ctx.fire('dispose')
    if (previous === undefined) delete process.env.DSH_SHELL_SOCKET
    else process.env.DSH_SHELL_SOCKET = previous
    server.closeAllConnections?.()
    await new Promise((resolve) => server.close(resolve))
    await rm(socket, { force: true })
  }
})
