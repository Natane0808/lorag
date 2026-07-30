//! G5: service control page — start/stop the embedded axum server and open
//! the Web UI chat in the system browser.
//!
//! This page validates the **tokio ↔ gpui bridge pattern** that G6–G11 follow:
//! model loading and axum run on the process-wide multi-threaded tokio runtime
//! (created in `gui_main`; handle stored on
//! [`super::app::AppState::tokio_handle`]), and all state transitions are
//! pushed back to the GPUI UI thread via `cx.spawn` + `Entity::update`.
//!
//! ## State machine
//!
//! ```text
//! Stopped ──(start click)──▶ Starting ──(ok)──▶ Running
//!                              │                      │
//!                              └──(fail)──▶ Stopped ◀─┘(stop→Stopping→Stopped)
//! ```
//!
//! While `Running`, [`ServiceState::shutdown_tx`] holds the
//! [`tokio::sync::oneshot::Sender`] that feeds `server::start_with_shutdown`;
//! pressing "stop" takes it and fires it, then waits (bounded by a 5s timer)
//! for graceful shutdown.

use std::sync::Arc;
use std::time::Duration;

use anyhow::Context as _;
use gpui::prelude::*;
use gpui::{App, AsyncApp, Context, Entity, IntoElement, Render, Window, div};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::{ActiveTheme as _, Disableable, StyledExt};
use tokio::sync::{Mutex as AsyncMutex, oneshot};

use crate::aha_provider::AhaClient;
use crate::config::AppConfig;
use crate::server::{self, AppState as AxumState};
use crate::store::sqlite_store::SqliteStore;
use crate::tray;

use super::app::AppState;
use super::logs::LogsPage;

/// Default TCP port for the embedded axum server when no override is supplied.
/// G10 (settings page) will let the user change this and persist to `.env`; for
/// G5 we hardcode the same default that `lorag serve` / `lorag tray` use.
const DEFAULT_PORT: u16 = 3000;

/// How long to show "停止中..." before flipping back to Stopped. axum's
/// graceful shutdown drains within this window under normal conditions; if it
/// doesn't, the port will still be released by the OS soon after.
const STOP_UI_TIMEOUT: Duration = Duration::from_secs(5);

/// Lifecycle phase of the embedded lorag HTTP service. Drives button
/// enablement, the status-indicator color, and the visible label.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceStatus {
    /// Server not running; nothing listening on the port.
    Stopped,
    /// User clicked start; model load + axum bind in progress (10s–minutes).
    Starting,
    /// Server bound and accepting requests; browser button is enabled.
    Running,
    /// User clicked stop; oneshot fired, awaiting graceful shutdown.
    Stopping,
}

impl ServiceStatus {
    /// Chinese label shown next to the status dot.
    fn label_cn(self) -> &'static str {
        match self {
            ServiceStatus::Stopped => "已停止",
            ServiceStatus::Starting => "启动中...",
            ServiceStatus::Running => "运行中",
            ServiceStatus::Stopping => "停止中...",
        }
    }

    /// RGB fill color (`0xRRGGBB`) for the status-dot icon.
    fn dot_color(self) -> u32 {
        match self {
            ServiceStatus::Stopped => 0x6b7280,
            ServiceStatus::Starting => 0xf59e0b,
            ServiceStatus::Running => 0x10b981,
            ServiceStatus::Stopping => 0xf59e0b,
        }
    }
}

/// Per-page runtime state held as a GPUI entity so that background tasks can
/// push state transitions back through `Entity::update` without borrowing the
/// view struct across await points.
pub struct ServiceState {
    /// Current lifecycle phase.
    pub status: ServiceStatus,
    /// TCP port the server is / will be / was listening on.
    pub port: u16,
    /// Last start/stop error surfaced to the user (red text under the status).
    pub error: Option<String>,
    /// Signals the running axum server to shut down. `Some` only while
    /// [`ServiceStatus::Running`]; taken by the stop flow to fire the shutdown
    /// future.
    pub shutdown_tx: Option<oneshot::Sender<()>>,
}

