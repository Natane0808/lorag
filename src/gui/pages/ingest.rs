//! G7: document ingest page (rfd file/folder picker + per-file progress +
//! ingested-sources table).
//!
//! The real implementation lives at [`crate::gui::ingest`]. This module is
//! retained so the `pub mod ingest;` in [`super::mod`] stays well-formed and
//! the per-page directory structure from G4 keeps looking uniform — same
//! pattern as G5 service / G6 models.
