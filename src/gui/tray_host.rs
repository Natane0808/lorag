//! G12: tray icon host for the GPUI desktop launcher.
//!
//! Runs the `tray-icon` event loop on a **dedicated OS thread** (not a tokio
//! worker, not the GPUI smol executor), because tray-icon requires a Win32
//! message pump on its creating thread and that pump conflicts with both
//! async runtimes.
//!
//! ## Wire-up
//!
//! `gui_main` spawns one background OS thread via [`spawn_tray_thread`] before
//! entering `gpui_platform::application().run(...)`. The tray thread owns:
//!
//! - the `tray_icon::TrayIcon` handle,
//! - its own Win32 message pump (`pump_windows_messages`),
//! - a std mpsc channel carrying [`TrayUiCommand`] to the GPUI foreground.
//!
//! The GPUI side spawns a foreground task that waits on that channel and
//! reacts by either restoring / focusing the window or asking GPUI to quit.
//!
//! ## Menu (right-click or double-click)
//!
//! - `Show Window` (`show`) → bring the GPUI window to the foreground
//! - `Open Web UI` (`open`) → `tray::open_browser(http://localhost:{port})`
//! - separator
//! - `Quit` (`quit`) → tell GPUI to shut the app down (which triggers
//!   `on_app_will_quit` handlers the service page uses to stop axum), then
//!   this function returns and the tray thread exits.
//!
//! A left **double-click** on the tray icon also fires the "show window"
//! action (the same behavior as the menu item).

use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use anyhow::{Context, Result};
use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIconBuilder, TrayIconEvent};

use crate::tray;

/// Commands the tray thread sends to the GPUI foreground task.
///
/// The tray thread cannot touch `AsyncApp` / `Window` directly (they are
/// neither `Send` nor safe to access off the UI thread), so instead it emits
/// these high-level intents through a std mpsc channel. The foreground task
/// on the GPUI smol executor blocks on that channel via
/// `tokio::task::spawn_blocking` and dispatches each command onto the app.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayUiCommand {
    /// User picked "Show Window" (menu) or double-clicked the tray icon.
    ShowWindow,
    /// User picked "Quit". GPUI should `cx.quit()` to tear down windows and
    /// run all registered `on_app_will_quit` handlers (the service page uses
    /// one to stop axum gracefully).
    Quit,
}

/// Tray-menu item id → internal command. Returns `None` for unknown ids.
fn menu_id_to_command(id: &str) -> Option<TrayMenuCommand> {
    match id {
        "show" => Some(TrayMenuCommand::ShowWindow),
        "open" => Some(TrayMenuCommand::OpenBrowser),
        "quit" => Some(TrayMenuCommand::Quit),
        _ => None,
    }
}

/// Internal commands dispatched inside the tray event loop. Distinct from
/// [`TrayUiCommand`] because `OpenBrowser` runs entirely on the tray thread
/// (no GPUI interaction needed).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TrayMenuCommand {
    ShowWindow,
    OpenBrowser,
    Quit,
}

/// Decode the embedded `assets/icon.png` → RGBA → `tray_icon::Icon`.
fn load_icon() -> Result<Icon> {
    let png_bytes = include_bytes!("../../assets/icon.png");
    let img = image::load_from_memory(png_bytes).context("failed to decode embedded icon.png")?;
    let rgba = img.to_rgba8();
    let (w, h) = rgba.dimensions();
    Icon::from_rgba(rgba.into_raw(), w, h).context("failed to build tray Icon from RGBA bytes")
}

/// Handle returned by [`spawn_tray_thread`]. Join the thread and take the
/// command receiver for the GPUI foreground poll loop.
pub struct TrayHandle {
    /// Join handle for the dedicated tray OS thread. The thread exits
    /// shortly after `TrayUiCommand::Quit` is sent (it drops the icon and
    /// returns from `run_tray_blocking`).
    pub join: JoinHandle<Result<()>>,
    /// Receiving end of the tray → GPUI command channel. The foreground task
    /// calls `recv` (blocking, from `spawn_blocking`) and forwards each
    /// command to `AsyncApp`.
    pub cmd_rx: Receiver<TrayUiCommand>,
}

/// Spawn the tray icon on its own OS thread and return a handle to join it
/// and drain UI commands.
///
/// Must be called **before** `gpui_platform::application().run(...)` so the
/// tray icon exists for the lifetime of the GPUI run loop. The thread runs
/// [`run_tray_blocking`] internally and exits on `Quit`.
///
/// # Arguments
///
/// * `port` - TCP port the embedded axum server is (or will be) listening
///   on; used to build the `Open Web UI` URL.
pub fn spawn_tray_thread(port: u16) -> Result<TrayHandle> {
    let (cmd_tx, cmd_rx) = channel::<TrayUiCommand>();
    let join = thread::Builder::new()
        .name("lorag-tray".to_string())
        .spawn(move || run_tray_blocking(port, cmd_tx))
        .context("failed to spawn tray OS thread")?;
    Ok(TrayHandle { join, cmd_rx })
}

