# lorag — Agent 协作规范

> 写给在 `lorag` 仓库里工作的 agent（包括我、未来的我、和接手的 agent）看的工作约定。
> 项目范围、模块设计、命令、配置见 `PLAN.md`；本文件只规定**怎么写代码、怎么做事**。

---

## 1. 项目速读（一分钟版本）

- **目标**：本地 Agent RAG CLI。ingest 多种文档入 LanceDB + SQLite，query 一次性 RAG 问答。
- **栈**：Rust 2021 + `aha`（**path 依赖**）+ `rig` **0.40** + `rig-lancedb` 0.40 + `lancedb` 0.30 + `rusqlite` + `clap` v4。
- **当前状态**：M0 / M1 / M2 / M3 / M4 / M5 已实装（CLI + aha 加载 + 6 种 loader + chunker + sqlite + lancedb + ingest pipeline + rig provider + RAG 端到端）。M6 (smoke test) / `lorag doctor` / README 待办。
- **LLM/embedding 推理**：走 aha **crate**（不起 HTTP server），通过实现 rig 0.40 的 `CompletionClient` / `EmbeddingsClient` 把 aha 装进 rig（**不**实现 `Provider` trait，0.40 的 `Provider` 是给 HTTP-based provider 用的）。
- **模型下载**：也走 aha **crate**（`aha::utils::download_model`），不调 aha CLI 二进制。
- **MVP 不做**：多轮 chat REPL、流式、Web UI、混合检索、re-rank。
- 跑通 MVP 的端到端命令：`lorag models pull && lorag ingest <path> && lorag query "..."`。
- 当前已能跑：`lorag init`（load 模型）+ `lorag ingest`（6 种格式）+ `lorag query` / `lorag shell`（**RAG 端到端，绕开 dynamic_context 62GB 内存 bug**）。
- **`lorag shell` 是临时方案** —— M7 实装真多轮后会被 `lorag chat` 替代（详见 PLAN.md §11.1）。M5 阶段 `shell` 是占位 REPL，`chat` 是 stub。

读 `PLAN.md` 整个文件后再动代码。

---

## 2. 通用约定

### 2.1 风格

- **`cargo fmt`** 通过 + **`cargo clippy --all-targets -- -D warnings`** 通过才能算完成。
- 命名遵循 Rust 标准库习惯；模块用 `snake_case`，类型用 `PascalCase`，错误变体用 `PascalCase`。
- 公开 API 必须有 `///` 文档注释，包含**至少一个**可运行的例子（如果可以）。
- 不引入 `unsafe`，除非有明确性能原因 + 注释解释。

### 2.2 错误处理

- 公开函数返回 `anyhow::Result<T>`，错误信息**面向人**（含上下文路径 / 模型 id / 维度等）。
- 内部模块可以自定义 `thiserror` 枚举，但**只在该模块内部**用，对外统一 `From` 转到 `anyhow::Error`。
- 不要用 `unwrap()` / `expect()`，除了：
  - 测试代码
  - 启动期强校验（配置 / 模型路径），失败意味着没法运行，应该 panic 并打印
- 用户可见的错误打印到 stderr，**不要** panic 让进程崩；exit code 1。

### 2.3 异步

- 用 `#[tokio::main]` 入口（macros + rt-multi-thread feature）。
- **aha 的 candle 推理是同步阻塞**——`AhaCompletionModel::completion` 必须用 `tokio::task::spawn_blocking` 包，不能直接在 async 上下文里调 `model.generate()`，否则会卡死 reactor。
- 阻塞 IO（`std::fs`）可以放 `spawn_blocking` 或 async 等价 crate；MVP 阶段简单起见用 `tokio::fs`。

### 2.4 配置

- **配置单一来源**：`.env`（由 `dotenvy` 加载）+ 强类型 `AppConfig`（`config.rs`）。
- **永远不要** `std::env::var("...")` 在业务代码里散落读环境变量，全部走 `AppConfig`。
- 新增配置项时：
  1. 加到 `config.rs` 的 `AppConfig` 字段
  2. 加到 `.env.example` 并写注释
  3. 在 `PLAN.md` §6.1 同步
- 配置缺失或非法时 fail-fast，**不要**给"看起来合理"的默认值去掩盖错误。
- 本项目**没有端口/base_url/health 配置**——aha 用 crate 调用，HTTP 概念不存在。

### 2.5 日志

