//! G7: document ingest page — rfd file/folder picker, per-file ingest progress,
//! and a table of already-ingested sources pulled from SQLite.
//!
//! Follows the G5/G6 tokio ↔ GPUI bridge pattern exactly:
//! - rfd native dialogs are dispatched via `tokio_handle.spawn_blocking`
//!   (they are blocking syscalls; keeping them off the UI thread avoids
//!   repaint stalls while the modal is up).
//! - Each file's ingest runs on the tokio runtime (async `run_ingest` which
//!   internally `spawn_blocking`s the candle embedding call). Status is
//!   pushed back to the UI via `Entity<IngestState>::update`.
//! - The ingested-sources table is refreshed via `spawn_blocking` over
//!   `SqliteStore::open` + `list_sources` (rusqlite is synchronous).
//!
//! ## State machine (per pending file)
//!
//! ```text
//! Pending ──(start click)──▶ Ingesting ──(ok)──────▶ Done
//!                                     ├──(skipped)─▶ Skipped
//!                                     └──(err)─────▶ Failed(msg)
//! ```
//!
//! ## AhaClient strategy (Case B)
//!
//! Even when the G5 service is `Running`, we always construct a fresh
//! `AhaClient::init_embed_only` (embedding-only, no LLM) for the ingest run
//! and drop it when done. Trading a few hundred MB / tens of seconds for
//! zero cross-page state sharing between ServicePage and IngestPage.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use gpui::prelude::*;
use gpui::{App, AsyncApp, Context, Entity, IntoElement, Render, Window, div};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::scroll::ScrollableElement;
use gpui_component::{ActiveTheme as _, Disableable, IconName, StyledExt};

use crate::aha_provider::AhaClient;
use crate::config::AppConfig;
use crate::ingest::pipeline::{self, IngestCounts};
use crate::models::SourceRecord;
use crate::store::sqlite_store::SqliteStore;

use super::app::AppState;

/// File extensions accepted by the rfd picker and the folder-recursion filter.
/// Mirrors `loader.rs`'s supported-type list (pdf, docx, pptx, xlsx, md, txt).
const SUPPORTED_EXTS: &[&str] = &["pdf", "docx", "pptx", "xlsx", "md", "txt"];

/// rfd filter name shown in the native "open files" dialog.
const PICKER_FILTER_NAME: &str = "Documents";

/// Per-file lifecycle in the pending-ingest queue. Drives the status badge
/// color, the visible label, and whether the "开始摄入" button may re-fire
/// for that row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IngestStatus {
    /// File selected by the user, not yet submitted.
    Pending,
    /// `run_ingest` is in flight for this file on the tokio runtime.
    Ingesting,
    /// Ingest succeeded (0 or more chunks written).
    Done,
    /// Ingest skipped (unchanged hash, empty content, etc.).
    Skipped,
    /// Ingest failed; the string is a human-readable reason.
    Failed(String),
}

impl IngestStatus {
    /// Chinese label shown in the per-file status badge.
    fn label_cn(&self) -> &'static str {
        match self {
            IngestStatus::Pending => "待摄入",
            IngestStatus::Ingesting => "摄入中...",
            IngestStatus::Done => "完成",
            IngestStatus::Skipped => "已跳过",
            IngestStatus::Failed(_) => "失败",
        }
    }

    /// RGB fill color (`0xRRGGBB`) for the status badge.
    fn badge_color(&self) -> u32 {
        match self {
            IngestStatus::Pending => 0x6b7280,
            IngestStatus::Ingesting => 0xf59e0b,
            IngestStatus::Done => 0x10b981,
            IngestStatus::Skipped => 0x6b7280,
            IngestStatus::Failed(_) => 0xef4444,
        }
    }
}

/// A single entry in the pending-ingest queue. Parallel vecs of paths and
/// statuses are intentionally avoided — pairing them in a struct keeps the
/// indices consistent across `Entity::update` calls.
#[derive(Debug, Clone)]
pub struct PendingFile {
    /// Absolute path selected by the user (file or discovered via folder walk).
    pub path: PathBuf,
    /// Current lifecycle phase.
    pub status: IngestStatus,
}

