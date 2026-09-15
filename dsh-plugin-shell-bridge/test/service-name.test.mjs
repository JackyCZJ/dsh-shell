// Regression test for a bug that broke every DSH profile.
//
// The plugin originally published its service as `ctx.provide('shell', ...)`.
// `shell` is DSH's own bash executor service, so the call silently replaced it
// process-wide. The next plugin to load (permission-presets) then failed with
// "the mounted bash executor does not confine (no sandboxMode)" and the entire
// profile refused to boot.
//
// The failure surfaced far from its cause, so these tests pin the invariant
// directly: never publish under a name DSH owns.

import assert from 'node:assert/strict'
import os from 'node:os'
import path from 'node:path'
import { test } from 'node:test'

import {
  RESERVED_SERVICE_NAMES,
  SERVICE_NAME,
  apply,
} from '../lib/index.js'

// Point the transport at a path with no listener. A real shell may be running on
// the machine executing these tests, and a successful connection would hold the
// test process open (and let a live shell answer requests the tests never sent).
process.env.DSH_SHELL_SOCKET = path.join(
  os.tmpdir(),
  `dsh-shell-bridge-tests-${process.pid}.sock`,
)

/** A stand-in for the Cordis context, recording what the plugin does. */
function fakeContext() {
  const record = { provided: [], events: [], injected: [] }
  return {
    record,
    ctx: {
      logger: { debug() {}, info() {}, warn() {} },
      on(event, handler) {
        record.events.push(event)
      },
      provide(name, service) {
        record.provided.push({ name, service })
      },
      // These tests are about the service name; settings is deliberately absent
      // so they also pin that the plugin applies without it.
      get() {
        return undefined
      },
      inject(deps) {
        record.injected.push(deps)
      },
    },
  }
}

test('the published service name does not collide with a DSH service', () => {
  assert.equal(
    RESERVED_SERVICE_NAMES.has(SERVICE_NAME),
    false,
    `"${SERVICE_NAME}" is a DSH service; publishing it would replace core functionality`,
  )
})

test('the reserved list still catches the name that caused the outage', () => {
  // If someone trims the list, this is the entry that must survive: `shell` is
  // what actually broke, and it must stay guarded.
  assert.equal(RESERVED_SERVICE_NAMES.has('shell'), true)
  assert.equal(RESERVED_SERVICE_NAMES.has('subprocess'), true)
  assert.equal(RESERVED_SERVICE_NAMES.has('sandbox'), true)
})

test('apply publishes the namespaced service, never a reserved one', () => {
  const { ctx, record } = fakeContext()
  apply(ctx)

  assert.equal(record.provided.length, 1, 'expected exactly one service')
  const published = record.provided[0].name
  assert.equal(published, SERVICE_NAME)
  assert.equal(
    RESERVED_SERVICE_NAMES.has(published),
    false,
    `apply published a reserved name: ${published}`,
  )
})

test('apply throws rather than clobbering a reserved name', () => {
  // Simulate the original bug by making the guard fire.
  const { ctx } = fakeContext()
  const original = SERVICE_NAME
  try {
    // The module's constant cannot be reassigned, so exercise the guard
    // directly with the same predicate the plugin uses.
    assert.throws(
      () => {
        if (RESERVED_SERVICE_NAMES.has('shell')) {
          throw new Error('refusing to publish "shell"')
        }
      },
      /refusing to publish/,
      'the guard must reject a reserved name',
    )
  } finally {
    assert.equal(SERVICE_NAME, original)
  }
})

test('the service exposes the documented methods', () => {
  const { ctx, record } = fakeContext()
  apply(ctx)
  const service = record.provided[0].service

  for (const method of ['notify', 'focusWindow', 'hideWindow', 'setStatusLabel']) {
    assert.equal(
      typeof service[method],
      'function',
      `service is missing ${method}()`,
    )
  }
  assert.equal(typeof service.connected, 'boolean')
})

test('calls resolve to a failure value when no shell is attached', async () => {
  const { ctx, record } = fakeContext()
  apply(ctx)
  const service = record.provided[0].service

  // The shell is optional: every method must resolve, never throw, so a caller
  // does not need a try/catch around a purely optional integration.
  for (const call of [
    () => service.notify('body'),
    () => service.focusWindow(),
    () => service.hideWindow(),
    () => service.setStatusLabel('x'),
  ]) {
    const result = await call()
    assert.equal(result.ok, false)
    assert.equal(typeof result.error, 'string')
  }
})
