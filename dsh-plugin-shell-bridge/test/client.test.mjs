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
	// The bundle fetches its configuration on mount; a stub keeps the mount
	// synchronous and side-effect free.
	globalThis.fetch = async () => ({
		ok: true,
		status: 200,
		json: async () => ({ ok: true, config: { hotkey: 'meta+shift+D', customCss: '' } }),
	})
	// eslint-disable-next-line no-new-func -- the bundle is a script, not a module
	new Function(source)()
	assert.ok(registered, 'the bundle must call __ModuleLoader__.load')
	return registered
}

/** The `require` the page's loader provides, limited to what the bundle asks for. */
function fakeRequire(name) {
	if (name === 'react') {
		const cells = []
		let cursor = 0
		return {
			__reset: () => {
				cursor = 0
			},
			// Enough of a hook runtime to mount once: state with a working setter,
			// and effects that run. Anything more faithful is jsdom, not a unit test.
			useState: (initial) => {
				const at = cursor
				cursor += 1
				if (cells[at] === undefined) cells[at] = initial
				return [cells[at], (next) => {
					cells[at] = typeof next === 'function' ? next(cells[at]) : next
				}]
			},
			useEffect: (fn) => {
				fn()
			},
			createElement: () => null,
		}
	}
	if (name === 'react/jsx-runtime') {
		// Record the element names so a test can assert on the tree's shape
		// without a renderer.
		return {
			__elements: [],
			jsx: (type, props) => ({ type, props }),
			jsxs: (type, props) => ({ type, props }),
		}
	}
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
	assert.equal(
		inject?.name,
		'settings.section',
		'this configuration is its own section, not a row inside General',
	)

	const registration = calls.find((call) => call.kind === 'register')
	assert.equal(registration?.options.name, 'settings.section')
	assert.equal(registration?.options.id, 'shell', 'a list slot needs an id')
	assert.equal(typeof registration?.options.label, 'function', 'the nav entry needs a label')
	assert.equal(typeof registration?.options.order, 'number', 'or it lands wherever')
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

// --- mounting the row ---------------------------------------------------------

/** Mount the exported section and return the element tree. */
function mountRow() {
	const module = loadBundle()
	const require = fakeRequire('react') && fakeRequire
	const exports = module.factory(fakeRequire)
	const { ctx } = fakeContext()
	exports.apply(ctx)
	// Rendered without a renderer: this is about whether the component body runs
	// at all, which is where a typo in a field name or hook order shows up.
	return exports.ShellSettingsSection({})
}

test('the settings row mounts without throwing', () => {
	const tree = mountRow()
	assert.ok(tree, 'the row must render something')
	assert.ok(Array.isArray(tree.props.children), 'the row composes several children')
})

test('the section registers itself and nothing into General', async () => {
	// It used to register a row into `settings.general.item`. Moving to its own
	// section must *stop* doing that, or the configuration would appear twice:
	// once under General and once under its own tab.
	const module = loadBundle()
	const exports = module.factory(fakeRequire)
	const { ctx, calls } = fakeContext()
	exports.apply(ctx)
	const injected = calls.filter((call) => call.kind === 'inject').map((call) => call.name)
	assert.ok(!injected.includes('settings.general.item'), 'General must be left alone')
	assert.ok(injected.includes('settings.section'))
})

test('the diff sends only what changed', () => {
	const exports = loadBundle().factory(fakeRequire)
	const base = {
		hotkey: 'meta+shift+D',
		customCss: '',
		updateChannel: 'latest',
		light: { accent: '#4176e6', background: '#ffffff' },
		dark: { accent: '#4176e6', background: '#151517' },
	}
	assert.deepEqual(exports.diff(base, base), {}, 'an unchanged form sends nothing')

	const edited = {
		...base,
		hotkey: 'meta+alt+K',
		light: { ...base.light, accent: '#ff0000' },
	}
	assert.deepEqual(exports.diff(base, edited), {
		hotkey: 'meta+alt+K',
		light: { accent: '#ff0000' },
	})
})

test('the diff will not send a field the shell never resolved', () => {
	// An older document has no `updateChannel`. Sending the form's fallback would
	// overwrite whatever the shell is actually using with a value the user never
	// saw, so an absent base field means an absent patch field.
	const exports = loadBundle().factory(fakeRequire)
	const base = { hotkey: 'meta+shift+D', customCss: '' }
	const edited = { ...base, updateChannel: 'alpha' }
	assert.deepEqual(exports.diff(base, edited), {}, 'nothing may be invented')
})

test('the diff handles a missing palette without throwing', () => {
	const exports = loadBundle().factory(fakeRequire)
	assert.deepEqual(exports.diff({ hotkey: 'a' }, { hotkey: 'a' }), {})
	assert.deepEqual(exports.diff(null, { hotkey: 'a' }), {})
})

// --- refusing values the schema would accept --------------------------------

test('a shortcut without a modifier is refused before it is sent', () => {
	// The schema says `hotkey` is a string, so the host accepts "D"; the shell
	// then cannot parse it and silently falls back to the default. Refusing here
	// is what turns that into something the user sees.
	const { problem } = loadBundle().factory(fakeRequire)
	assert.equal(problem({ hotkey: 'D' }), 'config.badHotkey')
	assert.equal(problem({ hotkey: 'shift+D' }), null)
	assert.equal(problem({ hotkey: 'meta+alt+K' }), null)
	assert.equal(problem({ hotkey: 'Cmd+Shift+P' }), null, 'cmd is a modifier too')
	assert.equal(problem({ hotkey: '' }), null, 'empty is not this check\u2019s business')
	assert.equal(problem({}), null)
})

test('a colour that is not six-digit hex is refused', () => {
	const { problem } = loadBundle().factory(fakeRequire)
	assert.equal(problem({ light: { accent: 'red' } }), 'config.badColour')
	assert.equal(problem({ dark: { background: '#fff' } }), 'config.badColour')
	assert.equal(problem({ light: { accent: '#4176e6' } }), null)
})

test('a patch with nothing to check passes', () => {
	const { problem } = loadBundle().factory(fakeRequire)
	assert.equal(problem({ customCss: 'body { color: red; }' }), null)
	assert.equal(problem({ updateChannel: 'alpha' }), null)
})
