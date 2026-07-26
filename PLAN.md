# lorag — 本地 Agent RAG 项目规划

> 状态：**M0–M5 已完成；`lorag doctor` 已实装；M6 smoke test 待办**。
> - M0 CLI 骨架 + config：✅ `lorag --help` / `lorag models status` 跑通
> - M1 `AhaClient::init` 实装 load_model：✅ 0.6B 端到端 load 成功（4B 8GB+ 等用户手动验证）
> - 后续优化：`AhaClient::init_embed_only` 只 load embedding（ingest 路径省 LLM 的 8GB 内存 + 数十秒 load）
> - M2 文档 loaders：✅ pdf / docx / pptx / xlsx / md / txt 6 种
> - M3 摄入 pipeline：✅ chunker + lancedb_store + sqlite_store
> - M4 rig 0.40 provider 适配：✅ `lorag query "1+1=?"` 拿到 `"1 + 1 = 2"`（via rig CompletionModel）
> - M5 RAG 端到端：✅ **重写绕开 `dynamic_context` 的 62GB 内存 bug**；`lorag query` 跑通
> - `lorag doctor`：✅ 诊断命令（env / models / storage / build features，11 项检查）
> - `lorag reindex`：✅ M5.1 实装（清 LanceDB + SQLite 后重新摄入，用于换 EMBED_MODEL 后）
> - 待办：M6 smoke test
>
> 后续功能（chat REPL 流式、多用户、混合检索、re-rank、ANN 索引）会在 `## 后续迭代` 一节追加。

---

## 1. 项目目标

构建一个**完全本地运行**的 Agent RAG 工具：

