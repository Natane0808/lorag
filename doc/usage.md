# 用法 / Usage

> lorag 命令清单 + 日常工作流。所有错误用 `anyhow` 打到 stderr，exit 1。

---

## 快速参考

| 命令 | 干啥 |
|---|---|
| `lorag models pull` | 下载 LLM + Embedding（+ Rerank，如果配了） |
| `lorag ingest <PATH>` | 摄入文件 / 目录（sha256 幂等） |
| `lorag query "..."` | 一次性 RAG 问答 |
| `lorag chat` | 多轮对话 REPL |
| `lorag reindex <PATH>` | 清库 + 重新 ingest（换 EMBED_MODEL 后必须） |
| `lorag sources list` | 列出已摄入文件 |
| `lorag doctor` | 11 项环境检查 |
| `lorag serve` | 启动 Web UI（浏览器聊天） |
| `lorag tray` | 启动 Web UI + 系统托盘 |
| `lorag-gui` | 启动桌面 GUI 启动器（需 `--features gui` 编译） |

---

## 完整命令参考

### `lorag init`

把 LLM + embedding 加载到内存。debug 用 —— `query` / `chat` 隐式调用，不需要手动跑。

### `lorag models pull`

下载 `LLM_MODEL` + `EMBED_MODEL`（+ `RERANK_MODEL` 如果配了）。`RERANK_MODEL` 留空跳过。

```bash
lorag models pull
```

### `lorag models status [--init]`

看模型文件存在性 + 可选真 load 验证（`--init` 触发真 load，比较慢但能 catch 损坏的 `*.safetensors`）。

### `lorag ingest <PATH>...`

摄入文件 / 目录，默认递归。sha256 幂等 —— 重复摄入只打 `skipped: <path> (unchanged)`。

```bash
lorag ingest path/to/your/docs/ # 默认 6 种格式 + 递归
lorag ingest file1.pdf file2.docx # 多个路径
lorag ingest path/ --ext pdf,docx,md # 限定格式
lorag ingest path/ --force # 强制重摄入（无视 hash）
lorag ingest path/ --no-recursive # 不递归
```

支持格式：`pdf` / `docx` / `pptx` / `xlsx` / `md` / `txt`。

### `lorag reindex <PATH>...`

**清 LanceDB + SQLite 后重新 ingest**。换 `EMBED_MODEL` 后必须走这个（向量维度变了 schema 不兼容）。

```bash
lorag reindex path/to/your/docs/ # 会问 "are you sure?"
lorag reindex path/ --yes # 跳过交互确认
lorag reindex path/ --dry-run # 只打印会做什么，不真跑
lorag reindex path/ --ext pdf,md # 同 ingest 的 --ext
```

⚠️ **不要手动 `rm -rf data/lancedb data/lorag.db`** —— reindex 帮你管交互确认 + sqlite 旁文件 + WAL。

### `lorag sources list [--json]`

列出已摄入的源文件（路径 + 大小 + chunk 数 + 时间）。

```bash
lorag sources list # 人类可读
lorag sources list --json # JSON，脚本里用
```

### `lorag query <QUESTION>`

一次性 RAG 问答。 起 token 级流式输出（CPU 跑 4B 不再"干等 30 秒"）。

```bash
lorag query "文档里讲了什么？"
lorag query "..." --top-k 10 # 覆盖 cfg.top_k
lorag query "..." --no-rerank # 跳过 rerank（即使 .env 配了）
lorag query "..." --rerank-top-n 30 # 覆盖 cfg.rerank_top_n
lorag query "..." --no-hybrid # 关闭混合检索（即使 .env 开了）
```

**回退行为**：LanceDB 任何错误（目录不存在 / 表不存在 / 内存不够）→ 自动回退到 `bare_llm_query`（裸 LLM，无上下文）。这是有意的 —— 让 LLM 在检索挂的时候还能答（虽然答得不准）。

### `lorag chat`

多轮对话 REPL，带 SQLite 历史 + RAG 检索。

```bash
lorag chat # 启动 REPL
lorag chat --message "你好" # 一次性首问（不读 stdin）
lorag chat --no-history # 不带历史（每轮独立）
lorag chat --no-banner # 安静启动
lorag chat --no-rag # 纯 LLM 对话（关 RAG 上下文；防注入 1-2 层不生效）
lorag chat --top-k 8 --no-rerank --no-hybrid # 各种 override
```

