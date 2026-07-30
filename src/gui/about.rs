//! G11: static about page.
//!
//! Renders read-only version / credits / quick-links content. No async work,
//! no state machine — the whole page is a pure function over the theme.
//!
//! ## Layout
//!
//! ```text
//! ┌──────────────────────────────────────────────────────────────┐
//! │  lorag                                                       │  hero
//! │  v0.1.0 · 完全本地运行的 RAG 桌面端                          │
//! ├──────────────────────────────────────────────────────────────┤
//! │  关于本项目                                                  │  section
//! │  lorag 是一个本地 RAG 工具 ...                                │
//! ├──────────────────────────────────────────────────────────────┤
//! │  技术栈                                                      │
//! │  UI: GPUI + gpui-component                                   │
//! │  LLM/Embedding: aha (Candle)                                 │
//! │  ...                                                          │
//! ├──────────────────────────────────────────────────────────────┤
//! │  相关链接  [项目主页] [aha] [GPUI]                            │
//! ├──────────────────────────────────────────────────────────────┤
//! │  版本信息  lorag 0.1.0 / aha 0.2.6 / GPUI 0.2.2 / ...        │
//! ├──────────────────────────────────────────────────────────────┤
//! │  [打开日志文件夹]  [打开数据目录]                             │  actions
//! └──────────────────────────────────────────────────────────────┘
//! ```

use std::path::PathBuf;

use gpui::prelude::*;
use gpui::{App, Context, Entity, IntoElement, Render, Window, div};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::scroll::ScrollableElement as _;
use gpui_component::{ActiveTheme as _, IconName, StyledExt as _};

use super::app::AppState;

/// Hardcoded dependency versions (mirrors `Cargo.lock` at G11 ship time).
const AHA_VERSION: &str = "0.2.6";
const GPUI_VERSION: &str = "0.2.2";
const GPUICOMPONENT_VERSION: &str = "0.5.2";
const GPUICOMPONENT_REV: &str = "57a9903f48160845aabc8b92a1e2f5348c80d439";

/// Project homepage (Codeberg).
const URL_PROJECT: &str = "https://codeberg.org/natane/lorag";
/// aha inference engine upstream.
const URL_AHA: &str = "https://github.com/jhqxxx/aha";
/// GPUI (Zed editor framework) upstream.
const URL_GPUI: &str = "https://github.com/zed-industries/zed";

/// Read-only about page entity.
///
/// Stateless: it holds an optional error string surfaced when opening a
/// folder fails, so failures can be shown inline instead of silently dropped.
pub struct AboutPage {
    /// Last error surfaced from an "open folder" action (cleared on next click).
    last_error: Option<String>,
    /// Reference to shared app state; needed to reach `tokio_handle` so the
    /// "open folder" click handlers can dispatch blocking IO (std::fs,
    /// std::process::Command) onto the tokio blocking pool via
    /// `Handle::spawn_blocking` instead of `tokio::task::spawn_blocking`.
    /// The latter panics when called from gpui's smol-driven `cx.spawn`
    /// because there is no tokio 1.x runtime "current" on that call stack;
    /// the `Handle`-based form works from any thread.
    app: Entity<AppState>,
}

impl AboutPage {
    /// Build the about page entity.
    pub fn new(app: Entity<AppState>, _window: &mut Window, _cx: &mut Context<Self>) -> Self {
        Self {
            last_error: None,
            app,
        }
    }
}

impl Render for AboutPage {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let lorag_version = env!("CARGO_PKG_VERSION");

        let section = |title: &'static str| div().text_sm().font_semibold().child(title);

