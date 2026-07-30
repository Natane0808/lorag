//! G10: `.env` settings form page.
//!
//! Lets the user view + edit every knob in [`AppConfig`], grouped into
//! Models / Retrieval / Paths / Server / Prompts sections, and persist back
//! to the `.env` file on disk. Saving shows a "restart required" banner —
//! we do **not** hot-reload, per plan §4 G10.
//!
//! Follows the established G5–G9 tokio ↔ GPUI bridge pattern.

use std::path::{Path, PathBuf};
use std::process::Command;

use gpui::prelude::*;
use gpui::{App, AsyncApp, Context, Entity, IntoElement, Render, Subscription, Window, div, px};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::scroll::ScrollableElement;
use gpui_component::select::{Select, SelectEvent, SelectState};
use gpui_component::{ActiveTheme as _, Disableable, IndexPath, StyledExt};

use crate::config::{self, AppConfig};

use super::app::AppState;
use super::autostart;

// ── Supported aha model lists (hardcoded to avoid coupling to the aha crate) ──
// Keep in sync with `aha/src/models/common/model_mapping.rs` WhichModel variants.

/// LLM models (model_type() == "llm").
const LLM_MODELS: &[&str] = &[
    "Qwen/Qwen3-0.6B",
    "Qwen/Qwen3-1.7B",
    "Qwen/Qwen3-4B",
    "Qwen/Qwen3.5-0.8B",
    "Qwen/Qwen3.5-2B",
    "Qwen/Qwen3.5-4B",
    "Qwen/Qwen3.5-9B",
    "OpenBMB/MiniCPM4-0.5B",
    "OpenBMB/MiniCPM5-1B",
    "LiquidAI/LFM2-1.2B",
    "LiquidAI/LFM2.5-1.2B-Instruct",
];

/// Embedding models (model_type() == "embedding").
const EMBED_MODELS: &[&str] = &[
    "sentence-transformers/all-MiniLM-L6-v2",
    "Qwen/Qwen3-Embedding-0.6B",
    "Qwen/Qwen3-Embedding-4B",
    "Qwen/Qwen3-Embedding-8B",
];

/// Reranker models (model_type() == "reranker").
const RERANK_MODELS: &[&str] = &[
    "Qwen/Qwen3-Reranker-0.6B",
    "Qwen/Qwen3-Reranker-4B",
    "Qwen/Qwen3-Reranker-8B",
];

/// Banner shown at the top of the page after a save/reset attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Banner {
    Saved,
    Reset,
    SaveFailed(String),
    ResetFailed(String),
    Opened(String),
}

impl Banner {
    fn text(&self) -> String {
        match self {
            Banner::Saved => "保存成功。请重启 lorag-gui 以应用新设置（服务也需要重启）。".into(),
            Banner::Reset => "重置成功：表单已恢复为磁盘当前值。".into(),
            Banner::SaveFailed(m) => format!("保存失败：{m}"),
            Banner::ResetFailed(m) => format!("重置失败：{m}"),
            Banner::Opened(p) => format!("已在系统默认编辑器打开：{p}"),
        }
    }
    fn is_error(&self) -> bool {
        matches!(self, Banner::SaveFailed(_) | Banner::ResetFailed(_))
    }
}

/// All form fields are stored as `String`. Validation happens on Save.
#[derive(Debug, Clone)]
struct FormValues {
    llm_model: String,
    embed_model: String,
    rerank_model: String,
    top_k: String,
    rerank_top_n: String,
    chunk_size: String,
    chunk_overlap: String,
    hybrid_enabled: bool,
    models_dir: String,
    lancedb_dir: String,
    sqlite_path: String,
    lorag_gui_port: String,
    prompt_system_role: String,
    prompt_rag_instruction: String,
    prompt_chat_context_instruction: String,
    prompt_bare_llm: String,
    download_max_retries: String,
    log_level: String,
}

/// State of the autostart toggle (G13).
///
/// Three states prevent races between a click and the in-flight spawn_blocking:
/// once the user flips the switch we move into `Enabling` / `Disabling` and
/// ignore further clicks until the OS call completes.
#[derive(Debug, Clone, PartialEq, Eq)]
enum AutostartState {
    /// Haven't queried the OS yet (page just opened).
    Loading,
    /// Known state; switch interactive.
    Idle(bool),
    /// Toggle in flight (OS call running in spawn_blocking).
    Toggling { target: bool },
    /// Last toggle failed; switch shows the last-known-good value plus an
    /// inline error string.
    Errored { last_known: bool, message: String },
}

impl FormValues {
    fn from_cfg(cfg: &AppConfig) -> Self {
        Self {
            llm_model: cfg.llm_model.clone(),
            embed_model: cfg.embed_model.clone(),
            rerank_model: cfg.rerank_model.clone(),
            top_k: cfg.top_k.to_string(),
            rerank_top_n: cfg.rerank_top_n.to_string(),
            chunk_size: cfg.chunk_size.to_string(),
            chunk_overlap: cfg.chunk_overlap.to_string(),
            hybrid_enabled: cfg.hybrid_enabled,
            models_dir: cfg.models_dir.to_string_lossy().to_string(),
            lancedb_dir: cfg.lancedb_dir.to_string_lossy().to_string(),
            sqlite_path: cfg.sqlite_path.to_string_lossy().to_string(),
            lorag_gui_port: cfg.lorag_gui_port.to_string(),
            prompt_system_role: cfg.prompt_system_role.clone(),
            prompt_rag_instruction: cfg.prompt_rag_instruction.clone(),
            prompt_chat_context_instruction: cfg.prompt_chat_context_instruction.clone(),
            prompt_bare_llm: cfg.prompt_bare_llm.clone(),
            download_max_retries: cfg.download_max_retries.to_string(),
            log_level: cfg.log_level.clone(),
        }
    }
}

