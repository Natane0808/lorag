//! SQLite 元数据存储。
//!
//! 职责：
//! - 初始化表结构（`sources` + `chunks` + `messages`）
//! - 摄入幂等：按 `(source_path, source_hash)` 判断是否需要重摄入
//! - 来源追踪：列出已摄入文件（`lorag sources list`）
//! - chat 多轮：持久化 `messages` 表（M7 实装），按 session 加载最近 N 条

use std::path::Path;

use anyhow::{Context, Result};
use rusqlite::params;

use crate::models::{Chunk, MessageRecord, SourceRecord};

/// SQLite 元数据存储句柄。
pub struct SqliteStore {
    conn: rusqlite::Connection,
}

impl SqliteStore {
    /// 打开（或创建）SQLite 数据库并建表。
    pub fn open(path: &Path) -> Result<Self> {
        let conn = rusqlite::Connection::open(path)
            .with_context(|| format!("failed to open sqlite: {}", path.display()))?;

        // 外键约束
        conn.execute_batch("PRAGMA foreign_keys = ON;")
            .context("failed to enable foreign keys")?;

        let slf = Self { conn };
        slf.init_tables()?;
        Ok(slf)
    }

    /// 创建表（幂等 `IF NOT EXISTS`）。
    fn init_tables(&self) -> Result<()> {
        self.conn
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS sources (
                    id            INTEGER PRIMARY KEY AUTOINCREMENT,
                    source_path   TEXT NOT NULL UNIQUE,
                    source_hash   TEXT NOT NULL,
                    file_type     TEXT NOT NULL,
                    ingested_at   TEXT NOT NULL,
                    chunk_count   INTEGER NOT NULL,
                    byte_size     INTEGER NOT NULL
                );

                CREATE TABLE IF NOT EXISTS chunks (
                    id            INTEGER PRIMARY KEY AUTOINCREMENT,
                    source_id     INTEGER NOT NULL REFERENCES sources(id) ON DELETE CASCADE,
                    chunk_ordinal INTEGER NOT NULL,
                    char_count    INTEGER NOT NULL,
                    text          TEXT NOT NULL DEFAULT '',
                    UNIQUE(source_id, chunk_ordinal)
                );

                CREATE INDEX IF NOT EXISTS idx_chunks_source ON chunks(source_id);

                -- M9 hybrid retrieval: BM25 FTS5 full-text index
                CREATE VIRTUAL TABLE IF NOT EXISTS chunks_fts USING fts5(text);

                -- M7 chat: 每条消息属于一个 session，ordinal session 内从 0 严格递增
                CREATE TABLE IF NOT EXISTS messages (
                    id          INTEGER PRIMARY KEY AUTOINCREMENT,
                    session_id  TEXT NOT NULL,
                    role        TEXT NOT NULL,
                    content     TEXT NOT NULL,
                    ordinal     INTEGER NOT NULL,
                    created_at  TEXT NOT NULL,
                    UNIQUE(session_id, ordinal)
                );

                CREATE INDEX IF NOT EXISTS idx_messages_session ON messages(session_id, ordinal);",
            )
            .context("failed to create sqlite tables")?;

        // 迁移：旧 chunks 表可能没有 text 列（M9 前创建）
        self.try_add_text_column()?;

