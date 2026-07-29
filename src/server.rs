//! axum HTTP server for the lorag Web UI (M10).
//!
//! Provides:
//! - `POST /api/chat`    — streaming multi-turn chat (SSE)
//! - `POST /api/query`   — streaming RAG query (SSE)
//! - `GET /api/status`   — system info (JSON)
//! - `GET /api/sessions`    — session history list (JSON)
//! - `DELETE /api/sessions/{session_id}` — delete a session and its messages
//! - `GET /*`               — embedded frontend (rust-embed, built from web/dist/)
//!
//! ## SSE protocol
//!
//! Each endpoint returns `Content-Type: text/event-stream` with `data:` lines:
//! - `data: {"token":"..."}` — a text token from the LLM
//! - `data: {"error":"..."}` — fatal error, stream ends after this
//! - `data: {"session_id":"..."}` — (chat only) first event: session id for persistence
//! - `data: [DONE]` — end of stream
//!
//! ## Dev vs prod
//!
//! - **Dev**: `cd web && bun dev` → Vite on :5173 proxies `/api/*` → axum on :3000
//! - **Prod release**: `cd web && bun run build && cargo build --features cuda`
//!   → single binary with embedded frontend, zero external dependencies

use std::convert::Infallible;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result};
use async_stream::stream;
use axum::{
    Json, Router,
    body::Body,
    extract::State,
    http::{StatusCode, Uri, header},
    response::{
        IntoResponse, Response,
        sse::{Event, KeepAlive, Sse},
    },
    routing::{delete, get, post},
};
use rust_embed::RustEmbed;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use crate::aha_provider::AhaClient;
use crate::config::AppConfig;
use crate::rag;
use crate::store::sqlite_store::SqliteStore;

/// Frontend assets embedded at compile time (via `rust-embed`).
/// Build with: `cd web && bun run build && cargo build`
#[derive(RustEmbed)]
#[folder = "web/dist"]
struct FrontendAssets;

/// Max chat history messages per turn (same as CLI).
const MAX_HISTORY_MESSAGES: usize = 20;

// ──────────────────────────────────────────────────────────────────────
// Shared state
// ──────────────────────────────────────────────────────────────────────

/// State shared across all axum handlers.
pub struct AppState {
    pub client: Arc<Mutex<AhaClient>>,
    pub cfg: Arc<AppConfig>,
    pub sqlite: Arc<Mutex<SqliteStore>>,
}

// ──────────────────────────────────────────────────────────────────────
// Request / response types
// ──────────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct ChatRequest {
    message: String,
    #[serde(default)]
    session_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct QueryRequest {
    question: String,
}

#[derive(Debug, Serialize)]
struct StatusResponse {
    llm_model: String,
    embed_model: String,
    rerank_model: String,
    sources_count: i64,
    chunks_count: i64,
}

// ──────────────────────────────────────────────────────────────────────
// Router
// ──────────────────────────────────────────────────────────────────────

/// Build the axum router (without static file serving — that's layered in
/// `serve` so static files can override `/api` routes).
pub fn build_router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/api/chat", post(handle_chat))
        .route("/api/query", post(handle_query))
        .route("/api/status", get(handle_status))
        .route("/api/sessions", get(handle_sessions))
        .route("/api/sessions/{session_id}", get(handle_session_messages))
        .route("/api/sessions/{session_id}", delete(handle_delete_session))
        .with_state(state)
}

/// Start the axum server on the given port.
///
/// Serves the embedded frontend (from `rust-embed`) at `/*`;
/// no external web/dist/ directory needed at runtime.
///
/// 行为完全等价于 M10 原版：内部委托给 [`start_with_shutdown`]，传入一个永不触发的
/// shutdown future（`futures::future::pending()`），保持 100% 向后兼容。
pub async fn start(state: Arc<AppState>, port: u16) -> Result<()> {
    start_with_shutdown(state, port, futures::future::pending()).await
}

