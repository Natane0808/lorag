//! SQLite 元数据存储。
//!
//! 职责：
//! - 初始化表结构（`sources` + `chunks`）
//! - 摄入幂等：按 `(source_path, source_hash)` 判断是否需要重摄入
//! - 来源追踪：列出已摄入文件（`lorag sources list`）

use std::path::Path;

use anyhow::{Context, Result};
use rusqlite::params;

use crate::models::{Chunk, SourceRecord};

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
                    UNIQUE(source_id, chunk_ordinal)
                );

                CREATE INDEX IF NOT EXISTS idx_chunks_source ON chunks(source_id);",
            )
            .context("failed to create sqlite tables")
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
    pub fn insert_chunks(&self, source_id: i64, chunks: &[Chunk]) -> Result<()> {
        let mut stmt = self
            .conn
            .prepare(
                "INSERT INTO chunks (source_id, chunk_ordinal, char_count) VALUES (?1, ?2, ?3)",
            )
            .context("failed to prepare insert_chunks statement")?;

        for chunk in chunks {
            stmt.execute(params![
                source_id,
                chunk.ordinal as i64,
                chunk.text.chars().count() as i64,
            ])
            .context("failed to insert chunk record")?;
        }

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
}