        Ok(())
    }

    /// 安全迁移：给已有 `chunks` 表加 `text` 列（不存在则跳过）。
    fn try_add_text_column(&self) -> Result<()> {
        let has_text: bool = self.conn.prepare("SELECT text FROM chunks LIMIT 0").is_ok();
        if has_text {
            return Ok(());
        }
        // 列不存在 → 加列 + 给旧行填空文本
        self.conn
            .execute_batch(
                "ALTER TABLE chunks ADD COLUMN text TEXT NOT NULL DEFAULT '';
                 INSERT INTO chunks_fts(chunks_fts) VALUES('rebuild');",
            )
            .context("failed to add text column to chunks table (M9 migration: run `lorag reindex` if this persists)")?;
        tracing::info!("migrated sqlite: added text column to chunks + rebuilt FTS5 index");
        Ok(())
    }

    /// 按 `source_path` 查找已摄入记录（如果存在）。
    ///
    /// 返回 `Some(SourceRecord)` 或 `None`。
    pub fn find_source(&self, source_path: &str) -> Result<Option<SourceRecord>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT source_path, source_hash, file_type, chunk_count, byte_size
                 FROM sources WHERE source_path = ?1",
            )
            .context("failed to prepare find_source query")?;

        let mut rows = stmt
            .query_map(params![source_path], |row| {
                Ok(SourceRecord {
                    source_path: row.get(0)?,
                    source_hash: row.get(1)?,
                    file_type: row.get(2)?,
                    chunk_count: row.get(3)?,
                    byte_size: row.get(4)?,
                })
            })
            .context("failed to query sources")?;

        match rows.next() {
            Some(Ok(record)) => Ok(Some(record)),
            Some(Err(e)) => Err(e.into()),
            None => Ok(None),
        }
    }

    /// 插入（或 `--force` 时替换）source 记录。
    ///
    /// 返回 `source_id`（主键）。
    pub fn upsert_source(&self, record: &SourceRecord, force: bool) -> Result<i64> {
        if force {
            // 先删旧记录（CASCADE 会同时删 chunks）
            self.conn
                .execute(
                    "DELETE FROM sources WHERE source_path = ?1",
                    params![record.source_path],
                )
                .context("failed to delete old source record (force)")?;
        }

        self.conn
            .execute(
                "INSERT OR REPLACE INTO sources (source_path, source_hash, file_type, ingested_at, chunk_count, byte_size)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    record.source_path,
                    record.source_hash,
                    record.file_type,
                    chrono::Utc::now().to_rfc3339(),
                    record.chunk_count,
                    record.byte_size as i64,
                ],
            )
            .context("failed to insert source record")?;

        Ok(self.conn.last_insert_rowid())
    }

    /// 批量插入 chunk 记录（关联到给定 `source_id`）。
    ///
    /// M9 起同时写入 `chunks.text` 和 `chunks_fts` 全文索引。
    pub fn insert_chunks(&self, source_id: i64, chunks: &[Chunk]) -> Result<()> {
        let mut insert_chunk = self
            .conn
            .prepare(
                "INSERT INTO chunks (source_id, chunk_ordinal, char_count, text) VALUES (?1, ?2, ?3, ?4)",
            )
            .context("failed to prepare insert_chunks statement")?;

        let mut insert_fts = self
            .conn
            .prepare("INSERT INTO chunks_fts(text) VALUES (?1)")
            .context("failed to prepare FTS5 insert")?;

        for chunk in chunks {
            insert_chunk
                .execute(params![
                    source_id,
                    chunk.ordinal as i64,
                    chunk.text.chars().count() as i64,
                    chunk.text.as_str(),
                ])
                .context("failed to insert chunk record")?;

            insert_fts
                .execute(params![chunk.text.as_str()])
                .context("failed to insert FTS5 entry")?;
        }

        Ok(())
    }

    /// M9 混合检索：BM25 FTS5 全文搜索。
    ///
    /// 搜索用户问题中的关键词，返回匹配文档文本（按 BM25 rank 降序）。
    /// `limit` 控制返回数量上限。
    pub fn search_fts(&self, query: &str, limit: usize) -> Result<Vec<String>> {
        if query.trim().is_empty() || limit == 0 {
            return Ok(Vec::new());
        }
        // FTS5 + unicode61 tokenizer：中文按单字切，英文按词切。
        // 用 OR 连接 token（不用短语/AND）——自然语言查询含补白词（"什么"、"了"），
        // 短语搜索要求所有 token 精确连续出现 → 几乎 0 匹配；AND 同样因补白词失败。
        // OR + BM25 排序：匹配更多关键词的文档自然排在前面。
        let fts_query = build_fts5_query(query);
        let sql = format!(
            "SELECT text FROM chunks_fts WHERE chunks_fts MATCH ? ORDER BY rank LIMIT {limit}"
        );
        let mut stmt = self
            .conn
            .prepare(&sql)
            .with_context(|| format!("failed to prepare FTS5 search: {query}"))?;

        let texts: Vec<String> = stmt
            .query_map(params![fts_query], |row| row.get(0))
            .context("FTS5 search failed")?
            .filter_map(|r| r.ok())
            .collect();

        Ok(texts)
    }

    /// M9: 重建 FTS5 索引（在 force 重摄入后调用，清理过时条目）。
    ///
    /// 清空 `chunks_fts` 后从 `chunks.text` 重新填入。
    pub fn rebuild_fts(&self) -> Result<()> {
        self.conn
            .execute_batch(
                "DELETE FROM chunks_fts;
                 INSERT INTO chunks_fts(text) SELECT text FROM chunks;",
            )
            .context("failed to rebuild FTS5 index")?;
        Ok(())
    }

    /// 列出所有已摄入 source（`lorag sources list`）。
    pub fn list_sources(&self) -> Result<Vec<SourceRecord>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT source_path, source_hash, file_type, chunk_count, byte_size
                 FROM sources ORDER BY source_path",
            )
            .context("failed to prepare list_sources query")?;

        let rows = stmt
            .query_map([], |row| {
                Ok(SourceRecord {
                    source_path: row.get(0)?,
                    source_hash: row.get(1)?,
                    file_type: row.get(2)?,
                    chunk_count: row.get(3)?,
                    byte_size: row.get(4)?,
                })
            })
            .context("failed to query sources list")?;

        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    // =========================================================================
    // M7 chat: messages 表
    // =========================================================================

    /// 给 session 追加一条消息，返回新行的 `id`。
    ///
    /// `role` 期望是 `"user"` / `"assistant"` / `"system"` 之一（本函数不校验，
    /// 由调用方保证语义正确）。`ordinal` 由 `MAX(ordinal)+1` 自动算，session 内
    /// 严格递增；UNIQUE(session_id, ordinal) 防止重复。
    pub fn append_message(&self, session_id: &str, role: &str, content: &str) -> Result<i64> {
        let next_ordinal: i64 = self
            .conn
            .query_row(
                "SELECT COALESCE(MAX(ordinal), -1) + 1 FROM messages WHERE session_id = ?1",
                params![session_id],
                |row| row.get(0),
            )
            .context("failed to compute next message ordinal")?;

        self.conn
            .execute(
                "INSERT INTO messages (session_id, role, content, ordinal, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    session_id,
                    role,
                    content,
                    next_ordinal,
                    chrono::Utc::now().to_rfc3339(),
                ],
            )
            .context("failed to insert message")?;

        Ok(self.conn.last_insert_rowid())
    }

    /// 加载一个 session 的最近 N 条消息，**按 ordinal 升序**返回（最早的在最前）。
    ///
    /// M7 简化：不做 token 计数，直接取最近 N 条，超长截最老的。
    pub fn load_recent_messages(
        &self,
        session_id: &str,
        limit: usize,
    ) -> Result<Vec<MessageRecord>> {
        if limit == 0 {
            return Ok(Vec::new());
        }

        // 子查询：按 ordinal DESC 取最近 N，再 ORDER BY ordinal ASC 翻回来
        let mut stmt = self
            .conn
            .prepare(
                "SELECT role, content FROM (
                    SELECT role, content, ordinal FROM messages
                    WHERE session_id = ?1
                    ORDER BY ordinal DESC
                    LIMIT ?2
                 ) ORDER BY ordinal ASC",
            )
            .context("failed to prepare load_recent_messages query")?;

        let rows = stmt
            .query_map(params![session_id, limit as i64], |row| {
                Ok(MessageRecord {
                    role: row.get(0)?,
                    content: row.get(1)?,
                })
            })
            .context("failed to query messages")?;

        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// 清空某个 session 的所有消息，返回被删条数。
    pub fn clear_session(&self, session_id: &str) -> Result<usize> {
        let n = self
            .conn
            .execute(
                "DELETE FROM messages WHERE session_id = ?1",
                params![session_id],
            )
            .context("failed to clear session")?;
        Ok(n)
    }

    /// 统计一个 session 当前的消息条数。
    pub fn session_message_count(&self, session_id: &str) -> Result<i64> {
        self.conn
            .query_row(
                "SELECT COUNT(*) FROM messages WHERE session_id = ?1",
                params![session_id],
                |row| row.get(0),
            )
            .context("failed to count session messages")
    }
}

