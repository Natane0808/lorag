//! GPUI desktop launcher (M12).
//!
//! Native desktop GUI built on top of the existing `lorag` library.
//! Business logic lives in the library crate; this module is view-only.
//!
//! Submodules added across the G1–G14 tasks:
//! - `gpu_probe` (G1): pre-flight GPU / blade renderer probe + friendly failure
//! - `fallback_dialog` (G1): native platform dialog for when GPU init fails
//! - `logging` (G3): broadcast-channel bridge wrapper for the GUI logs page
//! - `app` (G4): global [`AppState`](app::AppState) entity (current page, log buffer, cfg)
//! - `sidebar` (G4): left-hand nav with one entry per [`pages::Page`]
//! - `pages` (G4): 7 placeholder page renderers (G5–G11 will flesh them out)
//! - `root_view` (G4): window root (sidebar + current page pane)

pub mod about;
pub mod app;
pub mod autostart;
pub mod doctor;
pub mod fallback_dialog;
pub mod gpu_probe;
pub mod ingest;
pub mod logging;
pub mod logs;
pub mod models;
pub mod pages;
pub mod root_view;
pub mod service;
pub mod settings;
pub mod sidebar;
pub mod tray_host;