        let kv_row = |key: &'static str, value: String| {
            div()
                .flex()
                .gap_2()
                .text_sm()
                .child(
                    div()
                        .text_color(cx.theme().muted_foreground)
                        .child(format!("{key}：")),
                )
                .child(div().child(value))
        };

        let stack_row = |label: &'static str, tech: &'static str| {
            div()
                .flex()
                .gap_2()
                .text_sm()
                .child(
                    div()
                        .w_32()
                        .text_color(cx.theme().muted_foreground)
                        .child(label),
                )
                .child(div().child(tech))
        };

        let divider = || div().h_px().w_full().bg(cx.theme().border);

        let mut root = div()
            .size_full()
            .overflow_y_scrollbar()
            .p_6()
            .flex()
            .flex_col()
            .gap_4();

        // ── Hero ──────────────────────────────────────────────────────
        root = root.child(
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(div().text_xl().font_semibold().child("lorag"))
                .child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(format!("v{lorag_version} · 完全本地运行的 RAG 桌面端")),
                ),
        );
        root = root.child(divider());

        // ── 关于本项目 ────────────────────────────────────────────────
        root = root.child(section("关于本项目"));
        root = root.child(
            div()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(
                    "lorag 是一个本地 RAG 工具，把文档摄入到本地向量库，通过本地 LLM 完成问答。所有数据、模型、推理都跑在你的电脑上，无需联网、无需云端服务。",
                ),
        );
        root = root.child(divider());

        // ── 技术栈 ────────────────────────────────────────────────────
        root = root.child(section("技术栈"));
        root = root.child(
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(stack_row("UI", "GPUI + gpui-component"))
                .child(stack_row("LLM / Embedding", "aha (Candle)"))
                .child(stack_row("向量库", "LanceDB"))
                .child(stack_row("RAG 编排", "rig 0.40"))
                .child(stack_row("HTTP", "axum 0.8"))
                .child(stack_row("Web UI", "SolidJS + Vite + daisyUI")),
        );
        root = root.child(divider());

        // ── 相关链接 ──────────────────────────────────────────────────
        root = root.child(section("相关链接"));
        root = root.child(
            div()
                .flex()
                .items_center()
                .flex_wrap()
                .gap_2()
                .child(
                    Button::new("about-link-project")
                        .label("项目主页")
                        .primary()
                        .icon(IconName::ExternalLink)
                        .on_click(move |_ev, _window, _cx: &mut App| {
                            if let Err(e) = crate::tray::open_browser(URL_PROJECT) {
                                eprintln!("{e:#}");
                            }
                        }),
                )
                .child(
                    Button::new("about-link-aha")
                        .label("aha（推理引擎）")
                        .primary()
                        .icon(IconName::ExternalLink)
                        .on_click(move |_ev, _window, _cx: &mut App| {
                            if let Err(e) = crate::tray::open_browser(URL_AHA) {
                                eprintln!("{e:#}");
                            }
                        }),
                )
                .child(
                    Button::new("about-link-gpui")
                        .label("GPUI")
                        .primary()
                        .icon(IconName::ExternalLink)
                        .on_click(move |_ev, _window, _cx: &mut App| {
                            if let Err(e) = crate::tray::open_browser(URL_GPUI) {
                                eprintln!("{e:#}");
                            }
                        }),
                ),
        );
        root = root.child(divider());

        // ── 版本信息 ──────────────────────────────────────────────────
        root = root.child(section("版本信息"));
        root = root.child(
            div()
                .flex()
                .flex_col()
                .gap_1()
                .font_family("mono")
                .child(kv_row("lorag", lorag_version.to_string()))
                .child(kv_row("aha", AHA_VERSION.to_string()))
                .child(kv_row("GPUI", GPUI_VERSION.to_string()))
                .child(kv_row("gpui-component", GPUICOMPONENT_VERSION.to_string()))
                .child(
                    div()
                        .flex()
                        .gap_2()
                        .text_xs()
                        .child(
                            div()
                                .text_color(cx.theme().muted_foreground)
                                .child("gpui-component rev："),
                        )
                        .child(div().child(GPUICOMPONENT_REV)),
                ),
        );
        root = root.child(divider());

        // ── 快捷按钮 ──────────────────────────────────────────────────
        root = root.child(section("快捷操作"));
        let app_for_logs = self.app.clone();
        let app_for_data = self.app.clone();
        root = root.child(
            div()
                .flex()
                .items_center()
                .flex_wrap()
                .gap_2()
                .child(
                    Button::new("about-open-logs")
                        .label("打开日志文件夹")
                        .icon(IconName::FolderOpen)
                        .on_click(cx.listener(move |this, _ev, _window, cx| {
                            on_open_folder_clicked(this, FolderKind::Logs, &app_for_logs, cx);
                        })),
                )
                .child(
                    Button::new("about-open-data")
                        .label("打开数据目录")
                        .icon(IconName::FolderOpen)
                        .on_click(cx.listener(move |this, _ev, _window, cx| {
                            on_open_folder_clicked(this, FolderKind::Data, &app_for_data, cx);
                        })),
                ),
        );
        root = root.child(
            div()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(format!("数据目录：{}", data_dir_display())),
        );

        if let Some(err) = self.last_error.clone() {
            root = root.child(div().text_sm().text_color(gpui::rgb(0xef4444)).child(err));
        }

        root
    }
}

