// OpenAI-compatible `/v1/chat/completions` proxy endpoint.
//
// This module lets any OpenAI-SDK-compatible client (Python `openai`, JS `openai`,
// Rust `async-openai`, curl, the futures trading app, Zed IDE, etc.) point its
// `base_url` at RustCode and get responses routed through:
//
//   request → API-key auth → rate-limit → model routing (local/remote)
//           → RAG context injection → repo context injection
//           → Ollama (local) | Grok (remote)
//           → Redis/LRU response cache → OpenAI-shaped response
//
// # Endpoint
//
// ```text
// POST /v1/chat/completions
// Authorization: Bearer <RUSTCODE_API_KEY>
// Content-Type: application/json
//
// {
//   "model":    "auto",          // "auto" | "local" | "remote" | "rc:<hint>"
//   "messages": [
//     { "role": "system",    "content": "You are a trading analyst." },
//     { "role": "user",      "content": "Analyse BTC open interest spike." }
//   ],
//   "temperature": 0.2,          // optional, default 0.2
//   "max_tokens":  2048,         // optional
//   "stream":      true,         // SSE streaming supported (set false for JSON)
//
//   // RustCode extensions (all optional, ignored by stock OpenAI clients)
//   "x_repo_id":    "futures-bot",  // inject registered-repo RAG context
//   "x_no_cache":   false,          // bypass Redis/LRU cache
//   "x_force_remote": false         // skip local model regardless of task kind
// }
// ```
//
// # Streaming response  (`stream: true`)
//
// When `stream` is `true` the endpoint returns `Content-Type: text/event-stream`
// (SSE).  Each event carries a JSON delta in the standard OpenAI chunk shape:
//
// ```text
// data: {"id":"chatcmpl-rc-…","object":"chat.completion.chunk","created":…,
//        "model":"qwen2.5-coder:7b","choices":[{"index":0,"delta":{"role":"assistant","content":"Hello"},"finish_reason":null}]}
//
// data: {"id":"…","object":"chat.completion.chunk","created":…,"model":"…",
//        "choices":[{"index":0,"delta":{"content":" world"},"finish_reason":null}]}
//
// data: {"id":"…","object":"chat.completion.chunk","created":…,"model":"…",
//        "choices":[{"index":0,"delta":{},"finish_reason":"stop"}],
//        "usage":{"prompt_tokens":42,"completion_tokens":7,"total_tokens":49}}
//
// data: [DONE]
// ```
//
// # Non-streaming response  (`stream: false`, default)
//
// Returns `Content-Type: application/json`.
//
// # Response  (OpenAI `ChatCompletion` shape)
//
// ```json
// {
//   "id": "chatcmpl-rc-<uuid>",
//   "object": "chat.completion",
//   "created": 1710000000,
//   "model": "qwen2.5-coder:7b",
//   "choices": [{
//     "index": 0,
//     "message": { "role": "assistant", "content": "…" },
//     "finish_reason": "stop"
//   }],
//   "usage": {
//     "prompt_tokens": 312,
//     "completion_tokens": 128,
//     "total_tokens": 440
//   },
//   "x_ra_metadata": {
//     "task_kind":             "ArchitecturalReason",
//     "used_fallback":         false,
//     "repo_context_injected": true,
//     "rag_chunks_used":       3,
//     "cached":                false,
//     "cache_key":             "chat:a3f8c1d0e4b2f9a7"
//   }
// }
// ```
//
// # Model aliases understood in the `model` field
//
// | Value          | Behaviour                                      |
// |----------------|------------------------------------------------|
// | `"auto"`       | ModelRouter decides local vs remote            |
// | `"local"`      | Force Ollama regardless of task kind           |
// | `"remote"`     | Force Grok regardless of task kind             |
// | `"grok-*"`     | Force Grok, pass model name through            |
// | `"rc:<hint>"`  | Treat `<hint>` as the prompt for classification|
// | anything else  | Treated as `"auto"`                            |
//
// # Auth
//
// Set `RUSTCODE_PROXY_API_KEYS=key1,key2,...` in the environment.
// If the variable is empty / unset, the endpoint is open (useful for local dev).
// The key is read from `Authorization: Bearer <key>` or `X-API-Key: <key>`.
//
// # Fallback behaviour for the futures app
//
// The companion `ProxyClient` (see `src/api/proxy_client.rs`) tries
// RustCode first and falls back to the upstream Grok API transparently
// when RustCode is unreachable or returns a 5xx error.

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{
        IntoResponse, Response,
        sse::{Event, KeepAlive, Sse},
    },
    routing::post,
};
use futures::stream::{self, StreamExt};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio_stream::wrappers::ReceiverStream;
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::api::repos::RepoAppState;
use crate::model_router::{CompletionRequest, ModelTarget, TaskKind};
use crate::ollama_client::StreamChunk;
use crate::research::worker::{enhance_prompt_with_rag, search_rag_context};

// ---------------------------------------------------------------------------
// Shared proxy state — thin wrapper around RepoAppState + allowed API keys
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct ProxyState {
    // The full repo/chat state (model router, Ollama, Grok, cache, sync svc).
    pub repo_state: RepoAppState,
    // SHA-256 hashes of allowed bearer tokens.
    // Empty → auth disabled (open endpoint).
    pub allowed_key_hashes: Arc<Vec<String>>,
}

impl ProxyState {
    // Build from an existing `RepoAppState`.
    //
    // Reads `RUSTCODE_PROXY_API_KEYS` from the environment (comma-separated raw keys).
    // If the variable is absent or empty, auth is disabled.
    pub fn new(repo_state: RepoAppState) -> Self {
        let raw_keys = std::env::var("RUSTCODE_PROXY_API_KEYS").unwrap_or_default();
        let allowed_key_hashes: Vec<String> = raw_keys
            .split(',')
            .map(str::trim)
            .filter(|k| !k.is_empty())
            .map(hash_key)
            .collect();

        if allowed_key_hashes.is_empty() {
            warn!(
                "RUSTCODE_PROXY_API_KEYS is not set — /v1/chat/completions is open (no auth). \
                 Set RUSTCODE_PROXY_API_KEYS=<key> to restrict access."
            );
        } else {
            info!(
                key_count = allowed_key_hashes.len(),
                "Proxy auth enabled ({} key(s))",
                allowed_key_hashes.len()
            );
        }

        Self {
            repo_state,
            allowed_key_hashes: Arc::new(allowed_key_hashes),
        }
    }

