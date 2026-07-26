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
use rig::completion::CompletionModel;
use rig::completion::message::{Message, Text, UserContent};
use rig::completion::request::CompletionRequest;
use rig::embeddings::EmbeddingModel;
use rig::one_or_many::OneOrMany;

use crate::aha_provider::AhaClient;
use crate::config::AppConfig;
use crate::models::MessageRecord;

/// **M7 chat**：多轮对话时把【历史对话】 + 【文档上下文】拼成 LLM preamble。
///
/// - `history` 按 ordinal 升序（早→晚），assistant 视角读起来是"先发生的在前面"
/// - `chunks` 来自 `retrieve_chunks`；为空时说明走 `--no-rag` 或 RAG 失败
///
/// 拼装规则：
/// 1. 系统级 preamble 固定："你是简洁的本地 RAG 助手"
/// 2. 有 history → 加【历史对话】段
/// 3. 有 chunks → 加【文档上下文】段 + "上下文无法覆盖请直说" 提示
pub fn build_chat_preamble(history: &[MessageRecord], chunks: &[String]) -> String {
    let mut s = String::new();
    s.push_str("你是一个简洁的本地 RAG 助手。\n");
    s.push_str("回答要短（一两句话优先），不要重复问题，不要编造。\n");

    if !history.is_empty() {
        s.push_str("\n【历史对话】\n");
        for msg in history {
            let role_zh = match msg.role.as_str() {
                "user" => "用户",
                "assistant" => "助手",
                "system" => "系统",
                other => other,
            };
            s.push_str(&format!("{role_zh}：{}\n", msg.content));
        }
    }

    if !chunks.is_empty() {
        s.push_str("\n【文档上下文】\n");
        for (i, c) in chunks.iter().enumerate() {
            s.push_str(&format!("[{}] {}\n\n", i + 1, c));
        }
        s.push_str("仅根据上面的【文档上下文】回答【当前问题】；\n");
        s.push_str("如果上下文无法覆盖，请直接说\"未在文档中找到相关信息\"，不要编造。\n");
    }

    s
}

