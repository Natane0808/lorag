//! RAG 查询：手写 embed → lancedb vector_search → 拼 context → LLM。
//!
//! **M5 重要决定**：不用 rig-lancedb 的 `LanceDbVectorIndex` + `AgentBuilder::dynamic_context`。
//! 原因：rig 0.40 + rig-lancedb 0.40 + lancedb 0.30 集成内部某步会一次性分配 ~62GB 内存
//! （已实测爆掉用户 64GB 机器）。**绕开** 这层抽象，直接调 lancedb 原生 API + rig
//! `completion_model.completion()`，每步内存都自己控制，5 chunk 也能跑。
//!
//! ## 流程
//!
//! 1. embed question → 384-dim f64 vec（rig Embedding.vec 是 f64）
//! 2. 转 f64 → f32（lancedb vector_search 接受 &[f32]）
//! 3. lancedb native `table.vector_search(&[f32])?.limit(top_k).execute()` → RecordBatch stream
//! 4. 从 RecordBatch 抽 `text` 列（StringArray），收集 top_k chunks
//! 5. 拼 context 字符串，喂给 `llm_model.completion(req)` 拿答案
//!
//! ## Fallback 行为
//!
//! 如果 LanceDB 还没数据（lancedb 目录不存在 / `documents` 表不存在 / 其他 lance 错误），
//! 自动 fallback 到 **裸 LLM**（不检索 context，直接问模型）。这样：
//! - 首次跑通也能对话
//! - REPL / `lorag query` 任何时候都能用
//! - 有数据时享受 RAG，没数据时退化为 LLM（不报错）

use std::path::Path;

use anyhow::{Context, Result};
use arrow_array::{Array, StringArray};
use futures::StreamExt;
use lancedb::query::{ExecutableQuery, QueryBase};
use rig::client::{CompletionClient, EmbeddingsClient};
use rig::completion::message::{Message, Text, UserContent};
use rig::completion::request::CompletionRequest;
use rig::completion::CompletionModel;
use rig::embeddings::EmbeddingModel;
use rig::one_or_many::OneOrMany;

use crate::aha_provider::AhaClient;
use crate::config::AppConfig;

/// RAG 查询主流程。
///
/// 1. embed question
/// 2. lancedb vector_search top_k
/// 3. 拼 context 喂 LLM
/// 4. **如果 LanceDB 还没数据，自动 fallback 到裸 LLM**
pub async fn rag_query(
    client: &AhaClient,
    cfg: &AppConfig,
    question: &str,
    top_k: usize,
) -> Result<String> {
    match try_rag_with_lancedb(client, cfg, question, top_k).await {
        Ok(answer) => Ok(answer),
        Err(e) => {
            let err_str = format!("{e:#}");
            if is_recoverable_error(&err_str) {
                eprintln!("(RAG unavailable: {err_str})");
                eprintln!("(hint: run `lorag ingest <path>` to enable retrieval)");
                eprintln!("(falling back to bare LLM)");
                bare_llm_query(client, cfg, question).await
            } else {
                Err(e)
            }
        }
    }
}

/// 裸 LLM 查询（直发 prompt，不检索 context）。
///
/// `lorag shell --no-rag` 和 fallback 路径都用这个。
pub async fn bare_llm_query(
    client: &AhaClient,
    cfg: &AppConfig,
    question: &str,
) -> Result<String> {
    let llm_model = client.completion_model(&cfg.llm_model_name);
    let req = CompletionRequest {
        preamble: Some("你是一个简洁的助手，用一两句话直接回答问题。".to_string()),
        chat_history: OneOrMany::one(Message::user(question)),
        ..empty_completion_request()
    };
    let resp = llm_model
        .completion(req)
        .await
        .map_err(|e| anyhow::anyhow!("bare LLM completion failed: {e}"))?;
    extract_text_from_response(&resp)
}

/// rig `CompletionRequest` 没派生 Default —— 写个 helper 避免重复 boilerplate
fn empty_completion_request() -> CompletionRequest {
    CompletionRequest {
        model: None,
        preamble: None,
        chat_history: OneOrMany::one(Message::User {
            content: OneOrMany::one(UserContent::Text(Text {
                text: String::new(),
                additional_params: None,
            })),
        }),
        documents: vec![],
        tools: vec![],
        temperature: None,
        max_tokens: None,
        tool_choice: None,
        additional_params: None,
        output_schema: None,
    }
}

