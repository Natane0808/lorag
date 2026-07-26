//! rig 0.40 provider 适配。
//!
//! 把 [`crate::aha_provider::AhaClient`] 装成 rig 能用的 client：
//! - [`CompletionClient`] → [`AhaCompletionModel`]
//! - [`EmbeddingsClient`] → [`AhaEmbeddingModel`]
//!
//! 不实现：Provider / ProviderClient（0.40 是给 HTTP client 用的）、流式输出、tool calls。
//! 消息转换：rig `Message` → aha `ChatMessage`（preamble / documents 拼成 system + user）。

use rig::client::{CompletionClient, EmbeddingsClient};
use rig::completion::message::{AssistantContent, Message, Text, UserContent};
use rig::completion::request::CompletionRequest;
use rig::completion::{CompletionError, CompletionModel, CompletionResponse, Usage};
use rig::embeddings::{Embedding, EmbeddingError, EmbeddingModel};
use rig::one_or_many::OneOrMany;
use rig::streaming::StreamingCompletionResponse;

#[cfg(test)]
use rig::completion::request::Document;

use aha::models::ModelInstance;
use aha::params::chat::{ChatCompletionParameters, ChatMessage, ChatMessageContent};
use tokio::sync::Mutex;

use crate::aha_provider::AhaClient;

// =============================================================================
// 编译期断言（Send + Sync）
// =============================================================================

// rig 0.40 用 `WasmCompatSend: Send`（native build）当 trait bound，
// 所有跨 await 持有的类型都得 Send + Sync
#[allow(dead_code)]
// M5+ 按需放开
fn _assert_send_sync() {
    fn assert<T: Send + Sync>() {}
    assert::<AhaClient>();
    assert::<AhaCompletionModel>();
    assert::<AhaEmbeddingModel>();
    assert::<Mutex<ModelInstance<'static>>>();
}

// =============================================================================
// AhaCompletionModel
// =============================================================================

/// rig `CompletionModel` 包装，背后是 [`AhaClient`] 持有的 LLM。
#[derive(Clone)]
pub struct AhaCompletionModel {
    client: AhaClient,
    model: String,
}

// 我们用 `()` 当 streaming response（rig 已为 `()` 实现 [`GetTokenUsage`]，返回 `Usage::new()`）
// 这让我们不用造一个空 streaming struct。
// 但 trait 里要求 `Clone + Unpin + Serialize + DeserializeOwned`，`()` 都满足。

impl CompletionModel for AhaCompletionModel {
    type Response = aha::params::chat::ChatCompletionResponse;
    type StreamingResponse = ();
    type Client = AhaClient;

    fn make(client: &Self::Client, model: impl Into<String>) -> Self {
        Self {
            client: client.clone(),
            model: model.into(),
        }
    }

    async fn completion(
        &self,
        request: CompletionRequest,
    ) -> Result<CompletionResponse<Self::Response>, CompletionError> {
        // 1. rig → aha 消息转换
        let aha_messages = convert_messages(&request);

        // 2. 组装 ChatCompletionParameters
        let params = ChatCompletionParameters {
            messages: aha_messages,
            model: self.model.clone(),
            temperature: request.temperature.map(|t| t as f32),
            max_tokens: request.max_tokens.map(|n| n as u32),
            stream: Some(false),
            // 关 Qwen3 thinking mode：让模型直接答，不先"自言自语"输出几百个思考 token
            // （不传 = 用 aha 默认值，Qwen3 默认 thinking = 慢 2-3x；显式 false = 直接答）
            enable_thinking: Some(false),
            // 兜底 max_completion_tokens：即使上游没传，也限制最大输出 token 数，
            // 避免 0.6B 模型偶尔生成几千 token 卡住推理
            max_completion_tokens: Some(1024),
            ..Default::default()
        };

        // 3. 调 aha
        let resp = self
            .client
            .llm_generate(params)
            .await
            .map_err(|e| CompletionError::ProviderError(e.to_string()))?;

        // 4. 抽第一条 choice 的文本，包成 CompletionResponse
        // aha 的 ChatCompletionChoice.message 已经是 ChatMessage enum（不是 Assistant { content }）
        let choice_text = resp
            .choices
            .first()
            .and_then(|c| extract_assistant_text(&c.message))
            .unwrap_or_default();

        Ok(CompletionResponse {
            choice: OneOrMany::one(AssistantContent::Text(Text {
                text: choice_text,
                additional_params: None,
            })),
            usage: Usage::new(),
            raw_response: resp,
            message_id: None, // aha 不在 choice 层暴露 message id；将来 raw_response.id 也能用
        })
    }

    async fn stream(
        &self,
        _request: CompletionRequest,
    ) -> Result<StreamingCompletionResponse<Self::StreamingResponse>, CompletionError> {
        Err(CompletionError::ProviderError(
            "streaming is not supported in MVP (M5+ will add)".to_string(),
        ))
    }
}

impl CompletionClient for AhaClient {
    type CompletionModel = AhaCompletionModel;

    fn completion_model(&self, model: impl Into<String>) -> Self::CompletionModel {
        AhaCompletionModel {
            client: self.clone(),
            model: model.into(),
        }
    }
}

