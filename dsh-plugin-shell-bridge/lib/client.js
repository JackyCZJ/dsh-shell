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
		}

		/**
		 * Call the host route.
		 *
		 * @param {string} action - `status`, `check` or `install`.
		 * @param {string} method - HTTP method.
		 * @returns {Promise<object>} the parsed body, or an `{ok:false}` shape.
		 */
		async function call(action, method) {
			try {
				const response = await fetch(`${ROUTE}/${action}`, { method })
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

		/**
		 * The Updates row.
		 *
		 * Rendered inside DSH's own General settings section, so it looks like
		 * every other setting. `renderSlot` is supplied by the slot host.
		 *
		 * @param props - composed slot props.
		 * @returns the row element tree.
		 */
		function UpdateSection({ renderSlot }) {
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

			if (typeof renderSlot === 'function') {
				children.push(renderSlot('settings.general.item', {}))
			}
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
.dsh-shell-update-row { display: flex; gap: 16px; padding: 12px 0; border-bottom: 1px solid var(--dsh-border, rgba(128,128,128,.2)); }
.dsh-shell-update-label { flex: 0 0 148px; }
.dsh-shell-update-body { flex: 1; display: flex; flex-direction: column; gap: 6px; min-width: 0; }
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

			// `settings.general.item` is declared by the General section as a
			// list slot, so registering into it appends a row there rather than
			// claiming a section of our own — which is what the official
			// settings plugins do, and what makes this look native.
			ctx.slots.inject('settings.general.item', () =>
				ctx.slots.register({ name: 'settings.general.item', id: 'shell-updates' }, UpdateSection),
			)
		}

		exports.apply = apply
		exports.inject = inject
		exports.UpdateSection = UpdateSection
		return module.exports
	},
})
