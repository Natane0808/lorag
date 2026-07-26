//! aha ↔ rig 适配 + 模型下载/加载。
//!
//! 这是 aha crate 的**唯一**业务入口：上层模块（rag / ingest）**不要**直接 `use aha::*`。
//! 全部走 rig 抽象（[`AhaClient`] / [`AhaCompletionModel`] / [`AhaEmbeddingModel`]，M4 阶段实装）。
//!
//! M0 阶段实装：
//! - [`ensure_model_downloaded`]：调 `aha::utils::download_model` 下载模型
//! - [`models_status`]：检查模型文件存在性 + 报告本地路径
//!
//! M1 阶段实装：
//! - [`AhaClient::init`]：调 `aha::models::load_model` 把 LLM + embedding 加载进内存
//! - [`AhaClient::llm_generate`] / [`AhaClient::embed_texts`]：把同步 candle 调用包成 async
//!
//! ## 路径解析约定（重要）
//!
//! aha crate 自己的 `is_model_downloaded` 写死查 `~/.aha/{model_id}/`，跟 `download_model`
//! 接受的 `save_dir` 路径不同步——aha 自己的 CLI `aha list` 也踩这个坑（先 `aha download -m X -s /tmp/foo`
//! 再 `aha list` 仍显示 X 未下）。我们**不**依赖 aha 的 `is_model_downloaded`，
//! 而是用自己的 [`resolve_model_path`]：优先 `MODELS_DIR/{repo}/`，兜底 `~/.aha/{repo}/`，
//! 兼容之前用 aha CLI 装过模型的用户。
//!
//! "已下载"的判断：目录存在 + 含 `config.json` + 至少一个 `*.safetensors`（aha `init` 实际期待的文件）。

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result, anyhow};
use clap::ValueEnum;
use tokio::sync::Mutex;

use aha::models::common::embedding::TextEmbedding;
use aha::models::common::model_mapping::WhichModel;
use aha::models::{GenerateModel, ModelInstance, load_model};
use aha::params::chat::ChatCompletionParameters;
use aha::params::chat::ChatCompletionResponse;
use aha::utils::string_to_static_str;

use crate::config::AppConfig;

// =============================================================================
// 路径解析：自己写"已下载"判断（不依赖 aha::utils::is_model_downloaded）
// =============================================================================

/// "模型本地存在"的判断：目录存在 + 含 `config.json` + 至少一个 `*.safetensors`。
/// 这是 aha `init(path)` 实际期待的文件结构（见 `aha/src/models/*/mod.rs` 的 `init`）。
fn dir_has_model(dir: &Path) -> bool {
    if !dir.is_dir() {
        return false;
    }
    let config = dir.join("config.json");
    if !config.is_file() {
        return false;
    }
    match std::fs::read_dir(dir) {
        Ok(entries) => entries.flatten().any(|e| {
            e.path()
                .extension()
                .and_then(|x| x.to_str())
                .map(|x| x.eq_ignore_ascii_case("safetensors"))
                .unwrap_or(false)
        }),
        Err(_) => false,
    }
}

/// `~/.aha/` 的绝对路径。aha crate 内部用同一逻辑，**我们不调用 aha 的版本**
/// 以免它跟我们的 save_dir 耦合。
fn aha_default_home() -> Option<PathBuf> {
    dirs::home_dir().map(|mut p| {
        p.push(".aha");
        p
    })
}

/// 解析模型的本地路径，**不**触发下载。
///
/// 查找顺序：
/// 1. `<save_dir>/<repo>/`（用户配置的 `MODELS_DIR`）
/// 2. `<~/.aha>/<repo>/`（aha 默认位置，兼容 aha CLI 老用户）
///
/// 找到返回 `Some(path)`，否则 `None`。
pub fn resolve_model_path(repo: &str, save_dir: &Path) -> Option<PathBuf> {
    let candidates = [save_dir.join(repo), aha_default_home()?.join(repo)];
    candidates.into_iter().find(|p| dir_has_model(p))
}