impl ServiceState {
    fn new(port: u16) -> Self {
        Self {
            status: ServiceStatus::Stopped,
            port,
            error: None,
            shutdown_tx: None,
        }
    }
}

/// G5 service-control page view. Owns an [`Entity<ServiceState>`] that the
/// start/stop background tasks mutate through GPUI entity updates.
pub struct ServicePage {
    /// Shared [`AppState`] (config + tokio handle). Cloned into spawned tasks.
    app: Entity<AppState>,
    /// Per-page runtime state (status, port, shutdown sender, last error).
    state: Entity<ServiceState>,
    /// G9: embedded live log viewer. Reused as a child entity so the filter
    /// selection and scroll-handle "at bottom" heuristic survive service
    /// start/stop re-renders without a standalone sidebar entry.
    logs_page: Entity<LogsPage>,
}

impl ServicePage {
    /// Construct the page. Called from [`super::root_view`] when the
    /// [`super::pages::Page::Service`] variant is visible.
    ///
    /// `app` is the global [`AppState`] entity — kept alive as a strong ref so
    /// background start/stop tasks can read the frozen [`AppConfig`] and the
    /// tokio handle.
    pub fn new(app: Entity<AppState>, _window: &mut Window, cx: &mut Context<Self>) -> Self {
        let state = cx.new(|_| ServiceState::new(DEFAULT_PORT));
        let logs_page = cx.new(|cx| LogsPage::new(app.clone(), _window, cx, true));
        Self {
            app,
            state,
            logs_page,
        }
    }
}

impl Render for ServicePage {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let status = self.state.read(cx).status;
        let port = self.state.read(cx).port;
        let error = self.state.read(cx).error.clone();

        let can_start = matches!(status, ServiceStatus::Stopped);
        let can_stop = matches!(status, ServiceStatus::Running);
        let can_open = matches!(status, ServiceStatus::Running);

        let state_start = self.state.clone();
        let app_start = self.app.clone();
        let state_stop = self.state.clone();
        let app_stop = self.app.clone();
        let state_open = self.state.clone();

        let dot_rgb = status.dot_color();

        let starting = matches!(status, ServiceStatus::Starting);
        let stopping = matches!(status, ServiceStatus::Stopping);

        div()
            .size_full()
            .p_4()
            .flex()
            .flex_col()
            .gap_3()
            .child(div().text_lg().font_semibold().child("服务控制"))
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child("启动本地服务后，点击“打开聊天”在浏览器中使用 Web UI。"),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .py_1()
                    .child(
                        // Plain rounded div (10x10px) for the status dot —
                        // avoids the icon component dependency entirely.
                        div().w_2p5().h_2p5().rounded_full().bg(gpui::rgb(dot_rgb)),
                    )
                    .child(div().text_base().child(status.label_cn()))
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(format!("localhost:{port}")),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        Button::new("svc-start")
                            .label("启动")
                            .primary()
                            .loading(starting)
                            .disabled(!can_start)
                            .on_click(move |_ev, _window, cx: &mut App| {
                                on_start_clicked(&state_start, &app_start, cx);
                            }),
                    )
                    .child(
                        Button::new("svc-stop")
                            .label("停止")
                            .danger()
                            .loading(stopping)
                            .disabled(!can_stop)
                            .on_click(move |_ev, _window, cx: &mut App| {
                                on_stop_clicked(&state_stop, &app_stop, cx);
                            }),
                    )
                    .child(
                        Button::new("svc-open")
                            .label("打开聊天")
                            .disabled(!can_open)
                            .on_click(move |_ev, _window, cx: &mut App| {
                                let port = state_open.read(cx).port;
                                let url = format!("http://localhost:{port}");
                                if let Err(e) = tray::open_browser(&url) {
                                    state_open.update(cx, |s, cx| {
                                        s.error = Some(format!("{e:#}"));
                                        cx.notify();
                                    });
                                }
                            }),
                    ),
            )
            .child(
                if matches!(status, ServiceStatus::Starting | ServiceStatus::Stopping) {
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child("这可能需要几十秒到几分钟，请稍候...")
                        .into_any_element()
                } else {
                    div().into_any_element()
                },
            )
            .child(if let Some(err) = error {
                div()
                    .p_3()
                    .rounded_md()
                    .border_1()
                    .border_color(gpui::red())
                    .text_color(gpui::red())
                    .text_sm()
                    .child(err)
                    .into_any_element()
            } else {
                div().into_any_element()
            })
            .child(div().pt_3().text_sm().font_medium().child("实时日志"))
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child("服务运行日志自动记录；磁盘保留最近 7 天"),
            )
            .child(div().flex_1().min_h_0().child(self.logs_page.clone()))
    }
}