REPL 命令：
- 正常输入 → 多轮对话
- `/reset` → 清空当前会话历史
- `Ctrl-C` / `Ctrl-D` → 退出

⚠️ `chat` **不**做"续接 session" —— 实装发现几乎没人用，已 drop。每次启动都是新会话；session_id 仅供 sqlite 主键用。

### `lorag serve [--port <N>]`

启动 Web UI（axum + SolidJS + daisyUI），浏览器自动打开 `http://localhost:3000`。

```bash
lorag serve # 默认 port 3000
lorag serve --port 8080
```

完整功能 + 路由列表见 [PLAN.md §4.10](../PLAN.md)。前端在浏览器里，不是 GPUI。

### `lorag tray [--port <N>]`

启动 Web UI + 系统托盘图标常驻，浏览器自动打开。右键托盘菜单：Open Web UI / Quit（优雅关闭，5 秒超时强退）。

### `lorag-gui [--port <N>]`

启动 GPUI 桌面 GUI 启动器（7 页 sidebar + 托盘）。需要 `--features gui` 编译：

```bash
cargo build --features cuda --features gui
lorag-gui
```

详见 [doc/gui.md](gui.md)。

### `lorag doctor`

11 项环境检查：`.env` 解析 / 模型文件存在 / LanceDB 可访问 / SQLite 可写 / features 编译状态等。

```bash
lorag doctor
```

---

## 日常工作流

### 添加新文档

```bash
# 把文件丢进文件夹，然后：
lorag ingest path/to/new_docs/
```

重复摄入相同文件 hash 一致就 skip，不会重复入库。

### 换 LLM（不动 embedding）

```bash
# 1. 改 .env 的 LLM_MODEL
# 2. 拉新模型
lorag models pull
# 3. 重启 lorag（任何命令重新跑就行）
```

不用清库。

### 换 embedding 模型（必须重建）

```bash
# 1. 改 .env 的 EMBED_MODEL
# 2. 拉新模型
lorag models pull
# 3. 先 dry-run 看会做什么
lorag reindex path/to/your/docs/ --dry-run
# 4. 真重建
lorag reindex path/to/your/docs/ --yes
```

### 排查"LLM 没回答对的文档到底入了没"

```bash
lorag sources list --json | jq '.[] | select(.path | contains("那个文件名"))'
```

确认入了 → 试试 `lorag query "..." --top-k 20` 看更宽的 top-k 召回情况；如果还是召回不到 → 检查 chunker / embedding 维度是否对（`lorag doctor`）。

### 排查 LanceDB 错误

默认会回退到裸 LLM。如果你想看真错误：

```bash
RUST_LOG=info lorag query "..." 2>&1 | head -50
# 或者看 lance 内部：
RUST_LOG=lance::execution=debug lorag query "..." 2>&1 | head -100
```

---

## 数据存哪

| 内容 | 位置 |
|---|---|
| LanceDB（向量） | `./data/lancedb/` |
| SQLite（元数据 + 历史） | `./data/lorag.db`（+ `-wal` / `-shm` / `-journal`） |
| 下载的模型 | `./data/models/<repo>/` |
| `.env` | 当前目录，或 `LORAG_ENV` 指向的路径 |
| GUI 磁盘日志（仅 `lorag-gui`） | `%APPDATA%\lorag\logs\lorag.log.YYYY-MM-DD`（保留 7 天） |

---

## 环境变量

- `LORAG_ENV`：覆盖 `.env` 路径（CI / 多套配置时用）
- `RUST_LOG`：覆盖 tracing filter（默认走 `LOG_LEVEL` + lance silencing 后缀；详见 [doc/development.md](development.md)）

---

## 退出码

- `0`：成功
- `1`：通用错误（`anyhow::Error` 打到 stderr）
- 启动期配置错误（缺字段 / 字段冲突）：panic + 打印可执行下一步

---

## 更多信息

- 字段含义 → [doc/configuration.md](configuration.md)
- 怎么编译 → [doc/install.md](install.md)
- 数据流 → [doc/architecture.md](architecture.md)
- 桌面 GUI 使用 → [doc/gui.md](gui.md)
- 排错 / 开发循环 → [doc/development.md](development.md)