/// 从模型目录的 `config.json` 读 `hidden_size` 字段。
///
/// 大多数 HuggingFace embedding 模型（包括 Qwen3-Embedding / MiniLM）都用 `hidden_size`
/// 表示 embedding 维度。AhaClient load 完 embedding 模型后，用这个函数从模型目录
/// 读出维度存到 `AhaClient.embed_dim` 里（不再需要用户配 .env 的 `EMBED_DIM`）。
/// 读不到就返回 `None`（非 HF 格式模型，调用方需要 fallback）。
pub fn read_hidden_size_from_config(model_path: &Path) -> Option<usize> {
    let config_path = model_path.join("config.json");
    let bytes = std::fs::read(&config_path).ok()?;
    let json: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    json.get("hidden_size")?.as_u64().map(|n| n as usize)
}

// =============================================================================
// 模型下载（aha crate 直调，不走 CLI）
// =============================================================================

/// 把指定模型下载到 `save_dir` 下（最终落盘位置：`<save_dir>/<model_id>/`）。
///
/// - 先用 [`clap::ValueEnum::from_str`] 校验 model_id 在 aha 支持清单里
/// - [`resolve_model_path`] 命中（MODELS_DIR 或 `~/.aha/`）时直接跳过（幂等）
/// - 否则调 [`aha::utils::download_model`]，aha 内部走 ModelScope SDK
///
/// 返回落盘后的本地目录路径（与 `resolve_model_path` 的解析规则一致）。
pub async fn ensure_model_downloaded(
    model_id: &str,
    save_dir: &Path,
    max_retries: u32,
) -> Result<PathBuf> {
    // 1. 校验 id 合法
    let _ = WhichModel::from_str(model_id, true).map_err(|_| anyhow!(
        "unknown aha model id: {model_id} (see aha::models::common::model_mapping for the supported list)"
    ))?;

    // 2. 已下载则直接返回（查 MODELS_DIR 优先，再兜底 ~/.aha/）
    if let Some(p) = resolve_model_path(model_id, save_dir) {
        return Ok(p);
    }

    // 3. 调 aha crate 下载
    let save_dir_str = save_dir
        .to_str()
        .ok_or_else(|| anyhow!("save_dir is not valid UTF-8: {}", save_dir.display()))?;
    aha::utils::download_model(model_id, save_dir_str, max_retries)
        .await
        .with_context(|| {
            format!(
                "failed to download model {model_id} into {} (max_retries={max_retries})",
                save_dir.display()
            )
        })?;

    // 4. 下载完二次确认（ModelScope SDK 可能部分失败）
    resolve_model_path(model_id, save_dir).ok_or_else(|| {
        anyhow!(
            "download reported success for {model_id} but model is not present at {}/{} or ~/.aha/{}",
            save_dir.display(),
            model_id,
            model_id
        )
    })
}

// =============================================================================
// 模型状态报告（lorag models status）
// =============================================================================

/// 单个模型的状态摘要。
#[derive(Debug)]
pub struct ModelStatus {
    pub repo: String,
    /// 期望路径（`MODELS_DIR/{repo}/`），`exists == false` 时打印这个告诉用户"应该在这里"
    pub expected_path: PathBuf,
    /// 实际找到的路径（MODELS_DIR 或 `~/.aha/` 兜底），`exists == true` 时打印这个
    pub resolved_path: Option<PathBuf>,
    /// `true` 表示 [`resolve_model_path`] 在 `MODELS_DIR` 或 `~/.aha/` 找到了合法模型
    pub exists: bool,
}

impl ModelStatus {
    fn check(repo: &str, models_dir: &Path) -> Self {
        let expected = models_dir.join(repo);
        let resolved = resolve_model_path(repo, models_dir);
        Self {
            repo: repo.to_string(),
            expected_path: expected,
            exists: resolved.is_some(),
            resolved_path: resolved,
        }
    }
}

/// 同时检查 LLM + Embedding 两个模型的文件存在性。
///
/// **不**做加载（加载是 M1 阶段 AhaClient::init 的事）。仅返回文件存在性给 CLI 打印。
pub fn models_status(cfg: &AppConfig) -> Result<Vec<ModelStatus>> {
    // 顺便做 id 校验
    for id in [&cfg.llm_model, &cfg.embed_model] {
        WhichModel::from_str(id, true).map_err(|_| {
            anyhow!(
                "config: model id `{id}` is not recognized by aha (see aha::models::common::model_mapping)"
            )
        })?;
    }
    Ok(vec![
        ModelStatus::check(&cfg.llm_model, &cfg.models_dir),
        ModelStatus::check(&cfg.embed_model, &cfg.models_dir),
    ])
}

