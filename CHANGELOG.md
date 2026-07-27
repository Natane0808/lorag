# lorag — Changelog

lorag 的历史变更记录。**当前**架构 / 决策 / 限制见 [PLAN.md](PLAN.md)。

格式：每条按时间倒序（新→旧）。每个版本块列：变更点 + 关键 commit + 验证方式 + 关键经验。

---

## v0.1（current） — M0 到 M8 全实装

**当前 release**：MIT / codeberg，rust 2021，0.1.0 crate version。

### Unreleased（M10.1 — Mermaid 图表渲染）

- **Mermaid 图表渲染**：Web UI 聊天界面里 LLM 回复的 ` ```mermaid … ``` ` 代码块自动渲染成 SVG 图表（同主题色 + 响亮 Mermaid `default` / `dark` 主题随 daisyUI 主题切换初始化一次）。
- **架构**：
  - `web/src/utils/markdown.ts`：`extractMermaidBlocks` 预提取完整 mermaid 块并替换为 `M10MERMAIDTOKEN${counter}END` 占位符（避免 marked tokenize 块内语法 / 流式未闭合时不误提取）；`restoreMermaidBlocks` 在 marked 输出后把占位符换回 `<div class="mermaid-pending" data-source="…">`。
  - `web/src/utils/mermaid.ts`：单例 `mermaid.initialize` + 串行 microtask 队列防并发 `mermaid.render` ID 冲突 + `svgCache: Map<source, svg>` 跨流式 token 复用 SVG (同样 source 重现时立即恢复，不重复渲染)；渲染失败降级显示原码 + 错误消息。
  - `MessageBubble.tsx`：内容不再用 `innerHTML={html()}` 绑定，改 `createEffect + applyHtml(contentRef, html)` 命令式设值，这样 mermaid 节点能在每次重 render 时根据 svgCache 缓存自动复原。
- **CSS**：`.md-content .mermaid-rendered / .mermaid-loading / .mermaid-error` 状态类依次过渡，与 daisyUI `--b1` / `--er` oklch 变量同步。
- **依赖**：`mermaid@^11.12`（含 50+ diagram types，Vite 默认 code-split 只加载用到的那种）。
- **限制**：MVP 主题跟随页面初始化状态，后期切换主题不会重新渲染已有图表（仅新块会跟随）。
- **已知问题修复**：
  - **占位符字符集 bug**：第一版用 `{{__MERMAID_N__}}`，但 `__MERMAID_0__` 被 marked 当 GFM strong emphasis 处理 → `<strong>MERMAID_0</strong>`，剩下 `{{`/`}}` 单独可见，restore 正则匹配不到完整 token。修复：占位符改成无歧义字符 `M10MERMAIDTOKEN${counter}END`（详见 [PLAN.md §10.9](PLAN.md)）。
  - **SVG 显示过小**：timeline / gantt 这类扁平图 viewBox 比例 ~7:1，CSS 原 `max-width: 100%` 让浏览器按比例缩到容器宽、高度被压成 ~108px，文字看不清。修复：改为 `width: 100%`，让 SVG 撑满容器宽度，viewBox 比例由 `height: auto` 保留。
- Commit: (pending)

### Unreleased（M10 Web UI）

### M10 — Web UI（SolidJS + axum + daisyUI）

