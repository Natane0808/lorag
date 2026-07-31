# 架构 / Architecture

> lorag 单二进制同时承载 CLI / Web UI / Tray / GPUI 桌面 GUI 4 个前端，
> 共享同一份数据。本页讲模块边界 + 数据流 + aha 集成 + 存储 schema。

---

## 单 binary 4 前端

```
┌──────────────────────────── lorag (单 binary) ────────────────────────────┐
│                                                                            │
│  CLI (clap)                                                                │
│    ingest / query / chat / reindex / sources / models / doctor / init      │
│           │                                                               │
│  ┌────────▼────────┐         ┌──────────────────┐                         │
│  │ 业务模块         │────────▶│  aha crate        │                         │
│  │  rag / ingest   │  rig    │  (in-process      │                         │
│  │  chunker        │  抽象   │   inference)      │                         │
│  └────────┬────────┘         │  load_model       │                         │
│           │                  │  download_model   │                         │
│           │                  │  rerank / embed   │                         │
│           │                  └──────────────────┘                         │
│           │                                                               │
│  ┌────────▼────────┐         ┌──────────────────┐                         │
│  │ store           │         │ Web UI / Tray /   │                         │
│  │ lancedb + sqlite│         │ Desktop GUI       │                         │
│  └─────────────────┘         │ (共享 axum)       │                         │
│                              └──────────────────┘                         │
└────────────────────────────────────────────────────────────────────────────┘
```

**关键点**：
- 4 个前端**共享同一份数据**（`./data/`），任意切换不会丢上下文
- aha 推理在**进程内**完成，无 HTTP、无云
- Web UI 用 axum（仅前端需要 HTTP），CLI / GUI / Tray 都不需要 HTTP server

---

## 模块边界

```
config ──┬──→ aha_provider ──────┐    （★ 唯一 aha 入口 + 模型生命周期）
         ├──→ rig_compat ────────┤    （rig 0.40 provider trait 适配）
         ├──→ rag ──────────────┘    （手写 embed + lancedb native + FTS5 hybrid + RRF + rerank；**绕过 dynamic_context**）
         ├──→ chunker ──────────┤
         ├──→ ingest ───────────┘    （6 种 loader + pipeline）
         ├──→ store ─────────────┐   （lancedb + sqlite；store::lancedb_store 还管 HNSW 索引）
         ├──→ server ────────────┤   （axum HTTP server + 嵌入式前端，SSE 流式 API）
         ├──→ tray ──────────────┤   （系统托盘：tray-icon 事件循环 + open_browser）
         ├──→ gui ───────────────┘   （GPUI 桌面启动器：7 页 sidebar + 托盘）
         └──→ main (CLI)
```

**关键约束**：

- **`config` 不依赖**任何业务模块；其他模块都依赖 `config`
- **`aha_provider` 是 aha ↔ rig 适配 + 模型生命周期的唯一入口**——业务模块（rag / ingest）**不**直接 `use aha::*`
- **`store`**：对外只暴露具体方法，不暴露 `rusqlite::Connection` / `lancedb::Table`
- **`ingest::loader` 各子模块**只负责"文件 → 纯文本"，不知道 LanceDB / SQLite 存在

---

## 数据流

### 摄入（`lorag ingest`）

```
file path
  → ingest::loader::extract(path)         # 6 种格式分派 → 纯文本
  → chunker::split(text)                  # 段落级 + 字符滑窗
  → rag-style embed (AhaEmbeddingModel)   # aha crate 直接 embed
  → lancedb::table.add (native API)       # FixedSizeList<Float64, N>
  → sqlite upsert_source + insert_chunks  # sha256 幂等
  → ensure_hnsw_index (≥256 rows)         # IVF-HNSW-FLAT
```

### 问答（`lorag query` / `lorag chat`）

```
user question
  → embed question (AhaEmbeddingModel)
  → lancedb::table.vector_search(&[f32])?.limit(max(top_k, rerank_top_n))
  → 流式读 RecordBatch → StringArray::value(i) 抽 text
  → 启用 rerank → rerank_score → 排序取 top_k
  → 拼 context (history + chunks) → preamble
  → AhaCompletionModel::completion(req)
  → 打印答案（流式 token 输出，起）
```

**回退路径**：LanceDB 任何错误（目录不存在 / 表不存在 / 内存不够）→ `bare_llm_query` 走裸 LLM（`is_recoverable_error` 关键字匹配）。这是有意的，让 LLM 在检索挂的时候还能答。

---

## aha 集成（关键事实）

aha 是本项目的推理核心 —— LLM / Embedding / Rerank 都在一个 Rust crate 里完成：

```rust
use aha::models::load_model;                                  // 通用 safetensors 加载
use aha::models::common::model_mapping::WhichModel;           // 模型 id 枚举
use aha::models::ModelInstance;                               // 加载后实例
use aha::utils::{string_to_static_str, download_model};       // path leak + 下载
use clap::ValueEnum;                                          // WhichModel::from_str(id, true)
```

**坑 1：路径解析**

⚠️ **不要用 `aha::utils::is_model_downloaded` / `get_default_weight_path`**——它们写死查 `~/.aha/`，**跟 `download_model` 的 `save_dir` 不同步**（aha 自己的 `aha list` 也踩这个坑）。

