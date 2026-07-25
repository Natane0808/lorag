//! `lorag` CLI 入口。
//!
//! 只做 CLI 解析 + 命令分派；业务逻辑放 `src/<module>.rs`。
//! 错误用 `anyhow` 打到 stderr，exit code 1。

use std::io::Write as _;
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use lorag::aha_provider;
use lorag::config;

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

    /// 交互式 REPL：init 一次后循环 query（避免每次 query 都重 load 模型）
    Shell {
        /// RAG 检索 top_k（覆盖配置）
        #[arg(long)]
        top_k: Option<usize>,

        /// 不显示欢迎 banner
        #[arg(long)]
        no_banner: bool,

        /// 跳过 LanceDB 检索（纯 LLM 对话）—— 用于绕开 rig-lancedb 的内存 bug，
        /// 或在没 ingest 时也想对话
        #[arg(long)]
        no_rag: bool,
    },

    /// 列出已摄入文件
    Sources {
        #[command(subcommand)]
        action: SourcesAction,
    },

    /// 多轮对话 REPL（MVP 阶段占位）
    Chat {
        /// 第一轮的问题（MVP 阶段不读 stdin）
        #[arg(long, short)]
        message: Option<String>,
    },

    /// 诊断环境：检查 .env / 模型文件 / 存储路径 / 编译 feature
    Doctor,

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

    // 日志过滤：默认静默 lance / lancedb / datafusion / arrow 的 INFO 噪声（每次 query 都打
    // 一堆 plan_run / dataset_events log，太丑）。RUST_LOG 优先；否则用 LOG_LEVEL（兼容旧）；
    // 否则默认 `info`。**lance silencing 是必加后缀**，不管用户怎么设 base 都会追加。
    //
    // 注意：env_filter 的 target 段是字面量（不支持 glob），所以要显式列全 `lance::*` targets。
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
            Command::Query { question, top_k } => cmd_query(&cfg, question, top_k).await,
            Command::Models { action } => match action {
                ModelsAction::Pull => cmd_models_pull(&cfg).await,
                ModelsAction::Status { r#init } => cmd_models_status(&cfg, r#init).await,
            },
            Command::Init { verbose } => cmd_init(&cfg, verbose).await,
            Command::Shell {
                top_k,
                no_banner,
                no_rag,
            } => cmd_shell(&cfg, top_k, no_banner, no_rag).await,
            Command::Sources { action } => match action {
                SourcesAction::List { json } => cmd_sources_list(&cfg, json).await,
            },
            Command::Chat { message } => cmd_chat(message).await,
            Command::Doctor => cmd_doctor(&cfg),
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
// 命令实现（M0：占位；后续 milestone 替换为真实逻辑）
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

    // ingest 只需要 embedding 模型来向量化 chunk，加载 LLM 纯属浪费
    // （4B LLM ~8GB 内存 + 数十秒 load）。用 init_embed_only 跳过 LLM。
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

async fn cmd_query(cfg: &config::AppConfig, question: String, top_k: Option<usize>) -> Result<()> {
    use lorag::aha_provider::AhaClient;
    use lorag::rag;

    let k = top_k.unwrap_or(cfg.top_k);

    println!("loading models for query...");
    let client = AhaClient::init(cfg.clone())
        .await
        .context("failed to init AhaClient for query")?;

    println!("searching top-{} chunks for: {question:?}", k);
    let answer = rag::rag_query(&client, cfg, &question, k)
        .await
        .context("rag query failed")?;

    println!();
    println!("=== ANSWER ===");
    println!("{answer}");
    println!("=== END ===");
    Ok(())
}

async fn cmd_models_pull(cfg: &config::AppConfig) -> Result<()> {
    let targets = [cfg.llm_model.clone(), cfg.embed_model.clone()];
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

async fn cmd_shell(
    cfg: &config::AppConfig,
    top_k: Option<usize>,
    no_banner: bool,
    no_rag: bool,
) -> Result<()> {
    use lorag::aha_provider::AhaClient;
    use lorag::rag;

    let k = top_k.unwrap_or(cfg.top_k);

    if !no_banner {
        println!("lorag shell v{} (REPL mode)", env!("CARGO_PKG_VERSION"));
        if no_rag {
            println!("(RAG disabled via --no-rag — pure LLM mode)");
        }
        println!();
    }

    println!("loading models (10s~minutes first time, seconds after)...");
    let _ = std::io::Write::flush(&mut std::io::stdout());
    let client = AhaClient::init(cfg.clone())
        .await
        .context("failed to init AhaClient — try `lorag models status` first")?;
    let _ = std::io::Write::flush(&mut std::io::stdout());

    println!();
    println!("ready:");
    println!("  LLM       : {}", cfg.llm_model);
    println!("  Embedding : {}", cfg.embed_model);
    if no_rag {
        println!("  RAG       : disabled (--no-rag)");
    } else {
        println!(
            "  LanceDB   : {} (run `lorag ingest <path>` to add documents)",
            cfg.lancedb_dir.display()
        );
        println!("  Top-K     : {k}");
    }
    println!();
    println!("Type a question, or /help for commands. /exit to quit.");
    println!();

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
            let cmd = cmd.split_whitespace().next().unwrap_or("");
            match cmd {
                "exit" | "quit" | "q" => {
                    println!("bye.");
                    break;
                }
                "help" | "h" | "?" => {
                    print_shell_help();
                }
                "status" => {
                    print_shell_status(cfg, k, no_rag);
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

        // 普通问题 → RAG query（--no-rag 时跳过 lancedb 走裸 LLM）
        print!("(thinking... ");
        let _ = stdout.flush();
        let start = std::time::Instant::now();
        let result = if no_rag {
            rag::bare_llm_query(&client, cfg, line).await
        } else {
            rag::rag_query(&client, cfg, line, k).await
        };
        match result {
            Ok(answer) => {
                let secs = start.elapsed().as_secs_f32();
                println!("{}s)", secs);
                println!();
                println!("{answer}");
            }
            Err(e) => {
                let secs = start.elapsed().as_secs_f32();
                println!("{}s, ERROR)", secs);
                eprintln!();
                eprintln!("error: {e:#}");
                if format!("{e:#}").contains("run `lorag ingest`") {
                    eprintln!();
                    eprintln!("hint: 你的 LanceDB 还没有摄入文档。先跑：");
                    eprintln!("  lorag ingest <path>");
                    eprintln!("或者用 --no-rag 跳过 RAG：");
                    eprintln!("  lorag shell --no-rag");
                }
            }
        }
        println!();
    }

    Ok(())
}

fn print_shell_help() {
    println!("commands:");
    println!("  /help, /h, /?    show this help");
    println!("  /status          show loaded models + LanceDB status");
    println!("  /clear, /cls     clear the screen (50 newlines)");
    println!("  /exit, /quit, /q exit the shell");
    println!();
    println!("anything else is treated as a question and sent through the RAG pipeline.");
    println!("RAG: question → embed → top-K LanceDB search → context + question → LLM → answer");
}

fn print_shell_status(cfg: &config::AppConfig, k: usize, no_rag: bool) {
    println!("status:");
    println!("  LLM             : {}", cfg.llm_model);
    println!("  Embedding       : {}", cfg.embed_model);
    println!("  (dim auto-detected at load time)");
    println!("  Models dir      : {}", cfg.models_dir.display());
    println!("  LanceDB dir     : {}", cfg.lancedb_dir.display());
    println!("  SQLite path     : {}", cfg.sqlite_path.display());
    if no_rag {
        println!("  RAG             : disabled (--no-rag)");
    } else {
        println!("  Top-K (this session): {k}");
    }
    println!("  Chunk size      : {}", cfg.chunk_size);
    println!("  Chunk overlap   : {}", cfg.chunk_overlap);
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

async fn cmd_chat(message: Option<String>) -> Result<()> {
    let msg = message
        .as_deref()
        .unwrap_or("(no message provided; use --message)");
    anyhow::bail!("`lorag chat` is a placeholder in MVP (M7 计划实装). got: {msg:?}")
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
///
/// 直接在原路径字符串上 append suffix（不用 `set_file_name`）—— 保持跟 `cfg.sqlite_path`
/// 一样的分隔符风格（避免 Windows 上 `set_file_name` 跟 parent 的 forward slash 混出来
/// `data\foo.db-wal` 这种丑格式）。功能上 `remove_file` 都认，但 display 出来难看。
fn sqlite_files_for(sqlite_path: &std::path::Path) -> Vec<PathBuf> {
    let mut out = vec![sqlite_path.to_path_buf()];
    let base = sqlite_path.to_string_lossy();
    for suffix in ["-journal", "-wal", "-shm"] {
        out.push(std::path::PathBuf::from(format!("{base}{suffix}")));
    }
    out
}
