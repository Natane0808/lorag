//! G8: doctor / health-check page — runs [`crate::doctor::run_checks`] and
//! renders the result as a table with PASS/WARN/FAIL status dots.
//!
//! Follows the G5/G6/G7 tokio ↔ GPUI bridge pattern exactly:
//! [`crate::doctor::run_checks`] is a pure synchronous function doing
//! filesystem probes (`std::fs::read_dir`, write probes, `config.json` reads),
//! so we dispatch it to `tokio_handle.spawn_blocking` from inside a
//! `cx.spawn` foreground task and push the result back through
//! `Entity<DoctorState>::update`.
//!
//! ## State machine
//!
//! ```text
//! Empty ──(auto-run on construction OR "重新检查" click)──▶ Running ──▶ Run(results)
//!                                                                  └─▶ Failed(msg)
//! ```
//!
//! Auto-runs once when the page entity is first constructed so the user sees
//! fresh results the moment they click the "健康" sidebar entry; the "重新检查"
//! button re-runs on demand.

use std::sync::Arc;
use std::time::Instant;

use gpui::prelude::*;
use gpui::{App, AsyncApp, Context, Entity, IntoElement, Render, Window, div};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::scroll::ScrollableElement;
use gpui_component::{ActiveTheme as _, Disableable, IconName, StyledExt};

use crate::doctor::{Check, CheckStatus, Summary, run_checks};

use super::app::AppState;

/// RGB fill color (`0xRRGGBB`) for a PASS status dot.
const PASS_COLOR: u32 = 0x10b981;
/// RGB fill color (`0xRRGGBB`) for a WARN status dot.
const WARN_COLOR: u32 = 0xf59e0b;
/// RGB fill color (`0xRRGGBB`) for a FAIL status dot.
const FAIL_COLOR: u32 = 0xef4444;

/// Lifecycle phase of the doctor check run. Drives button loading state,
/// the visible status dot per row, and the bottom summary banner.
#[derive(Debug, Clone)]
pub enum DoctorPhase {
    /// Never run (should be fleeting — auto-run fires immediately on build).
    Empty,
    /// `run_checks` in flight on the tokio blocking pool.
    Running,
    /// Last run completed with a full results vector.
    Run(Vec<Check>),
    /// Last run aborted with a top-level error message (e.g. panic caught by
    /// the blocking task, or `cfg` clone failure).
    Failed(String),
}

/// Per-page runtime state held as a GPUI entity so background tasks can push
/// state transitions back through `Entity::update` without borrowing the view
/// struct across await points.
pub struct DoctorState {
    /// Current lifecycle phase.
    pub phase: DoctorPhase,
    /// Monotonic timestamp of the last successful/failed run, for the
    /// "上次检查: ..." label (GPUI-free, uses `std::time::Instant`).
    pub last_run_at: Option<Instant>,
}

impl DoctorState {
    /// Build the initial state. The page entity fires an auto-run immediately
    /// after construction in [`DoctorPage::new`], so this starts `Empty` only
    /// for the brief window between `cx.new` and the first spawn tick.
    fn new() -> Self {
        Self {
            phase: DoctorPhase::Empty,
            last_run_at: None,
        }
    }
}

/// G8 doctor page view. Owns an [`Entity<DoctorState>`] that the background
/// `run_checks` task mutates through GPUI entity updates.
pub struct DoctorPage {
    /// Shared [`AppState`] (config + tokio handle). Cloned into spawned tasks.
    app: Entity<AppState>,
    /// Per-page runtime state (phase + last-run timestamp).
    state: Entity<DoctorState>,
}

