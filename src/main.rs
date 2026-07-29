//! `lorag` CLI 入口。
//!
//! 只做 CLI 解析 + 命令分派；业务逻辑放 `src/<module>.rs`。
//! 错误用 `anyhow` 打到 stderr，exit code 1。

use std::io::Write as _;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use lorag::aha_provider;
use lorag::config;

/// M7 chat: 每轮拼回 LLM context 的历史消息条数上限。
///
/// 20 条 ≈ 10 轮 user/assistant 交替。Qwen3-4B context window 32K，20 条平均
/// 每条 200 字 → 约 4K tokens，留足 RAG context + 当前问题的空间。
const MAX_HISTORY_MESSAGES: usize = 20;

/// 生成一个 chat session id（`chat-YYYYMMDDTHHMMSS-<n>`）。
///
/// 进程内自增 counter 作后缀，**单进程内唯一**。多进程不冲突靠时间戳分辨率。
fn generate_session_id() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let now = chrono::Utc::now().format("%Y%m%dT%H%M%S");
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("chat-{now}-{n}")
}

#[derive(Debug, Parser)]
#[command(
    name = "lorag",
    version,
    about = "Fully local Agent RAG CLI (aha + rig + LanceDB)",
    long_about = "Ingest multi-format documents into LanceDB + SQLite, then ask one-shot RAG questions.\n\
                  All inference runs in-process via the aha Rust crate — no HTTP server, no cloud."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// 把文件 / 目录摄入 LanceDB + SQLite
    Ingest {
        /// 一个或多个文件 / 目录
        #[arg(required = true)]
        paths: Vec<PathBuf>,

        /// 限定扩展名（逗号分隔），默认全开
        #[arg(long, value_delimiter = ',', default_values_t = vec![
            "pdf".to_string(),
            "docx".to_string(),
            "pptx".to_string(),
            "xlsx".to_string(),
            "md".to_string(),
            "txt".to_string(),
        ])]
        ext: Vec<String>,

        /// 强制重摄入（无视 hash）
        #[arg(long)]
        force: bool,

        /// 目录是否递归
        #[arg(long, default_value_t = true)]
        recursive: bool,
    },

    /// 一次性 RAG 问答
    Query {
        /// 提问内容
        question: String,

        /// 检索 top_k
        #[arg(long)]
        top_k: Option<usize>,

        /// 跳过 rerank（即使 .env 配了 RERANK_MODEL）
        #[arg(long)]
        no_rerank: bool,

        /// rerank 粗筛条数（覆盖 .env 的 `RERANK_TOP_N`）。**必须** > `top_k`。
        #[arg(long)]
        rerank_top_n: Option<usize>,

        /// 关闭混合检索（即使 .env 配了 HYBRID_ENABLED=true）
        #[arg(long)]
        no_hybrid: bool,
    },

    /// 模型管理
    Models {
        #[command(subcommand)]
        action: ModelsAction,
    },

    /// 把模型加载到内存（等价于 `models status --init`）
    Init {
        /// 加载后立即打印一些 sanity check 信息
        #[arg(long)]
        verbose: bool,
    },

    /// 列出已摄入文件
    Sources {
        #[command(subcommand)]
        action: SourcesAction,
    },

    /// 多轮对话 REPL（M7 实装：带历史 + RAG + SQLite 持久化）
    Chat {
        /// 第一轮的问题（非交互式跑一次就退）
        #[arg(long, short)]
        message: Option<String>,

        /// 不带历史（每轮独立）
        #[arg(long)]
        no_history: bool,

        /// 不显示欢迎 banner
        #[arg(long)]
        no_banner: bool,

        /// 跳过 LanceDB 检索（纯 LLM 对话）
        #[arg(long)]
        no_rag: bool,

        /// 跳过 rerank（即使 .env 配了 RERANK_MODEL）
        #[arg(long)]
        no_rerank: bool,

        /// 检索 top_k
        #[arg(long)]
        top_k: Option<usize>,

        /// rerank 粗筛条数（覆盖 .env 的 `RERANK_TOP_N`）。**必须** > `top_k`。
        #[arg(long)]
        rerank_top_n: Option<usize>,

        /// 关闭混合检索（即使 .env 配了 HYBRID_ENABLED=true）
        #[arg(long)]
        no_hybrid: bool,
    },

    /// 诊断环境：检查 .env / 模型文件 / 存储路径 / 编译 feature
    Doctor,

    /// 启动 Web UI 服务器（SolidJS + axum，M10）
    Serve {
        /// 监听端口（默认 3000）
        #[arg(long, default_value_t = 3000)]
        port: u16,
    },

    /// 启动 Web UI + 系统托盘图标（办公用户友好模式，M11）
    Tray {
        /// 监听端口（默认 3000）
        #[arg(long, default_value_t = 3000)]
        port: u16,
    },

    /// 清掉 LanceDB + SQLite 后重新摄入（换 embedding 模型后必须走这个）
    Reindex {
        /// 一个或多个文件 / 目录
        #[arg(required = true)]
        paths: Vec<PathBuf>,

        /// 限定扩展名（逗号分隔），默认全开
        #[arg(long, value_delimiter = ',', default_values_t = vec![
            "pdf".to_string(),
            "docx".to_string(),
            "pptx".to_string(),
            "xlsx".to_string(),
            "md".to_string(),
            "txt".to_string(),
        ])]
        ext: Vec<String>,

        /// 目录是否递归
        #[arg(long, default_value_t = true)]
        recursive: bool,

        /// 跳过 interactive 确认
        #[arg(long, short = 'y')]
        yes: bool,

        /// 只打印会做什么，不真删不真 ingest
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(Debug, Subcommand)]
enum ModelsAction {
    /// 下载 LLM + Embedding 模型到 MODELS_DIR
    Pull,
    /// 打印模型文件存在性
    Status {
        /// 真正调用 AhaClient::init 把模型加载到内存（很慢）
        #[arg(long)]
        r#init: bool,
    },
}

