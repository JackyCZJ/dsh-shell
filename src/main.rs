//! A native desktop shell for the DeepSeek Harness web UI.
//!
//! The window has no system titlebar. The content fills the frame edge to edge,
//! and the traffic lights are inset so they float over the page — the same shape
//! as the upstream Electron desktop app, without shipping Chromium.
//!
//! The webview is the OS's own engine: WebKit on macOS, WebView2 on Windows,
//! WebKitGTK on Linux. Nothing is bundled, so the binary stays small and the
//! renderer is patched by the OS.
//!
//! Theme edits apply live to both the native chrome and the page itself.

mod bridge;
mod i18n;
mod instance;
mod logfile;
mod menu;
mod native;
mod runtime;
mod server;
mod settings;
mod theme;
mod updater;
mod window_state;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use tao::dpi::LogicalSize;
use tao::event::{Event, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopBuilder};
use tao::platform::macos::{WindowBuilderExtMacOS, WindowExtMacOS};
use tao::window::WindowBuilder;
use theme::{Theme, ThemeSource};

/// The boot screen shown while the host starts.
///
/// Compiled in rather than read from disk so it cannot go missing in a packaged
/// build, and so the first paint needs no file access.
const BOOT_HTML: &str = include_str!("../assets/boot.html");

/// The boot page with the shell's language baked in.
///
/// Substituted before the page's own script runs, so the first paint is already
/// in the right language rather than flashing English.
fn boot_page(locale: crate::i18n::Locale) -> String {
    BOOT_HTML.replace("{locale}", &format!("{:?}", locale.id()))
}

/// How often to wake the event loop to drain background channels.
///
/// This is the latency upper bound for tray and notification updates. 100ms is
/// imperceptible to a user and costs nothing measurable when idle.
const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(100);

/// IPC message the injected drag region posts to start a window move.
///
/// The string is namespaced because the same channel carries any future
/// shell-to-host messages.
const DRAG_MESSAGE: &str = "dsh-shell:drag";

/// IPC message for the double-click-to-zoom action on the drag strip.
const ZOOM_MESSAGE: &str = "dsh-shell:zoom";

/// Sent once the drag region is live, so the shell can confirm the window is
/// movable instead of leaving a silent dead zone if injection failed.
const READY_MESSAGE: &str = "dsh-shell:drag-ready";

/// The shortest gap between two window-geometry writes.
///
/// Dragging or resizing produces an event per frame, and each save replaces a
/// file. Half a second keeps the file close to current without turning a drag
/// into hundreds of writes.
const GEOMETRY_WRITE_INTERVAL: std::time::Duration = std::time::Duration::from_millis(500);

/// Install the diagnostics sinks.
///
/// Two of them, deliberately filtered differently:
///
///  * stdout, at `info`, for whoever launched the shell from a terminal;
///  * a file under DSH's home, at `debug`, for everyone else.
///
/// The file is the one that matters in practice. A Finder-launched app has no
/// terminal, so its stdout is discarded — which is how "the badge never
/// appears" stayed unexplained: the shell was recording exactly the answer and
/// nobody could read it. Keeping the file chattier than the terminal is the
/// point, not an oversight.
fn init_tracing() {
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;
    use tracing_subscriber::EnvFilter;
    // For `Layer::with_filter`, which is how each sink gets its own level.
    use tracing_subscriber::Layer;

    // `RUST_LOG` keeps working for a terminal run.
    let stdout_filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    let registry = tracing_subscriber::registry()
        .with(tracing_subscriber::fmt::layer().with_filter(stdout_filter));

    let Some(path) = logfile::path() else {
        registry.init();
        return;
    };
    let Some(file) = logfile::open(&path) else {
        registry.init();
        return;
    };

    // `dsh` is the target the host's captured stdout is logged under, so a
    // plugin's own `console.log` lands in the file too.
    let file_filter = std::env::var("DSH_SHELL_LOG_LEVEL")
        .ok()
        .and_then(|level| EnvFilter::try_new(level).ok())
        .unwrap_or_else(|| EnvFilter::new("info,dsh_shell=debug,dsh=debug"));

    registry
        .with(
            tracing_subscriber::fmt::layer()
                // Escape sequences would be noise in a file nobody pages with.
                .with_ansi(false)
                .with_writer(std::sync::Mutex::new(file))
                .with_filter(file_filter),
        )
        .init();

    tracing::info!(log = %path.display(), "diagnostics are being written to this file");
}

/// Show the unread-turn count everywhere it appears.
///
/// The Dock tile and the menu bar carry the same fact, so they are always
/// written together. Setting one and not the other reads as a bug, and leaves
/// the user unable to tell which of the two is stale. A count of zero clears
/// both, so "clear" has exactly one spelling.
fn apply_unseen(window: &tao::window::Window, tray: &mut Option<native::Tray>, unseen: usize) {
    native::set_dock_badge(window, Some(unseen));
    if let Some(active) = tray.as_mut() {
        active.set_badge(Some(unseen));
    }
}