type FieldErrors = std::collections::HashMap<&'static str, String>;

pub struct SettingsState {
    env_path: PathBuf,
    form: FormValues,
    field_errors: FieldErrors,
    banner: Option<Banner>,
    working: bool,
    inputs: InputHandles,
    _subscriptions: Vec<Subscription>,
    /// When set to `Some(cfg)`, the next render pass will call
    /// [`Self::refill_from_cfg`] using its `&mut Window` handle and clear
    /// it. Used by the async "Reset" handler, which cannot easily obtain a
    /// `&mut Window` from [`AsyncApp`].
    pending_refill: Option<AppConfig>,
    /// Per-field value updates queued from async handlers (e.g. after the
    /// native folder picker returns). Drained in render where a `&mut
    /// Window` is available.
    pending_input_sets: Vec<(Entity<InputState>, String)>,
    /// G13: in-memory state of the "开机自动启动" toggle. Not persisted to
    /// `.env` — it lives in the OS (registry / plist / .desktop file).
    autostart: AutostartState,
    /// Select widget for the LLM model field.
    llm_select: Entity<SelectState<Vec<&'static str>>>,
    /// Select widget for the embedding model field.
    embed_select: Entity<SelectState<Vec<&'static str>>>,
    /// Select widget for the rerank model field. First item is the "（禁用 rerank）" sentinel.
    rerank_select: Entity<SelectState<Vec<&'static str>>>,
}

struct InputHandles {
    llm_model: Entity<InputState>,
    embed_model: Entity<InputState>,
    rerank_model: Entity<InputState>,
    top_k: Entity<InputState>,
    rerank_top_n: Entity<InputState>,
    chunk_size: Entity<InputState>,
    chunk_overlap: Entity<InputState>,
    models_dir: Entity<InputState>,
    lancedb_dir: Entity<InputState>,
    sqlite_path: Entity<InputState>,
    lorag_gui_port: Entity<InputState>,
    prompt_system_role: Entity<InputState>,
    prompt_rag_instruction: Entity<InputState>,
    prompt_chat_context_instruction: Entity<InputState>,
    prompt_bare_llm: Entity<InputState>,
    download_max_retries: Entity<InputState>,
    log_level: Entity<InputState>,
}