/// Run the tray event loop on the current thread. Blocks until `Quit`.
///
/// This mirrors M11's `crate::tray::run_tray_loop` (same pump + 50ms poll
/// structure) but adds:
///
/// - a `Show Window` menu item that forwards to GPUI via `cmd_tx`,
/// - a `TrayIconEvent::DoubleClick` listener that also forwards `ShowWindow`,
/// - forwarding `Quit` as a [`TrayUiCommand::Quit`] instead of firing an
///   axum oneshot (in GUI mode, axum shutdown is driven by the service
///   page's `on_app_will_quit` handler).
///
/// `Open Web UI` is handled inline on the tray thread via
/// [`tray::open_browser`] (same as M11).
fn run_tray_blocking(port: u16, cmd_tx: Sender<TrayUiCommand>) -> Result<()> {
    let icon = load_icon()?;
    let menu = Menu::new();
    let show_item = MenuItem::with_id("show", "Show Window", true, None);
    let open_item = MenuItem::with_id("open", "Open Web UI", true, None);
    let quit_item = MenuItem::with_id("quit", "Quit", true, None);
    menu.append(&show_item)
        .context("failed to append Show Window menu item")?;
    menu.append(&open_item)
        .context("failed to append Open Web UI menu item")?;
    menu.append(&PredefinedMenuItem::separator())
        .context("failed to append separator")?;
    menu.append(&quit_item)
        .context("failed to append Quit menu item")?;

    let _tray_icon = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_tooltip("lorag")
        .with_icon(icon)
        .build()
        .context("failed to build tray icon")?;

    // Forward `MenuEvent`s (right-click selections) onto a local std mpsc so
    // the main loop can multiplex menu + tray + pump in one select().
    let (menu_tx, menu_rx) = std::sync::mpsc::channel::<TrayMenuCommand>();
    thread::spawn(move || {
        let menu_events = MenuEvent::receiver();
        while let Ok(event) = menu_events.recv() {
            if let Some(cmd) = menu_id_to_command(event.id.0.as_str())
                && menu_tx.send(cmd).is_err()
            {
                break;
            }
        }
    });

    // Forward `TrayIconEvent::DoubleClick` as a ShowWindow menu command.
    // Clicks / enters / leaves are ignored.
    let (tray_tx, tray_rx) = std::sync::mpsc::channel::<TrayMenuCommand>();
    thread::spawn(move || {
        let tray_events = TrayIconEvent::receiver();
        while let Ok(event) = tray_events.recv() {
            if matches!(event, TrayIconEvent::DoubleClick { .. })
                && tray_tx.send(TrayMenuCommand::ShowWindow).is_err()
            {
                break;
            }
        }
    });

    println!("lorag-gui: tray icon ready (right-click for menu, double-click to show window)");

    loop {
        pump_windows_messages();

        // Drain both menu and tray channels; prefer non-blocking try_recv so
        // a burst of events is handled before the next pump.
        let mut cmd = None;
        if let Ok(c) = menu_rx.try_recv() {
            cmd = Some(c);
        } else if let Ok(c) = tray_rx.try_recv() {
            cmd = Some(c);
        } else {
            match menu_rx.recv_timeout(Duration::from_millis(50)) {
                Ok(c) => cmd = Some(c),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    anyhow::bail!("menu event channel disconnected unexpectedly")
                }
            }
        }

        let Some(cmd) = cmd else { continue };

        match cmd {
            TrayMenuCommand::ShowWindow => {
                // GPUI side handles the actual activate / focus. If the
                // receiver has been dropped (app is already quitting) we
                // just swallow the click.
                let _ = cmd_tx.send(TrayUiCommand::ShowWindow);
            }
            TrayMenuCommand::OpenBrowser => {
                let url = format!("http://localhost:{port}");
                if let Err(e) = tray::open_browser(&url) {
                    eprintln!("{e:#}");
                }
            }
            TrayMenuCommand::Quit => {
                println!("lorag-gui: quitting from tray...");
                let _ = cmd_tx.send(TrayUiCommand::Quit);
                return Ok(());
            }
        }
    }
}

/// Pump Win32 message queue (non-blocking `PeekMessageW` loop).
///
/// Windows-only. tray-icon creates a hidden `HWND` on its own thread and
/// pushes menu / click events to internal channels from its window proc;
/// without an explicit pump those events never fire. This is the same
/// 7-line block `src/tray.rs` uses — kept here (rather than re-exported) so
/// the GUI tray host is self-contained and does not pull extra symbols from
/// the CLI-only `tray` module.
///
/// # Safety
///
/// Calls Win32 `PeekMessageW` / `TranslateMessage` / `DispatchMessageW`
/// with a zero-initialized `MSG`; these are FFI calls into user32 but take
/// only a pointer to local stack memory and a null window handle, so the
/// block is sound on the thread that owns the tray icon.
#[cfg(target_os = "windows")]
fn pump_windows_messages() {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        DispatchMessageW, MSG, PM_REMOVE, PeekMessageW, TranslateMessage,
    };
    unsafe {
        let mut msg: MSG = std::mem::zeroed();
        while PeekMessageW(&mut msg, std::ptr::null_mut(), 0, 0, PM_REMOVE) != 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

#[cfg(not(target_os = "windows"))]
fn pump_windows_messages() {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn menu_id_show_maps_to_show_window() {
        assert_eq!(
            menu_id_to_command("show"),
            Some(TrayMenuCommand::ShowWindow)
        );
    }

    #[test]
    fn menu_id_open_maps_to_open_browser() {
        assert_eq!(
            menu_id_to_command("open"),
            Some(TrayMenuCommand::OpenBrowser)
        );
    }

    #[test]
    fn menu_id_quit_maps_to_quit() {
        assert_eq!(menu_id_to_command("quit"), Some(TrayMenuCommand::Quit));
    }

    #[test]
    fn menu_id_unknown_returns_none() {
        assert_eq!(menu_id_to_command("bogus"), None);
        assert_eq!(menu_id_to_command(""), None);
        assert_eq!(menu_id_to_command("SHOW"), None);
        assert_eq!(menu_id_to_command("Quit"), None);
    }
}
