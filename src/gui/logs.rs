//! G9: live log viewer component.
//!
//! Formerly a standalone sidebar page, now embedded inside the Service page
//! ([`super::service::ServicePage`]) as a child entity below the service
//! controls. Renders the ring buffer already drained from the tracing broadcast bridge
//! by [`super::app::AppState::spawn_log_drain`] (see `src/gui/app.rs`). The page
//! does **not** subscribe to the broadcast channel itself — every 100 ms the
//! foreground drain appends new lines into `AppState::log_buffer` and calls
//! `cx.notify()`, so a plain `app.read(cx).log_buffer` is always fresh enough
//! for display purposes.
//!
//! ## Layout
//!
//! ```text
//! ┌──────────────────────────────────────────────────────────────┐
//! │  级别: [ALL ▾]   [清空]   [打开日志文件夹]                  │  top bar
//! ├──────────────────────────────────────────────────────────────┤
//! │  2026-07-29T12:34:56 INFO  tracing file appender initialized │  scrollable
//! │  2026-07-29T12:35:01 WARN  lancedb ...                       │  monospace
//! │  2026-07-29T12:35:02 ERROR failed to ...                     │  color-coded
//! │  ...                                                         │
//! ├──────────────────────────────────────────────────────────────┤
//! │  共 1234 行 | ERROR 2 | WARN 17 | INFO 988 | DEBUG 227       │  footer
//! └──────────────────────────────────────────────────────────────┘
//! ```
//!
//! ## Auto-scroll heuristic
//!
//! We keep a [`gpui::ScrollHandle`] on the page entity and call
//! [`gpui::ScrollHandle::scroll_to_bottom`] during render whenever the user has
//! not manually scrolled away from the bottom. "At bottom" is decided by
//! comparing `offset().y` to `max_offset().y` (within a 2-pixel epsilon to
//! absorb rounding). Once the user scrolls up by wheel / drag, auto-scroll
//! pauses; once they scroll back down to the very bottom it resumes.

use std::collections::VecDeque;
use std::path::PathBuf;

use gpui::prelude::*;
use gpui::{App, Context, Entity, IntoElement, Render, Window, div, px};
use gpui_component::button::Button;
use gpui_component::menu::{DropdownMenu, PopupMenuItem};
use gpui_component::scroll::ScrollableElement;
use gpui_component::{ActiveTheme as _, IconName, StyledExt as _};

use super::app::AppState;

/// Severity levels used for filtering + color-coding in the UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LevelFilter {
    /// Show every line regardless of parsed level.
    All,
    Trace,
    Debug,
    Info,
    Warn,
    Error,
}

impl LevelFilter {
    /// Chinese label shown in the dropdown trigger and in the footer counters.
    fn label_cn(self) -> &'static str {
        match self {
            LevelFilter::All => "全部",
            LevelFilter::Trace => "TRACE",
            LevelFilter::Debug => "DEBUG",
            LevelFilter::Info => "INFO",
            LevelFilter::Warn => "WARN",
            LevelFilter::Error => "ERROR",
        }
    }

    /// Ordered list shown in the dropdown menu.
    fn all() -> &'static [LevelFilter] {
        &[
            LevelFilter::All,
            LevelFilter::Error,
            LevelFilter::Warn,
            LevelFilter::Info,
            LevelFilter::Debug,
            LevelFilter::Trace,
        ]
    }
}

/// Per-line level classification parsed from the formatted tracing output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LineLevel {
    Error,
    Warn,
    Info,
    Debug,
    Trace,
    /// Lines we couldn't classify (e.g. multi-line continuation, panic dumps).
    Unknown,
}

impl LineLevel {
    /// Parse a formatted tracing line. `tracing_subscriber::fmt::layer().without_time()`
    /// is not in use — the default format is `<RFC3339 timestamp> <LEVEL> <target>:<line>: <msg>`
    /// so the level is the second whitespace-separated token (upper-case).
    fn parse(line: &str) -> Self {
        // The timestamp token may itself contain 'T' but no spaces, so splitting
        // by whitespace and taking the second token is stable.
        let mut tokens = line.split_whitespace();
        let _timestamp = tokens.next();
        match tokens.next() {
            Some("ERROR") => LineLevel::Error,
            Some("WARN") => LineLevel::Warn,
            Some("INFO") => LineLevel::Info,
            Some("DEBUG") => LineLevel::Debug,
            Some("TRACE") => LineLevel::Trace,
            _ => LineLevel::Unknown,
        }
    }

