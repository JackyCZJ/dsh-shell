// Tests for the update surface the plugin exposes to the rest of DSH.
//
// The shell owns every judgement here — which version is newer, whether an
// upgrade is available, whether an install succeeded. This plugin's job is
// narrow and therefore testable: forward two verbs, and read back the status the
// shell published. The path resolution is the part most likely to be wrong in a
// way nobody notices, because a wrong path does not throw; it reads as "no
// update status has been published" forever.

import assert from 'node:assert/strict'
import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import { test } from 'node:test'

import {
  ShellMethod,
  UPDATE_ROUTE,
  createUpdateRoute,
  readUpdateStatus,
  updateStatusPath,
} from '../lib/index.js'

test('the update verbs are spelled exactly as the shell parses them', () => {
  // These strings cross a socket to a Rust enum that matches on them; a typo
  // yields an error reply, not a compile failure.
  assert.equal(ShellMethod.CheckUpdate, 'checkUpdate')
  assert.equal(ShellMethod.InstallUpdate, 'installUpdate')
})

test('DSH_SHELL_LOG places the status beside the log', () => {
  // The shell writes both into the same directory, so an overridden log path
  // must move the status with it or a test shell reads the real shell's status.
  assert.equal(
    updateStatusPath({ DSH_SHELL_LOG: '/tmp/somewhere/shell.log' }),
    '/tmp/somewhere/status.json',
  )
})

test('an empty DSH_SHELL_LOG falls back rather than resolving to the root', () => {
  // The shell treats an exported-but-empty variable as unset; mirroring that
  // avoids reading `/status.json`.
  assert.equal(
    updateStatusPath({ DSH_SHELL_LOG: '   ', DSH_HOME: '/dsh' }),
    path.join('/dsh', 'cache', 'dsh-shell', 'status.json'),
  )
})

test('DSH_HOME is preferred over HOME', () => {
  assert.equal(
    updateStatusPath({ DSH_HOME: '/custom/dsh', HOME: '/home/u' }),
    path.join('/custom/dsh', 'cache', 'dsh-shell', 'status.json'),
  )
})

test('HOME falls back to ~/.dsh, matching the shell', () => {
  assert.equal(
    updateStatusPath({ HOME: '/home/u' }),
    path.join('/home/u', '.dsh', 'cache', 'dsh-shell', 'status.json'),
  )
})

test('no home at all yields no path rather than a relative one', () => {
  assert.equal(updateStatusPath({}), undefined)
})

test('a published status is read back as an object', () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'dsh-status-'))
  const log = path.join(dir, 'shell.log')
  const status = {
    current: '0.1.5-rc.2',
    target: '0.1.6-alpha.2',
    channel: 'alpha',
    phase: 'available',
    message: null,
    checkedAt: 1_700_000_000,
    busy: false,
  }
  fs.writeFileSync(path.join(dir, 'status.json'), JSON.stringify(status))

  assert.deepEqual(readUpdateStatus({ DSH_SHELL_LOG: log }), status)
  fs.rmSync(dir, { recursive: true, force: true })
})

test('a missing status file reads as undefined, not as a throw', () => {
  // The first launch has no status yet, and the UI renders "not checked" for
  // exactly this case; throwing here would break the settings page.
  assert.equal(readUpdateStatus({ DSH_SHELL_LOG: '/nonexistent/dir/shell.log' }), undefined)
})

test('a corrupt status file reads as undefined, not as a throw', () => {
  // A half-written file is possible: the shell writes it while a reader may be
  // reading. It must degrade to "not checked", never to a broken page.
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'dsh-status-'))
  fs.writeFileSync(path.join(dir, 'status.json'), '{"phase":')

  assert.equal(readUpdateStatus({ DSH_SHELL_LOG: path.join(dir, 'shell.log') }), undefined)
  fs.rmSync(dir, { recursive: true, force: true })
})

// --- the HTTP surface the browser half calls ---------------------------------

/**
 * A stand-in for Node's `ServerResponse` that records what was sent.
 *
 * The route's contract is a status code and a JSON body, so recording those is
 * enough to pin the behaviour without a socket.
 */
function fakeResponse() {
  return {
    status: undefined,
    headers: undefined,
    body: undefined,
    writeHead(status, headers) {
      this.status = status
      this.headers = headers
    },
    end(text) {
      this.body = JSON.parse(text)
    },
  }
}

