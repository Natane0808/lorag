//! G5: service control page (start/stop axum server, open web chat in browser).
//!
//! The real implementation lives at [`crate::gui::service`]. This module is
//! retained so the `pub mod service;` in [`super::mod`] stays well-formed and
//! the per-page directory structure from G4 keeps looking uniform — G6+ pages
//! will continue to live in sibling files (models.rs, ingest.rs, …).