impl DoctorPage {
    /// Construct the page. Called from [`super::root_view`] when the
    /// [`super::pages::Page::Doctor`] variant is visible.
    ///
    /// Kicks off an automatic first run so the user sees results immediately
    /// when they navigate to the page (per plan §4 G8: "GUI 启动时自动跑一次
    /// doctor"; doing it here instead of in `AppState::new` keeps eager work
    /// lazy until the user actually opens the page — same effect on first
    /// visit, cheaper if they never click the tab).
    pub fn new(app: Entity<AppState>, _window: &mut Window, cx: &mut Context<Self>) -> Self {
        let state = cx.new(|_| DoctorState::new());
        on_run_clicked(&state, &app, cx);
        Self { app, state }
    }
}

impl Render for DoctorPage {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let phase = self.state.read(cx).phase.clone();
        let last_run_at = self.state.read(cx).last_run_at;

        let is_running = matches!(phase, DoctorPhase::Running);

        let state_run = self.state.clone();
        let app_run = self.app.clone();

        let last_run_label = match last_run_at {
            Some(_) => "刚刚".to_string(),
            None => "尚未运行".to_string(),
        };

        let mut body = div()
            .w_full()
            .p_6()
            .flex()
            .flex_col()
            .gap_4()
            .child(div().text_xl().font_semibold().child("健康检查"))
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child("检查运行环境：配置文件 / 模型 / 存储 / 编译选项。启动时自动运行一次。"),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(format!("上次检查：{last_run_label}")),
                    )
                    .child(
                        Button::new("doctor-rerun")
                            .label("重新检查")
                            .primary()
                            .icon(IconName::Redo)
                            .loading(is_running)
                            .disabled(is_running)
                            .on_click(move |_ev, _window, cx: &mut App| {
                                on_run_clicked(&state_run, &app_run, cx);
                            }),
                    ),
            );

        match &phase {
            DoctorPhase::Empty | DoctorPhase::Running => {
                body = body.child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(if is_running {
                            "正在检查，请稍候..."
                        } else {
                            "准备中..."
                        }),
                );
            }
            DoctorPhase::Failed(msg) => {
                body = body.child(
                    div()
                        .p_3()
                        .rounded_md()
                        .border_1()
                        .border_color(gpui::red())
                        .text_color(gpui::red())
                        .text_sm()
                        .child(format!("检查失败：{msg}")),
                );
            }
            DoctorPhase::Run(checks) => {
                let summary = Summary::from_checks(checks);

                // Header row (check / status / detail / hint)
                body = body.child(
                    div()
                        .grid()
                        .grid_cols(3)
                        .gap_3()
                        .p_3()
                        .rounded_md()
                        .bg(cx.theme().secondary)
                        .text_sm()
                        .font_semibold()
                        .child(div().child("检查项"))
                        .child(div().child("状态"))
                        .child(div().child("详情")),
                );

                let mut last_category = "";
                for check in checks {
                    if check.category != last_category {
                        body = body.child(
                            div()
                                .pt_3()
                                .pb_1()
                                .text_sm()
                                .font_semibold()
                                .text_color(cx.theme().muted_foreground)
                                .child(category_label_cn(check.category)),
                        );
                        last_category = check.category;
                    }
                    body = body.child(render_check_row(check, cx));
                }

                body = body.child(render_summary_banner(&summary));
            }
        }

        div().size_full().overflow_y_scrollbar().child(body)
    }
}

/// Render one table row for a single [`Check`]. Columns: name / status dot+label
/// / detail (+ optional hint).
fn render_check_row(check: &Check, cx: &mut Context<DoctorPage>) -> gpui::AnyElement {
    let (dot_color, icon, label) = status_triple(check.status);

    let detail_line = if let Some(hint) = &check.hint {
        div()
            .flex()
            .flex_col()
            .gap_1()
            .child(div().text_sm().child(check.detail.clone()))
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(format!("提示：{hint}")),
            )
    } else {
        div().text_sm().child(check.detail.clone())
    };

    div()
        .grid()
        .grid_cols(3)
        .gap_3()
        .p_3()
        .border_b_1()
        .border_color(cx.theme().border)
        .child(div().text_sm().child(check.name.clone()))
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .w_2p5()
                        .h_2p5()
                        .rounded_full()
                        .bg(gpui::rgb(dot_color)),
                )
                .child(div().text_sm().child(format!("{icon} {label}"))),
        )
        .child(detail_line)
        .into_any_element()
}