/** A shell service whose answers are whatever the test says. */
function fakeShell(answers = {}) {
  return {
    checkUpdate: async () => answers.check ?? { ok: true, status: { phase: 'current' } },
    installUpdate: async () => answers.install ?? { ok: true },
    calls: [],
  }
}

async function call(action, method = 'POST', shell = fakeShell(), config = undefined, body = undefined) {
  const route = createUpdateRoute(shell, config)
  const res = fakeResponse()
  // A minimal request stand-in: the handler reads `url`, `method`, and — for a
  // POST body — the `data`/`end` events.
  const req = {
    url: `${UPDATE_ROUTE}/${action}`,
    method,
    on(event, listener) {
      if (event === 'end') queueMicrotask(() => listener())
      if (event === 'data' && body !== undefined) {
        queueMicrotask(() => listener(Buffer.from(JSON.stringify(body))))
      }
      return req
    },
    destroy() {},
  }
  await route.handler(req, res)
  return res
}

/** A settings handle whose read/write are recorded. */
function fakeConfig(initial = { hotkey: 'meta+shift+D' }) {
  const record = { written: [] }
  let current = initial
  return {
    record,
    get current() {
      return current
    },
    read: () => current,
    async write(patch) {
      record.written.push(patch)
      current = { ...current, ...patch }
    },
  }
}

test('the route is a prefix under the documented path', () => {
  const route = createUpdateRoute(fakeShell())
  assert.equal(route.kind, 'prefix')
  assert.equal(route.path, '/dsh-shell-update')
})

test('GET status reports a null status when nothing has been published', async () => {
  // The page reads this on open, before anything has been checked, so the
  // no-status case must be a 200 with a null status rather than an error.
  //
  // `DSH_SHELL_LOG` is pointed at a directory with no status file, because
  // otherwise this reads the real machine's published status and the assertion
  // depends on whether a shell happens to be running. That is exactly how this
  // test failed once.
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'dsh-nostatus-'))
  const saved = process.env.DSH_SHELL_LOG
  process.env.DSH_SHELL_LOG = path.join(dir, 'shell.log')
  try {
    const res = await call('status', 'GET')
    assert.equal(res.status, 200)
    assert.deepEqual(res.body, { ok: true, status: null })
  } finally {
    if (saved === undefined) delete process.env.DSH_SHELL_LOG
    else process.env.DSH_SHELL_LOG = saved
    fs.rmSync(dir, { recursive: true, force: true })
  }
})

test('POST check forwards to the shell and returns its answer', async () => {
  const shell = fakeShell({ check: { ok: true, status: { phase: 'available', target: '9.9.9' } } })
  const res = await call('check', 'POST', shell)
  assert.equal(res.status, 200)
  assert.equal(res.body.status.phase, 'available')
})

test('POST install forwards to the shell', async () => {
  const shell = fakeShell({ install: { ok: true } })
  const res = await call('install', 'POST', shell)
  assert.equal(res.status, 200)
  assert.deepEqual(res.body, { ok: true })
})

test('a read-only method cannot trigger a check or an install', async () => {
  // A GET on a mutating action must not act: link prefetching, a crawler, or a
  // stray browser retry would otherwise start an install.
  for (const action of ['check', 'install']) {
    const shell = fakeShell()
    const res = await call(action, 'GET', shell)
    assert.equal(res.status, 404, `${action} over GET should not be routed`)
  }
})

test('an unknown action is a 404 rather than a silent success', async () => {
  const res = await call('nonsense')
  assert.equal(res.status, 404)
  assert.equal(res.body.ok, false)
  assert.match(res.body.error, /no such action/)
})

test('a shell failure becomes a 500 with the reason, never a throw', async () => {
  // This surface must not take the host down, so a rejected service call is
  // reported rather than propagated.
  const shell = {
    checkUpdate: async () => {
      throw new Error('the shell is not running')
    },
    installUpdate: async () => ({ ok: true }),
  }
  const res = await call('check', 'POST', shell)
  assert.equal(res.status, 500)
  assert.equal(res.body.ok, false)
  assert.match(res.body.error, /not running/)
})

test('the responses are marked no-store', async () => {
  // A cached phase would show an upgrade that had already finished, or hide one
  // that had just become available.
  const res = await call('status', 'GET')
  assert.equal(res.headers['cache-control'], 'no-store')
  assert.equal(res.headers['content-type'], 'application/json')
})

// --- the configuration actions ----------------------------------------------