fn main() {
    init_tracing();

    // One shell per user session, claimed before anything else is started.
    //
    // The order matters: a second shell must not reach the point of launching
    // its own host or binding the bridge socket, because that is what would
    // leave two hosts running and the two processes fighting over one socket
    // path. So the slot is claimed first and everything below assumes it.
    let (instance_activations, _instance_guard) =
        match instance::claim(&runtime::lock_path(), &runtime::activate_path()) {
            Ok(instance::Claim::Owned { guard, activations }) => (activations, guard),
            Ok(instance::Claim::HandedOff) => {
                tracing::info!("another shell is already running; asked it to show its window");
                return;
            }
            Err(err) => {
                // Refusing to start is the safe failure: starting anyway risks
                // exactly the two-shell collision the lock exists to prevent.
                tracing::error!(%err, "could not claim the single-instance slot");
                std::process::exit(1);
            }
        };

    let dsh_program = std::env::var("DSH_BIN").unwrap_or_else(|_| "dsh".to_string());
    // Resolved once, here, so the host the shell launches and the install an
    // upgrade would replace cannot drift apart; a second resolution inside the
    // server could pick a different launcher and leave the upgrade rewriting a
    // tree nobody is running.
    let launcher = std::path::PathBuf::from(server::resolve_launcher(&dsh_program));
    tracing::info!(launcher = %launcher.display(), "resolved the DSH launcher");
    // The update workers read it from here; see `launcher_for_updates`.
    let _ = UPDATE_LAUNCHER.set(launcher.clone());
    let port: u16 = std::env::var("DSH_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(0);
    // Config lives in DSH's own settings document, under the `dsh-shell`
    // namespace the Host plugin registers. There is no shell-owned config file.
    let settings_file = theme::settings_path().unwrap_or_else(|| {
        tracing::warn!("no DSH home found; using built-in defaults");
        PathBuf::from("settings.yaml")
    });
    tracing::info!(settings = %settings_file.display(), "reading configuration");
    let theme_source = ThemeSource::new(settings_file);

    // Start `dsh web` on a worker runtime so the UI thread is never blocked
    // waiting for the host to boot.
    let (url_tx, url_rx) = std::sync::mpsc::channel::<Result<server::DshServer, String>>();
    spawn_host(url_tx, launcher.clone(), port);

    // --- Native capabilities ---------------------------------------------
    //
    // Created before the window so an early agent event is not missed. Both are
    // optional: a failure here is logged and the shell continues.

    // The bridge listener runs on its own thread; events are funnelled to the
    // UI thread over a channel, because tray APIs are main-thread-only.
    let (bridge_tx, bridge_rx) = std::sync::mpsc::channel::<bridge::BridgeEvent>();
    // Requests a plugin makes of the shell are forwarded to the UI thread for
    // the same reason events are: they touch main-thread-only objects.
    let (request_tx, request_rx) = std::sync::mpsc::channel::<bridge::ShellRequest>();
    let request_tx_for_handler = request_tx.clone();
    // The shell pushes settings writes through this link, so DSH owns
    // persistence and its writer preserves the rest of settings.yaml.
    let plugin_link = bridge::PluginLink::default();
    let link_for_listener = plugin_link.clone();
    if let Err(err) = bridge::listen(
        link_for_listener,
        move |event| {
            let _ = bridge_tx.send(event);
        },
        move |request| {
            // The UI thread answers; the listener thread only carries the reply
            // back. A closed channel means the shell is shutting down.
            request_tx_for_handler
                .send(request)
                .map_err(|_| "shell is shutting down".to_string())
        },
    ) {
        tracing::warn!(%err, "bridge unavailable; tray and notifications will stay idle");
    }

    let mut tray = match native::Tray::new(theme_source.locale()) {
        Ok(tray) => Some(tray),
        Err(err) => {
            tracing::warn!(%err, "tray unavailable; continuing without it");
            None
        }
    };
    let tray_menu_rx = tray_icon::menu::MenuEvent::receiver().clone();

    // Global summon shortcut. Optional: if another app owns the combination the
    // shell still runs, it just cannot be summoned while hidden.
    let mut hotkey = native::register_hotkey(initial_hotkey_spec(&theme_source));
    let hotkey_rx = global_hotkey::GlobalHotKeyEvent::receiver().clone();

    // The application menu.
    //
    // Installed before the window so the Edit items exist from the first frame.
    // Without them macOS has nothing to bind Cmd+C/V/X/A to and the standard
    // clipboard shortcuts silently do nothing — the web view implements them,
    // but the system only routes them through the menu bar.
    let app_menu = menu::build("DSH Shell", theme_source.locale());
    #[cfg(target_os = "macos")]
    app_menu.init_for_nsapp();
    #[cfg(not(target_os = "macos"))]
    {
        // Other platforms attach the menu to a window; there is no app-global
        // menu bar. Left for when those targets are actually exercised.
    }
    let menu_rx = muda::MenuEvent::receiver().clone();

    let event_loop = EventLoopBuilder::new().build();
    let theme_for_window = theme_source.current();

    // Where the window was last time. Resolved once: the paths do not change
    // while the shell runs, and reading the environment per write would be both
    // slower and surprising.
    let state_path = window_state::path();
    let saved = match state_path.as_deref() {
        Some(path) => window_state::load(path),
        None => window_state::WindowState::default(),
    };

    // A saved position is only worth restoring if it still lands on a display
    // the user has. Undocking a laptop would otherwise open the window on a
    // monitor that is no longer attached, where it cannot be reached.
    let monitors = logical_monitors(&event_loop);
    let restored = if saved.is_reachable(&monitors) {
        saved.clamped_to(&monitors)
    } else {
        tracing::info!("saved window position is off-screen; centring instead");
        window_state::WindowState::default()
    };

    let mut builder = WindowBuilder::new()
        .with_title("DeepSeek Harness")
        .with_inner_size(LogicalSize::new(restored.width, restored.height))
        .with_min_inner_size(LogicalSize::new(
            window_state::MIN_WIDTH,
            window_state::MIN_HEIGHT,
        ));

    if let (Some(x), Some(y)) = (restored.x, restored.y) {
        builder = builder.with_position(tao::dpi::LogicalPosition::new(x, y));
    }

    // No system titlebar: transparent and hidden, with the content running the
    // full height of the frame. The traffic lights stay visible so the window is
    // still closable and resizable via the standard affordances.
    #[cfg(target_os = "macos")]
    {
        builder = builder
            .with_titlebar_transparent(true)
            .with_title_hidden(true)
            .with_fullsize_content_view(true)
            .with_traffic_light_inset(tao::dpi::LogicalPosition::new(
                theme::TRAFFIC_LIGHT_INSET.0,
                theme::TRAFFIC_LIGHT_INSET.1,
            ));
    }

    let window = builder.build(&event_loop).expect("build window");

    // Measured before anything can resize the window, so `inner_size` and
    // `outer_size` agree and their difference is the real border.
    let chrome = WindowChrome::measure(&window);
    tracing::debug!(?chrome, "measured the window border");

    // Maximizing after the build rather than through the builder keeps the
    // restore path identical to the user pressing the zoom button.
    if restored.maximized {
        window.set_maximized(true);
    }

    // With the system titlebar hidden there is no OS drag region left, so the
    // page itself has to ask the window to move. A thin strip along the top is
    // marked draggable in CSS; pointerdown there posts an IPC message, and the
    // handler below turns it into an actual window drag.
    //
    // The handler runs on the main thread, which is what makes calling
    // `drag_window()` here sound. A background thread could not do this.
    let window = std::rc::Rc::new(window);
    let window_weak = std::rc::Rc::downgrade(&window);

    // The webview is created before the URL is known so the window can paint
    // its theme background immediately; it navigates once the host is ready.
    let webview = wry::WebViewBuilder::new()
        .with_background_color(hex_to_rgba(
            theme_for_window.palette_for(resolved_dark(&theme_source)).background.0,
        ))
        .with_initialization_script(&inject_script(
            &theme_for_window,
            resolved_dark(&theme_source),
        ))
        .with_ipc_handler(move |request| {
            let Some(window) = window_weak.upgrade() else {
                return;
            };
            // Unknown messages are ignored rather than guessed at, so adding a
            // future shell message cannot accidentally move the window.
            match request.body().as_str() {
                DRAG_MESSAGE => {
                    if let Err(err) = window.drag_window() {
                        tracing::warn!(%err, "window drag failed");
                    }
                }
                ZOOM_MESSAGE => {
                    // Match the platform titlebar gesture.
                    window.set_maximized(!window.is_maximized());
                }
                READY_MESSAGE => {
                    tracing::info!("drag region ready; window is movable from the caption strip")
                }
                other => tracing::debug!(message = %other, "ignoring unknown shell IPC message"),
            }
        })
        .with_html(boot_page(theme_source.locale()))
        .build(&*window)
        .expect("build webview");

    let webview = Arc::new(Mutex::new(webview));

    // Paint the boot screen in the right appearance immediately.
    if let Ok(wv) = webview.lock() {
        let script = format!(
            "window.__dshBoot && window.__dshBoot.setAppearance({});",
            resolved_dark(&theme_source)
        );
        let _ = wv.evaluate_script(&script);
    }

    // Theme hot reload.
    //
    // `wry::WebView` is !Send because it wraps main-thread-only AppKit objects,
    // so the watcher thread must not touch it. The watcher publishes the new
    // theme over a channel and the event loop applies it on the UI thread.
    let (theme_tx, theme_rx) = std::sync::mpsc::channel::<Arc<Theme>>();
    let _theme_watcher = theme::spawn_watcher(theme_source.clone_for_watcher(), move |theme| {
        let _ = theme_tx.send(theme);
    });

    // Poll for the server URL from the UI loop; the launcher thread only sends
    // once, so a disconnected channel simply means "already handled".
    let mut navigated = false;
    let mut server: Option<server::DshServer> = None;

    // Remembered window geometry. `last_geometry` is what has actually been
    // written; a resize or move only marks it dirty, so a drag costs one write
    // rather than one per frame.
    // Whether the notification permission has been asked for yet. One shot:
    // the system remembers the answer.
    let mut asked_for_notifications = false;
    let mut asked_for_updates = false;

    // Finished turns the user has not looked at yet, shown as a Dock badge and
    // as a number beside the tray icon.
    //
    // The window is assumed to open in front, so this starts settled rather
    // than badging work nobody has missed. `was_watched` is the same assumption
    // for the other direction: it is what "the user came back" is measured
    // against.
    let mut unseen: usize = 0;
    let mut window_focused = true;
    let mut was_watched = true;

    let mut last_geometry = restored;
    let mut geometry_dirty = false;
    let mut geometry_written_at = std::time::Instant::now();

    let main_window_id = window.id();
    // The settings window is created on demand and dropped when closed.
    let mut settings_window: Option<SettingsWindow> = None;
    // A save request handed from the settings page to the UI thread.
    let (settings_tx, settings_rx) =
        std::sync::mpsc::channel::<(String, settings::SettingsRequest)>();
    // Save outcomes come back from the worker thread that talked to the Host.
    let (save_tx, save_rx) = std::sync::mpsc::channel::<(bool, Option<String>)>();

    // --- DSH upgrades ---------------------------------------------------
    //
    // Detection is automatic and cached; application is always a response to a
    // click. The status is kept here, on the UI thread, because it is rendered
    // by the settings page and the tray.
    let update_channel = updater::Channel::parse(&theme_source.current().update_channel)
        .unwrap_or_default();
    let mut update_status = updater::Status {
        channel: update_channel,
        ..updater::Status::default()
    };
    // Checks and installs run on a worker: a registry fetch and a 280 MB install
    // must never block the UI thread.
    let (update_tx, update_rx) = std::sync::mpsc::channel::<updater::Status>();

    event_loop.run(move |event, event_loop, control_flow| {
        // The Rc owns the window; `&*window` yields the `&Window` the APIs want.
        let window = &*window;

        // Poll on a timer rather than blocking indefinitely.
        //
        // Bridge and tray events arrive on background threads and cannot wake
        // this loop. With `ControlFlow::Wait` they would sit unread until some
        // unrelated input event happened to tick the loop — so notifications
        // and tray updates appeared only when the user moved the mouse. A short
        // poll interval makes them land promptly at negligible cost.
        *control_flow =
            ControlFlow::WaitUntil(std::time::Instant::now() + POLL_INTERVAL);

        // Apply any theme reloads queued by the watcher thread.
        let mut applied_theme = false;
        while let Ok(theme) = theme_rx.try_recv() {
            if let Ok(wv) = webview.lock() {
                if let Err(err) = wv.evaluate_script(&rewrite_style_script(&theme, resolved_dark(&theme_source))) {
                    tracing::warn!(%err, "could not apply theme to page");
                }
            }

            // Hotkey changes ride the same reload.
            update_hotkey(&mut hotkey, &theme);
            // `tao::window::RGBA` is a plain (r, g, b, a) tuple, which is what
            // `hex_to_rgba` already produces.
            window.set_background_color(Some(hex_to_rgba(
                theme.palette_for(resolved_dark(&theme_source)).background.0,
            )));
            #[cfg(target_os = "macos")]
            window.set_traffic_light_inset(tao::dpi::LogicalPosition::new(
                theme::TRAFFIC_LIGHT_INSET.0,
                theme::TRAFFIC_LIGHT_INSET.1,
            ));
            applied_theme = true;
        }
        if applied_theme {
            window.request_redraw();
        }

        // Ask for notification permission once the loop is running.
        //
        // One shot: the system remembers the answer. Keyed on the first
        // iteration rather than on a particular `StartCause`, so it does not
        // depend on which event tao happens to deliver first. The loop only
        // starts after AppKit has finished launching, which is the timing the
        // API needs — asking before `run()` is what produced
        // `notificationsNotAllowed`.
        if !asked_for_notifications {
            asked_for_notifications = true;
            native::prepare_notifications();
        }

        // One automatic update check per launch, off the UI thread.
        //
        // Not `force`, so this only reaches the network when the cached answer
        // has aged past `CHECK_INTERVAL`; otherwise it is a file read. That is
        // what keeps a launch from ever waiting on the registry, and it is the
        // whole of the automatic behaviour — nothing is installed without a
        // click, because this project publishes release candidates.
        if !asked_for_updates {
            asked_for_updates = true;
            request_update_check(&update_tx, &mut update_status, false);
        }

        // --- Second launch handed off to us -------------------------------
        //
        // The lock made the new launch exit; this is the other half of it. The
        // window is raised rather than merely focused, because the request
        // usually comes from someone who cannot see the app at all.
        while instance_activations.try_recv().is_ok() {
            summon(&window);
            tracing::info!("a second launch handed off; window shown");
        }

        // --- Tray events --------------------------------------------------
        //
        // Drained here rather than on the tray's own thread because window
        // operations (show, focus) must happen on the main thread.
        while let Ok(event) = tray_menu_rx.try_recv() {
            if let Some(active) = tray.as_ref() {
                match native::tray_command(&event, active) {
                    Some(native::TrayCommand::Show) => summon(&window),
                    Some(native::TrayCommand::Settings) => {
                        open_settings(
                            &mut settings_window,
                            event_loop,
                            &theme_source,
                            &plugin_link,
                            &settings_tx,
                            &update_status,
                        );
                    }
                    Some(native::TrayCommand::CheckForUpdates) => {
                        request_update_check(&update_tx, &mut update_status, true);
                    }
                    Some(native::TrayCommand::Quit) => {
                        shutdown_server(&mut server);
                        runtime::cleanup();
                        *control_flow = ControlFlow::Exit;
                        return;
                    }
                    None => {}
                }
            }
        }

        // --- Application menu ---------------------------------------------
        //
        // Only the shell-owned items arrive here; cut/copy/paste/select-all are
        // handled by the platform through the responder chain and never surface
        // as events.
        while let Ok(event) = menu_rx.try_recv() {
            match event.id().0.as_str() {
                menu::ID_QUIT => {
                    tracing::info!("quit from the application menu");
                    shutdown_server(&mut server);
                    runtime::cleanup();
                    *control_flow = ControlFlow::Exit;
                    return;
                }
                menu::ID_RELOAD => {
                    // Reload the page, not the whole app: the host and session
                    // survive, which is what makes this useful after a UI stall.
                    if let Ok(wv) = webview.lock() {
                        if let Err(err) = wv.evaluate_script("window.location.reload()") {
                            tracing::warn!(%err, "reload failed");
                        }
                    }
                }
                menu::ID_SHOW => {
                    // "Close Window" hides to the tray rather than destroying
                    // the window, so the session survives.
                    window.set_visible(false);
                    tracing::info!("window hidden from the menu");
                }
                other => tracing::debug!(id = %other, "unhandled menu event"),
            }
        }

        // --- Global hotkey ------------------------------------------------
        //
        // Drain the hotkey channel on the UI thread: raising the window is a
        // main-thread operation.
        while let Ok(event) = hotkey_rx.try_recv() {
            if let Some(active) = hotkey.as_ref() {
                if native::is_summon_event(&event, active) {
                    // Summon rather than merely focus: the point of the shortcut
                    // is to reach the app when it is hidden behind others.
                    summon(&window);
                    tracing::info!("summoned by global hotkey");
                }
            }
        }

        // --- Update status -------------------------------------------------
        //
        // Arrives from the worker that checked the registry or staged an
        // install. Stored here and pushed to the settings page and the tray
        // menu, both of which are main-thread-only.
        while let Ok(status) = update_rx.try_recv() {
            update_status = status;
            push_update_status(&settings_window, &update_status);
            if let Some(active) = tray.as_mut() {
                active.set_update(&update_status);
            }
        }

        // --- Settings saves ------------------------------------------------
        //
        // Handled on the UI thread: a successful save reloads the theme, which
        // repaints the native window.
        while let Ok((_body, request)) = settings_rx.try_recv() {
            match request {
                settings::SettingsRequest::Save { config } => {
                    // Validate here so an obviously wrong entry is reported
                    // immediately; DSH validates again when it persists.
                    match settings::validate(&config) {
                        Err(err) => {
                            if let Some(active) = settings_window.as_ref() {
                                let outcome = settings::SaveOutcome {
                                    ok: false,
                                    error: Some(err),
                                };
                                let _ = active.webview.evaluate_script(&outcome.to_script());
                            }
                        }
                        Ok(theme) => {
                            // The write blocks on the Host's reply, so it runs on
                            // a worker and reports back through a channel.
                            let link = plugin_link.clone();
                            let tx = save_tx.clone();
                            std::thread::spawn(move || {
                                let payload = serde_json::json!({ "config": theme });
                                let outcome = link
                                    .call(
                                        "setConfig",
                                        payload,
                                        std::time::Duration::from_secs(10),
                                    )
                                    .map(|(ok, error)| (ok, error))
                                    .unwrap_or_else(|err| (false, Some(err)));
                                let _ = tx.send(outcome);
                            });
                        }
                    }
                }
                settings::SettingsRequest::CheckUpdate => {
                    request_update_check(&update_tx, &mut update_status, true);
                }
                settings::SettingsRequest::InstallUpdate => {
                    request_update_install(
                        &update_tx,
                        &mut update_status,
                        launcher.clone(),
                        &theme_source,
                    );
                }
            }
        }

        // --- Settings save outcomes ----------------------------------------
        //
        // Reported on the UI thread, then the page is told and the theme is
        // re-read so the form shows what DSH actually persisted.
        while let Ok((ok, error)) = save_rx.try_recv() {
            let outcome = settings::SaveOutcome { ok, error };
            if let Some(active) = settings_window.as_ref() {
                let _ = active.webview.evaluate_script(&outcome.to_script());
            }
            if ok {
                // DSH writes the document, so pick the canonical value up from it.
                if let Some(theme) = theme_source.reload() {
                    if let Ok(wv) = webview.lock() {
                        let _ = wv.evaluate_script(&rewrite_style_script(&theme, resolved_dark(&theme_source)));
                    }
                    if let Some(active) = settings_window.as_ref() {
                        let script = format!(
                            "window.__dshSettings && window.__dshSettings.update({});",
                            serde_json::to_string(&*theme).unwrap_or_else(|_| "{}".into())
                        );
                        let _ = active.webview.evaluate_script(&script);
                    }
                    apply_theme_to_chrome(&window, &theme, resolved_dark(&theme_source));
                }
            }
        }

        // --- Shell requests from plugins ----------------------------------
        //
        // A plugin asking the shell to do something. Each maps onto a native
        // action the UI thread owns.
        while let Ok(request) = request_rx.try_recv() {
            match request {
                bridge::ShellRequest::Notify { title, body } => {
                    // A plugin-supplied title is its own copy and is passed
                    // through untouched; only the fallback is the shell's.
                    native::notify(
                        theme_source.locale(),
                        title.as_deref().unwrap_or("DSH"),
                        &body,
                    );
                }
                bridge::ShellRequest::FocusWindow => summon(&window),
                bridge::ShellRequest::HideWindow => {
                    window.set_visible(false);
                }
                bridge::ShellRequest::SetStatusLabel { text } => {
                    // Only the label is plugin-controlled; the icon colour stays
                    // derived from real agent state so the tray cannot lie.
                    match text {
                        Some(text) => {
                            if let Some(active) = tray.as_mut() {
                                active.set_plugin_label(Some(text));
                            }
                        }
                        None => {
                            if let Some(active) = tray.as_mut() {
                                active.set_plugin_label(None);
                            }
                        }
                    }
                }
            }
        }

        // --- Bridge events ------------------------------------------------
        //
        // Drive the tray state and post notifications. Notifications are
        // limited to events a user actually wants interrupting; see
        // `BridgeEvent::wants_notification`.
        while let Ok(event) = bridge_rx.try_recv() {
            // A settings push means DSH persisted a change. Apply it, then tell
            // the settings page so an open form reflects what was stored.
            if let Some(theme) = event.as_theme() {
                match theme_source.apply_pushed(theme) {
                    Some(theme) => {
                        tracing::info!("settings pushed by the host");
                        if let Ok(wv) = webview.lock() {
                            let _ = wv.evaluate_script(&rewrite_style_script(&theme, resolved_dark(&theme_source)));
                        }
                        if let Some(active) = settings_window.as_ref() {
                            let script = format!(
                                "window.__dshSettings && window.__dshSettings.update({});",
                                serde_json::to_string(&*theme).unwrap_or_else(|_| "{}".into())
                            );
                            let _ = active.webview.evaluate_script(&script);
                        }
                        apply_theme_to_chrome(&window, &theme, resolved_dark(&theme_source));
                        update_hotkey(&mut hotkey, &theme);
                    }
                    None => tracing::debug!("pushed settings matched the current theme"),
                }
                continue;
            }

            tracing::info!(kind = %event.kind, summary = %event.summary(), "bridge event");
            if let Some(state) = native::AgentState::from_event(&event.kind, event.status.as_deref())
            {
                if let Some(active) = tray.as_mut() {
                    active.set_state(state);
                }
            }
            if event.wants_notification() {
                // Name the session when the host told us which one, so a
                // notification is actionable with several sessions open.
                let session = event
                    .session_id
                    .as_deref()
                    .map(short_session)
                    .unwrap_or_else(|| "session".to_string());
                let t = theme_source.locale().strings();
                let (title, body) = match event.kind.as_str() {
                    "request-error" => (
                        i18n::fill(t.notify_failed, "session", &session),
                        event
                            .message
                            .clone()
                            .unwrap_or_else(|| t.notify_errored.to_string()),
                    ),
                    _ => (
                        i18n::fill(t.notify_finished, "session", &session),
                        // Prefer an explicit reason ("end_turn", "cancelled")
                        // over a generic message: it tells the user whether the
                        // agent completed or stopped early.
                        event
                            .message
                            .clone()
                            .or_else(|| event.reason.clone())
                            .map(|r| i18n::fill(t.notify_stopped, "reason", &r))
                            .unwrap_or_else(|| t.notify_done.to_string()),
                    ),
                };
                native::notify(theme_source.locale(), &title, &body);

                // A turn that finishes while the user is looking at the window
                // needs no badge; one that finishes behind something else does.
                // The notification is posted either way — it is what carries
                // *what* happened, and it is what the user asked to be told.
                //
                // "Looking at it" is asked of AppKit rather than read from the
                // tracked focus flag: that flag starts as an assumption and only
                // moves when tao delivers a focus transition, so a window that
                // is never made key leaves it stuck at `true` and the badge
                // never appears. See `native::app_is_active`.
                //
                // The decision and its inputs are logged: whether a badge
                // appears depends on state the user cannot see, which makes
                // "nothing happened" impossible to act on otherwise.
                let visible = window.is_visible();
                let active = native::app_is_active().unwrap_or(window_focused);
                let watched = active && visible;
                if !watched {
                    unseen += 1;
                    apply_unseen(&window, &mut tray, unseen);
                }
                tracing::info!(
                    unseen,
                    active,
                    visible,
                    watched,
                    tracked_focus = window_focused,
                    "a turn finished"
                );
            }
        }

        if !navigated {
            match url_rx.try_recv() {
                Ok(Ok(srv)) => {
                    if let Ok(wv) = webview.lock() {
                        if let Err(err) = wv.load_url(&srv.url) {
                            tracing::error!(%err, "could not load dsh web");
                        }
                    }
                    server = Some(srv);
                    navigated = true;
                }
                Ok(Err(err)) => {
                    tracing::error!(%err, "dsh web failed to start");
                    // Show the failure in the window. A silent spinner would
                    // leave the user with no idea anything went wrong.
                    if let Ok(wv) = webview.lock() {
                        let script = format!(
                            "window.__dshBoot && window.__dshBoot.fail({}, {});",
                            json_string("无法启动本地运行时"),
                            json_string(&err),
                        );
                        let _ = wv.evaluate_script(&script);
                    }
                    navigated = true; // Stop retrying; the window shows why.
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    navigated = true;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    // Still booting; the poll interval will tick us again.
                }
            }
        }

        match event {
            // The settings window closes independently of the main one.
            Event::WindowEvent {
                window_id,
                event: WindowEvent::CloseRequested,
                ..
            } if window_id != main_window_id => {
                tracing::debug!("settings window closed");
                settings_window = None;
            }
            Event::WindowEvent {
                event: WindowEvent::CloseRequested,
                ..
            } => {
                // With a tray and a summon shortcut, closing the window hides
                // the app instead of quitting: that is what makes the shortcut
                // useful, and it matches how tray apps behave. Quitting is
                // explicit, from the tray menu or Cmd-Q.
                if tray.is_some() {
                    window.set_visible(false);
                    tracing::info!("window hidden to tray; use the tray menu to quit");
                } else {
                    // No tray to restore from, so closing must quit — otherwise
                    // the app would be unreachable.
                    shutdown_server(&mut server);
                    runtime::cleanup();
                    *control_flow = ControlFlow::Exit;
                }
            }
            // A resize both needs the traffic-light inset re-asserted (macOS
            // resets it across some fullscreen and resize transitions) and
            // changes what should be remembered.
            Event::WindowEvent {
                event: WindowEvent::Resized(_),
                ..
            } => {
                #[cfg(target_os = "macos")]
                window.set_traffic_light_inset(tao::dpi::LogicalPosition::new(
                    theme::TRAFFIC_LIGHT_INSET.0,
                    theme::TRAFFIC_LIGHT_INSET.1,
                ));
                geometry_dirty = true;
            }
            Event::WindowEvent {
                event: WindowEvent::Moved(_),
                ..
            } => {
                geometry_dirty = true;
            }
            // Kept for the fallback and for the logged comparison: the badge
            // decision itself asks AppKit, because this event is not a reliable
            // answer to "is the user looking" (see below).
            Event::WindowEvent {
                event: WindowEvent::Focused(focused),
                ..
            } => {
                window_focused = focused;
                tracing::debug!(focused, "tao reported a focus change");
            }
            // macOS only. Clicking the dock icon while the window is hidden must
            // bring it back: the window hides to the tray on close, so without
            // this the icon looks broken — the app is running, and nothing
            // happens.
            #[cfg(target_os = "macos")]
            Event::Reopen { .. } => {
                summon(&window);
                tracing::info!("reopened from the dock");
            }
            _ => {}
        }

        // Looking at the window is what "seen" means, so the badges clear as
        // soon as the app is in front again — whatever brought it back: a
        // click, the Dock, the summon shortcut, or the tray menu.
        //
        // Evaluated here, on the loop's own tick, rather than in a focus-event
        // handler. A window that AppKit never made key produces no focus
        // transitions at all, so a listener-based clear would leave the badge
        // stuck on just as the setter would leave it stuck off. Asking AppKit
        // each tick costs a property read and always has an answer.
        {
            let visible = window.is_visible();
            let active = native::app_is_active().unwrap_or(window_focused);
            let watched = active && visible;
            if watched && !was_watched && unseen > 0 {
                unseen = 0;
                apply_unseen(&window, &mut tray, unseen);
                tracing::debug!(active, visible, "unread badges cleared; the window is in front");
            }
            was_watched = watched;
        }

        // Persist the geometry, but not on every frame of a drag.
        //
        // The write is debounced rather than deferred to exit: a window that is
        // hidden to the tray and then lost to a crash should still reopen where
        // it was.
        if geometry_dirty && geometry_written_at.elapsed() >= GEOMETRY_WRITE_INTERVAL {
            if let Some(path) = state_path.as_deref() {
                let next = capture_geometry(&window, &chrome, &last_geometry);
                window_state::save(path, &next);
                last_geometry = next;
            }
            geometry_dirty = false;
            geometry_written_at = std::time::Instant::now();
        }
    });
}

/// Bring the window back: visible, un-minimized, and in front.
///
/// Every path that raises the window wants all three — a summon shortcut, a
/// tray click, a plugin asking for focus, a second launch, a dock click — and
/// `set_visible` alone leaves a minimized window minimized.
fn summon(window: &tao::window::Window) {
    window.set_visible(true);
    window.set_minimized(false);
    window.set_focus();
    // The window may have been hidden or occluded, in which case the page has
    // no reason to have repainted.
    window.request_redraw();
}

/// The window's non-content border, measured once while the geometry is settled.
///
/// `inner_size()` reads the content view's frame, which AppKit has not yet
/// updated by the time a resize event is delivered — sampling it during a drag
/// therefore returns the *previous* size, and a resize would never be saved.
/// `outer_size()` is current, so the content size is derived from it by
/// subtracting this border.
///
/// The border is zero for the main window, which is borderless. It is measured
/// rather than assumed so that a port to a platform where the shell keeps a
/// native titlebar does not drift by the titlebar height on every restart.
#[derive(Debug, Clone, Copy)]
struct WindowChrome {
    width: u32,
    height: u32,
}

impl WindowChrome {
    /// Measure the border from a window whose size has stopped changing.
    fn measure(window: &tao::window::Window) -> Self {
        let outer = window.outer_size();
        let inner = window.inner_size();
        Self {
            width: outer.width.saturating_sub(inner.width),
            height: outer.height.saturating_sub(inner.height),
        }
    }

    /// The content size in physical pixels, derived from the current frame.
    fn content_size(&self, window: &tao::window::Window) -> tao::dpi::PhysicalSize<u32> {
        let outer = window.outer_size();
        tao::dpi::PhysicalSize::new(
            outer.width.saturating_sub(self.width),
            outer.height.saturating_sub(self.height),
        )
    }
}

/// The current window rectangle, in logical units.
///
/// While maximized or fullscreen the size is deliberately kept from
/// `previous`: the platform reports the maximized size, and saving that would
/// make the window restore at full screen size even after it is un-maximized.
fn capture_geometry(
    window: &tao::window::Window,
    chrome: &WindowChrome,
    previous: &window_state::WindowState,
) -> window_state::WindowState {
    let scale = window.scale_factor();
    let mut next = *previous;

    if !window.is_maximized() {
        let size = chrome.content_size(window).to_logical::<f64>(scale);
        next.width = size.width;
        next.height = size.height;
    }
    if let Ok(position) = window.outer_position() {
        let position = position.to_logical::<f64>(scale);
        next.x = Some(position.x);
        next.y = Some(position.y);
    }
    next.maximized = window.is_maximized();
    tracing::debug!(
        width = next.width,
        height = next.height,
        x = next.x,
        y = next.y,
        maximized = next.maximized,
        scale,
        outer = ?window.outer_size(),
        "captured window geometry"
    );
    next
}

/// Every monitor as a logical `(x, y, width, height)` rectangle.
///
/// Logical units because that is what the saved state is in: mixing the two
/// would scale a restored window by the display's density factor.
fn logical_monitors(
    event_loop: &tao::event_loop::EventLoopWindowTarget<()>,
) -> Vec<(f64, f64, f64, f64)> {
    event_loop
        .available_monitors()
        .map(|monitor| {
            let scale = monitor.scale_factor();
            let position = monitor.position();
            let size = monitor.size();
            (
                position.x as f64 / scale,
                position.y as f64 / scale,
                size.width as f64 / scale,
                size.height as f64 / scale,
            )
        })
        .collect()
}

/// Shorten a session id for display.
///
/// Session ids are UUIDs; the first segment is enough to tell two apart in a
/// notification without wrapping the text.
fn short_session(id: &str) -> String {
    let head: String = id.chars().take(8).collect();
    if id.chars().count() > 8 {
        format!("{head}…")
    } else {
        head
    }
}

/// Parse the configured shortcut, falling back to the default.
///
/// A malformed entry must not leave the user without a hotkey, so the default is
/// used and the problem logged.
fn initial_hotkey_spec(theme_source: &ThemeSource) -> native::HotkeySpec {
    let text = theme_source.current().hotkey.clone();
    match native::HotkeySpec::parse(&text) {
        Ok(spec) => spec,
        Err(err) => {
            tracing::warn!(
                shortcut = %text,
                %err,
                "invalid hotkey in settings; using the default"
            );
            native::HotkeySpec::default_spec()
        }
    }
}

/// The settings window and its page.
///
/// Held together so closing the window drops the web view too. `Window` is not
/// `Clone`, so the pair lives in one place.
struct SettingsWindow {
    window: tao::window::Window,
    webview: wry::WebView,
}

/// Open the settings window, or bring an existing one forward.
/// Start the DSH host, undoing a bad upgrade if it will not come up.
///
/// An upgrade is applied while the old version is still running in memory, so
/// the previous launch is the first honest test of the new tree. If a note says
/// an upgrade is on trial and no host can be started, the previous version is
/// restored and the host tried once more — the difference between a bad release
/// costing a click and costing a hand-repaired install.
///
/// Exactly one rollback attempt is made, so a failure that is not the upgrade's
/// fault still reports its real error instead of cycling.
fn spawn_host(
    url_tx: std::sync::mpsc::Sender<Result<server::DshServer, String>>,
    launcher: std::path::PathBuf,
    port: u16,
) {
    std::thread::Builder::new()
        .name("dsh-launch".into())
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(err) => {
                    let _ = url_tx.send(Err(format!("tokio runtime: {err}")));
                    return;
                }
            };

            let mut attempt = 0;
            let result = loop {
                attempt += 1;
                let program = launcher.to_string_lossy().into_owned();
                match runtime.block_on(server::start(program, port)) {
                    Ok(started) => {
                        // The new tree can run. Stop watching it; from here on a
                        // crash is an ordinary crash, not a failed upgrade.
                        if let Some(install) = updater::Install::discover(&launcher) {
                            updater::clear_pending(&install);
                        }
                        break Ok(started);
                    }
                    Err(err) => {
                        let install = updater::Install::discover(&launcher);
                        let pending = install.as_ref().and_then(updater::pending);
                        match (attempt, install, pending) {
                            (1, Some(install), Some(target)) => {
                                tracing::error!(
                                    %err,
                                    %target,
                                    "the upgraded DSH would not start; rolling back"
                                );
                                if let Err(rollback_err) = updater::rollback(&install) {
                                    // Nothing more to try: report the rollback
                                    // failure, which is the actionable one.
                                    break Err(format!(
                                        "the upgraded DSH would not start ({err}), and rolling \
                                         back failed too: {rollback_err}"
                                    ));
                                }
                                updater::clear_pending(&install);
                                tracing::warn!("restored the previous DSH; retrying the host");
                            }
                            _ => break Err(err),
                        }
                    }
                }
            };

            // The runtime must outlive the server it supervises, so leak it into
            // a parked thread rather than dropping it here.
            std::mem::forget(runtime);
            let _ = url_tx.send(result);
        })
        .expect("spawn dsh launcher thread");
}