- 用 `tracing`，不要 `println!` 当日志。
- 入口 `main` 顶部 `tracing_subscriber::fmt()` + 自定义 `EnvFilter`：
  - 默认 `info`，加上 `lance::*` / `lancedb` / `datafusion` / `arrow` 的 `=warn` 后缀（必加，silence 它们的 INFO 噪声）
  - `RUST_LOG` 优先；`LOG_LEVEL` 旧变量保留兼容
  - 排查 lance 内部时设 `RUST_LOG=info` 或 `RUST_LOG=lance::execution=debug` 即可
- 用户面向的输出（ingest 进度、query 答案、下载进度）走 stdout 普通 `println!`，**不**走 tracing。
  - 唯一例外：`aha::utils::download_model` 内部自带 `println!` 输出下载进度，外部不接管。
- **踩过的坑**：`env_filter` 的 target 段是字面量（不是 glob），`lance=warn` 不会匹配 `lance::dataset_events`，必须显式 `lance::dataset_events=warn` 列出。
- **踩过的坑 2**：`.env` 里的 `LOG_LEVEL=info` 会**整体**当成 filter 字符串用，丢失我们的 lance silencing 后缀——所以 `lance_silence` 必须**format! 拼上**而不是直接用 base 字符串。

### 2.6 Cargo Profile（dev vs release）

```toml
# Cargo.toml
[profile.dev]
opt-level = 1    # 0.6B 实测 4.5s/query（vs full debug 142s）
debug = true

[profile.release]
opt-level = 3
lto = "thin"
codegen-units = 16   # 多核 link；6m33s cold → 30s incremental
strip = "symbols"
incremental = true
```

**开发约定**：
- 日常迭代用 `cargo build`（dev profile），**不**用 `cargo build --release`。
- dev + opt-level=1 0.6B 推理 ~4.5s/query（release ~1.14s），够用。
- release 链接会 5-10 分钟把 D 盘打 100%（lance + datafusion + rig + reqwest 全 link），**只在测性能时跑一次**。
- incremental=true 让 release 重 build 变 ~30s。

---

## 3. 模块边界

```
config ──┬──→ aha_provider ─────┐    （aha lib 适配 + 模型下载/加载 + async helper —— M0/M1 实装）
         ├──→ rig_compat ───────┤    （rig 0.40 provider trait 适配 —— M4 实装）
         ├──→ rag ──────────────┤    （手写 embed + lancedb native + 拼 context + completion —— M5 实装，**绕过 dynamic_context**）
         ├──→ chunker ──────────┤    （M3 实装）
         ├──→ ingest ───────────┤    （M2 loaders + M3 pipeline 已实装）
         ├──→ store ────────────┘    （M3 lancedb + sqlite 已实装）
         └──→ main (CLI)
```

- **`config` 不依赖**任何业务模块；其他模块都依赖 `config`。
- **`aha_provider` 是 aha ↔ rig 适配 + 模型生命周期的唯一入口**：
  - `AhaClient` 持有 `Arc<tokio::sync::Mutex<ModelInstance<'static>>>` × 2（LLM / embedding 各自独立锁）
  - `AhaClient::init(...)` 调 `aha::models::load_model`（被 `lorag init` / `lorag models status --init` / `lorag query` / `lorag shell` 调用）
  - `ensure_model_downloaded(...)` 调 `aha::utils::download_model`（被 `lorag models pull` 调用）
  - `resolve_model_path(...)` 查 `MODELS_DIR/{repo}/` 优先 + `~/.aha/{repo}/` 兜底（**不**用 aha 的 `is_model_downloaded`，因为它写死查 `~/.aha/`，跟 `download_model` 的 `save_dir` 路径不同步——详见 §6 禁止事项）
  - async helper `llm_generate` / `embed_texts` 把同步 candle 调用包成 `tokio::task::spawn_blocking`
- **`rig_compat` 是 rig 0.40 的 provider trait 适配**（M4 实装）：
  - `AhaCompletionModel` 实现 `CompletionModel`（`stream()` 留 `Err`，`type StreamingResponse = ()`）
  - `AhaEmbeddingModel` 实现 `EmbeddingModel`（`MAX_DOCUMENTS = 1024`，`ndims()` 从 `cfg.embed_dim` 来）
  - `AhaClient` 实现 `CompletionClient` + `EmbeddingsClient`（**不**实现 `Provider` / `ProviderClient`，0.40 的版本是 HTTP-based 的）
  - 消息转换 `convert_messages` 把 rig `Message` → aha `ChatMessage`（preamble + documents + chat_history）