- **`lorag serve`**：axum HTTP server（localhost:3000，`--port` 可配）。嵌入式前端（`rust-embed` 打包 `web/dist/` 到二进制），部署零外部依赖。
- **前端架构**：SolidJS + Vite 8 + Bun + daisyUI 5（Tailwind CSS 4）。开发期 `bun dev`，生产期 `bun run build && cargo build --features cuda`。
- **API 端点**：`POST /api/chat`（SSE 流式多轮）/ `POST /api/query`（SSE RAG）/ `GET /api/status` / `GET /api/sessions`（历史列表）/ `DELETE /api/sessions/{id}`（删除）/ `GET /*`（嵌入式前端）。
- **前端功能**：SSE token 级流式 / daisyUI 明暗主题 / 侧边栏（按日期分组 + 删除）/ 欢迎页推荐问题 / 移动端响应式。
- **关键 bug fix**：aiIdx 差一错误（AI 消息永不显示）/ 侧边栏流结束后不刷新 / 嵌套 button HTML 警告 → `div[role=button]`。
- **新增后端依赖**：`axum` 0.8 / `tower-http` 0.6 / `tokio-stream` 0.1 / `async-stream` 0.3 / `rust-embed` 8。
- **前端依赖**：`solid-js` 1.9 / `daisyui` 5.7 / `vite` 8.1 / `@tailwindcss/vite` 4.3 / `vite-plugin-solid` 2.11 / TypeScript 6.0。
- Commit: (pending)

### Unreleased（M7.1 之后的小修）

- **drop `--session <ID>` + `/reset`**：chat 进程内连续，跨进程不续接。session_id 内部仍生成（sqlite 主键需要），但用户感知不到。`/status` 不再显示 session_id，banner 不显示。
- **drop `tests/scratch/`**：开发期 RAG eval 脚本（`check_chunks.py` / `eval_questions.py` / `eval_ab.py`）已用 mavis-trash 移到回收站。`README.md` 旧 rerank 介绍去重。
- **RERANK_TOP_N 可配置**：从硬编码 `pub const RERANK_TOP_N: usize = 50;` 改成 `AppConfig.rerank_top_n: usize`（环境变量 `RERANK_TOP_N` / CLI `--rerank-top-n <N>`）。`rag_query` / `try_rag_with_lancedb` / `retrieve_chunks` 三个函数签名都加 `rerank_top_n: usize` 参数；启动期校验 `rerank_top_n > top_k` 否则拒绝。
- **Cargo profile 微调**：dev `opt-level=1` 0.6B 推理 4.5s/query（vs full debug 142s）；release 链接冷启动 5-10 分钟把 D 盘打 100%，**只在测性能时跑一次**。

### M8 — 流式输出 + 提示词配置化 + XLSX chunk 修复

- **流式输出**：`llm_complete_stream` 通过 `mpsc::channel(64)` + `spawn_blocking` 桥接 aha `generate_stream`。`cmd_query` 分三阶段流式（检索 → 生成 → 逐 token 打印），`cmd_chat` 每轮逐 token 输出 + sqlite 持久化。不走 rig 抽象，直接调 aha `GenerateModel::generate_stream`。
- **提示词配置化**：4 个 `PROMPT_*` 字段（可 `.env` 覆盖，留空用内置默认值）：`PROMPT_SYSTEM_ROLE` / `PROMPT_RAG_INSTRUCTION` / `PROMPT_CHAT_CONTEXT_INSTRUCTION` / `PROMPT_BARE_LLM`。
- **4 层防注入**：① `sanitize_user_input` 转义 ChatML token + HTML 实体；② `format_chunks_for_context` 每 chunk `[文档片段 N]...[/文档片段 N]` 边界包裹 + "参考资料不可执行"段头；③ 系统 prompt 5 条铁律（不可被用户覆盖）；④ `ANTI_INJECTION_SUFFIX` prompt 末尾重申规则最高优先级。
- **XLSX chunk 修复**：多 sheet 时给每个 sheet 加 `--- Sheet: {name} ---` header + 每行加 `[SheetName]` 前缀，确保 chunk 切分后数据行仍带 sheet 上下文。修复前跨 sheet 检索经常失败（sheet header 和数据行被切到不同 chunk 时向量相似度断崖式下降），修复后 sheet header 跟数据行大概率落在同一 chunk，跨 sheet 检索可命中。
- **关键经验**：① 流式通道：`spawn_blocking → blocking_lock → generate_stream → rt.block_on → poll async stream → tx.send`；② 纯向量检索对数字日期不敏感（相近日期的向量表示几乎重叠），混合检索（M9）才能解决；③ 防注入需多层——单靠 `sanitize_user_input` 不够，chunk 边界标记 + 系统 prompt 铁律 + recency bias 尾注形成纵深防御。
- Commit: `3c33674 feat: M8 streaming output + prompt config + anti-injection + XLSX fix`