/// Start a registry check on a worker thread.
///
/// The check is a network round trip, so it never runs on the UI thread. The
/// worker reports through `tx`, and the loop renders the result.
fn request_update_check(
    tx: &std::sync::mpsc::Sender<updater::Status>,
    status: &mut updater::Status,
    force: bool,
) {
    // Show the transient state immediately rather than when the answer lands.
    status.phase = updater::Phase::Checking;
    let channel = status.channel;
    let tx = tx.clone();
    let launcher = launcher_for_updates();
    std::thread::spawn(move || {
        let (result, _) = updater::check(&launcher, channel, force);
        let _ = tx.send(result);
    });
}

/// Stage, verify and apply the newest DSH on the configured channel.
///
/// Staging and applying are one operation from the user's point of view: a
/// staged tree that is never applied is just 280 MB of litter, so a failure at
/// either step reports as one failure with the live install untouched.
fn request_update_install(
    tx: &std::sync::mpsc::Sender<updater::Status>,
    status: &mut updater::Status,
    launcher: std::path::PathBuf,
    theme_source: &ThemeSource,
) {
    status.phase = updater::Phase::Working;
    let channel = updater::Channel::parse(&theme_source.current().update_channel)
        .unwrap_or_default();
    let tx = tx.clone();
    std::thread::spawn(move || {
        let (mut result, install) = updater::check(&launcher, channel, true);
        // A check that failed must not be followed by an install attempt.
        if !matches!(result.phase, updater::Phase::Available) {
            let _ = tx.send(result);
            return;
        }
        let (Some(install), Some(target)) = (install, result.target.clone()) else {
            result.phase = updater::Phase::Failed("nothing to install".into());
            let _ = tx.send(result);
            return;
        };

        if let Err(err) = updater::stage(&install, &target) {
            updater::discard_stage(&install, &target);
            result.phase = updater::Phase::Failed(err);
            let _ = tx.send(result);
            return;
        }
        if let Err(err) = updater::apply(&install, &target) {
            updater::discard_stage(&install, &target);
            result.phase = updater::Phase::Failed(err);
            let _ = tx.send(result);
            return;
        }

        // Note the target so the next launch can put the old tree back if this
        // one cannot start a host; see `spawn_host`.
        if let Err(err) = updater::record_pending(&install, &target) {
            tracing::warn!(%err, "could not record the pending upgrade");
        }

        // We are running the old DSH in memory while the new one is on disk, so
        // a restart is what actually applies it.
        result.phase = updater::Phase::RestartRequired;
        let _ = tx.send(result);
    });
}

