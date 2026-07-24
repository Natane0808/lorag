//! `.env` 加载 + 强类型 `AppConfig`。
//!
//! 任何业务模块都应通过 `AppConfig` 读环境变量，**不要**散落 `std::env::var`。
//! 配置缺失或非法时直接 fail-fast，**不**给"看起来合理"的默认值掩盖错误。

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow};

/// 启动入口：从 `.env` 加载并构造 [`AppConfig`]。
///
/// - 默认从当前目录读 `.env`
/// - 可由 `LORAG_ENV=/path/to/.env` 覆盖
/// - 已存在的环境变量优先级 > `.env` 里的值（dotenvy 默认行为）
pub fn load() -> Result<AppConfig> {
    let env_path = std::env::var("LORAG_ENV").unwrap_or_else(|_| ".env".to_string());
    if Path::new(&env_path).exists() {
        dotenvy::from_path(&env_path)
            .with_context(|| format!("failed to load env file: {env_path}"))?;
    } else {
        // 没找到不致命：用户可能用环境变量直接喂
        tracing::warn!("env file not found: {env_path} (relying on process env)");
    }
    // dotenvy::from_path 已把 .env 加载到 process env，直接从 env 读
    let raw = RawConfig::from_env_manual(&env_path).context("failed to parse config from env")?;
    let cfg: AppConfig = raw.into();
    cfg.validate()?;
    Ok(cfg)
}

// =============================================================================
// 原始配置（直接从 .env 读到的字符串）
// =============================================================================

#[derive(Debug, Default)]
struct RawConfig {
    llm_model_repo: String,
    llm_model_name: Option<String>,
    embed_model_repo: String,
    embed_model_name: Option<String>,
    models_dir: String,
    download_max_retries: Option<u32>,
    embed_dim: Option<usize>,
    lancedb_dir: String,
    sqlite_path: String,
    chunk_size: Option<usize>,
    chunk_overlap: Option<usize>,
    top_k: Option<usize>,
    log_level: String,
}

impl RawConfig {
    /// 不依赖 envy 的简易 .env 解析（兜底路径，正常用 dotenvy 把 env 注入到 process env）。
    fn from_env_manual(_env_path: &str) -> Result<Self> {
        // dotenvy::from_path 已经把 .env 加载到 process env，直接从 env 读
        Ok(Self {
            llm_model_repo: std::env::var("LLM_MODEL_REPO").unwrap_or_default(),
            llm_model_name: std::env::var("LLM_MODEL_NAME").ok(),
            embed_model_repo: std::env::var("EMBED_MODEL_REPO").unwrap_or_default(),
            embed_model_name: std::env::var("EMBED_MODEL_NAME").ok(),
            models_dir: std::env::var("MODELS_DIR").unwrap_or_default(),
            download_max_retries: std::env::var("DOWNLOAD_MAX_RETRIES")
                .ok()
                .map(|s| s.parse())
                .transpose()
                .context("DOWNLOAD_MAX_RETRIES must be a positive integer")?,
            embed_dim: std::env::var("EMBED_DIM")
                .ok()
                .map(|s| s.parse())
                .transpose()
                .context("EMBED_DIM must be a positive integer")?,
            lancedb_dir: std::env::var("LANCEDB_DIR").unwrap_or_default(),
            sqlite_path: std::env::var("SQLITE_PATH").unwrap_or_default(),
            chunk_size: std::env::var("CHUNK_SIZE")
                .ok()
                .map(|s| s.parse())
                .transpose()
                .context("CHUNK_SIZE must be a positive integer")?,
            chunk_overlap: std::env::var("CHUNK_OVERLAP")
                .ok()
                .map(|s| s.parse())
                .transpose()
                .context("CHUNK_OVERLAP must be a non-negative integer")?,
            top_k: std::env::var("TOP_K")
                .ok()
                .map(|s| s.parse())
                .transpose()
                .context("TOP_K must be a positive integer")?,
            log_level: std::env::var("LOG_LEVEL").unwrap_or_default(),
        })
    }
}

