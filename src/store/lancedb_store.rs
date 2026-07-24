//! LanceDB 向量存储。
//!
//! 职责：
//! - 创建/打开 `documents` 表
//! - 把嵌入后的 chunk 写入表
//! - 维护 `embedding` 列的 IVF-HNSW 索引（≥256 行时建）

use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use arrow_array::{FixedSizeListArray, Int64Array, RecordBatch, StringArray, types::Float64Type};
use arrow_schema::{DataType, Field, Schema};
use lancedb::connect;

use crate::models::Chunk;

/// 打开（或创建）LanceDB 的 `documents` 表。
///
/// schema: id (Utf8), source_path (Utf8), chunk_ordinal (Int64), text (Utf8),
///          embedding (FixedSizeList<Float64, EMBED_DIM>)
pub async fn ensure_table(lancedb_dir: &Path, embed_dim: usize) -> Result<lancedb::Table> {
    let db = connect(lancedb_dir.to_str().ok_or_else(|| {
        anyhow::anyhow!("lancedb path is not valid UTF-8: {}", lancedb_dir.display())
    })?)
    .execute()
    .await
    .context("failed to connect to lancedb")?;

    let names = db
        .table_names()
        .execute()
        .await
        .context("failed to list lancedb tables")?;
    if names.iter().any(|n| n == "documents") {
        db.open_table("documents")
            .execute()
            .await
            .context("failed to open existing documents table")
    } else {
        let schema = Arc::new(Schema::new(vec![
            Field::new("id", DataType::Utf8, false),
            Field::new("source_path", DataType::Utf8, false),
            Field::new("chunk_ordinal", DataType::Int64, false),
            Field::new("text", DataType::Utf8, false),
            // FixedSizeList 需要 embed_dim
            Field::new(
                "embedding",
                DataType::FixedSizeList(
                    Arc::new(Field::new("item", DataType::Float64, true)),
                    embed_dim as i32,
                ),
                false,
            ),
        ]));
        let empty = RecordBatch::new_empty(schema);
        db.create_table("documents", empty)
            .execute()
            .await
            .context("failed to create documents table")
    }
}

/// 把 `(chunks, embeddings)` 写入 lancedb。
///
/// `embeddings` 是 `f64` 向量列表，每个向量长度为 `embed_dim`。
/// 每个向量对应一个 chunk，chunk 的 `ordinal` 用于拼接 id。
pub async fn insert_batch(
    table: &lancedb::Table,
    chunks: &[Chunk],
    source_hash: &str,
    embeddings: Vec<Vec<f64>>,
    embed_dim: usize,
) -> Result<()> {
    let n = chunks.len();
    assert_eq!(
        n,
        embeddings.len(),
        "chunk count ({n}) != embedding count ({})",
        embeddings.len()
    );

    let ordinals: Vec<i64> = chunks.iter().map(|c| c.ordinal as i64).collect();

    let id_col = StringArray::from_iter_values(
        chunks
            .iter()
            .map(|c| format!("{}:{}", source_hash, c.ordinal)),
    );
    let source_path_col = StringArray::from_iter_values(chunks.iter().map(|c| &c.source_path));
    let text_col = StringArray::from_iter_values(chunks.iter().map(|c| &c.text));
    let ordinal_col = Int64Array::from_iter_values(ordinals);

    // 构建 FixedSizeListArray<Float64>
    let embed_col = FixedSizeListArray::from_iter_primitive::<Float64Type, _, _>(
        embeddings.iter().map(|v| Some(v.iter().map(|&f| Some(f)))),
        embed_dim as i32,
    );

    let schema = Arc::new(Schema::new(vec![
        Field::new("id", DataType::Utf8, false),
        Field::new("source_path", DataType::Utf8, false),
        Field::new("chunk_ordinal", DataType::Int64, false),
        Field::new("text", DataType::Utf8, false),
        Field::new(
            "embedding",
            DataType::FixedSizeList(
                Arc::new(Field::new("item", DataType::Float64, true)),
                embed_dim as i32,
            ),
            false,
        ),
    ]));

    let batch = RecordBatch::try_new(
        schema,
        vec![
            Arc::new(id_col),
            Arc::new(source_path_col),
            Arc::new(ordinal_col),
            Arc::new(text_col),
            Arc::new(embed_col),
        ],
    )
    .context("failed to build RecordBatch for lancedb insert")?;

    table
        .add(batch)
        .execute()
        .await
        .context("failed to insert batch into lancedb")?;
    Ok(())
}

/// IVF-HNSW 索引建索引的最小行数。
///
/// lance 0.30 的 IVF 训练（kmeans）要求至少 256 行才能跑，低于这个数会报
/// `Not enough data to train index` 之类的错。lorag 走"数据够了再建"策略：
/// < 256 行 silently 跳过（继续用 ENN 全表扫），≥ 256 行则建 IVF-HNSW-FLAT。
pub const HNSW_MIN_ROWS: usize = 256;

/// 为 `embedding` 列建 IVF-HNSW 索引。
///
/// - 行数 < 256 → 静默跳过（不报错，INFO 日志说明原因）
/// - 已经建过 → 跳过（幂等）
/// - 行数 ≥ 256 且没建过 → 建 IVF-HNSW-FLAT 索引；打印进度行
///
/// 出错 → 返回 `Err`，让上层（ingest pipeline）决定要不要硬失败
pub async fn ensure_hnsw_index(table: &lancedb::Table) -> Result<()> {
    let row_count = table
        .count_rows(None)
        .await
        .context("failed to count lancedb rows for HNSW check")?;

    if row_count < HNSW_MIN_ROWS {
        tracing::debug!(
            "HNSW index requires >= {} rows, have {}; skipping (will use ENN)",
            HNSW_MIN_ROWS,
            row_count
        );
        return Ok(());
    }

    // 已经建过？`index_stats` 接受 column 名或 index name
    if table.index_stats("embedding").await?.is_some() {
        tracing::debug!("HNSW index on `embedding` already exists; skipping");
        return Ok(());
    }

    println!(
        "  building HNSW index on `embedding` (rows={}, dim from schema)...",
        row_count
    );
    table
        .create_index(
            &["embedding"],
            lancedb::index::Index::IvfHnswFlat(
                lancedb::index::vector::IvfHnswFlatIndexBuilder::default(),
            ),
        )
        .execute()
        .await
        .context("failed to create IVF-HNSW-FLAT index on `embedding`")?;
    println!("  HNSW index built");

    Ok(())
}