/// Per-page runtime state held as a GPUI entity so background tasks can push
/// state transitions back through `Entity::update` without borrowing the
/// view struct across await points.
pub struct IngestState {
    /// Queue of files selected by the user, each tagged with its lifecycle
    /// status. Cleared by the "清空" button or appended to by the pickers.
    pub pending: Vec<PendingFile>,
    /// List of already-ingested sources read from SQLite. Refreshed on page
    /// load and after every successful ingest run.
    pub sources: Vec<SourceRecord>,
    /// Top-level error surfaced to the user in a red box (picker failure,
    /// embed-client init failure, etc.). Cleared on the next successful
    /// action.
    pub last_error: Option<String>,
    /// Aggregate counts from the most recent ingest run, surfaced as a small
    /// info line under the pending list.
    pub last_counts: Option<IngestCounts>,
    /// `true` while an ingest run is in flight — disables the "开始摄入" and
    /// picker buttons to prevent double-submission.
    pub ingest_running: bool,
}

impl IngestState {
    fn new() -> Self {
        Self {
            pending: Vec::new(),
            sources: Vec::new(),
            last_error: None,
            last_counts: None,
            ingest_running: false,
        }
    }
}

/// G7 document-ingest page view. Owns an [`Entity<IngestState>`] that the
/// picker / ingest / refresh background tasks mutate through GPUI entity
/// updates.
pub struct IngestPage {
    /// Shared [`AppState`] (config + tokio handle). Cloned into spawned tasks.
    app: Entity<AppState>,
    /// Per-page runtime state (pending queue, source list, last error).
    state: Entity<IngestState>,
}

impl IngestPage {
    /// Construct the page. Called from [`super::root_view`] when the
    /// [`super::pages::Page::Ingest`] variant is visible.
    ///
    /// Kicks off an initial sources-list refresh in the background so the
    /// lower table is populated on first paint without blocking UI startup.
    pub fn new(app: Entity<AppState>, _window: &mut Window, cx: &mut Context<Self>) -> Self {
        let state = cx.new(|_| IngestState::new());
        // Fire-and-forget initial source-list refresh.
        on_refresh_sources_clicked(&state, &app, cx);
        Self { app, state }
    }
}

impl Render for IngestPage {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (pending, sources, last_error, ingest_running) = self.state.read(cx).snapshot();

        let state_pick_files = self.state.clone();
        let app_pick_files = self.app.clone();
        let state_pick_folder = self.state.clone();
        let app_pick_folder = self.app.clone();
        let state_clear = self.state.clone();
        let state_start = self.state.clone();
        let app_start = self.app.clone();
        let state_refresh = self.state.clone();
        let app_refresh = self.app.clone();

        let picker_buttons = div()
            .flex()
            .items_center()
            .gap_2()
            .flex_wrap()
            .child(
                Button::new("ingest-pick-files")
                    .label("选择文件")
                    .primary()
                    .icon(IconName::File)
                    .disabled(ingest_running)
                    .on_click(move |_ev, _window, cx: &mut App| {
                        on_pick_files_clicked(&state_pick_files, &app_pick_files, cx);
                    }),
            )
            .child(
                Button::new("ingest-pick-folder")
                    .label("选择文件夹")
                    .icon(IconName::Folder)
                    .disabled(ingest_running)
                    .on_click(move |_ev, _window, cx: &mut App| {
                        on_pick_folder_clicked(&state_pick_folder, &app_pick_folder, cx);
                    }),
            )
            .child(
                Button::new("ingest-drag-drop")
                    .label("拖放到此处（敬请期待）")
                    .disabled(true),
            );

        let action_buttons = div()
            .flex()
            .items_center()
            .gap_2()
            .child(
                Button::new("ingest-start")
                    .label("开始摄入")
                    .primary()
                    .icon(IconName::ArrowRight)
                    .loading(ingest_running)
                    .disabled(ingest_running || pending.is_empty())
                    .on_click(move |_ev, _window, cx: &mut App| {
                        on_start_ingest_clicked(&state_start, &app_start, cx);
                    }),
            )
            .child(
                Button::new("ingest-clear")
                    .label("清空")
                    .icon(IconName::Delete)
                    .disabled(ingest_running || pending.is_empty())
                    .on_click(move |_ev, _window, cx: &mut App| {
                        state_clear.update(cx, |s, cx| {
                            s.pending.clear();
                            s.last_error = None;
                            s.last_counts = None;
                            cx.notify();
                        });
                    }),
            );

