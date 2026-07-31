# lorag

> **完全本地运行的 RAG**：把多格式文档（pdf / docx / pptx / xlsx / md / txt）摄入本地
> LanceDB + SQLite，本地 LLM 一次性问答或开多轮对话。所有推理走 [aha](https://github.com/jhqxxx/aha)
> Rust crate 库内调用，**不**起 HTTP server、**不**调云。
>
> 三种使用方式：**桌面 GUI**（双击 exe）/ **Web UI**（浏览器聊天）/ **CLI**（命令行）。

[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Codeberg](https://img.shields.io/badge/codeberg-lorag-blue.svg)](https://codeberg.org/natane/lorag)

---

## 选哪种前端？

| 场景 | 推荐 | 怎么进 |
|---|---|---|
| 办公小白，双击就能用 | **桌面 GUI** | 双击 `lorag-gui.exe`（MSI 安装后） |
| 想要聊天、历史、图表 | **Web UI** | `lorag serve` → 浏览器自动打开 |
| 写脚本、CI、纯命令行 | **CLI** | `lorag ingest / query / chat` |

所有前端共享同一份数据（`./data/`），任意切换。

---

## ✨ 特性

- **完全本地**：模型、向量库、元数据都在你机器上
- **6 种格式摄入**：pdf / docx / pptx / xlsx / md / txt（sha256 幂等）
- **多轮对话 + RAG**：SQLite 历史 + 检索增强；M8 起 token 级流式输出
- **可选 rerank**：配 `RERANK_MODEL=` 启用，召回 +15-25%
- **M8 4 层防注入**：sanitize + chunk 边界包裹 + 系统铁律 + recency bias
- **M9 混合检索（opt-in）**：BM25 + 向量 RRF 融合，大文档量时互补
- **M10 Web UI**：axum + SolidJS + daisyUI，SSE 流式 + Mermaid 图表
- **M11 系统托盘** / **M12 GPUI 桌面 GUI**：开箱即用面对办公小白
- **NVIDIA CUDA 可选加速**；`flash-attn` / `metal` 也支持

明确不做：tool calling（暂不需要）。

---

## 🚀 快速开始（5 分钟跑起来）

需要 [Rust 2024+](https://rustup.rs/) 和 Git。

```bash
# 1. 克隆 + 准备配置
git clone https://codeberg.org/natane/lorag.git
cd lorag
cp .env.example .env       # 按需改模型 id（默认 4B LLM + 0.6B Embedding）

# 2. 编译（首次 5-10 分钟 CUDA，之后增量 30 秒）
cargo build --features cuda

# 3. 下载模型（首次联网 + ~2GB 空间）
lorag models pull

# 4. 摄入文档 + 提问
lorag ingest path/to/your/docs/
lorag query "文档里讲了什么？"
lorag chat                  # 多轮对话 REPL
```

**Web UI 用户**：

```bash
lorag serve                 # 浏览器自动打开 localhost:3000
```

**桌面 GUI 用户**（需 `--features gui` 编译）：见 [doc/gui.md](doc/gui.md)。

> **CUDA 推荐**：`cargo build --features cuda` 让 4B 在 RTX 4080 SUPER 上跑到 1-3s/query；
> 不带 feature 会盖回 CPU 二进制（4B 退化到 15-30s/query）。详见 [doc/install.md](doc/install.md)。
>
> **0.6B LLM 起步也行**：纯 CPU ~5s/query，但答非所问率较高，复杂问题会失望。

---

## 📖 文档导航

| 主题 | 文档 |
|---|---|
| 编译 / CUDA / MSI 打包 | [doc/install.md](doc/install.md) |
| 命令清单 + 日常工作流 | [doc/usage.md](doc/usage.md) |
| `.env` 字段含义 | [doc/configuration.md](doc/configuration.md) |
| 数据流 + 模块边界 + aha 集成 | [doc/architecture.md](doc/architecture.md) |
| M12 桌面 GUI 使用 | [doc/gui.md](doc/gui.md) |
| 接手开发者（dev loop / 排错） | [doc/development.md](doc/development.md) |
| 架构 + 模块设计（Rust API 级） | [PLAN.md](PLAN.md) |
| AI agent 协作规范（避坑 + 硬规矩） | [AGENTS.md](AGENTS.md) |

---

## 📜 License

MIT，详见 [LICENSE](LICENSE)。

## 🙏 Credits

- [aha](https://github.com/jhqxxx/aha) — 本地 LLM / Embedding / Rerank 推理引擎，本项目的核心
- [rig](https://rig.rs) — Rust Agent 框架
- [LanceDB](https://lancedb.github.io/lancedb/) — 嵌入式向量数据库
- [GPUI](https://github.com/zed-industries/zed) + [gpui-component](https://github.com/longbridge/gpui-component) — M12 桌面 GUI 框架