/// 从 rig `CompletionResponse` 抽第一个 `AssistantContent::Text`
fn extract_text_from_response(
    resp: &rig::completion::CompletionResponse<aha::params::chat::ChatCompletionResponse>,
) -> Result<String> {
    use rig::completion::AssistantContent;
    let text = resp
        .choice
        .iter()
        .filter_map(|c| match c {
            AssistantContent::Text(t) => Some(t.text.clone()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    if text.is_empty() {
        // 拿不到 text 时拿整个 choice 的 Debug 表达
        Ok(format!("{:?}", resp.choice))
    } else {
        Ok(text)
    }
}

/// 完整 RAG 流程（不带 fallback）。
///
/// **手写** embed → lancedb vector_search → 拼 context → LLM。
/// 不依赖 `LanceDbVectorIndex` 或 `dynamic_context` —— 这两层抽象在当前 rig/lancedb
/// 集成里有隐藏的 ~62GB 内存分配 bug（实测过）。
async fn try_rag_with_lancedb(
    client: &AhaClient,
    cfg: &AppConfig,
    question: &str,
    top_k: usize,
) -> Result<String> {
    // ── 1. embed question（rig Embedding.vec 是 Vec<f64>）──
    let embed_model = client.embedding_model(&cfg.embed_model_name);
    let question_embedding = embed_model
        .embed_text(question)
        .await
        .map_err(|e| anyhow::anyhow!("failed to embed question: {e}"))?;
    let question_f32: Vec<f32> = question_embedding.vec.iter().map(|f| *f as f32).collect();
    if question_f32.len() != cfg.embed_dim {
        anyhow::bail!(
            "question embedding dim {} != configured EMBED_DIM {} (run `lorag models status` to verify)",
            question_f32.len(),
            cfg.embed_dim
        );
    }

    // ── 2. 打开 lancedb ──
    let lancedb_dir = Path::new(&cfg.lancedb_dir);
    if !lancedb_dir.exists() {
        anyhow::bail!(
            "lancedb directory not found at {} (run `lorag ingest <path>` first)",
            lancedb_dir.display()
        );
    }
    let db = lancedb::connect(
        lancedb_dir
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("lancedb path not UTF-8: {}", lancedb_dir.display()))?,
    )
    .execute()
    .await
    .context("failed to connect to lancedb")?;

    let table = db
        .open_table("documents")
        .execute()
        .await
        .context("failed to open `documents` table in lancedb (run `lorag ingest <path>` first)")?;

    // ── 3. vector_search top_k（lancedb 原生 API）──
    let mut stream = table
        .vector_search(&question_f32[..])?
        .limit(top_k)
        .execute()
        .await
        .context("lancedb vector_search failed")?;

    // ── 4. 收集 top_k chunks 的 text ──
    let mut chunks: Vec<String> = Vec::with_capacity(top_k);
    while let Some(rb) = stream.next().await
        .transpose()
        .context("failed to read lancedb search result")?
    {
        let text_col = rb
            .column_by_name("text")
            .ok_or_else(|| {
                anyhow::anyhow!("lancedb schema missing `text` column (current schema: {:?})", rb.schema())
            })?
            .as_any()
            .downcast_ref::<StringArray>()
            .ok_or_else(|| anyhow::anyhow!("`text` column is not StringArray"))?;
        for i in 0..rb.num_rows() {
            let text = text_col.value(i);
            if !text.is_empty() {
                chunks.push(text.to_string());
            }
        }
    }
    if chunks.is_empty() {
        anyhow::bail!(
            "lancedb returned no chunks for the query (ingest more documents or rephrase)"
        );
    }

    // ── 5. 拼 context，喂 LLM ──
    let context = chunks
        .iter()
        .enumerate()
        .map(|(i, t)| format!("[{}] {}", i + 1, t))
        .collect::<Vec<_>>()
        .join("\n\n");

    let llm_model = client.completion_model(&cfg.llm_model_name);
    let preamble = format!(
        "你是一个本地 RAG 助手，仅根据下面的【上下文】回答问题。\n\
         如果上下文无法覆盖问题，请直接说\"未在文档中找到相关信息\"，不要编造。\n\n\
         【上下文】\n{context}"
    );
    let req = CompletionRequest {
        preamble: Some(preamble),
        chat_history: OneOrMany::one(Message::user(question)),
        temperature: Some(0.1),
        ..empty_completion_request()
    };
    let resp = llm_model
        .completion(req)
        .await
        .map_err(|e| anyhow::anyhow!("LLM completion failed: {e}"))?;
    extract_text_from_response(&resp)
}

/// 判断 error 是不是"可恢复的"（lancedb 没数据 / 内存不够 / lance 出错）——
/// 这种情况下 fallback 跑裸 LLM，不让用户卡住
fn is_recoverable_error(err: &str) -> bool {
    err.contains("lancedb")
        || err.contains("documents table")
        || err.contains("run `lorag ingest`")
        || err.contains("LanceDB")
        || err.contains("Lance")
        || err.contains("memory allocation")
        || err.contains("allocation of")
        || err.contains("No such file")
}
