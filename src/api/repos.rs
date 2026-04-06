// src/api/repos.rs
// Repo management + chat endpoints for RustCode
// TODO: add auth middleware (reuse existing API key layer)
//
// RAG pipeline (per-request):
//   1. classify prompt → task_kind, model target
//   2. build repo context (tree/todos/symbols) if repo_id provided
//   3. search RAG index for semantically similar chunks
//   4. enhance prompt with RAG snippets prepended
//   5. check Redis/LRU cache (cache key covers rag-enriched prompt)
//   6. dispatch to Ollama (local) or Grok (remote)
//   7. cache response fire-and-forget

use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{delete, get, post},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, error, info, warn};

use crate::cache_layer::{CacheConfig, CacheLayer};
use crate::model_router::{CompletionRequest, ModelRouter, ModelTarget};
use crate::ollama_client::OllamaClient;
use crate::repo_sync::{RegisteredRepo, RepoSyncService, SyncResult};
use crate::research::worker::{enhance_prompt_with_rag, search_rag_context};

// ---------------------------------------------------------------------------
// AppState
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct RepoAppState {
    pub sync_service: Arc<RwLock<RepoSyncService>>,
    pub model_router: Arc<ModelRouter>,
    pub ollama_client: Arc<OllamaClient>,
    // Optional Grok client for remote completions — `None` when XAI_API_KEY is unset.
    pub grok_client: Option<Arc<crate::grok_client::GrokClient>>,
    // Multi-tier cache (in-memory LRU + optional Redis).
    pub cache: Arc<CacheLayer>,
}

// Chat response cache TTL in seconds (1 hour).
const CHAT_CACHE_TTL_SECS: u64 = 3600;

// ---------------------------------------------------------------------------
// Router builder
// ---------------------------------------------------------------------------

pub fn repo_router(state: RepoAppState) -> Router {
    Router::new()
        // Repo management
        .route("/repos", get(list_repos).post(register_repo))
        .route("/repos/{id}", get(get_repo))
        .route("/repos/{id}", delete(remove_repo))
        .route("/repos/{id}/sync", post(sync_repo))
        .route("/repos/{id}/context", get(get_repo_context))
        .route("/repos/{id}/todos", get(get_repo_todos))
        .route("/repos/{id}/symbols", get(get_repo_symbols))
        .route("/repos/{id}/tree", get(get_repo_tree))
        // Chat
        .route("/chat", post(chat))
        .route("/chat/repos/{id}", post(chat_with_repo))
        // Ollama status
        .route("/ollama/health", get(ollama_health))
        .route("/ollama/models", get(ollama_models))
        .with_state(state)
}

// ---------------------------------------------------------------------------
// Request / Response types
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct RegisterRepoRequest {
    pub name: String,
    pub local_path: String,
    pub remote_url: Option<String>,
    pub branch: Option<String>,
    // If true, immediately run a sync after registration.
    pub sync_on_register: Option<bool>,
}

#[derive(Debug, Serialize)]
pub struct RegisterRepoResponse {
    pub id: String,
    pub name: String,
    pub message: String,
    pub sync_result: Option<SyncResult>,
}