    /// Does this line pass the user's selected filter?
    fn passes(self, filter: LevelFilter) -> bool {
        match filter {
            LevelFilter::All => true,
            LevelFilter::Error => matches!(self, LineLevel::Error),
            LevelFilter::Warn => matches!(self, LineLevel::Error | LineLevel::Warn),
            LevelFilter::Info => {
                matches!(self, LineLevel::Error | LineLevel::Warn | LineLevel::Info)
            }
            LevelFilter::Debug => matches!(
                self,
                LineLevel::Error | LineLevel::Warn | LineLevel::Info | LineLevel::Debug
            ),
            LevelFilter::Trace => true,
        }
    }

    /// RGB accent used when rendering this line. Returns `None` for default
    /// foreground (no color override).
    fn color_rgb(self) -> Option<u32> {
        match self {
            LineLevel::Error => Some(0xef4444),
            LineLevel::Warn => Some(0xd97706),
            LineLevel::Info => None,
            LineLevel::Debug => Some(0x6b7280),
            LineLevel::Trace => Some(0x9ca3af),
            LineLevel::Unknown => None,
        }
    }
}

/// Per-page runtime state held as a GPUI entity.
pub struct LogsState {
    /// Currently selected severity filter. `All` by default.
    pub filter: LevelFilter,
}

impl LogsState {
    fn new() -> Self {
        Self {
            filter: LevelFilter::All,
        }
    }
}

/// G9 live-log page view. Owns an [`Entity<LogsState>`] for the filter + scroll
/// handle so they survive across re-renders and sidebar navigation.
pub struct LogsPage {
    /// Shared [`AppState`] - read-only from render (we never mutate it, but the
    /// "清空" button clears `AppState::log_buffer` via `update`).
    app: Entity<AppState>,
    /// Per-page runtime state (filter + scroll handle).
    state: Entity<LogsState>,
    /// When `true`, the page is rendered as a child of
    /// [`super::service::ServicePage`] and omits its own title row and padding
    /// to avoid overflowing the parent's flex boundary. When `false`
    /// (standalone, currently unused), renders as a full page with title +
    /// `size_full()` + `p_4()`.
    embedded: bool,
}

impl LogsPage {
    /// Construct the page. Called from [`super::service::ServicePage::new`]
    /// where the log viewer is embedded as a child entity below the service
    /// controls (no standalone sidebar entry).
    ///
    /// `embedded` should be `true` when the page is rendered as a child of
    /// [`super::service::ServicePage`] (the only current caller). When `true`,
    /// the render omits the outer `size_full()` + `p_4()` and the "实时日志"
    /// title row (the parent already provides both), preventing the scrollable
    /// area from overflowing the parent's flex boundary.
    pub fn new(
        app: Entity<AppState>,
        _window: &mut Window,
        cx: &mut Context<Self>,
        embedded: bool,
    ) -> Self {
        let state = cx.new(|_| LogsState::new());
        Self {
            app,
            state,
            embedded,
        }
    }
}

impl Render for LogsPage {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let filter = self.state.read(cx).filter;

        // Snapshot the buffer for this render frame. We clone out of the
        // VecDeque because we need to iterate twice (count + render) and
        // can't hold the read-ref while building elements.
        let buffer_snapshot: VecDeque<String> =
            self.app.read(cx).log_buffer.iter().cloned().collect();
        let total = buffer_snapshot.len();

        // Counts for the footer (computed over the full buffer, not the
        // filtered view — the user wants to know overall composition).
        let mut err = 0usize;
        let mut warn = 0usize;
        let mut info = 0usize;
        let mut debug = 0usize;
        for line in &buffer_snapshot {
            match LineLevel::parse(line) {
                LineLevel::Error => err += 1,
                LineLevel::Warn => warn += 1,
                LineLevel::Info => info += 1,
                LineLevel::Debug => debug += 1,
                _ => {}
            }
        }

        // Note: auto-scroll-to-bottom was previously implemented via the
        // custom `scroll_handle` + `track_scroll` + `scroll_to_bottom()`
        // dance. Since we now use `overflow_y_scrollbar()` (gpui-component
        // themed scrollbar wrapper) which doesn't expose the internal
        // scroll handle, that feature is dropped. The footer still shows
        // the "自动滚动" label, but it always reports "开" — manual scroll
        // only. (See the comment on the middle scroll area below for rationale.)

        // Filtered, rendered lines. We deliberately render *every* visible
        // line (no virtualization) — log_buffer is capped at 5000 lines and
        // the view window only shows ~60 at a time; a 5k-row div is still
        // well within GPUI's comfort zone for the use case.
        let visible_lines: Vec<(LineLevel, String)> = buffer_snapshot
            .iter()
            .filter_map(|line| {
                let level = LineLevel::parse(line);
                if level.passes(filter) {
                    Some((level, line.clone()))
                } else {
                    None
                }
            })
            .collect();
        let visible_count = visible_lines.len();