impl SettingsState {
    fn new(cfg: &AppConfig, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let form = FormValues::from_cfg(cfg);
        let env_path = cfg
            .env_path
            .clone()
            .unwrap_or_else(|| PathBuf::from(".env"));

        let mut subs: Vec<Subscription> = Vec::new();

        macro_rules! single {
            ($field:ident, $placeholder:expr) => {{
                let init_val = form.$field.clone();
                let state = cx.new(|cx| InputState::new(window, cx).placeholder($placeholder));
                state.update(cx, |s, cx| s.set_value(&init_val, window, cx));
                let state_for_sub = state.clone();
                subs.push(cx.subscribe_in(
                    &state,
                    window,
                    move |this, _, ev: &InputEvent, _window, cx| {
                        if matches!(ev, InputEvent::Change | InputEvent::PressEnter { .. }) {
                            let v = state_for_sub.read(cx).text_value();
                            this.form.$field = v;
                            cx.notify();
                        }
                    },
                ));
                state
            }};
        }
        macro_rules! multi {
            ($field:ident, $placeholder:expr, $rows:expr) => {{
                let init_val = form.$field.clone();
                let state = cx.new(|cx| {
                    InputState::new(window, cx)
                        .multi_line(true)
                        .rows($rows)
                        .placeholder($placeholder)
                });
                state.update(cx, |s, cx| s.set_value(&init_val, window, cx));
                let state_for_sub = state.clone();
                subs.push(cx.subscribe_in(
                    &state,
                    window,
                    move |this, _, ev: &InputEvent, _window, cx| {
                        if matches!(ev, InputEvent::Change) {
                            let v = state_for_sub.read(cx).text_value();
                            this.form.$field = v;
                            cx.notify();
                        }
                    },
                ));
                state
            }};
        }

        let inputs = InputHandles {
            llm_model: single!(llm_model, "Qwen/Qwen3-4B"),
            embed_model: single!(embed_model, "Qwen/Qwen3-Embedding-0.6B"),
            rerank_model: single!(rerank_model, "留空禁用 rerank"),
            top_k: single!(top_k, "5"),
            rerank_top_n: single!(rerank_top_n, "50"),
            chunk_size: single!(chunk_size, "500"),
            chunk_overlap: single!(chunk_overlap, "50"),
            models_dir: single!(models_dir, "./data/models"),
            lancedb_dir: single!(lancedb_dir, "./data/lancedb"),
            sqlite_path: single!(sqlite_path, "./data/lorag.db"),
            lorag_gui_port: single!(lorag_gui_port, "3000"),
            prompt_system_role: multi!(prompt_system_role, "留空使用内置系统提示词", 8),
            prompt_rag_instruction: multi!(prompt_rag_instruction, "留空使用内置 RAG 指令", 6),
            prompt_chat_context_instruction: multi!(
                prompt_chat_context_instruction,
                "留空使用内置多轮对话指令",
                6
            ),
            prompt_bare_llm: multi!(prompt_bare_llm, "留空使用内置裸 LLM 提示词", 5),
            download_max_retries: single!(download_max_retries, "3"),
            log_level: single!(log_level, "info"),
        };

        // ── Model Select widgets (gpui_component::select) ──────────────────
        // Rerank list gets a "（禁用 rerank）" sentinel prepended; its
        // Confirm event writes "" to form.rerank_model.
        let rerank_items: Vec<&'static str> = std::iter::once("（禁用 rerank）")
            .chain(RERANK_MODELS.iter().copied())
            .collect();

        let llm_initial = LLM_MODELS
            .iter()
            .position(|m| *m == form.llm_model)
            .map(IndexPath::new);
        let embed_initial = EMBED_MODELS
            .iter()
            .position(|m| *m == form.embed_model)
            .map(IndexPath::new);
        let rerank_initial = if form.rerank_model.trim().is_empty() {
            Some(IndexPath::new(0)) // sentinel "（禁用 rerank）"
        } else {
            RERANK_MODELS
                .iter()
                .position(|m| *m == form.rerank_model)
                .map(|ix| IndexPath::new(ix + 1)) // +1 because of sentinel prefix
        };

        let llm_select =
            cx.new(|cx| SelectState::new(LLM_MODELS.to_vec(), llm_initial, window, cx));
        let embed_select =
            cx.new(|cx| SelectState::new(EMBED_MODELS.to_vec(), embed_initial, window, cx));
        let rerank_select = cx.new(|cx| SelectState::new(rerank_items, rerank_initial, window, cx));

        // Wire each Select's Confirm event back into form + InputState.
        subs.push(cx.subscribe_in(
            &llm_select,
            window,
            |this, _, ev: &SelectEvent<Vec<&'static str>>, _window, cx| {
                if let SelectEvent::Confirm(Some(value)) = ev {
                    let v = value.to_string();
                    this.form.llm_model = v.clone();
                    this.pending_input_sets
                        .push((this.inputs.llm_model.clone(), v));
                    cx.notify();
                }
            },
        ));
        subs.push(cx.subscribe_in(
            &embed_select,
            window,
            |this, _, ev: &SelectEvent<Vec<&'static str>>, _window, cx| {
                if let SelectEvent::Confirm(Some(value)) = ev {
                    let v = value.to_string();
                    this.form.embed_model = v.clone();
                    this.pending_input_sets
                        .push((this.inputs.embed_model.clone(), v));
                    cx.notify();
                }
            },
        ));
        subs.push(cx.subscribe_in(
            &rerank_select,
            window,
            |this, _, ev: &SelectEvent<Vec<&'static str>>, _window, cx| {
                if let SelectEvent::Confirm(Some(value)) = ev {
                    // Index 0 is the "（禁用 rerank）" sentinel -> empty string.
                    let v = if *value == "（禁用 rerank）" {
                        String::new()
                    } else {
                        value.to_string()
                    };
                    this.form.rerank_model = v.clone();
                    this.pending_input_sets
                        .push((this.inputs.rerank_model.clone(), v));
                    cx.notify();
                }
            },
        ));

        Self {
            env_path,
            form,
            field_errors: FieldErrors::new(),
            banner: None,
            working: false,
            inputs,
            _subscriptions: subs,
            pending_refill: None,
            pending_input_sets: Vec::new(),
            autostart: AutostartState::Loading,
            llm_select,
            embed_select,
            rerank_select,
        }
    }

    fn validate_and_build(&mut self) -> Option<AppConfig> {
        let mut errs: FieldErrors = FieldErrors::new();

        let top_k = self
            .form
            .top_k
            .trim()
            .parse::<usize>()
            .ok()
            .filter(|v| *v >= 1);
        let rerank_top_n = self
            .form
            .rerank_top_n
            .trim()
            .parse::<usize>()
            .ok()
            .filter(|v| *v >= 1);
        let chunk_size = self
            .form
            .chunk_size
            .trim()
            .parse::<usize>()
            .ok()
            .filter(|v| *v >= 1);
        let chunk_overlap = self.form.chunk_overlap.trim().parse::<usize>().ok();
        let port = self
            .form
            .lorag_gui_port
            .trim()
            .parse::<u16>()
            .ok()
            .filter(|p| *p > 0);
        let download_max_retries = self
            .form
            .download_max_retries
            .trim()
            .parse::<u32>()
            .ok()
            .filter(|v| *v >= 1);

        if self.form.llm_model.trim().is_empty() {
            errs.insert("llm_model", "LLM_MODEL 必填".into());
        }
        if self.form.embed_model.trim().is_empty() {
            errs.insert("embed_model", "EMBED_MODEL 必填".into());
        }
        if top_k.is_none() {
            errs.insert("top_k", "必须是 ≥ 1 的整数".into());
        }
        if rerank_top_n.is_none() {
            errs.insert("rerank_top_n", "必须是 ≥ 1 的整数".into());
        }
        if chunk_size.is_none() {
            errs.insert("chunk_size", "必须是 ≥ 1 的整数".into());
        }
        if chunk_overlap.is_none() {
            errs.insert("chunk_overlap", "必须是 ≥ 0 的整数".into());
        }
        if port.is_none() {
            errs.insert("lorag_gui_port", "端口必须在 1..=65535".into());
        }
        if download_max_retries.is_none() {
            errs.insert("download_max_retries", "必须是 ≥ 1 的整数".into());
        }

        if let (Some(k), Some(n)) = (top_k, rerank_top_n)
            && n <= k
        {
            errs.insert(
                "rerank_top_n",
                format!("RERANK_TOP_N ({n}) 必须大于 TOP_K ({k})"),
            );
        }
        if let (Some(cs), Some(co)) = (chunk_size, chunk_overlap)
            && co >= cs
        {
            errs.insert(
                "chunk_overlap",
                format!("CHUNK_OVERLAP ({co}) 必须 < CHUNK_SIZE ({cs})"),
            );
        }

        self.field_errors = errs;
        if !self.field_errors.is_empty() {
            return None;
        }

        Some(AppConfig {
            llm_model: self.form.llm_model.trim().to_string(),
            embed_model: self.form.embed_model.trim().to_string(),
            rerank_model: self.form.rerank_model.trim().to_string(),
            models_dir: PathBuf::from(self.form.models_dir.trim()),
            lancedb_dir: PathBuf::from(self.form.lancedb_dir.trim()),
            sqlite_path: PathBuf::from(self.form.sqlite_path.trim()),
            chunk_size: chunk_size.unwrap(),
            chunk_overlap: chunk_overlap.unwrap(),
            top_k: top_k.unwrap(),
            rerank_top_n: rerank_top_n.unwrap(),
            lorag_gui_port: port.unwrap(),
            hybrid_enabled: self.form.hybrid_enabled,
            download_max_retries: download_max_retries.unwrap(),
            log_level: if self.form.log_level.trim().is_empty() {
                "info".to_string()
            } else {
                self.form.log_level.trim().to_string()
            },
            prompt_system_role: self.form.prompt_system_role.clone(),
            prompt_rag_instruction: self.form.prompt_rag_instruction.clone(),
            prompt_chat_context_instruction: self.form.prompt_chat_context_instruction.clone(),
            prompt_bare_llm: self.form.prompt_bare_llm.clone(),
            env_path: Some(self.env_path.clone()),
        })
    }

    fn refill_from_cfg(&mut self, cfg: &AppConfig, window: &mut Window, cx: &mut Context<Self>) {
        self.form = FormValues::from_cfg(cfg);
        self.field_errors.clear();
        let f = &self.form;
        self.inputs
            .llm_model
            .update(cx, |s, cx| s.set_value(&f.llm_model, window, cx));
        self.inputs
            .embed_model
            .update(cx, |s, cx| s.set_value(&f.embed_model, window, cx));
        self.inputs
            .rerank_model
            .update(cx, |s, cx| s.set_value(&f.rerank_model, window, cx));
        self.inputs
            .top_k
            .update(cx, |s, cx| s.set_value(&f.top_k, window, cx));
        self.inputs
            .rerank_top_n
            .update(cx, |s, cx| s.set_value(&f.rerank_top_n, window, cx));
        self.inputs
            .chunk_size
            .update(cx, |s, cx| s.set_value(&f.chunk_size, window, cx));
        self.inputs
            .chunk_overlap
            .update(cx, |s, cx| s.set_value(&f.chunk_overlap, window, cx));
        self.inputs
            .models_dir
            .update(cx, |s, cx| s.set_value(&f.models_dir, window, cx));
        self.inputs
            .lancedb_dir
            .update(cx, |s, cx| s.set_value(&f.lancedb_dir, window, cx));
        self.inputs
            .sqlite_path
            .update(cx, |s, cx| s.set_value(&f.sqlite_path, window, cx));
        self.inputs
            .lorag_gui_port
            .update(cx, |s, cx| s.set_value(&f.lorag_gui_port, window, cx));
        self.inputs
            .prompt_system_role
            .update(cx, |s, cx| s.set_value(&f.prompt_system_role, window, cx));
        self.inputs.prompt_rag_instruction.update(cx, |s, cx| {
            s.set_value(&f.prompt_rag_instruction, window, cx)
        });
        self.inputs
            .prompt_chat_context_instruction
            .update(cx, |s, cx| {
                s.set_value(&f.prompt_chat_context_instruction, window, cx)
            });
        self.inputs
            .prompt_bare_llm
            .update(cx, |s, cx| s.set_value(&f.prompt_bare_llm, window, cx));
        self.inputs
            .download_max_retries
            .update(cx, |s, cx| s.set_value(&f.download_max_retries, window, cx));
        self.inputs
            .log_level
            .update(cx, |s, cx| s.set_value(&f.log_level, window, cx));
    }

    fn snapshot(&self) -> SettingsSnapshot {
        SettingsSnapshot {
            working: self.working,
            banner: self.banner.clone(),
            hybrid_enabled: self.form.hybrid_enabled,
            autostart: self.autostart.clone(),
        }
    }

    /// Drain the pending refill (if any) and any queued input value updates
    /// by applying them to the underlying input states. Called from
    /// [`SettingsPage::render`] where a `&mut Window` is readily available.
    fn take_pending_refill(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AppConfig> {
        for (entity, value) in self.pending_input_sets.drain(..) {
            entity.update(cx, |inp, cx| inp.set_value(&value, window, cx));
        }
        let cfg = self.pending_refill.take()?;
        self.refill_from_cfg(&cfg, window, cx);
        Some(cfg)
    }

    fn inputs_snapshot(&self) -> InputSnapshot {
        let i = &self.inputs;
        InputSnapshot {
            top_k: i.top_k.clone(),
            rerank_top_n: i.rerank_top_n.clone(),
            chunk_size: i.chunk_size.clone(),
            chunk_overlap: i.chunk_overlap.clone(),
            models_dir: i.models_dir.clone(),
            lancedb_dir: i.lancedb_dir.clone(),
            sqlite_path: i.sqlite_path.clone(),
            lorag_gui_port: i.lorag_gui_port.clone(),
            prompt_system_role: i.prompt_system_role.clone(),
            prompt_rag_instruction: i.prompt_rag_instruction.clone(),
            prompt_chat_context_instruction: i.prompt_chat_context_instruction.clone(),
            prompt_bare_llm: i.prompt_bare_llm.clone(),
            download_max_retries: i.download_max_retries.clone(),
            log_level: i.log_level.clone(),
        }
    }

    fn errors_snapshot(&self) -> FieldErrors {
        self.field_errors.clone()
    }
}

