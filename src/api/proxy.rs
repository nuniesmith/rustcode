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
use crate::llm::ollama::StreamChunk;
use crate::llm::router::{ClaudeTier, CompletionRequest, ModelTarget, TaskKind};
use crate::research::worker::{enhance_prompt_with_rag, search_rag_context};

use ::api::{
    AnthropicClient, ContentBlockDelta, InputContentBlock, InputMessage, MessageRequest,
    MessageResponse, OutputContentBlock as AnthropicContentBlock, PromptCache, StreamEvent,
    SystemBlock, ToolChoice as AnthropicToolChoice, ToolDefinition, ToolResultContentBlock, Usage,
};

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
    // `"system"` | `"user"` | `"assistant"` | `"tool"`
    pub role: String,
    // Conversation text — may arrive as a plain string, as the newer OpenAI
    // array-of-parts format: `[{"type":"text","text":"..."}]`, or as `null`
    // (assistant turns that carried only `tool_calls`).  The custom
    // deserialiser normalises all forms to a plain `String`; non-text parts
    // (e.g. `image_url`) are silently ignored since this proxy is text-only.
    #[serde(default, deserialize_with = "deserialize_oai_content")]
    pub content: String,
    // Tool calls made by a prior assistant turn (OpenAI function-calling
    // history format). Forwarded to Claude as `tool_use` content blocks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<OaiToolCall>>,
    // For `role: "tool"` messages — the id of the tool call this message
    // is a result for. Forwarded to Claude as a `tool_result` block.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

// An OpenAI tool definition: `{ "type": "function", "function": {...} }`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OaiTool {
    #[serde(rename = "type")]
    pub kind: String,
    pub function: OaiFunctionDef,
}

// The `function` payload inside an OpenAI tool definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OaiFunctionDef {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    // JSON Schema for the arguments. OpenAI allows this to be omitted for
    // zero-argument functions.
    #[serde(default)]
    pub parameters: Option<serde_json::Value>,
}

// A tool call emitted by the assistant (OpenAI wire format). `arguments`
// is a JSON-encoded string per the OpenAI spec.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OaiToolCall {
    pub id: String,
    #[serde(rename = "type", default = "default_function_kind")]
    pub kind: String,
    pub function: OaiFunctionCall,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OaiFunctionCall {
    pub name: String,
    pub arguments: String,
}

