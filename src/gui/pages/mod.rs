//! Page definitions and placeholder renderers for the 6 top-level navigation
//! destinations shown in the desktop launcher sidebar.
//!
//! Each module exposes a single placeholder struct that implements [`gpui::Render`].
//! G5–G11 replace these placeholders with real functionality (service
//! control, model management, ingestion, doctor, settings, about). The live
//! log viewer is embedded inside the Service page rather than having its own
//! sidebar entry.

pub mod about;
pub mod doctor;
pub mod ingest;
pub mod logs;
pub mod models;
pub mod service;

/// All 6 top-level pages in the order they appear in the sidebar nav.
pub const ALL_PAGES: &[Page] = &[
    Page::Service,
    Page::Models,
    Page::Ingest,
    Page::Doctor,
    Page::Settings,
    Page::About,
];

/// Identifies which top-level page is currently visible in the right-hand pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Page {
    /// G5: Service control (start/stop axum, open chat).
    Service,
    /// G6: Model management (pull / status / switch).
    Models,
    /// G7: Document ingestion (rfd picker + progress).
    Ingest,
    /// G8: 11-item doctor health-check table.
    Doctor,
    /// G10: `.env` editor form.
    Settings,
    /// G11: Version / credits / legal.
    About,
}

impl Page {
    /// Chinese label shown in the sidebar menu.
    pub fn title_cn(&self) -> &'static str {
        match self {
            Page::Service => "服务",
            Page::Models => "模型",
            Page::Ingest => "文档",
            Page::Doctor => "健康",
            Page::Settings => "设置",
            Page::About => "关于",
        }
    }

    /// Free-form icon identifier. Kept as a string label; the GUI no longer
    /// renders icon components, so this is informational only.
    pub fn icon_name(&self) -> &'static str {
        match self {
            Page::Service => "server",
            Page::Models => "cpu",
            Page::Ingest => "file-up",
            Page::Doctor => "heart-pulse",
            Page::Settings => "settings",
            Page::About => "info",
        }
    }

    /// Placeholder text rendered inside each page's content area in G4.
    pub fn placeholder_text(&self) -> &'static str {
        match self {
            Page::Service => "Service Page (placeholder)",
            Page::Models => "Models Page (placeholder)",
            Page::Ingest => "Ingest Page (placeholder)",
            Page::Doctor => "Doctor Page (placeholder)",
            Page::Settings => "Settings Page (placeholder)",
            Page::About => "About Page (placeholder)",
        }
    }
}
