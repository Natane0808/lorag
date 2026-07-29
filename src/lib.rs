//! lorag — Fully local Agent RAG CLI.
//!
//! 项目规划见 `../PLAN.md`；agent 协作约定见 `../AGENTS.md`。

pub mod aha_provider;
pub mod chunker;
pub mod config;
pub mod doctor;
pub mod ingest;
pub mod models;
pub mod rag;
pub mod rig_compat;
pub mod server;
pub mod store;
pub mod tray;