        // Pending file list rows
        let mut pending_rows = div().flex().flex_col().gap_1();
        for (idx, pf) in pending.iter().enumerate() {
            let dot = pf.status.badge_color();
            let label = pf.status.label_cn().to_string();
            let failed_detail = match &pf.status {
                IngestStatus::Failed(m) => Some(m.clone()),
                _ => None,
            };
            let path_text = pf.path.to_str().unwrap_or("<non-utf8 path>").to_string();

            let mut row = div()
                .flex()
                .items_center()
                .justify_between()
                .gap_3()
                .p_2()
                .rounded_md()
                .border_1()
                .border_color(cx.theme().border)
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .flex_1()
                        .min_w_0()
                        .child(div().w_2().h_2().rounded_full().bg(gpui::rgb(dot)))
                        .child(div().text_sm().child(label).text_color(gpui::rgb(dot)))
                        .child(
                            div()
                                .text_sm()
                                .text_color(cx.theme().muted_foreground)
                                .flex_1()
                                .min_w_0()
                                .overflow_x_hidden()
                                .text_ellipsis()
                                .child(path_text),
                        ),
                );

            if let Some(detail) = failed_detail {
                row = row.child(
                    div()
                        .text_sm()
                        .text_color(gpui::red())
                        .child(truncate(&detail, 120)),
                );
            }

            _ = idx;
            pending_rows = pending_rows.child(row);
        }

        let pending_section = if pending.is_empty() {
            div()
                .p_4()
                .rounded_md()
                .border_1()
                .border_color(cx.theme().border)
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .text_center()
                .child("尚未选择文件。点击上方按钮选择要摄入的文档或文件夹。")
        } else {
            pending_rows
        };

        // Source table
        let source_table = if sources.is_empty() {
            div()
                .p_4()
                .rounded_md()
                .border_1()
                .border_color(cx.theme().border)
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .text_center()
                .child("暂无已摄入文档。")
        } else {
            let mut table = div().flex().flex_col().gap_1();
            // header
            table = table.child(
                div()
                    .flex()
                    .gap_3()
                    .px_2()
                    .py_1()
                    .text_sm()
                    .font_semibold()
                    .text_color(cx.theme().muted_foreground)
                    .child(div().flex_1().min_w_0().child("路径"))
                    .child(div().w_20().child("类型"))
                    .child(div().w_20().child("Chunks"))
                    .child(div().w_24().child("大小")),
            );

            for src in sources.iter().rev().take(200) {
                let size_kb = src.byte_size / 1024;
                let size_str = if size_kb == 0 {
                    format!("{} B", src.byte_size)
                } else if size_kb < 1024 {
                    format!("{size_kb} KB")
                } else {
                    format!("{:.1} MB", size_kb as f64 / 1024.0)
                };
                table = table.child(
                    div()
                        .flex()
                        .gap_3()
                        .px_2()
                        .py_1()
                        .rounded_sm()
                        .text_sm()
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .overflow_x_hidden()
                                .text_ellipsis()
                                .child(src.source_path.clone()),
                        )
                        .child(div().w_20().child(src.file_type.clone()))
                        .child(div().w_20().child(src.chunk_count.to_string()))
                        .child(
                            div()
                                .w_24()
                                .text_color(cx.theme().muted_foreground)
                                .child(size_str),
                        ),
                );
            }
            table
        };

        let counts_line = self
            .state
            .read(cx)
            .last_counts
            .as_ref()
            .map(|c| (c.ok, c.skipped, c.failed))
            .map(|(ok, skipped, failed)| {
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(format!(
                        "上次摄入结果：成功 {ok}，跳过 {skipped}，失败 {failed}"
                    ))
            });

        let mut body = div()
            .size_full()
            .p_6()
            .flex()
            .flex_col()
            .gap_4()
            .overflow_y_scrollbar()
            .child(div().text_xl().font_semibold().child("文档摄入"))
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(
                        "选择本地文件或文件夹，lorag 将分块、向量化后写入本地 LanceDB + SQLite。",
                    ),
            )
            .child(picker_buttons);

        if let Some(err) = last_error {
            body = body.child(
                div()
                    .p_3()
                    .rounded_md()
                    .border_1()
                    .border_color(gpui::red())
                    .text_color(gpui::red())
                    .text_sm()
                    .child(err),
            );
        }

        body = body
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(div().text_base().font_medium().child("待摄入队列"))
                    .child(action_buttons),
            )
            .child(pending_section);

        if let Some(c) = counts_line {
            body = body.child(c);
        }

        body = body
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .pt_2()
                    .child(div().text_base().font_medium().child("已摄入文档"))
                    .child(
                        Button::new("ingest-refresh-sources")
                            .label("刷新")
                            .icon(IconName::Redo)
                            .on_click(move |_ev, _window, cx: &mut App| {
                                on_refresh_sources_clicked(&state_refresh, &app_refresh, cx);
                            }),
                    ),
            )
            .child(source_table)
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child("提示：更换 EMBED_MODEL 后需在 CLI 执行 `lorag reindex` 重建向量索引。"),
            );

        body
    }
}