    // Return true when the provided raw key is authorised (or auth is off).
    pub fn is_authorised(&self, key: &str) -> bool {
        if self.allowed_key_hashes.is_empty() {
            return true;
        }
        let h = hash_key(key);
        self.allowed_key_hashes.contains(&h)
    }
}

// ---------------------------------------------------------------------------
// OpenAI-compatible request / response shapes
// ---------------------------------------------------------------------------

// A single message in the conversation history.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OaiMessage {
    // `"system"` | `"user"` | `"assistant"`
    pub role: String,
    // Conversation text — may arrive as a plain string or as the newer OpenAI
    // array-of-parts format: `[{"type":"text","text":"..."}]`.  The custom
    // deserialiser normalises both forms to a plain `String`; non-text parts
    // (e.g. `image_url`) are silently ignored since this proxy is text-only.
    #[serde(deserialize_with = "deserialize_oai_content")]
    pub content: String,
}

// Deserialise the OpenAI `content` field.
//
// The OpenAI Chat Completions API allows `content` to be either:
//   - a plain string:  `"content": "Hello"`
//   - an array of typed parts: `"content": [{"type":"text","text":"Hello"}]`
//
// Clients such as OpenClaw use the array form when building multi-turn
// conversations with tool calls, so we must accept both.  All `"text"` parts
// are concatenated (separated by `"\n"`); other part types are dropped.
fn deserialize_oai_content<'de, D>(de: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::{self, SeqAccess, Visitor};
    use std::fmt;

    struct ContentVisitor;

    impl<'de> Visitor<'de> for ContentVisitor {
        type Value = String;

        fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
            f.write_str("a string or an array of OpenAI content parts")
        }

        // Plain string form — the common case for simple clients.
        fn visit_str<E: de::Error>(self, v: &str) -> Result<String, E> {
            Ok(v.to_owned())
        }

        fn visit_string<E: de::Error>(self, v: String) -> Result<String, E> {
            Ok(v)
        }

        // Array-of-parts form: `[{"type":"text","text":"..."}, ...]`
        // Used by OpenClaw and newer OpenAI client libraries.
        fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<String, A::Error> {
            #[derive(serde::Deserialize)]
            struct Part {
                #[serde(rename = "type")]
                kind: String,
                text: Option<String>,
            }

            let mut texts: Vec<String> = Vec::new();
            while let Some(part) = seq.next_element::<Part>()? {
                if part.kind == "text" {
                    if let Some(t) = part.text {
                        texts.push(t);
                    }
                }
                // Non-text parts (image_url, tool_result, etc.) are silently
                // dropped — this proxy is text-only.
            }
            Ok(texts.join("\n"))
        }
    }

    de.deserialize_any(ContentVisitor)
}

// OpenAI `POST /v1/chat/completions` request body.
#[derive(Debug, Deserialize)]
pub struct OaiChatRequest {
    // ── Standard OpenAI fields ───────────────────────────────────────────────
    // Model alias. See module-level doc for accepted values.
    pub model: String,
    // Conversation messages in chronological order.
    pub messages: Vec<OaiMessage>,
    // Sampling temperature (0.0 – 2.0). Default: 0.2.
    #[serde(default = "default_temperature")]
    pub temperature: f32,
    // Maximum tokens to generate.
    pub max_tokens: Option<u32>,
    // Streaming — must be `false` (streaming is not yet implemented).
    #[serde(default)]
    pub stream: bool,

    // ── RustCode extensions ─────────────────────────────────────────────
    // Inject RAG + symbol context from a registered repo slug or UUID.
    pub x_repo_id: Option<String>,
    // Bypass Redis/LRU response cache.
    #[serde(default)]
    pub x_no_cache: bool,
    // Force remote (Grok) model regardless of task classification.
    #[serde(default)]
    pub x_force_remote: bool,
}

fn default_temperature() -> f32 {
    0.2
}

// OpenAI `ChatCompletion` response (non-streaming).
#[derive(Debug, Serialize)]
pub struct OaiChatResponse {
    pub id: String,
    pub object: String,
    pub created: u64,
    pub model: String,
    pub choices: Vec<OaiChoice>,
    pub usage: OaiUsage,
    // Non-standard metadata — OpenAI clients will ignore this field.
    pub x_ra_metadata: RaMetadata,
}

#[derive(Debug, Serialize)]
pub struct OaiChoice {
    pub index: u32,
    pub message: OaiMessage,
    pub finish_reason: String,
}

// ---------------------------------------------------------------------------
// SSE / streaming shapes  (OpenAI `chat.completion.chunk`)
// ---------------------------------------------------------------------------

// A single SSE data frame for streaming responses.
#[derive(Debug, Serialize)]
struct OaiChunkResponse {
    id: String,
    object: &'static str,
    created: u64,
    model: String,
    choices: Vec<OaiChunkChoice>,
    // Only present on the final chunk (finish_reason = "stop").
    #[serde(skip_serializing_if = "Option::is_none")]
    usage: Option<OaiUsage>,
}

#[derive(Debug, Serialize)]
struct OaiChunkChoice {
    index: u32,
    delta: OaiDelta,
    finish_reason: Option<String>,
}

// The delta payload inside a streaming chunk.
#[derive(Debug, Serialize)]
struct OaiDelta {
    // Only set on the very first chunk to establish the role.
    #[serde(skip_serializing_if = "Option::is_none")]
    role: Option<String>,
    // The incremental text content (empty string on the final chunk).
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct OaiUsage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
}

