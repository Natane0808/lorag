//! G6: model management page — download / status-check for the LLM, embedding,
//! and (optional) rerank models configured in [`AppConfig`].
//!
//! Follows the G5 tokio ↔ GPUI bridge pattern exactly: button clicks spawn a
//! GPUI foreground task that in turn dispatches blocking work to the process-
//! wide tokio runtime (handle on [`super::app::AppState::tokio_handle`]) and
//! pushes state transitions back through [`Entity::update`].
//!
//! Each model row has its own [`ModelRowState`] and lifecycle:
//!
//! ```text
//! Pending ──(refresh finds file)──▶ Downloaded
//! Pending ──(download click)──────▶ Downloading ──(ok)──▶ Downloaded
//!                                              └─(err)─▶ Failed(msg)
//! ```
//!
//! Download progress is not surfaced byte-by-byte — aha's
//! [`crate::aha_provider::ensure_model_downloaded`] reports progress via its
//! own `println!` lines (caught by the G3 tracing bridge into
//! [`super::app::AppState::log_buffer`]); the row simply shows a spinner while
//! [`ModelStatus::Downloading`].

use std::sync::Arc;

use gpui::prelude::*;
use gpui::{App, AsyncApp, Context, Entity, IntoElement, Render, Window, div};
use gpui_component::{ActiveTheme as _, Disableable, IconName, StyledExt, button::*};

use crate::aha_provider::{ensure_model_downloaded, resolve_model_path};
use crate::config::AppConfig;

use super::app::AppState;

/// Human-friendly label for the LLM row.
const LLM_LABEL: &str = "对话模型 (LLM)";
/// Human-friendly label for the embedding row.
const EMBED_LABEL: &str = "嵌入模型 (Embedding)";
/// Human-friendly label for the rerank row.
const RERANK_LABEL: &str = "重排模型 (Rerank)";

/// Lifecycle of a single model row on the page. Drives status-dot color, the
/// visible label, and which buttons are enabled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelStatus {
    /// Row rendered but we haven't checked the filesystem yet — waiting for
    /// the user (or the auto-refresh on first draw) to hit "刷新".
    Pending,
    /// Download in progress on the tokio runtime; spinner shown, download
    /// button disabled.
    Downloading,
    /// Filesystem check found a valid model directory at
    /// `<MODELS_DIR>/<repo>/` (or in the `~/.aha/` fallback).
    Downloaded,
    /// Download attempt failed with an error message rendered inline.
    Failed(String),
}

impl ModelStatus {
    /// Chinese label shown next to the status dot.
    fn label_cn(&self) -> &'static str {
        match self {
            ModelStatus::Pending => "未检查",
            ModelStatus::Downloading => "下载中...",
            ModelStatus::Downloaded => "已下载",
            ModelStatus::Failed(_) => "下载失败",
        }
    }

    /// RGB fill color (`0xRRGGBB`) for the status-dot indicator.
    fn dot_color(&self) -> u32 {
        match self {
            ModelStatus::Pending => 0x6b7280,
            ModelStatus::Downloading => 0xf59e0b,
            ModelStatus::Downloaded => 0x10b981,
            ModelStatus::Failed(_) => 0xef4444,
        }
    }
}

/// Runtime state for one row in the model list.
#[derive(Debug, Clone)]
pub struct ModelRowState {
    /// Stable human-friendly label (LLM / Embedding / Rerank).
    pub label: &'static str,
    /// HuggingFace / ModelScope repo id (read from [`AppConfig`] at page
    /// construction time; G10 settings page will require a restart to change).
    pub repo: String,
    /// Current lifecycle.
    pub status: ModelStatus,
}

/// Per-page runtime state held as a GPUI entity so background tasks can push
/// status transitions back through `Entity::update` without borrowing the
/// view struct across await points.
pub struct ModelsState {
    /// One row per configured model. Built from [`AppConfig`] in
    /// [`ModelsState::new`] — rerank is omitted entirely when
    /// [`AppConfig::rerank_model`] is empty.
    pub rows: Vec<ModelRowState>,
}

impl ModelsState {
    /// Build the initial row list from frozen `cfg`. Rerank is skipped when
    /// its id is empty. All rows start in [`ModelStatus::Pending`]; the first
    /// "刷新" click resolves them to `Downloaded` or leaves them `Pending`.
    fn new(cfg: &AppConfig) -> Self {
        let mut rows = vec![
            ModelRowState {
                label: LLM_LABEL,
                repo: cfg.llm_model.clone(),
                status: ModelStatus::Pending,
            },
            ModelRowState {
                label: EMBED_LABEL,
                repo: cfg.embed_model.clone(),
                status: ModelStatus::Pending,
            },
        ];
        if !cfg.rerank_model.trim().is_empty() {
            rows.push(ModelRowState {
                label: RERANK_LABEL,
                repo: cfg.rerank_model.clone(),
                status: ModelStatus::Pending,
            });
        }
        Self { rows }
    }
}