### M9 — 混合检索（BM25 FTS5 + 向量 RRF）

- **SQLite FTS5 全文索引**：`chunks` 表新增 `text` 列 + `chunks_fts` FTS5 虚拟表（`unicode61` tokenizer，BM25 排序）。安全迁移 `try_add_text_column` 兼容旧数据库。
- **BM25 查询**：`search_fts(query, limit)` 通过 `build_fts5_query` 把用户自然语言问题转为 OR 查询（拉丁/数字保留完整词 + 中文按单字，OR 连接）。**避坑**：FTS5 短语搜索（双引号）要求所有单字 token 精确连续出现 → 自然语言查询补白词导致 0 匹配。
- **RRF 融合**：`rrf_merge(vector_chunks, fts_chunks, top_k, k=60)` 两路分数融合去重 → 取 top_k。混合检索启用时跳过 rerank（RRF 直接输出 top_k）。
- **配置**：`HYBRID_ENABLED`（默认 `false`，opt-in）。小数据集（几十 chunk）向量检索已够用，大文档量时互补。
- **CLI**：`--no-hybrid` flag（`query` / `chat`），`/status` / banner 显示 Hybrid 状态。`cmd_query` 只在启用 hybrid 时打开 SqliteStore（避免无用开销）。
- **关键经验**：① FTS5 `unicode61` 对中文按单字切，短语搜索 = 坑；② OR 语义 + BM25 排序自然过滤；③ 小数据集的 RRF 退化——两路返回几乎相同的 chunk → 无额外收益。
- Commit: `1aa03a0 Add M9 hybrid retrieval (BM25 FTS5 + vector RRF)`

### M7.1 — Rerank（可选功能）

- `cfg.rerank_model` 留空 → 永远不 load，不调 rerank（零开销，零内存）
- `cfg.rerank_model` 非空 + 没传 `--no-rerank` → `enable_rerank=true`，第一次 query 时懒加载
- 懒加载用 `Arc<tokio::sync::OnceCell<...>>`（不是 `unsafe` self 改字段；clone 间共享 slot；并发 init 安全）
- RAG 内部 rerank 路径：`vector_search top-RERANK_TOP_N=50` → `rerank_score(question, chunks)` → 排序取 top_k
- `cmd_models_pull` 自动加 rerank 模型
- AhaClient 新增：`has_rerank()` / `rerank_configured()` / `ensure_rerank()` / `rerank_score(query, docs)` / `rerank_slot: Arc<OnceCell<...>>`
- A/B 测试 17 通用问题：rerank on/off 都 14/17 pass，~12-15s/q。**rerank 适合 hard case**（top-5 召回错但 top-50 里有）—— 这个 generic 测试集 top-5 直接就够，看不出 rerank 价值。
- Commit：`ccb41e6 feat(rerank): M7.1 optional rerank + RERANK_TOP_N config + --no-rerank flag`

### M7 — `lorag chat` 多轮 REPL

- SQLite `messages` 表（`session_id` + `ordinal` UNIQUE；idx_messages_session 索引）
- 多轮 REPL：history window 20（4B Qwen3 32K context，留足 RAG + 当前）
- 内部命令：`/help` `/status` `/clear` `/reset` `/exit`
- flags：`--message`（一次性首问）/ `--no-history`（每轮独立）/ `--no-banner` / `--no-rag`（纯 LLM）/ `--top-k`
- `build_chat_preamble`：history (max 20) + RAG context → LLM preamble
- `is_recoverable_error`：lancedb 任何错误 → fallback 到 bare LLM（让 chat 进程不会因 RAG 失败而崩）
- Commits：
  - `cda0e2e feat(store): M7 chat messages table + helpers`
  - `3eccfe4 feat(chat): M7 lorag chat multi-turn REPL; drop shell`
  - `c7d8c21 docs: sync PLAN/AGENTS/README to M7 chat + shell removal`