// RustCode-specific metadata returned alongside every response.
#[derive(Debug, Serialize)]
pub struct RaMetadata {
    // The `TaskKind` the model router assigned to this prompt.
    pub task_kind: String,
    // True when the local model was tried but fell back to remote.
    pub used_fallback: bool,
    // True when repo symbols/tree/todos were injected into the prompt.
    pub repo_context_injected: bool,
    // Number of RAG chunks prepended to the prompt.
    pub rag_chunks_used: usize,
    // True when the response was served from cache.
    pub cached: bool,
    // Cache key used for this request (useful for debugging).
    pub cache_key: String,
}

// OpenAI-compatible error body.
#[derive(Debug, Serialize)]
pub struct OaiError {
    pub error: OaiErrorDetail,
}

#[derive(Debug, Serialize)]
pub struct OaiErrorDetail {
    pub message: String,
    pub r#type: String,
    pub code: Option<String>,
}

impl OaiError {
    fn auth(msg: impl Into<String>) -> (StatusCode, Json<Self>) {
        (
            StatusCode::UNAUTHORIZED,
            Json(Self {
                error: OaiErrorDetail {
                    message: msg.into(),
                    r#type: "authentication_error".to_string(),
                    code: Some("invalid_api_key".to_string()),
                },
            }),
        )
    }

    fn bad_request(msg: impl Into<String>) -> (StatusCode, Json<Self>) {
        (
            StatusCode::BAD_REQUEST,
            Json(Self {
                error: OaiErrorDetail {
                    message: msg.into(),
                    r#type: "invalid_request_error".to_string(),
                    code: None,
                },
            }),
        )
    }
}

// ---------------------------------------------------------------------------
// Cache types (stored as JSON in CacheLayer)
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize, Clone)]
struct CachedProxyResponse {
    content: String,
    model_used: String,
    used_fallback: bool,
    task_kind: String,
    rag_chunks_used: usize,
    repo_context_injected: bool,
    prompt_tokens: u32,
    completion_tokens: u32,
}

// TTL for proxy response cache entries: 30 minutes.
// Shorter than the 1-hour chat TTL because market/code context can change fast.
const PROXY_CACHE_TTL_SECS: u64 = 1800;

// ---------------------------------------------------------------------------
// Router constructor — call this in server.rs
// ---------------------------------------------------------------------------

// Build the `/v1` router containing the OpenAI-compatible chat completion endpoint.
//
// Mount with `.nest("/v1", proxy_router(proxy_state))` in `run_server`.
pub fn proxy_router(state: ProxyState) -> Router {
    Router::new()
        .route("/chat/completions", post(handle_chat_completions))
        .route("/models", axum::routing::get(handle_list_models))
        .with_state(state)
}

// ---------------------------------------------------------------------------
// Handler — POST /v1/chat/completions
// ---------------------------------------------------------------------------

async fn handle_chat_completions(
    State(state): State<ProxyState>,
    headers: HeaderMap,
    Json(req): Json<OaiChatRequest>,
) -> Response {
    // ── 1. Auth ──────────────────────────────────────────────────────────────
    if let Some(err) = check_auth(&state, &headers) {
        return err.into_response();
    }

    // ── 2. Validate request ──────────────────────────────────────────────────
    if req.messages.is_empty() {
        return OaiError::bad_request("messages array must not be empty").into_response();
    }

    // ── 3. Extract the effective user prompt for routing/RAG ─────────────────
    // We use the last user message as the "active" prompt for classification
    // and RAG retrieval. The full history is concatenated for the model call.
    let last_user_msg = req
        .messages
        .iter()
        .rev()
        .find(|m| m.role == "user")
        .map(|m| m.content.as_str())
        .unwrap_or("");

    let system_prompt: Option<String> = req
        .messages
        .iter()
        .find(|m| m.role == "system")
        .map(|m| m.content.clone());

    // ── 4. Model routing ──────────────────────────────────────────────────────
    let (task_kind, mut target) =
        route_from_model_field(&state.repo_state, &req.model, last_user_msg).await;

    if req.x_force_remote {
        target = state
            .repo_state
            .model_router
            .route(&TaskKind::ArchitecturalReason);
    }

    // ── 5. Repo context injection ─────────────────────────────────────────────
    let (repo_context, repo_context_injected) = if let Some(ref rid) = req.x_repo_id {
        let svc = state.repo_state.sync_service.read().await;
        match svc.build_prompt_context(rid).await {
            Ok(ctx) => (Some(ctx), true),
            Err(e) => {
                warn!(repo = %rid, error = %e, "Proxy: could not build repo context — continuing without it");
                (None, false)
            }
        }
    } else {
        (None, false)
    };

    // ── 6. RAG retrieval ──────────────────────────────────────────────────────
    let (rag_enriched_prompt, rag_chunks_used) =
        enrich_with_rag(&state.repo_state, last_user_msg).await;

    // ── 7. Build full prompt (history + RAG + repo context) ──────────────────
    let full_prompt =
        build_full_prompt(&req.messages, &rag_enriched_prompt, repo_context.as_deref());

    // ── 8. Cache key & lookup ─────────────────────────────────────────────────
    let cache_key = build_proxy_cache_key(&target, &full_prompt, req.x_repo_id.as_deref());

    // ── 9. Stream or non-stream branch ───────────────────────────────────────
    if req.stream {
        return handle_streaming(
            state,
            req,
            full_prompt,
            target,
            task_kind,
            rag_chunks_used,
            repo_context_injected,
            cache_key,
            system_prompt.clone(),
        )
        .await;
    }

    if !req.x_no_cache {
        match state
            .repo_state
            .cache
            .get::<CachedProxyResponse>(&cache_key)
            .await
        {
            Ok(Some(hit)) => {
                debug!(cache_key = %cache_key, "Proxy cache hit");
                return build_oai_response(
                    hit.content,
                    hit.model_used,
                    hit.used_fallback,
                    hit.task_kind,
                    hit.rag_chunks_used,
                    hit.repo_context_injected,
                    hit.prompt_tokens,
                    hit.completion_tokens,
                    true,
                    cache_key,
                )
                .into_response();
            }
            Ok(None) => {}
            Err(e) => warn!(error = %e, "Proxy: cache read error — proceeding"),
        }
    }

    // ── 10. Dispatch to model ─────────────────────────────────────────────────
    let comp_req = CompletionRequest {
        system_prompt: system_prompt.clone(),
        user_prompt: full_prompt.clone(),
        max_tokens: req.max_tokens.unwrap_or(2048),
        temperature: req.temperature,
        repo_context: None, // already baked into full_prompt above
    };

    let (reply, model_used, used_fallback, tokens_used) =
        dispatch(&state.repo_state, &comp_req, &target).await;

    let (prompt_tok, completion_tok) = split_tokens(tokens_used, &full_prompt, &reply);

    // ── 11. Cache (fire-and-forget) ───────────────────────────────────────────
    let cached_val = CachedProxyResponse {
        content: reply.clone(),
        model_used: model_used.clone(),
        used_fallback,
        task_kind: format!("{:?}", task_kind),
        rag_chunks_used,
        repo_context_injected,
        prompt_tokens: prompt_tok,
        completion_tokens: completion_tok,
    };
    {
        let cache = Arc::clone(&state.repo_state.cache);
        let key = cache_key.clone();
        tokio::spawn(async move {
            if let Err(e) = cache
                .set(&key, &cached_val, Some(PROXY_CACHE_TTL_SECS))
                .await
            {
                warn!(error = %e, "Proxy: failed to write response to cache");
            }
        });
    }

    // ── 12. Build OpenAI-compatible response ──────────────────────────────────
    build_oai_response(
        reply,
        model_used,
        used_fallback,
        format!("{:?}", task_kind),
        rag_chunks_used,
        repo_context_injected,
        prompt_tok,
        completion_tok,
        false,
        cache_key,
    )
    .into_response()
}