/// G6 model-management page view. Owns an [`Entity<ModelsState>`] that the
/// download / refresh background tasks mutate through GPUI entity updates.
pub struct ModelsPage {
    /// Shared [`AppState`] (config + tokio handle). Cloned into spawned tasks.
    app: Entity<AppState>,
    /// Per-page runtime state (rows + per-row status).
    state: Entity<ModelsState>,
}

impl ModelsPage {
    /// Construct the page. Called from [`super::root_view`] when the
    /// [`super::pages::Page::Models`] variant is visible.
    ///
    /// `app` is the global [`AppState`] entity — kept alive as a strong ref so
    /// background download/refresh tasks can read the frozen [`AppConfig`] and
    /// the tokio handle.
    pub fn new(app: Entity<AppState>, _window: &mut Window, cx: &mut Context<Self>) -> Self {
        let cfg = app.read(cx).cfg.clone();
        let state = cx.new(|_| ModelsState::new(&cfg));
        // Fire-and-forget initial status check so the "已下载" status appears
        // on first paint without requiring a manual "刷新" click.
        on_refresh_clicked(&state, &app, cx);
        Self { app, state }
    }
}

impl Render for ModelsPage {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let rows = self.state.read(cx).rows.clone();
        let app_refresh = self.app.clone();
        let state_refresh = self.state.clone();

        let page_body = div()
            .size_full()
            .p_6()
            .flex()
            .flex_col()
            .gap_4()
            .child(div().text_xl().font_semibold().child("模型管理"))
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child("首次使用需先下载模型。下载过程可能需要几分钟，请稍候。"),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child("状态检查本地模型目录（含 ~/.aha 兼容路径）。"),
                    )
                    .child(
                        Button::new("models-refresh")
                            .label("刷新状态")
                            .icon(IconName::Redo)
                            .on_click(move |_ev, _window, cx: &mut App| {
                                on_refresh_clicked(&state_refresh, &app_refresh, cx);
                            }),
                    ),
            );

        let mut body = page_body;

        for (idx, row) in rows.iter().enumerate() {
            let is_downloading = matches!(row.status, ModelStatus::Downloading);
            let is_downloaded = matches!(row.status, ModelStatus::Downloaded);
            let is_failed = matches!(row.status, ModelStatus::Failed(_));
            let can_download = !is_downloading && !is_downloaded;

            let dot_rgb = row.status.dot_color();
            let status_label = row.status.label_cn();

            let state_dl = self.state.clone();
            let app_dl = self.app.clone();
            let repo_dl = row.repo.clone();
            let idx_dl = idx;

            let state_retry = self.state.clone();
            let app_retry = self.app.clone();
            let repo_retry = row.repo.clone();
            let idx_retry = idx;

            let failed_msg = match &row.status {
                ModelStatus::Failed(m) => Some(m.clone()),
                _ => None,
            };

            let mut row_div = div()
                .p_4()
                .rounded_md()
                .border_1()
                .border_color(cx.theme().border)
                .flex()
                .flex_col()
                .gap_2();

            let header = div()
                .flex()
                .items_center()
                .justify_between()
                .gap_3()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_3()
                        .flex_1()
                        .min_w_0()
                        .child(div().w_2p5().h_2p5().rounded_full().bg(gpui::rgb(dot_rgb)))
                        .child(div().text_base().font_medium().child(row.label))
                        .child(
                            div()
                                .text_sm()
                                .text_color(cx.theme().muted_foreground)
                                .child(status_label),
                        ),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(
                            Button::new(format!("models-download-{idx}"))
                                .label(if is_downloading {
                                    "下载中..."
                                } else if is_downloaded {
                                    "已下载"
                                } else {
                                    "下载"
                                })
                                .primary()
                                .icon(if is_downloaded {
                                    IconName::Check
                                } else {
                                    IconName::ArrowRight
                                })
                                .loading(is_downloading)
                                .disabled(!can_download)
                                .on_click(move |_ev, _window, cx: &mut App| {
                                    on_download_clicked(
                                        idx_dl,
                                        &state_dl,
                                        &app_dl,
                                        repo_dl.clone(),
                                        cx,
                                    );
                                }),
                        )
                        .child(
                            // Retry button is visible only after a failure, and
                            // is wired identically to "下载" (same flow).
                            Button::new(format!("models-retry-{idx}"))
                                .label("重试")
                                .disabled(!is_failed)
                                .on_click(move |_ev, _window, cx: &mut App| {
                                    on_download_clicked(
                                        idx_retry,
                                        &state_retry,
                                        &app_retry,
                                        repo_retry.clone(),
                                        cx,
                                    );
                                }),
                        ),
                );

            row_div = row_div.child(header);
            row_div = row_div.child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(format!("仓库：{}", row.repo)),
            );

            if let Some(msg) = failed_msg {
                row_div = row_div.child(
                    div()
                        .p_2()
                        .rounded_sm()
                        .border_1()
                        .border_color(gpui::red())
                        .text_color(gpui::red())
                        .text_sm()
                        .child(msg),
                );
            }

            body = body.child(row_div);
        }

        if rows.len() < 3 {
            body = body.child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child("提示：Rerank 模型未配置（留空 = 禁用）。可在设置页填写 RERANK_MODEL 启用。"),
            );
        }

        body
    }
}