/// Render the bottom summary banner ("X PASS, Y WARN, Z FAIL") in a color
/// matching the worst status.
fn render_summary_banner(summary: &Summary) -> gpui::AnyElement {
    let (banner_color, label) = if summary.fail > 0 {
        (
            FAIL_COLOR,
            format!(
                "{} 项检查：{} 通过 / {} 警告 / {} 失败 — 请先修复失败项",
                summary.total, summary.pass, summary.warn, summary.fail
            ),
        )
    } else if summary.warn > 0 {
        (
            WARN_COLOR,
            format!(
                "{} 项检查：{} 通过 / {} 警告 — 核心检查通过，建议查看警告",
                summary.total, summary.pass, summary.warn
            ),
        )
    } else {
        (
            PASS_COLOR,
            format!("{} 项检查：全部通过 — 环境正常", summary.total),
        )
    };

    div()
        .mt_2()
        .p_3()
        .rounded_md()
        .border_1()
        .border_color(gpui::rgb(banner_color))
        .text_sm()
        .font_medium()
        .child(label)
        .into_any_element()
}

/// Map a [`CheckStatus`] to (dot_rgb, icon_char, chinese_label) used in the
/// status column.
fn status_triple(status: CheckStatus) -> (u32, &'static str, &'static str) {
    match status {
        CheckStatus::Pass => (PASS_COLOR, "✅", "通过"),
        CheckStatus::Warn => (WARN_COLOR, "⚠️", "警告"),
        CheckStatus::Fail => (FAIL_COLOR, "❌", "失败"),
    }
}

/// Map the English category string emitted by [`crate::doctor`] into a Chinese
/// section header.
fn category_label_cn(category: &str) -> &'static str {
    match category {
        "config" => "配置",
        "models" => "模型",
        "storage" => "存储",
        "build" => "编译",
        _ => "其他",
    }
}

// ──────────────────────────────────────────────────────────────────────
// Button-click handler (free fn so it accepts `&mut App` directly)
// ──────────────────────────────────────────────────────────────────────

/// "重新检查" handler (also fired once on page construction): transition to
/// [`DoctorPhase::Running`], spawn `run_checks(cfg)` on the tokio blocking
/// pool, then transition to [`DoctorPhase::Run`] (or [`DoctorPhase::Failed`])
/// with the current timestamp.
///
/// Error messages follow the AGENTS.md §4.4 three-part template:
/// `[action] + [object] + [reason/suggestion]`.
pub fn on_run_clicked(state: &Entity<DoctorState>, app: &Entity<AppState>, cx: &mut App) {
    let (cfg, tokio_handle) =
        app.read_with(cx, |a, _cx| (Arc::clone(&a.cfg), a.tokio_handle.clone()));

    // Guard against double-click while already running.
    if matches!(state.read(cx).phase, DoctorPhase::Running) {
        return;
    }

    state.update(cx, |s, cx| {
        s.phase = DoctorPhase::Running;
        cx.notify();
    });

    let state_for_task = state.clone();
    cx.spawn(async move |cx: &mut AsyncApp| {
        let join = tokio_handle.spawn_blocking(move || run_checks(&cfg));

        let result = join.await;

        state_for_task.update(cx, |s, cx| {
            s.last_run_at = Some(Instant::now());
            match result {
                Ok(checks) => {
                    s.phase = DoctorPhase::Run(checks);
                }
                Err(join_err) => {
                    s.phase = DoctorPhase::Failed(format!(
                        "运行健康检查失败：后台任务中断 ({join_err})（重启程序再试）"
                    ));
                }
            }
            cx.notify();
        });
    })
    .detach();
}