/// 将用户自然语言问题转为 FTS5 兼容的 OR 查询。
///
/// `unicode61` tokenizer 对中文按单字切，对英文按词切。
/// 短语搜索（双引号）和隐式 AND 都太严格——用户问题含"了什么"等补白词时
/// 几乎无匹配。
///
/// 策略：提取拉丁/数字 token（保留完整词）+ 中文单字，用 `OR` 连接。
/// BM25 自动把匹配更多关键词的文档排前面。
fn build_fts5_query(query: &str) -> String {
    let mut tokens: Vec<String> = Vec::new();
    let mut latin_buf = String::new();

    for ch in query.chars() {
        // 拉丁字母 / 数字：缓冲到当前词
        if ch.is_alphanumeric() && !('\u{4e00}'..='\u{9fff}').contains(&ch) {
            latin_buf.push(ch);
        } else {
            // 遇到非拉丁字符 → flush 拉丁缓冲
            if !latin_buf.is_empty() {
                tokens.push(std::mem::take(&mut latin_buf));
            }
            // CJK 字符 → 独立 token
            if ('\u{4e00}'..='\u{9fff}').contains(&ch) {
                tokens.push(ch.to_string());
            }
            // 其他字符（标点、空格）跳过
        }
    }
    // flush 最后的拉丁缓冲
    if !latin_buf.is_empty() {
        tokens.push(latin_buf);
    }

    if tokens.is_empty() {
        // 边缘情况：无效输入 → 兜底
        return query.to_string();
    }

    tokens.join(" OR ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_open_and_init() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.db");
        let store = SqliteStore::open(&path).unwrap();
        // 能 open 且不 panic 就说明表建好了
        let sources = store.list_sources().unwrap();
        assert!(sources.is_empty());
    }

    #[test]
    fn test_upsert_and_find() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.db");
        let store = SqliteStore::open(&path).unwrap();

        let record = SourceRecord {
            source_path: "docs/readme.md".into(),
            source_hash: "abc123".into(),
            file_type: "md".into(),
            chunk_count: 5,
            byte_size: 1024,
        };

        let id = store.upsert_source(&record, false).unwrap();
        assert!(id > 0);

        let found = store.find_source("docs/readme.md").unwrap().unwrap();
        assert_eq!(found.source_hash, "abc123");
        assert_eq!(found.chunk_count, 5);
    }

    #[test]
    fn test_force_replace() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.db");
        let store = SqliteStore::open(&path).unwrap();

        let r1 = SourceRecord {
            source_path: "a.txt".into(),
            source_hash: "old".into(),
            file_type: "txt".into(),
            chunk_count: 1,
            byte_size: 10,
        };
        store.upsert_source(&r1, false).unwrap();

        let r2 = SourceRecord {
            source_path: "a.txt".into(),
            source_hash: "new".into(),
            file_type: "txt".into(),
            chunk_count: 3,
            byte_size: 30,
        };
        store.upsert_source(&r2, true).unwrap();

        let found = store.find_source("a.txt").unwrap().unwrap();
        assert_eq!(found.source_hash, "new");
        assert_eq!(found.chunk_count, 3);
    }

    #[test]
    fn test_insert_and_list_chunks() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.db");
        let store = SqliteStore::open(&path).unwrap();

        let record = SourceRecord {
            source_path: "b.txt".into(),
            source_hash: "xyz".into(),
            file_type: "txt".into(),
            chunk_count: 2,
            byte_size: 20,
        };
        let sid = store.upsert_source(&record, false).unwrap();

        let chunks = vec![
            Chunk {
                text: "hello".into(),
                ordinal: 0,
                source_path: "b.txt".into(),
            },
            Chunk {
                text: "world".into(),
                ordinal: 1,
                source_path: "b.txt".into(),
            },
        ];
        store.insert_chunks(sid, &chunks).unwrap();

        let sources = store.list_sources().unwrap();
        assert_eq!(sources.len(), 1);
    }

    #[test]
    fn test_find_nonexistent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.db");
        let store = SqliteStore::open(&path).unwrap();
        let found = store.find_source("nonexistent.txt").unwrap();
        assert!(found.is_none());
    }

    // =========================================================================
    // M7 chat: messages 表测试
    // =========================================================================

    #[test]
    fn test_append_and_load_messages() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.db");
        let store = SqliteStore::open(&path).unwrap();

        store.append_message("s1", "user", "hi").unwrap();
        store.append_message("s1", "assistant", "hello").unwrap();
        store.append_message("s1", "user", "how are you?").unwrap();

        let msgs = store.load_recent_messages("s1", 10).unwrap();
        assert_eq!(msgs.len(), 3);
        assert_eq!(msgs[0].role, "user");
        assert_eq!(msgs[0].content, "hi");
        assert_eq!(msgs[2].role, "user");
        assert_eq!(msgs[2].content, "how are you?");
    }

    #[test]
    fn test_load_recent_messages_respects_limit() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.db");
        let store = SqliteStore::open(&path).unwrap();

        for i in 0..5 {
            store
                .append_message("s1", "user", &format!("msg{i}"))
                .unwrap();
        }

        // limit=3 应该只返回最后 3 条（msg2, msg3, msg4）
        let msgs = store.load_recent_messages("s1", 3).unwrap();
        assert_eq!(msgs.len(), 3);
        assert_eq!(msgs[0].content, "msg2");
        assert_eq!(msgs[2].content, "msg4");
    }

    #[test]
    fn test_load_recent_messages_isolates_sessions() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.db");
        let store = SqliteStore::open(&path).unwrap();

        store.append_message("s1", "user", "s1-only").unwrap();
        store.append_message("s2", "user", "s2-only").unwrap();
        store.append_message("s1", "user", "s1-2").unwrap();

        let s1 = store.load_recent_messages("s1", 10).unwrap();
        let s2 = store.load_recent_messages("s2", 10).unwrap();
        assert_eq!(s1.len(), 2);
        assert_eq!(s2.len(), 1);
        assert_eq!(s1[1].content, "s1-2");
        assert_eq!(s2[0].content, "s2-only");
    }

    #[test]
    fn test_session_message_count() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.db");
        let store = SqliteStore::open(&path).unwrap();

        assert_eq!(store.session_message_count("nonexistent").unwrap(), 0);
        store.append_message("s1", "user", "a").unwrap();
        store.append_message("s1", "assistant", "b").unwrap();
        assert_eq!(store.session_message_count("s1").unwrap(), 2);
    }

    #[test]
    fn test_clear_session() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.db");
        let store = SqliteStore::open(&path).unwrap();

        store.append_message("s1", "user", "a").unwrap();
        store.append_message("s1", "user", "b").unwrap();
        store.append_message("s2", "user", "keep").unwrap();
        assert_eq!(store.session_message_count("s1").unwrap(), 2);

        let deleted = store.clear_session("s1").unwrap();
        assert_eq!(deleted, 2);
        assert_eq!(store.session_message_count("s1").unwrap(), 0);
        // s2 不受影响
        assert_eq!(store.session_message_count("s2").unwrap(), 1);
    }

    #[test]
    fn test_ordinal_increments_per_session() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.db");
        let store = SqliteStore::open(&path).unwrap();

        // 交替写两个 session —— 各自的 ordinal 从 0 严格递增
        store.append_message("s1", "user", "a").unwrap(); // s1:0
        store.append_message("s2", "user", "x").unwrap(); // s2:0
        store.append_message("s1", "user", "b").unwrap(); // s1:1
        store.append_message("s2", "user", "y").unwrap(); // s2:1

        let s1 = store.load_recent_messages("s1", 10).unwrap();
        let s2 = store.load_recent_messages("s2", 10).unwrap();
        assert_eq!(s1.len(), 2);
        assert_eq!(s1[0].content, "a");
        assert_eq!(s1[1].content, "b");
        assert_eq!(s2.len(), 2);
        assert_eq!(s2[0].content, "x");
        assert_eq!(s2[1].content, "y");
    }

    // M9: FTS5 查询构造
    #[test]
    fn test_build_fts5_query_mixed_cjk_latin() {
        // CJK + 拉丁数字混合 → 拉丁保留完整 token，CJK 单字
        let q = build_fts5_query("20210826常朕做了什么？");
        assert!(q.contains("20210826"), "expected 20210826 in: {q}");
        assert!(q.contains("常"), "expected 常 in: {q}");
        assert!(q.contains("朕"), "expected 朕 in: {q}");
        // 应使用 OR 连接
        assert!(q.contains(" OR "), "expected OR in: {q}");
        // 问号应被过滤
        assert!(!q.contains('？'), "expected no ？ in: {q}");
        assert!(!q.contains('?'), "expected no ? in: {q}");
    }

    #[test]
    fn test_build_fts5_query_pure_english() {
        let q = build_fts5_query("Rust blockchain development");
        assert!(q.contains("Rust"), "expected Rust in: {q}");
        assert!(q.contains("blockchain"), "expected blockchain in: {q}");
        assert!(q.contains("development"), "expected development in: {q}");
        assert!(q.contains(" OR "), "expected OR in: {q}");
    }

    #[test]
    fn test_build_fts5_query_empty() {
        let q = build_fts5_query("");
        assert!(q.is_empty());
    }
}