/// Start the axum server on the given port with graceful shutdown support (M11).
///
/// 跟 [`start`] 完全一致，但接受一个 `shutdown` future——一旦它 resolve，axum 会
/// 停止接受新连接并等现有连接处理完后退出（graceful shutdown）。
///
/// `lorag tray` 用这个把托盘的"Quit"菜单信号（`oneshot::Receiver<()>`）接进来，
/// 让用户右键托盘 → Quit → 进程能干净退出。
pub async fn start_with_shutdown<F>(state: Arc<AppState>, port: u16, shutdown: F) -> Result<()>
where
    F: std::future::Future<Output = ()> + Send + 'static,
{
    use tower_http::cors::CorsLayer;

    let api_router = build_router(state);
    let app = Router::new()
        .merge(api_router)
        .fallback(serve_static)
        .layer(CorsLayer::permissive());

    let addr = std::net::SocketAddr::from(([0, 0, 0, 0], port));
    println!("lorag serve: http://localhost:{port}");
    println!();

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .context(format!("failed to bind to port {port}"))?;

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown)
        .await
        .context("axum server error")
}

// ──────────────────────────────────────────────────────────────────────
// Handlers
// ──────────────────────────────────────────────────────────────────────

/// `POST /api/chat` — streaming multi-turn RAG chat.
///
/// Request body: `{"message": "...", "session_id": "..."}`
/// - `session_id` optional: first call creates one and sends it back
///
/// Returns SSE stream: `{"session_id":"..."}` → `{"token":"..."}` … → `[DONE]`
///
/// Uses `async_stream::stream!` instead of `tokio::spawn` to avoid `Send`
/// issues with `SqliteStore` (which is `!Sync` due to `rusqlite::Connection`'s `RefCell`).
async fn handle_chat(
    State(state): State<Arc<AppState>>,
    Json(body): Json<ChatRequest>,
) -> Sse<impl futures::Stream<Item = Result<Event, Infallible>>> {
    let s = stream! {
        let msg = body.message.trim().to_string();
        if msg.is_empty() {
            let json = serde_json::json!({"error": "message is empty"}).to_string();
            yield Ok(Event::default().data(json));
            yield Ok(Event::default().data("[DONE]"));
            return;
        }

        let session_id = body
            .session_id
            .filter(|s| !s.is_empty())
            .unwrap_or_else(generate_session_id);

        // Send session_id first
        let sid_json = serde_json::json!({"session_id": session_id}).to_string();
        yield Ok(Event::default().data(sid_json));

        // 1. Load history
        let history = {
            let sqlite = state.sqlite.lock().await;
            match sqlite.load_recent_messages(&session_id, MAX_HISTORY_MESSAGES) {
                Ok(h) => h,
                Err(e) => {
                    let json = serde_json::json!({"error": format!("{e:#}")}).to_string();
                    yield Ok(Event::default().data(json));
                    yield Ok(Event::default().data("[DONE]"));
                    return;
                }
            }
        };

        // 2. Retrieve chunks (vector-only: SqliteStore is !Sync, can't share across threads)
        let client = state.client.lock().await;
        let cfg = &state.cfg;
        let rerank_enabled = !cfg.rerank_model.is_empty();
        let chunks = match rag::retrieve_chunks_send(
            &client, cfg, &msg, cfg.top_k, rerank_enabled, cfg.rerank_top_n,
        ).await {
            Ok(c) => c,
            Err(e) => {
                let s = format!("{e:#}");
                if rag::is_recoverable_error(&s) {
                    Vec::new() // fallback: no context
                } else {
                    let json = serde_json::json!({"error": s}).to_string();
                    yield Ok(Event::default().data(json));
                    yield Ok(Event::default().data("[DONE]"));
                    return;
                }
            }
        };

        // 3. Build preamble
        let preamble = rag::build_chat_preamble(cfg, &history, &chunks);

        // 4. Stream LLM tokens
        let mut token_rx = match rag::llm_complete_stream(&client, cfg, preamble, &msg).await {
            Ok(rx) => rx,
            Err(e) => {
                let json = serde_json::json!({"error": format!("{e:#}")}).to_string();
                yield Ok(Event::default().data(json));
                yield Ok(Event::default().data("[DONE]"));
                return;
            }
        };

        drop(client); // release lock before SQLite persistence

        let mut answer = String::new();
        while let Some(result) = token_rx.recv().await {
            match result {
                Ok(token) => {
                    answer.push_str(&token);
                    let json = serde_json::json!({"token": token}).to_string();
                    yield Ok(Event::default().data(json));
                }
                Err(e) => {
                    let json = serde_json::json!({"error": format!("{e:#}")}).to_string();
                    yield Ok(Event::default().data(json));
                    yield Ok(Event::default().data("[DONE]"));
                    return;
                }
            }
        }

        // 5. Persist messages
        {
            let sqlite = state.sqlite.lock().await;
            let _ = sqlite.append_message(&session_id, "user", &msg);
            let _ = sqlite.append_message(&session_id, "assistant", &answer);
        }

        yield Ok(Event::default().data("[DONE]"));
    };

    Sse::new(s).keep_alive(KeepAlive::default())
}