// ──────────────────────────────────────────────────────────────────────
// Button-click handlers (free fns so they accept `&mut App` directly)
// ──────────────────────────────────────────────────────────────────────

fn on_start_clicked(state: &Entity<ServiceState>, app: &Entity<AppState>, cx: &mut App) {
    let (cfg, tokio_handle) =
        app.read_with(cx, |a, _cx| (Arc::clone(&a.cfg), a.tokio_handle.clone()));

    if !matches!(state.read(cx).status, ServiceStatus::Stopped) {
        return;
    }

    let port = state.read(cx).port;
    state.update(cx, |s, cx| {
        s.status = ServiceStatus::Starting;
        s.error = None;
        s.shutdown_tx = None;
        cx.notify();
    });

    let state_for_task = state.clone();
    cx.spawn(async move |cx: &mut AsyncApp| {
        // Two oneshots coordinate with the spawned server task:
        // - `ready_tx/rx`: fires once TcpListener::bind succeeds (the server is
        //   actually listening). Only then do we flip to Running and hand the
        //   shutdown_tx to UI state (so Stop can't fire before the server is
        //   bound; Bug 3 F3 fix).
        // - `shutdown_tx/rx`: fed into `axum::serve(...).with_graceful_shutdown`,
        //   fired when the user clicks Stop.
        let (ready_tx, ready_rx) = oneshot::channel::<()>();
        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();

        let server_join = tokio_handle.spawn(async move {
            run_server_until_shutdown_with_ready(cfg, port, shutdown_rx, ready_tx).await
        });

        // Wait for the server to signal it has bound the listener (or for the
        // task to drop/error out before binding — e.g. port-in-use, model load
        // failure). We don't apply an independent timeout here; the server task
        // itself already returns an error in all failure paths, which resolves
        // `ready_rx` with a RecvError.
        let ready_result = ready_rx.await;

        match ready_result {
            Ok(()) => {
                // Server bound successfully — transition to Running. Only now do
                // we store `shutdown_tx`; the Stop button is enabled by the
                // `ServiceStatus::Running` match and will take + fire it.
                state_for_task.update(cx, |s, cx| {
                    s.status = ServiceStatus::Running;
                    s.shutdown_tx = Some(shutdown_tx);
                    s.error = None;
                    cx.notify();
                });
            }
            Err(_) => {
                // Server died before signalling ready — fall through and let the
                // `server_join.await` result populate the error message. Note
                // that `shutdown_tx` is dropped here (no one holds it), which is
                // fine: the server task is already exiting.
            }
        }

        let result = server_join.await;

        state_for_task.update(cx, |s, cx| {
            s.shutdown_tx = None;
            match result {
                Ok(Ok(())) => {
                    s.status = ServiceStatus::Stopped;
                    s.error = None;
                }
                Ok(Err(e)) => {
                    s.status = ServiceStatus::Stopped;
                    s.error = Some(format!("{e:#}"));
                }
                Err(join_err) => {
                    s.status = ServiceStatus::Stopped;
                    s.error = Some(format!("server task aborted: {join_err}"));
                }
            }
            cx.notify();
        });
    })
    .detach();
}

