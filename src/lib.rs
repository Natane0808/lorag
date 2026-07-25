//! lorag — Fully local Agent RAG CLI.
//!
//! 项目规划见 `../PLAN.md`；agent 协作约定见 `../AGENTS.md`。
//!
//! 模块边界：
//! - `config` —— 加载 `.env`、提供 `AppConfig`
//! - `aha_provider` —— aha ↔ rig 适配 + 模型下载/加载（aha crate 唯一入口）
//! - 其余模块（`rag` / `chunker` / `ingest` / `store` / `models`）在对应 milestone 引入

pub mod aha_provider;
pub mod chunker;
pub mod config;
pub mod doctor;
pub mod ingest;
pub mod models;
pub mod rag;
pub mod rig_compat;
pub mod store;

// 占位：M5+ 之后逐步实装