/// `POST /api/query` — streaming one-shot RAG query.
///
/// Request body: `{"question": "..."}`
///
/// Returns SSE stream: `{"token":"..."}` … → `[DONE]`
async fn handle_query(
    State(state): State<Arc<AppState>>,
    Json(body): Json<QueryRequest>,
) -> Sse<impl futures::Stream<Item = Result<Event, Infallible>>> {
    let s = stream! {
        let q = body.question.trim().to_string();
        if q.is_empty() {
            let json = serde_json::json!({"error": "question is empty"}).to_string();
            yield Ok(Event::default().data(json));
            yield Ok(Event::default().data("[DONE]"));
            return;
        }

        let client = state.client.lock().await;
        let cfg = &state.cfg;
        let rerank_enabled = !cfg.rerank_model.is_empty();

        // Retrieve → preamble → stream (vector-only, SqliteStore !Sync)
        let context = match rag::retrieve_chunks_send(
            &client, cfg, &q, cfg.top_k, rerank_enabled, cfg.rerank_top_n,
        ).await {
            Ok(chunks) => rag::format_chunks_for_context(&chunks),
            Err(e) => {
                let s = format!("{e:#}");
                if rag::is_recoverable_error(&s) {
                    // fallback to bare LLM
                    let bare_preamble = cfg.prompt_bare_llm.clone();
                    let mut token_rx = match rag::llm_complete_stream(&client, cfg, bare_preamble, &q).await {
                        Ok(rx) => rx,
                        Err(e2) => {
                            let json = serde_json::json!({"error": format!("{e2:#}")}).to_string();
                            yield Ok(Event::default().data(json));
                            yield Ok(Event::default().data("[DONE]"));
                            return;
                        }
                    };
                    drop(client);
                    while let Some(result) = token_rx.recv().await {
                        match result {
                            Ok(token) => {
                                let json = serde_json::json!({"token": token}).to_string();
                                yield Ok(Event::default().data(json));
                            }
                            Err(e2) => {
                                let json = serde_json::json!({"error": format!("{e2:#}")}).to_string();
                                yield Ok(Event::default().data(json));
                                yield Ok(Event::default().data("[DONE]"));
                                return;
                            }
                        }
                    }
                    yield Ok(Event::default().data("[DONE]"));
                    return;
                }
                let json = serde_json::json!({"error": s}).to_string();
                yield Ok(Event::default().data(json));
                yield Ok(Event::default().data("[DONE]"));
                return;
            }
        };

        let preamble = rag::build_rag_preamble(cfg, &context);
        let mut token_rx = match rag::llm_complete_stream(&client, cfg, preamble, &q).await {
            Ok(rx) => rx,
            Err(e) => {
                let json = serde_json::json!({"error": format!("{e:#}")}).to_string();
                yield Ok(Event::default().data(json));
                yield Ok(Event::default().data("[DONE]"));
                return;
            }
        };

        drop(client);

        while let Some(result) = token_rx.recv().await {
            match result {
                Ok(token) => {
                    let json = serde_json::json!({"token": token}).to_string();
                    yield Ok(Event::default().data(json));
                }
                Err(e) => {
                    let json = serde_json::json!({"error": format!("{e:#}")}).to_string();
                    yield Ok(Event::default().data(json));
                    yield Ok(Event::default().data("[DONE]"));
                    return;
                }
            }
        }

        yield Ok(Event::default().data("[DONE]"));
    };

    Sse::new(s).keep_alive(KeepAlive::default())
}