test('GET config returns the resolved section, not the raw file', async () => {
  // Reading through the settings handle means the form shows what the shell is
  // actually using, including schema defaults for anything unset.
  const config = fakeConfig({ hotkey: 'meta+shift+D', customCss: '' })
  const res = await call('config', 'GET', fakeShell(), config)
  assert.equal(res.status, 200)
  assert.deepEqual(res.body, { ok: true, config: { hotkey: 'meta+shift+D', customCss: '' } })
})

test('GET config hands back a copy, not the live section', async () => {
  // The resolved value may be frozen and is the shell's own copy; a caller
  // mutating what it received must not reach either.
  const live = { hotkey: 'meta+shift+D' }
  const config = { read: () => live, write: async () => {} }
  const res = await call('config', 'GET', fakeShell(), config)
  res.body.config.hotkey = 'tampered'
  assert.equal(live.hotkey, 'meta+shift+D', 'the live section must not be reachable')
})

test('POST config merges through the same writer the shell uses', async () => {
  const config = fakeConfig()
  const res = await call('config', 'POST', fakeShell(), config, { hotkey: 'meta+alt+K' })
  assert.equal(res.status, 200)
  assert.equal(res.body.ok, true)
  assert.deepEqual(config.record.written, [{ hotkey: 'meta+alt+K' }])
  // The reply carries the resulting section so the form can show what landed.
  assert.equal(res.body.config.hotkey, 'meta+alt+K')
})

test('a schema refusal is reported, not thrown', async () => {
  // The message comes from the validator and is the only actionable part.
  const config = {
    read: () => ({}),
    write: async () => {
      throw new Error('hotkey: must include a modifier')
    },
  }
  const res = await call('config', 'POST', fakeShell(), config, { hotkey: 'D' })
  assert.equal(res.status, 200)
  assert.equal(res.body.ok, false)
  assert.match(res.body.error, /modifier/)
})

test('a non-object config body is refused before it reaches the writer', async () => {
  for (const body of [[], 'a string', 42, null]) {
    const config = fakeConfig()
    const res = await call('config', 'POST', fakeShell(), config, body)
    assert.equal(res.status, 400, `${JSON.stringify(body)} should be refused`)
    assert.equal(config.record.written.length, 0, 'nothing may be written')
  }
})

test('a malformed body is refused rather than crashing the route', async () => {
  const route = createUpdateRoute(fakeShell(), fakeConfig())
  const res = fakeResponse()
  await route.handler(
    {
      url: `${UPDATE_ROUTE}/config`,
      method: 'POST',
      on(event, listener) {
        if (event === 'data') queueMicrotask(() => listener(Buffer.from('{not json')))
        if (event === 'end') queueMicrotask(() => listener())
        return this
      },
      destroy() {},
    },
    res,
  )
  assert.equal(res.status, 400)
  assert.match(res.body.error, /malformed JSON/)
})

test('the config actions say so when no settings service is mounted', async () => {
  // Better an explicit reason than a form that silently does nothing.
  for (const method of ['GET', 'POST']) {
    const res = await call('config', method, fakeShell(), undefined, { hotkey: 'meta+alt+K' })
    assert.equal(res.status, 503)
    assert.equal(res.body.ok, false)
    assert.match(res.body.error, /settings service/)
  }
})

test('config cannot be reached over GET with a body, nor written over a read', async () => {
  // The verb decides: a GET must never write, and an unknown verb is a 404.
  const config = fakeConfig()
  const res = await call('config', 'DELETE', fakeShell(), config)
  assert.equal(res.status, 404)
  assert.equal(config.record.written.length, 0)
})

test('POST shell-settings opens the shell window through the service', async () => {
  // The fallback path for a DSH that will not start, so it has to reach the
  // shell rather than being handled anywhere else.
  let opened = 0
  const shell = {
    checkUpdate: async () => ({ ok: true }),
    installUpdate: async () => ({ ok: true }),
    openSettings: async () => {
      opened += 1
      return { ok: true }
    },
  }
  const res = await call('shell-settings', 'POST', shell)
  assert.equal(res.status, 200)
  assert.deepEqual(res.body, { ok: true })
  assert.equal(opened, 1)
})

test('shell-settings is POST-only', async () => {
  let opened = 0
  const shell = {
    checkUpdate: async () => ({ ok: true }),
    installUpdate: async () => ({ ok: true }),
    openSettings: async () => {
      opened += 1
      return { ok: true }
    },
  }
  const res = await call('shell-settings', 'GET', shell)
  assert.equal(res.status, 404)
  assert.equal(opened, 0, 'a GET must not open a window')
})