fn default_function_kind() -> String {
    "function".to_string()
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

        // `content: null` — sent on assistant turns that only carried
        // `tool_calls`. Normalised to an empty string.
        fn visit_unit<E: de::Error>(self) -> Result<String, E> {
            Ok(String::new())
        }

        fn visit_none<E: de::Error>(self) -> Result<String, E> {
            Ok(String::new())
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
    // Streaming — when `true` the response is SSE `chat.completion.chunk`
    // frames; when `false` (default) a single JSON body.
    #[serde(default)]
    pub stream: bool,
    // OpenAI function-calling tool definitions. Forwarded to Claude targets
    // as Anthropic `tools`; ignored (with a warning) for Ollama / Grok
    // targets, which have no function-calling path through this proxy.
    #[serde(default)]
    pub tools: Option<Vec<OaiTool>>,
    // OpenAI `tool_choice`: `"auto"` | `"none"` | `"required"` |
    // `{"type":"function","function":{"name":...}}`. `"none"` drops the
    // tool definitions entirely.
    #[serde(default)]
    pub tool_choice: Option<serde_json::Value>,

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
    pub message: OaiAssistantMessage,
    pub finish_reason: String,
}

// The assistant message inside a non-streaming choice. `content` is `null`
// (not the empty string) when the turn carried only tool calls, matching
// the OpenAI wire format that strict clients validate against.
#[derive(Debug, Serialize)]
pub struct OaiAssistantMessage {
    pub role: String,
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<OaiToolCall>>,
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
    // Anthropic prompt-cache write tokens. Only emitted on the final chunk of
    // a Claude-served stream — every other path leaves it `None` so the field
    // is skipped from the serialized JSON.
    #[serde(skip_serializing_if = "Option::is_none")]
    cache_creation_input_tokens: Option<u32>,
    // Anthropic prompt-cache read tokens. Only emitted on the final chunk of
    // a Claude-served stream — every other path leaves it `None`.
    #[serde(skip_serializing_if = "Option::is_none")]
    cache_read_input_tokens: Option<u32>,
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
    // Incremental tool-call fragments (OpenAI streaming function-call
    // format). The first fragment for a call carries `id` / `type` /
    // `function.name`; subsequent fragments append to `function.arguments`.
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_calls: Option<Vec<OaiDeltaToolCall>>,
}

#[derive(Debug, Serialize)]
struct OaiDeltaToolCall {
    index: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<String>,
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    kind: Option<&'static str>,
    function: OaiDeltaFunction,
}

#[derive(Debug, Serialize)]
struct OaiDeltaFunction {
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    arguments: String,
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
    // Anthropic prompt-cache tokens created on this request (only present for
    // Claude responses; omitted otherwise).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_creation_input_tokens: Option<u32>,
    // Anthropic prompt-cache tokens read on this request (only present for
    // Claude responses; omitted otherwise).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_read_input_tokens: Option<u32>,
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
        Self::auth_response(msg)
    }

    /// Build a `(401 Unauthorized, JSON error body)` response. Exposed at
    /// crate-level so other endpoints (e.g. `/v1/agent/run`) can return
    /// the same shape without re-implementing it.
    pub(crate) fn auth_response(msg: impl Into<String>) -> (StatusCode, Json<Self>) {
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

    /// Build a `(400 Bad Request, JSON error body)` response.
    pub(crate) fn bad_request(msg: impl Into<String>) -> (StatusCode, Json<Self>) {
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cache_creation_input_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cache_read_input_tokens: Option<u32>,
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
        .route("/agent/run", post(crate::api::agent::handle_agent_run))
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

    // Structured message history + translated tool definitions for Claude
    // targets. Ollama / Grok use the flattened `full_prompt` instead — warn
    // when a client sent tools that a non-Claude target will ignore.
    let claude_payload = build_claude_payload(&req, &rag_enriched_prompt, repo_context.as_deref());
    let has_tools = claude_payload.tools.is_some();
    if has_tools && !matches!(target, ModelTarget::Claude { .. }) {
        warn!(
            model = %req.model,
            "Proxy: request carries tool definitions but the resolved target \
             is not Claude — tools will be ignored"
        );
    }

    // ── 8. Cache key & lookup ─────────────────────────────────────────
    let cache_key = build_proxy_cache_key(&target, &full_prompt, req.x_repo_id.as_deref());

    // ── 9. Stream or non-stream branch ─────────────────────────────────
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
            claude_payload,
        )
        .await;
    }

    // The response cache stores plain text — replaying a cached text answer
    // to an agent that expects tool calls would break its loop, so tool
    // requests bypass the cache entirely.
    if !req.x_no_cache && !has_tools {
        match state
            .repo_state
            .cache
            .get::<CachedProxyResponse>(&cache_key)
            .await
        {
            Ok(Some(hit)) => {
                debug!(cache_key = %cache_key, "Proxy cache hit");
                log_dispatch_event(&DispatchLogContext {
                    task_kind: &hit.task_kind,
                    target_kind: target_kind_label(&target),
                    model_used: &hit.model_used,
                    prompt_tokens: hit.prompt_tokens,
                    completion_tokens: hit.completion_tokens,
                    cache_creation_input_tokens: hit.cache_creation_input_tokens.unwrap_or(0),
                    cache_read_input_tokens: hit.cache_read_input_tokens.unwrap_or(0),
                    rag_chunks_used: hit.rag_chunks_used,
                    repo_context_injected: hit.repo_context_injected,
                    repo_id: req.x_repo_id.as_deref(),
                    cached: true,
                    streaming: false,
                    used_fallback: hit.used_fallback,
                });
                return build_oai_response(
                    hit.content,
                    None,
                    "stop".to_string(),
                    hit.model_used,
                    hit.used_fallback,
                    hit.task_kind,
                    hit.rag_chunks_used,
                    hit.repo_context_injected,
                    hit.prompt_tokens,
                    hit.completion_tokens,
                    true,
                    cache_key,
                    hit.cache_creation_input_tokens,
                    hit.cache_read_input_tokens,
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

    let outcome = dispatch(&state.repo_state, &comp_req, &target, &claude_payload).await;
    let DispatchOutcome {
        reply,
        model_used,
        used_fallback,
        tokens_used,
        cache_creation_input_tokens,
        cache_read_input_tokens,
        tool_calls,
        finish_reason,
        error: dispatch_error,
    } = outcome;

    let (prompt_tok, completion_tok) = split_tokens(tokens_used, &full_prompt, &reply);
    let task_kind_str = format!("{:?}", task_kind);
    let target_kind = target_kind_label(&target);

    // ── 11. Backend-error fast path ───────────────────────────────────────────
    // When the dispatch helper returned an error, the `reply` body carries
    // the error text so the client still sees it; we skip the cache write
    // (poisoning the cache with error responses would replay failures on
    // every duplicate request for the TTL) and emit a `proxy.dispatch_error`
    // event instead of the success event.
    if let Some(error_message) = dispatch_error.as_deref() {
        log_dispatch_error_event(&DispatchErrorLogContext {
            task_kind: &task_kind_str,
            target_kind,
            model_used: &model_used,
            error_message,
            repo_id: req.x_repo_id.as_deref(),
            streaming: false,
        });
        return build_oai_response(
            reply,
            None,
            "stop".to_string(),
            model_used,
            used_fallback,
            task_kind_str,
            rag_chunks_used,
            repo_context_injected,
            prompt_tok,
            completion_tok,
            false,
            cache_key,
            cache_creation_input_tokens,
            cache_read_input_tokens,
        )
        .into_response();
    }

    // ── 11. Cache (fire-and-forget) ───────────────────────────────────────
    // Tool-call responses are never cached: `CachedProxyResponse` only
    // carries text, and tool calls are turn-specific.
    if !has_tools && tool_calls.is_none() {
        let cached_val = CachedProxyResponse {
            content: reply.clone(),
            model_used: model_used.clone(),
            used_fallback,
            task_kind: task_kind_str.clone(),
            rag_chunks_used,
            repo_context_injected,
            prompt_tokens: prompt_tok,
            completion_tokens: completion_tok,
            cache_creation_input_tokens,
            cache_read_input_tokens,
        };
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

    // ── 12. Emit structured dispatch log + build OpenAI-compatible response ──
    log_dispatch_event(&DispatchLogContext {
        task_kind: &task_kind_str,
        target_kind,
        model_used: &model_used,
        prompt_tokens: prompt_tok,
        completion_tokens: completion_tok,
        cache_creation_input_tokens: cache_creation_input_tokens.unwrap_or(0),
        cache_read_input_tokens: cache_read_input_tokens.unwrap_or(0),
        rag_chunks_used,
        repo_context_injected,
        repo_id: req.x_repo_id.as_deref(),
        cached: false,
        streaming: false,
        used_fallback,
    });

    build_oai_response(
        reply,
        tool_calls,
        finish_reason,
        model_used,
        used_fallback,
        task_kind_str,
        rag_chunks_used,
        repo_context_injected,
        prompt_tok,
        completion_tok,
        false,
        cache_key,
        cache_creation_input_tokens,
        cache_read_input_tokens,
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
    claude_payload: ClaudePayload,
) -> Response {
    // Tool-call streams are never cached (the cache only stores text).
    let skip_cache = claude_payload.tools.is_some();
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
                            let client =
                                crate::llm::grok_client::GrokClient::new(api_key.clone(), db);
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
                                cache_creation_input_tokens: None,
                                cache_read_input_tokens: None,
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
        ModelTarget::Claude { model, tier } => {
            // Real Anthropic SSE: pump `AnthropicClient::stream_message` and
            // translate each `StreamEvent` into a `StreamChunk`. We forward
            // only `TextDelta`s (the other delta variants — InputJson,
            // Thinking, Signature — are skipped to match the non-streaming
            // path's `extract_text` text-only contract). Final usage is read
            // off the last `MessageDelta` event and emitted on
            // `MessageStop`.
            //
            // Channel buffer is wider than the synthesised path's 4 because
            // a real stream produces dozens of small deltas; back-pressuring
            // the SSE pump on every token would block the upstream reqwest
            // chunk reader.
            let (tx, rx) = tokio::sync::mpsc::channel::<StreamChunk>(64);
            let model = model.clone();
            let tier = *tier;
            let anthropic_client = state.repo_state.anthropic_client.clone();
            let system = system_prompt.clone();
            let payload = claude_payload;

            tokio::spawn(async move {
                let client = match anthropic_client {
                    Some(c) => (*c).clone(),
                    None => match AnthropicClient::from_env() {
                        Ok(c) => c.with_prompt_cache(PromptCache::new("rustcode-proxy")),
                        Err(e) => {
                            let _ = tx.send(StreamChunk::Error(e.to_string())).await;
                            return;
                        }
                    },
                };

                let message_req = MessageRequest {
                    model: model.clone(),
                    max_tokens,
                    messages: payload.messages,
                    system: build_system_blocks(system.as_deref()),
                    tools: payload.tools,
                    tool_choice: payload.tool_choice,
                    temperature: None,
                    response_format: None,
                    // stream_message flips this on internally; the explicit
                    // false here matches the historical request shape so the
                    // prompt-cache request fingerprint is comparable to the
                    // non-streaming dispatch's.
                    stream: false,
                };

                let mut stream = match client.stream_message(&message_req).await {
                    Ok(stream) => stream,
                    Err(e) => {
                        warn!(error = %e, "Proxy stream: Claude stream_message failed");
                        let _ = tx.send(StreamChunk::Error(e.to_string())).await;
                        return;
                    }
                };

                // Per-stream accumulators. `model_used` is updated when the
                // `MessageStart` event arrives (Anthropic echoes the resolved
                // model slug — important for `auto` / aliased model routing).
                // `latest_usage` tracks the most recent `MessageDelta::usage`
                // so the final `Done` reflects the terminal token counts
                // including cache creation / read totals.
                let mut model_used = model.clone();
                let mut latest_usage: Option<Usage> = None;
                let mut done_sent = false;
                // Map Anthropic content-block index → OpenAI tool-call
                // ordinal. A response interleaves text and tool_use blocks,
                // so block index 1 may be tool call 0.
                let mut tool_ordinals: std::collections::HashMap<u32, u32> =
                    std::collections::HashMap::new();
                let mut next_tool_ordinal: u32 = 0;

                loop {
                    match stream.next_event().await {
                        Ok(Some(event)) => match event {
                            StreamEvent::MessageStart(start) => {
                                if !start.message.model.is_empty() {
                                    model_used = start.message.model;
                                }
                                // The initial usage carries the prompt-token
                                // count; later MessageDeltas update it with
                                // cumulative output + cache totals.
                                latest_usage = Some(start.message.usage);
                            }
                            StreamEvent::ContentBlockStart(start) => {
                                if let AnthropicContentBlock::ToolUse { id, name, .. } =
                                    start.content_block
                                {
                                    let ordinal = next_tool_ordinal;
                                    next_tool_ordinal += 1;
                                    tool_ordinals.insert(start.index, ordinal);
                                    if tx
                                        .send(StreamChunk::ToolCallStart {
                                            index: ordinal,
                                            id,
                                            name,
                                        })
                                        .await
                                        .is_err()
                                    {
                                        return;
                                    }
                                }
                            }
                            StreamEvent::ContentBlockDelta(delta) => match delta.delta {
                                ContentBlockDelta::TextDelta { text } => {
                                    if tx.send(StreamChunk::Delta(text)).await.is_err() {
                                        // Receiver dropped (client disconnected).
                                        // Stop pumping; no Done is needed.
                                        return;
                                    }
                                }
                                ContentBlockDelta::InputJsonDelta { partial_json } => {
                                    // Argument fragment for an open tool call.
                                    if let Some(&ordinal) = tool_ordinals.get(&delta.index)
                                        && tx
                                            .send(StreamChunk::ToolCallDelta {
                                                index: ordinal,
                                                arguments: partial_json,
                                            })
                                            .await
                                            .is_err()
                                    {
                                        return;
                                    }
                                }
                                // Thinking / Signature deltas are dropped:
                                // matches the non-streaming `extract_text`
                                // contract.
                                _ => {}
                            },
                            StreamEvent::MessageDelta(delta) => {
                                latest_usage = Some(delta.usage);
                            }
                            StreamEvent::MessageStop(_) => {
                                send_claude_done(
                                    &tx,
                                    model_used.clone(),
                                    latest_usage.as_ref(),
                                    tier,
                                )
                                .await;
                                done_sent = true;
                                // Don't break — keep draining so the
                                // underlying MessageStream gets a chance to
                                // settle its prompt-cache record. The next
                                // iteration sees Ok(None) and exits.
                            }
                            _ => {} // ContentBlockStop
                        },
                        Ok(None) => {
                            // Stream exhausted without an explicit
                            // MessageStop — emit Done with whatever usage
                            // we have so the cache write still fires.
                            if !done_sent {
                                send_claude_done(
                                    &tx,
                                    model_used.clone(),
                                    latest_usage.as_ref(),
                                    tier,
                                )
                                .await;
                            }
                            return;
                        }
                        Err(e) => {
                            warn!(error = %e, "Proxy stream: Claude stream error mid-flight");
                            let _ = tx.send(StreamChunk::Error(e.to_string())).await;
                            return;
                        }
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
    // Separate copies for the two closures below. The `.map` closure is
    // FnMut and only borrows on each call; the `.chain` `async move` block
    // consumes its copy when it builds `CachedProxyResponse`.
    let task_kind_for_map = task_kind_str.clone();
    let target_kind_for_map = target_kind_label(&target);
    let repo_id_for_map = req.x_repo_id.clone();
    // Model slug used for dispatch_error logging when the stream errors
    // before we observe the resolved model (e.g. before Claude's
    // MessageStart event). When the stream succeeds, the model is read
    // off the terminal StreamChunk::Done instead.
    let model_for_map = target_model_label(&target).to_string();
    let cache = Arc::clone(&state.repo_state.cache);

    // We need mutable accumulator state across closure calls.  Use an Arc<Mutex>
    // so the FnMut closure can share it with the cache-write spawned at the end.
    // The tuple carries: (model_used, used_fallback, prompt_tokens,
    // completion_tokens, cache_creation_input_tokens, cache_read_input_tokens).
    // The two cache token fields are only populated for the Claude arm.
    type FinalMeta =
        Arc<tokio::sync::Mutex<Option<(String, bool, u32, u32, Option<u32>, Option<u32>)>>>;
    let accumulated = Arc::new(tokio::sync::Mutex::new(String::new()));
    let final_meta: FinalMeta = Arc::new(tokio::sync::Mutex::new(None));

    let acc_clone = Arc::clone(&accumulated);
    let meta_clone = Arc::clone(&final_meta);

    // Set once a ToolCallStart flows through — flips the terminal
    // finish_reason from "stop" to "tool_calls".
    let mut saw_tool_call = false;

    let sse_stream = chunk_stream
        .map(move |chunk| -> Result<Event, std::convert::Infallible> {
            let id = id_clone.clone();
            let now = created;

            match chunk {
                StreamChunk::Error(e) => {
                    log_dispatch_error_event(&DispatchErrorLogContext {
                        task_kind: &task_kind_for_map,
                        target_kind: target_kind_for_map,
                        // The backend model slug isn't reliably knowable
                        // here — the stream errored before we observed a
                        // MessageStart for Claude or got Done metadata for
                        // Ollama/Grok. Use the original target's model
                        // string from the dispatcher's perspective.
                        model_used: &model_for_map,
                        error_message: &e,
                        repo_id: repo_id_for_map.as_deref(),
                        streaming: true,
                    });
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
                                tool_calls: None,
                            },
                            finish_reason: None,
                        }],
                        usage: None,
                        cache_creation_input_tokens: None,
                        cache_read_input_tokens: None,
                    };
                    let data =
                        serde_json::to_string(&chunk_resp).unwrap_or_else(|_| "{}".to_string());
                    Ok(Event::default().data(data))
                }

                StreamChunk::ToolCallStart {
                    index,
                    id: call_id,
                    name,
                } => {
                    saw_tool_call = true;
                    let chunk_resp = OaiChunkResponse {
                        id,
                        object: "chat.completion.chunk",
                        created: now,
                        model: "streaming".to_string(),
                        choices: vec![OaiChunkChoice {
                            index: 0,
                            delta: OaiDelta {
                                role: None,
                                content: None,
                                tool_calls: Some(vec![OaiDeltaToolCall {
                                    index,
                                    id: Some(call_id),
                                    kind: Some("function"),
                                    function: OaiDeltaFunction {
                                        name: Some(name),
                                        arguments: String::new(),
                                    },
                                }]),
                            },
                            finish_reason: None,
                        }],
                        usage: None,
                        cache_creation_input_tokens: None,
                        cache_read_input_tokens: None,
                    };
                    let data =
                        serde_json::to_string(&chunk_resp).unwrap_or_else(|_| "{}".to_string());
                    Ok(Event::default().data(data))
                }

                StreamChunk::ToolCallDelta { index, arguments } => {
                    let chunk_resp = OaiChunkResponse {
                        id,
                        object: "chat.completion.chunk",
                        created: now,
                        model: "streaming".to_string(),
                        choices: vec![OaiChunkChoice {
                            index: 0,
                            delta: OaiDelta {
                                role: None,
                                content: None,
                                tool_calls: Some(vec![OaiDeltaToolCall {
                                    index,
                                    id: None,
                                    kind: None,
                                    function: OaiDeltaFunction {
                                        name: None,
                                        arguments,
                                    },
                                }]),
                            },
                            finish_reason: None,
                        }],
                        usage: None,
                        cache_creation_input_tokens: None,
                        cache_read_input_tokens: None,
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
                    cache_creation_input_tokens,
                    cache_read_input_tokens,
                } => {
                    // Store final metadata so we can cache after the stream ends.
                    let pt = prompt_tokens.unwrap_or(0);
                    let ct = completion_tokens.unwrap_or(0);
                    if let Ok(mut m) = meta_clone.try_lock() {
                        *m = Some((
                            model_used.clone(),
                            used_fallback,
                            pt,
                            ct,
                            cache_creation_input_tokens,
                            cache_read_input_tokens,
                        ));
                    }

                    log_dispatch_event(&DispatchLogContext {
                        task_kind: &task_kind_for_map,
                        target_kind: target_kind_for_map,
                        model_used: &model_used,
                        prompt_tokens: pt,
                        completion_tokens: ct,
                        cache_creation_input_tokens: cache_creation_input_tokens.unwrap_or(0),
                        cache_read_input_tokens: cache_read_input_tokens.unwrap_or(0),
                        rag_chunks_used,
                        repo_context_injected,
                        repo_id: repo_id_for_map.as_deref(),
                        cached: false,
                        streaming: true,
                        used_fallback,
                    });

                    let finish_reason = if saw_tool_call {
                        "tool_calls".to_string()
                    } else {
                        "stop".to_string()
                    };
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
                                tool_calls: None,
                            },
                            finish_reason: Some(finish_reason),
                        }],
                        usage: Some(OaiUsage {
                            prompt_tokens: pt,
                            completion_tokens: ct,
                            total_tokens: pt + ct,
                        }),
                        cache_creation_input_tokens,
                        cache_read_input_tokens,
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
            // Skipped for tool-call requests: the accumulated text is not a
            // complete answer when the model stopped to call a tool.
            let acc = accumulated.lock().await;
            if skip_cache {
                return Ok::<Event, std::convert::Infallible>(Event::default().data("[DONE]"));
            }
            if let Some((
                model_used,
                used_fallback,
                pt,
                ct,
                cache_creation_input_tokens,
                cache_read_input_tokens,
            )) = final_meta.lock().await.clone()
            {
                let cached_val = CachedProxyResponse {
                    content: acc.clone(),
                    model_used: model_used.clone(),
                    used_fallback,
                    task_kind: task_kind_str,
                    rag_chunks_used,
                    repo_context_injected,
                    prompt_tokens: pt,
                    completion_tokens: ct,
                    cache_creation_input_tokens,
                    cache_read_input_tokens,
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
                tool_calls: None,
            },
            finish_reason: None,
        }],
        usage: None,
        cache_creation_input_tokens: None,
        cache_read_input_tokens: None,
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

    // Construct a RustCode virtual model entry that accepts `tools`.
    // Used for the Claude-tier entries — the proxy forwards OpenAI tool
    // definitions to Anthropic and translates `tool_use` back into
    // OpenAI `tool_calls`.
    fn rc_tools(id: &str, max_tokens: u32, max_completion_tokens: u32, now: u64) -> Self {
        Self {
            supports_tools: true,
            supports_parallel_tool_calls: true,
            ..Self::rc(id, max_tokens, max_completion_tokens, now)
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
    //   RustCode routes to Claude / Ollama / Grok automatically.
    // "auto"   — same as "rustcode" (ModelRouter decides)
    // "local"  — force Ollama regardless of task kind
    // "remote" — force Grok regardless of task kind
    // claude-* — explicit Claude tier selection
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

    // Claude tiers — clients can target Opus (planner) or Sonnet (executor)
    // directly. Read from the configured `RC_PLANNER_MODEL` /
    // `RC_EXECUTOR_MODEL` so overrides show up here instead of the
    // compiled-in defaults. Deduped when both tiers point at one slug.
    let planner = state.repo_state.model_router.planner_model().to_string();
    let executor = state.repo_state.model_router.executor_model().to_string();
    entries.push(ModelEntry::rc_tools(&planner, 200_000, 32_000, now));
    entries.push(ModelEntry::rc_tools(
        &format!("openai/{planner}"),
        200_000,
        32_000,
        now,
    ));
    if executor != planner {
        entries.push(ModelEntry::rc_tools(&executor, 200_000, 64_000, now));
        entries.push(ModelEntry::rc_tools(
            &format!("openai/{executor}"),
            200_000,
            64_000,
            now,
        ));
    }

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
//   (e.g. `anthropic/claude-opus-4-7`) when configured with a custom base URL;
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
        // often send the Anthropic model name verbatim.  Pick the right Claude tier
        // by inspecting the slug:
        //   - `claude-opus*` → Planner (Opus 4.7)
        //   - `claude-sonnet*` (and everything else under the prefix) → Executor (Sonnet 4.6)
        // When Anthropic isn't configured, fall through to the ArchitecturalReason
        // routing path which will land on Grok via `ModelRouter::route`.
        _ if model_lc.starts_with("anthropic/") || model_lc.starts_with("claude-") => {
            let stripped = model_lc.strip_prefix("anthropic/").unwrap_or(&model_lc);
            let tier = if stripped.contains("opus") {
                ClaudeTier::Planner
            } else {
                ClaudeTier::Executor
            };
            let task = match tier {
                ClaudeTier::Planner => TaskKind::ArchitecturalReason,
                ClaudeTier::Executor => TaskKind::RepoQuestion,
            };
            // Prefer a Claude target when the API key is configured; otherwise
            // fall through to the router's default route(task) behaviour.
            let target = state
                .model_router
                .claude_target(tier)
                .unwrap_or_else(|| state.model_router.route(&task));
            (task, target)
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

// ---------------------------------------------------------------------------
// OpenAI ↔ Anthropic translation (Claude targets)
// ---------------------------------------------------------------------------

// Everything the Claude dispatch paths need beyond the flattened prompt:
// a structured multi-turn message array plus translated tool definitions.
// Ollama / Grok targets ignore this and use the flattened `full_prompt`.
#[derive(Debug, Clone)]
struct ClaudePayload {
    messages: Vec<InputMessage>,
    tools: Option<Vec<ToolDefinition>>,
    tool_choice: Option<AnthropicToolChoice>,
}

// Build the Claude-side payload from the OpenAI request. RAG enrichment and
// repo context are injected into the last user turn (mirroring what
// `build_full_prompt` does for the flattened path); prior turns — including
// assistant `tool_calls` and `role: "tool"` results — are forwarded
// structurally so Claude sees real conversation history instead of a
// role-prefixed transcript.
fn build_claude_payload(
    req: &OaiChatRequest,
    rag_enriched_last_user: &str,
    repo_context: Option<&str>,
) -> ClaudePayload {
    let messages = build_claude_messages(&req.messages, rag_enriched_last_user, repo_context);

    // `tool_choice: "none"` means "don't call tools" — Anthropic has no
    // equivalent, so we drop the definitions entirely.
    let tools_disabled = matches!(
        req.tool_choice.as_ref().and_then(serde_json::Value::as_str),
        Some("none")
    );

    let tools: Option<Vec<ToolDefinition>> = if tools_disabled {
        None
    } else {
        req.tools
            .as_deref()
            .map(convert_tool_definitions)
            .filter(|t| !t.is_empty())
    };

    let tool_choice = if tools.is_some() {
        convert_tool_choice(req.tool_choice.as_ref())
    } else {
        None
    };

    ClaudePayload {
        messages,
        tools,
        tool_choice,
    }
}

// Translate the OpenAI message history into Anthropic `InputMessage`s.
//
// Rules:
//   - `system` turns are skipped (they travel via the `system` field).
//   - `tool` turns become `tool_result` blocks inside a *user* message.
//   - assistant `tool_calls` become `tool_use` blocks.
//   - consecutive same-role turns are merged — Anthropic requires strict
//     user/assistant alternation, and OpenAI clients send each tool result
//     as its own `role: "tool"` message.
//   - the last user turn is replaced by the RAG-enriched text, with repo
//     context prepended when present.
fn build_claude_messages(
    messages: &[OaiMessage],
    rag_enriched_last_user: &str,
    repo_context: Option<&str>,
) -> Vec<InputMessage> {
    let last_user_idx = messages.iter().rposition(|m| m.role == "user");

    let mut out: Vec<InputMessage> = Vec::new();

    for (i, msg) in messages.iter().enumerate() {
        let (role, blocks) = match msg.role.as_str() {
            "system" => continue,
            "tool" => {
                let tool_use_id = msg.tool_call_id.clone().unwrap_or_default();
                (
                    "user",
                    vec![InputContentBlock::ToolResult {
                        tool_use_id,
                        content: vec![ToolResultContentBlock::Text {
                            text: msg.content.clone(),
                        }],
                        is_error: false,
                    }],
                )
            }
            "assistant" => {
                let mut blocks: Vec<InputContentBlock> = Vec::new();
                if !msg.content.is_empty() {
                    blocks.push(InputContentBlock::Text {
                        text: msg.content.clone(),
                        cache_control: None,
                    });
                }
                if let Some(calls) = &msg.tool_calls {
                    for call in calls {
                        // OpenAI encodes arguments as a JSON string; fall
                        // back to an empty object on malformed fragments.
                        let input = serde_json::from_str(&call.function.arguments)
                            .unwrap_or_else(|_| serde_json::json!({}));
                        blocks.push(InputContentBlock::ToolUse {
                            id: call.id.clone(),
                            name: call.function.name.clone(),
                            input,
                        });
                    }
                }
                if blocks.is_empty() {
                    continue;
                }
                ("assistant", blocks)
            }
            // "user" and any unrecognised role.
            _ => {
                let text = if Some(i) == last_user_idx {
                    repo_context.map_or_else(
                        || rag_enriched_last_user.to_string(),
                        |ctx| format!("[Repo Context]\n{ctx}\n\n{rag_enriched_last_user}"),
                    )
                } else {
                    msg.content.clone()
                };
                if text.is_empty() {
                    continue;
                }
                (
                    "user",
                    vec![InputContentBlock::Text {
                        text,
                        cache_control: None,
                    }],
                )
            }
        };

        if let Some(prev) = out.last_mut() {
            if prev.role == role {
                prev.content.extend(blocks);
                continue;
            }
        }
        out.push(InputMessage {
            role: role.to_string(),
            content: blocks,
        });
    }

    // Anthropic rejects an empty messages array — fall back to the enriched
    // prompt (or a placeholder when even that is empty).
    if out.is_empty() {
        let fallback = if rag_enriched_last_user.is_empty() {
            "(empty message)"
        } else {
            rag_enriched_last_user
        };
        out.push(InputMessage::user_text(fallback));
    }
    out
}

// Convert OpenAI `{type:"function", function:{...}}` tool definitions into
// Anthropic `ToolDefinition`s. Non-function tool types are skipped.
fn convert_tool_definitions(tools: &[OaiTool]) -> Vec<ToolDefinition> {
    tools
        .iter()
        .filter(|t| t.kind == "function")
        .map(|t| ToolDefinition {
            name: t.function.name.clone(),
            description: t.function.description.clone(),
            input_schema: t
                .function
                .parameters
                .clone()
                .unwrap_or_else(|| serde_json::json!({ "type": "object", "properties": {} })),
        })
        .collect()
}

// Map the OpenAI `tool_choice` field onto Anthropic's. `"none"` is handled
// by the caller (tools are dropped); unknown shapes fall back to `None`
// (Anthropic defaults to auto).
fn convert_tool_choice(choice: Option<&serde_json::Value>) -> Option<AnthropicToolChoice> {
    match choice? {
        serde_json::Value::String(s) => match s.as_str() {
            "auto" => Some(AnthropicToolChoice::Auto),
            "required" | "any" => Some(AnthropicToolChoice::Any),
            _ => None,
        },
        serde_json::Value::Object(obj) => {
            let name = obj.get("function")?.get("name")?.as_str()?;
            Some(AnthropicToolChoice::Tool {
                name: name.to_string(),
            })
        }
        _ => None,
    }
}

// Translate Claude `tool_use` output blocks into OpenAI `tool_calls`.
// Returns `None` when the response contained no tool calls.
fn extract_tool_calls(resp: &MessageResponse) -> Option<Vec<OaiToolCall>> {
    let calls: Vec<OaiToolCall> = resp
        .content
        .iter()
        .filter_map(|block| match block {
            AnthropicContentBlock::ToolUse { id, name, input } => Some(OaiToolCall {
                id: id.clone(),
                kind: "function".to_string(),
                function: OaiFunctionCall {
                    name: name.clone(),
                    arguments: input.to_string(),
                },
            }),
            _ => None,
        })
        .collect();
    (!calls.is_empty()).then_some(calls)
}

// Map an Anthropic `stop_reason` onto the OpenAI `finish_reason` vocabulary.
fn map_stop_reason(stop_reason: Option<&str>, has_tool_calls: bool) -> String {
    match stop_reason {
        Some("tool_use") => "tool_calls".to_string(),
        Some("max_tokens") => "length".to_string(),
        _ if has_tool_calls => "tool_calls".to_string(),
        _ => "stop".to_string(),
    }
}

// Result of a single backend dispatch call.
#[derive(Debug, Clone)]
struct DispatchOutcome {
    reply: String,
    model_used: String,
    used_fallback: bool,
    tokens_used: Option<u32>,
    // Anthropic prompt-cache write tokens (only set for Claude responses).
    cache_creation_input_tokens: Option<u32>,
    // Anthropic prompt-cache read tokens (only set for Claude responses).
    cache_read_input_tokens: Option<u32>,
    // Tool calls requested by the model (only set for Claude responses
    // when the client sent tool definitions).
    tool_calls: Option<Vec<OaiToolCall>>,
    // OpenAI finish_reason: "stop" | "tool_calls" | "length".
    finish_reason: String,
    // Backend error message when the dispatch failed. The error is *also*
    // surfaced as the `reply` body (so clients still get a textual
    // response), but having it as an explicit field lets callers
    // distinguish errors from successes without string-matching on `reply`.
    // Populated only by `DispatchOutcome::error`.
    error: Option<String>,
}

impl DispatchOutcome {
    fn ok(reply: String, model_used: String, tokens: Option<u32>) -> Self {
        Self {
            reply,
            model_used,
            used_fallback: false,
            tokens_used: tokens,
            cache_creation_input_tokens: None,
            cache_read_input_tokens: None,
            tool_calls: None,
            finish_reason: "stop".to_string(),
            error: None,
        }
    }

    fn error(msg: String, model: String) -> Self {
        Self {
            reply: msg.clone(),
            model_used: model,
            used_fallback: false,
            tokens_used: None,
            cache_creation_input_tokens: None,
            cache_read_input_tokens: None,
            tool_calls: None,
            finish_reason: "stop".to_string(),
            error: Some(msg),
        }
    }
}

// Dispatch to Claude, Ollama, or Grok depending on the resolved target.
// Duplicated here (rather than sharing with `repos.rs`) to keep the proxy
// self-contained and avoid coupling to private internals.
async fn dispatch(
    state: &RepoAppState,
    req: &CompletionRequest,
    target: &ModelTarget,
    claude_payload: &ClaudePayload,
) -> DispatchOutcome {
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
                    DispatchOutcome {
                        reply: resp.content,
                        model_used: resp.model_used,
                        used_fallback: resp.used_fallback,
                        tokens_used: tokens,
                        cache_creation_input_tokens: None,
                        cache_read_input_tokens: None,
                        tool_calls: None,
                        finish_reason: "stop".to_string(),
                        error: None,
                    }
                }
                Err(e) => {
                    warn!(error = %e, "Proxy: Ollama dispatch failed");
                    DispatchOutcome::error(format!("Local model error: {}", e), model.clone())
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
                        let client = crate::llm::grok_client::GrokClient::new(api_key.clone(), db);
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
                    DispatchOutcome::ok(resp.content, model.clone(), Some(tokens))
                }
                Err(e) => {
                    warn!(error = %e, "Proxy: Grok dispatch failed");
                    DispatchOutcome::error(format!("Remote model error: {}", e), model.clone())
                }
            }
        }

        ModelTarget::Claude { model, tier } => {
            debug!(model = %model, tier = %tier, "Proxy: dispatching to Claude");
            dispatch_claude(state, req, model, *tier, claude_payload).await
        }
    }
}

// Build an AnthropicClient, send a single message, and translate the response
// into a DispatchOutcome. Uses the cached client from `RepoAppState` when set;
// otherwise builds a one-shot client from `ANTHROPIC_API_KEY`.
async fn dispatch_claude(
    state: &RepoAppState,
    req: &CompletionRequest,
    model: &str,
    tier: ClaudeTier,
    claude_payload: &ClaudePayload,
) -> DispatchOutcome {
    let client = match state.anthropic_client.as_ref() {
        Some(c) => (**c).clone(),
        None => match AnthropicClient::from_env() {
            Ok(c) => c.with_prompt_cache(PromptCache::new("rustcode-proxy")),
            Err(e) => {
                warn!(error = %e, "Proxy: ANTHROPIC_API_KEY not configured");
                return DispatchOutcome::error(
                    format!("Claude unavailable: {}", e),
                    model.to_string(),
                );
            }
        },
    };

    // Prepend retrieved memories to the last user turn when memory injection
    // is configured. Memory lookup is best-effort: on failure we log and
    // proceed with the unmodified messages rather than failing the request.
    let mut messages = claude_payload.messages.clone();
    if let Some(memory) = state.agent_memory.as_ref() {
        match memory.search(&req.user_prompt, None, 5).await {
            Ok(hits) if !hits.is_empty() => {
                let block = crate::memory::format_memories_for_prompt(&hits);
                debug!(
                    matches = hits.len(),
                    "Proxy: prepending memory block to last user turn"
                );
                if let Some(last_user) = messages.iter_mut().rev().find(|m| m.role == "user") {
                    last_user.content.insert(
                        0,
                        InputContentBlock::Text {
                            text: block,
                            cache_control: None,
                        },
                    );
                }
            }
            Ok(_) => {}
            Err(e) => {
                warn!(error = %e, "Proxy: memory.search failed — proceeding without memory");
            }
        }
    }

    let message_req = MessageRequest {
        model: model.to_string(),
        max_tokens: req.max_tokens,
        messages,
        system: build_system_blocks(req.system_prompt.as_deref()),
        tools: claude_payload.tools.clone(),
        tool_choice: claude_payload.tool_choice.clone(),
        temperature: None,
        response_format: None,
        stream: false,
    };

    match client.send_message(&message_req).await {
        Ok(resp) => {
            let reply = extract_text(&resp);
            let tool_calls = extract_tool_calls(&resp);
            let finish_reason = map_stop_reason(resp.stop_reason.as_deref(), tool_calls.is_some());
            let tokens = Some(resp.usage.total_tokens());
            let cache_creation = (resp.usage.cache_creation_input_tokens > 0)
                .then_some(resp.usage.cache_creation_input_tokens);
            let cache_read = (resp.usage.cache_read_input_tokens > 0)
                .then_some(resp.usage.cache_read_input_tokens);
            info!(
                model = %model,
                tier = %tier,
                input_tokens = resp.usage.input_tokens,
                output_tokens = resp.usage.output_tokens,
                cache_creation_input_tokens = resp.usage.cache_creation_input_tokens,
                cache_read_input_tokens = resp.usage.cache_read_input_tokens,
                "Proxy: Claude dispatch succeeded"
            );
            DispatchOutcome {
                reply,
                model_used: resp.model,
                used_fallback: false,
                tokens_used: tokens,
                cache_creation_input_tokens: cache_creation,
                cache_read_input_tokens: cache_read,
                tool_calls,
                finish_reason,
                error: None,
            }
        }
        Err(e) => {
            warn!(error = %e, "Proxy: Claude dispatch failed");
            DispatchOutcome::error(format!("Claude error: {}", e), model.to_string())
        }
    }
}

// Anthropic prompt-cache marker is only honoured when the cached block is at
// least 1024 tokens (Sonnet/Opus). The proxy can't tokenise without an extra
// dependency, so we use a 4-chars-per-token heuristic — the same approximation
// the cost estimator already uses — and skip the marker on prompts that won't
// clear the threshold.
const PROMPT_CACHE_MIN_TOKENS: usize = 1024;
const PROMPT_CACHE_CHAR_PER_TOKEN: usize = 4;
const PROMPT_CACHE_MIN_CHARS: usize = PROMPT_CACHE_MIN_TOKENS * PROMPT_CACHE_CHAR_PER_TOKEN;

// Short label for a `ModelTarget` variant. Used as the `target` field on
// structured dispatch logs so we can filter by backend without parsing the
// model slug. "claude" covers both Planner (Opus) and Sonnet tiers; the
// `tier` field already lives on the Claude-specific log entries.
const fn target_kind_label(target: &ModelTarget) -> &'static str {
    match target {
        ModelTarget::Local { .. } => "local",
        ModelTarget::Remote { .. } => "remote",
        ModelTarget::Claude { .. } => "claude",
    }
}

// Extract the model slug from a `ModelTarget` regardless of variant. Used
// for the `model` field on structured dispatch_error events emitted before
// the backend has confirmed its resolved model slug.
fn target_model_label(target: &ModelTarget) -> &str {
    match target {
        ModelTarget::Local { model, .. }
        | ModelTarget::Remote { model, .. }
        | ModelTarget::Claude { model, .. } => model,
    }
}

/// Carrier for the fields a "proxy dispatch" log line emits. Keeps the three
/// call sites (non-stream cache hit, non-stream dispatch, streaming Done) in
/// sync — drifting field names across paths would make the log unusable for
/// downstream metrics.
///
/// `task_kind` is a pre-formatted string rather than `&TaskKind` so the
/// streaming closure (which captures variables across multiple FnMut calls)
/// can reuse the same `format!("{:?}", task_kind)` it already needs for
/// `CachedProxyResponse::task_kind`. `TaskKind::Display` delegates to
/// `Debug`, so the wire format is identical either way.
struct DispatchLogContext<'a> {
    task_kind: &'a str,
    target_kind: &'static str,
    model_used: &'a str,
    prompt_tokens: u32,
    completion_tokens: u32,
    cache_creation_input_tokens: u32,
    cache_read_input_tokens: u32,
    rag_chunks_used: usize,
    repo_context_injected: bool,
    repo_id: Option<&'a str>,
    cached: bool,
    streaming: bool,
    used_fallback: bool,
}

// Emit the canonical "proxy.dispatch" structured event. The fields are a
// stable surface for log-based routing-quality analysis (see the RC-API
// "routing heuristic tuning" TODO).
fn log_dispatch_event(ctx: &DispatchLogContext<'_>) {
    info!(
        event = "proxy.dispatch",
        task_kind = ctx.task_kind,
        target = ctx.target_kind,
        model = %ctx.model_used,
        prompt_tokens = ctx.prompt_tokens,
        completion_tokens = ctx.completion_tokens,
        cache_creation_input_tokens = ctx.cache_creation_input_tokens,
        cache_read_input_tokens = ctx.cache_read_input_tokens,
        rag_chunks_used = ctx.rag_chunks_used,
        repo_context_injected = ctx.repo_context_injected,
        repo_id = ctx.repo_id.unwrap_or(""),
        cached = ctx.cached,
        streaming = ctx.streaming,
        used_fallback = ctx.used_fallback,
        "Proxy dispatch completed"
    );
}

/// Carrier for the fields a `proxy.dispatch_error` log line emits. Shape
/// intentionally mirrors `DispatchLogContext` for the fields they share
/// (task_kind, target, model, repo_id, streaming) so downstream queries
/// that group by `task_kind` + `target` work the same on both event types
/// — the error variant just swaps the success-side token fields for an
/// `error_message`.
struct DispatchErrorLogContext<'a> {
    task_kind: &'a str,
    target_kind: &'static str,
    model_used: &'a str,
    error_message: &'a str,
    repo_id: Option<&'a str>,
    streaming: bool,
}

// Emit the canonical "proxy.dispatch_error" structured event. Pairs with
// log_dispatch_event so a downstream metrics roll-up can compute
// dispatch_error_rate = count(proxy.dispatch_error) /
// (count(proxy.dispatch) + count(proxy.dispatch_error)) grouped by
// task_kind/target.
fn log_dispatch_error_event(ctx: &DispatchErrorLogContext<'_>) {
    warn!(
        event = "proxy.dispatch_error",
        task_kind = ctx.task_kind,
        target = ctx.target_kind,
        model = %ctx.model_used,
        error = ctx.error_message,
        repo_id = ctx.repo_id.unwrap_or(""),
        streaming = ctx.streaming,
        "Proxy dispatch failed"
    );
}

// Build the Anthropic `system` field for a Claude dispatch. Returns `None`
// when the caller had no system prompt. Otherwise emits a single `text`
// block, marked `cache_control: { type: "ephemeral" }` once the prompt is
// long enough to clear Anthropic's minimum cacheable size.
fn build_system_blocks(system_prompt: Option<&str>) -> Option<Vec<SystemBlock>> {
    let text = system_prompt?;
    if text.is_empty() {
        return None;
    }
    let block = if text.len() >= PROMPT_CACHE_MIN_CHARS {
        SystemBlock::cached_text(text)
    } else {
        SystemBlock::text(text)
    };
    Some(vec![block])
}

// Build and send the terminal `StreamChunk::Done` for a Claude stream.
// Centralises the usage → cache-token translation so the `MessageStop` and
// "stream exhausted without MessageStop" paths agree. Errors on the send
// (receiver dropped) are intentionally swallowed: the client is already
// gone, no further work would change the outcome.
async fn send_claude_done(
    tx: &tokio::sync::mpsc::Sender<StreamChunk>,
    model_used: String,
    usage: Option<&Usage>,
    tier: ClaudeTier,
) {
    let (input_tokens, output_tokens, cache_creation, cache_read) =
        usage.map_or((None, None, None, None), |u| {
            (
                Some(u.input_tokens),
                Some(u.output_tokens),
                (u.cache_creation_input_tokens > 0).then_some(u.cache_creation_input_tokens),
                (u.cache_read_input_tokens > 0).then_some(u.cache_read_input_tokens),
            )
        });
    info!(
        model = %model_used,
        tier = %tier,
        input_tokens = input_tokens.unwrap_or(0),
        output_tokens = output_tokens.unwrap_or(0),
        cache_creation_input_tokens = cache_creation.unwrap_or(0),
        cache_read_input_tokens = cache_read.unwrap_or(0),
        "Proxy stream: Claude stream finished"
    );
    let _ = tx
        .send(StreamChunk::Done {
            model_used,
            used_fallback: false,
            prompt_tokens: input_tokens,
            completion_tokens: output_tokens,
            cache_creation_input_tokens: cache_creation,
            cache_read_input_tokens: cache_read,
        })
        .await;
}

// Concatenate all Text content blocks from a Claude response into a single string.
fn extract_text(resp: &MessageResponse) -> String {
    let mut buf = String::new();
    for block in &resp.content {
        if let AnthropicContentBlock::Text { text } = block {
            if !buf.is_empty() {
                buf.push('\n');
            }
            buf.push_str(text);
        }
    }
    buf
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
    tool_calls: Option<Vec<OaiToolCall>>,
    finish_reason: String,
    model_used: String,
    used_fallback: bool,
    task_kind: String,
    rag_chunks_used: usize,
    repo_context_injected: bool,
    prompt_tokens: u32,
    completion_tokens: u32,
    cached: bool,
    cache_key: String,
    cache_creation_input_tokens: Option<u32>,
    cache_read_input_tokens: Option<u32>,
) -> Json<OaiChatResponse> {
    // OpenAI emits `content: null` (not "") on turns that only carry
    // tool calls; strict clients validate against that shape.
    let content = if content.is_empty() && tool_calls.is_some() {
        None
    } else {
        Some(content)
    };
    Json(OaiChatResponse {
        id: format!("chatcmpl-rc-{}", Uuid::new_v4()),
        object: "chat.completion".to_string(),
        created: unix_now(),
        model: model_used.clone(),
        choices: vec![OaiChoice {
            index: 0,
            message: OaiAssistantMessage {
                role: "assistant".to_string(),
                content,
                tool_calls,
            },
            finish_reason,
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
            cache_creation_input_tokens,
            cache_read_input_tokens,
        },
    })
}

// Build a deterministic cache key for the proxy.
// Identical to `repos.rs::build_cache_key` but namespaced as `proxy:`.
fn build_proxy_cache_key(target: &ModelTarget, prompt: &str, repo_id: Option<&str>) -> String {
    let label = match target {
        ModelTarget::Local { model, .. } => format!("local:{}", model),
        ModelTarget::Remote { model, .. } => format!("remote:{}", model),
        ModelTarget::Claude { model, tier } => format!("claude:{}:{}", tier, model),
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
                tool_calls: None,
                tool_call_id: None,
            },
            OaiMessage {
                role: "user".to_string(),
                content: "What is the BTC trend?".to_string(),
                tool_calls: None,
                tool_call_id: None,
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
            tool_calls: None,
            tool_call_id: None,
        }];
        let prompt = build_full_prompt(&messages, "Explain RSI divergence.", None);
        assert!(!prompt.contains("[System]"));
        assert!(!prompt.contains("[Repo Context]"));
        assert!(prompt.contains("[Conversation]"));
        assert!(prompt.contains("Explain RSI divergence."));
    }

    #[test]
    fn build_claude_messages_forwards_history_and_merges_tool_results() {
        // A typical agent loop: user asks, assistant calls two tools, the
        // client sends both results back as separate `role: "tool"` turns,
        // then asks a follow-up. The tool results must merge into a single
        // user message (Anthropic requires user/assistant alternation) and
        // the assistant tool_calls must round-trip as tool_use blocks.
        let messages = vec![
            OaiMessage {
                role: "system".to_string(),
                content: "You are helpful.".to_string(),
                tool_calls: None,
                tool_call_id: None,
            },
            OaiMessage {
                role: "user".to_string(),
                content: "read two files".to_string(),
                tool_calls: None,
                tool_call_id: None,
            },
            OaiMessage {
                role: "assistant".to_string(),
                content: String::new(),
                tool_calls: Some(vec![
                    OaiToolCall {
                        id: "call_1".to_string(),
                        kind: "function".to_string(),
                        function: OaiFunctionCall {
                            name: "read_file".to_string(),
                            arguments: "{\"path\":\"a.rs\"}".to_string(),
                        },
                    },
                    OaiToolCall {
                        id: "call_2".to_string(),
                        kind: "function".to_string(),
                        function: OaiFunctionCall {
                            name: "read_file".to_string(),
                            arguments: "{\"path\":\"b.rs\"}".to_string(),
                        },
                    },
                ]),
                tool_call_id: None,
            },
            OaiMessage {
                role: "tool".to_string(),
                content: "contents of a".to_string(),
                tool_calls: None,
                tool_call_id: Some("call_1".to_string()),
            },
            OaiMessage {
                role: "tool".to_string(),
                content: "contents of b".to_string(),
                tool_calls: None,
                tool_call_id: Some("call_2".to_string()),
            },
            OaiMessage {
                role: "user".to_string(),
                content: "now summarise".to_string(),
                tool_calls: None,
                tool_call_id: None,
            },
        ];

        let out = build_claude_messages(&messages, "now summarise", None);

        // system skipped; tool results + follow-up user merge into one turn:
        // [user, assistant(tool_use x2), user(tool_result x2 + text)]
        assert_eq!(out.len(), 3);
        assert_eq!(out[0].role, "user");
        assert_eq!(out[1].role, "assistant");
        assert_eq!(out[1].content.len(), 2);
        assert!(matches!(
            &out[1].content[0],
            InputContentBlock::ToolUse { id, name, .. }
                if id == "call_1" && name == "read_file"
        ));
        assert_eq!(out[2].role, "user");
        assert_eq!(out[2].content.len(), 3);
        assert!(matches!(
            &out[2].content[0],
            InputContentBlock::ToolResult { tool_use_id, .. } if tool_use_id == "call_1"
        ));
        assert!(matches!(
            &out[2].content[1],
            InputContentBlock::ToolResult { tool_use_id, .. } if tool_use_id == "call_2"
        ));
        assert!(matches!(
            &out[2].content[2],
            InputContentBlock::Text { text, .. } if text == "now summarise"
        ));
    }

    #[test]
    fn build_claude_messages_injects_rag_and_repo_context_into_last_user_turn() {
        let messages = vec![OaiMessage {
            role: "user".to_string(),
            content: "what does main do?".to_string(),
            tool_calls: None,
            tool_call_id: None,
        }];
        let out = build_claude_messages(
            &messages,
            "RAG:\nfn main() {}\n\nwhat does main do?",
            Some("tree: src/main.rs"),
        );
        assert_eq!(out.len(), 1);
        assert!(matches!(
            &out[0].content[0],
            InputContentBlock::Text { text, .. }
                if text.contains("[Repo Context]")
                    && text.contains("tree: src/main.rs")
                    && text.contains("RAG:")
        ));
    }

    #[test]
    fn convert_tool_choice_maps_openai_variants() {
        assert_eq!(
            convert_tool_choice(Some(&serde_json::json!("auto"))),
            Some(AnthropicToolChoice::Auto)
        );
        assert_eq!(
            convert_tool_choice(Some(&serde_json::json!("required"))),
            Some(AnthropicToolChoice::Any)
        );
        assert_eq!(convert_tool_choice(Some(&serde_json::json!("none"))), None);
        assert_eq!(
            convert_tool_choice(Some(&serde_json::json!({
                "type": "function",
                "function": { "name": "read_file" }
            }))),
            Some(AnthropicToolChoice::Tool {
                name: "read_file".to_string()
            })
        );
        assert_eq!(convert_tool_choice(None), None);
    }

    #[test]
    fn convert_tool_definitions_defaults_missing_parameters() {
        let tools = vec![OaiTool {
            kind: "function".to_string(),
            function: OaiFunctionDef {
                name: "ping".to_string(),
                description: Some("health check".to_string()),
                parameters: None,
            },
        }];
        let defs = convert_tool_definitions(&tools);
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].name, "ping");
        assert_eq!(defs[0].input_schema["type"], "object");
    }

    #[test]
    fn extract_tool_calls_translates_tool_use_blocks() {
        let resp = MessageResponse {
            id: "msg_1".to_string(),
            kind: "message".to_string(),
            role: "assistant".to_string(),
            content: vec![
                AnthropicContentBlock::Text {
                    text: "Let me check.".to_string(),
                },
                AnthropicContentBlock::ToolUse {
                    id: "toolu_1".to_string(),
                    name: "grep".to_string(),
                    input: serde_json::json!({ "pattern": "fn main" }),
                },
            ],
            model: "claude-sonnet-4-6".to_string(),
            stop_reason: Some("tool_use".to_string()),
            stop_sequence: None,
            usage: Usage::default(),
            request_id: None,
        };
        let calls = extract_tool_calls(&resp).expect("tool calls");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].id, "toolu_1");
        assert_eq!(calls[0].function.name, "grep");
        assert!(calls[0].function.arguments.contains("fn main"));

        // Text-only responses yield None.
        let text_only = MessageResponse {
            content: vec![AnthropicContentBlock::Text {
                text: "done".to_string(),
            }],
            ..resp
        };
        assert!(extract_tool_calls(&text_only).is_none());
    }

    #[test]
    fn map_stop_reason_covers_openai_vocabulary() {
        assert_eq!(map_stop_reason(Some("tool_use"), true), "tool_calls");
        assert_eq!(map_stop_reason(Some("max_tokens"), false), "length");
        assert_eq!(map_stop_reason(Some("end_turn"), false), "stop");
        // Defensive: tool calls present but stop_reason missing.
        assert_eq!(map_stop_reason(None, true), "tool_calls");
        assert_eq!(map_stop_reason(None, false), "stop");
    }

    #[test]
    fn oai_message_accepts_null_content_and_tool_fields() {
        // Assistant turn with tool_calls and `content: null` — the shape
        // Zed / OpenAI SDKs send back as conversation history.
        let raw = r#"{
            "role": "assistant",
            "content": null,
            "tool_calls": [{
                "id": "call_9",
                "type": "function",
                "function": { "name": "ls", "arguments": "{}" }
            }]
        }"#;
        let msg: OaiMessage = serde_json::from_str(raw).expect("deserialise");
        assert_eq!(msg.role, "assistant");
        assert_eq!(msg.content, "");
        assert_eq!(msg.tool_calls.as_ref().map(Vec::len), Some(1));

        // Tool-result turn.
        let raw = r#"{ "role": "tool", "content": "ok", "tool_call_id": "call_9" }"#;
        let msg: OaiMessage = serde_json::from_str(raw).expect("deserialise");
        assert_eq!(msg.tool_call_id.as_deref(), Some("call_9"));
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

    #[test]
    fn build_system_blocks_returns_none_for_missing_or_empty_prompt() {
        assert!(build_system_blocks(None).is_none());
        assert!(build_system_blocks(Some("")).is_none());
    }

    #[test]
    fn build_system_blocks_skips_cache_marker_below_threshold() {
        let small = "small system prompt";
        let blocks = build_system_blocks(Some(small)).expect("should emit a block");
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].text, small);
        assert!(blocks[0].cache_control.is_none());
    }

    #[test]
    fn build_system_blocks_marks_long_prompt_as_ephemeral() {
        // The 4-chars-per-token heuristic puts the threshold at 4096 chars.
        let large = "x".repeat(PROMPT_CACHE_MIN_CHARS);
        let blocks = build_system_blocks(Some(&large)).expect("should emit a block");
        assert_eq!(blocks.len(), 1);
        assert_eq!(
            blocks[0].cache_control.as_ref().map(|c| c.kind.as_str()),
            Some("ephemeral"),
            "expected ephemeral cache marker on long system prompt"
        );
    }

    #[test]
    fn streaming_final_chunk_serializes_cache_token_fields_when_set() {
        // Final-chunk shape on a Claude-served stream: usage carries the
        // standard OpenAI token counts and the proxy adds the two
        // Anthropic-specific cache counters so SSE clients can read them off
        // the wire the same way the non-streaming path exposes them via
        // x_ra_metadata.
        let chunk = OaiChunkResponse {
            id: "chatcmpl-rc-final".to_string(),
            object: "chat.completion.chunk",
            created: 0,
            model: "claude-sonnet-4-6".to_string(),
            choices: vec![OaiChunkChoice {
                index: 0,
                delta: OaiDelta {
                    role: None,
                    content: None,
                    tool_calls: None,
                },
                finish_reason: Some("stop".to_string()),
            }],
            usage: Some(OaiUsage {
                prompt_tokens: 100,
                completion_tokens: 50,
                total_tokens: 150,
            }),
            cache_creation_input_tokens: Some(1234),
            cache_read_input_tokens: Some(5678),
        };
        let json: serde_json::Value = serde_json::to_value(&chunk).expect("chunk should serialize");
        assert_eq!(json["cache_creation_input_tokens"], serde_json::json!(1234));
        assert_eq!(json["cache_read_input_tokens"], serde_json::json!(5678));
    }

    #[tokio::test]
    async fn send_claude_done_emits_cache_tokens_only_when_nonzero() {
        // Helper used by the native-SSE Claude arm to terminate the stream.
        // When the final `Usage` carries cache_creation/cache_read tokens,
        // those flow into `StreamChunk::Done` (so downstream `OaiChunkResponse`
        // can expose them on the wire). When both are zero, the
        // `then_some` guards keep them out of the chunk — keeps the cache
        // write metadata honest about whether Anthropic actually counted a
        // cache hit.
        let (tx, mut rx) = tokio::sync::mpsc::channel::<StreamChunk>(2);
        let usage = Usage {
            input_tokens: 1_500,
            cache_creation_input_tokens: 1_200,
            cache_read_input_tokens: 0,
            output_tokens: 320,
        };
        send_claude_done(
            &tx,
            "claude-sonnet-4-6".to_string(),
            Some(&usage),
            ClaudeTier::Executor,
        )
        .await;
        let chunk = rx.recv().await.expect("Done chunk should be sent");
        match chunk {
            StreamChunk::Done {
                model_used,
                used_fallback,
                prompt_tokens,
                completion_tokens,
                cache_creation_input_tokens,
                cache_read_input_tokens,
            } => {
                assert_eq!(model_used, "claude-sonnet-4-6");
                assert!(!used_fallback);
                assert_eq!(prompt_tokens, Some(1_500));
                assert_eq!(completion_tokens, Some(320));
                assert_eq!(cache_creation_input_tokens, Some(1_200));
                // Cache-read tokens are zero ⇒ the helper drops the field
                // (None) rather than emitting an explicit 0.
                assert_eq!(cache_read_input_tokens, None);
            }
            other => panic!("expected StreamChunk::Done, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn send_claude_done_handles_missing_usage_gracefully() {
        // Stream exhausted before any MessageDelta arrived. The helper
        // should still emit Done so the cache-write tail runs — token
        // fields are simply absent.
        let (tx, mut rx) = tokio::sync::mpsc::channel::<StreamChunk>(2);
        send_claude_done(
            &tx,
            "claude-opus-4-7".to_string(),
            None,
            ClaudeTier::Planner,
        )
        .await;
        let chunk = rx.recv().await.expect("Done chunk should be sent");
        match chunk {
            StreamChunk::Done {
                prompt_tokens,
                completion_tokens,
                cache_creation_input_tokens,
                cache_read_input_tokens,
                ..
            } => {
                assert_eq!(prompt_tokens, None);
                assert_eq!(completion_tokens, None);
                assert_eq!(cache_creation_input_tokens, None);
                assert_eq!(cache_read_input_tokens, None);
            }
            other => panic!("expected StreamChunk::Done, got {other:?}"),
        }
    }

    #[test]
    fn streaming_chunk_omits_cache_token_fields_when_unset() {
        // The two cache fields are `#[serde(skip_serializing_if = "Option::is_none")]`,
        // so non-Claude paths (and Claude chunks before Done) emit the standard
        // OpenAI shape without the extension fields.
        let chunk = OaiChunkResponse {
            id: "chatcmpl-rc-mid".to_string(),
            object: "chat.completion.chunk",
            created: 0,
            model: "streaming".to_string(),
            choices: vec![OaiChunkChoice {
                index: 0,
                delta: OaiDelta {
                    role: None,
                    content: Some("hello".to_string()),
                    tool_calls: None,
                },
                finish_reason: None,
            }],
            usage: None,
            cache_creation_input_tokens: None,
            cache_read_input_tokens: None,
        };
        let json: serde_json::Value = serde_json::to_value(&chunk).expect("chunk should serialize");
        assert!(json.get("cache_creation_input_tokens").is_none());
        assert!(json.get("cache_read_input_tokens").is_none());
    }

    #[test]
    fn target_kind_label_emits_stable_strings_per_variant() {
        // These labels are part of the structured dispatch-log surface
        // (`target = "local" | "remote" | "claude"`). Downstream log queries
        // pin against them, so the strings must stay stable; renaming any
        // requires a coordinated log-pipeline change.
        let local = ModelTarget::Local {
            model: "qwen2.5-coder:7b".to_string(),
            base_url: "http://localhost:11434".to_string(),
        };
        let remote = ModelTarget::Remote {
            model: "grok-3".to_string(),
            api_key: "test".to_string(),
        };
        let claude = ModelTarget::Claude {
            model: "claude-sonnet-4-6".to_string(),
            tier: ClaudeTier::Executor,
        };
        assert_eq!(target_kind_label(&local), "local");
        assert_eq!(target_kind_label(&remote), "remote");
        assert_eq!(target_kind_label(&claude), "claude");
        // target_model_label pulls the model slug out of any variant — used
        // when the stream errors before the backend confirms its resolved
        // model on a MessageStart event.
        assert_eq!(target_model_label(&local), "qwen2.5-coder:7b");
        assert_eq!(target_model_label(&remote), "grok-3");
        assert_eq!(target_model_label(&claude), "claude-sonnet-4-6");
    }

    #[test]
    fn dispatch_outcome_error_constructor_sets_error_and_reply() {
        // Two facts the rest of the proxy depends on:
        // 1. `reply` carries the error text so the client still sees a
        //    human-readable response in OaiChatResponse.message.content.
        // 2. `error` carries the same text, signalling to
        //    handle_chat_completions that this was a backend failure
        //    (skip the cache write, emit proxy.dispatch_error instead of
        //    proxy.dispatch).
        let outcome =
            DispatchOutcome::error("connection refused".to_string(), "grok-3".to_string());
        assert_eq!(outcome.reply, "connection refused");
        assert_eq!(outcome.error.as_deref(), Some("connection refused"));
        assert_eq!(outcome.model_used, "grok-3");
        assert!(!outcome.used_fallback);
        assert!(outcome.tokens_used.is_none());

        // Sanity: the success constructor leaves `error` as None so the
        // "fast path" branch in handle_chat_completions falls through to
        // the normal cache write + success log.
        let ok = DispatchOutcome::ok("hello".to_string(), "grok-3".to_string(), Some(42));
        assert!(ok.error.is_none());
    }
}
