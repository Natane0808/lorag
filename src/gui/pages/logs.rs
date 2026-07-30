//! G9: live log viewer moved to [`crate::gui::logs`] (top-level module, same
//! location convention as the other page implementations: `service.rs`,
//! `models.rs`, `ingest.rs`, `doctor.rs`).
//!
//! This placeholder module is kept only so `pub mod logs;` in
//! `src/gui/pages/mod.rs` continues to compile; the real page lives in
//! `crate::gui::logs::LogsPage` and is dispatched from
//! `crate::gui::root_view::render_page`. G15 may delete this stub when the
//! `pages/` directory is reorganized.