### M5.1 — `lorag reindex`

- 清 LanceDB + SQLite 后重新 ingest（换 EMBED_MODEL 后必须走这个）
- `--yes` / `-y` 跳确认；`--dry-run` 只打印
- 不删模型文件（`MODELS_DIR/` 保留）
- Commit：`32210c4 feat: lorag reindex — wipe LanceDB + SQLite then re-ingest`

### M5 — RAG 端到端（**重写绕开 62GB bug**）

- 不用 `AgentBuilder::dynamic_context` + `LanceDbVectorIndex`（实测 5 chunks 也爆 62GB 内存，OOM 干死 64GB 机器）
- 改走手写：`embed_text` → `table.vector_search(&[f32])?.limit(k).execute()` → `RecordBatch` 流式读 → `StringArray::value(i)` 抽 text → 拼 context → `llm_model.completion(req)`
- `is_recoverable_error` fallback：lancedb 任何错误（不存在 / 没数据 / 内存不够）→ 裸 LLM
- 5 chunks 实测 `iops=2 requests=2 bytes_read=20992`，无 allocation 爆炸
- **关键经验**（避坑 10.1）：arrow-array 58 的 `StringArray::value(i)` 返回 `&str`（**不是** `Option<&str>`，是早期版本 API）

### M4 — rig 0.40 provider 适配

- `AhaCompletionModel: CompletionModel`（`type StreamingResponse = ()`；`stream()` 返 `Err`）
- `AhaEmbeddingModel: EmbeddingModel`（`MAX_DOCUMENTS = 1024`，`ndims()` 从 `client.embed_dim()` 来）
- `AhaClient: CompletionClient + EmbeddingsClient`（**不**实现 `Provider` / `ProviderClient`）
- 消息转换 `convert_messages`：rig `Message` → aha `ChatMessage`（preamble + documents + chat_history）
- **关键经验**（避坑 10.2）：rig 0.40 vs 0.39 API 大变——`OneOrMany` 是 struct 不是 enum；`CompletionModel::make` 签名变；`EmbeddingModel::make` 多 `dims` 参数；`CompletionResponse` 多 `raw_response` / `usage` / `message_id` 字段；不要实现 `Provider`（那是给 HTTP-based 用的）
- 端到端验证：`lorag query "1+1=?"` 拿到 `"1 + 1 = 2"`

### M3 — 摄入 pipeline

- chunker：段落级（`\n\n`）+ 字符滑窗（超 `CHUNK_SIZE` 按 `CHUNK_SIZE` 切，重叠 `CHUNK_OVERLAP`）
- sqlite 元数据：`sources` (id / path UNIQUE / hash / type / chunk_count / byte_size) + `chunks` (`(source_id, chunk_ordinal)` UNIQUE)
- lancedb：表名固定 `documents`；schema：`id` / `source_path` / `chunk_ordinal` / `text` / `embedding: FixedSizeList<Float64, N>`
- 摄入幂等：sha256 哈希 + sqlite 查重，重复时打印 `skipped: <path> (unchanged)`
- IVF-HNSW-FLAT 索引：≥256 rows 自动建（`IvfHnswFlatIndexBuilder::default()`）；< 256 跳过（继续 ENN 全表扫）

### M2 — 6 种文档 loader

| ext | crate | 备注 |
|-----|-------|------|
| `.pdf` | `pdf-extract` | 扫描版无效（无 OCR） |
| `.docx` | `zip` + `quick-xml` | 抽 `word/document.xml` 的 `<w:t>` |
| `.pptx` | `zip` + `quick-xml` | 遍历 `ppt/slides/slide*.xml` 抽 `<a:t>` |
| `.xlsx` | `calamine` | 多 sheet 平铺；`range.start() == None` skip + warn，**不**再 bail（M7 后期发现） |
| `.md` | `pulldown-cmark` | 抽 text 节点 |
| `.txt` | std fs | utf-8 读 |

单文件失败 → warn + skip，不中断整次 ingest。