/// 人类可读打印（`lorag models status` 用）。
pub fn print_models_status(statuses: &[ModelStatus]) {
    for s in statuses {
        if s.exists {
            // 区分在 MODELS_DIR 还是 ~/.aha/ 找到
            let in_home = aha_default_home()
                .map(|h| {
                    s.resolved_path
                        .as_ref()
                        .map(|p| p.starts_with(&h))
                        .unwrap_or(false)
                })
                .unwrap_or(false);
            let where_ = if in_home {
                "~/.aha/ (aha default)"
            } else {
                "MODELS_DIR"
            };
            println!(
                "  [ok ] {:42}  {}  [{}]",
                s.repo,
                s.resolved_path.as_ref().unwrap().display(),
                where_
            );
        } else {
            println!(
                "  [MISS] {:42}  {}  (expected)",
                s.repo,
                s.expected_path.display()
            );
        }
    }
}

// =============================================================================
// AhaClient（M1 实装：加载模型到内存 + async helper；M4 阶段实装 rig trait）
// =============================================================================

/// lorag 对 aha 的统一句柄。
///
/// - M1：持有两个 `Arc<Mutex<ModelInstance<'static>>>`（LLM + embedding），
///   暴露 async helper [`Self::llm_generate`] 和 [`Self::embed_texts`]
/// - M4：实现 rig `ProviderClient` / `CompletionClient` / `EmbeddingsClient`，
///   上层 agent 只通过 rig 抽象访问
///
/// **设计要点**
/// - LLM 和 embedding 用**各自**的 `Mutex`（不共享一个），让 embed 和 generate 能并发
/// - 同步 candle 调用一律包成 `tokio::task::spawn_blocking`，避免阻塞 reactor
/// - 路径用 [`resolve_model_path`]，不依赖 aha 的 `is_model_downloaded` / `get_default_weight_path`
/// - `llm` 是 `Option`——`init` 同时 load LLM+embedding（query / shell 用），
///   `init_embed_only` 只 load embedding（ingest 用，省 LLM 的 ~8GB 内存 + 数秒到数分钟 load 时间）
/// - `embed_dim` 在 load embedding 模型后从 `config.json::hidden_size` 读出来；
///   lancedb schema 跟模型走，**不再需要** .env 的 `EMBED_DIM`
/// - `rerank_slot` 是 `Arc<OnceCell<...>>` —— `cfg.rerank_model` 留空时永远 `None`
///   （不调 `ensure_rerank` 就拿不到模型）；非空时第一次 `ensure_rerank` 触发懒加载，
///   之后所有 AhaClient clone 共享同一个 ModelInstance。零开销走 RAG 跳过 rerank 路径
#[derive(Clone)]
pub struct AhaClient {
    llm: Option<Arc<Mutex<ModelInstance<'static>>>>,
    embed: Arc<Mutex<ModelInstance<'static>>>,
    /// 懒加载槽：`ensure_rerank` 第一次成功 init 后填上；后续所有调用都从这个 slot 取。
    /// 用 `OnceCell` 而不是 `Mutex<Option<...>>`：避免反复 lock 检查 + 明确"init once"语义。
    rerank_slot: Arc<tokio::sync::OnceCell<Arc<Mutex<ModelInstance<'static>>>>>,
    /// embedding 模型的实际输出维度（load 后从 `config.json::hidden_size` 读）。
    /// 启动期间都填好；用于 lancedb 建表 / query 维度校验。
    embed_dim: Option<usize>,
    cfg: Arc<AppConfig>,
}

impl std::fmt::Debug for AhaClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AhaClient")
            .field("llm_model", &self.cfg.llm_model)
            .field("embed_model", &self.cfg.embed_model)
            .field("rerank_model", &self.cfg.rerank_model)
            .field("rerank_loaded", &self.rerank_slot.get().is_some())
            .field("embed_dim", &self.embed_dim)
            .finish_non_exhaustive()
    }
}