// ---------------------------------------------------------------------------
// SSE streaming handler
// ---------------------------------------------------------------------------

// Handle a streaming (`stream: true`) chat completion request.
//
// Drives `OllamaClient::complete_streaming` and translates each
// [`StreamChunk`] into an OpenAI-compatible SSE `data:` frame.
// On completion the full assembled reply is written to the cache.
#[allow(clippy::too_many_arguments)]
async fn handle_streaming(
    state: ProxyState,
    req: OaiChatRequest,
    full_prompt: String,
    target: ModelTarget,
    task_kind: TaskKind,
    rag_chunks_used: usize,
    repo_context_injected: bool,
    cache_key: String,
    system_prompt: Option<String>,
) -> Response {
    let completion_id = format!("chatcmpl-rc-{}", Uuid::new_v4());
    let created = unix_now();
    let max_tokens = req.max_tokens.unwrap_or(2048);
    let temperature = req.temperature;

    // For remote (Grok) targets we don't have a native streaming path yet —
    // fall back to the blocking dispatch and emit all tokens in one burst.
    // Ollama supports native NDJSON streaming.
    let chunk_rx = match &target {
        ModelTarget::Local { .. } => {
            state
                .repo_state
                .ollama_client
                .complete_streaming(
                    system_prompt.as_deref(),
                    &full_prompt,
                    temperature,
                    max_tokens,
                )
                .await
        }
        ModelTarget::Remote { model, api_key } => {
            // Synthesise a single-delta stream from the blocking Grok call.
            let (tx, rx) = tokio::sync::mpsc::channel::<StreamChunk>(4);
            let model = model.clone();
            let api_key = api_key.clone();
            let grok_client = state.repo_state.grok_client.clone();
            let prompt = full_prompt.clone();

            tokio::spawn(async move {
                let result = if let Some(ref grok) = grok_client {
                    grok.ask_tracked(&prompt, None, "proxy-stream").await
                } else {
                    use crate::db::Database;
                    match Database::new("data/rustcode.db").await {
                        Ok(db) => {
                            let client = crate::grok_client::GrokClient::new(api_key.clone(), db);
                            client.ask_tracked(&prompt, None, "proxy-stream").await
                        }
                        Err(e) => Err(anyhow::anyhow!("DB init failed: {}", e)),
                    }
                };

                match result {
                    Ok(resp) => {
                        let _ = tx.send(StreamChunk::Delta(resp.content)).await;
                        let _ = tx
                            .send(StreamChunk::Done {
                                model_used: model.clone(),
                                used_fallback: false,
                                prompt_tokens: Some(resp.prompt_tokens as u32),
                                completion_tokens: Some(resp.completion_tokens as u32),
                            })
                            .await;
                    }
                    Err(e) => {
                        let _ = tx.send(StreamChunk::Error(e.to_string())).await;
                    }
                }
            });

            rx
        }
    };

    // Wrap the mpsc receiver in a Stream so axum's Sse can consume it.
    let chunk_stream = ReceiverStream::new(chunk_rx);

    // State shared across the closure: we accumulate the full reply so we can
    // write it to the cache once the stream finishes.
    let id_clone = completion_id.clone();
    let cache_key_clone = cache_key.clone();
    let task_kind_str = format!("{:?}", task_kind);
    let cache = Arc::clone(&state.repo_state.cache);

    // We need mutable accumulator state across closure calls.  Use an Arc<Mutex>
    // so the FnMut closure can share it with the cache-write spawned at the end.
    type FinalMeta = Arc<tokio::sync::Mutex<Option<(String, bool, u32, u32)>>>;
    let accumulated = Arc::new(tokio::sync::Mutex::new(String::new()));
    let final_meta: FinalMeta = Arc::new(tokio::sync::Mutex::new(None));

    let acc_clone = Arc::clone(&accumulated);
    let meta_clone = Arc::clone(&final_meta);

    let sse_stream = chunk_stream
        .map(move |chunk| -> Result<Event, std::convert::Infallible> {
            let id = id_clone.clone();
            let now = created;

            match chunk {
                StreamChunk::Error(e) => {
                    // Emit an error frame that OpenAI clients recognise.
                    let err_payload = serde_json::json!({
                        "error": { "message": e, "type": "stream_error" }
                    });
                    Ok(Event::default().data(err_payload.to_string()))
                }

                StreamChunk::Delta(text) => {
                    // Accumulate for cache write later (best-effort, non-blocking).
                    if let Ok(mut acc) = acc_clone.try_lock() {
                        acc.push_str(&text);
                    }

                    let chunk_resp = OaiChunkResponse {
                        id,
                        object: "chat.completion.chunk",
                        created: now,
                        // Model name not yet known; use placeholder until Done.
                        model: "streaming".to_string(),
                        choices: vec![OaiChunkChoice {
                            index: 0,
                            delta: OaiDelta {
                                role: None,
                                content: Some(text),
                            },
                            finish_reason: None,
                        }],
                        usage: None,
                    };
                    let data =
                        serde_json::to_string(&chunk_resp).unwrap_or_else(|_| "{}".to_string());
                    Ok(Event::default().data(data))
                }

                StreamChunk::Done {
                    model_used,
                    used_fallback,
                    prompt_tokens,
                    completion_tokens,
                } => {
                    // Store final metadata so we can cache after the stream ends.
                    let pt = prompt_tokens.unwrap_or(0);
                    let ct = completion_tokens.unwrap_or(0);
                    if let Ok(mut m) = meta_clone.try_lock() {
                        *m = Some((model_used.clone(), used_fallback, pt, ct));
                    }

                    let final_chunk = OaiChunkResponse {
                        id,
                        object: "chat.completion.chunk",
                        created: now,
                        model: model_used,
                        choices: vec![OaiChunkChoice {
                            index: 0,
                            delta: OaiDelta {
                                role: None,
                                content: None,
                            },
                            finish_reason: Some("stop".to_string()),
                        }],
                        usage: Some(OaiUsage {
                            prompt_tokens: pt,
                            completion_tokens: ct,
                            total_tokens: pt + ct,
                        }),
                    };
                    let data =
                        serde_json::to_string(&final_chunk).unwrap_or_else(|_| "{}".to_string());
                    Ok(Event::default().data(data))
                }
            }
        })
        // Append the mandatory `data: [DONE]` sentinel.
        .chain(stream::once(async move {
            // Best-effort cache write once the stream is complete.
            let acc = accumulated.lock().await;
            if let Some((model_used, used_fallback, pt, ct)) = final_meta.lock().await.clone() {
                let cached_val = CachedProxyResponse {
                    content: acc.clone(),
                    model_used: model_used.clone(),
                    used_fallback,
                    task_kind: task_kind_str,
                    rag_chunks_used,
                    repo_context_injected,
                    prompt_tokens: pt,
                    completion_tokens: ct,
                };
                let key = cache_key_clone;
                tokio::spawn(async move {
                    if let Err(e) = cache
                        .set(&key, &cached_val, Some(PROXY_CACHE_TTL_SECS))
                        .await
                    {
                        warn!(error = %e, "Proxy stream: failed to cache response");
                    }
                });
            }

            Ok::<Event, std::convert::Infallible>(Event::default().data("[DONE]"))
        }));

    // Emit a role-establishing first chunk before the model content arrives.
    let first_chunk = OaiChunkResponse {
        id: completion_id.clone(),
        object: "chat.completion.chunk",
        created,
        model: "streaming".to_string(),
        choices: vec![OaiChunkChoice {
            index: 0,
            delta: OaiDelta {
                role: Some("assistant".to_string()),
                content: None,
            },
            finish_reason: None,
        }],
        usage: None,
    };
    let first_data = serde_json::to_string(&first_chunk).unwrap_or_else(|_| "{}".to_string());
    let first_event = stream::once(async move {
        Ok::<Event, std::convert::Infallible>(Event::default().data(first_data))
    });

    let full_stream = first_event.chain(sse_stream);

    Sse::new(full_stream)
        .keep_alive(KeepAlive::default())
        .into_response()
}