struct SettingsSnapshot {
    working: bool,
    banner: Option<Banner>,
    hybrid_enabled: bool,
    autostart: AutostartState,
}

#[derive(Clone)]
struct InputSnapshot {
    top_k: Entity<InputState>,
    rerank_top_n: Entity<InputState>,
    chunk_size: Entity<InputState>,
    chunk_overlap: Entity<InputState>,
    models_dir: Entity<InputState>,
    lancedb_dir: Entity<InputState>,
    sqlite_path: Entity<InputState>,
    lorag_gui_port: Entity<InputState>,
    prompt_system_role: Entity<InputState>,
    prompt_rag_instruction: Entity<InputState>,
    prompt_chat_context_instruction: Entity<InputState>,
    prompt_bare_llm: Entity<InputState>,
    download_max_retries: Entity<InputState>,
    log_level: Entity<InputState>,
}

pub struct SettingsPage {
    app: Entity<AppState>,
    state: Entity<SettingsState>,
}

impl SettingsPage {
    pub fn new(app: Entity<AppState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let cfg = app.read(cx).cfg.clone();
        let state = cx.new(|cx| SettingsState::new(&cfg, window, cx));

        // Kick off the initial autostart status query (G13). This touches the OS
        // registry / filesystem so it runs in spawn_blocking and writes back
        // via cx.update once complete.
        let tokio_handle = app.read(cx).tokio_handle.clone();
        let state_for_init = state.clone();
        cx.spawn(async move |_window, cx: &mut AsyncApp| {
            let join = tokio_handle.spawn_blocking(autostart::is_enabled);
            let res = join.await;
            state_for_init.update(cx, |s, _cx| match res {
                Ok(Ok(enabled)) => {
                    s.autostart = AutostartState::Idle(enabled);
                }
                Ok(Err(e)) => {
                    s.autostart = AutostartState::Errored {
                        last_known: false,
                        message: format!("无法读取开机自启状态：{e:#}"),
                    };
                }
                Err(join_err) => {
                    s.autostart = AutostartState::Errored {
                        last_known: false,
                        message: format!("后台任务中断 ({join_err})"),
                    };
                }
            });
        })
        .detach();

        Self { app, state }
    }
}

