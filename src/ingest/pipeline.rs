//! 摄入 pipeline：文件 → 文本 → chunks → embeddings → lancedb + sqlite。
//!
//! 这是 `lorag ingest` 的核心编排逻辑。

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use rig::client::EmbeddingsClient;
use rig::embeddings::EmbeddingModel;

use crate::aha_provider::AhaClient;
use crate::config::AppConfig;
use crate::ingest::loader;
use crate::models::SourceRecord;
use crate::store::lancedb_store;
use crate::store::sqlite_store::SqliteStore;

/// 摄入进度计数器。
#[derive(Default)]
pub struct IngestCounts {
    pub ok: usize,
    pub skipped: usize,
    pub failed: usize,
}

/// 主摄入流程。
pub async fn run_ingest(
    client: &AhaClient,
    cfg: &AppConfig,
    paths: &[PathBuf],
    allowed_exts: &[String],
    force: bool,
    recursive: bool,
) -> Result<IngestCounts> {
    let sqlite = SqliteStore::open(&cfg.sqlite_path)
        .with_context(|| format!("failed to open sqlite at {}", cfg.sqlite_path.display()))?;

    // lancedb 表维度跟 embedding 模型走（不再从 .env 读 EMBED_DIM）
    let embed_dim = client.embed_dim().context(
        "AhaClient has no embed_dim (model failed to expose dim; run `lorag doctor` to debug)",
    )?;
    let table = lancedb_store::ensure_table(&cfg.lancedb_dir, embed_dim)
        .await
        .context("failed to ensure lancedb documents table")?;

    // 收集所有文件
    let files = collect_files(paths, allowed_exts, recursive);
    let mut counts = IngestCounts::default();

    for file_path in &files {
        match ingest_one(client, cfg, &sqlite, &table, file_path, force).await {
            Ok(status) => match status {
                IngestStatus::Ok => {
                    println!("  ok: {}", file_path.display());
                    counts.ok += 1;
                }
                IngestStatus::Skipped(reason) => {
                    println!("  skipped: {} ({})", file_path.display(), reason);
                    counts.skipped += 1;
                }
            },
            Err(e) => {
                // 单个文件失败不中断
                tracing::warn!("failed to ingest {}: {:#}", file_path.display(), e);
                println!("  failed: {} — {}", file_path.display(), e);
                counts.failed += 1;
            }
        }
    }

    Ok(counts)
}

enum IngestStatus {
    Ok,
    Skipped(String),
}

/// 摄入单个文件。
async fn ingest_one(
    client: &AhaClient,
    cfg: &AppConfig,
    sqlite: &SqliteStore,
    table: &lancedb::Table,
    path: &Path,
    force: bool,
) -> Result<IngestStatus> {
    // 1. 读取文件内容 + 算 hash
    let bytes =
        std::fs::read(path).with_context(|| format!("failed to read file: {}", path.display()))?;
    let hash = compute_sha256(&bytes);
    let source_path = path.to_string_lossy().to_string();
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();

    // 2. 幂等检查
    if !force
        && let Some(prev) = sqlite.find_source(&source_path)?
        && prev.source_hash == hash
    {
        return Ok(IngestStatus::Skipped("unchanged".into()));
    }

    // 3. 提取文本
    let text = loader::extract(path)
        .with_context(|| format!("failed to extract text from: {}", path.display()))?;
    if text.trim().is_empty() {
        return Ok(IngestStatus::Skipped("empty content".into()));
    }

    // 4. 分块
    let chunks = crate::chunker::split(&text, path, cfg.chunk_size, cfg.chunk_overlap);
    if chunks.is_empty() {
        return Ok(IngestStatus::Skipped("no chunks produced".into()));
    }

    // 5. 批量 embed —— 直接用 embedding_model 的 batch embed
    let embed_model = client.embedding_model(&cfg.embed_model);
    let texts: Vec<String> = chunks.iter().map(|c| c.text.clone()).collect();
    let embeddings = embed_model
        .embed_texts(texts)
        .await
        .map_err(|e| anyhow::anyhow!("failed to embed {} chunks: {}", chunks.len(), e))?;

    // 直接取 Embedding.vec（已是 Vec<f64>，跟 lancedb Float64 schema 一致）
    let vecs: Vec<Vec<f64>> = embeddings.iter().map(|emb| emb.vec.clone()).collect();

    // 6. 写 lancedb（dim 来自 AhaClient.loaded 模型，不再从 cfg 读）
    let embed_dim = client
        .embed_dim()
        .context("AhaClient has no embed_dim (model failed to expose dim); this is a bug")?;
    lancedb_store::insert_batch(table, &chunks, &hash, vecs, embed_dim).await?;

    // 6.5 数据量够了就建 HNSW 索引；不够 silently 跳过（< 256 行）
    //    失败不阻塞 ingest —— index 可以后续手动重试
    if let Err(e) = lancedb_store::ensure_hnsw_index(table).await {
        tracing::warn!("HNSW index build failed (ingest still ok): {:#}", e);
        println!("  warning: HNSW index build failed: {e:#}");
    }

    // 7. 写 sqlite
    let record = SourceRecord {
        source_path: source_path.clone(),
        source_hash: hash,
        file_type: ext,
        chunk_count: chunks.len(),
        byte_size: bytes.len() as u64,
    };
    let source_id = sqlite.upsert_source(&record, force)?;
    sqlite.insert_chunks(source_id, &chunks)?;

    Ok(IngestStatus::Ok)
}

/// sha256 摘要，hex 编码。
fn compute_sha256(data: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(data))
}

/// 收集所有待摄入文件。
fn collect_files(paths: &[PathBuf], allowed_exts: &[String], recursive: bool) -> Vec<PathBuf> {
    let exts: Vec<String> = allowed_exts.iter().map(|e| e.to_lowercase()).collect();
    let mut out = Vec::new();

    for p in paths {
        if p.is_dir() {
            collect_from_dir(p, &exts, recursive, &mut out);
        } else if ext_matches(p, &exts) {
            out.push(p.clone());
        }
    }

    out.sort();
    out
}

fn collect_from_dir(dir: &Path, exts: &[String], recursive: bool, out: &mut Vec<PathBuf>) {
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if recursive {
                    collect_from_dir(&path, exts, recursive, out);
                }
            } else if ext_matches(&path, exts) {
                out.push(path);
            }
        }
    }
}

fn ext_matches(path: &Path, exts: &[String]) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| exts.iter().any(|x| x == e || x == "*"))
        .unwrap_or(false)
}