// =============================================================================
// AhaEmbeddingModel
// =============================================================================

/// rig `EmbeddingModel` 包装，背后是 [`AhaClient`] 持有的 embedding 模型。
#[derive(Clone)]
pub struct AhaEmbeddingModel {
    client: AhaClient,
    /// model name（记录用；M3+ 走 lancedb 时可能用作 metadata 过滤）
    #[allow(dead_code)]
    model: String,
    ndims: usize,
}

impl EmbeddingModel for AhaEmbeddingModel {
    /// aha 实际单批上限：~1024。超了上层（rig `EmbeddingsBuilder`）会分批。
    const MAX_DOCUMENTS: usize = 1024;

    type Client = AhaClient;

    fn make(client: &Self::Client, model: impl Into<String>, dims: Option<usize>) -> Self {
        // 优先用调用方显式传的 dims（rig 0.40 支持），否则从 AhaClient 拿模型实际 dim
        // （load 时从 config.json 读的，不再从 .env 读）
        let ndims = dims.unwrap_or_else(|| client.embed_dim().unwrap_or(0));
        Self {
            client: client.clone(),
            model: model.into(),
            ndims,
        }
    }

    fn ndims(&self) -> usize {
        self.ndims
    }

    async fn embed_texts(
        &self,
        texts: impl IntoIterator<Item = String> + Send,
    ) -> Result<Vec<Embedding>, EmbeddingError> {
        let texts: Vec<String> = texts.into_iter().collect();
        // 调 aha
        let vecs = self
            .client
            .embed_texts(&texts)
            .await
            .map_err(|e| EmbeddingError::ProviderError(e.to_string()))?;
        // 转 f32 → f64（rig `Embedding.vec` 是 `Vec<f64>`），并把每个 vec 配上原文
        Ok(texts
            .into_iter()
            .zip(vecs)
            .map(|(document, vec)| Embedding {
                document,
                vec: vec.into_iter().map(|f| f as f64).collect(),
            })
            .collect())
    }
}

impl EmbeddingsClient for AhaClient {
    type EmbeddingModel = AhaEmbeddingModel;

    fn embedding_model(&self, model: impl Into<String>) -> Self::EmbeddingModel {
        AhaEmbeddingModel {
            client: self.clone(),
            model: model.into(),
            ndims: self.embed_dim().unwrap_or(0),
        }
    }

    fn embedding_model_with_ndims(
        &self,
        model: impl Into<String>,
        ndims: usize,
    ) -> Self::EmbeddingModel {
        AhaEmbeddingModel {
            client: self.clone(),
            model: model.into(),
            ndims,
        }
    }
}

// =============================================================================
// 消息转换：rig → aha
// =============================================================================