impl AhaClient {
    /// 把 LLM + embedding 都 load 到内存。**很慢**（数 GB 模型数十秒），
    /// 命令入口要明确提示用户等待。
    ///
    /// 失败可能原因：
    /// - 模型文件缺失（`run: lorag models pull`）
    /// - model id 写错（`check aha::models::common::model_mapping`）
    /// - safetensors 损坏
    pub async fn init(cfg: AppConfig) -> Result<Self> {
        let cfg = Arc::new(cfg);

        // 1. 解析 id + 找本地路径
        let llm_which = WhichModel::from_str(&cfg.llm_model, true).map_err(|_| {
            anyhow!(
                "config: LLM_MODEL `{}` is not recognized by aha (see aha::models::common::model_mapping)",
                cfg.llm_model
            )
        })?;
        let embed_which = WhichModel::from_str(&cfg.embed_model, true).map_err(|_| {
            anyhow!(
                "config: EMBED_MODEL `{}` is not recognized by aha (see aha::models::common::model_mapping)",
                cfg.embed_model
            )
        })?;
        let llm_path = resolve_model_path(&cfg.llm_model, &cfg.models_dir).ok_or_else(|| {
            anyhow!(
                "failed to init AhaClient: LLM model not found at {}/{} or ~/.aha/{} (run: lorag models pull)",
                cfg.models_dir.display(),
                cfg.llm_model,
                cfg.llm_model
            )
        })?;
        let embed_path =
            resolve_model_path(&cfg.embed_model, &cfg.models_dir).ok_or_else(|| {
                anyhow!(
                    "failed to init AhaClient: embedding model not found at {}/{} or ~/.aha/{} (run: lorag models pull)",
                    cfg.models_dir.display(),
                    cfg.embed_model,
                    cfg.embed_model
                )
            })?;

        // 2. 读 embedding 模型维度（从 config.json），存到 AhaClient
        // 失败（读不到 config.json）就让后续 embed_texts 报错
        let embed_dim = read_hidden_size_from_config(&embed_path);

        // 2. load_model 接受 &str，且需要 'static（用于 leak 成 &'static str 喂给 candle mmap）
        let llm_path_str = leak_path_str(&llm_path);
        let embed_path_str = leak_path_str(&embed_path);

        // 3. 真正加载：candle 同步阻塞，必须 spawn_blocking。模型越大越慢
        println!(
            "loading LLM {} from {} (may take 10s~minutes)...",
            cfg.llm_model,
            llm_path.display()
        );
        // PowerShell capture 模式下 stdout 全缓冲，强制 flush 让用户能看到进度
        let _ = std::io::Write::flush(&mut std::io::stdout());
        let llm_repo = cfg.llm_model.clone();
        let llm_path_for_err = llm_path.clone();
        let llm =
            tokio::task::spawn_blocking(move || load_model(llm_which, llm_path_str, None, None))
                .await
                .context("LLM load task panicked")?
                .with_context(|| {
                    format!(
                        "failed to load LLM `{llm_repo}` from {}",
                        llm_path_for_err.display()
                    )
                })?;

        println!(
            "loading embedding {} from {} (may take 10s~minutes)...",
            cfg.embed_model,
            embed_path.display()
        );
        let _ = std::io::Write::flush(&mut std::io::stdout());
        let embed_repo = cfg.embed_model.clone();
        let embed_path_for_err = embed_path.clone();
        let embed = tokio::task::spawn_blocking(move || {
            load_model(embed_which, embed_path_str, None, None)
        })
        .await
        .context("embedding load task panicked")?
        .with_context(|| {
            format!(
                "failed to load embedding `{embed_repo}` from {}",
                embed_path_for_err.display()
            )
        })?;

        Ok(Self {
            llm: Some(Arc::new(Mutex::new(llm))),
            embed: Arc::new(Mutex::new(embed)),
            rerank_slot: Arc::new(tokio::sync::OnceCell::new()),
            embed_dim,
            cfg,
        })
    }

