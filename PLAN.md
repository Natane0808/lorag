# lorag — 规划（架构 / 模块设计）

> **定位**：本文讲 lorag 的**技术架构 + 模块设计**（Rust API 级别）。其它一切（用户文档 / 命令清单 / 避坑经验 / 路线图）已经拆出去，分别见：
>
> - 用户入口 → [README.md](README.md)
> - 命令清单 → [doc/usage.md](doc/usage.md)
> - `.env` 配置 → [doc/configuration.md](doc/configuration.md)
> - 数据流 + 模块边界 + aha 集成 → [doc/architecture.md](doc/architecture.md)
> - M12 桌面 GUI 使用 → [doc/gui.md](doc/gui.md)
> - 接手开发者（dev loop / 排错）→ [doc/development.md](doc/development.md)
> - AI agent 协作规范（避坑 + 硬规矩）→ [AGENTS.md](AGENTS.md)
>
> **本文是 Rust API 级的模块设计**，不是用户文档。

---

## 1. 项目目标

完全本地运行的 Agent RAG CLI + Web UI + Tray + GPUI 桌面 GUI：

- 摄入多格式文档（pdf / docx / pptx / xlsx / md / txt）入 LanceDB + SQLite
- 一次性 RAG 问答 + 多轮对话 REPL（带历史 + RAG）
- 全部推理走 aha Rust crate，**不**起 HTTP、**不**调云
- 配置切换模型、维度、数据库路径全在 `.env` 里

**明确不做**（除非触发用户需求）：

- 多用户 / 权限
- 工具调用（tool use / function calling）

---

## 2. 技术栈