/// Which directory the "open folder" action targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FolderKind {
    /// `dirs::data_dir()/lorag/logs` (same as G9).
    Logs,
    /// `dirs::data_dir()/lorag` (sqlite + lancedb + logs parent).
    Data,
}

/// Resolve a folder path, creating it if missing, then dispatch to the OS file
/// manager. Mirrors G9's `open_path_in_os_file_manager` / `resolve_log_dir_for_open`
/// but is scoped here to avoid a cross-module public API.
fn on_open_folder_clicked(
    this: &mut AboutPage,
    kind: FolderKind,
    app: &Entity<AppState>,
    cx: &mut Context<AboutPage>,
) {
    this.last_error = None;
    cx.notify();

    // Clone the tokio Handle out of AppState BEFORE entering the gpui smol
    // task. We deliberately use `Handle::spawn_blocking` (not
    // `tokio::task::spawn_blocking`) because `cx.spawn` runs on gpui's
    // internal smol executor where the tokio runtime is NOT set as current;
    // calling tokio's free-function spawn_blocking from there panics with
    // "there is no reactor running". The Handle-based form does not require
    // an active tokio context on the calling thread. Mirrors the working
    // pattern used by `gui_main`'s tray-command drain and (now) G9 logs page.
    let handle = app.read_with(cx, |a, _cx| a.tokio_handle.clone());

    cx.spawn(async move |this, cx| {
        let path_join = handle
            .spawn_blocking(move || -> anyhow::Result<PathBuf> {
                let base = dirs::data_dir().ok_or_else(|| {
                    anyhow::anyhow!("无法解析系统数据目录（请设置 HOME / APPDATA / XDG_DATA_HOME）")
                })?;
                let dir = match kind {
                    FolderKind::Logs => base.join("lorag").join("logs"),
                    FolderKind::Data => base.join("lorag"),
                };
                std::fs::create_dir_all(&dir)?;
                Ok(dir)
            })
            .await;

        let path = match path_join {
            Ok(Ok(p)) => p,
            Ok(Err(e)) => {
                let _ = this.update(cx, |this, cx| {
                    this.last_error = Some(format!("打开文件夹失败：{e:#}"));
                    cx.notify();
                });
                return;
            }
            Err(e) => {
                let _ = this.update(cx, |this, cx| {
                    this.last_error = Some(format!("打开文件夹失败：后台任务中断 ({e})"));
                    cx.notify();
                });
                return;
            }
        };

        let open_join = handle
            .spawn_blocking(move || open_path_in_os_file_manager(&path))
            .await;
        match open_join {
            Ok(Ok(_)) => {}
            Ok(Err(e)) => {
                let _ = this.update(cx, |this, cx| {
                    this.last_error = Some(format!("打开文件夹失败：{e}"));
                    cx.notify();
                });
            }
            Err(e) => {
                let _ = this.update(cx, |this, cx| {
                    this.last_error = Some(format!("打开文件夹失败：后台任务中断 ({e})"));
                    cx.notify();
                });
            }
        }
    })
    .detach();
}

/// Render a best-effort display string for the data dir (no IO here — just the
/// resolved `dirs::data_dir()/lorag`, or a placeholder if unresolved).
fn data_dir_display() -> String {
    dirs::data_dir()
        .map(|p| p.join("lorag").display().to_string())
        .unwrap_or_else(|| "<无法解析>".into())
}

/// Open an absolute filesystem path in the OS file manager.
/// Same platform dispatch as G9 (`src/gui/logs.rs::open_path_in_os_file_manager`).
fn open_path_in_os_file_manager(
    path: &std::path::Path,
) -> std::io::Result<std::process::ExitStatus> {
    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("explorer").arg(path).status()
    }
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open").arg(path).status()
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        std::process::Command::new("xdg-open").arg(path).status()
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos", unix)))]
    {
        let _ = path;
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "open-in-file-manager not supported on this platform",
        ))
    }
}