/// Snapshot helper — reads the state once and clones out the bits the render
/// closure needs, avoiding repeated `state.read(cx)` calls inside closures.
impl IngestState {
    fn snapshot(&self) -> (Vec<PendingFile>, Vec<SourceRecord>, Option<String>, bool) {
        (
            self.pending.clone(),
            self.sources.clone(),
            self.last_error.clone(),
            self.ingest_running,
        )
    }
}

// ──────────────────────────────────────────────────────────────────────
// Button-click handlers (free fns so they accept `&mut App` directly)
// ──────────────────────────────────────────────────────────────────────

/// "选择文件" handler: open the native multi-file picker via `rfd` (dispatched
/// onto `spawn_blocking` because `FileDialog::pick_files` is a blocking modal
/// syscall). Append the selected paths to the pending queue.
fn on_pick_files_clicked(state: &Entity<IngestState>, app: &Entity<AppState>, cx: &mut App) {
    let tokio_handle = app.read(cx).tokio_handle.clone();
    let state_for_task = state.clone();

    state.update(cx, |s, cx| {
        s.last_error = None;
        cx.notify();
    });

    cx.spawn(async move |cx: &mut AsyncApp| {
        let join = tokio_handle.spawn_blocking(move || {
            rfd::FileDialog::new()
                .set_title("选择要摄入的文件")
                .add_filter(PICKER_FILTER_NAME, SUPPORTED_EXTS)
                .pick_files()
                .unwrap_or_default()
        });

        let picked = match join.await {
            Ok(v) => v,
            Err(join_err) => {
                state_for_task.update(cx, |s, cx| {
                    s.last_error = Some(format!("打开文件选择器失败：后台任务中断 ({join_err})"));
                    cx.notify();
                });
                return;
            }
        };

        if !picked.is_empty() {
            state_for_task.update(cx, |s, cx| {
                for p in picked {
                    s.pending.push(PendingFile {
                        path: p,
                        status: IngestStatus::Pending,
                    });
                }
                cx.notify();
            });
        }
    })
    .detach();
}

/// "选择文件夹" handler: open the native single-folder picker, then
/// recursively walk the directory looking for files with supported
/// extensions. Permission errors on individual entries are logged via
/// `tracing::warn!` and skipped — they do not abort the walk.
fn on_pick_folder_clicked(state: &Entity<IngestState>, app: &Entity<AppState>, cx: &mut App) {
    let tokio_handle = app.read(cx).tokio_handle.clone();
    let state_for_task = state.clone();

    state.update(cx, |s, cx| {
        s.last_error = None;
        cx.notify();
    });

    cx.spawn(async move |cx: &mut AsyncApp| {
        let join = tokio_handle.spawn_blocking(move || {
            let folder = rfd::FileDialog::new()
                .set_title("选择要摄入的文件夹")
                .pick_folder();
            let Some(folder) = folder else {
                return Vec::new();
            };
            walk_dir_supported(&folder)
        });

        let picked = match join.await {
            Ok(v) => v,
            Err(join_err) => {
                state_for_task.update(cx, |s, cx| {
                    s.last_error = Some(format!("打开文件夹选择器失败：后台任务中断 ({join_err})"));
                    cx.notify();
                });
                return;
            }
        };

        if picked.is_empty() {
            state_for_task.update(cx, |s, cx| {
                s.last_error =
                    Some("所选文件夹中未找到支持的文档（pdf/docx/pptx/xlsx/md/txt）。".into());
                cx.notify();
            });
        } else {
            state_for_task.update(cx, |s, cx| {
                for p in picked {
                    s.pending.push(PendingFile {
                        path: p,
                        status: IngestStatus::Pending,
                    });
                }
                cx.notify();
            });
        }
    })
    .detach();
}