/// The launcher the shell resolved at startup, for the update workers.
///
/// A `OnceLock` rather than a parameter because the helpers are called from the
/// event loop's closure, where threading another owned value to every call site
/// for a value that never changes would be noise.
static UPDATE_LAUNCHER: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();

fn launcher_for_updates() -> std::path::PathBuf {
    UPDATE_LAUNCHER
        .get()
        .cloned()
        .unwrap_or_else(|| std::path::PathBuf::from("dsh"))
}

/// Push an update status into the settings page, if it is open.
fn push_update_status(window: &Option<SettingsWindow>, status: &updater::Status) {
    if let Some(active) = window.as_ref() {
        let script = format!(
            "window.__dshUpdate && window.__dshUpdate.set({});",
            status.json()
        );
        let _ = active.webview.evaluate_script(&script);
    }
}

fn open_settings(
    slot: &mut Option<SettingsWindow>,
    event_loop: &tao::event_loop::EventLoopWindowTarget<()>,
    theme_source: &ThemeSource,
    link: &bridge::PluginLink,
    tx: &std::sync::mpsc::Sender<(String, settings::SettingsRequest)>,
    update: &updater::Status,
) {
    if let Some(existing) = slot.as_ref() {
        existing.window.set_visible(true);
        existing.window.set_focus();
        return;
    }

    let theme = theme_source.current();
    let builder = tao::window::WindowBuilder::new()
        .with_title(theme_source.locale().strings().settings_window_title)
        .with_inner_size(tao::dpi::LogicalSize::new(620.0, 720.0))
        .with_min_inner_size(tao::dpi::LogicalSize::new(480.0, 420.0));

    let window = match builder.build(event_loop) {
        Ok(window) => window,
        Err(err) => {
            tracing::warn!(%err, "could not open the settings window");
            return;
        }
    };

    let tx = tx.clone();
    let webview = wry::WebViewBuilder::new()
        .with_html(settings::page(
            &theme,
            link.is_connected(),
            theme_source.locale(),
            update,
        ))
        .with_ipc_handler(move |request| {
            let body = request.body().to_string();
            match settings::parse_request(&body) {
                Ok(parsed) => {
                    // Hand it to the UI thread: a save eventually reloads the
                    // theme, which touches main-thread-only objects.
                    if tx.send((body, parsed)).is_err() {
                        tracing::debug!("settings request dropped; shell is shutting down");
                    }
                }
                Err(err) => tracing::warn!(%err, "unusable settings message"),
            }
        })
        .build(&window);

    match webview {
        Ok(webview) => {
            *slot = Some(SettingsWindow { window, webview });
            if let Some(active) = slot.as_ref() {
                active.window.set_focus();
            }
        }
        Err(err) => tracing::warn!(%err, "could not create the settings page"),
    }
}