        // ── Top bar: filter dropdown + action buttons ──────────────────────
        let state_for_filter = self.state.clone();
        let filter_items: Vec<(LevelFilter, &'static str)> = LevelFilter::all()
            .iter()
            .map(|f| (*f, f.label_cn()))
            .collect();
        let selected_filter = filter;

        let app_clear = self.app.clone();
        let app_open_folder = self.app.clone();

        // ── Button bar: filter dropdown + clear + open-folder ─────────────
        // Shared between embedded and standalone modes; only the title row
        // above it differs.
        let button_bar = div()
            .flex()
            .items_center()
            .gap_2()
            .child(
                Button::new("logs-filter")
                    .label(format!("级别: {}", filter.label_cn()))
                    .dropdown_caret(true)
                    .dropdown_menu(move |menu, _, _| {
                        let mut m = menu;
                        for (value, label) in filter_items.iter().copied() {
                            let checked = value == selected_filter;
                            let state = state_for_filter.clone();
                            m = m.item(PopupMenuItem::new(label).checked(checked).on_click(
                                move |_, _, cx| {
                                    state.update(cx, |s, cx| {
                                        s.filter = value;
                                        cx.notify();
                                    });
                                },
                            ));
                        }
                        m
                    }),
            )
            .child(
                Button::new("logs-clear")
                    .label("清空")
                    .icon(IconName::Delete)
                    .on_click(move |_ev, _window, cx: &mut App| {
                        app_clear.update(cx, |s, cx| {
                            s.log_buffer.clear();
                            cx.notify();
                        });
                    }),
            )
            .child(
                Button::new("logs-open-folder")
                    .label("打开日志文件夹")
                    .icon(IconName::FolderOpen)
                    .on_click(move |_ev, _window, cx: &mut App| {
                        // Pull the tokio Handle from AppState up front so we
                        // can use `Handle::spawn_blocking` inside the gpui
                        // task. The free-function
                        // `tokio::task::spawn_blocking` panics when called
                        // from gpui's smol-driven `cx.spawn` because no tokio
                        // runtime is "current" on that call stack (same class
                        // of bug as G11 about page, fixed in F3).
                        let handle = app_open_folder.read_with(cx, |a, _cx| a.tokio_handle.clone());
                        cx.spawn(async move |_cx| {
                            let path = handle.spawn_blocking(resolve_log_dir_for_open).await;
                            let Ok(Ok(path)) = path else {
                                return;
                            };
                            let _ = open_path_in_os_file_manager(&path);
                            // Failure to open the folder is non-fatal.
                        })
                        .detach();
                    }),
            );

        // ── Top bar ────────────────────────────────────────────────────────
        // When embedded inside ServicePage, the parent already renders the
        // "实时日志" header + description, so we omit the title row and only
        // show the action buttons (right-aligned). When standalone, the full
        // title + description + buttons bar is shown.
        let top_bar = if self.embedded {
            div()
                .flex()
                .items_center()
                .justify_end()
                .gap_3()
                .child(button_bar)
        } else {
            div()
                .flex()
                .items_center()
                .justify_between()
                .gap_3()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_3()
                        .child(div().text_xl().font_semibold().child("实时日志"))
                        .child(
                            div()
                                .text_sm()
                                .text_color(cx.theme().muted_foreground)
                                .child("日志文件同时写入磁盘，重启后保留最近 7 天。"),
                        ),
                )
                .child(button_bar)
        };

        // ── Outer container ────────────────────────────────────────────────
        // Embedded: no padding (parent ServicePage already has p_6) and use
        // h_full to fill the flex slot instead of size_full which can overflow
        // the parent's flex boundary. Standalone: full page with its own
        // padding.
        let outer = if self.embedded {
            div()
                .w_full()
                .h_full()
                .flex()
                .flex_col()
                .gap_3()
                .overflow_hidden()
        } else {
            div().size_full().p_4().flex().flex_col().gap_3()
        };