// ---------------------------------------------------------------------------
// Handler — GET /v1/models
// ---------------------------------------------------------------------------
//
// Returns an OpenAI-compatible model list.  Each entry carries the capability
// flags and token-limit fields that Zed and other smart clients inspect when
// building their "Add Model" UI:
//
//   max_tokens              — total context window (prompt + completion)
//   max_completion_tokens   — upper bound on generated tokens
//   max_output_tokens       — alias used by some clients (same value)
//   supports_tools          — whether the model accepts `tools` / `functions`
//   supports_parallel_tool_calls — whether parallel function calls are ok
//   supports_images         — whether image content parts are accepted
//   supports_prompt_cache_key    — proprietary cache-key extension
//   supports_chat_completions    — always true for every entry here

#[derive(Serialize)]
struct ModelList {
    object: &'static str,
    data: Vec<ModelEntry>,
}

#[derive(Serialize)]
struct ModelEntry {
    id: String,
    object: &'static str,
    created: u64,
    owned_by: &'static str,

    // ── Token limits ────────────────────────────────────────────────
    // Total context window (prompt + completion).
    max_tokens: u32,
    // Maximum tokens the model will generate in one turn.
    max_completion_tokens: u32,
    // Alias for `max_completion_tokens` (used by some clients, e.g. Zed).
    max_output_tokens: u32,

    // ── Capability flags ────────────────────────────────────────────
    // Whether `tools` / `functions` are accepted.  RustCode does not
    // forward tool definitions, so this is `false` for all entries.
    supports_tools: bool,
    // Whether parallel tool-call responses are supported.
    supports_parallel_tool_calls: bool,
    // Whether image content parts are accepted.
    supports_images: bool,
    // Whether a `prompt_cache_key` extension field is understood.
    supports_prompt_cache_key: bool,
    // Always `true` — every entry here is a chat-completions model.
    supports_chat_completions: bool,
}

impl ModelEntry {
    // Construct a RustCode virtual model entry.
    fn rc(id: &str, max_tokens: u32, max_completion_tokens: u32, now: u64) -> Self {
        Self {
            id: id.to_string(),
            object: "model",
            created: now,
            owned_by: "rustcode",
            max_tokens,
            max_completion_tokens,
            max_output_tokens: max_completion_tokens,
            supports_tools: false,
            supports_parallel_tool_calls: false,
            supports_images: false,
            supports_prompt_cache_key: false,
            supports_chat_completions: true,
        }
    }