/// "开始摄入" handler: snapshot the pending paths, flip them all to
/// `Ingesting`, then on the tokio runtime build a fresh embed-only
/// [`AhaClient`] (Case B) and call `run_ingest` one file at a time so each
/// row can transition to `Done` / `Skipped` / `Failed(msg)` independently.
/// Refreshes the sources table at the end.
fn on_start_ingest_clicked(state: &Entity<IngestState>, app: &Entity<AppState>, cx: &mut App) {
    let (cfg, tokio_handle) =
        app.read_with(cx, |a, _cx| (Arc::clone(&a.cfg), a.tokio_handle.clone()));

    let pending_count = state.read(cx).pending.len();
    if pending_count == 0 {
        return;
    }
    if state.read(cx).ingest_running {
        return;
    }

    // Reset all pending rows to Ingesting at t=0 (any previously Failed/Done
    // rows that were not cleared get another shot).
    state.update(cx, |s, cx| {
        s.ingest_running = true;
        s.last_error = None;
        s.last_counts = None;
        for pf in s.pending.iter_mut() {
            pf.status = IngestStatus::Ingesting;
        }
        cx.notify();
    });

    let state_for_task = state.clone();
    let state_for_refresh = state.clone();
    let app_for_refresh = app.clone();

    cx.spawn(async move |cx: &mut AsyncApp| {
        // Snapshot the paths under the lock-free read before moving work to
        // tokio — the vec is consumed by the ingest loop below.
        let paths: Vec<PathBuf> = state_for_task.read_with(cx, |s, _cx| {
            s.pending.iter().map(|p| p.path.clone()).collect::<Vec<_>>()
        });

        let exts: Vec<String> = SUPPORTED_EXTS.iter().map(|s| (*s).to_string()).collect();

        let cfg_for_init = (*cfg).clone();
        let init_join =
            tokio_handle.spawn(async move { AhaClient::init_embed_only(cfg_for_init).await });

        let client = match init_join.await {
            Ok(Ok(c)) => c,
            Ok(Err(e)) => {
                state_for_task.update(cx, |s, cx| {
                    s.ingest_running = false;
                    s.last_error = Some(format!(
                        "加载嵌入模型失败：{e:#}（run: lorag models pull 检查 EMBED_MODEL）"
                    ));
                    for pf in s.pending.iter_mut() {
                        pf.status = IngestStatus::Failed("嵌入模型加载失败".into());
                    }
                    cx.notify();
                });
                return;
            }
            Err(join_err) => {
                state_for_task.update(cx, |s, cx| {
                    s.ingest_running = false;
                    s.last_error = Some(format!("后台任务中断 ({join_err})"));
                    cx.notify();
                });
                return;
            }
        };

        let mut counts = IngestCounts::default();

        for (idx, path) in paths.iter().enumerate() {
            let single = vec![path.clone()];
            let client_clone = client.clone();
            let cfg_clone = (*cfg).clone();
            let exts_clone = exts.clone();
            let join = tokio_handle.spawn_blocking(move || {
                let rt = tokio::runtime::Handle::current();
                rt.block_on(async move {
                    ingest_one_file(client_clone, cfg_clone, single, exts_clone).await
                })
            });

            let result = join.await;

            state_for_task.update(cx, |s, cx| {
                let Some(pf) = s.pending.get_mut(idx) else {
                    return;
                };
                match result {
                    Ok(Ok((status, file_counts))) => {
                        counts.ok += file_counts.ok;
                        counts.skipped += file_counts.skipped;
                        counts.failed += file_counts.failed;
                        pf.status = status;
                    }
                    Ok(Err(e)) => {
                        counts.failed += 1;
                        pf.status = IngestStatus::Failed(format!("{e:#}"));
                    }
                    Err(join_err) => {
                        counts.failed += 1;
                        pf.status = IngestStatus::Failed(format!("后台任务中断 ({join_err})"));
                    }
                }
                cx.notify();
            });
        }

        state_for_task.update(cx, |s, cx| {
            s.ingest_running = false;
            s.last_counts = Some(counts);
            cx.notify();
        });

        // Drop the embed-only client before refreshing the table.
        drop(client);

        // Inline refresh (mirrors `on_refresh_sources_clicked` but takes
        // `&mut AsyncApp` since we're already inside a `cx.spawn` future).
        let (cfg, tokio_handle) =
            app_for_refresh.read_with(cx, |a, _cx| (Arc::clone(&a.cfg), a.tokio_handle.clone()));
        let sqlite_path = cfg.sqlite_path.clone();
        let refresh_join =
            tokio_handle.spawn_blocking(move || -> anyhow::Result<Vec<SourceRecord>> {
                if !sqlite_path.exists() {
                    return Ok(Vec::new());
                }
                let store = SqliteStore::open(&sqlite_path)?;
                store.list_sources()
            });
        if let Ok(Ok(sources)) = refresh_join.await {
            state_for_refresh.update(cx, |s, cx| {
                s.sources = sources;
                cx.notify();
            });
        }
    })
    .detach();
}