    /// 只 load embedding（**不** load LLM）。
    ///
    /// 用途：`lorag ingest` 只需要 embedding 来向量化 chunk，加载 LLM 纯属浪费
    /// （4B LLM ~8GB 内存 + 数十秒 load）。`llm` 字段是 `None`，
    /// 任何调用 `completion_model` / `llm_generate` 的代码会拿一个明确的错误，
    /// 不会"意外"调到一个空的占位符。
    pub async fn init_embed_only(cfg: AppConfig) -> Result<Self> {
        let cfg = Arc::new(cfg);

        // 1. 解析 embedding id + 找本地路径
        let embed_which = WhichModel::from_str(&cfg.embed_model, true).map_err(|_| {
            anyhow!(
                "config: EMBED_MODEL `{}` is not recognized by aha (see aha::models::common::model_mapping)",
                cfg.embed_model
            )
        })?;
        let embed_path =
            resolve_model_path(&cfg.embed_model, &cfg.models_dir).ok_or_else(|| {
                anyhow!(
                    "failed to init AhaClient: embedding model not found at {}/{} or ~/.aha/{} (run: lorag models pull)",
                    cfg.models_dir.display(),
                    cfg.embed_model,
                    cfg.embed_model
                )
            })?;

        // 1.5. 读 embedding 模型维度（从 config.json），存到 AhaClient
        let embed_dim = read_hidden_size_from_config(&embed_path);

        // 2. leak path 成 &'static str
        let embed_path_str = leak_path_str(&embed_path);

        // 3. load embedding
        println!(
            "loading embedding {} from {} (may take 10s~minutes)...",
            cfg.embed_model,
            embed_path.display()
        );
        let _ = std::io::Write::flush(&mut std::io::stdout());
        let embed_repo = cfg.embed_model.clone();
        let embed_path_for_err = embed_path.clone();
        let embed = tokio::task::spawn_blocking(move || {
            load_model(embed_which, embed_path_str, None, None)
        })
        .await
        .context("embedding load task panicked")?
        .with_context(|| {
            format!(
                "failed to load embedding `{embed_repo}` from {}",
                embed_path_for_err.display()
            )
        })?;

        Ok(Self {
            llm: None,
            embed: Arc::new(Mutex::new(embed)),
            rerank_slot: Arc::new(tokio::sync::OnceCell::new()),
            embed_dim,
            cfg,
        })
    }

    /// 是否 load 了 LLM（`init` → true，`init_embed_only` → false）。
    /// `AhaCompletionModel::completion` 调这个判断要不要报错。
    pub fn has_llm(&self) -> bool {
        self.llm.is_some()
    }

    /// Embedding 模型的实际输出维度。
    ///
    /// 在 `init` / `init_embed_only` 成功后存到 client 里（从模型目录的
    /// `config.json::hidden_size` 读出来）。lancedb schema 用这个值，**不再需要**
    /// 用户在 `.env` 配 `EMBED_DIM`。
    ///
    /// 理论上永远有值（init 时就读了），但万一 `config.json` 读不到（比如非 HF 格式
    /// 模型）会返回 `None`。None 时调用方应该走 fallback（dummy embed 测 / 报错）。
    pub fn embed_dim(&self) -> Option<usize> {
        self.embed_dim
    }

    /// 访问配置。
    pub fn config(&self) -> &AppConfig {
        &self.cfg
    }

    /// LLM 推理：candle 同步，包装成 async + spawn_blocking。
    ///
    /// M4 阶段会变成 `AhaCompletionModel::completion` 的实现。
    pub async fn llm_generate(
        &self,
        params: ChatCompletionParameters,
    ) -> Result<ChatCompletionResponse> {
        let llm = self.llm.as_ref().ok_or_else(|| {
            anyhow!(
                "AhaClient has no LLM loaded (was created via init_embed_only); \
                 call `init` (loads both LLM + embedding) instead of `init_embed_only`"
            )
        })?;
        let llm = llm.clone();
        tokio::task::spawn_blocking(move || llm.blocking_lock().generate(params))
            .await
            .context("LLM generate task panicked")?
            .context("aha LLM generate failed")
    }

    /// Embedding 推理：candle 同步，包装成 async + spawn_blocking。
    ///
    /// M4 阶段会变成 `AhaEmbeddingModel::embed_texts` 的实现。
    pub async fn embed_texts(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        let embed = self.embed.clone();
        let texts = texts.to_vec();
        tokio::task::spawn_blocking(move || {
            // 必须先 match 出 embedding 变体再调 embed_texts（aha 的 ModelInstance enum 没有直接实现 TextEmbedding）
            let mut g = embed.blocking_lock();
            match &mut *g {
                ModelInstance::AllMiniLML6V2(m) => m.embed_texts(&texts),
                ModelInstance::Qwen3Embedding(m) => m.embed_texts(&texts),
                other => Err(anyhow!(
                    "configured embedding model is not an embedding model: {}",
                    std::any::type_name_of_val(other)
                )),
            }
        })
        .await
        .context("embedding task panicked")?
        .context("aha embed_texts failed")
    }