impl Render for SettingsPage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Drain any pending refill posted by the async Reset handler.
        self.state.update(cx, |s, cx| {
            s.take_pending_refill(window, cx);
        });

        let inputs = self.state.read(cx).inputs_snapshot();
        let snapshot = self.state.read(cx).snapshot();
        let errs = self.state.read(cx).errors_snapshot();

        let state_save = self.state.clone();
        let app_save = self.app.clone();
        let state_reset = self.state.clone();
        let app_reset = self.app.clone();
        let state_open = self.state.clone();
        let app_open = self.app.clone();
        let state_hybrid = self.state.clone();
        let state_models = self.state.clone();
        let app_models = self.app.clone();
        let state_lance = self.state.clone();
        let app_lance = self.app.clone();
        let state_sqlite = self.state.clone();
        let app_sqlite = self.app.clone();

        let mut body = div()
            .size_full()
            .p_6()
            .flex()
            .flex_col()
            .gap_4()
            .child(div().text_xl().font_semibold().child("设置"))
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(
                        "修改后点\"保存\"写入 .env 文件。设置不会热重载，保存后需重启 lorag-gui。",
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .flex_wrap()
                    .child(
                        Button::new("settings-save")
                            .label("保存")
                            .primary()
                            .loading(snapshot.working)
                            .disabled(snapshot.working)
                            .on_click(move |_ev, _window, cx: &mut App| {
                                on_save_clicked(&state_save, &app_save, cx);
                            }),
                    )
                    .child(
                        Button::new("settings-reset")
                            .label("重置")
                            .disabled(snapshot.working)
                            .on_click(move |_ev, _window, cx: &mut App| {
                                on_reset_clicked(&state_reset, &app_reset, cx);
                            }),
                    )
                    .child(
                        Button::new("settings-open-env")
                            .label("打开 .env 文件")
                            .disabled(snapshot.working)
                            .on_click(move |_ev, _win, cx: &mut App| {
                                on_open_env_clicked(&state_open, &app_open, cx);
                            }),
                    ),
            );

        if let Some(b) = &snapshot.banner {
            let color = if b.is_error() {
                gpui::rgb(0xef4444)
            } else {
                gpui::rgb(0x10b981)
            };
            body = body.child(
                div()
                    .p_3()
                    .rounded_md()
                    .border_1()
                    .border_color(color)
                    .text_color(color)
                    .text_sm()
                    .child(b.text()),
            );
        }

        let muted = cx.theme().muted_foreground;
        let border = cx.theme().border;
        let _ = (border, muted);

        let mut form = div().flex().flex_col().gap_6();

        form = form.child(section_header("模型"));
        form = form.child(field_row(
            "对话模型 (LLM_MODEL)",
            Select::new(&self.state.read(cx).llm_select)
                .placeholder("（请选择模型）")
                .w_full()
                .into_any_element(),
            errs.get("llm_model"),
        ));
        form = form.child(field_row(
            "嵌入模型 (EMBED_MODEL)",
            Select::new(&self.state.read(cx).embed_select)
                .placeholder("（请选择模型）")
                .w_full()
                .into_any_element(),
            errs.get("embed_model"),
        ));
        form = form.child(field_row(
            "重排模型 (RERANK_MODEL)",
            Select::new(&self.state.read(cx).rerank_select)
                .placeholder("（请选择模型）")
                .w_full()
                .into_any_element(),
            errs.get("rerank_model"),
        ));
        form = form.child(div().text_xs().text_color(gpui::rgb(0x9ca3af)).child(
            "RERANK_MODEL 留空 = 禁用 rerank。换 embedding 模型后需在 CLI 执行 `lorag reindex`。",
        ));

        form = form.child(section_header("检索"));
        form = form.child(
            div()
                .max_w(px(480.))
                .grid()
                .grid_cols(2)
                .gap_3()
                .child(field_row(
                    "TOP_K",
                    Input::new(&inputs.top_k).w_full(),
                    errs.get("top_k"),
                ))
                .child(field_row(
                    "RERANK_TOP_N",
                    Input::new(&inputs.rerank_top_n).w_full(),
                    errs.get("rerank_top_n"),
                ))
                .child(field_row(
                    "CHUNK_SIZE",
                    Input::new(&inputs.chunk_size).w_full(),
                    errs.get("chunk_size"),
                ))
                .child(field_row(
                    "CHUNK_OVERLAP",
                    Input::new(&inputs.chunk_overlap).w_full(),
                    errs.get("chunk_overlap"),
                )),
        );
        form = form.child(hybrid_row(snapshot.hybrid_enabled, state_hybrid));

        form = form.child(section_header("路径"));
        form = form.child(path_row(
            "模型目录 (MODELS_DIR)",
            &inputs.models_dir,
            "settings-browse-models",
            &state_models,
            &app_models,
            PathKind::Folder,
            errs.get("models_dir"),
        ));
        form = form.child(path_row(
            "向量库目录 (LANCEDB_DIR)",
            &inputs.lancedb_dir,
            "settings-browse-lancedb",
            &state_lance,
            &app_lance,
            PathKind::Folder,
            errs.get("lancedb_dir"),
        ));
        form = form.child(path_row(
            "SQLite 元数据路径 (SQLITE_PATH)",
            &inputs.sqlite_path,
            "settings-browse-sqlite",
            &state_sqlite,
            &app_sqlite,
            PathKind::File,
            errs.get("sqlite_path"),
        ));

        form = form.child(section_header("服务 / 日志"));
        form = form.child(
            div()
                .max_w(px(480.))
                .grid()
                .grid_cols(2)
                .gap_3()
                .child(field_row(
                    "GUI 服务端口 (LORAG_GUI_PORT)",
                    Input::new(&inputs.lorag_gui_port).w_full(),
                    errs.get("lorag_gui_port"),
                ))
                .child(field_row(
                    "日志级别 (LOG_LEVEL)",
                    Input::new(&inputs.log_level).w_full(),
                    errs.get("log_level"),
                ))
                .child(field_row(
                    "下载重试 (DOWNLOAD_MAX_RETRIES)",
                    Input::new(&inputs.download_max_retries).w_full(),
                    errs.get("download_max_retries"),
                )),
        );
        form = form.child(
            div()
                .text_xs()
                .text_color(gpui::rgb(0x9ca3af))
                .child("端口默认 3000。端口冲突会导致服务启动失败（CLI 可用 --port 覆盖一次）。"),
        );

        form = form.child(section_header("提示词"));
        form = form.child(field_row(
            "PROMPT_SYSTEM_ROLE",
            Input::new(&inputs.prompt_system_role).w_full().h(px(200.)),
            None,
        ));
        form = form.child(field_row(
            "PROMPT_RAG_INSTRUCTION",
            Input::new(&inputs.prompt_rag_instruction)
                .w_full()
                .h(px(160.)),
            None,
        ));
        form = form.child(field_row(
            "PROMPT_CHAT_CONTEXT_INSTRUCTION",
            Input::new(&inputs.prompt_chat_context_instruction)
                .w_full()
                .h(px(160.)),
            None,
        ));
        form = form.child(field_row(
            "PROMPT_BARE_LLM",
            Input::new(&inputs.prompt_bare_llm).w_full().h(px(140.)),
            None,
        ));
        form = form.child(
            div()
                .text_xs()
                .text_color(gpui::rgb(0x9ca3af))
                .child("提示：清空字段后保存，将使用内置默认（含 5 条防注入铁律）。"),
        );

        // G13: "系统" section with the autostart toggle. Always last.
        form = form.child(section_header("系统"));
        form = form.child(div().max_w(px(640.)).child(autostart_row(
            snapshot.autostart,
            self.state.clone(),
            self.app.clone(),
        )));

        body = body.child(form);

        let _ = window;
        div()
            .size_full()
            .flex()
            .flex_col()
            .child(div().size_full().overflow_y_scrollbar().child(body))
    }
}