- **`rag` 模块是 M5 实装的关键**（**绕开 dynamic_context**）：
  - **不要**用 `AgentBuilder::dynamic_context` + `LanceDbVectorIndex` —— 5 chunks 也会爆 62GB 内存（实测，详见 PLAN.md §6.5.1）
  - `rag_query` 主流程：`embed_text` → `lancedb::connect().open_table("documents").vector_search(&[f32]).limit(k).execute()` → 流式读 `RecordBatch` + `StringArray::value(i)` 抽 text → 拼 context → `llm_model.completion(req)`
  - `bare_llm_query`：直发 prompt，不检索（被 `lorag shell --no-rag` 和 fallback 用）
  - `is_recoverable_error`：lancedb 任何错误（不存在 / 没数据 / 内存不够）→ fallback 到 bare LLM，不让用户卡住
  - 上层模块（cmd_query / cmd_shell）一律走 `rag_query` / `bare_llm_query`，不直接调 lancedb / rig
- **`store::lancedb_store` 还管 IVF-HNSW 索引**：
  - `ensure_hnsw_index(table)` 在 `ingest_one` 写完 lancedb 之后调
  - lancedb 0.30 HNSW 走 IVF-HNSW-FLAT（`IvfHnswFlatIndexBuilder::default()`），用 `table.create_index(&["embedding"], Index::IvfHnswFlat(...))`
  - lance 的 IVF kmeans 训练要求 **≥ 256 行**；< 256 silently skip，≥ 256 且没建过才建
  - 查询时**不**需要额外传参——lancedb 检测到有索引自动走 ANN
- **`store` 模块**对外只暴露 `trait`（如 `trait MetadataStore`），不暴露 `rusqlite::Connection` / `lancedb::Table`。
- **`ingest::loader` 各子模块**只负责"文件 → 纯文本"，不知道 LanceDB / SQLite 的存在。
- **本项目没有 `aha_runner` 模块**——所有 aha 交互（推理 + 下载）都在 `aha_provider` 内完成。
- **上层模块（rag / ingest）一律走 rig 抽象**，**不**直接 `use aha::*`（这条是 §6 禁止事项里的硬规矩）。

新加模块时，先在 `PLAN.md` §5 标位置，然后才写代码。

---

## 4. 关键实现约定

### 4.1 摄入幂等

- `lorag ingest <path>` 默认**不重摄入**已存在且 hash 一致的文件。
- 重复时打印 `skipped: <path> (unchanged)`，**不算错误**。
- `--force` 时无视 hash，重写 chunks（先 delete 后 insert 同 source_id）。
- hash 算法：sha256，hex 编码存到 `sources.source_hash`。

### 4.2 LanceDB 表 schema 是**契约**

- schema 见 `PLAN.md` §6.6。改 schema 等于不向后兼容。
- 改 schema 时必须**先**在 `PLAN.md` 写明，然后**清空** `data/lancedb/`。
- `EMBED_DIM` 改了 = 重建表 = `rm -rf data/lancedb`。

### 4.3 aha 模型路径与生命周期

- aha 的 `load_model(which, model_path, None, None)` 返回 `ModelInstance<'static>`。
- `model_path` 必须是 `&'static str` → 用 `aha::utils::string_to_static_str(path)` leak。
- 启动时构造一次（`AhaClient::init`），用 `Arc<RwLock<AhaModelSet>>` 共享给两个 model。
- **不要在每次 `agent.prompt()` 时重新 load 模型**——load 很慢（数 GB 模型要数十秒）。
- 模型本地路径 = `{MODELS_DIR}/{MODEL_REPO}/`，`aha::utils::download_model` 把模型下到这个位置。
- 模型 id 解析：`WhichModel::from_str(model_id, true)`（它实现了 `clap::ValueEnum`）。失败说明用户写错 id。

### 4.4 错误信息模板

- 错误信息包含三段：**[动作] + [对象] + [原因/建议]**。
- 例：
  - `failed to load pdf: fixtures/sample.pdf: file not found`
  - `failed to init AhaClient with LLM=Qwen/Qwen3-4B: model not found at ./data/models/Qwen/Qwen3-4B (run: lorag models pull)`
  - `config: EMBED_DIM=768 but model returned 1024-dim; fix EMBED_DIM in .env or rebuild lancedb`
  - `failed to embed 12 chunks: aha returned embedding of dim 0 (check aha logs; is all-MiniLM-L6-v2 correctly loaded?)`
  - `failed to download model Qwen3-X: model id not recognized by aha (see aha::models::common::model_mapping)`