- 支持常见办公文档（ppt / excel / word / pdf / markdown / txt）批量向量化入 LanceDB
- 通过 CLI 指定文件或目录进行摄入
- 内置 **通用 LLM** + **向量模型** 两个本地模型，由 [aha](https://github.com/jhqxxx/aha) 提供
- 通过 `.env` 配置文件灵活切换模型、模型路径、数据库路径
- 用 [rig](https://rig.rs) 框架做 agent 编排（preamble / memory / tools / dynamic context）
- 最小可执行程序（MVP）覆盖：**文档摄入 + 一次性 RAG 问答**

非 MVP 目标（明确不做，后续追加）：
- 多轮对话 REPL（schema 留好，命令留占位）
- 流式输出
- Web UI / HTTP 服务
- 混合检索（BM25 + 向量）
- re-rank
- 多用户 / 权限

---

## 2. 技术栈

| 组件 | 选型 | 用途 |
|------|------|------|
| 语言 | Rust 2021 edition | — |
| LLM / Embedding 推理 | [`aha = { path = "D:/workspace/rust/aha" }`](https://github.com/jhqxxx/aha) | aha 是 Rust + Candle 写的本地推理引擎；lorag 直接 `use aha::*` 调库，**不起 HTTP 进程** |
| Agent / RAG 框架 | `rig` **0.40** + `rig-lancedb` 0.40（M3+ 才用） | provider / agent / completion / embedding / vector store 抽象；为 aha 实现一个 **自定义 provider**（详见 §6.2） |
| 向量库 | `lancedb` 0.30（M3+ 才用） | 嵌入式向量存储（通过 `rig-lancedb` 集成使用） |
| 关系库 | `rusqlite` (bundled)（M3+ 才用） | 文档元数据 / chunk 映射 / 摄入历史 |
| 文档解析（M2 才用） | `pdf-extract`（pdf）<br>`calamine`（excel）<br>`zip` + `quick-xml`（docx / pptx 抽 XML 文本）<br>`pulldown-cmark`（md）<br>std fs（txt） | 各类文档转纯文本 |
| 文本切分 | 自己实现简单 chunker（按段落 / 字符） | — |
| CLI | `clap` v4 (derive) | 命令解析 |
| 配置 | `dotenvy` + 手 parse（.env → `AppConfig`） | `.env` 加载 |
| 异步运行时 | `tokio` (rt-multi-thread + macros) | 调度 spawn_blocking 包裹同步 candle 推理 |
| 错误处理 | `anyhow` + `thiserror` | — |
| 日志 | `tracing` + `tracing-subscriber` | — |
| 工具 | `dirs = "6"` | `~/.aha/` 兼容 aha CLI 老用户 |

> **关于 aha 集成方式**（关键决策）：
> aha 同时提供 HTTP server（OpenAI 兼容）和标准 Rust crate（`pub mod` + `pub use`）。
> aha 自己的 server 也是用 lib API 写的（`aha/src/server/api.rs:5-7` 直接 `use aha::models::load_model;`）。
> lorag **全程走 crate 路线**（`aha = { path = "D:/workspace/rust/aha" }`），不调任何 aha CLI、不起 HTTP：
> - 单进程同时持有 LLM + embedding 两个模型实例
> - 不需要端口、不需要 base_url、不需要 health check、不需要进程管理
> - 函数调用直接传错，错误处理统一 `anyhow`
> - 模型下载也走 `aha::utils::download_model`（aha 自己的 CLI 也是调它）

> **关于 rig 集成**：
> 为 rig 实现一个 **自定义 provider**（`AhaClient` + `AhaCompletionModel` + `AhaEmbeddingModel`）。
> 实现完成后，rig 的所有上层能力（`AgentBuilder` / `EmbeddingsBuilder` / `LanceDbVectorIndex`）天然可用。
> 详细设计见 `§6.2 aha_provider`。

---

## 3. 关于 aha 的事实（必读）

1. **aha 一次只能加载一个模型**（同一个进程只能 serve 一个 model）。
   → 即使走 crate 路线，单个 `ModelInstance<'static>` 也只能装一个模型。
   → LLM 和 embedding **必须各 load 一次**，但可以在**同一进程内**同时持有两个实例（用 `Arc<RwLock<>>` 共享）。
2. **aha 公开的 lib API**（`aha = "0.2.6"`，源码 `lib.rs:1-9`）：
   ```rust
   pub mod chat_template;
   pub mod exec;
   pub mod models;       // load_model, load_gguf_model, Qwen3Embedding, ...
   pub mod params;       // ChatCompletionParameters, ...
   pub mod position_embed;
   pub mod tokenizer;
   pub mod utils;        // string_to_static_str, download_model, is_model_downloaded, ...
   pub use candle_core::{DType, Device, Tensor};
   ```
3. **加载 / 下载模型的关键 API**（`aha/src/utils/mod.rs:498, 652` + `models/mod.rs`）：
   ```rust
   use aha::models::load_model;                       // 通用 safetensors 模型
   use aha::models::load_gguf_model;                  // gguf 格式
   use aha::models::common::model_mapping::WhichModel;
   use aha::models::ModelInstance;
   use aha::utils::{string_to_static_str, download_model, is_model_downloaded};
   use clap::ValueEnum;

   // 加载本地模型
   let which: WhichModel = WhichModel::from_str("Qwen/Qwen3-4B", true).unwrap();
   let path: &'static str = string_to_static_str(format!("{}/{}", MODELS_DIR, which.as_string()));
   let model: ModelInstance<'static> = load_model(which, path, None, None)?;

   // 下载模型（aha CLI 的 `aha download` 底层也是调这个）
   download_model("Qwen/Qwen3-4B", MODELS_DIR, 3).await?;
   ```
4. **推理 API**：
   - LLM：`GenerateModel::generate(mes: serde_json::Value) -> Result<Value>`（aha 自己 exec 层就用这个）
   - Embedding：`TextEmbedding::embed_texts(&mut self, input: &[String]) -> Result<Vec<Vec<f32>>>`（来自 `models/common/embedding.rs`）
5. **aha 官方支持的模型**（从 `model_mapping.rs`）：
   - **LLM**：`Qwen/Qwen3-{0.6B,1.7B,4B}`、`Qwen/Qwen3.5-{0.8B,2B,4B,9B}`、`OpenBMB/MiniCPM4-0.5B`、`OpenBMB/MiniCPM5-1B`、`LiquidAI/LFM2-1.2B`、`LiquidAI/LFM2.5-1.2B-Instruct`
   - **Embedding**：`sentence-transformers/all-MiniLM-L6-v2`（384 维）、`Qwen/Qwen3-Embedding-{0.6B,4B,8B}`（1024 / 2560 / 4096 维）
6. **所有调用都是库内直接函数调用**——本项目不走 HTTP 协议。
7. **`download_model` 和 `is_model_downloaded` 路径不同步**（aha crate 行为坑）：
   - `aha::utils::download_model(id, save_dir, ...)` 把模型下到 `<save_dir>/<id>/`
   - `aha::utils::is_model_downloaded(which)` 写死查 `~/.aha/{id}/`
   - `aha::utils::get_default_weight_path(which)` 返回 `~/.aha/{id}/`
   - **aha 自己的 CLI `aha list` 也踩这个坑**：`aha download -m X -s /tmp/foo` 下完，`aha list` 仍显示 X 未下
   - 我们的 workaround：不依赖 aha 的 `is_model_downloaded`，自己写 `resolve_model_path`——
     优先 `MODELS_DIR/{repo}/`，兜底 `~/.aha/{repo}/`，"已下"判断 = 目录存在 + `config.json` + 至少一个 `*.safetensors`
     （aha `init(path)` 实际期待的文件结构，详见 `src/aha_provider.rs`）

---

## 4. 架构

### 4.1 高层视图

```
┌───────────────────────────────────────────────────────────────┐
│                          lorag (单 binary, 单进程)                │
│                                                                │
│  ┌──────────┐  ┌──────────┐  ┌──────────┐  ┌───────────────┐    │
│  │  ingest  │  │  query   │  │  models  │  │     ...       │    │
│  └────┬─────┘  └────┬─────┘  └────┬─────┘  └───────────────┘    │
│       │             │             │                              │
│  ┌────▼─────────────▼─────────────▼─────────────────────────┐  │
│  │                  core 模块（业务逻辑）                    │  │
│  │  loader → chunker → EmbeddingsBuilder → LanceDbVectorIdx │  │
│  │                                          → top_n         │  │
│  │                          → Agent::prompt → aha.generate   │  │
│  └────┬────────────┬───────────────────┬────────────────────┘  │
│       │            │                   │                         │
│  ┌────▼────┐  ┌────▼────┐  ┌──────────▼────────┐               │
│  │  store  │  │  chunker│  │  aha_provider     │               │
│  │lancedb+ │  │         │  │ (rig 适配)        │               │
│  │ sqlite  │  │         │  │ + 模型下载/加载    │               │
│  └─────────┘  └─────────┘  └──────────┬────────┘               │
│                                       │                          │
│                                ┌──────▼───────┐                  │
│                                │     aha      │  ← Rust crate 依赖  │
│                                │  (lib API)   │     load_model +    │
│                                │              │     generate +      │
│                                │              │     embed_texts +   │
│                                │              │     download_model  │
│                                └──────────────┘                  │
└───────────────────────────────────────────────────────────────┘
```

### 4.2 数据流

**摄入（ingest）**：
```
file path
  → loader::extract(path)               # 解析为纯文本
  → chunker::split(text)                # 按段落/字符切块
  → vec::pipeline::embed(chunks)        # rig::EmbeddingsBuilder + AhaEmbeddingModel
  → store::indexer::add(embeds + meta)  # LanceDbVectorIndex::insert_documents
  → store::sqlite::record_sources       # 写 sqlite 元数据
```

**问答（query）**：
```
user question
  → vec::retrieve::top_k(question, k=5)   # LanceDbVectorIndex::top_n
  → rag::build_prompt(retrieved)         # 拼 system + context + question
  → llm::agent::prompt(prompt)           # AhaCompletionModel::completion
  → aha::generate(params)                # candle 同步推理
  → print answer
```

**下载模型（models pull）**：
```
lorag models pull
  → aha_provider::ensure_model_downloaded(LLM_MODEL, MODELS_DIR)
      → aha::utils::download_model(model_id, save_dir, max_retries).await
      → aha::utils::is_model_downloaded(WhichModel::from_str(...)) 二次确认
  → aha_provider::ensure_model_downloaded(EMBED_MODEL, MODELS_DIR)
  → 打印 "ok: <repo> at <path> (<size>)"
```

---

## 5. 目录结构（MVP）

> **实装状态**：
> - ✅ `src/main.rs`（CLI 骨架 + `init` / `query` / `models` / `sources` / `reindex` / `chat` / `doctor` 命令）
> - ✅ `src/lib.rs`（模块声明）
> - ✅ `src/config.rs`（dotenvy + `AppConfig` + validate）
> - ✅ `src/aha_provider.rs`（path resolve + `AhaClient::init` + `llm_generate` / `embed_texts`）
> - ✅ `src/rig_compat.rs`（rig 0.40 provider 适配）
> - ✅ `src/rag.rs`（**M5 重写**：手写 lancedb 路径 + 拼 context + LLM，绕开 `dynamic_context` 62GB bug）
- M2：✅ `ingest/*` loader 全部 6 种已实装
- M3：✅ `chunker.rs` / `models.rs` / `ingest/pipeline.rs` / `store/*` 已实装
- M5：✅ `src/rag.rs` 重写（绕开 `dynamic_context` 62GB bug）

```
lorag/
├── Cargo.toml                      # aha path 依赖；rig 0.40 走 crates.io（M3+ 才用 rig-lancedb / lancedb / rusqlite）
├── .env.example                    # 含默认 model id（EMBED_DIM 自动从模型读）
├── .env                            # gitignore
├── .env.smoke                      # gitignore；0.6B 小模型 smoke 配置
├── .gitignore
├── README.md
├── PLAN.md
├── AGENTS.md
├── src/
│   ├── main.rs                     # ✅ CLI 入口（clap）：init / query / models / sources / reindex / chat / doctor
│   ├── lib.rs                      # ✅ 模块声明（pub mod aha_provider / config / rig_compat / rag）
│   ├── config.rs                   # ✅ AppConfig：LLM_MODEL / EMBED_MODEL / MODELS_DIR / ...
│   ├── aha_provider.rs             # ✅ AhaClient + ensure_model_downloaded + models_status + resolve_model_path
│   ├── rig_compat.rs               # ✅ AhaCompletionModel + AhaEmbeddingModel + message convert
│   ├── rag.rs                      # ✅ M5 重写：手写 embed + lancedb vector_search + 拼 context + completion
│   ├── chunker.rs                  # ✅ M3
│   ├── models.rs                   # ✅ M3
│   ├── ingest/
│   │   ├── mod.rs                  # ✅ M2
│   │   ├── loader.rs               # ✅ M2：按扩展名分派
│   │   ├── pdf.rs                  # ✅ M2
│   │   ├── docx.rs                 # ✅ M2
│   │   ├── pptx.rs                 # ✅ M2
│   │   ├── xlsx.rs                 # ✅ M2
│   │   ├── md.rs                   # ✅ M2
│   │   ├── txt.rs                  # ✅ M2
│   │   └── pipeline.rs             # ✅ M3：文件→chunks→embeddings→lancedb+sqlite
│   └── store/
│       ├── mod.rs                  # ✅ M3
│       ├── lancedb_store.rs        # ✅ M3
│       └── sqlite_store.rs         # ✅ M3
├── tests/
│   └── smoke.rs                    # ⏳ M6
├── data/                           # .gitignore
    ├── lancedb/                    # ✅ M3
    ├── models/                     # aha 下载到这里
    └── lorag.db                    # ✅ M3
```

---

## 6. 模块设计

### 6.1 `config.rs`

从 `.env` 加载，强类型校验，提供 `AppConfig` 结构体。

**核心字段**：

| 字段 | 含义 | 默认 |
|------|------|------|
| `LLM_MODEL` | LLM 模型 id（aha 用，HF/ModelScope repo 形式） | 必填 |
| `EMBED_MODEL` | Embedding 模型 id | 必填 |
| `MODELS_DIR` | 模型下载/加载目录（`aha::utils::download_model` 下到 `<MODELS_DIR>/<MODEL>/`） | `./data/models` |
| `DOWNLOAD_MAX_RETRIES` | `aha::utils::download_model` 的重试次数 | `3` |
| `LANCEDB_DIR` | lancedb 数据目录 | `./data/lancedb` |
| `SQLITE_PATH` | sqlite 文件路径 | `./data/lorag.db` |
| `EMBED_DIM` | **已废弃** | 从 `embedding 模型 config.json::hidden_size` 自动读出，**不用配** |
| `CHUNK_SIZE` | chunk 字符上限 | `500` |
| `CHUNK_OVERLAP` | chunk 滑动窗口重叠 | `50` |
| `TOP_K` | 检索 top_k | `5` |

### 6.2 `aha_provider.rs`（★ 核心，✅ M0+M1 已实装）

**职责**：
- 路径解析（`resolve_model_path`：MODELS_DIR 优先 + `~/.aha/` 兜底 + 严格"已下"判断 = 目录存在 + `config.json` + 至少一个 `*.safetensors`）
- 模型下载（`ensure_model_downloaded`：调 `aha::utils::download_model`）
- 模型状态报告（`models_status` + `print_models_status`：`lorag models status` 用）
- 模型加载（`AhaClient::init`：调 `aha::models::load_model` 把 LLM + embedding 加载进内存）
- async helper（`AhaClient::llm_generate` / `AhaClient::embed_texts`：candle 同步包成 `tokio::task::spawn_blocking`）

**关键设计**：
- LLM 和 embedding 用**各自**的 `Arc<tokio::sync::Mutex<ModelInstance<'static>>>`，不共享一个 mutex，embed 和 generate 能并发
- 路径解析用 `resolve_model_path`（**不**用 aha 的 `is_model_downloaded` —— 它写死查 `~/.aha/`，跟 `download_model` 的 `save_dir` 路径不同步，aha 自己的 `aha list` 也踩这个坑；详见 §3.7）
- `ModelInstance<'static>` 要求 weight path 是 `&'static str` → 用 `aha::utils::string_to_static_str(path)` leak（启动时一次性，可接受 ~100 字节 leak）

**实装代码**（`src/aha_provider.rs`）：

```rust
pub struct AhaClient {
    llm: Arc<tokio::sync::Mutex<ModelInstance<'static>>>,
    embed: Arc<tokio::sync::Mutex<ModelInstance<'static>>>,
    cfg: Arc<AppConfig>,
}

impl AhaClient {
    pub async fn init(cfg: AppConfig) -> Result<Self> {
        // 1. 校验 id + 找本地路径
        let llm_which = WhichModel::from_str(&cfg.llm_model_repo, true)?;
        let llm_path = resolve_model_path(&cfg.llm_model_repo, &cfg.models_dir)
            .ok_or_else(|| anyhow!("LLM model not found ..."))?;
        // 2. leak path → &'static str
        let llm_path_str = string_to_static_str(llm_path.to_string_lossy().into_owned());
        // 3. spawn_blocking 调 candle 同步 load_model
        let llm = tokio::task::spawn_blocking(move || load_model(llm_which, llm_path_str, None, None))
            .await??;
        Ok(Self { llm: Arc::new(Mutex::new(llm)), embed: ..., cfg: Arc::new(cfg) })
    }

    pub async fn llm_generate(&self, params: ChatCompletionParameters) -> Result<ChatCompletionResponse> {
        let llm = self.llm.clone();
        tokio::task::spawn_blocking(move || llm.blocking_lock().generate(params)).await?
    }

    pub async fn embed_texts(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        let embed = self.embed.clone();
        let texts = texts.to_vec();
        tokio::task::spawn_blocking(move || {
            let mut g = embed.blocking_lock();
            match &mut *g {
                ModelInstance::AllMiniLML6V2(m) => m.embed_texts(&texts),
                ModelInstance::Qwen3Embedding(m) => m.embed_texts(&texts),
                other => Err(anyhow!("not an embedding model: {}", std::any::type_name_of_val(other))),
            }
        }).await?
    }
}
```

**关键参考**：
- aha 自己 server 怎么调 `load_model` + `GenerateModel::generate`：`aha/src/server/api.rs:5-7, 36-56`（同样用 `string_to_static_str` leak）
- aha 的 `ModelInstance` 枚举定义：`aha/src/models/mod.rs:77-98`
- aha crate 的模型下载 API：`aha/src/utils/mod.rs:498-533`（`download_model`）
- aha crate 的"模型是否已下载"检测（**避开**，路径不同步坑）：`aha/src/utils/mod.rs:650-661`

### 6.3 `rig_compat.rs`（★ 核心，✅ M4 已实装）

rig 0.40 provider 适配：把 `AhaClient` 装成 rig 能用的 client。

**实现范围（MVP）**：
- `impl CompletionClient for AhaClient` → `AhaCompletionModel`
- `impl EmbeddingsClient for AhaClient` → `AhaEmbeddingModel`
- `impl CompletionModel for AhaCompletionModel`（`stream()` 留 `Err(ProviderError)`）
- `impl EmbeddingModel for AhaEmbeddingModel`

**不实现**：
- `Provider` / `ProviderClient`（rig 0.40 的版本是给 HTTP client 用的：`reqwest::Client` + `from_env` / `from_val`，我们走 in-process 推理，不需要）
- 流式输出（`type StreamingResponse = ()`，`()` 已实现 `GetTokenUsage`；`stream()` 直接 `Err`）
- tool calls / structured output（MVP 留 TODO）

**rig 0.40 API 关键变化（vs 0.39）**：
- `OneOrMany` 在 0.39 是 enum，0.40 改成 `struct { Vec<T> | single T }`，配套 `first()` / `iter()` / `one()` / `many()` 方法
- `CompletionModel` 0.40 有 associated function `make(client, model) -> Self`（0.39 是 `completion_model(&self, name)` on client）
- `EmbeddingModel::make(client, model, dims: Option<usize>) -> Self`（0.40 多 `dims` 参数 + 必实现 `ndims()`）
- `CompletionResponse` 0.40 多了 `raw_response: T` / `usage: Usage` / `message_id: Option<String>` 字段
- `type Response: Serialize + DeserializeOwned` —— 直接复用 `aha::params::chat::ChatCompletionResponse`（aha 已有 derives）
- `WasmCompatSend: Send`（native 编译时等价 `Send`）—— 所有跨 await 持有的类型都得 `Send + Sync`

**消息转换**（`convert_messages`）：

```
rig CompletionRequest
  ├─ preamble: Option<String>              → aha ChatMessage::System (插最前)
  ├─ documents: Vec<Document>             → aha ChatMessage::System ("[id] text\n\n..." 拼一个块)
  └─ chat_history: OneOrMany<Message>     → 逐条转
       ├─ Message::System                 → ChatMessage::System
       ├─ Message::User                    → 抽 UserContent::Text 拼 string → ChatMessage::User
       └─ Message::Assistant               → 抽 AssistantContent::Text 拼 string → ChatMessage::Assistant
```

aha response → rig 转换：抽第一条 choice 的 `ChatMessage::Assistant.content` 的 text，包成 `AssistantContent::Text(Text { text, additional_params: None })`。

**实装代码**（`src/rig_compat.rs`）：

```rust
pub struct AhaCompletionModel { client: AhaClient, model: String }

impl CompletionModel for AhaCompletionModel {
    type Response = aha::params::chat::ChatCompletionResponse;
    type StreamingResponse = ();  // () 已有 GetTokenUsage impl
    type Client = AhaClient;

    fn make(client: &Self::Client, model: impl Into<String>) -> Self { ... }

    async fn completion(&self, req: CompletionRequest) -> Result<CompletionResponse<Self::Response>, CompletionError> {
        let params = ChatCompletionParameters {
            messages: convert_messages(&req),
            model: self.model.clone(),
            temperature: req.temperature.map(|t| t as f32),
            max_tokens: req.max_tokens.map(|n| n as u32),
            stream: Some(false),
            ..Default::default()
        };
        let resp = self.client.llm_generate(params).await
            .map_err(|e| CompletionError::ProviderError(e.to_string()))?;
        let text = resp.choices.first()
            .and_then(|c| extract_assistant_text(&c.message))
            .unwrap_or_default();
        Ok(CompletionResponse {
            choice: OneOrMany::one(AssistantContent::Text(Text { text, additional_params: None })),
            usage: Usage::new(),
            raw_response: resp,
            message_id: None,
        })
    }

    async fn stream(&self, _: CompletionRequest) -> Result<StreamingCompletionResponse<Self::StreamingResponse>, CompletionError> {
        Err(CompletionError::ProviderError("streaming is not supported in MVP".to_string()))
    }
}

impl CompletionClient for AhaClient {
    type CompletionModel = AhaCompletionModel;
    fn completion_model(&self, model: impl Into<String>) -> Self::CompletionModel {
        AhaCompletionModel { client: self.clone(), model: model.into() }
    }
}

pub struct AhaEmbeddingModel { client: AhaClient, model: String, ndims: usize }

impl EmbeddingModel for AhaEmbeddingModel {
    const MAX_DOCUMENTS: usize = 1024;  // aha 实际限制
    type Client = AhaClient;
    fn make(client: &Self::Client, model: impl Into<String>, dims: Option<usize>) -> Self { ... }
    fn ndims(&self) -> usize { self.ndims }
    async fn embed_texts(&self, texts: impl IntoIterator<Item = String> + Send)
        -> Result<Vec<Embedding>, EmbeddingError> {
        // 调 aha::embed_texts，转 f32→f64（rig Embedding.vec 是 Vec<f64>）
    }
}
```

**关键参考**：
- rig 官方 custom provider 文档：https://rig.rs/docs/guides/extension/write_your_own_provider
- rig 0.40 实际 API（在 `~/.cargo/registry/src/.../rig-core-0.40.0/src/`）：
  - `client/completion.rs`（`CompletionClient` trait）
  - `client/embeddings.rs`（`EmbeddingsClient` trait）
  - `completion/request.rs`（`CompletionModel` / `CompletionRequest` / `CompletionResponse`）
  - `embeddings/embedding.rs`（`EmbeddingModel` / `Embedding`）
  - `completion/message.rs`（`Message` / `Text` / `UserContent` / `AssistantContent`）
  - `streaming.rs`（`StreamingCompletionResponse`）
  - `one_or_many.rs`（`OneOrMany` struct，不是 enum！）

### 6.4 ~~`aha_runner.rs`~~

**删除**。本项目不调 aha CLI：推理走 `aha_provider` 内的 `load_model`，下载走 `aha_provider::ensure_model_downloaded`（调 `aha::utils::download_model`）。不需要独立的 runner 模块。

### 6.4 `ingest::pipeline.rs`

```rust
pub async fn run_ingest(
    client: &AhaClient,
    sources: Vec<PathBuf>,
) -> anyhow::Result<()> {
    for path in sources {
        let text = loader::extract(&path)?;
        let chunks = chunker::split(&text, &path, config.chunk_size, config.chunk_overlap);

        // 1. 摄入幂等检查：sqlite 查 source_hash
        // 2. rig::EmbeddingsBuilder 批量 embed
        let embed_model = client.embedding_model(&config.embed_model_name);
        let embeddings = EmbeddingsBuilder::new(embed_model)
            .documents(chunks_with_embed_derive)?
            .build().await?;
        // 3. 写入 lancedb
        let index = open_lancedb_index(...).await?;
        index.insert_documents(embeddings).await?;
        // 4. 写 sqlite 元数据
        sqlite_store::record_source(&path, &chunks).await?;
    }
    Ok(())
}
```

### 6.5 `rag.rs`（✅ M5 已实装，**重写绕开 62GB 内存 bug**）

#### 6.5.1 为什么不用 `AgentBuilder::dynamic_context` + `LanceDbVectorIndex`？

实测 `rig 0.40` + `rig-lancedb 0.40` + `lancedb 0.30` 这条集成链路在 5 chunks 的小数据上会触发：
```
memory allocation of 62864906528 bytes failed
```
（~62GB 单次分配；用户机器 64GB DDR4，进程被 OOM 干死。）

是 rig-lancedb 内部某步把整个表 / 整列 / 整个 index 一次性读进 `Vec<f32>`（盲猜）——不是数据量问题，是代码路径问题（5 chunks 也炸）。

#### 6.5.2 重写方案：手写 lancedb native API

不用 `LanceDbVectorIndex`、不用 `AgentBuilder::dynamic_context`。直接调 lancedb 原生查询 + rig 的 `CompletionModel::completion`：

```rust
// 1. embed question
let question_f32: Vec<f32> = embed_model.embed_text(question).await?.vec.iter().map(|f| *f as f32).collect();

// 2. 打开 lancedb
let db = lancedb::connect(&cfg.lancedb_dir).execute().await?;
let table = db.open_table("documents").execute().await?;

// 3. vector_search top_k（lancedb 原生 API）
let mut stream = table.vector_search(&question_f32)?.limit(top_k).execute().await?;

// 4. 从 RecordBatch 抽 text
let mut chunks = Vec::new();
while let Some(rb) = stream.next().await.transpose()? {
    let text_col = rb.column_by_name("text")?.as_any().downcast_ref::<StringArray>()?;
    for i in 0..rb.num_rows() {
        let text = text_col.value(i);  // arrow-array 58: 返回 &str（不是 Option）
        if !text.is_empty() { chunks.push(text.to_string()); }
    }
}

// 5. 拼 context，喂 LLM
let preamble = format!("根据【上下文】回答：\n{context}");
llm_model.completion(CompletionRequest { preamble, chat_history: OneOrMany::one(Message::user(question)), .. }).await
```

每步都自己控制：`top_k=3..5` 限定 batch，无 `Vec<f32>` 整体拉取，5 chunks 也能跑。

#### 6.5.3 Fallback 行为

`rag_query` 包了一层：如果 lancedb 还没数据（目录不存在 / `documents` 表不存在 / lance 任何错误），自动 fallback 到 **裸 LLM**（`bare_llm_query`）。这样：
- 首次跑通也能对话
- REPL / `lorag query` 任何时候都能用
- 有数据时享受 RAG，没数据时退化为 LLM（不报错）

`is_recoverable_error` 匹配关键字：`lancedb` / `Lance` / `documents table` / `run lorag ingest` / `memory allocation` / `No such file`。

#### 6.5.4 `lorag chat --no-rag` flag

`cmd_chat` 多了一个 `--no-rag` flag：直接走 LLM，完全不碰 lancedb。用途：
- 跑通后想快速对话、不想 load LanceDB
- 没摄入文档时测试 LLM 本身

#### 6.5.5 关键实现细节

- `arrow_array::StringArray::value(i)` 在 arrow 58 返回 `&str`（**不是** `Option<&str>`，是 53 之前某些版本的 API，58 已变）
- `RecordBatchStream` trait 在 `lancedb::arrow`，但用 `futures::StreamExt` 拿 `.next()` 时不需要这个 trait 在 scope
- 上下文拼装格式：`[1] text1\n\n[2] text2\n\n...`（标号 + 双换行分隔）
- 检索 system prompt 强制："仅根据【上下文】回答；上下文无法覆盖时说"未在文档中找到相关信息"；不要编造"——`temperature=0.1` 减少随机性
- bare LLM 的 preamble："你是一个简洁的助手，用一两句话直接回答问题"

#### 6.5.6 IVF-HNSW 索引（数据量 ≥256 时自动建）

lancedb 0.30 提供三种 IVF-HNSW 索引：
- `IvfHnswFlatIndexBuilder` — 存原始向量（最高 recall，最占内存）—— **lorag 默认**
- `IvfHnswSqIndexBuilder` — IVF-HNSW + scalar quantizer（4x 压缩）
- `IvfHnswPqIndexBuilder` — IVF-HNSW + product quantizer（更高压缩）

API：
```rust
table.create_index(
    &["embedding"],
    lancedb::index::Index::IvfHnswFlat(IvfHnswFlatIndexBuilder::default()),
).execute().await?
```

**关键约束**：lance 的 IVF 训练（kmeans）要求至少 **256 行**才能跑。lorag 的策略：
- `< 256` 行：silently 跳过（继续 ENN 全表扫），`tracing::debug!` 记录
- `≥ 256` 行 + 没建过：建 IVF-HNSW-FLAT 索引，打印 `building HNSW index on embedding...` 进度行
- 已经建过：跳过（幂等）

挂在 `ingest_one` 写完 lancedb 之后；HNSW 失败不阻塞 ingest（warn log + warning 打印行，continue）。

```rust
// store/lancedb_store.rs
pub const HNSW_MIN_ROWS: usize = 256;
pub async fn ensure_hnsw_index(table: &lancedb::Table) -> Result<()> {
    let row_count = table.count_rows(None).await?;
    if row_count < HNSW_MIN_ROWS {
        tracing::debug!("HNSW index requires >= {} rows, have {}; skipping (will use ENN)", HNSW_MIN_ROWS, row_count);
        return Ok(());
    }
    if table.index_stats("embedding").await?.is_some() {
        return Ok(());
    }
    println!("  building HNSW index on `embedding` (rows={})...", row_count);
    table.create_index(&["embedding"], lancedb::index::Index::IvfHnswFlat(
        lancedb::index::vector::IvfHnswFlatIndexBuilder::default(),
    )).execute().await?;
    println!("  HNSW index built");
    Ok(())
}
```

查询时**不需要**额外传参——lancedb 检测到有 HNSW 索引就会自动走 ANN。`SearchParams::default()` 用 ENN（无索引时）或 ANN（有索引时）。

### 6.6 `store::lancedb_store.rs`

- 表名固定 `documents`
- schema：
  - `id: Utf8`（`{source_path_hash}:{chunk_ordinal}`）
  - `source_path: Utf8`
  - `chunk_ordinal: Int64`
  - `text: Utf8`
  - `embedding: FixedSizeList<Float64, N>` —— N 从 `AhaClient.embed_dim()` 拿，Float64 跟 aha embed 输出对齐
- MVP 用 `LanceDbVectorIndex::new(table, embed_model, "id", SearchParams::default())` 包装
- 数据量小直接 ENN（exact），不建 ANN 索引

### 6.7 `store::sqlite_store.rs`

```sql
CREATE TABLE IF NOT EXISTS sources (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    source_path   TEXT NOT NULL UNIQUE,
    source_hash   TEXT NOT NULL,            -- 文件内容 sha256
    file_type     TEXT NOT NULL,            -- ext
    ingested_at   TEXT NOT NULL,            -- ISO 8601
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

CREATE INDEX IF NOT EXISTS idx_chunks_source ON chunks(source_id);
```

用途：摄入幂等（按 hash skip）、来源追踪、列出已摄入（`lorag sources list`）、按 source 删除（schema 留好，UI 留 TODO）。

### 6.8 `chunker.rs`

- 按 `\n\n` 切段（段落级）
- 每段超过 `CHUNK_SIZE` 字符时按 `CHUNK_SIZE` 滑动窗口切，叠加 `CHUNK_OVERLAP` 字符重叠
- 输出 `Vec<Chunk { text, ordinal, source_path }>`

### 6.9 文档 loader

按文件扩展名分派：

| ext | 解析方式 | crate |
|-----|----------|-------|
| `.pdf` | `pdf-extract` | `pdf-extract` |
| `.docx` | 解 zip → 读 `word/document.xml` 抽 `<w:t>` 文本 | `zip` + `quick-xml` |
| `.pptx` | 解 zip → 遍历 `ppt/slides/slide*.xml` 抽 `<a:t>` 文本 | `zip` + `quick-xml` |
| `.xlsx` | `calamine::open_workbook_auto` → 各 sheet 文本拼接 | `calamine` |
| `.md` | `pulldown-cmark` 解析，提取 text 节点 | `pulldown-cmark` |
| `.txt` | utf-8 读 | std |

> 单个 loader 失败时记 warning 跳过，不中断整个 ingest。

---

## 7. CLI 命令（MVP）

```
lorag ingest <PATH>...           # ✅ M2+M3 实装：PATH 可以是文件或目录（递归），支持混合多个 PATH
    --ext pdf,docx,pptx,xlsx,md,md,txt   # 默认全开
    --force                              # 强制重摄入（忽略 hash）
    --recursive / --no-recursive         # 默认 recursive

lorag query <QUESTION>           # ✅ M5 实装：一次性 RAG 问答
    --top-k <N>                  # 默认 5

lorag models pull                # ✅ M0 实装：调 aha::utils::download_model 把 LLM + embed 下到 MODELS_DIR
lorag models status              # ✅ M0 实装：打印模型文件存在性（path + "in MODELS_DIR / ~/.aha/"）
    --init                         # 额外：真把模型 load 进内存验证 AhaClient::init

lorag init                       # ✅ M1 实装：把 LLM + embedding 加载进内存
                                 #   （等价 `lorag models status --init`）

lorag sources list               # ✅ M3 实装：列出已摄入文件
    --json

lorag reindex <PATH>...          # ✅ M5.1 实装：清 LanceDB + SQLite 后重新摄入
    --ext <list>                  # 同 ingest
    --recursive / --no-recursive  # 同 ingest
    --yes / -y                    # 跳过 interactive 确认
    --dry-run                     # 只打印会做什么，不真删不真 ingest
                                 #   适用：换 EMBED_MODEL 后清数据；想完全重建
                                 #   不删模型文件（MODELS_DIR/）—— 模型仍走 `models pull`

lorag chat                       # ✅ M7 实装：多轮对话 REPL（带 SQLite 历史 + RAG + /reset /session 续接）
    --message <TEXT>      # 一次性首问（不读 stdin）
    --session <ID>        # 续接已有 session
    --no-history          # 不带历史（每轮独立）
    --no-rag              # 跳过 LanceDB 检索（纯 LLM）
    --no-banner           # 安静启动
    --top-k <N>           # 检索 top_k
```

> 全部命令均带 `--help`，错误信息用 `anyhow` 打印，exit code 1。
> 配置文件路径：默认当前目录 `.env`，可由 `LORAG_ENV` 环境变量覆盖。

### 7.2 日志过滤

默认用 `tracing` + `tracing-subscriber::fmt()`，但 **silence 掉 lance / lancedb / datafusion / arrow 的 INFO 噪声**（每次 query 都打一堆 `lance::dataset_events` / `lance::execution` plan_run / `lance::file_audit` log，太丑）。

实现（`src/main.rs`）：
```rust
// env_filter target 段是字面量，不是 glob；必须显式列全 lance 子模块
let lance_silence = ",lance::dataset_events=warn,lance::execution=warn,lance::io_events=warn,\
lance::file_audit=warn,lancedb=warn,datafusion=warn,arrow=warn";
let base = std::env::var("RUST_LOG")
    .or_else(|_| std::env::var("LOG_LEVEL"))   // 兼容旧 .env
    .unwrap_or_else(|_| "info".to_string());
let full_filter = format!("{base}{lance_silence}");
tracing_subscriber::fmt()
    .with_env_filter(
        tracing_subscriber::EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(&full_filter)),
    )
    .init();
```

**关键陷阱**（踩过）：
- 早期版本把 `LOG_LEVEL` 直接当完整 filter 用，丢了 `lance_silence` 后缀——`.env` 写 `LOG_LEVEL=info` 就把 lance 噪声全打开了
- 修法：`lance_silence` 写成**必加后缀**，不管 base 是什么都 `format!("{base}{silence}")` 拼上
- 排查时用 `RUST_LOG=info`（或更细的 `RUST_LOG=lance::execution=debug`）覆盖即可
> 同一时间**只能 init 一次**（AhaClient 持有 LLM + embedding 在内存），init 慢（4B ~30s-1min；0.6B ~5s）。

### 7.1 端到端验证方式

M0+M1+M4 验收的最快路径：

```powershell
# 1. 静态检查
cd D:\workspace\rust\lorag
cargo fmt --check; cargo clippy --all-targets -- -D warnings; cargo test --lib

# 2. 模型路径检查（秒级，不真 load）
.\target\debug\lorag.exe models status
# 预期：两个模型都 [ok]，一个标 ~/.aha/ 一个标 MODELS_DIR

# 3. 端到端 aha→rig 真跑通（推荐 4B + CUDA 1-3s/query；0.6B 纯 CPU 也能跑 ~5s 但质量弱）
$env:LORAG_ENV = ".env"           # 4B + 0.6B Embedding（推荐配置）
.\target\debug\lorag.exe init     # 加载
.\target\debug\lorag.exe query "1+1=?"  # 预期: "1 + 1 = 2"
```

---

## 8. `.env` 模板

```dotenv
# ----- 模型仓库 -----
# aha 官方支持的 LLM：Qwen3 / MiniCPM4/5 / LFM2/2.5
# aha 官方支持的 Embedding：
#   - sentence-transformers/all-MiniLM-L6-v2 → 384 维
#   - Qwen/Qwen3-Embedding-0.6B  → 1024 维
#   - Qwen/Qwen3-Embedding-4B    → 2560 维
#   - Qwen/Qwen3-Embedding-8B    → 4096 维
LLM_MODEL=Qwen/Qwen3-4B
EMBED_MODEL=Qwen/Qwen3-Embedding-0.6B

# ----- 模型下载位置（aha::utils::download_model 的 save_dir）-----
MODELS_DIR=./data/models

# ----- 下载重试次数 -----
DOWNLOAD_MAX_RETRIES=3

# ----- 向量维度 -----
# **不用配**。lorag 启动时从 embedding 模型的 config.json::hidden_size 自动读出来。
# lancedb schema 跟模型走；改 EMBED_MODEL 后**清掉数据库重建**：
#   rm -rf data/lancedb data/lorag.db

# ----- 数据库路径 -----
LANCEDB_DIR=./data/lancedb
SQLITE_PATH=./data/lorag.db

# ----- 文本切分 -----
CHUNK_SIZE=500
CHUNK_OVERLAP=50

# ----- 检索 -----
TOP_K=5
```

---

## 9. 开发里程碑

| 序号 | 状态 | 目标 | 验收 |
|------|------|------|------|
| M0 | ✅ | `cargo init` + 依赖（aha path / rig 0.40 / clap / dotenvy / anyhow / tracing / dirs）+ 配置加载 + CLI 骨架 | `lorag --help` 正常；M3+ 用的 rig-lancedb / lancedb / rusqlite 等先注释避免首次编译慢 |
| M1 | ✅ | `AhaClient::init` 调 `aha::models::load_model` 把 LLM + embedding 加载进内存；async helper `llm_generate` / `embed_texts` 用 `spawn_blocking` 包同步 candle | `lorag init` 端到端跑通（0.6B 5s load；4B 等用户手动验证，~1-3 min + 11GB 内存）；4 个 unit test 覆盖 `resolve_model_path` / `dir_has_model` |
| M2 | ✅ | loader 全部 6 种（pdf / docx / pptx / xlsx / md / txt） | 每个类型 fixture 文件解析出非空文本 |
| M3 | ✅ | chunker + sqlite + lancedb 摄入 + pipeline | `lorag ingest fixtures/` 跑通，sqlite + lancedb 都有数据，40 个 unit test 全部通过 |
| M4 | ✅ | rig 0.40 provider 适配（`AhaCompletionModel` + `AhaEmbeddingModel`） | unit + integration 测试通过（7 个 unit test 全过）；**`lorag query "1+1=?"` 真拿到 `"1 + 1 = 2"`**（端到端：aha crate load + candle 推理 + rig CompletionModel trait） |
| M5 | ✅ | `lorag query` 端到端 | retrieve → context → completion → 打印答案（**5 chunks 也能跑**，62GB 内存 bug 绕开） |
| M6 | ⏳ | smoke test + README | 跑通 `./scripts/smoke.sh` |

### 9.1 Cargo Profile（性能 + SSD 友好的开发循环）

```toml
# Cargo.toml
[profile.dev]
opt-level = 1    # dev build 跑 0.6B 实测 4.5s/query（vs full debug 142s）
debug = true

[profile.release]
opt-level = 3
lto = "thin"
codegen-units = 16   # 多核 link，6m33s cold → 30s incremental
strip = "symbols"
incremental = true
```

**开发约定**：
- 日常迭代用 `cargo build`（dev profile），**不**用 `cargo build --release`
- dev profile + opt-level=1 已经比 full debug 快 3-5x，足以让 0.6B 推理变成 4.5s/query（release 是 1.14s）
- release 链接冷启动会 5-10 分钟把 D 盘打 100%（lance + datafusion + rig + reqwest 全 link），所以**只在测性能时跑一次 release**，日常别跑
- incremental = true 让 release 重 build 变 ~30s

> **M5 关键经验**（重写后）：
> - **不能用** `AgentBuilder::dynamic_context` + `LanceDbVectorIndex` —— 5 chunks 也会爆 62GB 内存（实测，OOM 干死进程）
> - 改走手写：`embed_text` → `table.vector_search(&[f32])?.limit(k).execute()` → `RecordBatch` 流式读 → `StringArray::value(i)` 抽 text → 拼 context → `llm_model.completion(req)`
> - 5 chunks 实测 `iops=2 requests=2 bytes_read=20992`，无 allocation 爆炸
> - 加 `is_recoverable_error` fallback：lancedb 出错时自动转裸 LLM
> - `lorag chat --no-rag` flag：不走 lancedb，纯 LLM（应急 / 快速对话用）
> - arrow-array 58 的 `StringArray::value(i)` 返回 `&str`（**不是** `Option<&str>`，是早期版本 API）
> **M4 关键经验**（给未来接手的 agent）：
> - rig 0.40 跟 0.39 API 差异大；用 0.40 写的代码升级到 0.41+ 时**必须重看** `client/completion.rs` + `client/embeddings.rs` + `completion/request.rs`
> - 不实现 `Provider` trait——0.40 的 `Provider` 是给 HTTP-based provider 用的（要 `VERIFY_PATH` / `build_uri` / `with_custom`），in-process 推理不需要
> - `type StreamingResponse = ()` 是 MVP 最省事写法（`()` 已有 `GetTokenUsage` impl），`stream()` 直接返 `Err`
> - `OneOrMany` 在 0.40 是 `struct { single | Vec }`，配套 `first()` / `iter()` / `one()` / `many()` 方法（**不是** enum 模式匹配）

---

## 10. 风险与待确认

1. **aha 用 path 依赖**：`aha = { path = "D:/workspace/rust/aha" }` 仅你本机可编译。发布到 crates.io 时需切换 `aha = "0.2.6"`。
2. **aha 用 edition = "2024"**：aha 的 `Cargo.toml:3` 是 2024 edition。lorag 也升级到 2024 edition，利用 `if let` 链式语法等新特性。
3. **单进程内存叠加**：LLM 4B（~8GB FP16）+ Embedding 0.6B（~1.5GB）≈ 10GB RAM，机器要够。
4. **`ModelInstance<'static>` 内存 leak**：`string_to_static_str` 会 leak 两个 path 字符串（每次启动 ~100 字节），可接受。
5. **`stream()` unimplemented**：MVP 阶段 `CompletionModel::stream` 暂时 panic。后续加流式要参考 aha 的 `stream_completion`。
6. **换 embedding 模型要清数据库**：维度硬编码在 lancedb schema，换 `EMBED_MODEL` 时**必须重建 lancedb + sqlite**（`rm -rf data/lancedb data/lorag.db`，前者 schema 不匹配，后者 chunks 表 UNIQUE 约束撞索引）。
7. **大文档性能**：MVP 同步摄入，超大文件（>100MB）可能 OOM，后续做流式。
8. **PDF 解析**：`pdf-extract` 对扫描版 PDF 无效，扫描版得 OCR（aha 本身有 OCR 模型，后续迭代）。
9. **xlsx 多 sheet**：MVP 把所有 sheet 文本拼一起，不保留表结构。
10. **rig Custom Provider 复杂度**：`AhaCompletionModel` 实现大概 300 行（含类型 + trait + message convert），需要参考 rig 文档和 aha 自己的 server 源码。
11. **Windows 路径**：开发机是 Windows，配置默认是 Unix 风格（`./data/...`），`MODELS_DIR` 等用相对路径；调用 `aha::utils::download_model` 内部用 ModelScope SDK，无 shell 介入。

---

## 11. 后续迭代（占位）

### 11.1 其他待办

- `lorag chat` ✅ M7 已实装（`src/main.rs::cmd_chat`）：多轮 + SQLite 持久化历史 + RAG fallback
- `lorag reindex` ✅ M5.1 已实装（`src/main.rs::cmd_reindex`）：删 lancedb + sqlite 后重新 ingest。自动检测 embed_model 变化（MVP 不做，太重；用户手动跑 reindex 即可）。
- 流式输出（aha 支持 SSE）
- Web UI（axum）
- 混合检索（SQLite FTS5 BM25 + 向量 RRF 融合）
- re-rank（aha 原生支持 `Qwen3-Reranker`）
- 按 source 删除 / 重建索引
- 文档结构保留（标题层级、表格、代码块）
- 利用 aha 的 vision/OCR/ASR 能力（图片理解、扫描 PDF 转文本、语音转写）
- 发布到 crates.io 时把 aha 依赖从 path 切到 version