#[derive(Debug, Deserialize)]
pub struct ChatRequest {
    pub message: String,
    // Optional: inject context from a specific registered repo.
    pub repo_id: Option<String>,
    // If true, force the remote model regardless of task classification.
    pub force_remote: Option<bool>,
    // Conversation history for multi-turn chat.
    pub history: Option<Vec<ChatMessage>>,
    // If true, bypass cache and always call the model.
    pub no_cache: Option<bool>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ChatMessage {
    pub role: String, // "user" | "assistant"
    pub content: String,
}

#[derive(Debug, Serialize)]
pub struct ChatResponse {
    pub reply: String,
    pub task_kind: String,
    pub model_used: String,
    pub used_fallback: bool,
    pub repo_context_injected: bool,
    pub tokens_used: Option<u32>,
    // True when this response was served from cache.
    pub cached: bool,
}

// ApiError is defined in crate::api::types and re-exported here for
// backwards-compat with any code that was importing it from this module.
pub use crate::api::types::ApiError;

// ---------------------------------------------------------------------------
// Repo handlers
// ---------------------------------------------------------------------------

async fn list_repos(State(state): State<RepoAppState>) -> impl IntoResponse {
    let service = state.sync_service.read().await;
    let repos: Vec<_> = service
        .list_repos()
        .iter()
        .map(|r| {
            serde_json::json!({
                "id": r.id,
                "name": r.name,
                "path": r.local_path,
                "branch": r.branch,
                "last_synced": r.last_synced,
                "remote_url": r.remote_url,
            })
        })
        .collect();
    Json(repos)
}

async fn register_repo(
    State(state): State<RepoAppState>,
    Json(req): Json<RegisterRepoRequest>,
) -> impl IntoResponse {
    info!(name = %req.name, path = %req.local_path, "Registering repo via API");

    let mut repo = RegisteredRepo::new(&req.name, &req.local_path);
    if let Some(url) = req.remote_url {
        repo.remote_url = Some(url);
    }
    if let Some(branch) = req.branch {
        repo.branch = branch;
    }

    let mut service = state.sync_service.write().await;
    let id = match service.register(repo).await {
        Ok(id) => id,
        Err(e) => {
            error!(error = %e, "Failed to register repo");
            return ApiError::internal(e.to_string()).into_response();
        }
    };

    // Optionally run immediate sync
    let sync_result = if req.sync_on_register.unwrap_or(false) {
        match service.sync(&id).await {
            Ok(r) => Some(r),
            Err(e) => {
                error!(error = %e, "Sync after register failed");
                None
            }
        }
    } else {
        None
    };

    Json(RegisterRepoResponse {
        id: id.clone(),
        name: req.name,
        message: format!("Repo '{}' registered successfully", id),
        sync_result,
    })
    .into_response()
}

async fn get_repo(State(state): State<RepoAppState>, Path(id): Path<String>) -> impl IntoResponse {
    let service = state.sync_service.read().await;
    match service.get_repo(&id) {
        Some(repo) => Json(serde_json::json!({
            "id": repo.id,
            "name": repo.name,
            "path": repo.local_path,
            "branch": repo.branch,
            "last_synced": repo.last_synced,
            "remote_url": repo.remote_url,
            "cache_dir": repo.cache_dir(),
        }))
        .into_response(),
        None => ApiError::not_found(format!("Repo '{}' not found", id)).into_response(),
    }
}

async fn remove_repo(
    State(state): State<RepoAppState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let mut service = state.sync_service.write().await;
    if service.remove_repo_async(&id).await {
        Json(serde_json::json!({ "message": format!("Repo '{}' removed", id) })).into_response()
    } else {
        ApiError::not_found(format!("Repo '{}' not found", id)).into_response()
    }
}

async fn sync_repo(State(state): State<RepoAppState>, Path(id): Path<String>) -> impl IntoResponse {
    info!(repo = %id, "Manual sync triggered via API");
    let mut service = state.sync_service.write().await;
    match service.sync(&id).await {
        Ok(result) => Json(result).into_response(),
        Err(e) => ApiError::internal(e.to_string()).into_response(),
    }
}

async fn get_repo_context(
    State(state): State<RepoAppState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let service = state.sync_service.read().await;
    match service.build_prompt_context(&id).await {
        Ok(ctx) => (StatusCode::OK, ctx).into_response(),
        Err(e) => ApiError::not_found(e.to_string()).into_response(),
    }
}

async fn get_repo_todos(
    State(state): State<RepoAppState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    serve_cache_file(&state, &id, |r| r.todos_path()).await
}

async fn get_repo_symbols(
    State(state): State<RepoAppState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    serve_cache_file(&state, &id, |r| r.symbols_path()).await
}

async fn get_repo_tree(
    State(state): State<RepoAppState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    serve_cache_file(&state, &id, |r| r.tree_path()).await
}

// ---------------------------------------------------------------------------
// Ollama status endpoints
// ---------------------------------------------------------------------------

async fn ollama_health(State(state): State<RepoAppState>) -> impl IntoResponse {
    let reachable = state.ollama_client.health_check().await;
    let status = if reachable { "ok" } else { "unreachable" };
    Json(serde_json::json!({
        "status": status,
        "reachable": reachable,
    }))
}

async fn ollama_models(State(state): State<RepoAppState>) -> impl IntoResponse {
    match state.ollama_client.list_models().await {
        Ok(models) => Json(serde_json::json!({ "models": models })).into_response(),
        Err(e) => ApiError::internal(e.to_string()).into_response(),
    }
}

// ---------------------------------------------------------------------------
// Chat handlers
// ---------------------------------------------------------------------------

async fn chat(
    State(state): State<RepoAppState>,
    Json(req): Json<ChatRequest>,
) -> impl IntoResponse {
    handle_chat(state, req, None).await
}

async fn chat_with_repo(
    State(state): State<RepoAppState>,
    Path(repo_id): Path<String>,
    Json(req): Json<ChatRequest>,
) -> impl IntoResponse {
    handle_chat(state, req, Some(repo_id)).await
}

async fn handle_chat(
    state: RepoAppState,
    req: ChatRequest,
    repo_id_override: Option<String>,
) -> impl IntoResponse {
    let effective_repo_id = repo_id_override.or(req.repo_id.clone());
    let bypass_cache = req.no_cache.unwrap_or(false);

    // 1. Classify and route — use async LLM classification (Ollama one-shot) with
    //    keyword fallback so the router gets smarter as the local model warms up.
    let (task_kind, mut target) = state.model_router.route_prompt_async(&req.message).await;

    // Let the caller force remote
    if req.force_remote.unwrap_or(false) {
        target = state
            .model_router
            .route(&crate::model_router::TaskKind::ArchitecturalReason);
    }

    // 2. Build repo context if a repo is specified
    let (repo_context, context_injected) = if let Some(ref rid) = effective_repo_id {
        let service = state.sync_service.read().await;
        match service.build_prompt_context(rid).await {
            Ok(ctx) => (Some(ctx), true),
            Err(e) => {
                warn!(repo = %rid, error = %e, "Failed to build repo context — continuing without it");
                (None, false)
            }
        }
    } else {
        (None, false)
    };

    // 3. RAG search — find semantically similar chunks from the knowledge base
    //    and prepend them to the user message before building the full prompt.
    //    We only do this when the index is likely to have relevant content
    //    (skip for trivial/greeting-style tasks to save latency).
    let rag_enriched_message = {
        // Get a pool reference from the sync service's embedded DB if available,
        // otherwise skip RAG gracefully.
        let maybe_pool = {
            let svc = state.sync_service.read().await;
            svc.db_pool()
        };

        if let Some(pool) = maybe_pool {
            match search_rag_context(&pool, &req.message, 4).await {
                Ok(rag_results) if !rag_results.is_empty() => {
                    debug!(
                        hits = rag_results.len(),
                        "RAG: enriching prompt with {} chunk(s)",
                        rag_results.len()
                    );
                    enhance_prompt_with_rag(&req.message, &rag_results)
                }
                Ok(_) => req.message.clone(),
                Err(e) => {
                    warn!(error = %e, "RAG search failed — using plain prompt");
                    req.message.clone()
                }
            }
        } else {
            req.message.clone()
        }
    };

    // 4. Build completion request (uses RAG-enriched message as the user prompt)
    let completion_req = CompletionRequest::for_stub(&rag_enriched_message, repo_context);
    let final_prompt = completion_req.build_prompt();

    // 5. Build cache key  (SHA-256 of  model_target | prompt | repo_id)
    //    Note: we key on `final_prompt` which already includes RAG context,
    //    so identical queries with identical RAG results share the same cache slot.
    let cache_key = build_cache_key(&target, &final_prompt, effective_repo_id.as_deref());

    // 6. Check cache
    if !bypass_cache {
        match state.cache.get::<CachedChatResponse>(&cache_key).await {
            Ok(Some(cached)) => {
                debug!(cache_key = %cache_key, "Cache hit for chat request");
                return Json(ChatResponse {
                    reply: cached.reply,
                    task_kind: format!("{:?}", task_kind),
                    model_used: cached.model_used,
                    used_fallback: cached.used_fallback,
                    repo_context_injected: context_injected,
                    tokens_used: cached.tokens_used,
                    cached: true,
                })
                .into_response();
            }
            Ok(None) => {}
            Err(e) => {
                warn!(error = %e, "Cache read error — proceeding without cache");
            }
        }
    }

    // 7. Dispatch to the appropriate model
    let (reply, model_used, used_fallback, tokens_used) =
        dispatch_completion(&state, &completion_req, &target).await;

    // 8. Store in cache (fire-and-forget — don't fail the request on cache write error)
    let cache_value = CachedChatResponse {
        reply: reply.clone(),
        model_used: model_used.clone(),
        used_fallback,
        tokens_used,
    };
    let cache_clone = Arc::clone(&state.cache);
    tokio::spawn(async move {
        if let Err(e) = cache_clone
            .set(&cache_key, &cache_value, Some(CHAT_CACHE_TTL_SECS))
            .await
        {
            warn!(error = %e, "Failed to write chat response to cache");
        }
    });

    Json(ChatResponse {
        reply,
        task_kind: format!("{:?}", task_kind),
        model_used,
        used_fallback,
        repo_context_injected: context_injected,
        tokens_used,
        cached: false,
    })
    .into_response()
}

// ---------------------------------------------------------------------------
// Model dispatch — Ollama local or Grok remote
// ---------------------------------------------------------------------------

async fn dispatch_completion(
    state: &RepoAppState,
    req: &CompletionRequest,
    target: &ModelTarget,
) -> (String, String, bool, Option<u32>) {
    let final_prompt = req.build_prompt();
    let system = req.system_prompt.as_deref();

    match target {
        ModelTarget::Local { model, .. } => {
            debug!(model = %model, "Dispatching to local Ollama");
            match state
                .ollama_client
                .complete(system, &final_prompt, req.temperature, req.max_tokens)
                .await
            {
                Ok(resp) => {
                    let tokens = resp
                        .prompt_tokens
                        .and_then(|p| resp.completion_tokens.map(|c| p + c));
                    (resp.content, resp.model_used, resp.used_fallback, tokens)
                }
                Err(e) => {
                    error!(error = %e, "Both Ollama and its fallback failed");
                    let err_reply = format!(
                        "// ERROR: model dispatch failed — {}\n// Prompt was: {}...",
                        e,
                        &final_prompt.chars().take(80).collect::<String>()
                    );
                    (err_reply, model.clone(), false, None)
                }
            }
        }

        ModelTarget::Remote { model, api_key } => {
            debug!(model = %model, "Dispatching to remote Grok");

            // Use the injected GrokClient if available, otherwise fall back to
            // constructing one from the key in the ModelTarget itself.
            let result = if let Some(ref grok) = state.grok_client {
                grok.ask_tracked(&final_prompt, None, "chat").await
            } else {
                // Build a one-shot client — no cost-DB tracking, but functional.
                use crate::db::Database;
                match Database::new("data/rustcode.db").await {
                    Ok(db) => {
                        let client = crate::grok_client::GrokClient::new(api_key.clone(), db);
                        client.ask_tracked(&final_prompt, None, "chat").await
                    }
                    Err(e) => Err(anyhow::anyhow!("DB init for one-shot Grok failed: {}", e)),
                }
            };

            match result {
                Ok(resp) => {
                    let tokens = (resp.prompt_tokens + resp.completion_tokens) as u32;
                    (resp.content, model.clone(), false, Some(tokens))
                }
                Err(e) => {
                    error!(error = %e, "Grok API call failed");
                    let err_reply = format!(
                        "// ERROR: remote model call failed — {}\n// Model: {}",
                        e, model
                    );
                    (err_reply, model.clone(), false, None)
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Cache key builder
// ---------------------------------------------------------------------------

// Build a deterministic, URL-safe cache key for a chat request.
// Format: `chat:<hex16>` where hex16 is the first 16 chars of
// SHA-256( target_label | "\0" | prompt | "\0" | repo_id ).
fn build_cache_key(target: &ModelTarget, prompt: &str, repo_id: Option<&str>) -> String {
    let target_label = match target {
        ModelTarget::Local { model, .. } => format!("local:{}", model),
        ModelTarget::Remote { model, .. } => format!("remote:{}", model),
    };

    let mut hasher = Sha256::new();
    hasher.update(target_label.as_bytes());
    hasher.update(b"\x00");
    hasher.update(prompt.as_bytes());
    hasher.update(b"\x00");
    hasher.update(repo_id.unwrap_or("").as_bytes());
    let digest = hasher.finalize();

    format!("chat:{}", hex::encode(&digest[..8]))
}

// ---------------------------------------------------------------------------
// Cached response shape (stored in CacheLayer as bincode)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CachedChatResponse {
    reply: String,
    model_used: String,
    used_fallback: bool,
    tokens_used: Option<u32>,
}

// ---------------------------------------------------------------------------
// Helper: serve a .rustcode/ cache file as text/JSON
// ---------------------------------------------------------------------------

async fn serve_cache_file<F>(
    state: &RepoAppState,
    repo_id: &str,
    path_fn: F,
) -> impl IntoResponse + use<F>
where
    F: FnOnce(&RegisteredRepo) -> std::path::PathBuf,
{
    let service = state.sync_service.read().await;
    let repo = match service.get_repo(repo_id) {
        Some(r) => r,
        None => {
            return ApiError::not_found(format!("Repo '{}' not found", repo_id)).into_response();
        }
    };

    let file_path = path_fn(repo);
    match tokio::fs::read_to_string(&file_path).await {
        Ok(content) => (StatusCode::OK, content).into_response(),
        Err(_) => ApiError::not_found(format!(
            "Cache not found at {:?} — run /api/v1/repos/{}/sync first",
            file_path, repo_id
        ))
        .into_response(),
    }
}

// ---------------------------------------------------------------------------
// RepoAppState builder helper (used in server.rs startup)
// ---------------------------------------------------------------------------

impl RepoAppState {
    // Construct a `RepoAppState` wiring all clients from environment variables.
    //
    // * `sync_service` — caller-owned `Arc<RwLock<RepoSyncService>>`
    // * `model_router` — caller-owned `Arc<ModelRouter>`
    // * `grok_client`  — `None` when `XAI_API_KEY` is unset (local-only mode)
    pub async fn from_env(
        sync_service: Arc<RwLock<RepoSyncService>>,
        model_router: Arc<ModelRouter>,
        grok_client: Option<Arc<crate::grok_client::GrokClient>>,
    ) -> Self {
        // Build Ollama client, attaching Grok as its fallback if available.
        let ollama_client = {
            let base = crate::ollama_client::OllamaClient::from_env();
            if let Some(ref grok) = grok_client {
                Arc::new(base.with_fallback(Arc::clone(grok)))
            } else {
                Arc::new(base)
            }
        };

        // Build the cache layer (memory + Redis if REDIS_URL is set).
        let cache_config = CacheConfig {
            enable_redis: std::env::var("REDIS_URL").is_ok(),
            redis_url: std::env::var("REDIS_URL").ok(),
            redis_prefix: std::env::var("CACHE_PREFIX").unwrap_or_else(|_| "rustcode:".to_string()),
            ..CacheConfig::default()
        };

        let cache = match CacheLayer::new(cache_config).await {
            Ok(c) => Arc::new(c),
            Err(e) => {
                warn!(error = %e, "Failed to initialise cache layer — using memory-only fallback");
                Arc::new(
                    CacheLayer::new(CacheConfig::development())
                        .await
                        .expect("In-memory CacheLayer must always succeed"),
                )
            }
        };

        Self {
            sync_service,
            model_router,
            ollama_client,
            grok_client,
            cache,
        }
    }
}
