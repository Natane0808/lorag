//! `lorag doctor` 诊断模块。
//!
//! 跑全套环境检查：env / models / storage / build features。
//! 不做破坏性操作（不 load 模型、不改文件），只读不写。
//!
//! 退出码：
//! - 0：所有 FAIL 检查都通过（可以有 WARN）
//! - 1：至少一个 FAIL

use std::path::{Path, PathBuf};

use crate::aha_provider::models_status;
use crate::config::AppConfig;

/// 检查状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckStatus {
    Pass,
    Warn,
    Fail,
}

impl CheckStatus {
    pub fn label(self) -> &'static str {
        match self {
            CheckStatus::Pass => "ok ",
            CheckStatus::Warn => "WARN",
            CheckStatus::Fail => "FAIL",
        }
    }
}

/// 单条检查结果。
#[derive(Debug, Clone)]
pub struct Check {
    pub category: &'static str,
    pub name: String,
    pub status: CheckStatus,
    pub detail: String,
    /// FAIL/WARN 时给的一行修复提示
    pub hint: Option<String>,
}

impl Check {
    fn pass(category: &'static str, name: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            category,
            name: name.into(),
            status: CheckStatus::Pass,
            detail: detail.into(),
            hint: None,
        }
    }
    fn warn(
        category: &'static str,
        name: impl Into<String>,
        detail: impl Into<String>,
        hint: impl Into<String>,
    ) -> Self {
        Self {
            category,
            name: name.into(),
            status: CheckStatus::Warn,
            detail: detail.into(),
            hint: Some(hint.into()),
        }
    }
    fn fail(
        category: &'static str,
        name: impl Into<String>,
        detail: impl Into<String>,
        hint: impl Into<String>,
    ) -> Self {
        Self {
            category,
            name: name.into(),
            status: CheckStatus::Fail,
            detail: detail.into(),
            hint: Some(hint.into()),
        }
    }
}

/// 跑全套检查，按 [config / models / storage / build] 顺序返回。
pub fn run_checks(cfg: &AppConfig) -> Vec<Check> {
    let mut checks = Vec::new();
    check_config(&mut checks, cfg);
    check_models(&mut checks, cfg);
    check_storage(&mut checks, cfg);
    check_build_features(&mut checks);
    checks
}

/// 检查总数 / 失败 / 警告数。
#[derive(Debug, Default, Clone, Copy)]
pub struct Summary {
    pub total: usize,
    pub pass: usize,
    pub warn: usize,
    pub fail: usize,
}

impl Summary {
    pub fn from_checks(checks: &[Check]) -> Self {
        let mut s = Self {
            total: checks.len(),
            ..Default::default()
        };
        for c in checks {
            match c.status {
                CheckStatus::Pass => s.pass += 1,
                CheckStatus::Warn => s.warn += 1,
                CheckStatus::Fail => s.fail += 1,
            }
        }
        s
    }
}

/// 打印检查结果到 stdout。返回汇总。
pub fn print_checks(checks: &[Check]) -> Summary {
    println!("lorag doctor v{}", env!("CARGO_PKG_VERSION"));
    println!();
    let mut last_category = "";
    for c in checks {
        if c.category != last_category {
            // 新分类：先空行再打印分类标题
            if !last_category.is_empty() {
                println!();
            }
            println!("{}:", c.category);
            last_category = c.category;
        }
        println!("  [{}] {}: {}", c.status.label(), c.name, c.detail);
        if let Some(hint) = &c.hint {
            println!("         hint: {hint}");
        }
    }
    let summary = Summary::from_checks(checks);
    println!();
    if summary.fail > 0 {
        println!(
            "{} checks: {} pass, {} warn, {} FAIL — fix the FAIL items above.",
            summary.total, summary.pass, summary.warn, summary.fail
        );
    } else if summary.warn > 0 {
        println!(
            "{} checks: {} pass, {} warn — all hard checks passed, but check the warnings.",
            summary.total, summary.pass, summary.warn
        );
    } else {
        println!(
            "{} checks: {} pass — everything looks good.",
            summary.total, summary.pass
        );
    }
    summary
}

// ============================================================================
// config
// ============================================================================