/// "刷新" handler: re-read `SqliteStore::list_sources` on a blocking thread
/// and replace the in-memory `sources` vec on the UI entity.
pub(crate) fn on_refresh_sources_clicked(
    state: &Entity<IngestState>,
    app: &Entity<AppState>,
    cx: &mut App,
) {
    let (cfg, tokio_handle) =
        app.read_with(cx, |a, _cx| (Arc::clone(&a.cfg), a.tokio_handle.clone()));
    let state_for_task = state.clone();

    cx.spawn(async move |cx: &mut AsyncApp| {
        let sqlite_path = cfg.sqlite_path.clone();
        let join = tokio_handle.spawn_blocking(move || -> anyhow::Result<Vec<SourceRecord>> {
            if !sqlite_path.exists() {
                return Ok(Vec::new());
            }
            let store = SqliteStore::open(&sqlite_path).map_err(|e| {
                anyhow::anyhow!("failed to open sqlite at {}: {e:#}", sqlite_path.display())
            })?;
            store
                .list_sources()
                .map_err(|e| anyhow::anyhow!("failed to list sources: {e:#}"))
        });

        match join.await {
            Ok(Ok(sources)) => {
                state_for_task.update(cx, |s, cx| {
                    s.sources = sources;
                    cx.notify();
                });
            }
            Ok(Err(e)) => {
                state_for_task.update(cx, |s, cx| {
                    s.last_error = Some(format!("读取已摄入源列表失败：{e:#}"));
                    cx.notify();
                });
            }
            Err(join_err) => {
                state_for_task.update(cx, |s, cx| {
                    s.last_error = Some(format!("读取已摄入源列表失败：后台任务中断 ({join_err})"));
                    cx.notify();
                });
            }
        }
    })
    .detach();
}

// ──────────────────────────────────────────────────────────────────────
// Helpers
// ──────────────────────────────────────────────────────────────────────

/// Recursively walk `dir`, collecting files whose extension is in
/// [`SUPPORTED_EXTS`]. Consumes `read_dir` errors per-entry (permission
/// denied, reparse points, etc.) with a `tracing::warn!` and keeps going.
fn walk_dir_supported(dir: &Path) -> Vec<PathBuf> {
    let exts: Vec<String> = SUPPORTED_EXTS.iter().map(|s| (*s).to_string()).collect();
    let mut out = Vec::new();
    walk_dir_inner(dir, &exts, &mut out);
    out.sort();
    out
}

fn walk_dir_inner(dir: &Path, exts: &[String], out: &mut Vec<PathBuf>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) => {
            tracing::warn!("skipping unreadable directory {}: {e}", dir.display());
            return;
        }
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let md = match entry.metadata() {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!("skipping unreadable entry {}: {e}", path.display());
                continue;
            }
        };
        if md.is_dir() {
            walk_dir_inner(&path, exts, out);
        } else if ext_matches(&path, exts) {
            out.push(path);
        }
    }
}

fn ext_matches(path: &Path, exts: &[String]) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| exts.iter().any(|x| x.eq_ignore_ascii_case(e)))
        .unwrap_or(false)
}

/// Run `run_ingest` for exactly one file. The pipeline returns aggregated
/// `IngestCounts`, but we also derive the per-file status from its
/// side-channel `println!`/tracing output: when a file is skipped (unchanged
/// hash / empty content) the counts come back with `ok=0, skipped=1` and no
/// error; when it succeeds `ok=1`; when it errors the outer `Result` is
/// `Err`. We translate counts → status for the per-row badge.
async fn ingest_one_file(
    client: AhaClient,
    cfg: AppConfig,
    path: Vec<PathBuf>,
    exts: Vec<String>,
) -> anyhow::Result<(IngestStatus, IngestCounts)> {
    let res = pipeline::run_ingest(&client, &cfg, &path, &exts, false, false).await?;
    let status = if res.ok > 0 {
        IngestStatus::Done
    } else {
        IngestStatus::Skipped
    };
    Ok((status, res))
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max).collect();
        out.push('…');
        out
    }
}