fn section_header(title: &'static str) -> gpui::AnyElement {
    div()
        .text_base()
        .font_medium()
        .child(title)
        .into_any_element()
}

fn field_row(
    label: &'static str,
    input: impl IntoElement,
    error: Option<&String>,
) -> gpui::AnyElement {
    let mut col = div()
        .flex()
        .flex_col()
        .gap_1()
        .child(div().text_sm().font_medium().child(label))
        .child(div().max_w(px(640.)).child(input));
    if let Some(e) = error {
        col = col.child(div().text_xs().text_color(gpui::red()).child(e.clone()));
    }
    col.into_any_element()
}

// ── Model dropdown helpers ────────────────────────────────────────────────
// Model fields are wired to gpui_component::select::Select (see
// `SettingsState::llm_select` / `embed_select` / `rerank_select` and the
// subscriptions registered in `SettingsState::new`). The Select's
// `Confirm` event writes the value back into `form.<field>` and queues a
// `pending_input_sets` entry so the hidden InputState stays in sync (the
// save/validation flow remains unchanged).

fn hybrid_row(checked: bool, state_hybrid: Entity<SettingsState>) -> gpui::AnyElement {
    div()
        .flex()
        .items_center()
        .justify_between()
        .p_3()
        .rounded_md()
        .border_1()
        .border_color(gpui::rgb(0x374151))
        .max_w(px(640.))
        .child(
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(div().text_sm().font_medium().child("HYBRID_ENABLED"))
                .child(
                    div()
                        .text_xs()
                        .text_color(gpui::rgb(0x9ca3af))
                        .child("启用 BM25 FTS5 + 向量 RRF 混合检索（大文档量时建议开启）"),
                ),
        )
        .child(
            gpui_component::switch::Switch::new("settings-hybrid")
                .checked(checked)
                .on_click(move |_ev, _window, cx: &mut App| {
                    state_hybrid.update(cx, |st, cx| {
                        st.form.hybrid_enabled = !st.form.hybrid_enabled;
                        cx.notify();
                    });
                }),
        )
        .into_any_element()
}