/// `GET /api/status` — system info (JSON).
async fn handle_status(
    State(state): State<Arc<AppState>>,
) -> Result<Json<StatusResponse>, (StatusCode, String)> {
    let sqlite = state.sqlite.lock().await;
    let sources = sqlite.list_sources().unwrap_or_default();
    let sources_count = sources.len() as i64;
    let chunks_count: i64 = sources.iter().map(|r| r.chunk_count as i64).sum();

    Ok(Json(StatusResponse {
        llm_model: state.cfg.llm_model.clone(),
        embed_model: state.cfg.embed_model.clone(),
        rerank_model: if state.cfg.rerank_model.is_empty() {
            "(not configured)".to_string()
        } else {
            state.cfg.rerank_model.clone()
        },
        sources_count,
        chunks_count,
    }))
}

/// `GET /api/sessions` — session history list (M10 chat sidebar).
async fn handle_sessions(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Vec<crate::models::SessionInfo>>, (StatusCode, String)> {
    let sqlite = state.sqlite.lock().await;
    let sessions = sqlite.list_sessions().unwrap_or_default();
    Ok(Json(sessions))
}

/// `GET /api/sessions/{session_id}` — load messages for a session (M10).
async fn handle_session_messages(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(session_id): axum::extract::Path<String>,
) -> Result<Json<Vec<crate::models::MessageRecord>>, (StatusCode, String)> {
    let sqlite = state.sqlite.lock().await;
    let messages = sqlite
        .load_recent_messages(&session_id, MAX_HISTORY_MESSAGES)
        .unwrap_or_default();
    Ok(Json(messages))
}

/// `DELETE /api/sessions/{session_id}` — delete a session and all its messages (M10).
async fn handle_delete_session(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(session_id): axum::extract::Path<String>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let sqlite = state.sqlite.lock().await;
    let n = sqlite
        .delete_session(&session_id)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))?;
    Ok(Json(
        serde_json::json!({"deleted": n, "session_id": session_id}),
    ))
}

// ──────────────────────────────────────────────────────────────────────
// Static file serving (from embedded assets)
// ──────────────────────────────────────────────────────────────────────

/// Serve a file from `FrontendAssets` (embedded `web/dist/`).
///
/// SPA fallback: unknown paths serve `index.html` so the SolidJS router handles them.
async fn serve_static(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    let path = if path.is_empty() { "index.html" } else { path };

    if let Some(file) = FrontendAssets::get(path) {
        let mime = guess_mime(path);
        return Response::builder()
            .header(header::CONTENT_TYPE, mime)
            .body(Body::from(file.data))
            .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response());
    }

    // SPA fallback: let SolidJS router handle unknown paths
    if let Some(file) = FrontendAssets::get("index.html") {
        return Response::builder()
            .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
            .body(Body::from(file.data))
            .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response());
    }

    (StatusCode::NOT_FOUND, "not found").into_response()
}

fn guess_mime(path: &str) -> &'static str {
    let lower = path.to_lowercase();
    if lower.ends_with(".html") {
        "text/html; charset=utf-8"
    } else if lower.ends_with(".js") || lower.ends_with(".mjs") {
        "application/javascript"
    } else if lower.ends_with(".css") {
        "text/css"
    } else if lower.ends_with(".svg") {
        "image/svg+xml"
    } else if lower.ends_with(".png") {
        "image/png"
    } else if lower.ends_with(".ico") {
        "image/x-icon"
    } else if lower.ends_with(".json") {
        "application/json"
    } else if lower.ends_with(".woff2") {
        "font/woff2"
    } else {
        "application/octet-stream"
    }
}

// ──────────────────────────────────────────────────────────────────────
// Helpers
// ──────────────────────────────────────────────────────────────────────

fn generate_session_id() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    format!("s{:08x}", COUNTER.fetch_add(1, Ordering::Relaxed))
}