fn check_config(checks: &mut Vec<Check>, _cfg: &AppConfig) {
    // 1. .env 存在
    let env_path = std::env::var("LORAG_ENV")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(".env"));
    if env_path.exists() {
        checks.push(Check::pass(
            "config",
            ".env file",
            format!("present at {}", env_path.display()),
        ));
    } else {
        checks.push(Check::fail(
            "config",
            ".env file",
            format!("not found at {}", env_path.display()),
            "cp .env.example .env (then edit LLM_MODEL_REPO / EMBED_MODEL_REPO as needed)",
        ));
    }

    // 2. LLM_MODEL_REPO 非空
    // cfg 加载时已经强校验了，这里再保险一次（如果 cfg 加载过了，字段肯定非空）
    if _cfg.llm_model_repo.is_empty() {
        checks.push(Check::fail(
            "config",
            "LLM_MODEL_REPO",
            "empty (required)",
            "set LLM_MODEL_REPO in .env, e.g. LLM_MODEL_REPO=Qwen/Qwen3-0.6B",
        ));
    } else {
        checks.push(Check::pass(
            "config",
            "LLM_MODEL_REPO",
            format!("{:?}", _cfg.llm_model_repo),
        ));
    }

    // 3. EMBED_MODEL_REPO 非空
    if _cfg.embed_model_repo.is_empty() {
        checks.push(Check::fail(
            "config",
            "EMBED_MODEL_REPO",
            "empty (required)",
            "set EMBED_MODEL_REPO in .env, e.g. EMBED_MODEL_REPO=Qwen/Qwen3-Embedding-0.6B",
        ));
    } else {
        checks.push(Check::pass(
            "config",
            "EMBED_MODEL_REPO",
            format!("{:?}", _cfg.embed_model_repo),
        ));
    }

    // 4. EMBED_DIM 合理
    if _cfg.embed_dim == 0 {
        checks.push(Check::fail(
            "config",
            "EMBED_DIM",
            format!("{} (must be > 0)", _cfg.embed_dim),
            "set EMBED_DIM in .env to match your embedding model's output dim (MiniLM=384, Qwen3-Embedding-0.6B=1024, ...)",
        ));
    } else {
        checks.push(Check::pass(
            "config",
            "EMBED_DIM",
            format!("{} (must match your embedding model)", _cfg.embed_dim),
        ));
    }
}

// ============================================================================
// models
// ============================================================================

fn check_models(checks: &mut Vec<Check>, cfg: &AppConfig) {
    // 用 models_status 拿 LLM + Embedding 的本地路径
    let statuses = match models_status(cfg) {
        Ok(s) => s,
        Err(e) => {
            checks.push(Check::fail(
                "models",
                "status query",
                format!("failed: {e}"),
                "check that MODELS_DIR in .env is a valid path",
            ));
            return;
        }
    };

    for (i, status) in statuses.iter().enumerate() {
        let label = if i == 0 { "LLM" } else { "Embedding" };
        if status.exists {
            let path = status
                .resolved_path
                .as_deref()
                .unwrap_or(&status.expected_path);
            let size = dir_size_mb(path);
            checks.push(Check::pass(
                "models",
                format!("{label} ({})", status.repo),
                format!("present at {} ({:.1} MB)", path.display(), size),
            ));
        } else {
            checks.push(Check::fail(
                "models",
                format!("{label} ({})", status.repo),
                format!("not found locally (expected at {})", status.expected_path.display()),
                "run `lorag models pull` to download",
            ));
        }
    }
}

fn dir_size_mb(path: &Path) -> f64 {
    let total: u64 = walkdir_size(path).unwrap_or(0);
    total as f64 / 1_048_576.0
}

fn walkdir_size(path: &Path) -> std::io::Result<u64> {
    let mut total = 0u64;
    if path.is_file() {
        return Ok(path.metadata()?.len());
    }
    for entry in std::fs::read_dir(path)? {
        let entry = entry?;
        let meta = entry.metadata()?;
        if meta.is_file() {
            total += meta.len();
        } else if meta.is_dir() {
            total += walkdir_size(&entry.path())?;
        }
    }
    Ok(total)
}

// ============================================================================
// storage
// ============================================================================