fn autostart_row(
    state: AutostartState,
    state_entity: Entity<SettingsState>,
    app: Entity<AppState>,
) -> gpui::AnyElement {
    // Derive the current checked visual + disabled flag from the state machine.
    let (checked, disabled) = match state {
        AutostartState::Loading => (false, true),
        AutostartState::Idle(v) => (v, false),
        AutostartState::Toggling { target } => (target, true),
        AutostartState::Errored { last_known, .. } => (last_known, false),
    };

    let mut col = div().flex().flex_col().gap_1();
    col = col.child(
        div()
            .flex()
            .items_center()
            .justify_between()
            .p_3()
            .rounded_md()
            .border_1()
            .border_color(gpui::rgb(0x374151))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(div().text_sm().font_medium().child("开机自动启动"))
                    .child(
                        div()
                            .text_xs()
                            .text_color(gpui::rgb(0x9ca3af))
                            .child("登录系统后自动启动 lorag-gui（托盘常驻）。"),
                    ),
            )
            .child({
                let state_for_click = state_entity.clone();
                let app_for_click = app.clone();
                gpui_component::switch::Switch::new("settings-autostart")
                    .checked(checked)
                    .disabled(disabled)
                    .on_click(move |_ev, _window, cx: &mut App| {
                        on_autostart_toggled(&state_for_click, &app_for_click, cx);
                    })
            }),
    );

    if let AutostartState::Errored { message, .. } = &state {
        col = col.child(
            div()
                .text_xs()
                .text_color(gpui::red())
                .child(message.clone()),
        );
    }

    col.into_any_element()
}

#[derive(Clone, Copy)]
enum PathKind {
    Folder,
    File,
}

#[allow(clippy::too_many_arguments)]
fn path_row(
    label: &'static str,
    input_state: &Entity<InputState>,
    btn_id: &'static str,
    page_state: &Entity<SettingsState>,
    app: &Entity<AppState>,
    kind: PathKind,
    error: Option<&String>,
) -> gpui::AnyElement {
    let state_btn = page_state.clone();
    let app_btn = app.clone();
    let field =
        div()
            .flex()
            .items_center()
            .gap_2()
            .max_w(px(640.))
            .child(Input::new(input_state).flex_1().min_w_0())
            .child(Button::new(btn_id).label("浏览...").on_click(
                move |_ev, _win, cx: &mut App| {
                    on_browse_clicked(&state_btn, &app_btn, btn_id, kind, cx);
                },
            ));
    let mut col = div()
        .flex()
        .flex_col()
        .gap_1()
        .child(div().text_sm().font_medium().child(label))
        .child(field);
    if let Some(e) = error {
        col = col.child(div().text_xs().text_color(gpui::red()).child(e.clone()));
    }
    col.into_any_element()
}

// ── Handlers ──────────────────────────────────────────────────────────────

fn on_save_clicked(state: &Entity<SettingsState>, app: &Entity<AppState>, cx: &mut App) {
    let tokio_handle = app.read(cx).tokio_handle.clone();

    let cfg_built = state.update(cx, |s, cx| {
        s.banner = None;
        s.working = true;
        cx.notify();
        s.validate_and_build()
    });

    let Some(cfg) = cfg_built else {
        state.update(cx, |s, cx| {
            s.working = false;
            s.banner = Some(Banner::SaveFailed(
                "表单包含非法字段，请参见各字段下方的红色提示。".into(),
            ));
            cx.notify();
        });
        return;
    };

    let env_path = state.read(cx).env_path.clone();
    let state_for_task = state.clone();

    cx.spawn(async move |cx: &mut AsyncApp| {
        let join = tokio_handle.spawn_blocking(move || cfg.save_to_dotenv(&env_path));
        let result = join.await;
        state_for_task.update(cx, |s, cx| {
            s.working = false;
            match result {
                Ok(Ok(())) => {
                    s.banner = Some(Banner::Saved);
                    s.field_errors.clear();
                }
                Ok(Err(e)) => {
                    s.banner = Some(Banner::SaveFailed(format!("{e:#}")));
                }
                Err(join_err) => {
                    s.banner = Some(Banner::SaveFailed(format!("后台任务中断 ({join_err})")));
                }
            }
            cx.notify();
        });
    })
    .detach();
}

fn on_reset_clicked(state: &Entity<SettingsState>, app: &Entity<AppState>, cx: &mut App) {
    let tokio_handle = app.read(cx).tokio_handle.clone();
    let env_path = state.read(cx).env_path.clone();
    let state_for_task = state.clone();

    state.update(cx, |s, cx| {
        s.banner = None;
        s.working = true;
        s.field_errors.clear();
        cx.notify();
    });

    cx.spawn(async move |cx: &mut AsyncApp| {
        let p = env_path.clone();
        let join = tokio_handle.spawn_blocking(move || config::reload_from_path(Path::new(&p)));
        let result = join.await;

        let cfg = match result {
            Ok(Ok(c)) => c,
            Ok(Err(e)) => {
                state_for_task.update(cx, |s, cx| {
                    s.working = false;
                    s.banner = Some(Banner::ResetFailed(format!("{e:#}")));
                    cx.notify();
                });
                return;
            }
            Err(join_err) => {
                state_for_task.update(cx, |s, cx| {
                    s.working = false;
                    s.banner = Some(Banner::ResetFailed(format!("后台任务中断 ({join_err})")));
                    cx.notify();
                });
                return;
            }
        };

        state_for_task.update(cx, |s, cx| {
            s.working = false;
            // Defer the actual input-state mutation to the next render,
            // where we have a `&mut Window`.
            s.pending_refill = Some(cfg);
            s.banner = Some(Banner::Reset);
            cx.notify();
        });
    })
    .detach();
}