impl From<RawConfig> for AppConfig {
    fn from(r: RawConfig) -> Self {
        let llm_repo = r.llm_model_repo.clone();
        let embed_repo = r.embed_model_repo.clone();
        Self {
            llm_model_repo: r.llm_model_repo,
            llm_model_name: r.llm_model_name.unwrap_or(llm_repo),
            embed_model_repo: r.embed_model_repo,
            embed_model_name: r.embed_model_name.unwrap_or(embed_repo),
            models_dir: PathBuf::from(if r.models_dir.is_empty() {
                "./data/models".to_string()
            } else {
                r.models_dir
            }),
            download_max_retries: r.download_max_retries.unwrap_or(3),
            embed_dim: r.embed_dim.unwrap_or(384),
            lancedb_dir: PathBuf::from(if r.lancedb_dir.is_empty() {
                "./data/lancedb".to_string()
            } else {
                r.lancedb_dir
            }),
            sqlite_path: PathBuf::from(if r.sqlite_path.is_empty() {
                "./data/lorag.db".to_string()
            } else {
                r.sqlite_path
            }),
            chunk_size: r.chunk_size.unwrap_or(500),
            chunk_overlap: r.chunk_overlap.unwrap_or(50),
            top_k: r.top_k.unwrap_or(5),
            log_level: if r.log_level.is_empty() {
                "info".to_string()
            } else {
                r.log_level
            },
        }
    }
}

// =============================================================================
// 强类型 AppConfig
// =============================================================================

/// 运行时配置。所有字段在 [`AppConfig::validate`] 阶段会做合法性检查。
#[derive(Debug, Clone)]
pub struct AppConfig {
    /// LLM 模型 id（aha WhichModel 接受的字符串，如 `Qwen/Qwen3-4B`）。
    pub llm_model_repo: String,
    /// 调 aha 时 JSON 的 `model` 字段值；默认 = `llm_model_repo`。
    pub llm_model_name: String,
    /// Embedding 模型 id。
    pub embed_model_repo: String,
    /// 调 aha 时 JSON 的 `model` 字段值；默认 = `embed_model_repo`。
    pub embed_model_name: String,
    /// 模型下载/加载目录。aha 的 `download_model` 会下到 `<models_dir>/<repo>/`。
    pub models_dir: PathBuf,
    /// 下载重试次数（传给 `aha::utils::download_model`）。
    pub download_max_retries: u32,
    /// 向量维度（建 lancedb 表时强校验；改这个值必须 `rm -rf data/lancedb`）。
    pub embed_dim: usize,
    /// lancedb 数据目录。
    pub lancedb_dir: PathBuf,
    /// sqlite 元数据库路径。
    pub sqlite_path: PathBuf,
    /// 单 chunk 字符上限。
    pub chunk_size: usize,
    /// 滑窗重叠字符数。
    pub chunk_overlap: usize,
    /// RAG 检索 top_k。
    pub top_k: usize,
    /// 日志级别（tracing filter），如 `info` / `debug` / `info,lance=error`。默认 `info`。
    /// 如果同时设置了 `RUST_LOG`，`RUST_LOG` 优先。
    pub log_level: String,
}

impl AppConfig {
    /// 启动期合法性检查：缺必填、数值范围、维度合理性。
    pub fn validate(&self) -> Result<()> {
        if self.llm_model_repo.is_empty() {
            return Err(anyhow!("LLM_MODEL_REPO is required (e.g. Qwen/Qwen3-4B)"));
        }
        if self.embed_model_repo.is_empty() {
            return Err(anyhow!(
                "EMBED_MODEL_REPO is required (e.g. all-MiniLM-L6-v2)"
            ));
        }
        if self.embed_dim == 0 {
            return Err(anyhow!("EMBED_DIM must be > 0"));
        }
        if self.chunk_size == 0 {
            return Err(anyhow!("CHUNK_SIZE must be > 0"));
        }
        if self.chunk_overlap >= self.chunk_size {
            return Err(anyhow!(
                "CHUNK_OVERLAP ({}) must be < CHUNK_SIZE ({})",
                self.chunk_overlap,
                self.chunk_size
            ));
        }
        if self.top_k == 0 {
            return Err(anyhow!("TOP_K must be > 0"));
        }
        if self.download_max_retries == 0 {
            return Err(anyhow!("DOWNLOAD_MAX_RETRIES must be > 0"));
        }
        Ok(())
    }

    /// 模型的本地目录（`models_dir/repo`）。
    /// 跟 `aha::utils::download_model` 的行为一致：它把模型下到 `<save_dir>/<model_id>/`。
    pub fn model_local_dir(&self, repo: &str) -> PathBuf {
        self.models_dir.join(repo)
    }
}