fn check_storage(checks: &mut Vec<Check>, cfg: &AppConfig) {
    // 1. MODELS_DIR 可写
    check_dir_writable(checks, "MODELS_DIR", &cfg.models_dir, /* must_exist */ true);

    // 2. LANCEDB_DIR —— 不存在是 OK（run ingest 会创建），但父目录要可写
    check_dir_or_parent_writable(
        checks,
        "LANCEDB_DIR",
        &cfg.lancedb_dir,
    );

    // 3. SQLITE_PATH 父目录可写
    if let Some(parent) = cfg.sqlite_path.parent() {
        if parent.as_os_str().is_empty() {
            // 就在 CWD 下，不需要检查父目录
            checks.push(Check::pass(
                "storage",
                "SQLITE_PATH parent",
                format!("CWD (sqlite will create {} here)", cfg.sqlite_path.display()),
            ));
        } else {
            check_dir_writable(checks, "SQLITE_PATH parent", parent, /* must_exist */ true);
        }
    }

    // 4. lancedb 状态
    if cfg.lancedb_dir.exists() {
        let docs_lance = cfg.lancedb_dir.join("documents.lance");
        if docs_lance.exists() {
            checks.push(Check::pass(
                "storage",
                "LanceDB documents table",
                format!("present at {}", docs_lance.display()),
            ));
        } else {
            checks.push(Check::warn(
                "storage",
                "LanceDB documents table",
                format!("lancedb dir exists but documents table not found at {}", docs_lance.display()),
                "run `lorag ingest <path>` to create the table and add documents",
            ));
        }
    } else {
        checks.push(Check::warn(
            "storage",
            "LanceDB directory",
            format!("does not exist at {}", cfg.lancedb_dir.display()),
            "run `lorag ingest <path>` to create it (will also create the `documents` table)",
        ));
    }
}

fn check_dir_writable(checks: &mut Vec<Check>, name: &str, path: &Path, must_exist: bool) {
    if !path.exists() {
        if must_exist {
            checks.push(Check::fail(
                "storage",
                name,
                format!("does not exist at {}", path.display()),
                "create the directory, or fix the path in .env",
            ));
        } else {
            checks.push(Check::warn(
                "storage",
                name,
                format!("does not exist (will be created on first write): {}", path.display()),
                "no action needed; just be aware",
            ));
        }
        return;
    }
    if !path.is_dir() {
        checks.push(Check::fail(
            "storage",
            name,
            format!("exists but is not a directory: {}", path.display()),
            "remove the file or fix the path in .env",
        ));
        return;
    }
    // 试写一个临时文件验证可写
    let probe = path.join(".lorag-doctor-write-probe");
    match std::fs::write(&probe, b"") {
        Ok(()) => {
            let _ = std::fs::remove_file(&probe);
            checks.push(Check::pass(
                "storage",
                name,
                format!("{} (writable)", path.display()),
            ));
        }
        Err(e) => {
            checks.push(Check::fail(
                "storage",
                name,
                format!("exists but not writable: {e}"),
                "check directory permissions",
            ));
        }
    }
}

fn check_dir_or_parent_writable(checks: &mut Vec<Check>, name: &str, path: &Path) {
    if path.exists() {
        check_dir_writable(checks, name, path, /* must_exist */ true);
    } else {
        // 找最近存在的祖先目录
        let mut probe = path;
        while let Some(parent) = probe.parent() {
            if parent.exists() {
                check_dir_writable(checks, &format!("{name} parent"), parent, true);
                return;
            }
            probe = parent;
        }
        checks.push(Check::warn(
            "storage",
            format!("{name} (or any parent)"),
            format!("no existing ancestor directory found for {}", path.display()),
            "create a parent directory or fix the path in .env",
        ));
    }
}

// ============================================================================
// build features
// ============================================================================

fn check_build_features(checks: &mut Vec<Check>) {
    // lorag 把 `cuda` / `flash-attn` / `metal` 作为顶层 feature，
    // 各自 passthrough 到 `aha/cuda` 等。cfg!() 看到的是 lorag 自己的 feature 名。
    let features: Vec<(&str, bool)> = vec![
        ("cuda", cfg!(feature = "cuda")),
        ("flash-attn", cfg!(feature = "flash-attn")),
        ("metal", cfg!(feature = "metal")),
    ];
    let enabled: Vec<&str> = features
        .iter()
        .filter_map(|(name, on)| if *on { Some(*name) } else { None })
        .collect();

    let detail = if enabled.is_empty() {
        "none (CPU only inference)".to_string()
    } else {
        format!("enabled: {}", enabled.join(", "))
    };

    let status = if enabled.is_empty() {
        CheckStatus::Warn
    } else {
        CheckStatus::Pass
    };
    let hint = if enabled.is_empty() {
        Some("rebuild with `--features cuda` (NVIDIA) or `--features metal` (macOS) for GPU acceleration".to_string())
    } else {
        None
    };
    checks.push(Check {
        category: "build",
        name: "GPU acceleration features".to_string(),
        status,
        detail,
        hint,
    });
}