fn on_open_env_clicked(state: &Entity<SettingsState>, app: &Entity<AppState>, cx: &mut App) {
    let tokio_handle = app.read(cx).tokio_handle.clone();
    let env_path = state.read(cx).env_path.clone();
    let state_for_task = state.clone();

    cx.spawn(async move |cx: &mut AsyncApp| {
        let p_for_blocking = env_path.clone();
        let p_for_banner = env_path.clone();
        let join = tokio_handle.spawn_blocking(move || open_in_os_editor(&p_for_blocking));
        let res = join.await;
        state_for_task.update(cx, |s, cx| {
            match res {
                Ok(Ok(())) => {
                    s.banner = Some(Banner::Opened(p_for_banner.display().to_string()));
                }
                Ok(Err(e)) => {
                    s.banner = Some(Banner::SaveFailed(format!("打开 .env 失败：{e:#}")));
                }
                Err(join_err) => {
                    s.banner = Some(Banner::SaveFailed(format!("后台任务中断 ({join_err})")));
                }
            }
            cx.notify();
        });
    })
    .detach();
}

fn on_browse_clicked(
    state: &Entity<SettingsState>,
    app: &Entity<AppState>,
    field: &'static str,
    kind: PathKind,
    cx: &mut App,
) {
    let tokio_handle = app.read(cx).tokio_handle.clone();
    let state_for_task = state.clone();

    cx.spawn(async move |cx: &mut AsyncApp| {
        let join = tokio_handle.spawn_blocking(move || match kind {
            PathKind::Folder => rfd::FileDialog::new()
                .set_title("选择目录")
                .pick_folder()
                .map(|p| p.to_string_lossy().to_string()),
            PathKind::File => rfd::FileDialog::new()
                .set_title("选择文件")
                .pick_file()
                .map(|p| p.to_string_lossy().to_string()),
        });
        let picked = match join.await {
            Ok(Some(p)) => p,
            _ => return,
        };

        state_for_task.update(cx, |s, cx| {
            let input = match field {
                "settings-browse-models" => s.inputs.models_dir.clone(),
                "settings-browse-lancedb" => s.inputs.lancedb_dir.clone(),
                "settings-browse-sqlite" => s.inputs.sqlite_path.clone(),
                _ => return,
            };
            match field {
                "settings-browse-models" => s.form.models_dir = picked.clone(),
                "settings-browse-lancedb" => s.form.lancedb_dir = picked.clone(),
                "settings-browse-sqlite" => s.form.sqlite_path = picked.clone(),
                _ => {}
            }
            s.pending_input_sets.push((input, picked));
            cx.notify();
        });
    })
    .detach();
}

/// G13: user flipped the "开机自动启动" switch.
///
/// Gate on the state machine: ignore clicks while Loading / Toggling / and
/// derive the target value from whatever the current `Idle`/`Errored` state
/// says (so a double-click doesn't queue two OS calls).
fn on_autostart_toggled(state: &Entity<SettingsState>, app: &Entity<AppState>, cx: &mut App) {
    let target_and_handle = state.update(cx, |s, cx| {
        let current = match s.autostart {
            AutostartState::Loading | AutostartState::Toggling { .. } => return None,
            AutostartState::Idle(v) => v,
            AutostartState::Errored { last_known, .. } => last_known,
        };
        let target = !current;
        s.autostart = AutostartState::Toggling { target };
        cx.notify();
        Some((target, app.read(cx).tokio_handle.clone()))
    });

    let Some((target, tokio_handle)) = target_and_handle else {
        return;
    };

    let state_for_task = state.clone();
    cx.spawn(async move |cx: &mut AsyncApp| {
        let join = tokio_handle.spawn_blocking(move || {
            if target {
                autostart::enable()
            } else {
                autostart::disable()
            }
        });
        let res = join.await;
        state_for_task.update(cx, |s, cx| {
            match res {
                Ok(Ok(())) => {
                    s.autostart = AutostartState::Idle(target);
                }
                Ok(Err(e)) => {
                    s.autostart = AutostartState::Errored {
                        last_known: !target,
                        message: if target {
                            format!("启用开机自启失败：{e:#}")
                        } else {
                            format!("关闭开机自启失败：{e:#}")
                        },
                    };
                }
                Err(join_err) => {
                    s.autostart = AutostartState::Errored {
                        last_known: !target,
                        message: format!("后台任务中断 ({join_err})"),
                    };
                }
            }
            cx.notify();
        });
    })
    .detach();
}

fn open_in_os_editor(path: &Path) -> anyhow::Result<()> {
    #[cfg(target_os = "windows")]
    {
        use anyhow::Context as _;
        Command::new("notepad")
            .arg(path)
            .spawn()
            .with_context(|| format!("failed to launch notepad for {}", path.display()))?;
    }
    #[cfg(target_os = "macos")]
    {
        use anyhow::Context as _;
        Command::new("open")
            .arg(path)
            .spawn()
            .with_context(|| format!("failed to open {} via `open`", path.display()))?;
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        use anyhow::Context as _;
        Command::new("xdg-open")
            .arg(path)
            .spawn()
            .with_context(|| format!("failed to xdg-open {}", path.display()))?;
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos", unix)))]
    {
        let _ = path;
        anyhow::bail!("open-in-editor not supported on this platform");
    }
    Ok(())
}

trait InputValueExt {
    fn text_value(&self) -> String;
}

impl InputValueExt for InputState {
    fn text_value(&self) -> String {
        self.value().to_string()
    }
}