/// Apply a hotkey change from a theme.
///
/// A rejected shortcut leaves the previous one registered, so the user is never
/// left without a way to summon the window.
fn update_hotkey(hotkey: &mut Option<native::HotKeyHandle>, theme: &Theme) {
    let Some(active) = hotkey.as_mut() else {
        return;
    };
    match native::HotkeySpec::parse(&theme.hotkey) {
        Ok(spec) => match active.retarget(spec) {
            Ok(()) => tracing::info!(
                shortcut = %active.spec().to_string_canonical(),
                "hotkey updated"
            ),
            Err(err) => tracing::warn!(
                %err,
                keeping = %active.spec().to_string_canonical(),
                "hotkey change rejected; previous shortcut kept"
            ),
        },
        Err(err) => tracing::warn!(
            shortcut = %theme.hotkey,
            %err,
            "invalid hotkey; previous shortcut kept"
        ),
    }
}

/// Apply a theme to the native chrome: window background and traffic lights.
fn apply_theme_to_chrome(window: &tao::window::Window, theme: &Theme, is_dark: bool) {
    window.set_background_color(Some(hex_to_rgba(
        theme.palette_for(is_dark).background.0,
    )));
    #[cfg(target_os = "macos")]
    window.set_traffic_light_inset(tao::dpi::LogicalPosition::new(
        theme::TRAFFIC_LIGHT_INSET.0,
        theme::TRAFFIC_LIGHT_INSET.1,
    ));
    window.request_redraw();
}