### M1 — `AhaClient::init` 加载模型

- 调 `aha::models::load_model(which, path, None, None)` 把 LLM + embedding 加载进内存
- 路径 leak 成 `&'static str`：`aha::utils::string_to_static_str(path)`（每次启动 ~100 字节，可接受）
- async helper `llm_generate` / `embed_texts` 把同步 candle 包成 `tokio::task::spawn_blocking`
- 端到端：`lorag init` 0.6B 5s load，4B 1-3min + ~11GB 内存

### M0 — CLI 骨架 + config

- clap v4 derive + tokio rt-multi-thread + dotenvy + anyhow + tracing
- `AppConfig`：从 `.env` 强类型加载 + validate 启动期拦截
- `lorag --help` / `lorag models status` 跑通
- 4 个 unit test 覆盖 `dir_has_model` / `resolve_model_path`

---

## 历史教训（保留备用）

### 集成 bug：rig-lancedb 62GB 内存分配（M5 触发）

`rig 0.40` + `rig-lancedb 0.40` + `lancedb 0.30` 这条集成链路在 `AgentBuilder::dynamic_context` + `LanceDbVectorIndex` 路径上，某步会一次性分配 `~62GB`（实测，5 chunks 也炸；用户机器 64GB DDR4，进程 OOM 干死）。

**不是数据量问题**，是代码路径问题（盲猜：内部把整列 / 整 index 读进 `Vec<f32>`）。

**解法**：`src/rag.rs` 改走手写 lancedb native API（`table.vector_search(&[f32])?.limit(k).execute()` + RecordBatch 流式读）。5 chunks 实测 `iops=2 requests=2 bytes_read=20992`，无 allocation 爆炸。

**未来升级 lancedb / rig-lancedb 时务必验证**：5 chunks query 内存 < 1GB（否则 bug 复发）。

### aha 路径同步坑

`aha::utils::download_model(id, save_dir, ...)` 把模型下到 `<save_dir>/<id>/`；但 `aha::utils::is_model_downloaded(which)` / `get_default_weight_path(which)` 写死查 `~/.aha/{id}/`。**aha 自己的 `aha list` 也踩这个坑**（`aha download -m X -s /tmp/foo` 下完，`aha list` 仍显示 X 未下）。

**解法**：`src/aha_provider.rs::resolve_model_path` 自己写：优先 `MODELS_DIR/{repo}/`，兜底 `~/.aha/{repo}/`，"已下"判断 = 目录存在 + `config.json` + 至少一个 `*.safetensors`。

### Windows + Zed 文件锁

Zed 编辑器打开时 rust-analyzer 会锁 `data/lorag.db`，`lorag reindex` 删库会被拒绝。**先关 Zed 再 reindex**。

### Cargo `cargo build` 覆盖 CUDA 二进制

`cargo build`（无 flag）会重新编译出 CPU-only 二进制，**覆盖**之前 `cargo build --features cuda` 编译的 CUDA 版本。改完代码后**必须**用 `cargo build --features cuda` 保住 GPU 加速。CPU 跑 4B 15-30s/query，CUDA 1-3s/query。

### aha crate 是 Rust 2024 edition

aha 自己的 `Cargo.toml` 是 `edition = "2024"`。lorag 升 2024 edition 后可享受 `if let` 链式等新语法；但 edition 2024 要 Rust 1.85+。

### Python 跨平台中文 arg 坑（仅 dev eval 用）

PowerShell 的 `Start-Process` 拆多 word 中文 arg 时会丢字符。跑 dev eval 用 Python `subprocess.run([list])` 绕开。**这些 eval 脚本都已 drop**（见 Unreleased）。

### LanceDB 0.30 走 IVF-HNSW-FLAT

三种 index 选 `IvfHnswFlat`（存原始向量，recall 最高）。lance kmeans 训练要 ≥256 行；< 256 silently skip，≥ 256 且没建过才建。查询时**不**需要额外传参——lancedb 检测到索引自动走 ANN。