/// 从 rig `ChatMessageContent`（实际上 aha 自己的）抽纯文本。
fn extract_text(content: &ChatMessageContent) -> Option<String> {
    match content {
        ChatMessageContent::Text(s) => Some(s.clone()),
        ChatMessageContent::None => None,
        // 多模态 content part：MVP 简单拼 text part
        ChatMessageContent::ContentPart(parts) => {
            let s = parts
                .iter()
                .filter_map(|p| match p {
                    aha::params::chat::ChatMessageContentPart::Text(t) => Some(t.text.clone()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("");
            if s.is_empty() { None } else { Some(s) }
        }
    }
}

/// 从 aha 的 [`ChatMessage`] 任何变体里抽纯文本（用于把 aha response 转 rig AssistantContent::Text）。
fn extract_assistant_text(m: &ChatMessage) -> Option<String> {
    match m {
        ChatMessage::Assistant {
            content: Some(c), ..
        } => extract_text(c),
        ChatMessage::User { content, .. } => extract_text(content),
        ChatMessage::System { content, .. } => extract_text(content),
        ChatMessage::Developer { content, .. } => extract_text(content),
        ChatMessage::Tool { content, .. } => extract_text(content),
        ChatMessage::Assistant { content: None, .. } => None,
    }
}

/// rig `UserContent` → 纯文本。
fn user_content_to_text(c: &UserContent) -> Option<String> {
    match c {
        UserContent::Text(t) => Some(t.text.clone()),
        UserContent::ToolResult(r) => {
            // ToolResultContent::Text(Text) / ::Image(Image)，MVP 拼 text 部分
            let s: String = r
                .content
                .iter()
                .filter_map(|item| match item {
                    rig::completion::message::ToolResultContent::Text(t) => Some(t.text.clone()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n");
            if s.is_empty() { None } else { Some(s) }
        }
        // 其它（Image/Audio/Video/Document）MVP 跳过
        _ => None,
    }
}

/// rig `AssistantContent` → 纯文本。
fn assistant_content_to_text(c: &AssistantContent) -> Option<String> {
    match c {
        AssistantContent::Text(t) => Some(t.text.clone()),
        // MVP 跳过 ToolCall / Reasoning / Image
        _ => None,
    }
}

/// 把 rig 的 `CompletionRequest` 翻译成 aha 的 `Vec<ChatMessage>`。
///
/// 转换规则：
/// 1. `preamble` → 第一条 aha `System` 消息
/// 2. `documents` → aha `System` 消息（"参考：..."），插在 user 消息前
/// 3. `chat_history` → 逐条翻译（User / Assistant / System）
fn convert_messages(req: &CompletionRequest) -> Vec<ChatMessage> {
    let mut out: Vec<ChatMessage> = Vec::new();

    // 1. preamble → System
    if let Some(p) = &req.preamble
        && !p.is_empty()
    {
        out.push(ChatMessage::System {
            content: ChatMessageContent::Text(p.clone()),
            name: None,
        });
    }

    // 2. documents → System 块
    if !req.documents.is_empty() {
        let block = req
            .documents
            .iter()
            .map(|d| format!("[{}] {}", d.id, d.text))
            .collect::<Vec<_>>()
            .join("\n\n");
        let sys = format!("以下是检索到的参考文档：\n\n{block}");
        out.push(ChatMessage::System {
            content: ChatMessageContent::Text(sys),
            name: None,
        });
    }

    // 3. chat_history 逐条翻译
    for m in req.chat_history.iter() {
        match m {
            Message::System { content } => {
                out.push(ChatMessage::System {
                    content: ChatMessageContent::Text(content.clone()),
                    name: None,
                });
            }
            Message::User { content } => {
                let joined: String = content
                    .iter()
                    .filter_map(user_content_to_text)
                    .collect::<Vec<_>>()
                    .join("\n");
                if !joined.is_empty() {
                    out.push(ChatMessage::User {
                        content: ChatMessageContent::Text(joined),
                        name: None,
                    });
                }
            }
            Message::Assistant { content, .. } => {
                let joined: String = content
                    .iter()
                    .filter_map(assistant_content_to_text)
                    .collect::<Vec<_>>()
                    .join("\n");
                // Assistant 消息 content 允许 None（tool_calls-only），但我们只翻译 text
                out.push(ChatMessage::Assistant {
                    content: if joined.is_empty() {
                        None
                    } else {
                        Some(ChatMessageContent::Text(joined))
                    },
                    reasoning_content: None,
                    refusal: None,
                    name: None,
                    audio: None,
                    tool_calls: None,
                });
            }
        }
    }

    out
}

// =============================================================================
// 单元测试
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn convert_preamble_into_system() {
        let req = CompletionRequest {
            preamble: Some("be helpful".to_string()),
            chat_history: OneOrMany::one(Message::user("hi")),
            ..CompletionRequest::default_for_tests()
        };
        let msgs = convert_messages(&req);
        assert_eq!(msgs.len(), 2);
        match &msgs[0] {
            ChatMessage::System { content, .. } => {
                assert_eq!(extract_text(content).unwrap(), "be helpful")
            }
            _ => panic!("expected System"),
        }
        match &msgs[1] {
            ChatMessage::User { content, .. } => assert_eq!(extract_text(content).unwrap(), "hi"),
            _ => panic!("expected User"),
        }
    }

    #[test]
    fn convert_documents_into_system_block() {
        let req = CompletionRequest {
            preamble: None,
            chat_history: OneOrMany::one(Message::user("question")),
            documents: vec![
                Document {
                    id: "doc1".into(),
                    text: "first content".into(),
                    additional_props: Default::default(),
                },
                Document {
                    id: "doc2".into(),
                    text: "second content".into(),
                    additional_props: Default::default(),
                },
            ],
            ..CompletionRequest::default_for_tests()
        };
        let msgs = convert_messages(&req);
        assert_eq!(msgs.len(), 2);
        match &msgs[0] {
            ChatMessage::System { content, .. } => {
                let s = extract_text(content).unwrap();
                assert!(s.contains("[doc1] first content"));
                assert!(s.contains("[doc2] second content"));
            }
            _ => panic!("expected System with documents"),
        }
    }

    #[test]
    fn empty_preamble_skipped() {
        let req = CompletionRequest {
            preamble: Some("".to_string()),
            chat_history: OneOrMany::one(Message::user("q")),
            ..CompletionRequest::default_for_tests()
        };
        let msgs = convert_messages(&req);
        assert_eq!(msgs.len(), 1);
        assert!(matches!(msgs[0], ChatMessage::User { .. }));
    }
}

// rig 没给 CompletionRequest 派生 Default，这里给个 test-only helper
// 放在 mod tests 后纯粹因为要 `use super::*` 才能拿到 Text/UserContent；clippy lint 允许
#[cfg(test)]
#[allow(clippy::items_after_test_module)]
trait DefaultForTests {
    fn default_for_tests() -> Self;
}
#[cfg(test)]
#[allow(clippy::items_after_test_module)]
impl DefaultForTests for CompletionRequest {
    fn default_for_tests() -> Self {
        use rig::completion::message::UserContent;
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
}

// Send / Sync 编译期断言（已挪到文件顶部，clippy 抱怨 items_after_test_module）

// 占位：M3+ ingest pipeline 用 `EmbeddingsBuilder` + `Embed` trait，
// 之后加 ingest 时直接 `use rig::embeddings::EmbeddingsBuilder;` 即可