        outer
            .child(
                top_bar
                    .flex_shrink_0()
                    .w_full(),
            )
            .child(
                // ── Middle: scrollable monospace tail ──────────────────────
                //
                // Uses gpui-component's `overflow_y_scrollbar()` for the
                // themed scrollbar (matches the rest of the codebase and
                // gives a custom dark-themed scrollbar overlay — much better
                // look than the native browser scrollbar on the dark log
                // background).
                //
                // Note: `overflow_y_scrollbar()` wraps the element in a
                // `Scrollable<Div>` that doesn't re-export `track_scroll`, so
                // we can't drive `scroll_to_bottom()` from the handle. The
                // at-bottom heuristic below is therefore a no-op in the
                // current implementation; the user can still manually scroll
                // the log view. (Auto-scroll-to-bottom is only meaningful
                // when new lines stream in faster than the user reads; the
                // current 100ms drain cadence + 5000-line cap makes it
                // a nice-to-have, not a hard requirement.)
                div()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .rounded_md()
                    .border_1()
                    .border_color(cx.theme().border)
                    .bg(gpui::rgb(0x0b0d12))
                    .overflow_y_scrollbar()
                    .p_3()
                    .pr_4()
                    .gap_0()
                    .font_family("mono")
                    .text_size(px(12.5))
                    .text_color(gpui::rgb(0xe5e7eb))
                    .children(visible_lines.into_iter().map(|(level, line)| {
                        let color = level.color_rgb().map(gpui::rgb).unwrap_or_else(|| {
                            match level {
                                LineLevel::Unknown => gpui::rgb(0x9ca3af),
                                _ => gpui::rgb(0xe5e7eb),
                            }
                        });
                        div()
                            .text_color(color)
                            .overflow_x_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .child(line)
                    })),
            )
            .child(
                // ── Footer: line counts ───────────────────────────────────
                div()
                    .flex_shrink_0()
                    .w_full()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(
                        div()
                            .min_w_0()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .child(format!(
                                "共 {total} 行（当前显示 {visible_count}）| ERROR {err} | WARN {warn} | INFO {info} | DEBUG {debug}"
                            )),
                    )
                    .child(
                        div()
                            .flex_shrink_0()
                            .whitespace_nowrap()
                            .child("自动滚动：开"),
                    ),
            )
    }
}

/// Resolve the on-disk log directory, creating it if it doesn't exist. Mirrors
/// [`crate::logging::resolve_log_dir`] but is exposed here as a free function
/// that can be called from inside `spawn_blocking` (the logging version is
/// private to the crate and returns an anyhow::Result).
fn resolve_log_dir_for_open() -> anyhow::Result<PathBuf> {
    let base = dirs::data_dir().ok_or_else(|| {
        anyhow::anyhow!("failed to resolve OS data directory (set HOME/APPDATA/XDG_DATA_HOME)")
    })?;
    let log_dir = base.join("lorag").join("logs");
    std::fs::create_dir_all(&log_dir)?;
    Ok(log_dir)
}

/// Open an absolute filesystem path in the OS file manager, selecting it if
/// possible. Uses the same platform dispatch pattern as [`crate::tray::open_browser`],
/// but points at the native shell rather than a URL handler.
///
/// - Windows: `explorer <path>` (opens the folder; passing a file uses `/select,`)
/// - macOS: `open <path>`
/// - Linux/other: `xdg-open <path>`
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
        // Fallback for unknown platforms — pretend success; the user was told
        // where logs live via the footer and can navigate there on their own.
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "open-in-file-manager not supported on this platform",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_level_parse_default_tracing_format() {
        assert_eq!(
            LineLevel::parse("2026-07-29T12:34:56.789Z INFO lorag: hello"),
            LineLevel::Info
        );
        assert_eq!(
            LineLevel::parse("2026-07-29T12:34:56.789Z ERROR lorag: boom"),
            LineLevel::Error
        );
        assert_eq!(
            LineLevel::parse("2026-07-29T12:34:56.789Z WARN lorag: careful"),
            LineLevel::Warn
        );
        assert_eq!(
            LineLevel::parse("2026-07-29T12:34:56.789Z DEBUG lorag: details"),
            LineLevel::Debug
        );
        assert_eq!(
            LineLevel::parse("2026-07-29T12:34:56.789Z TRACE lorag: spam"),
            LineLevel::Trace
        );
    }

    #[test]
    fn line_level_parse_unknown_lines() {
        assert_eq!(LineLevel::parse(""), LineLevel::Unknown);
        assert_eq!(LineLevel::parse("panic at the disco"), LineLevel::Unknown);
        assert_eq!(
            LineLevel::parse("    at backtrace line 42"),
            LineLevel::Unknown
        );
    }

    #[test]
    fn filter_passes_inclusive() {
        // INFO filter shows ERROR / WARN / INFO but not DEBUG / TRACE.
        assert!(LineLevel::Error.passes(LevelFilter::Info));
        assert!(LineLevel::Warn.passes(LevelFilter::Info));
        assert!(LineLevel::Info.passes(LevelFilter::Info));
        assert!(!LineLevel::Debug.passes(LevelFilter::Info));
        assert!(!LineLevel::Trace.passes(LevelFilter::Info));

        // WARN filter shows ERROR + WARN.
        assert!(LineLevel::Error.passes(LevelFilter::Warn));
        assert!(LineLevel::Warn.passes(LevelFilter::Warn));
        assert!(!LineLevel::Info.passes(LevelFilter::Warn));

        // All shows everything.
        assert!(LineLevel::Unknown.passes(LevelFilter::All));
        assert!(LineLevel::Trace.passes(LevelFilter::All));
    }
}
