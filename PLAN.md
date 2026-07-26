# lorag — 规划 (v0.1)

> **状态**：M0–M7.1 全部实装，当前 v0.1 release（MIT，codeberg）。
> 历史细节见 [CHANGELOG.md](CHANGELOG.md)。

---

## 1. 项目目标

完全本地运行的 Agent RAG CLI：

- 摄入多格式文档（pdf / docx / pptx / xlsx / md / txt）入 LanceDB + SQLite
- 一次性 RAG 问答 + 多轮对话 REPL（带历史 + RAG）
- 全部推理走 aha Rust crate，**不**起 HTTP、**不**调云
- 配置切换模型、维度、数据库路径全在 `.env` 里

**明确不做**（除非触发用户需求）：
- 流式输出
- Web UI / HTTP server
- 混合检索（BM25 + 向量）
- 多用户 / 权限
- 工具调用（tool use / function calling）

---

## 2. 技术栈

| 组件 | 选型 | 用途 |
|------|------|------|
| 语言 | Rust 2021 edition | — |
| 推理 | [`aha = { path = "D:/workspace/rust/aha" }`](https://github.com/jhqxxx/aha) | Candle 内核，LLM + embedding 库内调用 |
| 框架 | `rig` **0.40** | agent / completion / embedding 抽象；自定义 aha Provider |
| 向量库 | `lancedb` **0.30** | 手写 native API（绕开 `dynamic_context` 62GB bug） |
| 元数据 | `rusqlite` (bundled) | source / chunk / message 表 |
| 文档解析 | `pdf-extract` / `calamine` / `zip` + `quick-xml` / `pulldown-cmark` | 6 种 loader |
| 异步 | `tokio` (rt-multi-thread) | 包裹同步 candle 推理 |
| CLI | `clap` v4 (derive) | 命令解析 |
| 配置 | `dotenvy` + 手 parse | `.env` → `AppConfig` |
| 日志 | `tracing` + `tracing-subscriber` | silence lance 噪声 |

**关键决策**：

- **aha 走 crate，不起 server**：单进程持有 LLM + embedding 两个 `ModelInstance`，函数调用直传；无端口、无 base_url、无 health check。下载也走 `aha::utils::download_model`，不调 aha CLI 二进制。
- **rig 自定义 provider**：`AhaClient` 实现 `CompletionClient` + `EmbeddingsClient`，**不**实现 `Provider` / `ProviderClient`（0.40 那两个是给 HTTP-based 用的）。详见 `src/rig_compat.rs`。
- **绕开 `dynamic_context`**：rig 0.40 + rig-lancedb 0.40 + lancedb 0.30 集成在某步会一次性分配 ~62GB（实测，5 chunks 也炸）。`src/rag.rs` 改走手写 `table.vector_search()` + `RecordBatch` 流式读。

---

## 3. aha 集成（关键事实）

aha crate 公开 lib API（`aha = "0.2.6"`）：

```rust
use aha::models::load_model;                                  // 通用 safetensors 加载
use aha::models::common::model_mapping::WhichModel;           // 模型 id 枚举
use aha::models::ModelInstance;                               // 加载后实例
use aha::utils::{string_to_static_str, download_model};       // path leak + 下载
use clap::ValueEnum;                                          // WhichModel::from_str(id, true)
```

**核心坑**（避坑必看）：

1. `aha::utils::is_model_downloaded` / `get_default_weight_path` 写死查 `~/.aha/`，**跟 `download_model` 的 `save_dir` 不同步**——aha 自己的 `aha list` 也踩这个坑。**必须**自己写 `resolve_model_path`（在 `src/aha_provider.rs`）：优先 `MODELS_DIR/{repo}/`，兜底 `~/.aha/{repo}/`，"已下"判断 = 目录存在 + `config.json` + 至少一个 `*.safetensors`。
2. `load_model` 第一个参数是 `WhichModel`（**用 `clap::ValueEnum::from_str(id, true)` 解析**），第二个 `path: &str` 必须 `'static`（`string_to_static_str` leak）。同进程可同时持多个 `ModelInstance`（LLM + embedding 各一），互不冲突。
3. LLM 推理 `GenerateModel::generate(mes)` 同步阻塞——**必须** `tokio::task::spawn_blocking` 包。
4. 完整支持模型清单见 aha 官方 [`supported-models.zh-CN.md`](https://github.com/jhqxxx/aha/blob/main/docs/supported-models.zh-CN.md)。LLM：Qwen3-{0.6B,1.7B,4B} / Qwen3.5-{0.8B,2B,4B,9B} / MiniCPM4-0.5B / MiniCPM5-1B / LFM2-1.2B / LFM2.5-1.2B-Instruct；Embedding：MiniLM-L6-V2 (384) / Qwen3-Embedding-{0.6B,4B,8B}；**Reranker**：Qwen3-Reranker-{0.6B,4B,8B}。

参考实现：
- aha 自己的 server 怎么调 lib API：`aha/src/server/api.rs:5-7, 36-56`
- aha 下载 API 签名：`aha/src/utils/mod.rs:498-533`
- aha WhichModel 枚举：`aha/src/models/common/model_mapping.rs`

---

## 4. 架构

### 4.1 高层视图

```
┌──────────────────────────── lorag (单 binary) ────────────────────────────┐
│                                                                            │
│  CLI (clap)                                                                │
│    ingest / query / chat / reindex / sources / models / doctor / init      │
│           │                                                               │
│  ┌────────▼────────┐                                                       │
│  │ 业务模块         │                                                       │
│  │  rag / ingest   │──→ rig 抽象 (CompletionModel / EmbeddingModel)         │
│  │  chunker        │                                                       │
│  └────────┬────────┘                                                       │
│           │                                                               │
│  ┌────────▼────────┐         ┌──────────────────┐                         │
│  │ aha_provider    │────────▶│  aha crate        │                         │
│  │  (唯一 aha 入口) │         │  load_model       │                         │
│  │  + 路径解析      │         │  download_model   │                         │
│  │  + ensure_rerank│         │  rerank / embed   │                         │
│  └─────────────────┘         └──────────────────┘                         │
│           │                                                               │
│  ┌────────▼────────┐                                                       │
│  │ store           │  lancedb (手写 native) + sqlite (rusqlite)            │
│  └─────────────────┘                                                       │
└────────────────────────────────────────────────────────────────────────────┘
```

### 4.2 数据流

**摄入**（`lorag ingest`）：

```
file path
  → ingest::loader::extract(path)         # 6 种格式分派 → 纯文本
  → chunker::split(text)                  # 段落级 + 字符滑窗
  → rag-style embed (AhaEmbeddingModel)   # aha crate 直接 embed
  → lancedb::table.add (native API)       # FixedSizeList<Float64, N>
  → sqlite upsert_source + insert_chunks  # sha256 幂等
  → ensure_hnsw_index (≥256 rows)         # IVF-HNSW-FLAT
```

**问答**（`lorag query` / `lorag chat`）：

```
user question
  → embed question (AhaEmbeddingModel)
  → lancedb::table.vector_search(&[f32])?.limit(max(top_k, rerank_top_n))
  → 流式读 RecordBatch → StringArray::value(i) 抽 text
  → 启用 rerank → rerank_score → 排序取 top_k
  → 拼 context (history + chunks) → preamble
  → AhaCompletionModel::completion(req)
  → 打印答案
```

**回退**：lance 任何错误（目录不存在 / 表不存在 / 内存不够）→ `bare_llm_query` 走裸 LLM（`is_recoverable_error` 关键字匹配）。

---

## 5. 目录结构

```
lorag/
├── Cargo.toml                  # aha path + rig 0.40 + lancedb 0.30 + ...
├── .env.example                # 配置模板
├── .gitignore                  # data/ / .env / tests/fixtures/ / nul
├── README.md                   # 入门 / 快速开始
├── PLAN.md                     # ← 本文件：当前架构 + 决策 + 限制 + 未来
├── CHANGELOG.md                # M0–M7.1 历史 + 关键变更
├── AGENTS.md                   # agent 协作规范（怎么写代码 + 硬规矩）
├── LICENSE                     # MIT
├── src/
│   ├── main.rs                 # CLI 入口（clap 分派）
│   ├── lib.rs                  # 模块声明
│   ├── config.rs               # dotenvy + AppConfig + validate
│   ├── aha_provider.rs         # ★ 唯一 aha 入口：AhaClient + ensure_rerank + 路径解析
│   ├── rig_compat.rs           # AhaCompletionModel + AhaEmbeddingModel（rig 0.40 trait 适配）
│   ├── rag.rs                  # RAG 主流程（手写 lancedb native）+ chat preamble
│   ├── chunker.rs              # 段落 + 字符滑窗切块
│   ├── models.rs               # SourceRecord / Chunk / MessageRecord
│   ├── doctor.rs               # 11 项环境检查
│   ├── ingest/
│   │   ├── loader.rs           # 按扩展名分派
│   │   ├── pdf.rs / docx.rs / pptx.rs / xlsx.rs / md.rs / txt.rs
│   │   └── pipeline.rs         # 摄入主流程
│   └── store/
│       ├── lancedb_store.rs    # 建表 / HNSW 索引
│       └── sqlite_store.rs     # sources / chunks / messages 表
└── tests/                      # cargo test（fixtures/ 已 gitignore）
```

---

## 6. 模块设计

### 6.1 `config.rs`

`AppConfig`（`src/config.rs`）从 `.env` 强类型加载，validate 启动期拦截。**没有端口 / base_url / health 配置**——aha 走 crate。

**字段**：

| 字段 | 含义 | 默认 |
|------|------|------|
| `LLM_MODEL` | LLM 模型 id（aha WhichModel 字符串） | 必填 |
| `EMBED_MODEL` | Embedding 模型 id | 必填 |
| `RERANK_MODEL` | Rerank 模型 id（**留空 = 禁用**） | 空 |
| `RERANK_TOP_N` | 粗筛条数（必须 > TOP_K） | 50 |
| `MODELS_DIR` | 模型下载/加载目录 | `./data/models` |
| `DOWNLOAD_MAX_RETRIES` | `aha::utils::download_model` 重试次数 | 3 |
| `LANCEDB_DIR` | lancedb 数据目录 | `./data/lancedb` |
| `SQLITE_PATH` | sqlite 元数据库路径 | `./data/lorag.db` |
| `CHUNK_SIZE` / `CHUNK_OVERLAP` | 切块参数 | 500 / 50 |
| `TOP_K` | 检索 top_k | 5 |
| `LOG_LEVEL` | tracing 级别 | info |

**EMBED_DIM 不再是配置项**——`AhaClient.embed_dim()` 在 load 模型后从 `config.json::hidden_size` 自动读出，lancedb schema 跟模型走。改 `EMBED_MODEL` 后**必须**清库重建（`lorag reindex`）。

### 6.2 `aha_provider.rs`（★ 唯一 aha 入口）

`AhaClient` 持有 LLM / embedding / rerank 三个 slot：

```rust
pub struct AhaClient {
    llm: Option<Arc<Mutex<ModelInstance<'static>>>>,                          // None if init_embed_only
    embed: Arc<Mutex<ModelInstance<'static>>>,                                // 必有
    rerank_slot: Arc<tokio::sync::OnceCell<Arc<Mutex<ModelInstance<'static>>>>>,  // 懒加载
    embed_dim: Option<usize>,                                                 // 从 config.json::hidden_size 读
    cfg: Arc<AppConfig>,
}
```

- `init(cfg)`：load LLM + embedding + 读 `embed_dim`（被 `init` / `query` / `chat` / `models status --init` 调）
- `init_embed_only(cfg)`：只 load embedding（被 `ingest` / `reindex` 调——省 LLM 的 ~8GB 内存 + 数十秒 load）
- `has_llm()`：区分两种 init 模式
- `ensure_rerank()`：懒加载 rerank 模型（`OnceCell` 内部保证并发只 load 一次）
- `has_rerank()`：rerank 是否已 load（`cfg.rerank_model` 留空 → 永远 false）
- `rerank_score(query, docs)`：调 aha `ModelInstance::rerank`（同步 → `spawn_blocking`）
- `llm_generate(params)` / `embed_texts(texts)`：candle 同步包 `spawn_blocking`

辅助：
- `resolve_model_path(repo, save_dir)`：路径解析（见 §3 坑 1）
- `ensure_model_downloaded(repo, save_dir, max_retries)`：调 `aha::utils::download_model`（幂等）
- `read_hidden_size_from_config(path)`：从 `config.json` 读 `hidden_size`
- `models_status(cfg)` + `print_models_status(...)`：`lorag models status` 用

### 6.3 `rig_compat.rs`

实现 rig 0.40 trait：

- `AhaCompletionModel: CompletionModel`（`stream()` 返 `Err`，`type StreamingResponse = ()`）
- `AhaEmbeddingModel: EmbeddingModel`（`MAX_DOCUMENTS = 1024`，`ndims()` 从 `client.embed_dim()` 读）
- `AhaClient: CompletionClient + EmbeddingsClient`（**不**实现 `Provider`）

消息转换 `convert_messages`：rig `CompletionRequest` → aha `Vec<ChatMessage>`（preamble + documents + chat_history）。

### 6.4 `rag.rs`

RAG 主流程，**不**用 `AgentBuilder::dynamic_context`（62GB bug），手写 lancedb native：

```rust
pub async fn retrieve_chunks(
    client: &AhaClient, cfg: &AppConfig, question: &str,
    top_k: usize, enable_rerank: bool, rerank_top_n: usize,
) -> Result<Vec<String>>;

pub async fn llm_complete(
    client: &AhaClient, cfg: &AppConfig, preamble: String, question: &str,
) -> Result<String>;

pub async fn rag_query(...) -> Result<String>;  // RAG + fallback to bare LLM
pub async fn bare_llm_query(...) -> Result<String>;
pub fn build_chat_preamble(history: &[MessageRecord], chunks: &[String]) -> String;
pub fn is_recoverable_error(err: &str) -> bool;
```

**Rerank 路径**（`cfg.rerank_model` 非空 + `--no-rerank` 未传时启用）：
1. `vector_search` 取 `max(top_k, rerank_top_n)` 条
2. 调 `client.rerank_score(question, &chunks)` 打分
3. 按分数降序排，取前 `top_k` 条
4. 留空时走零开销直 vector_search top_k

**LanceDB schema**（契约，改 = 不向后兼容）：
- `id: Utf8`（`{source_path_hash}:{chunk_ordinal}`）
- `source_path: Utf8`
- `chunk_ordinal: Int64`
- `text: Utf8`
- `embedding: FixedSizeList<Float64, N>`（N 从 `AhaClient.embed_dim()` 来）

**HNSW 索引**：`store::lancedb_store::ensure_hnsw_index` 在 ingest 写完 lancedb 后调；`< 256 rows` 跳过，≥ 256 且没建过则建 IVF-HNSW-FLAT（`IvfHnswFlatIndexBuilder::default()`）。

### 6.5 `ingest/`

- `loader.rs`：按扩展名分派（pdf / docx / pptx / xlsx / md / txt）
- 各 `*::extract(path: &Path) -> Result<String>`：纯文本提取（不知道 LanceDB / SQLite 存在）
- `pipeline.rs::run_ingest`：摄入主流程（loader → chunker → embed → lancedb → sqlite → HNSW）
- 单个文件失败 → warn + skip，不中断整次 ingest

### 6.6 `store/`

- `lancedb_store.rs`：建表 / 写数据 / `ensure_hnsw_index` / vector_search
- `sqlite_store.rs`：
  - `sources` 表（`source_path` UNIQUE + `source_hash` 幂等）
  - `chunks` 表（`(source_id, chunk_ordinal)` UNIQUE）
  - `messages` 表（`session_id` + `ordinal`，多轮聊天用）
  - `append_message` / `load_recent_messages(session, limit)` / `clear_session` / `session_message_count`

对外只暴露具体方法（不暴露 `rusqlite::Connection` / `lancedb::Table`）。

### 6.7 `chunker.rs`

按 `\n\n` 切段（段落级），每段超 `CHUNK_SIZE` 字符按 `CHUNK_SIZE` 滑窗切，重叠 `CHUNK_OVERLAP` 字符。输出 `Vec<Chunk>`。

---

## 7. CLI 命令

```
lorag init                          # 把 LLM + embedding 加载到内存（debug 用；query/chat 隐式调）
lorag models pull                   # 下载 LLM + embedding + rerank（rerank 留空跳过）
lorag models status [--init]        # 看模型文件存在性 + 可选真 load 验证

lorag ingest <PATH>...              # 摄入文件/目录（默认递归）
    --ext pdf,docx,pptx,xlsx,md,txt # 默认全 6 种
    --force                         # 强制重摄入（无视 hash）
    --recursive / --no-recursive    # 默认 recursive

lorag reindex <PATH>...             # 删 LanceDB + SQLite 后重新 ingest
    --ext <list>                    # 同 ingest
    --yes / -y                      # 跳过 interactive 确认
    --dry-run                       # 只打印会做什么

lorag sources list [--json]         # 列出已摄入文件

lorag query <QUESTION>              # 一次性 RAG 问答
    --top-k <N>                     # 覆盖 cfg.top_k
    --no-rerank                     # 跳过 rerank（即使 .env 配了 RERANK_MODEL）
    --rerank-top-n <N>              # 覆盖 cfg.rerank_top_n

lorag chat                          # 多轮 REPL（带 SQLite 历史 + RAG；进程内连续，跨进程不续接）
    --message <TEXT>                # 一次性首问
    --no-history                    # 不带历史（每轮独立）
    --no-banner                     # 安静启动
    --no-rag                        # 纯 LLM 对话
    --no-rerank / --rerank-top-n <N>
    --top-k <N>

lorag doctor                        # 11 项环境检查（env / models / storage / features）
```

错误一律 `anyhow` 打到 stderr，exit 1。`.env` 路径默认当前目录，可由 `LORAG_ENV` 环境变量覆盖。

---

## 8. `.env` 关键配置

完整模板见 [`.env.example`](.env.example)。**必填**：

| 变量 | 必填 | 含义 |
|------|------|------|
| `LLM_MODEL` | ✅ | aha WhichModel 接受的 LLM id（如 `Qwen/Qwen3-4B`） |
| `EMBED_MODEL` | ✅ | aha WhichModel 接受的 embedding id（如 `Qwen/Qwen3-Embedding-0.6B`） |

**可选**（留空 = 禁用）：

| 变量 | 默认 | 含义 |
|------|------|------|
| `RERANK_MODEL` | 空 | 启用时第一次 query 懒加载 |
| `RERANK_TOP_N` | 50 | 粗筛条数，**必须** > `TOP_K` |
| `MODELS_DIR` | `./data/models` | 模型下载/加载目录 |
| `LANCEDB_DIR` | `./data/lancedb` | lancedb 数据目录 |
| `SQLITE_PATH` | `./data/lorag.db` | sqlite 元数据库 |
| `CHUNK_SIZE` / `CHUNK_OVERLAP` | 500 / 50 | 切块参数 |
| `TOP_K` | 5 | 检索 top_k |
| `LOG_LEVEL` | info | tracing filter（默认会 silence lance/lancedb/datafusion/arrow 噪声） |

**换 embedding 模型**（维度变）：改 `EMBED_MODEL` → `lorag models pull` → `lorag reindex <path>`（自动清库重建）。只换 LLM 不动 embedding 时**不用**清库。

---

## 9. 当前限制

1. **单进程内存叠加**：4B LLM (~8GB FP16) + 0.6B Embedding (~1.5GB) + 可选 Rerank (~1.5GB) ≈ 10-12GB RAM。换小模型可降（0.6B LLM ~1.2GB + 0.6B Embedding ~1.5GB ≈ 3GB）。
2. **CUDA 编译陷阱**：`cargo build`（无 flag）会盖掉 CUDA 二进制。改完代码后**必须**用 `cargo build --features cuda` 保住 GPU 加速（CPU 二进制仍能跑，但 4B 会从 1-3s 退化到 15-30s/query）。
3. **PDF 扫描版无效**：`pdf-extract` 只读文本层；扫描版得 OCR（aha 本身有 OCR 模型，未实装）。
4. **xlsx 多 sheet 平铺**：所有 sheet 文本拼一起，不保留表结构 / 公式。
5. **同步摄入**：超大文件（>100MB）可能 OOM；后续可改流式。
6. **rerank hard case 未验证**：generic 14/17 测试 rerank on/off 都 14/17，无质量差异；rerank 价值预期在 hard case（top-5 召回错但 top-50 里有），待真业务问题验证。
7. **Windows 文件锁**：Zed 编辑器打开时 rust-analyzer 会锁 `data/lorag.db`，关闭 Zed 才能 `lorag reindex` 删库。

---

## 10. 关键经验（避坑）

### 10.1 M5 重写：绕开 `dynamic_context` 62GB bug

rig 0.40 + rig-lancedb 0.40 + lancedb 0.30 这条集成链路在 5 chunks 的小数据上会触发 `memory allocation of 62864906528 bytes failed`（OOM 干死进程）。不是数据量问题，是 rig-lancedb 内部某步把整列 / 整 index 一次性读进 `Vec<f32>`。

**改走手写**（`src/rag.rs`）：
```rust
embed_text → table.vector_search(&[f32])?.limit(k).execute()
  → futures::StreamExt::next() 流式读 RecordBatch
  → arrow_array::StringArray::value(i) 抽 text
  → 拼 context → llm_model.completion(req)
```

5 chunks 实测 `iops=2 requests=2 bytes_read=20992`，无 allocation 爆炸。

**arrow-array 58 坑**：`StringArray::value(i)` 返回 `&str`（**不是** `Option<&str>`，是 53 之前某些版本的 API）。

### 10.2 M4 rig 自定义 Provider

- **不实现** `Provider` trait——rig 0.40 的 `Provider` 是给 HTTP-based provider 用的（要 `VERIFY_PATH` / `build_uri` / `with_custom`），in-process 推理不需要
- `CompletionModel::stream` 必须实现（trait 强制），用 `type StreamingResponse = ()`（`()` 已 `GetTokenUsage`），`stream()` 直接 `Err(CompletionError::new(...))`
- `EmbeddingModel::make(client, model, dims: Option<usize>) -> Self`（0.40 比 0.39 多了 `dims` 参数）
- `OneOrMany` 在 0.40 是 `struct { single | Vec }`，配套 `first()` / `iter()` / `one()` / `many()` 方法（**不是** enum 模式匹配）
- 跨 await 持有的类型都得 `Send + Sync`（rig 用 `WasmCompatSend` trait bound，native 编译时等价 `Send`）

### 10.3 aha 路径

- `load_model` 的 `path` 参数必须 `&'static str` → `aha::utils::string_to_static_str(path)` leak（每次启动 ~100 字节，可接受）
- **不要**在每次 `agent.prompt()` 时重新 load 模型——load 数 GB 模型要数十秒
- 路径解析用自己写的 `resolve_model_path`，**不要**用 `aha::utils::is_model_downloaded`（它写死查 `~/.aha/`，跟 `download_model` 的 `save_dir` 不同步）

### 10.4 LanceDB schema 变更

改 schema = 不向后兼容。**必须**先在 `src/store/lancedb_store.rs` 写明，然后清 `data/lancedb/` + `data/lorag.db[-wal/-shm/-journal]`（用 `lorag reindex`，不要手动 rm）。

### 10.5 Cargo profile

dev profile `opt-level = 1` 跑 0.6B 实测 4.5s/query（vs full debug 142s），足够日常迭代。**不要**用 `cargo build --release` 日常循环——首次 link 5-10 分钟把 D 盘打 100%（lance + datafusion + rig + reqwest 全 link）。incremental=true 让 release 重 build 变 ~30s。

### 10.6 日志过滤

`tracing_subscriber::EnvFilter` 的 target 段是**字面量**（不是 glob），`lance=warn` 不会匹配 `lance::dataset_events`，必须显式列全。`.env` 写 `LOG_LEVEL=info` 会**整体**当 filter 字符串用，丢我们的 lance silencing 后缀——所以 `lance_silence` 写成**必加后缀**，不管 base 是什么都 `format!("{base}{silence}")` 拼上。

---

## 11. 未来方向

按优先级：

1. **MCP server**（中-高）：把 `lorag query` / `lorag ingest` / `lorag sources list` 暴露成 MCP tools，让 Claude Desktop / Cursor / IDE agent 直接调。生态价值高，工作量 ~500-1000 行 + 测试。**触发条件**：有外部用户 / IDE 集成需求。
2. **rerank hard case 验证**：当前 generic 14/17 测试 rerank on/off 都 14/17，没差异；需要真业务问题（top-5 召回错的）验证 rerank 实际价值。
3. **流式输出**（aha 支持 SSE）：等用户需求触发。
4. **混合检索**（SQLite FTS5 BM25 + 向量 RRF 融合）：等 chunk 量到 5K+ 触发。
5. **多知识库**（多 lancedb 目录 + namespace）：schema 迁移成本高，**推迟**到 ≥3 个 domain / 每个 domain ≥1000 chunks 才做。
6. **Tool calling / function calling**：aha 自身 server 是否实现 tool use 待确认；rig 0.40 支持 `CompletionRequest.tools: Vec<ToolDefinition>`。**优先级低**——个人项目 MVP 用不上。
7. **Web UI（axum）**：等有 web 集成需求触发。

---

## 12. 参考资料

- aha 文档：https://github.com/jhqxxx/aha
- aha 源码（本地）：`D:/workspace/rust/aha/`
- aha lib 用法（参考 server 实现）：`aha/src/server/api.rs:5-7, 36-56`
- aha 下载 API：`aha/src/utils/mod.rs:498-533`
- Rig 文档：https://rig.rs/docs
- Rig 自定义 Provider：https://rig.rs/docs/guides/extension/write_your_own_provider
- Rig 自定义 Provider 示例：https://github.com/joshua-mo-143/rig-custom-provider-example
- LanceDB：https://lancedb.github.io/lancedb/
