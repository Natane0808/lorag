# lorag

> **完全本地运行的 RAG CLI**：把多格式文档（pdf / docx / pptx / xlsx / md / txt）摄入本地
> LanceDB + SQLite，然后用本地 LLM 一次性问答。所有推理走 [aha](https://github.com/jhqxxx/aha)
> Rust crate 库内调用，**不**起 HTTP server、**不**调云。
>
> A fully-local Agent RAG CLI. Ingest multi-format documents into local LanceDB + SQLite, then
> ask one-shot RAG questions powered by [aha](https://github.com/jhqxxx/aha) in-process inference.

[![Rust](https://img.shields.io/badge/rust-2024-orange.svg)](https://www.rust-lang.org)
[![License](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

---

## ✨ 特点 / Features

- **完全本地**：模型、向量库、元数据全在你机器上，没任何外部服务
- **零配置启动**：从 [aha](https://github.com/jhqxxx/aha) 自动识别支持的本地模型
  （[aha 支持的模型列表](https://github.com/jhqxxx/aha/blob/main/docs/supported-models.zh-CN.md)）
- **多格式摄入**：pdf / docx / pptx / xlsx / md / txt 6 种，sha256 幂等
- **可观测**：每步 ingestion 打印进度；query 跑出 RAG 命中 / fallback
- **GPU 加速可选**：默认 CPU 跑；要 NVIDIA GPU 加速加 `--features cuda` 一个开关
- **流式 / WebUI 都没有**（明确不做，见 §路线图）

## 🚀 快速开始 / Quick Start

需要 [Rust 2024](https://rustup.rs/) 和 Git。

```bash
# 1. 克隆
git clone https://github.com/yourname/lorag.git
cd lorag

# 2. 配置：拷贝 .env.example 改成 .env，按需改模型 id
cp .env.example .env

# 3. 下载模型（首次需要联网 + ~2GB 空间）
lorag models pull

# 4. 摄入文档
lorag ingest path/to/your/docs/

# 5. 问问题
lorag query "文档里讲了什么？"
```

默认模型（4B LLM + 0.6B Embedding 起步，性价比最优点）：
- LLM：`Qwen/Qwen3-4B`（CUDA 1-3s/query，CPU 15-30s/query；想要更快换 `Qwen3-1.7B` / `0.6B`，想要更强上 `8B`）
- Embedding：`Qwen/Qwen3-Embedding-0.6B`（1024 维，质量比 MiniLM 显著好；维度自动读，不用配）
- 可选：加 `Qwen3-Reranker-0.6B` 召回率再 +15-25%（aha 原生支持）

> **CUDA 推荐**：`cargo build --features aha/cuda` 重 build 一次，4B 在 RTX 4080 SUPER 上能跑到 1-3s/query。
> **0.6B 起步**也行：纯 CPU 也能跑（~5s/query），但 LLM 答非所问率较高，复杂问题会失望。

完整支持的模型列表见 [aha supported-models.zh-CN.md](https://github.com/jhqxxx/aha/blob/main/docs/supported-models.zh-CN.md)。

**换 embedding 模型**（会让向量维度变）：
1. 改 `.env` 里的 `EMBED_MODEL`
2. 跑 `lorag models pull`（下新模型）
3. 跑 `lorag reindex <path>` 重新摄入（**自动**清 LanceDB + SQLite + 重新 ingest 一次）

> 向量维度不需要手动配——`lorag` 启动时自动从 embedding 模型的 `config.json::hidden_size` 读出来。
> 想看 `reindex` 会做什么但不真跑：`lorag reindex --dry-run <path>`。

只换 LLM（不改 embedding）不用清数据库——只更新 `LLM_MODEL` + `lorag models pull` 就行。

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
cargo build --release --features cuda
```

| Feature | 用途 | 前置 |
|---------|------|------|
| `cuda` | NVIDIA GPU 加速（推荐 RTX 3060+） | CUDA Toolkit 12.x + nvcc + MSVC |
| `flash-attn` | 配合 cuda 加速 attention | 需要先有 `cuda` |
| `metal` | macOS Apple Silicon GPU | Xcode CLI tools |

第一次 CUDA 编译要 5-10 分钟（cudnn + cublas 全 link）。编译时需要 CUDA Toolkit，**运行时**
只要 NVIDIA 驱动（带 `cudart64_12.dll` / `libcudart.so`）就够了。

## 📖 命令 / Commands

```
lorag init                   # 把 LLM + embedding 加载到内存
lorag models pull            # 下载配置的模型到 ./data/models
lorag models status          # 看模型是否下载好 + 路径
lorag ingest <PATH>...       # 摄入文件 / 目录（默认递归）
    --ext pdf,docx,md        # 只吃指定扩展（默认全 6 种）
    --force                  # 无视 hash 强制重摄入
    --no-recursive           # 不递归子目录
lorag query <QUESTION>       # 一次性 RAG 问答
    --top-k <N>              # 检索 top_k（默认 5）
lorag chat                   # 多轮对话 REPL（带 SQLite 历史 + RAG）
    --message <TEXT>         # 一次性首问（不读 stdin）
    --session <ID>           # 续接已有 session
    --no-history             # 不带历史（每轮独立）
    --no-rag                 # 不检索，纯 LLM
    --no-banner              # 安静启动
    --top-k <N>              # 覆盖 top_k
lorag sources list           # 列出已摄入文件
    --json
lorag reindex <PATH>...      # 清 LanceDB + SQLite 后重新摄入（换 EMBED_MODEL 后必须走这个）
    --yes / -y                # 跳过确认
    --dry-run                 # 只打印不真做
lorag chat                   # M7 计划：真多轮对话（占位 stub）
lorag doctor                 # ✅ 诊断环境：.env / 模型 / 存储 / 编译 feature（全 PASS exit 0）
```

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
                                  拼 context
                                         │
                                         ▼
                              ┌──────────────────┐
                              │  aha LLM (Qwen3) │
                              └────────┬─────────┘
                                       ▼
                                    答案
```

- **LLM / Embedding 推理**：[aha](https://github.com/jhqxxx/aha) Rust crate（Candle 内核）
- **RAG 编排**：[rig](https://rig.rs) 0.40 框架（自定义 Provider 适配 aha，绕过
  `dynamic_context` 那个 62GB 内存 bug）
- **向量库**：LanceDB 0.30（≥256 chunks 时自动建 IVF-HNSW-FLAT 索引）
- **元数据**：SQLite（rusqlite bundled）

详细设计见 [`PLAN.md`](PLAN.md)；协作约定见 [`AGENTS.md`](AGENTS.md)。

## 🗂️ 项目结构 / Project Structure

```
lorag/
├── Cargo.toml              # 依赖 + feature flags
├── .env.example            # 配置模板
├── PLAN.md                 # 完整设计文档
├── AGENTS.md               # 协作规范
├── src/
│   ├── main.rs             # CLI 入口
│   ├── lib.rs              # 模块声明
│   ├── config.rs           # .env 加载 + 强类型 AppConfig
│   ├── aha_provider.rs     # aha crate 适配 + 模型生命周期
│   ├── rig_compat.rs       # rig 0.40 Provider 适配
│   ├── rag.rs              # RAG 主流程（手写 lancedb native）
│   ├── chunker.rs          # 文本分块
│   ├── models.rs           # SourceRecord / Chunk
│   ├── ingest/             # 6 种 loader + pipeline
│   └── store/              # lancedb_store + sqlite_store
└── tests/
    └── fixtures/           # 测试用文件
```

## 🛣️ 路线图 / Roadmap

- ✅ M0–M5：CLI / aha 加载 / 6 种 loader / chunker / SQLite / LanceDB / RAG 端到端
- 🚧 M6：smoke test
- ✅ `lorag doctor`：诊断命令（环境检查、模型完整性、lancedb 状态）
- ✅ `lorag chat` 多轮对话 + SQLite 持久化历史（M7 done）
- 🚧 README 完善 + 一键 release 脚本
- 📋 流式输出（aha 支持 SSE）
- 📋 Web UI（axum）
- 📋 混合检索（BM25 + 向量 RRF 融合）
- 📋 re-rank（aha 原生支持 `Qwen3-Reranker`）
- 📋 文档结构保留（标题层级、表格、代码块）

完整规划见 [`PLAN.md`](PLAN.md)。

## 🤝 贡献 / Contributing

欢迎 PR / Issue。本项目走个人 GitHub scope，主要自己用，但 **PR 全收**。

开发循环：

```bash
cargo build           # dev profile（opt-level=1，~30s 增量）
cargo clippy --all-targets -- -D warnings
cargo test --lib
```

CI 没配（个人项目），跑上面三个当 self-check。

## 📜 协议 / License

MIT，见 [LICENSE](LICENSE)。

## 🙏 致谢 / Credits

- [aha](https://github.com/jhqxxx/aha) — 本地 LLM / Embedding 推理引擎，本项目的核心
- [rig](https://rig.rs) — Rust Agent 框架
- [LanceDB](https://lancedb.github.io/lancedb/) — 嵌入式向量数据库