- 错误提到用户能采取的行动（`run: ...` / `fix ...` / `check ...`），不要只说"出错了"。

### 4.5 测试

- 单元测试放在同文件 `#[cfg(test)] mod tests`，覆盖核心算法（chunker、id 生成、消息转换、WhichModel 解析）。
- 集成测试放 `tests/`，每个测试建独立临时目录（`tempfile` crate）。
- **aha lib 集成测试**：可以正常 `cargo test`（不需要网络或 server）。`AhaClient::init` 需要模型已下载到本地路径；CI 跳过或 stub。
- **下载测试**：用小模型 + 临时目录验证 `ensure_model_downloaded` 端到端；CI 可 skip（耗时长）。
- 跑测试前 `git clean` 数据目录，避免历史 sqlite / lancedb 干扰。

---

## 5. 常见任务工作流

### 5.1 加新的 loader（接一种新文件类型）

1. 在 `src/ingest/` 加 `myformat.rs`，实现 `pub fn extract(path: &Path) -> Result<String>`。
2. 在 `src/ingest/loader.rs` 的分派表里加一行 `Some("ext") => myformat::extract(path)`。
3. 在 `src/ingest/mod.rs` 加 `pub mod myformat;`。
4. 在 `PLAN.md` §6.9 的表格加一行。
5. 在 `tests/` 加一个最小 fixture。
6. 更新 `.env.example` 的 `lorag ingest --help` 描述（如适用）。

### 5.2 加新的命令

1. 在 `src/main.rs` 的 `Cli`（clap derive）加新 variant。
2. 实现函数放 `src/<module>.rs` 的 `pub async fn cmd_xxx(...) -> anyhow::Result<()>`。
3. `main` 里 `match cli.command { ... }` 分派。
4. 在 `PLAN.md` §7 加一行。

### 5.3 改配置 schema

1. 改 `AppConfig`，加 `#[serde(default = "...")]` 给旧 `.env` 兼容期。
2. 改 `.env.example`。
3. 改 `PLAN.md` §6.1。

### 5.4 rig AhaProvider 实现细节（关键任务，✅ M4 已实装）

