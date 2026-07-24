//! 共享数据模型。
//!
//! 各模块间传递的基础数据结构在本文件中定义。

use serde::Serialize;

/// 一段切分后的文本块。
#[derive(Debug, Clone)]
pub struct Chunk {
    /// 纯文本内容。
    pub text: String,
    /// 块序号（从 0 开始，全局递增）。
    pub ordinal: usize,
    /// 来源文件路径（相对于 project root 的字符串）。
    pub source_path: String,
}

/// 来源文件的元数据记录（对应 SQLite `sources` 表的一行）。
#[derive(Debug, Clone, Serialize)]
pub struct SourceRecord {
    /// 文件路径（相对于 project root）。
    pub source_path: String,
    /// 文件内容的 sha256 hex 摘要。
    pub source_hash: String,
    /// 文件扩展名（不含 `.`），如 `pdf` / `docx`。
    pub file_type: String,
    /// 切出来的 chunk 数量。
    pub chunk_count: usize,
    /// 文件字节大小。
    pub byte_size: u64,
}
