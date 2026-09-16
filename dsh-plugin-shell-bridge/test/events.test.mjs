// Tests for the agent events this plugin forwards to the shell.
//
// Two mistakes are pinned here, both of which produce a plugin that looks like
// it works while being wrong:
//
//   1. Reading payload fields DSH never sends. Every agent-subject event is
//      dispatched through a fused carrier that injects the subject as
//      `payload.agent`, and the identity is `agent.id`. There is no
//      `sessionId`, so an invented one is always `undefined` — the notification
//      still appears, it just cannot say which session finished.
//
//   2. Registering a listener on a cordis *waterfall* without forwarding
//      `next`. cordis documents that such a listener "vetoes the rest of the
//      chain, including the built-in behavior". `agent/request-error` is a
//      waterfall and `dsh-llm-retry` is on it, so a pure observer that skips
//      `next` silently disables retries for the whole host.
//
// The payloads in these tests are copied from the emit sites in
// `@deepseek-ai/dsh-agent` and `@deepseek-ai/dsh-agent-loop`, so they fail when
// the wire shape is guessed rather than read.

import assert from 'node:assert/strict'
import os from 'node:os'
import path from 'node:path'
import { test } from 'node:test'

import { apply, summarize } from '../lib/index.js'

// Point the transport somewhere with no listener: a real shell may well be
// running on the machine executing these tests, and a successful connection
// would hold the test process open.
process.env.DSH_SHELL_SOCKET = path.join(
  os.tmpdir(),
  `dsh-shell-bridge-events-${process.pid}.sock`,
)

/** The wire form: what the shell actually parses, with `undefined` dropped. */
const wire = (kind, payload) => JSON.parse(JSON.stringify(summarize(kind, payload)))

/**
 * A context just rich enough to run `apply`.
 *
 * `inject` deliberately does not invoke its callback: the settings section is
 * optional by design, and leaving it unmounted keeps these tests independent of
 * a schemastery resolution.
 */
function recordingContext() {
  const listeners = new Map()
  return {
    logger: { debug() {}, info() {}, warn() {}, error() {} },
    on(name, callback) {
      listeners.set(name, callback)
      return () => listeners.delete(name)
    },
    provide() {},
    inject() {},
    listeners,
  }
}

test('the session id is read from the injected agent subject', () => {
  // Every one of these carries `agent`, never a top-level `sessionId`.
  for (const [kind, payload] of [
    ['created', { agent: { id: 'sess-1' } }],
    ['disposed', { agent: { id: 'sess-1' } }],
    ['status', { agent: { id: 'sess-1' }, status: 'running' }],
    ['turn-stopping', { agent: { id: 'sess-1' }, turn: 3, signal: {} }],
    ['request-error', { agent: { id: 'sess-1' }, failure: { message: 'boom' } }],
  ]) {
    assert.equal(wire(kind, payload).sessionId, 'sess-1', `${kind} lost the session id`)
  }
})

test('a payload without an agent reports no session rather than a wrong one', () => {
  assert.equal(wire('turn-stopping', { turn: 3 }).sessionId, undefined)
  // A non-string id must not reach the wire as a number or an object.
  assert.equal(wire('turn-stopping', { agent: { id: 7 } }).sessionId, undefined)
})

test('turn-stopping forwards the agent turn and no invented reason', () => {
  // `agent/turn-stopping` is dispatched as exactly `{ turn, signal, agent }`.
  // It has no `reason`, so the shell's "done" wording is the right body.
  assert.deepEqual(wire('turn-stopping', { agent: { id: 's' }, turn: 2, signal: {} }), {
    kind: 'turn-stopping',
    sessionId: 's',
  })
})

test('a request error forwards the failure message', () => {
  // The emit site nests the text under `failure`; `error.message` is not there.
  const event = wire('request-error', {
    agent: { id: 's' },
    turn: 1,
    step: 0,
    provider: 'deepseek',
    failure: { message: 'upstream 503', code: 'server_error' },
  })
  assert.equal(event.message, 'upstream 503')
  assert.equal(event.sessionId, 's')
})

test('a failure message is flattened and trimmed for a banner', () => {
  const { message } = summarize('request-error', {
    agent: { id: 's' },
    failure: { message: `line one\n\n   line two ${'x'.repeat(400)}` },
  })
  assert.ok(!message.includes('\n'), 'a banner body must be one line')
  assert.equal(message.length, 300)
  assert.ok(message.endsWith('…'), 'a trimmed message is marked as trimmed')
})

test('status defaults instead of dropping the event', () => {
  assert.equal(wire('status', { agent: { id: 's' } }).status, 'unknown')
  assert.equal(wire('status', { agent: { id: 's' }, status: 'running' }).status, 'running')
})

test('the request-error listener continues the waterfall chain', () => {
  const ctx = recordingContext()
  apply(ctx)

  const listener = ctx.listeners.get('agent/request-error')
  assert.equal(typeof listener, 'function', 'the retry chain needs this listener to exist')

  let forwarded = 0
  const result = listener({ agent: { id: 's' }, failure: { message: 'boom' } }, () => {
    forwarded += 1
    return 'downstream decision'
  })

  assert.equal(forwarded, 1, 'skipping next() would veto DSH retry and any later listener')
  assert.equal(result, 'downstream decision', 'the downstream decision must reach the agent loop')
})

test('a throwing observer still leaves the waterfall chain intact', () => {
  const ctx = recordingContext()
  apply(ctx)

  // A payload that cannot be serialised is dropped rather than thrown, and the
  // chain must still be continued: observing must never change the host.
  const circular = { agent: { id: 's' } }
  circular.self = circular

  let forwarded = 0
  ctx.listeners.get('agent/request-error')(circular, () => {
    forwarded += 1
  })
  assert.equal(forwarded, 1)
})

test('plain notifications tolerate the absent next parameter', () => {
  const ctx = recordingContext()
  apply(ctx)

  // `agent/emit` passes one argument, so `next` is undefined for everything
  // except the waterfall. Returning undefined must not throw.
  for (const name of ['agent/created', 'agent/disposed', 'agent/status', 'agent/turn-stopping']) {
    const listener = ctx.listeners.get(name)
    assert.equal(typeof listener, 'function', `${name} is not observed`)
    assert.equal(listener({ agent: { id: 's' } }), undefined)
  }
})

test('the observed agent events are exactly the documented five', () => {
  const ctx = recordingContext()
  apply(ctx)
  // `dispose` is the plugin's own teardown hook and is not an agent event, so
  // it is filtered out rather than folded into the expectation.
  assert.deepEqual(
    [...ctx.listeners.keys()].filter((name) => name.startsWith('agent/')).sort(),
    [
      'agent/created',
      'agent/disposed',
      'agent/request-error',
      'agent/status',
      'agent/turn-stopping',
    ],
  )
})
