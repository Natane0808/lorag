# lorag

> **完全本地运行的 RAG CLI**：把多格式文档（pdf / docx / pptx / xlsx / md / txt）摄入本地
> LanceDB + SQLite，然后用本地 LLM 一次性问答或开多轮对话。所有推理走 [aha](https://github.com/jhqxxx/aha)
> Rust crate 库内调用，**不**起 HTTP server、**不**调云。
>
> A fully-local Agent RAG CLI. Ingest multi-format documents into local LanceDB + SQLite, then
> ask one-shot RAG questions or chat with history, powered by [aha](https://github.com/jhqxxx/aha)
> in-process inference.

[![Rust](https://img.shields.io/badge/rust-2021-orange.svg)](https://www.rust-lang.org)
[![License](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Codeberg](https://img.shields.io/badge/codeberg-lorag-blue.svg)](https://codeberg.org/natane/lorag)

---

## ✨ 特点 / Features

- **完全本地**：模型、向量库、元数据全在你机器上，没任何外部服务
- **6 种格式摄入**：pdf / docx / pptx / xlsx / md / txt，sha256 幂等
- **多轮对话**：REPL 带 SQLite 历史 + RAG 检索
- **可选 rerank**：配 `RERANK_MODEL=` 即启用（aha `Qwen3-Reranker-0.6B`），召回 +15-25%
- **M8 流式输出**：aha → lorag mpsc 通道，token 级逐字打印；CPU 跑 4B 不再"干等 15-30 秒"
- **M8 4 层防注入**：① sanitize ② chunk 边界包裹 ③ 系统 prompt 5 条铁律 ④ recency bias 尾注
- **M8 Prompt 可配置**：4 个 `PROMPT_*` 字段覆盖默认（默认含 5 条防注入铁律）
- **可观测**：每步 ingestion 打印进度；query 跑出 RAG 命中 / fallback
- **GPU 加速可选**：默认 CPU 跑；NVIDIA GPU 加 `--features cuda`
- **明确不做**：Web UI（M10 计划）/ 工具调用（Backlog）

---

## 🚀 快速开始 / Quick Start

需要 [Rust 2021+](https://rustup.rs/) 和 Git。

```bash
# 1. 克隆
git clone https://codeberg.org/natane/lorag.git
cd lorag

# 2. 配置：拷贝 .env.example 改成 .env，按需改模型 id
cp .env.example .env

# 3. 下载模型（首次需要联网 + ~2GB 空间）
lorag models pull

# 4. 摄入文档
lorag ingest path/to/your/docs/

# 5. 问问题
lorag query "文档里讲了什么？"

# 6.（可选）开多轮对话
lorag chat
```

**默认模型**（4B LLM + 0.6B Embedding 起步，性价比最优点）：

- LLM：`Qwen/Qwen3-4B`（CUDA 1-3s/query，CPU 15-30s/query；M8 起 token 级流式输出，不再"干等 30s"）
- Embedding：`Qwen/Qwen3-Embedding-0.6B`（1024 维，质量比 MiniLM 显著好；维度自动读，不用配）
- Rerank（**可选**）：`Qwen/Qwen3-Reranker-0.6B`（**留空 = 禁用**，懒加载，第一次 query 才 load）
- **Prompt 可配置**（M8）：4 个 `PROMPT_*` 字段覆盖默认（默认含 5 条防注入铁律 + recency bias 尾注，详见 [`.env.example`](.env.example)）

> **CUDA 推荐**：`cargo build --features cuda` 重 build 一次，4B 在 RTX 4080 SUPER 上能跑到 1-3s/query。
> **0.6B 起步**也行：纯 CPU 也能跑（~5s/query），但 LLM 答非所问率较高，复杂问题会失望。

完整支持的模型列表见 [aha supported-models.zh-CN.md](https://github.com/jhqxxx/aha/blob/main/docs/supported-models.zh-CN.md)。

### 换 embedding 模型（会让向量维度变）

1. 改 `.env` 里的 `EMBED_MODEL`
2. 跑 `lorag models pull`（下新模型）
3. 跑 `lorag reindex <path>` 重新摄入（**自动**清 LanceDB + SQLite + 重新 ingest 一次）

> 向量维度不需要手动配——`lorag` 启动时自动从 embedding 模型的 `config.json::hidden_size` 读出来。
> 想看 `reindex` 会做什么但不真跑：`lorag reindex --dry-run <path>`。

只换 LLM（不改 embedding）不用清数据库——只更新 `LLM_MODEL` + `lorag models pull` 就行。

---

## 🛠️ 编译 / Build

```bash
# 默认（CPU only，~30s 增量 / 几分钟首次）
cargo build

# 跑测试
cargo test --lib

# Release 优化（首次 link 5-10 分钟，之后 incremental ~30s）
cargo build --release
```

### GPU 加速（NVIDIA CUDA）

要 RTX/GTX 跑得更快：

```bash
# 编译时启用 CUDA（需要 CUDA Toolkit 12.x + nvcc + MSVC）
cargo build --features cuda
```

| Feature | 用途 | 前置 |
|---------|------|------|
| `cuda` | NVIDIA GPU 加速（推荐 RTX 3060+） | CUDA Toolkit 12.x + nvcc + MSVC |
| `flash-attn` | 配合 cuda 加速 attention | 需要先有 `cuda` |
| `metal` | macOS Apple Silicon GPU | Xcode CLI tools |

> **⚠️ 不要用 `cargo build` 日常循环**——`cargo build`（无 feature）会**覆盖** CUDA 二进制为 CPU-only 版本。改完代码后**必须**用 `cargo build --features cuda` 保住 GPU 加速（CPU 跑 4B 15-30s/query，CUDA 1-3s/query）。
> 第一次 CUDA 编译要 5-10 分钟（cudnn + cublas 全 link）。编译时需要 CUDA Toolkit，**运行时**只要 NVIDIA 驱动（带 `cudart64_12.dll` / `libcudart.so`）就够了。

---

## 📖 命令 / Commands

```
lorag init                          # 把 LLM + embedding 加载到内存（debug 用；query/chat 隐式调）
lorag models pull                   # 下载 LLM + embedding + rerank（rerank 留空跳过）
lorag models status [--init]        # 看模型文件存在性 + 可选真 load 验证

lorag ingest <PATH>...              # 摄入文件/目录（默认递归）
    --ext pdf,docx,pptx,xlsx,md,txt # 默认全 6 种
    --force                         # 强制重摄入（无视 hash）
    --no-recursive                  # 不递归子目录

lorag reindex <PATH>...             # 清 LanceDB + SQLite 后重新 ingest（换 EMBED_MODEL 后必须走这个）
    --yes / -y                      # 跳过 interactive 确认
    --dry-run                       # 只打印会做什么

lorag sources list [--json]         # 列出已摄入文件

lorag query <QUESTION>              # 一次性 RAG 问答（M8 起 token 级流式输出）
    --top-k <N>                     # 覆盖 cfg.top_k
    --no-rerank                     # 跳过 rerank（即使 .env 配了 RERANK_MODEL）
    --rerank-top-n <N>              # 覆盖 cfg.rerank_top_n

lorag chat                          # 多轮对话 REPL（带 SQLite 历史 + RAG；M8 起 token 级流式输出；进程内连续，跨进程不续接）
    --message <TEXT>                # 一次性首问（不读 stdin）
    --no-history                    # 不带历史（每轮独立）
    --no-banner                     # 安静启动
    --no-rag                        # 纯 LLM 对话（关闭 RAG 上下文；防注入 1-2 层不生效）
    --no-rerank / --rerank-top-n <N>
    --top-k <N>

lorag doctor                        # 11 项环境检查（env / models / storage / features）
```

错误一律 `anyhow` 打到 stderr，exit 1。`.env` 路径默认当前目录，可由 `LORAG_ENV` 环境变量覆盖。

---

## 🧠 工作原理 / How It Works

```
   ┌─────────────┐
   │ 你的文档     │ PDF / DOCX / PPTX / XLSX / MD / TXT
   └──────┬──────┘
          │ ingest (sha256 幂等)
          ▼
   ┌─────────────┐    ┌──────────────┐
   │  chunker    │───▶│  aha embed   │  (Qwen3-Embedding-0.6B, 1024-dim)
   └─────────────┘    └──────┬───────┘
                             │
                  ┌──────────┴──────────┐
                  ▼                     ▼
           ┌─────────────┐      ┌──────────────┐
           │  LanceDB    │      │   SQLite     │
           │  (向量)     │      │  (元数据)    │
           └──────┬──────┘      └──────────────┘
                  │
   你的问题 ──── embed ────▶ top-k 检索 ──┐
                                         ▼
                                  [可选] rerank
                                         │
                                         ▼
                                  拼 context (history + chunks)
                                         │
                                         ▼
                              ┌──────────────────┐
                              │  aha LLM (Qwen3) │
                              └────────┬─────────┘
                                       ▼
                                    答案
```

- **LLM / Embedding / Rerank 推理**：[aha](https://github.com/jhqxxx/aha) Rust crate（Candle 内核）
- **RAG 编排**：[rig](https://rig.rs) 0.40 框架（自定义 Provider 适配 aha，**手写** lancedb native
  query——绕开 `dynamic_context` 那个 62GB 内存 bug）
- **向量库**：LanceDB 0.30（≥256 chunks 时自动建 IVF-HNSW-FLAT 索引）
- **元数据**：SQLite（rusqlite bundled）

详细设计见 [`PLAN.md`](PLAN.md)；历史变更见 [`CHANGELOG.md`](CHANGELOG.md)；协作约定见 [`AGENTS.md`](AGENTS.md)。

---

## 🗂️ 项目结构 / Project Structure

```
lorag/
├── Cargo.toml                  # 依赖 + aha path 依赖
├── .env.example                # 配置模板
├── README.md                   # ← 本文件
├── PLAN.md                     # 当前架构 + 决策 + 限制 + 未来
├── CHANGELOG.md                # 历史变更 + bug 教训
├── AGENTS.md                   # agent 协作规范
├── LICENSE                     # MIT
├── src/
│   ├── main.rs                 # CLI 入口（clap 分派）
│   ├── lib.rs                  # 模块声明
│   ├── config.rs               # .env 加载 + 强类型 AppConfig
│   ├── aha_provider.rs         # ★ 唯一 aha 入口 + 模型生命周期 + rerank
│   ├── rig_compat.rs           # AhaCompletionModel + AhaEmbeddingModel
│   ├── rag.rs                  # RAG 主流程（手写 lancedb native）+ chat preamble
│   ├── chunker.rs              # 段落 + 字符滑窗切块
│   ├── models.rs               # SourceRecord / Chunk / MessageRecord
│   ├── doctor.rs               # 11 项环境检查
│   ├── ingest/                 # 6 种 loader + pipeline
│   │   ├── loader.rs           # 按扩展名分派
│   │   ├── pdf.rs / docx.rs / pptx.rs / xlsx.rs / md.rs / txt.rs
│   │   └── pipeline.rs
│   └── store/                  # lancedb_store + sqlite_store
│       ├── lancedb_store.rs    # 建表 / HNSW 索引
│       └── sqlite_store.rs     # sources / chunks / messages
└── tests/                      # cargo test（fixtures/ 已 gitignore）
```

---

## 🛣️ 路线图 / Roadmap

- ✅ M0–M5：CLI / aha 加载 / 6 种 loader / chunker / SQLite / LanceDB / RAG 端到端
- ✅ M5.1 `lorag reindex`：换 EMBED_MODEL 后清库重建
- ✅ M6：`lorag doctor` 11 项环境检查
- ✅ M7 `lorag chat`：多轮 REPL + SQLite 历史 + RAG fallback
- ✅ M7.1 Rerank（可选）：Qwen3-Reranker 懒加载 + `--no-rerank` + `RERANK_TOP_N` 可配
- ✅ M8 流式输出 + 4 层防注入 + 4 个 PROMPT_* 可配 + XLSX 多 sheet 行前缀
- 📋 M9 混合检索（SQLite FTS5 BM25 + 向量 RRF 融合）—— 纯向量对关键词不敏感（`DFDB` / 数字日期都召回失败）
- 📋 M10 Web UI（axum server + 浏览器 + HTMX）—— 前置依赖 M8
- 📋 M11 CI（Codeberg CI / `.forgejo/workflows/ci.yml`）
- 📋 M12 MCP server（把 `lorag` 暴露成 MCP tools，让 IDE agent 直接调）
- 📋 Backlog：tool calling / 多知识库 / 模型量化 / 评估框架增强 / rerank 价值验证 / 发布到 crates.io

完整规划见 [`PLAN.md`](PLAN.md)；按优先级和触发条件标在 [PLAN.md §11](PLAN.md)。

---

## 🤝 贡献 / Contributing

欢迎 PR / Issue。本项目走个人 codeberg scope（[`codeberg.org/natane/lorag`](https://codeberg.org/natane/lorag)），主要自己用，但 **PR 全收**。

开发循环：

```bash
cargo build --features cuda    # 保住 CUDA 二进制
cargo clippy --all-targets --features cuda -- -D warnings
cargo test --lib --features cuda
```

CI 没配（个人项目），跑上面三个当 self-check。**改完代码后必须**用 `cargo build --features cuda`——`cargo build`（无 feature）会盖掉 CUDA 二进制（详见 [PLAN.md §10 经验](PLAN.md)）。

> **敏感数据**：开发脚本 / test fixture / doc 例子**绝不**入公司名 / 真人名 / 内部系统名 / 业务术语。

---

## 📜 协议 / License

MIT，见 [LICENSE](LICENSE)。

---

## 🙏 致谢 / Credits

- [aha](https://github.com/jhqxxx/aha) — 本地 LLM / Embedding / Rerank 推理引擎，本项目的核心
- [rig](https://rig.rs) — Rust Agent 框架
- [LanceDB](https://lancedb.github.io/lancedb/) — 嵌入式向量数据库