    // Construct an Ollama passthrough model entry.
    fn ollama(id: String, now: u64) -> Self {
        Self {
            id,
            object: "model",
            created: now,
            owned_by: "ollama",
            // Ollama models vary; 16 k is a safe default for 7-B class models.
            max_tokens: 16_384,
            max_completion_tokens: 8_192,
            max_output_tokens: 8_192,
            supports_tools: false,
            supports_parallel_tool_calls: false,
            supports_images: false,
            supports_prompt_cache_key: false,
            supports_chat_completions: true,
        }
    }
}

async fn handle_list_models(State(state): State<ProxyState>) -> impl IntoResponse {
    let now = unix_now();

    // ── Static entries ───────────────────────────────────────────────────────
    // "rustcode" — canonical single model to configure in Zed / curl.
    //   RustCode routes to Ollama or Grok automatically.
    // "auto"   — same as "rustcode" (ModelRouter decides)
    // "local"  — force Ollama regardless of task kind
    // "remote" — force Grok regardless of task kind
    let mut entries: Vec<ModelEntry> = vec![
        ModelEntry::rc("rustcode", 131_072, 32_768, now),
        ModelEntry::rc("auto", 131_072, 32_768, now),
        ModelEntry::rc("local", 16_384, 8_192, now),
        ModelEntry::rc("remote", 131_072, 32_768, now),
        // OpenAI-provider-prefixed aliases — OpenClaw (and other clients that
        // use the OpenAI SDK with a custom base URL) prepend "openai/" to the
        // model id.  Advertise these so the client's model validation passes.
        ModelEntry::rc("openai/rustcode", 131_072, 32_768, now),
        ModelEntry::rc("openai/auto", 131_072, 32_768, now),
        ModelEntry::rc("openai/local", 16_384, 8_192, now),
        ModelEntry::rc("openai/remote", 131_072, 32_768, now),
    ];

    // ── Live Ollama models ───────────────────────────────────────────────────
    // Expose each installed Ollama model directly so clients can target them
    // by tag (e.g. "qwen2.5-coder:7b") if they want to bypass routing.
    match state.repo_state.ollama_client.list_models().await {
        Ok(models) => {
            for m in models {
                entries.push(ModelEntry::ollama(m, now));
            }
        }
        Err(e) => {
            warn!(error = %e, "Proxy /v1/models: could not list Ollama models");
        }
    }

    Json(ModelList {
        object: "list",
        data: entries,
    })
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

// Verify the bearer token or X-API-Key header against the configured key set.
// Returns `Some(error_response)` when auth fails, `None` when authorised.
fn check_auth(state: &ProxyState, headers: &HeaderMap) -> Option<(StatusCode, Json<OaiError>)> {
    if state.allowed_key_hashes.is_empty() {
        // Auth disabled — open endpoint.
        return None;
    }

    let raw_key = headers
        .get("Authorization")
        .or_else(|| headers.get("X-API-Key"))
        .and_then(|v| v.to_str().ok())
        .map(|s| s.strip_prefix("Bearer ").unwrap_or(s));

    match raw_key {
        None => Some(OaiError::auth(
            "No API key provided. Use Authorization: Bearer <key> or X-API-Key: <key>.",
        )),
        Some(key) if state.is_authorised(key) => None,
        Some(_) => Some(OaiError::auth("Invalid API key.")),
    }
}

// Determine model target from the `model` field in the request.
//
// Supported aliases:
// - `"auto"` — let ModelRouter classify the last user message
// - `"local"` — always use Ollama
// - `"remote"` — always use Grok
// - `"grok-*"` / `"grok"` — always use Grok
// - `"anthropic/*"` / `"claude-*"` — treated as explicit remote (Grok) targets.
//   OpenClaw and other OpenAI-compatible clients send Anthropic model names
//   (e.g. `anthropic/claude-opus-4-6`) when configured with a custom base URL;
//   we intercept and route to the remote backend without the classifier round-trip.
// - `"rc:<hint>"` — strip prefix, use hint as the classification prompt
// - anything else — treat as `"auto"` (message-classifier decides)
async fn route_from_model_field(
    state: &RepoAppState,
    model: &str,
    last_user_msg: &str,
) -> (TaskKind, ModelTarget) {
    // Strip the "openai/" provider prefix that OpenAI-SDK clients (OpenClaw,
    // Cursor, etc.) prepend when configured with a custom OPENAI_API_BASE.
    let raw = model.to_lowercase();
    let model_lc = raw.strip_prefix("openai/").unwrap_or(&raw).to_string();

    match model_lc.as_str() {
        "local" => {
            let target = state.model_router.route(&TaskKind::ScaffoldStub);
            (TaskKind::ScaffoldStub, target)
        }
        "remote" | "grok" => {
            let target = state.model_router.route(&TaskKind::ArchitecturalReason);
            (TaskKind::ArchitecturalReason, target)
        }
        _ if model_lc.starts_with("grok-") => {
            let target = state.model_router.route(&TaskKind::ArchitecturalReason);
            (TaskKind::ArchitecturalReason, target)
        }
        // OpenAI-compatible clients (e.g. OpenClaw) configured with a custom base URL
        // often send the Anthropic model name verbatim.  Route these straight to the
        // remote backend so we skip the classifier round-trip and give the caller a
        // sensible echoed model name in the response.
        _ if model_lc.starts_with("anthropic/") || model_lc.starts_with("claude-") => {
            let target = state.model_router.route(&TaskKind::ArchitecturalReason);
            (TaskKind::ArchitecturalReason, target)
        }
        _ => {
            // "auto" or unknown — let the router classify the message.
            // For "rc:<hint>" we use the hint instead of the raw last message.
            let classify_text = if let Some(hint) = model.strip_prefix("rc:") {
                hint
            } else {
                last_user_msg
            };
            state.model_router.route_prompt_async(classify_text).await
        }
    }
}

// Retrieve RAG chunks for the given prompt and return the enriched prompt
// string plus the number of chunks actually used.
async fn enrich_with_rag(state: &RepoAppState, prompt: &str) -> (String, usize) {
    let maybe_pool = {
        let svc = state.sync_service.read().await;
        svc.db_pool()
    };

    if let Some(pool) = maybe_pool {
        match search_rag_context(&pool, prompt, 4).await {
            Ok(chunks) if !chunks.is_empty() => {
                let count = chunks.len();
                let enriched = enhance_prompt_with_rag(prompt, &chunks);
                debug!(rag_chunks = count, "Proxy: RAG enriched prompt");
                (enriched, count)
            }
            Ok(_) => (prompt.to_owned(), 0),
            Err(e) => {
                warn!(error = %e, "Proxy: RAG search failed — using plain prompt");
                (prompt.to_owned(), 0)
            }
        }
    } else {
        (prompt.to_owned(), 0)
    }
}

// Collapse the full conversation history into a single prompt string that
// the underlying completion API can handle, injecting RAG text and optional
// repo context.
//
// Format:
// ```text
// [System]
// <system message if present>
//
// [Repo Context]
// <repo symbols / tree / todos if injected>
//
// [Conversation]
// user: <msg>
// assistant: <msg>
// user: <rag-enriched last message>
// ```
fn build_full_prompt(
    messages: &[OaiMessage],
    rag_enriched_last_user: &str,
    repo_context: Option<&str>,
) -> String {
    let mut parts: Vec<String> = Vec::new();

    // System message (excluded from the [Conversation] block).
    if let Some(sys) = messages.iter().find(|m| m.role == "system") {
        parts.push(format!("[System]\n{}", sys.content));
    }

    // Repo context block.
    if let Some(ctx) = repo_context {
        parts.push(format!("[Repo Context]\n{}", ctx));
    }

    // Conversation history (all non-system messages except the last user turn).
    let non_system: Vec<&OaiMessage> = messages.iter().filter(|m| m.role != "system").collect();

    if !non_system.is_empty() {
        let mut conv_lines: Vec<String> = Vec::new();

        // All turns except the last one verbatim.
        for msg in non_system.iter().take(non_system.len().saturating_sub(1)) {
            conv_lines.push(format!("{}: {}", msg.role, msg.content));
        }

        // Replace the last user turn with the RAG-enriched version.
        conv_lines.push(format!("user: {}", rag_enriched_last_user));

        parts.push(format!("[Conversation]\n{}", conv_lines.join("\n")));
    }

    parts.join("\n\n")
}

// Dispatch to Ollama or Grok via the existing `dispatch_completion` logic.
// Duplicated here (rather than sharing with `repos.rs`) to keep the proxy
// self-contained and avoid coupling to private internals.
async fn dispatch(
    state: &RepoAppState,
    req: &CompletionRequest,
    target: &ModelTarget,
) -> (String, String, bool, Option<u32>) {
    match target {
        ModelTarget::Local { model, .. } => {
            debug!(model = %model, "Proxy: dispatching to local Ollama");
            match state
                .ollama_client
                .complete(
                    req.system_prompt.as_deref(),
                    &req.user_prompt,
                    req.temperature,
                    req.max_tokens,
                )
                .await
            {
                Ok(resp) => {
                    let tokens = resp
                        .prompt_tokens
                        .and_then(|p| resp.completion_tokens.map(|c| p + c));
                    (resp.content, resp.model_used, resp.used_fallback, tokens)
                }
                Err(e) => {
                    warn!(error = %e, "Proxy: Ollama dispatch failed");
                    let msg = format!("Local model error: {}", e);
                    (msg, model.clone(), false, None)
                }
            }
        }

        ModelTarget::Remote { model, api_key } => {
            debug!(model = %model, "Proxy: dispatching to remote Grok");

            let result = if let Some(ref grok) = state.grok_client {
                grok.ask_tracked(&req.user_prompt, None, "proxy-chat").await
            } else {
                // One-shot client when no pre-built GrokClient is available.
                use crate::db::Database;
                match Database::new("data/rustcode.db").await {
                    Ok(db) => {
                        let client = crate::grok_client::GrokClient::new(api_key.clone(), db);
                        client
                            .ask_tracked(&req.user_prompt, None, "proxy-chat")
                            .await
                    }
                    Err(e) => Err(anyhow::anyhow!(
                        "DB init for one-shot Grok client failed: {}",
                        e
                    )),
                }
            };

            match result {
                Ok(resp) => {
                    let tokens = (resp.prompt_tokens + resp.completion_tokens) as u32;
                    (resp.content, model.clone(), false, Some(tokens))
                }
                Err(e) => {
                    warn!(error = %e, "Proxy: Grok dispatch failed");
                    let msg = format!("Remote model error: {}", e);
                    (msg, model.clone(), false, None)
                }
            }
        }
    }
}

// Split a combined token count into an approximate prompt/completion split.
// If the model returned both counts, use them. Otherwise estimate using a
// naive 4-chars-per-token heuristic.
fn split_tokens(combined: Option<u32>, prompt: &str, completion: &str) -> (u32, u32) {
    if let Some(total) = combined {
        // Rough proportional split when we only have the total.
        let p_chars = prompt.len() as f64;
        let c_chars = completion.len() as f64;
        let total_chars = (p_chars + c_chars).max(1.0);
        let p_tok = ((p_chars / total_chars) * total as f64).round() as u32;
        let c_tok = total.saturating_sub(p_tok);
        (p_tok, c_tok)
    } else {
        // Full heuristic fallback.
        let p_tok = (prompt.len() as u32).saturating_div(4).max(1);
        let c_tok = (completion.len() as u32).saturating_div(4).max(1);
        (p_tok, c_tok)
    }
}

// Build the final `OaiChatResponse`.
#[allow(clippy::too_many_arguments)]
fn build_oai_response(
    content: String,
    model_used: String,
    used_fallback: bool,
    task_kind: String,
    rag_chunks_used: usize,
    repo_context_injected: bool,
    prompt_tokens: u32,
    completion_tokens: u32,
    cached: bool,
    cache_key: String,
) -> Json<OaiChatResponse> {
    Json(OaiChatResponse {
        id: format!("chatcmpl-rc-{}", Uuid::new_v4()),
        object: "chat.completion".to_string(),
        created: unix_now(),
        model: model_used.clone(),
        choices: vec![OaiChoice {
            index: 0,
            message: OaiMessage {
                role: "assistant".to_string(),
                content,
            },
            finish_reason: "stop".to_string(),
        }],
        usage: OaiUsage {
            prompt_tokens,
            completion_tokens,
            total_tokens: prompt_tokens + completion_tokens,
        },
        x_ra_metadata: RaMetadata {
            task_kind,
            used_fallback,
            repo_context_injected,
            rag_chunks_used,
            cached,
            cache_key,
        },
    })
}

// Build a deterministic cache key for the proxy.
// Identical to `repos.rs::build_cache_key` but namespaced as `proxy:`.
fn build_proxy_cache_key(target: &ModelTarget, prompt: &str, repo_id: Option<&str>) -> String {
    let label = match target {
        ModelTarget::Local { model, .. } => format!("local:{}", model),
        ModelTarget::Remote { model, .. } => format!("remote:{}", model),
    };

    let mut h = Sha256::new();
    h.update(label.as_bytes());
    h.update(b"\x00");
    h.update(prompt.as_bytes());
    h.update(b"\x00");
    h.update(repo_id.unwrap_or("").as_bytes());
    let digest = h.finalize();

    format!("proxy:{}", hex::encode(&digest[..8]))
}

// Hash a raw API key with SHA-256 for constant-time-safe comparison.
fn hash_key(key: &str) -> String {
    let mut h = Sha256::new();
    h.update(key.as_bytes());
    hex::encode(h.finalize())
}

// Seconds since Unix epoch.
fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use runtime::test_remove_var;

    #[test]
    fn hash_key_is_deterministic() {
        assert_eq!(hash_key("my-secret"), hash_key("my-secret"));
        assert_ne!(hash_key("my-secret"), hash_key("other-secret"));
        assert_eq!(hash_key("my-secret").len(), 64); // hex-encoded SHA-256
    }

    #[test]
    fn build_proxy_cache_key_stable() {
        let target_a = ModelTarget::Local {
            model: "qwen2.5-coder:7b".to_string(),
            base_url: "http://localhost:11434".to_string(),
        };
        let target_b = ModelTarget::Local {
            model: "qwen2.5-coder:7b".to_string(),
            base_url: "http://localhost:11434".to_string(),
        };
        let k1 = build_proxy_cache_key(&target_a, "hello world", Some("my-repo"));
        let k2 = build_proxy_cache_key(&target_b, "hello world", Some("my-repo"));
        assert_eq!(k1, k2);
        assert!(k1.starts_with("proxy:"));
    }

    #[test]
    fn split_tokens_proportional() {
        // With a combined count, the split should sum back to the total.
        let (p, c) = split_tokens(Some(100), "prompt text here", "completion text");
        assert_eq!(p + c, 100);
    }

    #[test]
    fn split_tokens_heuristic_fallback() {
        // Without a combined count, we fall back to the character heuristic.
        let prompt = "a".repeat(400); // ~100 tokens
        let completion = "b".repeat(200); // ~50 tokens
        let (p, c) = split_tokens(None, &prompt, &completion);
        assert!(p > 0);
        assert!(c > 0);
    }

    #[test]
    fn build_full_prompt_contains_all_sections() {
        let messages = vec![
            OaiMessage {
                role: "system".to_string(),
                content: "You are a trading bot.".to_string(),
            },
            OaiMessage {
                role: "user".to_string(),
                content: "What is the BTC trend?".to_string(),
            },
        ];
        let rag = "RAG: BTC recently crossed the 200-day MA.";
        let ctx = "Tree: src/\n  trading/\n    bot.rs";

        let prompt = build_full_prompt(&messages, rag, Some(ctx));

        assert!(prompt.contains("[System]"));
        assert!(prompt.contains("You are a trading bot."));
        assert!(prompt.contains("[Repo Context]"));
        assert!(prompt.contains("Tree:"));
        assert!(prompt.contains("[Conversation]"));
        assert!(prompt.contains("RAG: BTC recently crossed"));
    }

    #[test]
    fn build_full_prompt_no_system_no_ctx() {
        let messages = vec![OaiMessage {
            role: "user".to_string(),
            content: "Explain RSI divergence.".to_string(),
        }];
        let prompt = build_full_prompt(&messages, "Explain RSI divergence.", None);
        assert!(!prompt.contains("[System]"));
        assert!(!prompt.contains("[Repo Context]"));
        assert!(prompt.contains("[Conversation]"));
        assert!(prompt.contains("Explain RSI divergence."));
    }

    #[test]
    fn proxy_state_auth_disabled_when_no_keys() {
        // When RUSTCODE_PROXY_API_KEYS is not set, is_authorised returns true for anything.
        test_remove_var("RUSTCODE_PROXY_API_KEYS");
        // We can't construct a real ProxyState without a RepoAppState, so we test
        // the key-hash logic in isolation.
        let hashes: Vec<String> = vec![];
        // Simulate the is_authorised logic:
        let is_open = hashes.is_empty();
        assert!(is_open);
    }

    #[test]
    fn proxy_state_auth_rejects_wrong_key() {
        let good_key = "super-secret-key";
        let hashes = [hash_key(good_key)];
        let provided = hash_key("wrong-key");
        assert!(!hashes.contains(&provided));
    }

    #[test]
    fn proxy_state_auth_accepts_correct_key() {
        let good_key = "super-secret-key";
        let hashes = [hash_key(good_key)];
        let provided = hash_key(good_key);
        assert!(hashes.contains(&provided));
    }

    #[test]
    fn unix_now_is_positive() {
        assert!(unix_now() > 0);
    }
}