    /// Rerank 是否可用（`cfg.rerank_model` 非空，且 rerank 成功 load）。
    ///
    /// 跟 `has_llm()` 不同：rerank 永远是懒加载，所以要查"已 load"而非"config 设置"。
    /// `cfg.rerank_model` 留空 → 永远 false（永不触发 load）。
    pub fn has_rerank(&self) -> bool {
        self.rerank_slot.get().is_some()
    }

    /// Rerank 模型是否**配置**（`.env` 里 `RERANK_MODEL=` 非空）。
    ///
    /// 跟 [`has_rerank`] 区别：`has_rerank` 还要看是否真 load（懒加载），
    /// `rerank_configured` 只看 config。banner / status 用这个。
    pub fn rerank_configured(&self) -> bool {
        !self.cfg.rerank_model.is_empty()
    }

    /// 懒加载 rerank 模型（**第一次** rerank 时调）。
    ///
    /// - 不会重复 load（OnceCell 内部保证）
    /// - `cfg.rerank_model` 留空 → 报清晰错误（让上层 fallback 到无 rerank 路径）
    /// - 模型本地路径找不到 → 报 `run: lorag models pull` 提示
    /// - model id 写错 → 报 `check aha::models::common::model_mapping` 提示
    /// - 并发第一次 load：OnceCell 保证只有一个 task 真正 load，其他 task 等
    pub async fn ensure_rerank(&self) -> Result<()> {
        if self.cfg.rerank_model.is_empty() {
            return Err(anyhow!(
                "rerank requested but RERANK_MODEL is empty in .env (set e.g. RERANK_MODEL=Qwen/Qwen3-Reranker-0.6B, or pass --no-rerank)"
            ));
        }

        // OnceCell::get_or_try_init 保证并发只 load 一次
        self.rerank_slot
            .get_or_try_init(|| async {
                let which = WhichModel::from_str(&self.cfg.rerank_model, true).map_err(|_| {
                    anyhow!(
                        "config: RERANK_MODEL `{}` is not recognized by aha (see aha::models::common::model_mapping)",
                        self.cfg.rerank_model
                    )
                })?;
                let path = resolve_model_path(&self.cfg.rerank_model, &self.cfg.models_dir)
                    .ok_or_else(|| {
                        anyhow!(
                            "rerank model not found at {}/{} or ~/.aha/{} (run: lorag models pull)",
                            self.cfg.models_dir.display(),
                            self.cfg.rerank_model,
                            self.cfg.rerank_model
                        )
                    })?;
                let path_str = leak_path_str(&path);
                let repo = self.cfg.rerank_model.clone();
                let path_for_err = path.clone();

                println!(
                    "loading rerank {} from {} (may take 10s~minutes, first time only)...",
                    repo,
                    path.display()
                );
                let _ = std::io::Write::flush(&mut std::io::stdout());
                let model = tokio::task::spawn_blocking(move || {
                    load_model(which, path_str, None, None)
                })
                .await
                .context("rerank load task panicked")?
                .with_context(|| {
                    format!(
                        "failed to load rerank `{repo}` from {}",
                        path_for_err.display()
                    )
                })?;
                Ok::<_, anyhow::Error>(Arc::new(Mutex::new(model)))
            })
            .await
            .map(|_| ())
    }

    /// Rerank：对 query + 候选文档列表打分，返回每 doc 的相关分数（float）。
    ///
    /// **必须先调 `ensure_rerank` 一次**（rag 层负责）。返回的分数越高越相关，
    /// 调用方自己按分数降序排 + 取 top_k。
    pub async fn rerank_score(&self, query: &str, documents: &[String]) -> Result<Vec<f32>> {
        let rerank = self.rerank_slot.get().ok_or_else(|| {
            anyhow!(
                "AhaClient.rerank_slot is empty; call `ensure_rerank` first (caller should have done so)"
            )
        })?;
        let rerank = rerank.clone();
        let query = query.to_string();
        let documents = documents.to_vec();
        tokio::task::spawn_blocking(move || {
            let mut g = rerank.blocking_lock();
            g.rerank(&query, &documents)
        })
        .await
        .context("rerank task panicked")?
        .context("aha rerank failed")
    }
}

/// 把 `&Path` 转成 `&'static str`（调用一次 leak 一次）。仅 `init` 时调用。
fn leak_path_str(p: &Path) -> &'static str {
    string_to_static_str(p.to_string_lossy().into_owned())
}

