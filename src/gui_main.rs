#![cfg(feature = "gui")]

use std::sync::{Arc, Mutex};

use gpui::*;
use gpui_component::*;

use lorag::gui::app::AppState;
use lorag::gui::root_view::RootView;
use lorag::gui::tray_host::{self, TrayUiCommand};

fn main() {
    // G1: GPU probe first. If blade renderer can't init (VMs without GPU passthrough,
    // RDP without GPU redirection, old iGPUs, headless servers), bail out with a
    // native platform dialog instead of panicking inside `application().run()`.
    if let Err(e) = lorag::gui::gpu_probe::probe_gpu() {
        eprintln!("GPU initialization failed: {e:#}");
        // Dialog failure is non-fatal — user already got the stderr trace above.
        let _ = lorag::gui::fallback_dialog::show_unavailable_gpu_dialog(&e);
        std::process::exit(1);
    }

    // G3: build the log broadcast bridge BEFORE installing the global subscriber so
    // the bridge layer can capture every event from this point on (including the
    // "tracing file appender initialized" info line).
    let log_bridge = lorag::gui::logging::make_bridge(256);

    // G2/G3: init tracing (stderr + daily rolling file + broadcast bridge).
    // If the file appender fails we still want the GUI to launch — fall back to
    // stderr-only by not hard-erroring here.
    if let Err(e) = lorag::logging::init_tracing(true, Some(log_bridge.clone())) {
        eprintln!("warning: failed to init file logging, falling back to stderr-only: {e:#}");
        let _ = lorag::logging::init_tracing(false, Some(log_bridge.clone()));
    }

    // G4: load `.env` config once up-front; frozen in AppState (G10 writes back
    // to disk and prompts for restart rather than hot-reloading).
    let cfg = lorag::config::load().expect("failed to load lorag config (check .env)");
    let port = cfg.lorag_gui_port;

    // G5: create a multi-threaded tokio runtime BEFORE entering the GPUI event
    // loop. All library async code (AhaClient::init uses spawn_blocking; axum
    // server; ingest pipeline) requires a tokio context. We keep the runtime
    // alive for the whole process and hand out its Handle to pages.
    let tokio_rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("failed to build tokio runtime");
    let tokio_handle = tokio_rt.handle().clone();
    // Leak the runtime so it lives for process lifetime — gpui's smol executor
    // drives the UI thread, but tokio worker threads need to keep running for
    // model inference and axum to continue working after gui_main returns into
    // the GPUI run() closure. ~100 bytes, one-time cost.
    std::mem::forget(tokio_rt);

    // G12: spawn the tray icon on its own OS thread BEFORE entering the GPUI
    // run loop. The tray thread owns its Win32 message pump and must not share
    // a thread with GPUI's smol executor (risk of deadlock / missed events).
    let tray_handle =
        tray_host::spawn_tray_thread(port).expect("failed to spawn lorag tray thread");

    // Shared slot for the main window's AnyWindowHandle. Written once by the
    // open_window closure on the GPUI foreground; read by the tray command
    // drain task when the user clicks "Show Window". Contention is zero in
    // practice (one write, rare reads).
    let window_slot: Arc<Mutex<Option<AnyWindowHandle>>> = Arc::new(Mutex::new(None));

    let app = gpui_platform::application().with_assets(gpui_component_assets::Assets);

    app.run(move |cx| {
        gpui_component::init(cx);
        // Init theme system (watches ./themes/ dir, persists theme choice, handles SwitchTheme actions)
        gpui_component::theme::init(cx);

        // G4: AppState owns the broadcast receiver + log buffer drain task.
        let log_rx = log_bridge.subscribe();
        let app_state: Entity<AppState> =
            cx.new(|cx| AppState::new(log_rx, cfg, tokio_handle.clone(), cx));

        let slot_for_open = Arc::clone(&window_slot);
        let slot_for_drain = Arc::clone(&window_slot);
        let app_state_for_open = app_state.clone();
        cx.spawn(async move |cx| {
            let window_result = cx.open_window(
                WindowOptions {
                    titlebar: Some(TitleBar::title_bar_options()),
                    ..Default::default()
                },
                |window, cx| {
                    // G12: intercept the native window-close (X button). GPUI's
                    // default tears the window down and, once no windows remain,
                    // exits the run loop — which would silently kill the process
                    // even though the tray icon is still visible. Instead we:
                    //   1. cancel the close (return false), and
                    //   2. minimize the window so the user gets the same visual
                    //      feedback as a real minimize-to-tray.
                    // The tray "Show Window" / double-click then calls
                    // `window.activate_window()` (which restores + focuses).
                    window.on_window_should_close(cx, |window, _cx| {
                        window.minimize_window();
                        false
                    });

                    let view = cx.new(|cx| RootView::new(app_state_for_open.clone(), window, cx));
                    let root = cx.new(|cx| Root::new(view, window, cx).bg(cx.theme().background));

                    if let Ok(mut guard) = slot_for_open.lock() {
                        *guard = Some(window.window_handle());
                    }

                    root
                },
            );
            if let Ok(handle) = window_result
                && let Ok(mut guard) = slot_for_open.lock()
            {
                *guard = Some(handle.into());
            }
        })
        .detach();

        // G12: foreground task that drains tray commands and applies them to
        // the GPUI app / window. std mpsc recv is blocking, so we run it on
        // the tokio blocking pool via `spawn_blocking` and forward each
        // command back to the GPUI foreground with `cx.update_window` /
        // `cx.update`.
        let cmd_rx_for_task = tray_handle.cmd_rx;
        let tokio_for_drain = tokio_handle.clone();
        cx.spawn(async move |cx: &mut AsyncApp| {
            let _tray_join = tray_handle.join; // keeps the OS thread handle alive
            let mut rx = Some(cmd_rx_for_task);
            loop {
                let rx_for_block = rx.take().expect("recv rx taken only once per iteration");
                let recv = tokio_for_drain
                    .spawn_blocking(move || {
                        let res = rx_for_block.recv();
                        (rx_for_block, res)
                    })
                    .await;

                let Ok((rx_returned, recv_result)) = recv else {
                    // spawn_blocking itself panicked (very unexpected); bail.
                    break;
                };
                rx = Some(rx_returned);

                match recv_result {
                    Ok(TrayUiCommand::ShowWindow) => {
                        let maybe_handle = slot_for_drain.lock().ok().and_then(|g| *g);
                        if let Some(handle) = maybe_handle {
                            let _ = cx.update_window(handle, |_root, window, _cx| {
                                window.activate_window();
                            });
                        }
                    }
                    Ok(TrayUiCommand::Quit) => {
                        cx.update(|cx| cx.quit());
                        break;
                    }
                    Err(_) => {
                        // Tray thread dropped its sender (already exited); ask
                        // GPUI to quit so the process doesn't linger without
                        // a tray icon.
                        cx.update(|cx| cx.quit());
                        break;
                    }
                }
            }
        })
        .detach();
    });
}