| 组件 | 选型 | 用途 |
|------|------|------|
| 语言 | Rust 2024 edition | — |
| 推理 | [`aha = { path = "D:/workspace/rust/aha" }`](https://github.com/jhqxxx/aha) | Candle 内核，LLM + embedding + rerank 库内调用 |
| 框架 | `rig` **0.40** | agent / completion / embedding 抽象；自定义 aha Provider |
| 向量库 | `lancedb` **0.30** | 手写 native API（绕开 `dynamic_context` 62GB bug） |
| 元数据 | `rusqlite` (bundled) | source / chunk / messages / FTS5 |
| 文档解析 | `pdf-extract` / `calamine` / `zip` + `quick-xml` / `pulldown-cmark` | 6 种 loader |
| 异步 | `tokio` (rt-multi-thread) | 包裹同步 candle 推理 |
| CLI | `clap` v4 (derive) | 命令解析 |
| Web UI | `axum` 0.8 + SolidJS + Vite + daisyUI 5 | 浏览器聊天界面 |
| 系统托盘 | `tray-icon` 0.19 + `image` 0.25 + `windows-sys` 0.59 | `lorag tray` + M12 GUI 托盘共存 |
| 桌面 GUI | `gpui` / `gpui_platform` (zed) + `gpui-component@57a9903f` + `rfd` 0.15 + `tracing-appender` 0.2 | M12 `lorag-gui`（feature flag `gui` 隔离） |
| 配置 | `dotenvy` + 手 parse | `.env` → `AppConfig` |
| 日志 | `tracing` + `tracing-subscriber` (+ `tracing-appender` for GUI) | silence lance 噪声 / GUI 磁盘滚动 |

**关键决策**：

- **aha 走 crate，不起 server**：单进程持有 LLM + embedding 两个 `ModelInstance`，函数调用直传；无端口、无 base_url、无 health check。下载也走 `aha::utils::download_model`，不调 aha CLI 二进制
- **rig 自定义 provider**：`AhaClient` 实现 `CompletionClient` + `EmbeddingsClient`，**不**实现 `Provider` / `ProviderClient`（0.40 那两个是给 HTTP-based 用的）。详见 `src/rig_compat.rs`
- **绕开 `dynamic_context`**：rig 0.40 + rig-lancedb 0.40 + lancedb 0.30 集成在某步会一次性分配 ~62GB（实测，5 chunks 也炸）。`src/rag.rs` 改走手写 `table.vector_search()` + `RecordBatch` 流式读

---

## 3. 目录结构

```
lorag/
├── Cargo.toml                  # aha path + rig 0.40 + lancedb 0.30 + ...
├── .env.example                # 配置模板
├── .gitignore                  # data/ / .env / .omo/ / tests/fixtures/ / nul
├── README.md                   # 入口 / 快速开始
├── PLAN.md                     # ← 本文件：技术架构 + Rust API 级模块设计
├── AGENTS.md                   # AI agent 协作规范（避坑 + 硬规矩）
├── LICENSE                     # MIT
├── doc/                        # 用户文档
│   ├── install.md              # 编译 / CUDA / MSI
│   ├── usage.md                # 命令 + 工作流
│   ├── configuration.md        # .env 字段
│   ├── architecture.md         # 数据流 + 模块边界 + aha 集成
│   ├── gui.md                  # M12 桌面 GUI 使用
│   └── development.md          # 接手开发者
├── src/
│   ├── main.rs                 # CLI 入口（clap 分派）
│   ├── gui_main.rs             # M12 GUI bin 入口（GPU probe + GPUI bootstrap + tokio runtime + tray）
│   ├── lib.rs                  # 模块声明
│   ├── config.rs               # dotenvy + AppConfig + validate (+ save_to_dotenv for settings page)
│   ├── logging.rs              # 公共 tracing init（CLI/GUI 共用；GUI 开 tracing-appender 滚动文件）
│   ├── aha_provider.rs         # ★ 唯一 aha 入口：AhaClient + ensure_rerank + 路径解析
│   ├── rig_compat.rs           # AhaCompletionModel + AhaEmbeddingModel（rig 0.40 trait 适配）
│   ├── rag.rs                  # RAG 主流程（手写 lancedb native）+ chat preamble + 防注入 4 层
│   ├── chunker.rs              # 段落 + 字符滑窗切块
│   ├── models.rs               # SourceRecord / Chunk / MessageRecord
│   ├── doctor.rs               # 11 项环境检查（+ pub Check/CheckResult 供 GUI 消费）
│   ├── tray.rs                 # 系统托盘核心（M11，GUI 复用 open_browser）
│   ├── server.rs               # M10 axum HTTP server + SSE 流式 API + 嵌入式前端
│   ├── ingest/
│   │   ├── loader.rs           # 按扩展名分派
│   │   ├── pdf.rs / docx.rs / pptx.rs / xlsx.rs / md.rs / txt.rs
│   │   └── pipeline.rs         # 摄入主流程
│   ├── store/
│   │   ├── lancedb_store.rs    # 建表 / HNSW 索引 / vector_search
│   │   └── sqlite_store.rs     # sources / chunks / chunks_fts / messages
│   └── gui/                    # M12 GPUI 桌面启动器（feature = gui）
│       ├── mod.rs              # gui 模块 root
│       ├── app.rs              # AppState entity
│       ├── root_view.rs        # sidebar + 页面 dispatcher
│       ├── sidebar.rs          # 7 页侧边栏导航
│       ├── gpu_probe.rs        # 启动 GPU 探测（失败 → fallback_dialog）
│       ├── fallback_dialog.rs  # 无 GPU 友好原生对话框
│       ├── logging.rs          # tracing Layer → broadcast channel 桥接
│       ├── tray_host.rs        # GUI 托盘（独立 OS thread + Win32 pump + 桥接）
│       ├── service.rs          # 服务控制页
│       ├── models.rs           # 模型管理页
│       ├── ingest.rs           # 文档摄入页
│       ├── doctor.rs           # 健康检查页
│       ├── logs.rs             # 日志页
│       ├── settings.rs         # 设置页
│       ├── about.rs            # 关于页
│       ├── autostart.rs        # 开机自启（G13 TODO，未接设置页）
│       └── pages/              # pages mod root + Page enum
└── tests/                      # cargo test（fixtures/ 已 gitignore）
```

---

## 4. 模块设计（Rust API 级）

### 4.1 `config.rs`

`AppConfig`（`src/config.rs`）从 `.env` 强类型加载，validate 启动期拦截。**没有端口 / base_url / health 配置** —— aha 走 crate。

完整字段表见 [doc/configuration.md](doc/configuration.md)；本节只列 Rust 结构上的设计要点：

- 字段用 `#[serde(default = "...")]` 给老 `.env` 兼容期
- 必填字段缺失 → panic + 打印可执行的下一步（`run: lorag models pull`）
- 数字字段冲突（如 `RERANK_TOP_N < TOP_K`）→ panic + 解释为啥错
- `save_to_dotenv()` 原子写（`.tmp` → rename），给 GUI 设置页用

### 4.2 `aha_provider.rs`（★ 唯一 aha 入口）

`AhaClient` 持有 LLM / embedding / rerank 三个 slot：

```rust
pub struct AhaClient {
    llm: Option<Arc<Mutex<ModelInstance<'static>>>>,                              // None if init_embed_only
    embed: Arc<Mutex<ModelInstance<'static>>>,                                   // 必有
    rerank_slot: Arc<tokio::sync::OnceCell<Arc<Mutex<ModelInstance<'static>>>>>,  // 懒加载
    embed_dim: Option<usize>,                                                    // 从 config.json::hidden_size 读
    cfg: Arc<AppConfig>,
}
```

主要 API：

- `init(cfg)`：load LLM + embedding + 读 `embed_dim`（被 `init` / `query` / `chat` / `models status --init` 调）
- `init_embed_only(cfg)`：只 load embedding（被 `ingest` / `reindex` 调 —— 省 LLM 的 ~8GB 内存 + 数十秒 load）
- `has_llm()` / `has_rerank()`：区分 init 模式 + rerank 是否启用
- `ensure_rerank()`：懒加载 rerank 模型（`OnceCell` 内部保证并发只 load 一次）
- `rerank_score(query, docs)`：调 aha `ModelInstance::rerank`（同步 → `spawn_blocking`）
- `llm_generate(params)` / `embed_texts(texts)`：candle 同步包 `spawn_blocking`
- `llm_generate_stream(params)`：M8 流式版，通过 `mpsc::channel(64)` 桥接 aha `generate_stream`。返回 `Receiver<Result<String>>` 逐 token 消费

辅助：

- `resolve_model_path(repo, save_dir)`：路径解析（见 [doc/architecture.md](doc/architecture.md) §aha 集成）
- `ensure_model_downloaded(repo, save_dir, max_retries)`：调 `aha::utils::download_model`（幂等）
- `read_hidden_size_from_config(path)`：从 `config.json` 读 `hidden_size`
- `models_status(cfg)` + `print_models_status(...)`：`lorag models status` 用

### 4.3 `rig_compat.rs`

实现 rig 0.40 trait：

- `AhaCompletionModel: CompletionModel`（`stream()` 返 `Err`，`type StreamingResponse = ()`）
- `AhaEmbeddingModel: EmbeddingModel`（`MAX_DOCUMENTS = 1024`，`ndims()` 从 `client.embed_dim()` 读）
- `AhaClient: CompletionClient + EmbeddingsClient`（**不**实现 `Provider`）

消息转换 `convert_messages`：rig `CompletionRequest` → aha `Vec<ChatMessage>`（preamble + documents + chat_history）。

### 4.4 `rag.rs`

RAG 主流程，**不**用 `AgentBuilder::dynamic_context`（62GB bug），手写 lancedb native：

```rust
pub async fn retrieve_chunks(
    client: &AhaClient, cfg: &AppConfig,
    sqlite: Option<&SqliteStore>, question: &str,
    top_k: usize, enable_hybrid: bool,
    enable_rerank: bool, rerank_top_n: usize,
) -> Result<Vec<String>>;

pub async fn llm_complete(
    client: &AhaClient, cfg: &AppConfig, preamble: String, question: &str,
) -> Result<String>;

/// M8 流式版 llm_complete，通过 mpsc channel 逐 token 输出。
/// 调用方用 `tokio_stream::wrappers::ReceiverStream` 消费。
pub async fn llm_complete_stream(
    client: &AhaClient, cfg: &AppConfig,
    preamble: String, question: &str,
) -> Result<tokio::sync::mpsc::Receiver<anyhow::Result<String>>>;

pub async fn rag_query(...) -> Result<String>;  // RAG + fallback to bare LLM
pub async fn bare_llm_query(...) -> Result<String>;

/// 用户输入防注入：转义 ChatML token + HTML 实体 + 角色前缀
pub fn sanitize_user_input(input: &str) -> String;
/// 将 chunks 格式化为 [文档片段 N]...[文档片段 N] 包裹的上下文
pub fn format_chunks_for_context(chunks: &[String]) -> String;
/// 构建 RAG prompt（拼接系统角色 + 上下文 + 问题 + 防注入尾注）
pub fn build_rag_preamble(cfg: &AppConfig, context: &str, question: &str) -> String;
/// 构建 chat 多轮对话 preamble（cfg 参数新增，M8 重构）
pub fn build_chat_preamble(cfg: &AppConfig, history: &[MessageRecord], chunks: &[String]) -> String;
pub fn is_recoverable_error(err: &str) -> bool;
```

**混合检索路径**（`HYBRID_ENABLED=true` 时，SQLite FTS5 + 向量 RRF 融合）：
1. `vector_search` 取 `top_k * 3`（至少 10）条
2. SQLite FTS5 BM25 搜索取同等条数
3. RRF（Reciprocal Rank Fusion，k=60）两路分数合并 → 取 `top_k`
4. 混合检索启用时**不走 rerank**（RRF 直接输出最终 top_k）

**纯向量 + Rerank 路径**（混合检索关闭 + `cfg.rerank_model` 非空 + `--no-rerank` 未传时启用）：
1. `vector_search` 取 `max(top_k, rerank_top_n)` 条
2. 调 `client.rerank_score(question, &chunks)` 打分
3. 按分数降序排，取前 `top_k` 条
4. 留空时走零开销直 vector_search top_k

**LanceDB schema 契约**：见 [doc/architecture.md](doc/architecture.md) §LanceDB schema。改 = 不向后兼容。

**HNSW 索引**：`store::lancedb_store::ensure_hnsw_index` 在 ingest 写完 lancedb 后调；`< 256 rows` 跳过，≥ 256 且没建过则建 IVF-HNSW-FLAT（`IvfHnswFlatIndexBuilder::default()`）。

### 4.5 `ingest/`

- `loader.rs`：按扩展名分派（pdf / docx / pptx / xlsx / md / txt）
- 各 `*::extract(path: &Path) -> Result<String>`：纯文本提取（不知道 LanceDB / SQLite 存在）
- `pipeline.rs::run_ingest`：摄入主流程（loader → chunker → embed → lancedb → sqlite → HNSW）
- 单个文件失败 → warn + skip，不中断整次 ingest

### 4.6 `store/`

- `lancedb_store.rs`：建表 / 写数据 / `ensure_hnsw_index` / vector_search
- `sqlite_store.rs`：
  - `sources` 表（`source_path` UNIQUE + `source_hash` 幂等）
  - `chunks` 表（`(source_id, chunk_ordinal)` UNIQUE，含 `text` 列供 FTS5 索引）
  - `chunks_fts` FTS5 虚拟表（`unicode61` tokenizer，BM25 排序）
  - `search_fts(query, limit)`：把用户问题转为 OR 查询（`build_fts5_query` 提取拉丁/数字 token + 中文单字，OR 连接），BM25 排名
  - `rebuild_fts()`：清空 FTS5 后从 `chunks.text` 重新填充
  - `messages` 表（`session_id` + `ordinal`，多轮聊天用）
  - `append_message` / `load_recent_messages(session, limit)` / `clear_session` / `session_message_count`

对外只暴露具体方法（不暴露 `rusqlite::Connection` / `lancedb::Table`）。

### 4.7 `chunker.rs`

按 `\n\n` 切段（段落级），每段超 `CHUNK_SIZE` 字符按 `CHUNK_SIZE` 滑窗切，重叠 `CHUNK_OVERLAP` 字符。输出 `Vec<Chunk>`。

### 4.8 `tray.rs`

M11 系统托盘核心（`lorag tray`）：axum server + 托盘图标常驻，浏览器自动打开。

- `run_tray_loop(port, shutdown_tx)`：构建托盘图标（`include_bytes!("../assets/icon.png")` → `image` 解码 RGBA）+ 菜单（`Open Web UI` / `Quit`），阻塞 main thread 跑 `tray_icon` 事件循环；`Quit` → oneshot 通知 axum `with_graceful_shutdown`（5 秒超时强退）
- `open_browser(url)`：跨平台 `std::process::Command`（Windows `cmd /C start "" url` / macOS `open` / Linux `xdg-open`），**不**引入 webbrowser crate。GUI 复用
- `menu_id_to_command(id)`：菜单 id → `TrayCommand` 纯函数（单元测试覆盖）
- **Windows message pump**：tray-icon 0.19 在 Windows 要求创建线程显式 pump Win32 message queue（故 `windows-sys` 是直接依赖），否则菜单点击事件永不触发
- **平台状态**：Windows 已验证；macOS 需 `tray_icon::platform::macos::init_ns_app()`（后续）；Linux 未验证

### 4.9 `gui/`（M12 GPUI 桌面启动器，`lorag-gui`）

M12 第四个前端（CLI / server / tray / gui），通过 `gui` feature flag 隔离（默认 OFF，`cargo build` 不拉 GPUI 依赖）。

**目的**：办公小白双击 `lorag-gui.exe` 就能用 —— 7 张页面 sidebar 启动器覆盖"服务启停 / 模型下载 / 文档摄入 / 健康检查 / 日志 / .env 设置 / 关于"全流程；聊天走"打开聊天"按钮 → 浏览器开 `localhost:port`（复用 M10 Web UI）。

**关键约束**：

- 依赖 `tray::open_browser`（跨平台开浏览器，复用 M11 逻辑）、`server::start_with_shutdown`、`aha_provider`、`config`、`store::sqlite_store`，**不**直接 `use aha::*` / `use rig_compat::*` / 碰 `chunker`
- aha candle 推理、`std::fs`、`rfd::FileDialog`（原生 modal loop 阻塞）、`std::process::Command` 一律放 tokio `spawn_blocking`（tokio runtime 在 GUI 启动时建一次，整个进程复用），**绝不能上 GPUI UI thread**
- 配置单一来源：设置页改完写回 `.env`（`AppConfig::save_to_dotenv()` 原子写 `.tmp`→rename），不引入 GUI 专属配置文件；重启服务才重新读 cfg
- 关闭窗口不退出：`on_window_should_close` 返回 false + `window.minimize_window()` 最小化到托盘；托盘双击 = ShowWindow，托盘 Quit = `cx.quit()` + 服务 on_app_will_quit 关 axum

**子模块**：

- `app.rs`：`AppState` gpui Entity（持有 tokio runtime handle、axum shutdown sender、broadcast log sender (`VecDeque<String>` 上限 5000 行)、当前 `Page`、各子页 state）
- `gpu_probe.rs` + `fallback_dialog.rs`：启动时 blade GPU probe；失败用 `windows-sys MessageBoxW` 弹友好对话框后 `exit(1)`（gpui 本身起不来时兜底）
- `logging.rs`：`tracing::Layer` 桥接层，把 tracing event 格式化后广播到 `tokio::sync::broadcast::Sender<String>`（容量 256）；GUI 日志页持 Receiver 追加显示；同时 `tracing-appender` 写 `%APPDATA%/lorag/logs/lorag.log.YYYY-MM-DD`（daily 滚动、保留 7 天）
- `tray_host.rs`：独立 OS thread 跑 tray-icon 0.19 事件循环 + Win32 pump（避免跟 GPUI smol executor 抢线程）；菜单 Show Window / Open Web UI / Quit；`std::sync::mpsc::Sender<TrayUiCommand>` → GPUI 前台 `cx.spawn` + `tokio::spawn_blocking` 桥接（`AsyncApp` is `!Send`）
- 7 页：
  - `service.rs`：4 状态机 Stopped/Starting/Running/Stopping + oneshot 关断 + 5s 超时；port=3000 暂硬编码
  - `models.rs`：LLM/Embedding/Rerank 三行 + download spinner + refresh；rerank 空优雅降级
  - `ingest.rs`：rfd 选文件/文件夹 + per-entry 状态机 + SqliteStore list_sources；每次新起 `init_embed_only` 客户端 Case B 策略
  - `doctor.rs`：`spawn_blocking` 跑 `doctor::run_checks`，3 列 grid + PASS/WARN/FAIL 汇总横幅
  - `logs.rs`：ScrollHandle + 自动滚到底 epsilon 判定 + level 下拉着色 + 打开文件夹/导出
  - `settings.rs`：5 组表单 17 字段 + HYBRID 开关 + 原子 save_to_dotenv + "需重启"横幅
  - `about.rs`：版本/技术栈/链接，按钮→`tray::open_browser` 或开文件夹
- `root_view.rs` + `sidebar.rs`：gpui-component `Sidebar` + `SidebarMenu` 7 项导航，主区分发当前页；`#[allow(clippy::too_many_arguments)]` 压 8 参数分发（7 页 + 页本体）
- `pages/`：`mod.rs` 定义 `Page` enum + 各页占位重导出（历史遗留，主体逻辑已提升到 `src/gui/*.rs`）
- `autostart.rs`：G13 开机自启实现（`enable` / `is_enabled` / `disable`），**未接进设置页**（等 G13 收尾）

**pin 版本**：`gpui` / `gpui_platform` = `{ git = "https://github.com/zed-industries/zed" }`（Cargo 用 git URL 统一 rev）；`gpui-component` / `gpui-component-assets` = `{ git = "https://github.com/longbridge/gpui-component", rev = "57a9903f48160845aabc8b92a1e2f5348c80d439" }`；`rfd = "0.15"`；`tracing-appender = "0.2"`。全部 `optional = true` behind `gui` feature。

### 4.10 `server.rs`（M10 axum HTTP server）

axum 0.8 + rust-embed 嵌入前端。路由：

- `POST /api/chat`：SSE 流式多轮对话（复用 M8 `llm_complete_stream`）
- `POST /api/query`：SSE RAG
- `GET /api/status`：系统信息（模型、文档数、chunk 数）
- `GET /api/sessions`：对话历史列表
- `DELETE /api/sessions/{id}`：删除会话
- `GET /*`：嵌入式前端（`rust-embed` 打包 `web/dist/` 到二进制）

依赖：`aha_provider`、`config`、`rag`、`store::sqlite_store`，不直接调 lancedb。`start_with_shutdown(port, shutdown_signal)` 是 tray / GUI 共用入口。

---

## 5. 当前限制

1. **单进程内存叠加**：4B LLM (~8GB FP16) + 0.6B Embedding (~1.5GB) + 可选 Rerank (~1.5GB) ≈ 10–12GB RAM。换小模型可降（0.6B LLM ~1.2GB + 0.6B Embedding ~1.5GB ≈ 3GB）
2. **CUDA 编译陷阱**：`cargo build`（无 flag）会盖掉 CUDA 二进制。改完代码后**必须**用 `cargo build --features cuda` 保住 GPU 加速（CPU 二进制仍能跑，但 4B 会从 1–3s 退化到 15–30s/query）。详见 [doc/install.md](doc/install.md) §CUDA 陷阱
3. **纯向量检索**：关键词召回相对弱。SQLite FTS5 BM25 混合检索已在 M9 实装（`HYBRID_ENABLED=true` 启用），但小数据集下效果不明显 —— 向量检索已覆盖大部分文档。大文档量（100+ 文件、1000+ chunk）时 BM25 可互补召回精确关键词（人名、日期、编号）。目前默认关闭（opt-in）
4. **PDF 扫描版无效**：`pdf-extract` 只读文本层；扫描版得 OCR（aha 本身有 OCR 模型，未实装）
5. **xlsx 多 sheet 行前缀**：多 sheet 时每行加 `[SheetName]` 前缀（M8 修复），但仍不保留表结构 / 公式
6. **同步摄入**：超大文件（>100MB）可能 OOM；后续可改流式
7. **rerank hard case 未验证**：generic 14/17 测试 rerank on/off 都 14/17，无质量差异；rerank 价值预期在 hard case（top-5 召回错但 top-50 里有），待真业务问题验证
8. **Windows 文件锁**：Zed 编辑器打开时 rust-analyzer 会锁 `data/lorag.db`，关闭 Zed 才能 `lorag reindex` 删库
9. **防注入仅 RAG 模式生效**：`sanitize_user_input` + chunk 边界包裹只在 RAG 模式（`--no-rag` 关闭时）启用。`--no-rag` 走裸 LLM，无上下文隔离，理论上 prompt injection 风险更高 —— 故意保留，因为这是用户"绕开 RAG 聊纯 LLM"的本意
10. **G13 开机自启未接设置页**：`gui::autostart` 模块已实装但还没 GUI 入口；CLI 用户可直接调 `lorag::gui::autostart::{enable,is_enabled,disable}`
11. **macOS / Linux 桌面 GUI 打包未验证**：Windows MSI 已端到端验证；macOS / Linux 待跟进