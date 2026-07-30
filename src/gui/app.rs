//! Global application state for the desktop launcher.
//!
//! [`AppState`] is a GPUI entity owned by the window's root view. It holds the
//! currently selected [`Page`], the live log ring buffer fed by the tracing
//! broadcast bridge (G3), and the frozen [`AppConfig`] loaded at startup.
//!
//! Cross-page communication and background task updates all flow through
//! `Entity<AppState>`: pages read it via `state.read(cx)` and mutate it via
//! `state.update(cx, |s, cx| ...)` from foreground tasks.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;

use gpui::{App, AsyncApp, Context, Task};
use gpui_component::{Theme, ThemeRegistry};
use tokio::runtime::Handle as TokioHandle;
use tokio::sync::broadcast;

use crate::config::AppConfig;

use super::pages::Page;

/// Maximum number of formatted log lines kept in the in-memory ring buffer.
/// When the buffer is full the oldest line is dropped on every new arrival.
const LOG_BUFFER_CAP: usize = 5000;

/// How often the foreground poll drains pending events from the broadcast
/// receiver into the ring buffer and notifies observers (logs page).
const LOG_DRAIN_INTERVAL: Duration = Duration::from_millis(100);

/// Global GUI state entity. One instance per `lorag-gui` process.
pub struct AppState {
    /// Currently visible page (highlighted in sidebar, rendered in right pane).
    pub current_page: Page,
    /// Live subscription to the tracing broadcast bridge installed in G3.
    /// Polled by [`AppState::spawn_log_drain`]; not read directly elsewhere.
    log_receiver: broadcast::Receiver<String>,
    /// Ring buffer of the most recent formatted log lines, shown in the logs
    /// page (G9). New lines are appended by [`AppState::drain_log_events`].
    pub log_buffer: VecDeque<String>,
    /// Frozen snapshot of `.env` loaded at GUI startup. G10's settings page
    /// will write a new config to disk and prompt for restart; we do not
    /// hot-reload.
    pub cfg: Arc<AppConfig>,
    /// Background task that polls `log_receiver` every 100 ms. Held here so
    /// the task is cancelled when `AppState` drops (window close).
    _log_drain_task: Task<()>,
    /// Handle to the multi-threaded tokio runtime created in `gui_main`.
    /// All `lorag` library code (AhaClient, server, ingest) uses tokio
    /// primitives (`spawn_blocking`, `TcpListener`, etc.) and requires a
    /// running tokio context. Spawned by GUI pages via
    /// [`tokio::runtime::Handle::spawn`] and bridged back to GPUI via
    /// `cx.spawn`/`cx.update` (G5 critical-path pattern).
    pub tokio_handle: TokioHandle,
    pub dark_mode: bool,
}

impl AppState {
    /// Construct a new state entity. Called exactly once from `gui_main`.
    ///
    /// `log_receiver` should be obtained from
    /// [`crate::logging::LogBridge::subscribe`] (or the thin
    /// [`super::logging::make_bridge`] wrapper) after the global tracing
    /// subscriber has been installed, so no events are missed.
    ///
    /// `tokio_handle` is the [`tokio::runtime::Handle`] to the multi-threaded
    /// runtime created in `gui_main` before entering the GPUI event loop; all
    /// business-logic async tasks (service start/stop, model download, ingest)
    /// are spawned on this runtime.
    pub fn new(
        log_receiver: broadcast::Receiver<String>,
        cfg: AppConfig,
        tokio_handle: TokioHandle,
        cx: &mut Context<Self>,
    ) -> Self {
        let task = Self::spawn_log_drain(cx);
        Self {
            current_page: Page::Service,
            log_receiver,
            log_buffer: VecDeque::with_capacity(LOG_BUFFER_CAP),
            cfg: Arc::new(cfg),
            _log_drain_task: task,
            tokio_handle,
            dark_mode: false,
        }
    }

    /// Switch to a different top-level page and request a re-render.
    ///
    /// Called from sidebar menu item click handlers. Switching is a cheap
    /// notify — each page is rendered directly into the right-hand pane on
    /// demand, so no lazy-loading machinery is needed in G4.
    pub fn switch_page(&mut self, page: Page, cx: &mut Context<Self>) {
        if self.current_page != page {
            self.current_page = page;
            cx.notify();
        }
    }

    pub fn toggle_theme(&mut self, cx: &mut App) {
        self.dark_mode = !self.dark_mode;
        let theme_name = if self.dark_mode { "tokyonight" } else { "ayu" };
        if let Some(theme) = ThemeRegistry::global(cx).themes().get(theme_name).cloned() {
            Theme::global_mut(cx).apply_config(&theme);
        }
        cx.refresh_windows();
    }

    /// Pull all currently-pending lines from the broadcast receiver and append
    /// them to the ring buffer, evicting oldest lines once
    /// [`LOG_BUFFER_CAP`] is reached. Returns the freshly-appended lines so
    /// callers (G9 logs page) can decide whether to auto-scroll.
    ///
    /// This is non-blocking: [`broadcast::Receiver::try_recv`] returns
    /// `Err(TryRecvError::Empty)` immediately when no events are queued.
    pub fn drain_log_events(&mut self) -> Vec<String> {
        let mut fresh = Vec::new();
        loop {
            match self.log_receiver.try_recv() {
                Ok(line) => {
                    if self.log_buffer.len() >= LOG_BUFFER_CAP {
                        self.log_buffer.pop_front();
                    }
                    self.log_buffer.push_back(line.clone());
                    fresh.push(line);
                }
                Err(broadcast::error::TryRecvError::Empty) => break,
                Err(broadcast::error::TryRecvError::Lagged(n)) => {
                    // Slow subscriber — record a marker so the user can see
                    // that lines were dropped. Don't break; more may follow.
                    let marker = format!("[... log receiver lagged, {n} lines dropped ...]");
                    if self.log_buffer.len() >= LOG_BUFFER_CAP {
                        self.log_buffer.pop_front();
                    }
                    self.log_buffer.push_back(marker);
                }
                Err(broadcast::error::TryRecvError::Closed) => break,
            }
        }
        fresh
    }

    /// Spawn a foreground task that wakes every [`LOG_DRAIN_INTERVAL`], drains
    /// pending log events, and notifies any observer of [`AppState`]. The
    /// returned [`Task`] must be kept alive on the struct — dropping it
    /// cancels the polling loop.
    fn spawn_log_drain(cx: &mut Context<Self>) -> Task<()> {
        cx.spawn(async move |this, cx: &mut AsyncApp| {
            loop {
                cx.background_executor().timer(LOG_DRAIN_INTERVAL).await;
                let should_notify = this
                    .update(cx, |state, cx| {
                        let fresh = state.drain_log_events();
                        if !fresh.is_empty() {
                            cx.notify();
                        }
                    })
                    .is_ok();
                if !should_notify {
                    // Entity dropped (window closed) — stop polling.
                    break;
                }
            }
        })
    }
}