/// Rerank 粗筛条数由 `AppConfig::rerank_top_n` 控制（环境变量 `RERANK_TOP_N` / CLI `--rerank-top-n`）。
///
/// 默认 50（见 [`crate::config::AppConfig::rerank_top_n`]）。必须 > `top_k`
/// （否则 rerank 排序没空间）。
///
/// **低层**：把 `preamble` + `question` 喂给 LLM，抽第一个 text 段返回。
///
/// `rag_query` / `bare_llm_query` / `cmd_chat` 都走这个统一入口。
pub async fn llm_complete(
    client: &AhaClient,
    cfg: &AppConfig,
    preamble: String,
    question: &str,
) -> Result<String> {
    let llm_model = client.completion_model(&cfg.llm_model);
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

/// **低层**：embed question + lancedb vector_search top_k + 收集 chunks 文本。
///
/// 不调 LLM；不拼 context。RAG 失败时返回 `Err`，调用方决定要不要 fallback 到裸 LLM。
///
/// **rerank 路径**：当 `enable_rerank=true` 且 `client.has_rerank()` 为 true：
/// 1. vector_search 取 `rerank_top_n` 条候选（**比 `top_k` 大**才有排序空间；调用方保证 `rerank_top_n > top_k`）
/// 2. 调 `client.rerank_score(question, chunks)` 打分
/// 3. 按分数降序排，取前 `top_k` 条
///
/// `enable_rerank=false` 或 `!client.has_rerank()`：直接 vector_search `top_k` 条，零开销。
pub async fn retrieve_chunks(
    client: &AhaClient,
    cfg: &AppConfig,
    question: &str,
    top_k: usize,
    enable_rerank: bool,
    rerank_top_n: usize,
) -> Result<Vec<String>> {
    // ── 1. embed question ──
    let embed_model = client.embedding_model(&cfg.embed_model);
    let question_embedding = embed_model
        .embed_text(question)
        .await
        .map_err(|e| anyhow::anyhow!("failed to embed question: {e}"))?;
    let question_f32: Vec<f32> = question_embedding.vec.iter().map(|f| *f as f32).collect();
    if let Some(expected) = client.embed_dim()
        && question_f32.len() != expected
    {
        anyhow::bail!(
            "question embedding dim {} != model dim {} (aha / candle 异常，模型可能加载坏了)",
            question_f32.len(),
            expected
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

    let table =
        db.open_table("documents").execute().await.context(
            "failed to open `documents` table in lancedb (run `lorag ingest <path>` first)",
        )?;

    // ── 3. vector_search limit = max(top_k, rerank_top_n) ──
    //   rerank 时拿更多候选（top_N），让 rerank 有排序空间；不 rerank 时只取 top_k，省 IO
    let should_rerank = enable_rerank && client.has_rerank();
    let fetch_limit = if should_rerank {
        top_k.max(rerank_top_n)
    } else {
        top_k
    };
    let mut stream = table
        .vector_search(&question_f32[..])?
        .limit(fetch_limit)
        .execute()
        .await
        .context("lancedb vector_search failed")?;

    // ── 4. 收集 chunks ──
    let mut chunks: Vec<String> = Vec::with_capacity(fetch_limit);
    while let Some(rb) = stream
        .next()
        .await
        .transpose()
        .context("failed to read lancedb search result")?
    {
        let text_col = rb
            .column_by_name("text")
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "lancedb schema missing `text` column (current schema: {:?})",
                    rb.schema()
                )
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

    // ── 5. rerank 路径（如果启用）──
    if should_rerank {
        // 已经 ensure 过：has_rerank() 返回 true 说明 slot 填了；不会二次 ensure
        // 防御性：万一 slot 仍空，ensure 一次
        if !client.has_rerank() {
            client
                .ensure_rerank()
                .await
                .context("failed to load rerank model")?;
        }
        let scores = client
            .rerank_score(question, &chunks)
            .await
            .context("rerank scoring failed")?;
        if scores.len() != chunks.len() {
            anyhow::bail!(
                "rerank returned {} scores for {} chunks — should be 1:1",
                scores.len(),
                chunks.len()
            );
        }
        // 按分数降序排（同分时保持原顺序稳定）
        let mut indexed: Vec<(usize, f32)> = scores.iter().copied().enumerate().collect();
        indexed.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        let top: Vec<String> = indexed
            .into_iter()
            .take(top_k)
            .map(|(i, _)| chunks[i].clone())
            .collect();
        return Ok(top);
    }

    Ok(chunks)
}

/// RAG 查询主流程。
///
/// 1. embed question
/// 2. lancedb vector_search top_k（如果 enable_rerank + 有 rerank 模型 → 粗筛 `rerank_top_n` + rerank 排序取 top_k）
/// 3. 拼 context 喂 LLM
/// 4. **如果 LanceDB 还没数据，自动 fallback 到裸 LLM**
///
/// `rerank_top_n`：rerank 启用时用作粗筛上限（必须 > `top_k`）。调用方传
/// `cfg.rerank_top_n`（用户可通过 `RERANK_TOP_N` 环境变量 / `--rerank-top-n` CLI flag 覆盖）。
pub async fn rag_query(
    client: &AhaClient,
    cfg: &AppConfig,
    question: &str,
    top_k: usize,
    enable_rerank: bool,
    rerank_top_n: usize,
) -> Result<String> {
    match try_rag_with_lancedb(client, cfg, question, top_k, enable_rerank, rerank_top_n).await {
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
pub async fn bare_llm_query(client: &AhaClient, cfg: &AppConfig, question: &str) -> Result<String> {
    let preamble = "你是一个简洁的助手，用一两句话直接回答问题。".to_string();
    llm_complete(client, cfg, preamble, question)
        .await
        .map_err(|e| anyhow::anyhow!("bare LLM completion failed: {e}"))
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
///
/// 内部走 `retrieve_chunks` + `llm_complete` 两个低层。
async fn try_rag_with_lancedb(
    client: &AhaClient,
    cfg: &AppConfig,
    question: &str,
    top_k: usize,
    enable_rerank: bool,
    rerank_top_n: usize,
) -> Result<String> {
    let chunks = retrieve_chunks(client, cfg, question, top_k, enable_rerank, rerank_top_n).await?;
    let context = chunks
        .iter()
        .enumerate()
        .map(|(i, t)| format!("[{}] {}", i + 1, t))
        .collect::<Vec<_>>()
        .join("\n\n");
    let preamble = format!(
        "你是一个本地 RAG 助手，仅根据下面的【上下文】回答问题。\n\
         如果上下文无法覆盖问题，请直接说\"未在文档中找到相关信息\"，不要编造。\n\n\
         【上下文】\n{context}"
    );
    llm_complete(client, cfg, preamble, question).await
}

/// 判断 error 是不是"可恢复的"（lancedb 没数据 / 内存不够 / lance 出错）——
/// 这种情况下 fallback 跑裸 LLM，不让用户卡住。
///
/// `pub` 是因为 `cmd_chat` 也要用（M7 多轮走 `retrieve_chunks` 低层，失败时
/// 不能 fallback 到 `rag_query` 因为那不带 history，要自己处理）。
pub fn is_recoverable_error(err: &str) -> bool {
    err.contains("lancedb")
        || err.contains("documents table")
        || err.contains("run `lorag ingest`")
        || err.contains("LanceDB")
        || err.contains("Lance")
        || err.contains("memory allocation")
        || err.contains("allocation of")
        || err.contains("No such file")
}
