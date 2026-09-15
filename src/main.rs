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
mod menu;
mod native;
mod server;
mod theme;

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

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let dsh_program = std::env::var("DSH_BIN").unwrap_or_else(|_| "dsh".to_string());
    let port: u16 = std::env::var("DSH_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(0);
    let workspace = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let theme_path = theme_path(&workspace);

    tracing::info!(theme = %theme_path.display(), "loading theme");
    let theme_source = ThemeSource::new(theme_path);

    // Start `dsh web` on a worker runtime so the UI thread is never blocked
    // waiting for the host to boot.
    let (url_tx, url_rx) = std::sync::mpsc::channel::<Result<server::DshServer, String>>();
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
            let result = runtime.block_on(server::start(dsh_program, port));
            // The runtime must outlive the server it supervises, so leak it into
            // a parked thread rather than dropping it here.
            std::mem::forget(runtime);
            let _ = url_tx.send(result);
        })
        .expect("spawn dsh launcher thread");

    // --- Native capabilities ---------------------------------------------
    //
    // Created before the window so an early agent event is not missed. Both are
    // optional: a failure here is logged and the shell continues.

    // The bridge listener runs on its own thread; events are funnelled to the
    // UI thread over a channel, because tray APIs are main-thread-only.
    let (bridge_tx, bridge_rx) = std::sync::mpsc::channel::<bridge::BridgeEvent>();
    if let Err(err) = bridge::listen(move |event| {
        let _ = bridge_tx.send(event);
    }) {
        tracing::warn!(%err, "bridge unavailable; tray and notifications will stay idle");
    }

    let mut tray = match native::Tray::new() {
        Ok(tray) => Some(tray),
        Err(err) => {
            tracing::warn!(%err, "tray unavailable; continuing without it");
            None
        }
    };
    let tray_menu_rx = tray_icon::menu::MenuEvent::receiver().clone();

    // Global summon shortcut. Optional: if another app owns the combination the
    // shell still runs, it just cannot be summoned while hidden.
    let hotkey = native::register_hotkey(None);
    let hotkey_rx = global_hotkey::GlobalHotKeyEvent::receiver().clone();

    // The application menu.
    //
    // Installed before the window so the Edit items exist from the first frame.
    // Without them macOS has nothing to bind Cmd+C/V/X/A to and the standard
    // clipboard shortcuts silently do nothing — the web view implements them,
    // but the system only routes them through the menu bar.
    let app_menu = menu::build("DSH Shell");
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

    let mut builder = WindowBuilder::new()
        .with_title("DeepSeek Harness")
        .with_inner_size(LogicalSize::new(1280.0, 840.0))
        .with_min_inner_size(LogicalSize::new(720.0, 480.0));

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
                theme_for_window.traffic_light_inset.x,
                theme_for_window.traffic_light_inset.y,
            ));
    }

    let window = builder.build(&event_loop).expect("build window");

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
            theme_for_window.palette_for(resolved_dark(&theme_for_window)).background.0,
        ))
        .with_initialization_script(&inject_script(&theme_for_window))
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
        .with_html(BOOT_HTML)
        .build(&*window)
        .expect("build webview");

    let webview = Arc::new(Mutex::new(webview));

    // Paint the boot screen in the right appearance immediately.
    if let Ok(wv) = webview.lock() {
        let script = format!(
            "window.__dshBoot && window.__dshBoot.setAppearance({});",
            resolved_dark(&theme_for_window)
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

    event_loop.run(move |event, _, control_flow| {
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
                if let Err(err) = wv.evaluate_script(&rewrite_style_script(&theme)) {
                    tracing::warn!(%err, "could not apply theme to page");
                }
            }
            // `tao::window::RGBA` is a plain (r, g, b, a) tuple, which is what
            // `hex_to_rgba` already produces.
            window.set_background_color(Some(hex_to_rgba(
                theme.palette_for(resolved_dark(&theme)).background.0,
            )));
            #[cfg(target_os = "macos")]
            window.set_traffic_light_inset(tao::dpi::LogicalPosition::new(
                theme.traffic_light_inset.x,
                theme.traffic_light_inset.y,
            ));
            applied_theme = true;
        }
        if applied_theme {
            window.request_redraw();
        }

        // --- Tray events --------------------------------------------------
        //
        // Drained here rather than on the tray's own thread because window
        // operations (show, focus) must happen on the main thread.
        while let Ok(event) = tray_menu_rx.try_recv() {
            if let Some(active) = tray.as_ref() {
                match native::tray_command(&event, active) {
                    Some(native::TrayCommand::Show) => {
                        window.set_visible(true);
                        window.set_focus();
                    }
                    Some(native::TrayCommand::Quit) => {
                        shutdown_server(&mut server);
                        bridge::cleanup();
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
                    bridge::cleanup();
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
                    window.set_visible(true);
                    window.set_minimized(false);
                    window.set_focus();
                    tracing::info!("summoned by global hotkey");
                }
            }
        }

        // --- Bridge events ------------------------------------------------
        //
        // Drive the tray state and post notifications. Notifications are
        // limited to events a user actually wants interrupting; see
        // `BridgeEvent::wants_notification`.
        while let Ok(event) = bridge_rx.try_recv() {
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
                let (title, body) = match event.kind.as_str() {
                    "request-error" => (
                        format!("DSH: {session} failed"),
                        event
                            .message
                            .clone()
                            .unwrap_or_else(|| "The agent hit an error.".into()),
                    ),
                    _ => (
                        format!("DSH: {session} finished"),
                        // Prefer an explicit reason ("end_turn", "cancelled")
                        // over a generic message: it tells the user whether the
                        // agent completed or stopped early.
                        event
                            .message
                            .clone()
                            .or_else(|| event.reason.clone())
                            .map(|r| format!("Stopped: {r}"))
                            .unwrap_or_else(|| "The agent is done.".into()),
                    ),
                };
                native::notify(&title, &body);
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
                    bridge::cleanup();
                    *control_flow = ControlFlow::Exit;
                }
            }
            // Re-assert the traffic-light inset: macOS resets it on some
            // fullscreen and resize transitions.
            #[cfg(target_os = "macos")]
            Event::WindowEvent {
                event: WindowEvent::Resized(_),
                ..
            } => {
                let t = theme_source.current();
                window.set_traffic_light_inset(tao::dpi::LogicalPosition::new(
                    t.traffic_light_inset.x,
                    t.traffic_light_inset.y,
                ));
            }
            _ => {}
        }
    });
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

