// Browser half of the shell-bridge plugin: the DSH Updates section.
//
// This is a hand-written `window.__ModuleLoader__.load({id, factory})` bundle,
// which is what a `dsh.client` package's `./client` export is expected to be.
// It is not generated: DSH resolves that export straight from this package, so
// a plugin installed by `link:` needs no build step — see the note in
// `docs/auto-upgrade.md`.
//
// Everything it displays comes from the shell, through the HTTP route the host
// half registers:
//
//   GET  /dsh-shell-update/status   the last published status
//   POST /dsh-shell-update/check    ask for a new check, then read the status
//   POST /dsh-shell-update/install  stage, verify and apply an upgrade
//
// It deliberately does not try `ctx.remote.desktopShell`: that surface is
// generated from TypeScript decorators into a `typert.remote-client` module,
// which a plain JavaScript plugin cannot produce. A route reaches the same
// service with no code generation.

window.__ModuleLoader__.load({
	id: 'dsh-plugin-shell-bridge',
	factory: (require) => {
		var module = { exports: {} }
		var exports = module.exports
		Object.defineProperty(exports, Symbol.toStringTag, { value: 'Module' })

		const react = require('react')
		const jsx = require('react/jsx-runtime')

		/** The route the host half serves; must match `UPDATE_ROUTE`. */
		const ROUTE = '/dsh-shell-update'

		/** Locale namespace for this bundle's strings. */
		const NS = 'shell-updates'

		/** Key-set source of truth. */
		const zh = {
			'title': 'DSH 版本',
			'current': '当前版本',
			'unknown': '未知',
			'checkNow': '检查更新',
			'checking': '检查中…',
			'upToDate': '已是最新版本。',
			'available': '发现新版本 {target}。',
			'upgrade': '升级到 {target}',
			'working': '正在下载并验证…需要几秒。',
			'restart': '已升级到 {target}，重启应用后生效。',
			'failed': '更新失败：{reason}',
			'unsupported': '这个 DSH 不是包管理器安装的，无法自动升级。',
			'notChecked': '尚未检查。',
			'noShell': '桌面 shell 未连接，无法升级。',
			'channel': '通道：{channel}',

			'section.title': 'DSH Shell',
			'config.title': '外观与快捷键',
			'config.hint': '改动会立即生效，无需重启。',
			'config.hotkey': '唤起快捷键',
			'config.hotkeyHint': '至少需要一个修饰键，例如 meta+shift+D。',
			'config.light': '亮色',
			'config.dark': '暗色',
			'config.customCss': '自定义 CSS',
			'config.customCssHint': '追加在内置规则之后，因此优先级最高。',
			'config.save': '保存',
			'config.revert': '还原',
			'config.saving': '保存中…',
			'config.saved': '已保存，立即生效。',
			'config.unsaved': '有未保存的改动。',
			'config.loading': '读取中…',
			'config.unavailable': '设置服务不可用，无法在这里修改配置。',
			'config.badHotkey': '快捷键至少需要一个修饰键（meta/ctrl/alt/shift）。',
			'config.badColour': '颜色必须是六位十六进制，例如 #4176e6。',
			'config.colour.background': '背景',
			'config.colour.surface': '表面',
			'config.colour.surfaceHover': '悬停表面',
			'config.colour.border': '边框',
			'config.colour.text': '文字',
			'config.colour.textMuted': '次要文字',
			'config.colour.accent': '强调色',
		}
		const en = {
			'title': 'DSH version',
			'current': 'Installed',
			'unknown': 'unknown',
			'checkNow': 'Check Now',
			'checking': 'Checking…',
			'upToDate': 'Up to date.',
			'available': 'Version {target} is available.',
			'upgrade': 'Upgrade to {target}',
			'working': 'Downloading and verifying… this takes a few seconds.',
			'restart': 'Upgraded to {target}. Restart the app to apply it.',
			'failed': 'Update failed: {reason}',
			'unsupported': 'This DSH was not installed by a package manager, so it cannot upgrade itself.',
			'notChecked': 'Not checked yet.',
			'noShell': 'No desktop shell is connected, so it cannot upgrade.',
			'channel': 'Channel: {channel}',

			'section.title': 'DSH Shell',
			'config.title': 'Appearance and shortcut',
			'config.hint': 'Changes apply immediately; no restart.',
			'config.hotkey': 'Summon shortcut',
			'config.hotkeyHint': 'At least one modifier is required, e.g. meta+shift+D.',
			'config.light': 'Light',
			'config.dark': 'Dark',
			'config.customCss': 'Custom CSS',
			'config.customCssHint': 'Appended after the built-in rules, so it wins.',
			'config.save': 'Save',
			'config.revert': 'Revert',
			'config.saving': 'Saving…',
			'config.saved': 'Saved. Applied immediately.',
			'config.unsaved': 'Unsaved changes.',
			'config.loading': 'Loading…',
			'config.unavailable': 'The settings service is unavailable, so the configuration cannot be edited here.',
			'config.badHotkey': 'A shortcut needs at least one modifier (meta/ctrl/alt/shift).',
			'config.badColour': 'A colour must be six-digit hex, e.g. #4176e6.',
			'config.colour.background': 'Background',
			'config.colour.surface': 'Surface',
			'config.colour.surfaceHover': 'Surface hover',
			'config.colour.border': 'Border',
			'config.colour.text': 'Text',
			'config.colour.textMuted': 'Muted text',
			'config.colour.accent': 'Accent',
		}

		/**
		 * Call the host route.
		 *
		 * @param {string} action - `status`, `check` or `install`.
		 * @param {string} method - HTTP method.
		 * @returns {Promise<object>} the parsed body, or an `{ok:false}` shape.
		 */
		async function call(action, method, body) {
			try {
				const response = await fetch(`${ROUTE}/${action}`, {
					method,
					headers: body === undefined ? undefined : { 'content-type': 'application/json' },
					body: body === undefined ? undefined : JSON.stringify(body),
				})
				if (!response.ok && response.status !== 200) {
					return { ok: false, error: `HTTP ${response.status}` }
				}
				return await response.json()
			} catch (error) {
				return { ok: false, error: error?.message ?? String(error) }
			}
		}

		/** The strings the component uses. */
		let t = (key) => key

		/** The palette field names the shell defines, in display order. */
		const PALETTE_KEYS = [
			'background',
			'surface',
			'surfaceHover',
			'border',
			'text',
			'textMuted',
			'accent',
		]

		/** A six-digit hex colour, which is what the shell's own validation takes. */
		const HEX = /^#[0-9a-fA-F]{6}$/

		/**
		 * The changes between what the form holds and what was loaded.
		 *
		 * Only differences are sent. The write is a merge, so resending an
		 * untouched field is harmless — but sending one the shell did not resolve
		 * (an older document has no `updateChannel`) would overwrite a value this
		 * form never read, which is not.
		 *
		 * @param base - the configuration as loaded.
		 * @param local - the configuration as edited.
		 * @returns the patch to send, which may be empty.
		 */
		function diff(base, local) {
			const patch = {}
			if (base === null || local === null) return patch
			if (local.hotkey !== base.hotkey) patch.hotkey = local.hotkey
			if (local.customCss !== base.customCss) patch.customCss = local.customCss
			if (local.updateChannel !== base.updateChannel && base.updateChannel !== undefined) {
				patch.updateChannel = local.updateChannel
			}
			for (const name of ['light', 'dark']) {
				const from = base[name]
				const to = local[name]
				if (from === undefined || to === undefined) continue
				for (const key of PALETTE_KEYS) {
					if (to[key] !== undefined && to[key] !== from[key]) {
						patch[name] = { ...(patch[name] ?? {}), [key]: to[key] }
					}
				}
			}
			return patch
		}

		/** The modifiers a shortcut must include at least one of. */
		const MODIFIERS = ['meta', 'cmd', 'command', 'ctrl', 'control', 'alt', 'option', 'shift']

		/**
		 * Check a patch before sending it.
		 *
		 * The schema only says these fields are strings, so the host would accept
		 * `"D"` as a shortcut and `"red"` as a colour. The shell rejects both —
		 * it has to parse the shortcut and apply the colour — but that check lives
		 * on the far side of a socket, so a bad value would be written and then
		 * quietly fall back to a default. Refusing it here is the difference
		 * between an error the user sees and a setting that silently does not
		 * work.
		 *
		 * @param patch - the change about to be sent.
		 * @returns an error message, or null.
		 */
		function problem(patch) {
			if (typeof patch.hotkey === 'string' && patch.hotkey.trim() !== '') {
				const parts = patch.hotkey.toLowerCase().split('+').map((part) => part.trim())
				if (!parts.some((part) => MODIFIERS.includes(part))) return 'config.badHotkey'
			}
			for (const name of ['light', 'dark']) {
				for (const [key, value] of Object.entries(patch[name] ?? {})) {
					if (!HEX.test(value)) return `config.badColour`
				}
			}
			return null
		}

		/** One labelled colour input, with the hex shown beside it. */
		function ColourField({ label, value, onChange }) {
			return jsx.jsxs('label', {
				className: 'dsh-shell-field',
				children: [
					jsx.jsx('span', { className: 'dsh-shell-field-label', children: label }),
					jsx.jsxs('span', {
						className: 'dsh-shell-colour',
						children: [
							jsx.jsx('input', {
								type: 'color',
								value: HEX.test(value) ? value : '#000000',
								onChange: (event) => onChange(event.target.value),
							}),
							jsx.jsx('code', { children: value }),
						],
					}),
				],
			})
		}

		/**
		 * The DSH Shell configuration form.
		 *
		 * Everything the shell's own settings window can change: the summon
		 * shortcut, both palettes, and the custom CSS. It lives here because a
		 * second settings window drawn by the shell looked like what it was — a
		 * different application. The shell keeps its own window as the fallback
		 * for when DSH will not start.
		 *
		 * @returns the form element tree.
		 */
		function ConfigSection() {
			const [loaded, setLoaded] = react.useState(null)
			const [local, setLocal] = react.useState(null)
			const [state, setState] = react.useState('loading')
			const [message, setMessage] = react.useState(null)
			const [error, setError] = react.useState(null)

			react.useEffect(() => {
				let live = true
				call('config', 'GET').then((reply) => {
					if (!live) return
					if (reply.ok) {
						setLoaded(reply.config)
						setLocal(reply.config)
						setState('ready')
					} else {
						setError(reply.error)
						setState('unavailable')
					}
				})
				return () => {
					live = false
				}
			}, [])

			if (state === 'loading') {
				return jsx.jsx('div', { className: 'dsh-shell-config', children: t('config.loading') })
			}
			if (state === 'unavailable') {
				return jsx.jsx('div', {
					className: 'dsh-shell-config',
					children: jsx.jsx('div', {
						className: 'dsh-shell-update-message is-error',
						children: error ?? t('config.unavailable'),
					}),
				})
			}
			if (local === null) return null

			const patch = diff(loaded, local)
			const dirty = Object.keys(patch).length > 0

			const edit = (change) => {
				setLocal((current) => ({ ...current, ...change }))
				setMessage(null)
				setError(null)
			}
			const editColour = (name, key, value) => {
				setLocal((current) => ({
					...current,
					[name]: { ...(current[name] ?? {}), [key]: value },
				}))
				setMessage(null)
				setError(null)
			}

			const save = async () => {
				const refusal = problem(patch)
				if (refusal !== null) {
					setError(t(refusal))
					return
				}
				setState('saving')
				setError(null)
				const reply = await call('config', 'POST', patch)
				if (reply.ok) {
					// Adopt what the settings service resolved, so the form shows
					// the stored value rather than the typed one.
					setLoaded(reply.config)
					setLocal(reply.config)
					setState('ready')
					setMessage(t('config.saved'))
				} else {
					setState('ready')
					setError(reply.error)
				}
			}

			const palette = (name) =>
				jsx.jsxs('div', {
					className: 'dsh-shell-palette',
					children: [
						jsx.jsx('div', {
							className: 'dsh-shell-subhead',
							children: t(`config.${name}`),
						}),
						...PALETTE_KEYS.filter((key) => local[name]?.[key] !== undefined).map((key) =>
							jsx.jsx(
								ColourField,
								{
									label: t(`config.colour.${key}`),
									value: local[name][key],
									onChange: (value) => editColour(name, key, value),
								},
								`${name}.${key}`,
							),
						),
					],
				})

			return jsx.jsxs('div', {
				className: 'dsh-shell-config',
				children: [
					jsx.jsxs('div', {
						className: 'dsh-shell-config-head',
						children: [
							jsx.jsx('div', { className: 'dsh-shell-subhead', children: t('config.title') }),
							jsx.jsx('div', {
								className: 'dsh-shell-update-message',
								children: t('config.hint'),
							}),
						],
					}),
					jsx.jsxs('label', {
						className: 'dsh-shell-field',
						children: [
							jsx.jsx('span', {
								className: 'dsh-shell-field-label',
								children: t('config.hotkey'),
							}),
							jsx.jsxs('span', {
								className: 'dsh-shell-field-body',
								children: [
									jsx.jsx('input', {
										type: 'text',
										spellCheck: false,
										value: local.hotkey ?? '',
										onChange: (event) => edit({ hotkey: event.target.value }),
									}),
									jsx.jsx('span', {
										className: 'dsh-shell-update-message',
										children: t('config.hotkeyHint'),
									}),
								],
							}),
						],
					}),
					jsx.jsx('div', {
						className: 'dsh-shell-palettes',
						children: [palette('light'), palette('dark')],
					}),
					jsx.jsxs('label', {
						className: 'dsh-shell-field',
						children: [
							jsx.jsx('span', {
								className: 'dsh-shell-field-label',
								children: t('config.customCss'),
							}),
							jsx.jsxs('span', {
								className: 'dsh-shell-field-body',
								children: [
									jsx.jsx('textarea', {
										rows: 4,
										spellCheck: false,
										value: local.customCss ?? '',
										onChange: (event) => edit({ customCss: event.target.value }),
									}),
									jsx.jsx('span', {
										className: 'dsh-shell-update-message',
										children: t('config.customCssHint'),
									}),
								],
							}),
						],
					}),
					jsx.jsxs('div', {
						className: 'dsh-shell-config-actions',
						children: [
							jsx.jsx('button', {
								type: 'button',
								className: 'primary',
								disabled: !dirty || state === 'saving',
								onClick: save,
								children: state === 'saving' ? t('config.saving') : t('config.save'),
							}),
							jsx.jsx('button', {
								type: 'button',
								disabled: !dirty || state === 'saving',
								onClick: () => {
									setLocal(loaded)
									setMessage(null)
									setError(null)
								},
								children: t('config.revert'),
							}),
							jsx.jsx('span', {
								className: `dsh-shell-update-message${error ? ' is-error' : ''}`,
								children: error ?? message ?? (dirty ? t('config.unsaved') : ''),
							}),
						],
					}),
				],
			})
		}

		/**
		 * The update row: the installed DSH version and the upgrade control.
		 *
		 * @returns the row element tree.
		 */
		function ShellSettingsSection() {
			// A section of its own rather than a row inside General: the shell's
			// configuration is its own subject, and burying it under General made
			// DSH's general preferences and the shell's look like one list.
			//
			// No `renderSlot` passthrough here: this section declares no child
			// slots, so there is nothing to pass through. The General section's
			// child slot is no longer ours.
			return jsx.jsx('div', {
				className: 'dsh-shell-section',
				children: [jsx.jsx(ConfigSection, {}, 'config'), UpdateRow()],
			})
		}

		/**
		 * The version and upgrade row.
		 *
		 * @returns the row element tree.
		 */
		function UpdateRow() {
			const [status, setStatus] = react.useState(null)
			const [busy, setBusy] = react.useState(false)
			const [error, setError] = react.useState(null)

			// Read the published status once on mount. The shell checks on its own
			// at launch, so there is usually something to show without asking.
			react.useEffect(() => {
				let live = true
				call('status', 'GET').then((reply) => {
					if (!live) return
					if (reply.ok) setStatus(reply.status ?? null)
					else setError(reply.error)
				})
				return () => {
					live = false
				}
			}, [])

			const check = async () => {
				setBusy(true)
				setError(null)
				const reply = await call('check', 'POST')
				if (reply.ok) setStatus(reply.status ?? null)
				else setError(reply.error)
				setBusy(false)
			}

			const install = async () => {
				setBusy(true)
				setError(null)
				// The reply means "started", not "finished": the install replaces
				// the host serving this page. Poll for the phase.
				const reply = await call('install', 'POST')
				if (!reply.ok) {
					setError(reply.error)
					setBusy(false)
					return
				}
				for (let attempt = 0; attempt < 60; attempt += 1) {
					await new Promise((resolve) => setTimeout(resolve, 1000))
					const polled = await call('status', 'GET')
					if (!polled.ok) continue
					setStatus(polled.status ?? null)
					const phase = polled.status?.phase
					if (phase !== 'working' && phase !== 'checking') break
				}
				setBusy(false)
			}

			const phase = status?.phase ?? 'unknown'
			const current = status?.current ?? t('unknown')
			const target = status?.target
			const channel = status?.channel
			const working = busy || phase === 'working' || phase === 'checking'
			const canInstall = !working && phase === 'available' && target

			let message = t('notChecked')
			let tone = 'muted'
			if (error) {
				message = t('failed', { reason: error })
				tone = 'error'
			} else if (phase === 'checking') message = t('checking')
			else if (phase === 'working') message = t('working')
			else if (phase === 'current') message = t('upToDate')
			else if (phase === 'available') message = t('available', { target })
			else if (phase === 'restartRequired') message = t('restart', { target })
			else if (phase === 'failed') {
				message = t('failed', { reason: status?.message ?? '' })
				tone = 'error'
			} else if (phase === 'unsupported') {
				message = t('unsupported')
				tone = 'error'
			}

			const children = [
				jsx.jsxs('div', {
					className: 'dsh-shell-update-row',
					children: [
						jsx.jsx('div', {
							className: 'dsh-shell-update-label',
							children: t('title'),
						}),
						jsx.jsxs('div', {
							className: 'dsh-shell-update-body',
							children: [
								jsx.jsxs('div', {
									className: 'dsh-shell-update-versions',
									children: [
										jsx.jsx('code', { children: current }),
										channel
											? jsx.jsx('span', {
													className: 'dsh-shell-update-channel',
													children: t('channel', { channel }),
												})
											: null,
									],
								}),
								jsx.jsx('div', {
									className: `dsh-shell-update-message is-${tone}`,
									children: message,
								}),
								jsx.jsxs('div', {
									className: 'dsh-shell-update-actions',
									children: [
										jsx.jsx('button', {
											type: 'button',
											disabled: working,
											onClick: check,
											children: working ? t('checking') : t('checkNow'),
										}),
										canInstall
											? jsx.jsx('button', {
													type: 'button',
													className: 'primary',
													disabled: working,
													onClick: install,
													children: t('upgrade', { target }),
												})
											: null,
									],
								}),
							],
						}),
					],
				}),
			]

			return jsx.jsx('div', { children })
		}

		/** Required services: the UI slot registry and the locale runtime. */
		const inject = ['slots', 'locale']

		/**
		 * A few rules for the row.
		 *
		 * Injected rather than bundled as a module, because this bundle is
		 * hand-written and a CSS module would need the build step this plugin
		 * deliberately does not have. Colours come from the app's own custom
		 * properties so the row follows the theme like everything around it.
		 */
		const CSS = `
.dsh-shell-section { display: flex; flex-direction: column; gap: 22px; max-width: 720px; }
.dsh-shell-subhead { font-weight: 600; margin-bottom: 6px; }
.dsh-shell-config { display: flex; flex-direction: column; gap: 16px; }
.dsh-shell-config-head { border-bottom: 1px solid var(--dsh-border, rgba(128,128,128,.2)); padding-bottom: 8px; }
.dsh-shell-field { display: flex; flex-direction: column; gap: 6px; }
.dsh-shell-field-label { font-weight: 500; }
.dsh-shell-field-body { display: flex; flex-direction: column; gap: 4px; min-width: 0; }
.dsh-shell-field input[type=text], .dsh-shell-field textarea {
  font: inherit; width: 100%; box-sizing: border-box; padding: 5px 8px;
  border-radius: 6px; border: 1px solid var(--dsh-border, rgba(128,128,128,.35));
  background: var(--dsh-surface, transparent); color: inherit;
}
.dsh-shell-field textarea { font-family: ui-monospace, SFMono-Regular, monospace; font-size: 12px; resize: vertical; }
.dsh-shell-palettes { display: flex; gap: 24px; flex-wrap: wrap; }
.dsh-shell-palette { flex: 1 1 240px; min-width: 200px; }
.dsh-shell-colour { display: flex; align-items: center; gap: 8px; }
.dsh-shell-colour input[type=color] { width: 32px; height: 22px; padding: 0; border: 1px solid var(--dsh-border, rgba(128,128,128,.35)); border-radius: 4px; background: none; }
.dsh-shell-colour code { font-size: 11px; opacity: .7; }
.dsh-shell-config-actions { display: flex; align-items: center; gap: 8px; }
.dsh-shell-update-row { display: flex; flex-direction: column; gap: 6px; padding-top: 16px; border-top: 1px solid var(--dsh-border, rgba(128,128,128,.2)); }
.dsh-shell-update-label { font-weight: 500; }
.dsh-shell-update-body { display: flex; flex-direction: column; gap: 6px; min-width: 0; }
.dsh-shell-update-versions { display: flex; align-items: center; gap: 10px; }
.dsh-shell-update-versions code { font-size: 12px; }
.dsh-shell-update-channel { font-size: 11px; opacity: .6; }
.dsh-shell-update-message { font-size: 12px; opacity: .8; }
.dsh-shell-update-message.is-error { color: var(--dsh-danger, #d9534f); opacity: 1; }
.dsh-shell-update-actions { display: flex; gap: 8px; }
`

		/** Install the stylesheet once, however many times a section mounts. */
		function installStyles() {
			const id = 'dsh-shell-update-styles'
			if (typeof document === 'undefined') return
			if (document.getElementById(id) !== null) return
			const style = document.createElement('style')
			style.id = id
			style.textContent = CSS
			document.head.appendChild(style)
		}

		/**
		 * Register the section once the General section's child slot exists.
		 *
		 * @param ctx - client root context.
		 */
		function apply(ctx) {
			installStyles()
			ctx.effect(
				() => ctx.locale.register(NS, { zh, en }),
				'shell-bridge: update dictionaries',
			)
			t = ctx.locale.bind(NS)

			// The settings dialog declares `settings.section` as a list, so
			// registering here adds a navigation entry of our own — which is what
			// this configuration deserves, and how the official sections appear.
			//
			// No icon is declared: the dialog picks one by section id and falls
			// back to a settings cog for an id it does not know. Naming one of the
			// official icons would mean importing a platform seed whose exports
			// this bundle cannot verify, and a wrong name fails the whole page.
			ctx.slots.inject('settings.section', () =>
				ctx.slots.register(
					{
						name: 'settings.section',
						id: 'shell',
						order: 20,
						label: () => t('section.title'),
					},
					ShellSettingsSection,
				),
			)
		}

		exports.apply = apply
		exports.inject = inject
		exports.ShellSettingsSection = ShellSettingsSection
		// Exported for the test that executes this bundle: the diff decides which
		// fields a save sends, and sending the wrong ones silently reverts a
		// setting the form never showed.
		exports.diff = diff
		exports.problem = problem
		return module.exports
	},
})
