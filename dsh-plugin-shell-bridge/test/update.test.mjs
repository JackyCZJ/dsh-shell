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

async function call(action, method = 'POST', shell = fakeShell()) {
  const route = createUpdateRoute(shell)
  const res = fakeResponse()
  await route.handler({ url: `${UPDATE_ROUTE}/${action}`, method }, res)
  return res
}

test('the route is a prefix under the documented path', () => {
  const route = createUpdateRoute(fakeShell())
  assert.equal(route.kind, 'prefix')
  assert.equal(route.path, '/dsh-shell-update')
})

test('GET status reports the published status', async () => {
  // The page reads this on open, before anything has been checked, so the
  // no-status case must be a 200 with a null status rather than an error.
  const res = await call('status', 'GET')
  assert.equal(res.status, 200)
  assert.deepEqual(res.body, { ok: true, status: null })
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
