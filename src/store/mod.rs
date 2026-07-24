//! 存储层：SQLite（元数据）+ LanceDB（向量）。
//!
//! - `sqlite_store`：来源追踪、摄入幂等、`sources list`
//! - `lancedb_store`：文档 chunk 向量存储

pub mod lancedb_store;
pub mod sqlite_store;
