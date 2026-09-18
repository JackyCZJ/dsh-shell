// Tests for the browser half of the plugin.
//
// The client bundle is hand-written rather than built, so there is no compiler
// to catch a mistake: it is a `window.__ModuleLoader__.load({id, factory})` file
// that nothing in this repository executes. These tests execute it the way the
// page's loader does — a fake loader and a fake `require` — so a bundle that
// never registers, or registers under the wrong slot, fails here instead of
// silently rendering nothing in someone's settings window.

import assert from 'node:assert/strict'
import fs from 'node:fs'
import path from 'node:path'
import { fileURLToPath } from 'node:url'
import { test } from 'node:test'

const BUNDLE = path.join(path.dirname(fileURLToPath(import.meta.url)), '..', 'lib', 'client.js')

/** Load the bundle and return the module it registers. */
function loadBundle() {
	const source = fs.readFileSync(BUNDLE, 'utf8')
	let registered
	// The loader is the only global the bundle touches at load time, plus the
	// document when `apply` installs its stylesheet.
	globalThis.window = { __ModuleLoader__: { load: (module) => { registered = module } } }
	globalThis.document = {
		getElementById: () => null,
		createElement: () => ({ id: '', textContent: '' }),
		head: { appendChild: () => {} },
	}
	// eslint-disable-next-line no-new-func -- the bundle is a script, not a module
	new Function(source)()
	assert.ok(registered, 'the bundle must call __ModuleLoader__.load')
	return registered
}

/** The `require` the page's loader provides, limited to what the bundle asks for. */
function fakeRequire(name) {
	if (name === 'react') {
		return {
			useState: (initial) => [initial, () => {}],
			useEffect: () => {},
			createElement: () => null,
		}
	}
	if (name === 'react/jsx-runtime') return { jsx: () => null, jsxs: () => null }
	throw new Error(`the bundle required an unexpected module: ${name}`)
}

/** A client context that records what the bundle registers. */
function fakeContext() {
	const calls = []
	return {
		calls,
		ctx: {
			effect: (fn) => {
				fn()
				return () => {}
			},
			locale: {
				register: (ns) => {
					calls.push({ kind: 'locale', ns })
					return () => {}
				},
				bind: () => (key) => key,
			},
			slots: {
				inject: (name, cb) => {
					calls.push({ kind: 'inject', name })
					cb()
					return () => {}
				},
				register: (options, component) => {
					calls.push({ kind: 'register', options, component })
					return () => {}
				},
			},
		},
	}
}

test('the bundle registers under the plugin package name', () => {
	// The id must match the package name, or the host's scan finds no bundle to
	// match this package's `dsh.client` declaration.
	const module = loadBundle()
	assert.equal(module.id, 'dsh-plugin-shell-bridge')
	assert.equal(typeof module.factory, 'function')
})

test('applying the bundle registers dictionaries and one settings row', () => {
	const module = loadBundle()
	const { ctx, calls } = fakeContext()
	module.factory(fakeRequire).apply(ctx)

	const locale = calls.find((call) => call.kind === 'locale')
	assert.ok(locale, 'the bundle must register its locale dictionaries')
	assert.equal(locale.ns, 'shell-updates')

	// The row goes into the General section's declared child slot, which is what
	// makes it appear inside DSH's own settings rather than in a window of ours.
	const inject = calls.find((call) => call.kind === 'inject')
	assert.equal(inject?.name, 'settings.general.item')

	const registration = calls.find((call) => call.kind === 'register')
	assert.equal(registration?.options.name, 'settings.general.item')
	assert.equal(registration?.options.id, 'shell-updates', 'a list slot needs an id')
	assert.equal(typeof registration?.component, 'function')
})

test('the bundle declares the services it needs', () => {
	const module = loadBundle()
	const exports = module.factory(fakeRequire)
	assert.deepEqual(exports.inject, ['slots', 'locale'])
})

test('the bundle requires only the platform seeds it declares', () => {
	// A require the page's loader cannot answer throws at load time, taking the
	// whole settings page down with it.
	const module = loadBundle()
	const exports = module.factory(fakeRequire)
	const { ctx } = fakeContext()
	// Any missing seed would throw from inside here.
	exports.apply(ctx)
})

test('the host route path is spelled the same in both halves', async () => {
	// The bundle posts to a literal path; the host half registers a constant.
	// If these drift, the row renders and every action 404s.
	const source = fs.readFileSync(BUNDLE, 'utf8')
	const { UPDATE_ROUTE } = await import('../lib/index.js')
	assert.ok(
		source.includes(`'${UPDATE_ROUTE}'`),
		`the bundle must call the host's route ${UPDATE_ROUTE}`,
	)
})