fn on_stop_clicked(state: &Entity<ServiceState>, _app: &Entity<AppState>, cx: &mut App) {
    if !matches!(state.read(cx).status, ServiceStatus::Running) {
        return;
    }

    let tx = state.update(cx, |s, cx| {
        s.status = ServiceStatus::Stopping;
        s.error = None;
        cx.notify();
        s.shutdown_tx.take()
    });

    let Some(tx) = tx else {
        state.update(cx, |s, cx| {
            s.status = ServiceStatus::Stopped;
            cx.notify();
        });
        return;
    };

    let fired = tx.send(()).is_ok();
    let state_for_task = state.clone();

    cx.spawn(async move |cx: &mut AsyncApp| {
        if fired {
            cx.background_executor().timer(STOP_UI_TIMEOUT).await;
        }
        state_for_task.update(cx, |s, cx| {
            if s.status == ServiceStatus::Stopping {
                s.status = ServiceStatus::Stopped;
                s.shutdown_tx = None;
            }
            cx.notify();
        });
    })
    .detach();
}

// ──────────────────────────────────────────────────────────────────────
// Server lifecycle (runs on tokio worker threads)
// ──────────────────────────────────────────────────────────────────────

/// Load AhaClient + open sqlite, bind the TcpListener, signal `ready_tx`,
/// then run axum with graceful shutdown until `shutdown_rx` fires.
///
/// The `ready_tx` oneshot is fired **after** `TcpListener::bind` succeeds and
/// **before** `axum::serve(...).await` — that exact moment is when the UI
/// should flip from `Starting` to `Running` (Bug 3 F3 fix): the port is
/// listening, the Stop button can safely fire `shutdown_tx`, and the "open
/// chat" button can open a browser to a server that will respond.
///
/// We inline the bind/serve logic from [`server::start_with_shutdown`] here
/// (rather than modifying `server.rs`) so we can slot the ready signal
/// between `bind()` and `serve()`. The router-construction + CORS + fallback
/// steps are duplicated intentionally to keep the `server` module's public
/// API unchanged for its other callers (`lorag serve` / `lorag tray`).
async fn run_server_until_shutdown_with_ready(
    cfg: Arc<AppConfig>,
    port: u16,
    shutdown_rx: oneshot::Receiver<()>,
    ready_tx: oneshot::Sender<()>,
) -> anyhow::Result<()> {
    tracing::info!(port, "service: loading AhaClient...");
    let client = AhaClient::init((*cfg).clone()).await.map_err(|e| {
        anyhow::anyhow!(
            "failed to init AhaClient with LLM={}: {e:#} (run: lorag models status)",
            cfg.llm_model
        )
    })?;

    tracing::info!(path = %cfg.sqlite_path.display(), "service: opening sqlite...");
    let sqlite = SqliteStore::open(&cfg.sqlite_path).map_err(|e| {
        anyhow::anyhow!(
            "failed to open sqlite at {}: {e:#}",
            cfg.sqlite_path.display()
        )
    })?;

    let state = Arc::new(AxumState {
        client: Arc::new(AsyncMutex::new(client)),
        cfg: Arc::clone(&cfg),
        sqlite: Arc::new(AsyncMutex::new(sqlite)),
    });

    tracing::info!(port, "service: building axum router...");
    use tower_http::cors::CorsLayer;
    let api_router = server::build_router(Arc::clone(&state));
    let app = axum::Router::new()
        .merge(api_router)
        .fallback(server::serve_static)
        .layer(CorsLayer::permissive());

    let addr = std::net::SocketAddr::from(([0, 0, 0, 0], port));
    tracing::info!(port, "service: binding TcpListener...");
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .context(format!("failed to bind to port {port}"))?;

    // We've bound the socket — signal the UI that we're "Running". If the
    // receiver was dropped (UI gone), that's fine; we continue serving until
    // the shutdown signal (or process exit).
    let _ = ready_tx.send(());

    println!("lorag gui: http://localhost:{port}");

    let shutdown_future = async move {
        let _ = shutdown_rx.await;
    };

    tracing::info!(port, "service: axum serving (with graceful shutdown)...");
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_future)
        .await
        .context("axum server error")
}