// ──────────────────────────────────────────────────────────────────────
// Button-click handlers (free fns so they accept `&mut App` directly)
// ──────────────────────────────────────────────────────────────────────

/// "刷新" handler: re-run filesystem existence checks for every row using
/// [`resolve_model_path`] (which also honors the `~/.aha/` fallback used by
/// aha's CLI). Synchronous IO is cheap (`is_dir` + `read_dir`), so we still
/// dispatch to tokio via spawn_blocking to avoid stalling the UI thread.
fn on_refresh_clicked(state: &Entity<ModelsState>, app: &Entity<AppState>, cx: &mut App) {
    let (cfg, tokio_handle) =
        app.read_with(cx, |a, _cx| (Arc::clone(&a.cfg), a.tokio_handle.clone()));

    // Collect (idx, repo) pairs so the worker doesn't borrow `state.rows`.
    let repos: Vec<(usize, String)> = state
        .read(cx)
        .rows
        .iter()
        .enumerate()
        .map(|(i, r)| (i, r.repo.clone()))
        .collect();

    let state_for_task = state.clone();
    cx.spawn(async move |cx: &mut AsyncApp| {
        let models_dir = cfg.models_dir.clone();
        let join = tokio_handle.spawn_blocking(move || {
            repos
                .into_iter()
                .map(|(i, repo)| (i, resolve_model_path(&repo, &models_dir).is_some()))
                .collect::<Vec<_>>()
        });

        let resolved = match join.await {
            Ok(v) => v,
            Err(join_err) => {
                state_for_task.update(cx, |s, cx| {
                    for row in s.rows.iter_mut() {
                        if !matches!(row.status, ModelStatus::Downloading) {
                            row.status = ModelStatus::Failed(format!(
                                "刷新模型状态失败：后台任务中断 ({join_err})"
                            ));
                        }
                    }
                    cx.notify();
                });
                return;
            }
        };

        state_for_task.update(cx, |s, cx| {
            for (idx, exists) in resolved {
                if let Some(row) = s.rows.get_mut(idx) {
                    // Don't overwrite an in-progress download or an error
                    // the user is still reading; refresh is a "best effort"
                    // probe of the filesystem.
                    if matches!(row.status, ModelStatus::Downloading) {
                        continue;
                    }
                    row.status = if exists {
                        ModelStatus::Downloaded
                    } else {
                        ModelStatus::Pending
                    };
                }
            }
            cx.notify();
        });
    })
    .detach();
}

/// "下载" / "重试" handler: transition the row to [`ModelStatus::Downloading`],
/// spawn `ensure_model_downloaded` on the tokio runtime, then transition to
/// `Downloaded` / `Failed(msg)` when it resolves.
///
/// Error messages follow the AGENTS.md §4.4 three-part template:
/// `[action] + [object] + [reason/suggestion]`.
fn on_download_clicked(
    idx: usize,
    state: &Entity<ModelsState>,
    app: &Entity<AppState>,
    repo: String,
    cx: &mut App,
) {
    let (cfg, tokio_handle) =
        app.read_with(cx, |a, _cx| (Arc::clone(&a.cfg), a.tokio_handle.clone()));

    // Guard against double-click / spurious clicks.
    let current = state.read(cx).rows.get(idx).cloned();
    match current.as_ref().map(|r| &r.status) {
        Some(ModelStatus::Downloading) | Some(ModelStatus::Downloaded) => return,
        _ => {}
    }

    state.update(cx, |s, cx| {
        if let Some(row) = s.rows.get_mut(idx) {
            row.status = ModelStatus::Downloading;
        }
        cx.notify();
    });

    let models_dir = cfg.models_dir.clone();
    let retries = cfg.download_max_retries;
    let repo_for_task = repo.clone();
    let state_for_task = state.clone();

    cx.spawn(async move |cx: &mut AsyncApp| {
        let join = tokio_handle.spawn(async move {
            ensure_model_downloaded(&repo_for_task, &models_dir, retries).await
        });

        let result = join.await;

        state_for_task.update(cx, |s, cx| {
            let Some(row) = s.rows.get_mut(idx) else {
                return;
            };
            match result {
                Ok(Ok(_path)) => {
                    row.status = ModelStatus::Downloaded;
                }
                Ok(Err(e)) => {
                    row.status = ModelStatus::Failed(format!(
                        "下载模型 {} 失败：{e:#}（检查网络或重试；日志页查看下载进度）",
                        repo
                    ));
                }
                Err(join_err) => {
                    row.status = ModelStatus::Failed(format!(
                        "下载模型 {} 失败：后台任务中断 ({join_err})",
                        repo
                    ));
                }
            }
            cx.notify();
        });
    })
    .detach();
}