/// Decide whether the shell should render dark.
///
/// There is deliberately no shell-side override: the page follows DSH's own
/// `ui-theme.preference`, so the chrome must resolve appearance from it too.
/// A shell override could only ever make the two disagree. To change the theme,
/// change it in DSH — that is the DSH-native path.
fn resolved_dark(theme_source: &ThemeSource) -> bool {
    theme_source.is_dark()
}

/// Stop the supervised host, if one is running.
fn shutdown_server(server: &mut Option<server::DshServer>) {
    if let Some(srv) = server.take() {
        if let Ok(rt) = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            rt.block_on(srv.shutdown());
        }
    }
}



/// `0xrrggbb` to the RGBA byte order `wry` expects.
fn hex_to_rgba(hex: u32) -> (u8, u8, u8, u8) {
    (
        ((hex >> 16) & 0xff) as u8,
        ((hex >> 8) & 0xff) as u8,
        (hex & 0xff) as u8,
        255,
    )
}

/// Script that installs the theme styles and the window-drag region on load.
///
/// Both are installed here rather than through separate initialization scripts
/// so they share one `DOMContentLoaded` path and survive navigations together.
fn inject_script(theme: &Theme, is_dark: bool) -> String {
    format!(
        r#"(function() {{
  var apply = function() {{
    applyStyles();
    applyDragRegion();
  }};
  if (document.readyState === 'loading') {{
    document.addEventListener('DOMContentLoaded', apply);
  }} else {{
    apply();
  }}

  function applyStyles() {{
    var el = document.getElementById('__dsh_shell_theme');
    if (!el) {{
      el = document.createElement('style');
      el.id = '__dsh_shell_theme';
      (document.head || document.documentElement).appendChild(el);
    }}
    el.textContent = {payload};
  }}

  // With the system titlebar hidden there is no OS drag area, so a strip along
  // the top of the page forwards pointer presses to the shell, which moves the
  // window. The strip only covers empty chrome: the strip sits above DSH's own
  // content because the page is padded down by --dsh-shell-caption-height.
  function applyDragRegion() {{
    if (document.getElementById('__dsh_shell_drag')) return;
    if (!document.documentElement) {{
      console.error('[dsh-shell] no documentElement; drag region not installed');
      return;
    }}
    var strip = document.createElement('div');
    strip.id = '__dsh_shell_drag';
    strip.setAttribute('aria-hidden', 'true');
    document.documentElement.appendChild(strip);

    strip.addEventListener('pointerdown', function (event) {{
      // Left button only. Right-click must still open context menus, and
      // double-click should keep the platform's zoom behaviour.
      if (event.button !== 0) return;
      event.preventDefault();
      if (window.ipc && window.ipc.postMessage) {{
        window.ipc.postMessage({drag_message});
      }}
    }});

    // Double-clicking empty chrome should zoom the window, matching the
    // behaviour a real titlebar would have.
    strip.addEventListener('dblclick', function () {{
      if (window.ipc && window.ipc.postMessage) {{
        window.ipc.postMessage({zoom_message});
      }}
    }});

    if (!window.ipc || !window.ipc.postMessage) {{
      // Without the IPC bridge the strip is inert and the window becomes
      // unmovable. Report it so the dead zone is diagnosable rather than silent.
      console.error('[dsh-shell] window.ipc unavailable; window drag is disabled');
    }} else {{
      window.ipc.postMessage({ready_message});
    }}
  }}
}})();"#,
        payload = json_string(&theme.injected_css(is_dark)),
        drag_message = json_string(DRAG_MESSAGE),
        zoom_message = json_string(ZOOM_MESSAGE),
        ready_message = json_string(READY_MESSAGE),
    )
}

