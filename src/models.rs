//! 共享数据模型。
//!
//! 各模块间传递的基础数据结构在本文件中定义。

use serde::{Deserialize, Serialize};

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

/// 一条 chat 历史消息（对应 SQLite `messages` 表的一行）。
///
/// 持久化在 sqlite，多轮 chat 时把同一 session 的最近 N 条塞回 LLM context。
#[derive(Debug, Clone, Serialize)]
pub struct MessageRecord {
    /// 角色：`"user"` | `"assistant"` | `"system"`。
    pub role: String,
    /// 消息文本。
    pub content: String,
}

/// 会话摘要（M10 会话历史侧边栏用）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionInfo {
    /// 会话 ID。
    pub session_id: String,
    /// 会话标题：取第一条 user 消息（截断到 40 字）。
    pub title: String,
    /// 消息总数。
    pub message_count: i64,
    /// 最后一条消息的时间（ISO 8601）。
    pub updated_at: String,
}