- **严格按 rig 0.40 文档实现**（[write your own provider](https://rig.rs/docs/guides/extension/write_your_own_provider)），不要自己造 trait。
- **不实现 `Provider` trait**——rig 0.40 的 `Provider` 是给 HTTP-based provider 用的（要 `VERIFY_PATH` / `build_uri` / `with_custom`），in-process 推理不需要。
- `CompletionModel::stream` MVP 阶段**必须实现**（trait 强制），用 `unimplemented!()` 或返回 `Err(CompletionError::new("streaming not supported in MVP"))`。
- `type StreamingResponse = ()` 最省事（`()` 已实现 `GetTokenUsage`），`stream()` 直接返 `Err`。
- `EmbeddingModel::MAX_DOCUMENTS` = 1024（aha 实际限制；超过要分批）。
- `EmbeddingModel::ndims()` 必实现——用 `cfg.embed_dim`。
- `EmbeddingModel::make(client, model, dims: Option<usize>) -> Self`（0.40 比 0.39 多了 `dims` 参数）。
- `CompletionModel::make(client, model) -> Self` 是 associated function（0.40 跟 0.39 API 不同）。
- **`OneOrMany` 在 0.40 是 `struct { single | Vec }`，配套 `first()` / `iter()` / `one()` / `many()` 方法**，**不是** enum 模式匹配。
- `CompletionResponse` 0.40 多 `raw_response: T` / `usage: Usage` / `message_id: Option<String>` 字段。
- 跨 await 持有的类型都得 `Send + Sync`（rig 用 `WasmCompatSend` trait bound，native 编译时等价 `Send`）。`_assert_send_sync` 编译期断言在 `rig_compat.rs` 顶部。
- **aha 的 `load_model` 第一次非常慢**（数 GB 模型 → 数十秒到几分钟），放在 `spawn_blocking` 里且要设长 timeout。
- **aha 的 `WhichModel` 解析** 用 `clap::ValueEnum::from_str(id, true)`；记得把 `clap::ValueEnum` 引入 use。
- 消息转换规则（`convert_messages`）：
  - `preamble: Option<String>` → 第一条 aha `System` 消息
  - `documents: Vec<Document>` → aha `System` 消息（"[id] text\n\n..." 拼一个块，插在 user 消息前）
  - `chat_history: OneOrMany<Message>` → 逐条翻译：System / User / Assistant 抽 text 后映射到 aha 对应变体
- aha response → rig `AssistantContent::Text(Text { text, additional_params: None })`（抽第一条 choice 的 `ChatMessage::Assistant.content` 的 text）
- 参考实现：
  - 官方 custom provider 示例 crate：https://github.com/joshua-mo-143/rig-custom-provider-example
  - aha 自己 server 用 lib API 的方式：`D:/workspace/rust/aha/src/server/api.rs:36-56`
  - aha 下载 API：`D:/workspace/rust/aha/src/utils/mod.rs:498-533`
  - rig 0.40 实际源码（在 cargo 缓存里）：`D:/devtools/rust/.cargo/registry/src/.../rig-core-0.40.0/src/`
    - `client/completion.rs` / `client/embeddings.rs`
    - `completion/request.rs`（`CompletionModel` / `CompletionRequest` / `CompletionResponse` / `Document`）
    - `completion/message.rs`（`Message` / `Text` / `UserContent` / `AssistantContent`）
    - `embeddings/embedding.rs`（`EmbeddingModel` / `Embedding` / `EmbeddingError`）
    - `streaming.rs`（`StreamingCompletionResponse`）
    - `one_or_many.rs`（**0.40 是 struct**，不是 enum）
    - `wasm_compat.rs`（`WasmCompatSend` trait）

### 5.5 升级依赖

- 升级 `aha` / `rig` / `rig-lancedb` / `lancedb` 前先看 CHANGELOG。
- aha 升级要重新校验 `WhichModel` 枚举值、模型路径接口、消息格式、`download_model` 签名。
- rig 升级（0.40 → 0.41+）通常 breaking，**重点看** `client/completion.rs` + `client/embeddings.rs` + `completion/request.rs` 的 `OneOrMany` 是否还是 struct、`CompletionModel::make` 签名有没有变。
- rig-lancedb / lancedb 升级要重新校验 `LanceDbVectorIndex::new` 签名和 `SearchParams` 默认值（**M5 已用，但绕开**——见 `rag` 模块说明）。
- **M5 重要**：升级时务必重新跑 RAG 端到端（`lorag query`）验证 62GB 内存 bug 没复发。

---

## 6. 禁止事项

- **不要**在源码里硬编码任何路径、端口、模型名、API key。配置从 `.env` 走。
- **不要**让业务模块（rag / ingest）直接 `use aha::*`——必须经 `aha_provider` 的 rig 抽象。
- **不要**在 async 上下文里直接调 `model.generate()`（candle 同步阻塞）——必须 `spawn_blocking`。
- **不要**在每次 `agent.prompt()` 时重新 load 模型（慢）。
- **不要**写 `unsafe` 代码。
- **不要**在 `Cargo.toml` 加新依赖前不更新本文件 §7。
- **不要**在 `main.rs` 写业务逻辑，只做 CLI 解析 + 分派。
- **不要**为绕过错误处理而 `unwrap()` / `expect()`。
- **不要**提交 `data/` 下的运行时数据到 git。
- **不要**调任何 aha CLI 二进制（`aha download` / `aha serv` / `aha cli`）——本项目只走 aha crate 库 API。
- **不要**用 `aha::utils::is_model_downloaded` / `get_default_weight_path`——它们写死查 `~/.aha/`，跟 `download_model` 的 `save_dir` 路径不同步（aha 自己的 `aha list` 也踩这个坑）。必须用 `lorag::aha_provider::resolve_model_path` 做"已下"判断和路径解析，它会同时查 `MODELS_DIR` 和 `~/.aha/`。

---

## 7. 依赖备忘

| 依赖 | 用途 | 备注 |
|------|------|------|
| `aha = { path = "D:/workspace/rust/aha" }` | **核心**：aha 推理 + 模型下载 crate | 本地 path 依赖；发布时改 `"0.2.6"` |
| `rig` **0.40** | ✅ M4 已用：LLM / agent / embedding 框架 | `default-features = false`（不用 reqwest/derive/rustls）；锁 0.40.x |
| `lancedb` 0.30 | ✅ M5 已用：lancedb 核心（**手写 native API**） | 走 `vector_search().limit(k).execute()` + `RecordBatch` 流；不依赖 rig-lancedb |
| `arrow-array` 58 + `arrow-schema` 58 | ✅ M5 已用：RecordBatch / StringArray | lancedb 0.30 拉入；`StringArray::value(i)` 返回 `&str` |
| `futures` 0.3 | ✅ M5 已用：`StreamExt::next()` | 处理 lancedb RecordBatch stream |
| `tokio` (rt-multi-thread + macros + fs + process + sync) | runtime | **必须**用来 wrap candle 同步调用 |
| `clap` v4 (derive) | CLI | 同时 `WhichModel::from_str` 也需要 `clap::ValueEnum` |
| `dotenvy` | 加载 .env | |
| `serde` + `serde_json` | 配置 / 消息转换 | |
| `anyhow` + `thiserror` | 错误 | |
| `tracing` + `tracing-subscriber` | 日志 | |
| `dirs = "6"` | ✅ M0 已用：拿 `~/.aha/` 兼容 aha CLI 老用户 | 跟 aha 自己依赖的 `dirs` 同版本 |
| `pdf-extract` | ✅ M2 已用：pdf 解析 | |
| `calamine` | ✅ M2 已用：xlsx 解析 | |
| `zip` + `quick-xml` | ✅ M2 已用：pptx / docx 解析 | |
| `pulldown-cmark` | ✅ M2 已用：md 解析 | |
| `rusqlite` (bundled) | ✅ M3 已用：sqlite 元数据 | bundled feature 避免系统依赖 |
| `sha2` + `hex` + `chrono` | ✅ M3 已用：源文件 hash + 时间戳 | |
| `tempfile` | 测试 | dev-dep |

加新依赖前**先**在这里登记。

### 7.1 当前依赖的实装/未实装清单

- ✅ `aha` / `tokio` / `clap` / `dotenvy` / `serde` / `serde_json` / `anyhow` / `thiserror` / `tracing` / `tracing-subscriber` / `dirs` / `tempfile` —— 实际在用
- ✅ `rig = "0.40"` —— M4 实装，已放开（在 `Cargo.toml` 里写明）
- ✅ `pdf-extract` / `calamine` / `zip` / `quick-xml` / `pulldown-cmark` —— M2 实装，已放开
- ✅ `lancedb` / `rusqlite` / `sha2` / `hex` / `chrono` / `arrow-array` / `arrow-schema` / `futures` —— M3+M5 实装，已放开
- ✅ `arrow-array` 58 + `arrow-schema` 58 —— lancedb 0.30 拉入（M5 实装）
- ⏸ `rig-lancedb` 0.40 —— 已在 `Cargo.toml` 但**不直接使用**（绕开 dynamic_context 62GB bug；保留依赖用于未来或备选）

---

## 8. 参考资料

- Rig 文档：https://rig.rs/docs
- Rig LanceDB 集成：https://rig.rs/docs/integrations/vector_stores/lancedb
- Rig Embeddings：https://rig.rs/docs/concepts/embeddings
- Rig Loaders：https://rig.rs/docs/concepts/loaders
- Rig Agent：https://rig.rs/docs/concepts/agent
- **Rig 自定义 Provider**：https://rig.rs/docs/guides/extension/write_your_own_provider
- **Rig 自定义 Provider 示例 crate**：https://github.com/joshua-mo-143/rig-custom-provider-example
- aha GitHub：https://github.com/jhqxxx/aha
- aha 源码（本地）：`D:/workspace/rust/aha/`
- aha lib 用法（参考 server 实现）：`D:/workspace/rust/aha/src/server/api.rs:5-7, 36-56`
- aha 模型下载 API（`aha::utils::download_model`）：`D:/workspace/rust/aha/src/utils/mod.rs:498-533`
- aha 模型存在性检测（`aha::utils::is_model_downloaded`）：`D:/workspace/rust/aha/src/utils/mod.rs:650-661`
- aha WhichModel 枚举：`D:/workspace/rust/aha/src/models/common/model_mapping.rs`
- LanceDB：https://lancedb.github.io/lancedb/

---

## 9. 与 Mavis 协作的约定

- 接到需求时**先**读 `PLAN.md` + 本文件，再动代码。
- 改 PLAN.md / AGENTS.md 时在同一 commit 里改，别拆。
- 写完一段代码后跑 `cargo fmt && cargo clippy --all-targets -- -D warnings && cargo test`，全过才算完。
- 报错优先自查 `AGENTS.md` 的"禁止事项"和"常见任务工作流"，不要重复问相同问题。
- 用户没说要 chat / web UI / streaming 时**不要**自作主张加。
- 本项目只调 aha crate 库 API，**不**调 aha CLI 二进制（任何要 spawn `aha ...` 子进程的方案都是错的）。