/// Decide whether the shell should render dark.
///
/// Precedence, and why:
///   1. The theme file's own `appearance`, so a user can pin the shell.
///   2. DSH's `ui-theme.preference`, because that is what DSH applies to the
///      page — following the OS instead would let the chrome and the page
///      disagree, which is the exact seam this theming exists to remove.
///   3. The operating system, for DSH's `system` setting.
///
/// Resolved in one place so the native window and the injected CSS cannot
/// drift apart.
fn resolved_dark(theme: &Theme) -> bool {
    use theme::Appearance;
    match theme.appearance {
        Appearance::Light => false,
        Appearance::Dark => true,
        Appearance::System => native::resolved_is_dark(),
    }
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

/// Resolve `theme.json`, preferring the crate directory so `cargo run` works
/// from anywhere, with `DSH_THEME` as an explicit override.
fn theme_path(workspace: &std::path::Path) -> PathBuf {
    if let Ok(explicit) = std::env::var("DSH_THEME") {
        return PathBuf::from(explicit);
    }
    let manifest_relative = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("theme.json");
    if manifest_relative.exists() {
        return manifest_relative;
    }
    workspace.join("theme.json")
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
fn inject_script(theme: &Theme) -> String {
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
        payload = json_string(&theme.injected_css(resolved_dark(&theme))),
        drag_message = json_string(DRAG_MESSAGE),
        zoom_message = json_string(ZOOM_MESSAGE),
        ready_message = json_string(READY_MESSAGE),
    )
}

/// Script that replaces the style element's contents on a theme reload.
fn rewrite_style_script(theme: &Theme) -> String {
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
        payload = json_string(&theme.injected_css(resolved_dark(&theme)))
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
        let script = inject_script(&t);

        // The payload must be JSON-escaped, not raw.
        assert!(script.contains("\\\"a\\\\\\\"b\\\""), "expected escaping: {script}");
        assert!(!script.contains("\nbody {"), "raw newline leaked into script");
    }

    #[test]
    fn rgba_conversion_matches_byte_order() {
        assert_eq!(hex_to_rgba(0x16161e), (0x16, 0x16, 0x1e, 255));
        assert_eq!(hex_to_rgba(0xff0000), (0xff, 0x00, 0x00, 255));
    }

    #[test]
    fn injected_script_installs_the_drag_region() {
        let script = inject_script(&Theme::default());
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
        // Pin the appearance so the assertion does not depend on the machine's
        // current light/dark setting.
        let mut t = Theme::default();
        t.appearance = theme::Appearance::Light;
        t.light.accent = ColorHex(0x123456);
        let script = inject_script(&t);
        assert!(script.contains("#123456"), "accent missing from script");
    }
}