/// Script that replaces the style element's contents on a theme reload.
fn rewrite_style_script(theme: &Theme, is_dark: bool) -> String {
    format!(
        r#"(function() {{
  var el = document.getElementById('__dsh_shell_theme');
  if (!el) {{
    el = document.createElement('style');
    el.id = '__dsh_shell_theme';
    (document.head || document.documentElement).appendChild(el);
  }}
  el.textContent = {payload};
}})();"#,
        payload = json_string(&theme.injected_css(is_dark))
    )
}

/// Quote a string as a JSON literal so it is safe to embed in JavaScript.
fn json_string(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| "\"\"".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use theme::{ColorHex, Theme};

    #[test]
    fn css_is_embedded_as_a_safe_js_literal() {
        let mut t = Theme::default();
        // A quote and a newline are the cases that would break naive string
        // interpolation into the script.
        t.custom_css = "body { content: \"a\\\"b\"; }\n/* x */".into();
        let script = inject_script(&t, false);

        // The payload must be JSON-escaped, not raw.
        assert!(script.contains("\\\"a\\\\\\\"b\\\""), "expected escaping: {script}");
        assert!(!script.contains("\nbody {"), "raw newline leaked into script");
    }

    #[test]
    fn the_boot_page_carries_the_locale() {
        let zh = boot_page(crate::i18n::Locale::Zh);
        assert!(zh.contains("\"zh\""), "locale missing from the boot page");
        assert!(
            !zh.contains("{locale}"),
            "boot page placeholder was not substituted"
        );
        let en = boot_page(crate::i18n::Locale::En);
        assert!(en.contains("\"en\""));
    }

    #[test]
    fn rgba_conversion_matches_byte_order() {
        assert_eq!(hex_to_rgba(0x16161e), (0x16, 0x16, 0x1e, 255));
        assert_eq!(hex_to_rgba(0xff0000), (0xff, 0x00, 0x00, 255));
    }

    #[test]
    fn injected_script_installs_the_drag_region() {
        let script = inject_script(&Theme::default(), false);
        // The strip must be created and wired to IPC, or the window cannot be
        // moved at all once the system titlebar is hidden.
        assert!(script.contains("__dsh_shell_drag"), "drag element missing");
        assert!(script.contains(DRAG_MESSAGE), "drag message not posted");
        assert!(script.contains(ZOOM_MESSAGE), "zoom message not posted");
        assert!(script.contains("pointerdown"), "no pointer handler");
        // Only the primary button may start a drag, so right-click menus work.
        assert!(script.contains("event.button !== 0"), "button guard missing");
    }

    #[test]
    fn drag_strip_css_is_injected() {
        let css = Theme::default().injected_css(false);
        assert!(css.contains("#__dsh_shell_drag"), "drag strip CSS missing");
        // The strip must be topmost, or DSH's own chrome would cover it.
        assert!(css.contains("z-index: 2147483647"), "strip not on top");
        // It must span the caption strip only, never the whole page.
        assert!(
            css.contains("height: var(--dsh-shell-caption-height)"),
            "strip height not tied to the caption strip"
        );
    }

    #[test]
    fn theme_tokens_reach_the_script() {
        // Resolve light explicitly so the assertion does not depend on the
        // machine's current appearance.
        let mut t = Theme::default();
        t.light.accent = ColorHex(0x123456);
        let script = inject_script(&t, false);
        assert!(script.contains("#123456"), "accent missing from script");
    }
}