#[derive(Debug, Subcommand)]
enum SourcesAction {
    /// 列出已摄入文件
    List {
        /// 输出 JSON
        #[arg(long)]
        json: bool,
    },
}

fn main() -> ExitCode {
    // 先加载 .env，让 LOG_LEVEL 在 tracing init 时可用
    let _ = dotenvy::dotenv();

    // 默认静默 lance / lancedb / datafusion / arrow 的 INFO 日志。
    // RUST_LOG > LOG_LEVEL > 默认 `info`。lance silencing 是必加后缀。
    // env_filter target 段是字面量（不支持 glob），必须显式列全。
    let lance_silence = ",lance::dataset_events=warn,lance::execution=warn,lance::io_events=warn,\
lance::file_audit=warn,lancedb=warn,datafusion=warn,arrow=warn";
    let base = std::env::var("RUST_LOG")
        .or_else(|_| std::env::var("LOG_LEVEL"))
        .unwrap_or_else(|_| "info".to_string());
    let full_filter = format!("{base}{lance_silence}");
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(&full_filter)),
        )
        .init();

    let cli = Cli::parse();
    match run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> Result<()> {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("failed to build tokio runtime")?;
    rt.block_on(async move {
        // M0 阶段不调 AhaClient：只加载 config
        let cfg = config::load().context("failed to load config")?;

        match cli.command {
            Command::Ingest {
                paths,
                ext,
                force,
                recursive,
            } => cmd_ingest(&cfg, paths, ext, force, recursive).await,
            Command::Query {
                question,
                top_k,
                no_rerank,
                rerank_top_n,
                no_hybrid,
            } => cmd_query(&cfg, question, top_k, no_rerank, rerank_top_n, no_hybrid).await,
            Command::Models { action } => match action {
                ModelsAction::Pull => cmd_models_pull(&cfg).await,
                ModelsAction::Status { r#init } => cmd_models_status(&cfg, r#init).await,
            },
            Command::Init { verbose } => cmd_init(&cfg, verbose).await,
            Command::Sources { action } => match action {
                SourcesAction::List { json } => cmd_sources_list(&cfg, json).await,
            },
            Command::Chat {
                message,
                no_history,
                no_banner,
                no_rag,
                no_rerank,
                top_k,
                rerank_top_n,
                no_hybrid,
            } => {
                cmd_chat(
                    &cfg,
                    message,
                    no_history,
                    no_banner,
                    no_rag,
                    no_rerank,
                    top_k,
                    rerank_top_n,
                    no_hybrid,
                )
                .await
            }
            Command::Doctor => cmd_doctor(&cfg),
            Command::Serve { port } => cmd_serve(&cfg, port).await,
            Command::Tray { port } => cmd_tray(&cfg, port).await,
            Command::Reindex {
                paths,
                ext,
                recursive,
                yes,
                dry_run,
            } => cmd_reindex(&cfg, paths, ext, recursive, yes, dry_run).await,
        }
    })
}

// =============================================================================
// 命令实现
// =============================================================================

async fn cmd_ingest(
    cfg: &config::AppConfig,
    paths: Vec<PathBuf>,
    ext: Vec<String>,
    force: bool,
    recursive: bool,
) -> Result<()> {
    use lorag::aha_provider::AhaClient;
    use lorag::ingest::pipeline;

    // ingest 只需 embedding 模型，跳过 LLM 省 ~8GB 内存 + 数十秒 load 时间。
    println!("loading embedding model for ingest (skipping LLM to save memory + time)...");
    let client = AhaClient::init_embed_only(cfg.clone())
        .await
        .context("failed to init AhaClient for ingest")?;

    let counts = pipeline::run_ingest(&client, cfg, &paths, &ext, force, recursive)
        .await
        .context("ingest pipeline failed")?;

    println!();
    println!(
        "done. ok={} skipped={} failed={}",
        counts.ok, counts.skipped, counts.failed
    );
    Ok(())
}

async fn cmd_query(
    cfg: &config::AppConfig,
    question: String,
    top_k: Option<usize>,
    no_rerank: bool,
    rerank_top_n: Option<usize>,
    no_hybrid: bool,
) -> Result<()> {
    use lorag::aha_provider::AhaClient;
    use lorag::rag;
    use lorag::store::sqlite_store::SqliteStore;

    let k = top_k.unwrap_or(cfg.top_k);
    let rn = rerank_top_n.unwrap_or(cfg.rerank_top_n);
    let enable_rerank = effective_rerank_enabled(cfg, no_rerank);

    if enable_rerank && rn <= k {
        anyhow::bail!(
            "--rerank-top-n ({rn}) must be > --top-k ({k}); rerank needs more candidates than the final count"
        );
    }

    println!("loading models for query...");
    let client = AhaClient::init(cfg.clone())
        .await
        .context("failed to init AhaClient for query")?;

    if enable_rerank {
        // ensure_rerank 在 retrieve_chunks 内部 lazy load，但提早提示
        // 让用户知道"第一次 query 会额外 load rerank"
        println!(
            "rerank enabled (model: {}; coarse-fetch top-{rn} → rerank → keep top-{k}; will lazy-load on first query if not already loaded)",
            cfg.rerank_model
        );
    }

    let enable_hybrid = cfg.hybrid_enabled && !no_hybrid;
    let sqlite =
        if enable_hybrid {
            Some(SqliteStore::open(&cfg.sqlite_path).with_context(|| {
                format!("failed to open sqlite at {}", cfg.sqlite_path.display())
            })?)
        } else {
            None
        };

    println!("searching top-{k} chunks for: {question:?}");
    if enable_hybrid {
        println!("  hybrid search: BM25 (FTS5) + vector (cosine) → RRF merge");
    }

    // 1. 检索 chunks + 构建 preamble（复制 rag_query 逻辑，但不调 LLM）
    let preamble = match rag::retrieve_chunks(
        &client,
        cfg,
        sqlite.as_ref(),
        &question,
        k,
        enable_hybrid,
        enable_rerank,
        rn,
    )
    .await
    {
        Ok(chunks) => {
            let context = rag::format_chunks_for_context(&chunks);
            rag::build_rag_preamble(cfg, &context)
        }
        Err(e) => {
            let s = format!("{e:#}");
            if rag::is_recoverable_error(&s) {
                eprintln!("(RAG unavailable: {s})");
                eprintln!("(hint: run `lorag ingest <path>` to enable retrieval)");
                eprintln!("(falling back to bare LLM)");
                cfg.prompt_bare_llm.clone()
            } else {
                return Err(e);
            }
        }
    };

    // 2. 流式推理
    let mut rx = rag::llm_complete_stream(&client, cfg, preamble, &question)
        .await
        .context("stream LLM completion failed")?;

    println!();
    println!("=== ANSWER ===");
    while let Some(result) = rx.recv().await {
        match result {
            Ok(token) => print!("{token}"),
            Err(e) => {
                eprintln!("\nstream error: {e}");
                break;
            }
        }
    }
    println!();
    println!("=== END ===");
    Ok(())
}

async fn cmd_models_pull(cfg: &config::AppConfig) -> Result<()> {
    // LLM + embedding + rerank（rerank 留空就跳过；M7.1 后 `models pull` 也管 rerank）
    let mut targets = vec![cfg.llm_model.clone(), cfg.embed_model.clone()];
    if !cfg.rerank_model.is_empty() {
        targets.push(cfg.rerank_model.clone());
    }
    for repo in targets {
        println!("pulling {repo} → {}/", cfg.models_dir.display());
        let p =
            aha_provider::ensure_model_downloaded(&repo, &cfg.models_dir, cfg.download_max_retries)
                .await
                .with_context(|| format!("failed to pull {repo}"))?;
        println!("  ok → {}", p.display());
    }
    println!("all models ready.");
    Ok(())
}

async fn cmd_models_status(cfg: &config::AppConfig, do_init: bool) -> Result<()> {
    println!("lorag model status");
    println!("  MODELS_DIR = {}", cfg.models_dir.display());
    println!("  (embedding dim auto-detected from model config.json on load)");
    println!();
    let statuses = aha_provider::models_status(cfg).context("failed to query model status")?;
    aha_provider::print_models_status(&statuses);
    println!();
    if statuses.iter().all(|s| s.exists) {
        if do_init {
            println!("init: loading models into memory (this can take 10s~minutes)...");
            let _client = aha_provider::AhaClient::init(cfg.clone())
                .await
                .context("failed to init AhaClient")?;
            println!("init: ok — both models loaded.");
        } else {
            println!(
                "hint: run `lorag models status --init` to actually load the models into memory."
            );
        }
    } else {
        println!("hint: run `lorag models pull` to download missing models.");
        std::process::exit(1);
    }
    Ok(())
}

async fn cmd_init(cfg: &config::AppConfig, _verbose: bool) -> Result<()> {
    println!("init: loading models into memory (this can take 10s~minutes)...");
    let _client = aha_provider::AhaClient::init(cfg.clone())
        .await
        .context("failed to init AhaClient")?;
    println!("init: ok — both models loaded.");
    Ok(())
}

async fn cmd_sources_list(cfg: &config::AppConfig, json: bool) -> Result<()> {
    use lorag::store::sqlite_store::SqliteStore;

    let store = SqliteStore::open(&cfg.sqlite_path)
        .with_context(|| format!("failed to open sqlite at {}", cfg.sqlite_path.display()))?;
    let sources = store
        .list_sources()
        .context("failed to list sources from sqlite")?;

    if json {
        let j = serde_json::to_string_pretty(&sources).context("failed to serialize sources")?;
        println!("{j}");
    } else {
        if sources.is_empty() {
            println!("(no ingested sources)");
        } else {
            println!("{:<50} {:>5} {:>10}", "source_path", "chunks", "bytes");
            println!("{}", "-".repeat(70));
            for s in &sources {
                println!(
                    "{:<50} {:>5} {:>10}",
                    truncate_str(&s.source_path, 50),
                    s.chunk_count,
                    s.byte_size
                );
            }
            println!("{} total sources", sources.len());
        }
    }
    Ok(())
}

fn truncate_str(s: &str, max: usize) -> String {
    if s.chars().count() > max {
        let truncated: String = s.chars().take(max - 3).collect();
        format!("{}...", truncated)
    } else {
        s.to_string()
    }
}

/// 是否启用 rerank：综合 `.env` 的 `RERANK_MODEL` 配置 + CLI 的 `--no-rerank` flag。
///
/// - `cfg.rerank_model` 留空 → 永远 false（没配模型）
/// - 用户传 `--no-rerank` → 强制 false（临时禁）
/// - 都有 → true（启用）
fn effective_rerank_enabled(cfg: &config::AppConfig, no_rerank: bool) -> bool {
    !cfg.rerank_model.is_empty() && !no_rerank
}

/// M7 `lorag chat` —— 多轮对话 REPL（带历史 + RAG）。
///
/// 行为：
/// - 启动时 load LLM + embedding（init 一次，不重 load）
/// - 把每轮的 user/assistant 消息存 sqlite 的 `messages` 表
/// - 下一轮 LLM call 前从 sqlite 读最近 N 条拼到 preamble
/// - RAG 检索失败时退化成纯 chat（带历史无 context）
/// - `--no-history` 每轮独立，不存不读历史
///
/// session_id 内部生成（`chat-YYYYMMDDTHHMMSS-<n>`）但用户感知不到——chat 进程
/// 内连续，跨进程不续接（web UI / 续接需求出现再设计）。
#[allow(clippy::too_many_arguments)]
async fn cmd_chat(
    cfg: &config::AppConfig,
    message: Option<String>,
    no_history: bool,
    no_banner: bool,
    no_rag: bool,
    no_rerank: bool,
    top_k: Option<usize>,
    rerank_top_n: Option<usize>,
    no_hybrid: bool,
) -> Result<()> {
    use lorag::aha_provider::AhaClient;
    use lorag::store::sqlite_store::SqliteStore;

    let k = top_k.unwrap_or(cfg.top_k);
    let rn = rerank_top_n.unwrap_or(cfg.rerank_top_n);
    let enable_rerank = effective_rerank_enabled(cfg, no_rerank);
    if enable_rerank && rn <= k {
        anyhow::bail!(
            "--rerank-top-n ({rn}) must be > --top-k ({k}); rerank needs more candidates than the final count"
        );
    }
    let session_id = generate_session_id();
    let sqlite = SqliteStore::open(&cfg.sqlite_path)
        .with_context(|| format!("failed to open sqlite at {}", cfg.sqlite_path.display()))?;

    let enable_hybrid = cfg.hybrid_enabled && !no_hybrid;

    if !no_banner {
        print_chat_banner(cfg, k, rn, no_history, no_rag, enable_rerank, enable_hybrid);
    }

    println!("loading models (10s~minutes first time, seconds after)...");
    let _ = std::io::Write::flush(&mut std::io::stdout());
    let client = AhaClient::init(cfg.clone())
        .await
        .context("failed to init AhaClient — try `lorag models status` first")?;
    let _ = std::io::Write::flush(&mut std::io::stdout());

    println!();
    println!("ready. (type /help for commands)");
    println!();

    // 一次性首问模式（--message）
    if let Some(msg) = message {
        match run_chat_turn(
            &client,
            cfg,
            &sqlite,
            &session_id,
            &msg,
            k,
            rn,
            no_history,
            no_rag,
            enable_rerank,
            enable_hybrid,
        )
        .await
        {
            Ok(_) => println!(), // answer 已流式打印；空行分隔
            Err(e) => eprintln!("error: {e:#}"),
        }
        return Ok(());
    }

    // REPL loop
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    let mut input = String::new();

    loop {
        print!(">> ");
        let _ = stdout.flush();
        input.clear();
        match stdin.read_line(&mut input) {
            Ok(0) => {
                println!();
                break;
            }
            Ok(_) => {}
            Err(e) => {
                eprintln!("read error: {e}");
                break;
            }
        }

        let line = input.trim();
        if line.is_empty() {
            continue;
        }

        // 内部命令（以 / 开头）
        if let Some(cmd) = line.strip_prefix('/') {
            let cmd_name = cmd.split_whitespace().next().unwrap_or("");
            match cmd_name {
                "exit" | "quit" | "q" => {
                    println!("bye.");
                    break;
                }
                "help" | "h" | "?" => {
                    print_chat_help(no_rag);
                }
                "status" => {
                    print_chat_status(
                        cfg,
                        &sqlite,
                        &session_id,
                        k,
                        rn,
                        no_history,
                        no_rag,
                        enable_rerank,
                        enable_hybrid,
                    );
                }
                "clear" | "cls" => {
                    for _ in 0..50 {
                        println!();
                    }
                }
                other => {
                    eprintln!("unknown command: /{other} (try /help)");
                }
            }
            continue;
        }

        // 普通问题 → 多轮 chat turn
        match run_chat_turn(
            &client,
            cfg,
            &sqlite,
            &session_id,
            line,
            k,
            rn,
            no_history,
            no_rag,
            enable_rerank,
            enable_hybrid,
        )
        .await
        {
            Ok(_) => println!(), // answer 已流式打印；空行分隔下一轮
            Err(e) => eprintln!("error: {e:#}"),
        }
    }
    Ok(())
}

/// 单轮 chat：拿历史 + 检索 → 拼 preamble → LLM → 存。
#[allow(clippy::too_many_arguments)]
async fn run_chat_turn(
    client: &aha_provider::AhaClient,
    cfg: &config::AppConfig,
    sqlite: &lorag::store::sqlite_store::SqliteStore,
    session_id: &str,
    user_msg: &str,
    top_k: usize,
    rerank_top_n: usize,
    no_history: bool,
    no_rag: bool,
    enable_rerank: bool,
    enable_hybrid: bool,
) -> Result<String> {
    use lorag::rag;

    print!("(");
    let _ = std::io::Write::flush(&mut std::io::stdout());
    let start = std::time::Instant::now();

    // 1. 加载历史
    let history = if no_history {
        Vec::new()
    } else {
        sqlite
            .load_recent_messages(session_id, MAX_HISTORY_MESSAGES)
            .context("failed to load chat history")?
    };

    // 2. RAG 检索（--no-rag 或失败 → 空 chunks；enable_rerank 决定是否 rerank）
    let chunks = if no_rag {
        Vec::new()
    } else {
        match rag::retrieve_chunks(
            client,
            cfg,
            Some(sqlite),
            user_msg,
            top_k,
            enable_hybrid,
            enable_rerank,
            rerank_top_n,
        )
        .await
        {
            Ok(c) => c,
            Err(e) => {
                let s = format!("{e:#}");
                if rag::is_recoverable_error(&s) {
                    eprintln!("\n(RAG unavailable: {s})");
                    eprintln!("(hint: run `lorag ingest <path>` to enable retrieval)");
                    eprintln!("(falling back to chat without context)");
                    Vec::new()
                } else {
                    return Err(e);
                }
            }
        }
    };

    // 3. 拼 preamble（history + context）
    let preamble = rag::build_chat_preamble(cfg, &history, &chunks);

    // 4. 流式 LLM（逐 token 打印 + 收集完整 answer 用于持久化）
    let mut rx = rag::llm_complete_stream(client, cfg, preamble, user_msg).await?;
    let mut answer = String::new();
    while let Some(result) = rx.recv().await {
        match result {
            Ok(token) => {
                print!("{token}");
                answer.push_str(&token);
            }
            Err(e) => {
                eprintln!("\nstream error: {e}");
                break;
            }
        }
    }

    let secs = start.elapsed().as_secs_f32();
    println!(" ({secs:.1}s)");

    // 5. 持久化（user + assistant）
    if !no_history {
        sqlite
            .append_message(session_id, "user", user_msg)
            .context("failed to persist user message")?;
        sqlite
            .append_message(session_id, "assistant", &answer)
            .context("failed to persist assistant message")?;
    }

    Ok(answer)
}

fn print_chat_banner(
    cfg: &config::AppConfig,
    top_k: usize,
    rerank_top_n: usize,
    no_history: bool,
    no_rag: bool,
    enable_rerank: bool,
    enable_hybrid: bool,
) {
    println!(
        "lorag chat v{} (multi-turn REPL)",
        env!("CARGO_PKG_VERSION")
    );
    if no_history {
        println!("  history:    disabled (--no-history, 每轮独立)");
    } else {
        println!(
            "  history:    sqlite (max {} message(s) per turn)",
            MAX_HISTORY_MESSAGES
        );
    }
    println!("  LLM:        {}", cfg.llm_model);
    println!("  Embedding:  {}", cfg.embed_model);
    if no_rag {
        println!("  RAG:        disabled (--no-rag)");
    } else {
        println!("  RAG:        enabled (top_k={top_k})");
    }
    if enable_hybrid && !no_rag {
        println!("  Hybrid:     BM25 (FTS5) + vector (cosine) → RRF merge");
    }
    if cfg.rerank_model.is_empty() {
        println!("  Rerank:     not configured (set RERANK_MODEL= in .env to enable)");
    } else if enable_rerank {
        println!(
            "  Rerank:     enabled ({}, coarse top-{rerank_top_n} → rerank → keep top-{top_k})",
            cfg.rerank_model
        );
    } else {
        println!("  Rerank:     disabled (--no-rerank)");
    }
    println!();
}

fn print_chat_help(no_rag: bool) {
    println!("commands:");
    println!("  /help, /h, /?    show this help");
    println!("  /status          show current model + RAG + hybrid + rerank config");
    println!("  /clear, /cls     clear the screen (50 newlines)");
    println!("  /exit, /quit, /q exit the chat");
    println!();
    println!("anything else is treated as a question and sent through the chat pipeline.");
    if no_rag {
        println!("pipeline: question → LLM (RAG disabled)");
    } else {
        println!(
            "pipeline: question → embed + BM25 FTS5 → RRF merge → history + context + question → LLM → answer"
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn print_chat_status(
    cfg: &config::AppConfig,
    sqlite: &lorag::store::sqlite_store::SqliteStore,
    session_id: &str,
    top_k: usize,
    rerank_top_n: usize,
    no_history: bool,
    no_rag: bool,
    enable_rerank: bool,
    enable_hybrid: bool,
) {
    println!("status:");
    if no_history {
        println!("  history:    disabled (--no-history)");
    } else {
        let count = sqlite.session_message_count(session_id).unwrap_or(-1);
        println!(
            "  history:    {count} message(s) in sqlite (max {MAX_HISTORY_MESSAGES} per turn)"
        );
    }
    println!("  LLM:        {}", cfg.llm_model);
    println!("  Embedding:  {}", cfg.embed_model);
    println!("  LanceDB:    {}", cfg.lancedb_dir.display());
    if no_rag {
        println!("  RAG:        disabled (--no-rag)");
    } else {
        println!("  Top-K:      {top_k}");
    }
    if enable_hybrid && !no_rag {
        println!("  Hybrid:     enabled (BM25 FTS5 + vector RRF merge)");
    } else if !no_rag {
        println!("  Hybrid:     disabled (--no-hybrid or HYBRID_ENABLED=false)");
    }
    if cfg.rerank_model.is_empty() {
        println!("  Rerank:     not configured");
    } else if enable_rerank {
        println!(
            "  Rerank:     enabled ({}, coarse top-{rerank_top_n} → rerank → keep top-{top_k})",
            cfg.rerank_model
        );
    } else {
        println!("  Rerank:     disabled (--no-rerank)");
    }
    println!("  Chunk size: {}", cfg.chunk_size);
}

/// `lorag doctor` —— 诊断环境。
///
/// 检查 .env / 模型文件 / 存储路径 / 编译 feature。
/// 不做破坏性操作（不 load 模型、不改文件），只读 + 写探针测可写性。
/// exit 0 表示无 FAIL，exit 1 表示至少一个 FAIL。
fn cmd_doctor(cfg: &config::AppConfig) -> Result<()> {
    use lorag::doctor;
    let checks = doctor::run_checks(cfg);
    let summary = doctor::print_checks(&checks);
    if summary.fail > 0 {
        std::process::exit(1);
    }
    Ok(())
}

/// `lorag serve` — 启动 Web UI（axum HTTP server, M10）。
///
/// 启动时 load LLM + embedding 模型到内存，然后在指定端口监听。
/// 前端由 Vite dev server（开发期）或 axum ServeDir（生产期）提供。
async fn cmd_serve(cfg: &config::AppConfig, port: u16) -> Result<()> {
    use std::sync::Arc;

    use lorag::aha_provider::AhaClient;
    use lorag::server::{self, AppState};
    use lorag::store::sqlite_store::SqliteStore;

    println!("loading models (10s~minutes first time, seconds after)...");
    let _ = std::io::Write::flush(&mut std::io::stdout());
    let client = AhaClient::init(cfg.clone())
        .await
        .context("failed to init AhaClient — try `lorag models status` first")?;

    let sqlite = SqliteStore::open(&cfg.sqlite_path)
        .with_context(|| format!("failed to open sqlite at {}", cfg.sqlite_path.display()))?;

    let state = Arc::new(AppState {
        client: Arc::new(tokio::sync::Mutex::new(client)),
        cfg: Arc::new(cfg.clone()),
        sqlite: Arc::new(tokio::sync::Mutex::new(sqlite)),
    });

    server::start(state, port).await
}

/// `lorag tray` — 启动 Web UI + 系统托盘图标（M11 phase 1）。
///
/// 行为：
/// - 在 tokio worker thread 上加载模型 + 启动 axum（带 graceful shutdown）
/// - server 起来后 1 秒自动打开浏览器到 `http://localhost:{port}`
/// - main thread 跑托盘事件循环（同步阻塞）——托盘图标显示 + 菜单可交互
/// - 用户选 Quit → 通过 oneshot 通知 axum 优雅关闭 → 等最多 5 秒 → 进程退出
///
/// 为什么 main thread 跑 tray 而不是 tokio task：tray-icon 在 Windows / macOS 都要求
/// 事件循环跑在创建托盘图标的同一个线程；当前 main thread 已被 `rt.block_on` 占用，
/// 直接同步调用 `run_tray_loop` 即可，tokio worker threads 会继续驱动 background axum task。
async fn cmd_tray(cfg: &config::AppConfig, port: u16) -> Result<()> {
    use std::sync::Arc;

    use lorag::aha_provider::AhaClient;
    use lorag::server::{self, AppState};
    use lorag::store::sqlite_store::SqliteStore;
    use lorag::tray;

    println!("lorag tray: starting on port {port}...");
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();

    // Background task：加载模型 → 启动 axum（带 graceful shutdown）→ 自动开浏览器
    let cfg_clone = cfg.clone();
    let server_handle = tokio::spawn(async move {
        println!("loading models (10s~minutes first time, seconds after)...");
        let _ = std::io::Write::flush(&mut std::io::stdout());
        let client = match AhaClient::init(cfg_clone.clone()).await {
            Ok(c) => c,
            Err(e) => {
                eprintln!("failed to init AhaClient: {e:#} (try `lorag models status` first)");
                return;
            }
        };

        let sqlite = match SqliteStore::open(&cfg_clone.sqlite_path) {
            Ok(s) => s,
            Err(e) => {
                eprintln!(
                    "failed to open sqlite at {}: {e:#}",
                    cfg_clone.sqlite_path.display()
                );
                return;
            }
        };

        let state = Arc::new(AppState {
            client: Arc::new(tokio::sync::Mutex::new(client)),
            cfg: Arc::new(cfg_clone.clone()),
            sqlite: Arc::new(tokio::sync::Mutex::new(sqlite)),
        });

        // 提前 1 秒自动开浏览器（等 axum bind 完成）
        let url = format!("http://localhost:{port}");
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            if let Err(e) = tray::open_browser(&url) {
                eprintln!("{e:#}");
            }
        });

        // oneshot::Receiver<()> 的 Output 是 Result<(), RecvError>，map 成 ()
        let shutdown_future = async move {
            let _ = shutdown_rx.await;
        };
        if let Err(e) = server::start_with_shutdown(state, port, shutdown_future).await {
            eprintln!("server error: {e:#}");
        }
    });

    // Main thread：跑 tray 事件循环（同步阻塞，直到用户选 Quit）
    let tray_result = tray::run_tray_loop(port, shutdown_tx);

    // 等 axum graceful shutdown 完成，最多 5 秒（超时强退）
    let _ = tokio::time::timeout(std::time::Duration::from_secs(5), server_handle).await;

    tray_result
}

/// `lorag reindex` —— 删 LanceDB + SQLite 后重新摄入。
///
/// 适用场景：
/// - 换了 `EMBED_MODEL`（向量维度变了）→ lancedb schema 跟 dim 不匹配
/// - 想完全重建（不管有没有变）
///
/// 流程：interactive 确认（除非 `--yes`） → 删 lancedb 目录 + sqlite 主文件 + WAL/SHM
///   → 调 `pipeline::run_ingest` 重新摄入。
///
/// **不**删模型文件（`MODELS_DIR/`）。模型仍然要走 `lorag models pull` 单独下。
async fn cmd_reindex(
    cfg: &config::AppConfig,
    paths: Vec<PathBuf>,
    ext: Vec<String>,
    recursive: bool,
    yes: bool,
    dry_run: bool,
) -> Result<()> {
    use lorag::aha_provider::AhaClient;
    use lorag::ingest::pipeline;

    let sqlite_files = sqlite_files_for(&cfg.sqlite_path);

    if dry_run {
        println!("DRY RUN — nothing will be deleted or ingested.");
        println!();
        println!("would delete:");
        println!("  {}", cfg.lancedb_dir.display());
        for f in &sqlite_files {
            println!("  {}", f.display());
        }
        println!();
        println!("would re-ingest from:");
        for p in &paths {
            println!("  {}", p.display());
        }
        println!();
        println!("hint: pass --yes (or interactive `y`) to actually do it.");
        return Ok(());
    }

    // 1. 打印计划
    println!("reindex will:");
    println!("  delete:");
    println!("    {}", cfg.lancedb_dir.display());
    for f in &sqlite_files {
        println!("    {}", f.display());
    }
    println!("  re-ingest from:");
    for p in &paths {
        println!("    {}", p.display());
    }
    println!();
    println!(
        "(model files in {} are NOT touched)",
        cfg.models_dir.display()
    );
    println!();

    // 2. 确认
    if !yes {
        print!("proceed? [y/N] ");
        std::io::Write::flush(&mut std::io::stdout())?;
        let mut input = String::new();
        std::io::stdin()
            .read_line(&mut input)
            .context("failed to read confirmation")?;
        let ans = input.trim().to_lowercase();
        if ans != "y" && ans != "yes" {
            println!("aborted.");
            return Ok(());
        }
    }

    // 3. 删 lancedb 目录（不存在 OK）
    match std::fs::remove_dir_all(&cfg.lancedb_dir) {
        Ok(()) => println!("removed: {}", cfg.lancedb_dir.display()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            println!("(already gone: {})", cfg.lancedb_dir.display());
        }
        Err(e) => {
            return Err(anyhow::anyhow!(
                "failed to delete {}: {}",
                cfg.lancedb_dir.display(),
                e
            ));
        }
    }

    // 4. 删 sqlite 主文件 + WAL/SHM/Journal（不存在 OK）
    for f in &sqlite_files {
        match std::fs::remove_file(f) {
            Ok(()) => println!("removed: {}", f.display()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                return Err(anyhow::anyhow!("failed to delete {}: {}", f.display(), e));
            }
        }
    }

    // 5. 重新 ingest
    println!();
    println!("loading embedding model for re-ingest (skipping LLM to save memory + time)...");
    let client = AhaClient::init_embed_only(cfg.clone())
        .await
        .context("failed to init AhaClient for reindex")?;

    // reindex 强制重摄入（hash 检查不适用，因为旧记录已删）
    let counts = pipeline::run_ingest(&client, cfg, &paths, &ext, /*force=*/ true, recursive)
        .await
        .context("re-ingest pipeline failed")?;

    println!();
    println!(
        "done. ok={} skipped={} failed={}",
        counts.ok, counts.skipped, counts.failed
    );
    Ok(())
}

/// 列出 sqlite 主文件 + 旁文件（journal / wal / shm）。
fn sqlite_files_for(sqlite_path: &std::path::Path) -> Vec<PathBuf> {
    let mut out = vec![sqlite_path.to_path_buf()];
    let base = sqlite_path.to_string_lossy();
    for suffix in ["-journal", "-wal", "-shm"] {
        out.push(std::path::PathBuf::from(format!("{base}{suffix}")));
    }
    out
}
