//! 文档摄入模块：loader（文件 → 纯文本）+ pipeline（text → chunks → embeddings → lancedb）。
//!
//! M2 阶段：loader 子模块（文件 → 纯文本）。
//! M3 阶段：`pipeline.rs`（chunks → EmbeddingsBuilder → lancedb）。

pub mod docx;
pub mod loader;
pub mod md;
pub mod pdf;
pub mod pipeline;
pub mod pptx;
pub mod txt;
pub mod xlsx;