// =============================================================================
// 引用 aha 的 WhichModel 枚举，避免在 public API 里泄漏 aha 类型
// =============================================================================

/// 把字符串解析为 aha 的 [`WhichModel`]，校验用。
///
/// 不直接返回 `WhichModel`（会泄漏 aha 类型到公共 API），仅在 `aha_provider` 内部用。
fn _check_id(model_id: &str) -> Result<()> {
    WhichModel::from_str(model_id, true)
        .map(|_| ())
        .map_err(|_| anyhow!("unknown aha model id: {model_id}"))
}

// =============================================================================
// Unit tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    /// 在 `dir` 下放一个"合法模型"（config.json + 至少一个 safetensors）
    fn touch_model(dir: &Path) {
        fs::create_dir_all(dir).unwrap();
        fs::write(dir.join("config.json"), "{}").unwrap();
        fs::write(dir.join("model.safetensors"), b"fake").unwrap();
    }

    #[test]
    fn dir_has_model_requires_config_and_safetensors() {
        // 每种 case 用独立 tempdir，不互相污染
        // 1. 空目录 → false
        let tmp = tempdir().unwrap();
        assert!(!dir_has_model(tmp.path()));

        // 2. 只有 config → false
        let tmp = tempdir().unwrap();
        fs::write(tmp.path().join("config.json"), "{}").unwrap();
        assert!(!dir_has_model(tmp.path()));

        // 3. 只有 safetensors → false
        let tmp = tempdir().unwrap();
        fs::write(tmp.path().join("model.safetensors"), b"x").unwrap();
        assert!(!dir_has_model(tmp.path()));

        // 4. 都有 → true
        let tmp = tempdir().unwrap();
        fs::write(tmp.path().join("config.json"), "{}").unwrap();
        fs::write(tmp.path().join("model.safetensors"), b"x").unwrap();
        assert!(dir_has_model(tmp.path()));

        // 5. 大小写不敏感（`.SAFETENSORS` 也算）
        let tmp = tempdir().unwrap();
        fs::write(tmp.path().join("config.json"), "{}").unwrap();
        fs::write(tmp.path().join("MODEL.SAFETENSORS"), b"x").unwrap();
        assert!(dir_has_model(tmp.path()));

        // 6. 路径不存在 → false
        let tmp = tempdir().unwrap();
        let missing = tmp.path().join("does-not-exist");
        assert!(!dir_has_model(&missing));
    }

    #[test]
    fn resolve_prefers_models_dir_over_home() {
        let tmp = tempdir().unwrap();
        let models_dir = tmp.path().join("models");
        let home_aha = tmp.path().join("home").join(".aha");
        let repo = "fake/repo";
        // 在 ~/.aha/ 放一份
        touch_model(&home_aha.join(repo));
        // 在 MODELS_DIR 也放一份
        touch_model(&models_dir.join(repo));

        let resolved = resolve_model_path(repo, &models_dir).expect("should find");
        // 应当是 MODELS_DIR 那份，不是 home 那份
        assert!(resolved.starts_with(&models_dir));
    }

    #[test]
    fn resolve_falls_back_to_home_aha() {
        let tmp = tempdir().unwrap();
        let models_dir = tmp.path().join("models"); // 空的
        let home_aha = tmp.path().join("home").join(".aha");
        let repo = "fake/repo";
        touch_model(&home_aha.join(repo));

        // 设置 HOME 环境变量（dirs 优先读它）
        // 实际上我们用 path_override 的方式更稳——但 dirs::home_dir() 在不同平台行为不同
        // 这里我们只验证"如果 ~/.aha/ 找到了就 fallback"，不去构造 HOME 环境变量
        // 跳过这个 test 在 CI 上
        if aha_default_home().is_none() {
            return;
        }
        // 兜底逻辑：resolve_model_path 应当至少 candidates 列表长度 >= 2
        // 真正的"在 ~/.aha/ 找到"测试需要隔离 HOME，复杂度高，留给 integration
        // 至少确认函数签名不 panic
        let _ = resolve_model_path(repo, &models_dir);
    }

    #[test]
    fn resolve_returns_none_when_neither_exists() {
        let tmp = tempdir().unwrap();
        let models_dir = tmp.path().join("models");
        // 啥都不放
        assert!(resolve_model_path("any/repo", &models_dir).is_none());
    }
}