必须自己写 `resolve_model_path`（在 `src/aha_provider.rs`）：
- 优先 `MODELS_DIR/{repo}/`
- 兜底 `~/.aha/{repo}/`
- "已下"判断 = 目录存在 + `config.json` + 至少一个 `*.safetensors`

**坑 2：`path: &str` 必须 `'static`**

`load_model` 第二个参数是 `&'static str` → 用 `aha::utils::string_to_static_str(path)` leak（每次启动 ~100 字节，可接受）。

**坑 3：candle 同步阻塞**

LLM 推理 `GenerateModel::generate(mes)` 同步阻塞 —— **必须** `tokio::task::spawn_blocking` 包，否则会卡死 tokio reactor。

**坑 4：流式 channel bridge**

`generate_stream` 返回的 stream 生命周期绑 `&mut self`，不能从 `spawn_blocking` 闭包 return。解法走 `mpsc::channel(64)` 桥接：

```rust
let (tx, rx) = mpsc::channel(64);
spawn_blocking(move || {
    let mut g = llm.blocking_lock;
    let mut stream = g.generate_stream(params)?;
    rt.block_on(async { while let Some(chunk) = stream.next.await { tx.blocking_send(chunk).ok; } });
});
rx
```

详见 [AGENTS.md §2.3](../AGENTS.md)。

---

## LanceDB schema（**契约**）

| 字段 | 类型 | 含义 |
|---|---|---|
| `id` | `Utf8` | `{source_path_hash}:{chunk_ordinal}` |
| `source_path` | `Utf8` | 原始文件路径 |
| `chunk_ordinal` | `Int64` | 该文件里的第几块 |
| `text` | `Utf8` | chunk 文本 |
| `embedding` | `FixedSizeList<Float64, N>` | N 从 `AhaClient.embed_dim` 来 |

⚠️ **改 schema = 不向后兼容**。改时**先**在 `src/store/lancedb_store.rs` 写明 + 更新本文件 + 跑 `lorag reindex` 重建。

**HNSW 索引**：`store::lancedb_store::ensure_hnsw_index` 在 ingest 写完 lancedb 后调；`< 256 rows` 跳过，≥ 256 且没建过则建 IVF-HNSW-FLAT（`IvfHnswFlatIndexBuilder::default`）。

---

## SQLite schema

| 表 | 关键字段 | 用途 |
|---|---|---|
| `sources` | `source_path` UNIQUE, `source_hash` | sha256 幂等摄入 |
| `chunks` | `(source_id, chunk_ordinal)` UNIQUE, `text` | chunk 元数据 + FTS5 源 |
| `chunks_fts` | FTS5 虚拟表（`unicode61` tokenizer） | B全文检索 |
| `messages` | `session_id`, `ordinal` | chat 多轮历史 |

**FTS5 关键设计**：用 OR 语义（`build_fts5_query` 提取拉丁/数字 token + 中文单字），B自动把匹配更多 token 的文档排前面。

中文用 unicode61 按单字切，**双引号包 query 做短语搜索会要求所有单字 token 精确连续出现**——自然语言查询中的补白词（"了什么"、"怎么做"）几乎不会同时出现在文档中，导致 0 匹配。所以默认走 OR 语义。

---

## 混合检索（opt-in）

开启 `HYBRID_ENABLED=true` 时：

1. `vector_search` 取 `top_k * 3`（至少 10）条
2. SQLite FTS5 B搜索取同等条数
3. RRF（Reciprocal Rank Fusion，k=60）两路分数合并 → 取 `top_k`
4. 混合检索启用时**不走 rerank**（RRF 直接输出最终 top_k）

**默认关闭**：小数据集（几十 chunk）下向量检索已覆盖大部分文档，B查询结果高度重合 → RRF 融合无额外收益，只有开销。大文档量（100+ 文件、1000+ chunk）时 `true` 开启互补召回精确关键词（人名、日期、编号）。

---

## 各模块一句话职责

| 模块 | 职责 |
|---|---|
| `config` | `.env` → `AppConfig` 强类型 + fail-fast 校验 |
| `aha_provider` | aha crate 唯一入口 + AhaClient + 路径解析 + rerank 懒加载 |
| `rig_compat` | rig 0.40 的 `CompletionModel` / `EmbeddingModel` trait 适配 |
| `rag` | RAG 主流程（手写 lancedb native + rerank + 4 层防注入 + chat preamble） |
| `chunker` | 段落 + 字符滑窗切块 |
| `ingest/` | 6 种 loader + pipeline |
| `store/lancedb_store` | LanceDB 建表 / 写 / HNSW 索引 / vector_search |
| `store/sqlite_store` | sources / chunks / chunks_fts / messages + FTS5 |
| `server` | axum HTTP server（仅 Web UI / Tray / GUI 用） |
| `tray` | 系统托盘（tray-icon + open_browser 跨平台） |
| `gui` | GPUI 桌面启动器（7 页 sidebar + 托盘 + 服务桥接） |
| `logging` | 公共 tracing init（CLI stderr only / GUI 追加滚动文件） |
| `doctor` | 11 项环境检查 |

详细 Rust API 见 [PLAN.md §4](../PLAN.md)。