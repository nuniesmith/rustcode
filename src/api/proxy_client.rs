//! Standalone RustCode proxy client for external apps (e.g. the futures trading app).
//!
//! # Purpose
//!
//! Drop this file into any Rust project that currently calls Grok/xAI directly.
//! It wraps the RustCode OpenAI-compatible `/v1/chat/completions` endpoint
//! and falls back to the real Grok API transparently when:
//!
//!   - RustCode is unreachable (connection refused, timeout, DNS failure)
//!   - RustCode returns a 5xx error
//!   - The configured `ra_timeout` is exceeded
//!
//! The caller never needs to know which backend answered.
//!
//! # Quick start
//!
//! ```rust,ignore
//! use proxy_client::{ProxyClient, ProxyClientConfig, ChatMessage};
//!
//! let client = ProxyClient::from_env();
//!
//! let reply = client
//!     .chat(vec![
//!         ChatMessage::system("You are a futures trading analyst."),
//!         ChatMessage::user("Analyse the current BTC open interest spike."),
//!     ])
//!     .await?;
//!
//! println!("{}", reply.content);
//! println!("answered by: {}", reply.answered_by);  // "rustcode" | "grok"
//! println!("model: {}",       reply.model_used);
//! println!("tokens: {}",      reply.total_tokens);
//! ```
//!
//! # Environment variables
//!
//! | Variable                  | Default                         | Purpose                                    |
//! |---------------------------|---------------------------------|--------------------------------------------|
//! | `RUSTCODE_BASE_URL`             | `http://localhost:3500`         | RustCode server URL                   |
//! | `RUSTCODE_API_KEY`              | *(empty — open endpoint)*       | Bearer token for RustCode             |
//! | `RUSTCODE_TIMEOUT_SECS`         | `15`                            | Per-request timeout before fallback fires  |
//! | `RUSTCODE_MODEL`                | `auto`                          | Model hint sent to RustCode           |
//! | `RUSTCODE_REPO_ID`              | *(none)*                        | Repo context to inject (optional)          |
//! | `RUSTCODE_FORCE_REMOTE`         | `false`                         | Force Grok even when calling RUSTCODE            |
//! | `XAI_API_KEY`             | *(required for fallback)*       | Grok / xAI API key                         |
//! | `XAI_MODEL`               | `grok-4`                        | Grok model to use in fallback              |
//! | `XAI_BASE_URL`            | `https://api.x.ai/v1`          | xAI API base URL                           |
//! | `PROXY_CLIENT_DISABLE_RA` | `false`                         | Skip RUSTCODE entirely, always use Grok directly |

use std::time::Duration;

use reqwest::Client;
use serde::{Deserialize, Serialize};
use thiserror::Error;

// ============================================================================
// Public error type
// ============================================================================

#[derive(Debug, Error)]
pub enum ProxyClientError {
    #[error("Both RustCode and Grok fallback failed: RUSTCODE={rc}, Grok={grok}")]
    BothFailed { rc: String, grok: String },

    #[error(
        "Grok fallback is not configured (XAI_API_KEY is unset) and RustCode failed: {rc}"
    )]
    FallbackUnconfigured { rc: String },

    #[error("Request serialisation failed: {0}")]
    Serialise(#[from] serde_json::Error),

    #[error("HTTP client error: {0}")]
    Http(#[from] reqwest::Error),
}

// ============================================================================
// Configuration
// ============================================================================

/// Complete configuration for the proxy client.
///
/// Build with [`ProxyClientConfig::from_env`] for the common case, or
/// construct the struct directly for unit-test / programmatic use.
#[derive(Debug, Clone)]
pub struct ProxyClientConfig {
    // ── RustCode ────────────────────────────────────────────────────────
    /// Full base URL of the RustCode server, e.g. `http://10.0.1.5:3500`.
    pub ra_base_url: String,

    /// Bearer token for RustCode (`RUSTCODE_API_KEY`).
    /// Empty string → no `Authorization` header sent (open endpoint).
    pub ra_api_key: String,

    /// Per-request timeout when calling RustCode.
    /// On timeout the fallback fires immediately.
    pub ra_timeout: Duration,

    /// Model hint forwarded in the `model` field.
    /// `"auto"` (default) lets the ModelRouter decide.
    pub ra_model: String,

    /// Optional registered-repo slug or UUID to inject as RAG context.
    pub ra_repo_id: Option<String>,

    /// Force remote (Grok) inside RustCode regardless of task kind.
    pub ra_force_remote: bool,

    /// When `true`, skip RustCode entirely and call Grok directly.
    /// Useful for hot-patching without redeploying.
    pub disable_ra: bool,

    // ── Grok fallback ────────────────────────────────────────────────────────
    /// xAI API key (`XAI_API_KEY`).  `None` → fallback disabled.
    pub xai_api_key: Option<String>,

    /// Grok model name, e.g. `"grok-4"`.
    pub xai_model: String,

    /// xAI API base URL.
    pub xai_base_url: String,

    /// Per-request timeout when calling Grok directly.
    pub xai_timeout: Duration,
}

impl Default for ProxyClientConfig {
    fn default() -> Self {
        Self {
            ra_base_url: "http://localhost:3500".to_string(),
            ra_api_key: String::new(),
            ra_timeout: Duration::from_secs(15),
            ra_model: "auto".to_string(),
            ra_repo_id: None,
            ra_force_remote: false,
            disable_ra: false,
            xai_api_key: None,
            xai_model: "grok-4".to_string(),
            xai_base_url: "https://api.x.ai/v1".to_string(),
            xai_timeout: Duration::from_secs(60),
        }
    }
}

impl ProxyClientConfig {
    /// Read configuration from environment variables.
    ///
    /// Falls back to sensible defaults for every variable that is not set.
    pub fn from_env() -> Self {
        let ra_timeout = std::env::var("RUSTCODE_TIMEOUT_SECS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(15);

        let xai_timeout = std::env::var("XAI_TIMEOUT_SECS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(60);

        let disable_ra = std::env::var("PROXY_CLIENT_DISABLE_RA")
            .map(|v| v.eq_ignore_ascii_case("true") || v == "1")
            .unwrap_or(false);

        let ra_force_remote = std::env::var("RUSTCODE_FORCE_REMOTE")
            .map(|v| v.eq_ignore_ascii_case("true") || v == "1")
            .unwrap_or(false);

        Self {
            ra_base_url: std::env::var("RUSTCODE_BASE_URL")
                .unwrap_or_else(|_| "http://localhost:3500".to_string()),
            ra_api_key: std::env::var("RUSTCODE_API_KEY").unwrap_or_default(),
            ra_timeout: Duration::from_secs(ra_timeout),
            ra_model: std::env::var("RUSTCODE_MODEL").unwrap_or_else(|_| "auto".to_string()),
            ra_repo_id: std::env::var("RUSTCODE_REPO_ID").ok().filter(|v| !v.is_empty()),
            ra_force_remote,
            disable_ra,
            xai_api_key: std::env::var("XAI_API_KEY").ok().filter(|v| !v.is_empty()),
            xai_model: std::env::var("XAI_MODEL").unwrap_or_else(|_| "grok-4".to_string()),
            xai_base_url: std::env::var("XAI_BASE_URL")
                .unwrap_or_else(|_| "https://api.x.ai/v1".to_string()),
            xai_timeout: Duration::from_secs(xai_timeout),
        }
    }
}

// ============================================================================
// Message helpers
// ============================================================================

/// A single chat message — mirrors the OpenAI message shape.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
}

impl ChatMessage {
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: "system".to_string(),
            content: content.into(),
        }
    }

    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: "user".to_string(),
            content: content.into(),
        }
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: "assistant".to_string(),
            content: content.into(),
        }
    }
}

// ============================================================================
// Chat request builder
// ============================================================================

/// Fluent builder for a single chat request.
///
/// ```rust,ignore
/// let reply = client
///     .request()
///     .system("You are a futures trading analyst.")
///     .user("Is BTC in a squeeze right now?")
///     .model("remote")          // override: always use Grok
///     .repo_id("futures-bot")   // inject repo RAG context
///     .temperature(0.1)
///     .no_cache(true)
///     .send()
///     .await?;
/// ```
pub struct ChatRequestBuilder<'c> {
    client: &'c ProxyClient,
    messages: Vec<ChatMessage>,
    model: Option<String>,
    temperature: Option<f32>,
    max_tokens: Option<u32>,
    repo_id: Option<String>,
    no_cache: bool,
    force_remote: bool,
}

impl<'c> ChatRequestBuilder<'c> {
    fn new(client: &'c ProxyClient) -> Self {
        Self {
            client,
            messages: Vec::new(),
            model: None,
            temperature: None,
            max_tokens: None,
            repo_id: None,
            no_cache: false,
            force_remote: false,
        }
    }

    /// Add a system message.
    pub fn system(mut self, content: impl Into<String>) -> Self {
        self.messages.push(ChatMessage::system(content));
        self
    }

    /// Add a user message.
    pub fn user(mut self, content: impl Into<String>) -> Self {
        self.messages.push(ChatMessage::user(content));
        self
    }

    /// Add an assistant message (for multi-turn history).
    pub fn assistant(mut self, content: impl Into<String>) -> Self {
        self.messages.push(ChatMessage::assistant(content));
        self
    }

    /// Append a pre-built message.
    pub fn message(mut self, msg: ChatMessage) -> Self {
        self.messages.push(msg);
        self
    }

    /// Append a slice of pre-built messages (e.g. conversation history).
    pub fn messages(mut self, msgs: impl IntoIterator<Item = ChatMessage>) -> Self {
        self.messages.extend(msgs);
        self
    }

    /// Override the model hint for this request.
    pub fn model(mut self, m: impl Into<String>) -> Self {
        self.model = Some(m.into());
        self
    }

    /// Sampling temperature (0.0–2.0).
    pub fn temperature(mut self, t: f32) -> Self {
        self.temperature = Some(t);
        self
    }

    /// Maximum tokens to generate.
    pub fn max_tokens(mut self, n: u32) -> Self {
        self.max_tokens = Some(n);
        self
    }

    /// Inject RAG context from the named registered repo.
    pub fn repo_id(mut self, id: impl Into<String>) -> Self {
        self.repo_id = Some(id.into());
        self
    }

    /// Bypass the Redis/LRU cache for this request.
    pub fn no_cache(mut self, v: bool) -> Self {
        self.no_cache = v;
        self
    }

    /// Force Grok inside RustCode for this request.
    pub fn force_remote(mut self, v: bool) -> Self {
        self.force_remote = v;
        self
    }

    /// Execute the request.
    pub async fn send(self) -> Result<ChatReply, ProxyClientError> {
        self.client
            .chat_inner(
                self.messages,
                self.model,
                self.temperature,
                self.max_tokens,
                self.repo_id,
                self.no_cache,
                self.force_remote,
            )
            .await
    }
}

// ============================================================================
// Response type
// ============================================================================

/// The reply returned to the caller regardless of which backend answered.
#[derive(Debug, Clone)]
pub struct ChatReply {
    /// The assistant's text response.
    pub content: String,

    /// Which backend actually answered: `"rustcode"` or `"grok"`.
    pub answered_by: String,

    /// The specific model name returned by the backend.
    pub model_used: String,

    /// Total tokens consumed (prompt + completion).
    pub total_tokens: u32,

    /// Prompt tokens (approximate when RUSTCODE answered).
    pub prompt_tokens: u32,

    /// Completion tokens (approximate when RUSTCODE answered).
    pub completion_tokens: u32,

    /// True when the local Ollama model was tried but fell back to Grok
    /// inside RustCode.
    pub used_internal_fallback: bool,

    /// True when the response was served from RustCode's cache.
    pub cached: bool,

    /// The `TaskKind` RustCode assigned (empty string when Grok answered
    /// directly).
    pub task_kind: String,

    /// Number of RAG chunks injected into the prompt (0 when Grok answered
    /// directly).
    pub rag_chunks_used: usize,
}

// ============================================================================
// Internal wire types — OpenAI-compatible shapes
// ============================================================================

#[derive(Serialize)]
struct OaiRequest {
    model: String,
    messages: Vec<ChatMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,
    stream: bool,
    // RustCode extensions
    #[serde(skip_serializing_if = "Option::is_none")]
    x_repo_id: Option<String>,
    x_no_cache: bool,
    x_force_remote: bool,
}

#[derive(Deserialize)]
struct OaiResponse {
    model: String,
    choices: Vec<OaiChoice>,
    usage: Option<OaiUsage>,
    x_ra_metadata: Option<RaMetadata>,
}

#[derive(Deserialize)]
struct OaiChoice {
    message: OaiChoiceMessage,
}

#[derive(Deserialize)]
struct OaiChoiceMessage {
    content: String,
}

#[derive(Deserialize, Default)]
struct OaiUsage {
    prompt_tokens: u32,
    completion_tokens: u32,
    total_tokens: u32,
}

#[derive(Deserialize)]
struct RaMetadata {
    task_kind: String,
    used_fallback: bool,
    rag_chunks_used: usize,
    cached: bool,
}

// ============================================================================
// Main client
// ============================================================================

/// RustCode proxy client with transparent Grok fallback.
///
/// Create once and reuse (the underlying `reqwest::Client` connection-pools
/// underneath).
///
/// ```rust,ignore
/// // Simplest possible usage — reads all config from env vars.
/// let client = ProxyClient::from_env();
/// let reply  = client.chat(vec![ChatMessage::user("Hello!")]).await?;
/// ```
#[derive(Clone)]
pub struct ProxyClient {
    config: ProxyClientConfig,
    http: Client,
}

impl ProxyClient {
    // ── Constructors ──────────────────────────────────────────────────────────

    /// Build from explicit config.
    pub fn new(config: ProxyClientConfig) -> Self {
        // Use a single HTTP client for both backends; timeouts are set per-request.
        let http = Client::builder()
            .timeout(Duration::from_secs(120)) // hard ceiling; per-request timeouts override
            .user_agent("rustcode-proxy-client/1.0")
            .build()
            .expect("Failed to build HTTP client");

        Self { config, http }
    }

    /// Build from environment variables (most common path).
    pub fn from_env() -> Self {
        Self::new(ProxyClientConfig::from_env())
    }

    /// Build from environment variables but override the RUSTCODE base URL.
    /// Convenient for tests or when you know the URL at compile time.
    pub fn with_base_url(base_url: impl Into<String>) -> Self {
        let mut cfg = ProxyClientConfig::from_env();
        cfg.ra_base_url = base_url.into();
        Self::new(cfg)
    }

    // ── Shortcut — single-turn ────────────────────────────────────────────────

    /// Send a list of messages and return the reply.
    /// Uses the defaults from the client config.
    ///
    /// For more control (temperature, repo context, cache bypass) use
    /// [`ProxyClient::request`] instead.
    pub async fn chat(&self, messages: Vec<ChatMessage>) -> Result<ChatReply, ProxyClientError> {
        self.chat_inner(messages, None, None, None, None, false, false)
            .await
    }

    /// Ask a single question with no conversation history.
    pub async fn ask(&self, question: impl Into<String>) -> Result<ChatReply, ProxyClientError> {
        self.chat(vec![ChatMessage::user(question)]).await
    }

    /// Ask with an explicit system prompt and a user question.
    pub async fn ask_with_system(
        &self,
        system: impl Into<String>,
        question: impl Into<String>,
    ) -> Result<ChatReply, ProxyClientError> {
        self.chat(vec![
            ChatMessage::system(system),
            ChatMessage::user(question),
        ])
        .await
    }

    // ── Fluent builder ────────────────────────────────────────────────────────

    /// Start building a chat request with full control over all parameters.
    pub fn request(&self) -> ChatRequestBuilder<'_> {
        ChatRequestBuilder::new(self)
    }

    // ── Availability check ────────────────────────────────────────────────────

    /// Probe RustCode with a `GET /health` request.
    ///
    /// Returns `true` if the server is reachable and healthy.
    /// Does **not** count against rate limits.
    pub async fn is_ra_available(&self) -> bool {
        if self.config.disable_ra {
            return false;
        }

        let url = format!("{}/health", self.config.ra_base_url.trim_end_matches('/'));

        match self
            .http
            .get(&url)
            .timeout(Duration::from_secs(3))
            .send()
            .await
        {
            Ok(resp) => resp.status().is_success(),
            Err(_) => false,
        }
    }

    // ── Core implementation ───────────────────────────────────────────────────

    #[allow(clippy::too_many_arguments)]
    async fn chat_inner(
        &self,
        messages: Vec<ChatMessage>,
        model_override: Option<String>,
        temperature: Option<f32>,
        max_tokens: Option<u32>,
        repo_id_override: Option<String>,
        no_cache: bool,
        force_remote: bool,
    ) -> Result<ChatReply, ProxyClientError> {
        // When RUSTCODE is disabled, skip straight to Grok.
        if !self.config.disable_ra {
            match self
                .try_rustcode(
                    &messages,
                    model_override.as_deref(),
                    temperature,
                    max_tokens,
                    repo_id_override.as_deref(),
                    no_cache,
                    force_remote,
                )
                .await
            {
                Ok(reply) => return Ok(reply),
                Err(ra_err) => {
                    // Log the RUSTCODE failure and fall through to Grok.
                    eprintln!(
                        "[proxy_client] RustCode unavailable or errored — \
                         falling back to Grok. RUSTCODE error: {}",
                        ra_err
                    );

                    return self
                        .try_grok(&messages, temperature, max_tokens, ra_err)
                        .await;
                }
            }
        }

        // RUSTCODE disabled — call Grok directly.
        self.try_grok(
            &messages,
            temperature,
            max_tokens,
            "RUSTCODE disabled".to_string(),
        )
        .await
    }

    // ── RustCode call ────────────────────────────────────────────────────

    #[allow(clippy::too_many_arguments)]
    async fn try_rustcode(
        &self,
        messages: &[ChatMessage],
        model_override: Option<&str>,
        temperature: Option<f32>,
        max_tokens: Option<u32>,
        repo_id: Option<&str>,
        no_cache: bool,
        force_remote: bool,
    ) -> Result<ChatReply, String> {
        let url = format!(
            "{}/v1/chat/completions",
            self.config.ra_base_url.trim_end_matches('/')
        );

        let body = OaiRequest {
            model: model_override.unwrap_or(&self.config.ra_model).to_string(),
            messages: messages.to_vec(),
            temperature,
            max_tokens,
            stream: false,
            x_repo_id: repo_id
                .map(str::to_owned)
                .or_else(|| self.config.ra_repo_id.clone()),
            x_no_cache: no_cache,
            x_force_remote: force_remote || self.config.ra_force_remote,
        };

        let mut req = self
            .http
            .post(&url)
            .timeout(self.config.ra_timeout)
            .header("Content-Type", "application/json");

        if !self.config.ra_api_key.is_empty() {
            req = req.header(
                "Authorization",
                format!("Bearer {}", self.config.ra_api_key),
            );
        }

        let http_resp = req
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("HTTP error: {}", e))?;

        let status = http_resp.status();

        // Treat 5xx as a fallback trigger; surface 4xx as hard errors.
        if status.is_server_error() {
            let text = http_resp
                .text()
                .await
                .unwrap_or_else(|_| "<unreadable body>".to_string());
            return Err(format!("RUSTCODE server error {}: {}", status, text));
        }

        if !status.is_success() {
            let text = http_resp
                .text()
                .await
                .unwrap_or_else(|_| "<unreadable body>".to_string());
            return Err(format!("RUSTCODE client error {}: {}", status, text));
        }

        let oai: OaiResponse = http_resp
            .json()
            .await
            .map_err(|e| format!("Failed to parse RUSTCODE response: {}", e))?;

        let content = oai
            .choices
            .into_iter()
            .next()
            .map(|c| c.message.content)
            .unwrap_or_default();

        let usage = oai.usage.unwrap_or_default();
        let meta = oai.x_ra_metadata;

        Ok(ChatReply {
            content,
            answered_by: "rustcode".to_string(),
            model_used: oai.model,
            total_tokens: usage.total_tokens,
            prompt_tokens: usage.prompt_tokens,
            completion_tokens: usage.completion_tokens,
            used_internal_fallback: meta.as_ref().is_some_and(|m| m.used_fallback),
            cached: meta.as_ref().is_some_and(|m| m.cached),
            task_kind: meta
                .as_ref()
                .map_or_else(String::new, |m| m.task_kind.clone()),
            rag_chunks_used: meta.as_ref().map_or(0, |m| m.rag_chunks_used),
        })
    }

    // ── Grok fallback call ────────────────────────────────────────────────────

    async fn try_grok(
        &self,
        messages: &[ChatMessage],
        temperature: Option<f32>,
        max_tokens: Option<u32>,
        ra_err: impl Into<String>,
    ) -> Result<ChatReply, ProxyClientError> {
        let ra_err_str = ra_err.into();

        let api_key = match &self.config.xai_api_key {
            Some(k) => k.clone(),
            None => {
                return Err(ProxyClientError::FallbackUnconfigured { rc: ra_err_str });
            }
        };

        let url = format!(
            "{}/chat/completions",
            self.config.xai_base_url.trim_end_matches('/')
        );

        #[derive(Serialize)]
        struct GrokRequest<'a> {
            model: &'a str,
            messages: &'a [ChatMessage],
            #[serde(skip_serializing_if = "Option::is_none")]
            temperature: Option<f32>,
            #[serde(skip_serializing_if = "Option::is_none")]
            max_tokens: Option<u32>,
            stream: bool,
        }

        let body = GrokRequest {
            model: &self.config.xai_model,
            messages,
            temperature,
            max_tokens,
            stream: false,
        };

        let http_resp = self
            .http
            .post(&url)
            .timeout(self.config.xai_timeout)
            .header("Content-Type", "application/json")
            .header("Authorization", format!("Bearer {}", api_key))
            .json(&body)
            .send()
            .await
            .map_err(|e| ProxyClientError::BothFailed {
                rc: ra_err_str.clone(),
                grok: format!("HTTP error: {}", e),
            })?;

        if !http_resp.status().is_success() {
            let status = http_resp.status();
            let text = http_resp
                .text()
                .await
                .unwrap_or_else(|_| "<unreadable body>".to_string());
            return Err(ProxyClientError::BothFailed {
                rc: ra_err_str,
                grok: format!("Grok error {}: {}", status, text),
            });
        }

        let oai: OaiResponse =
            http_resp
                .json()
                .await
                .map_err(|e| ProxyClientError::BothFailed {
                    rc: ra_err_str.clone(),
                    grok: format!("Failed to parse Grok response: {}", e),
                })?;

        let content = oai
            .choices
            .into_iter()
            .next()
            .map(|c| c.message.content)
            .unwrap_or_default();

        let usage = oai.usage.unwrap_or_default();

        Ok(ChatReply {
            content,
            answered_by: "grok".to_string(),
            model_used: oai.model,
            total_tokens: usage.total_tokens,
            prompt_tokens: usage.prompt_tokens,
            completion_tokens: usage.completion_tokens,
            used_internal_fallback: false,
            cached: false,
            task_kind: String::new(),
            rag_chunks_used: 0,
        })
    }
}

// ============================================================================
// Convenience: check `answered_by` on the reply
// ============================================================================

impl ChatReply {
    /// True when RustCode (local Ollama or internal Grok) answered.
    pub fn from_rustcode(&self) -> bool {
        self.answered_by == "rustcode"
    }

    /// True when the upstream Grok API answered (RUSTCODE was unavailable).
    pub fn from_grok_fallback(&self) -> bool {
        self.answered_by == "grok"
    }

    /// Cost estimate in USD based on a simple token heuristic.
    /// This is only meaningful when [`ChatReply::from_grok_fallback`] is true;
    /// when RUSTCODE answered with the local model the cost is effectively $0.
    pub fn estimated_cost_usd(&self) -> f64 {
        if self.from_rustcode() && !self.used_internal_fallback {
            // Local Ollama — no API cost.
            return 0.0;
        }
        // Approximate xAI grok-4 pricing (update as pricing changes).
        const COST_PER_M_INPUT: f64 = 2.00;
        const COST_PER_M_OUTPUT: f64 = 10.00;
        let input_cost = (self.prompt_tokens as f64 / 1_000_000.0) * COST_PER_M_INPUT;
        let output_cost = (self.completion_tokens as f64 / 1_000_000.0) * COST_PER_M_OUTPUT;
        input_cost + output_cost
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Env-var tests mutate the process-global environment.  Rust runs tests
    /// in parallel threads, so two tests that call `set_var`/`remove_var` on
    /// the same keys will race.  Serialise all such tests with this mutex.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn default_config() -> ProxyClientConfig {
        ProxyClientConfig {
            ra_base_url: "http://localhost:3500".to_string(),
            ra_api_key: "test-key".to_string(),
            ra_timeout: Duration::from_secs(5),
            ra_model: "auto".to_string(),
            ra_repo_id: None,
            ra_force_remote: false,
            disable_ra: false,
            xai_api_key: Some("grok-key".to_string()),
            xai_model: "grok-4".to_string(),
            xai_base_url: "https://api.x.ai/v1".to_string(),
            xai_timeout: Duration::from_secs(30),
        }
    }

    #[test]
    fn config_from_env_uses_defaults() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        // Clear relevant env vars so we get defaults.
        // SAFETY: This test is single-threaded and guarded by ENV_LOCK.
        unsafe {
            std::env::remove_var("RUSTCODE_BASE_URL");
            std::env::remove_var("RUSTCODE_TIMEOUT_SECS");
            std::env::remove_var("RUSTCODE_MODEL");
            std::env::remove_var("PROXY_CLIENT_DISABLE_RA");
        }

        let cfg = ProxyClientConfig::from_env();
        assert_eq!(cfg.ra_base_url, "http://localhost:3500");
        assert_eq!(cfg.ra_timeout, Duration::from_secs(15));
        assert_eq!(cfg.ra_model, "auto");
        assert!(!cfg.disable_ra);
    }

    #[test]
    fn config_from_env_reads_overrides() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        // SAFETY: This test is single-threaded and guarded by ENV_LOCK.
        unsafe {
            std::env::set_var("RUSTCODE_BASE_URL", "http://oryx:3500");
            std::env::set_var("RUSTCODE_TIMEOUT_SECS", "30");
            std::env::set_var("RUSTCODE_MODEL", "remote");
            std::env::set_var("PROXY_CLIENT_DISABLE_RA", "true");
        }

        let cfg = ProxyClientConfig::from_env();
        assert_eq!(cfg.ra_base_url, "http://oryx:3500");
        assert_eq!(cfg.ra_timeout, Duration::from_secs(30));
        assert_eq!(cfg.ra_model, "remote");
        assert!(cfg.disable_ra);

        // Clean up — always runs because _guard is still in scope.
        // SAFETY: This test is single-threaded and guarded by ENV_LOCK.
        unsafe {
            std::env::remove_var("RUSTCODE_BASE_URL");
            std::env::remove_var("RUSTCODE_TIMEOUT_SECS");
            std::env::remove_var("RUSTCODE_MODEL");
            std::env::remove_var("PROXY_CLIENT_DISABLE_RA");
        }
    }

    #[test]
    fn chat_message_constructors() {
        let s = ChatMessage::system("Be helpful.");
        assert_eq!(s.role, "system");
        assert_eq!(s.content, "Be helpful.");

        let u = ChatMessage::user("Hello?");
        assert_eq!(u.role, "user");

        let a = ChatMessage::assistant("Hi there!");
        assert_eq!(a.role, "assistant");
    }

    #[test]
    fn chat_reply_cost_estimate_zero_for_local() {
        let reply = ChatReply {
            content: "ok".to_string(),
            answered_by: "rustcode".to_string(),
            model_used: "qwen2.5-coder:7b".to_string(),
            total_tokens: 500,
            prompt_tokens: 400,
            completion_tokens: 100,
            used_internal_fallback: false,
            cached: false,
            task_kind: "ScaffoldStub".to_string(),
            rag_chunks_used: 2,
        };
        assert_eq!(reply.estimated_cost_usd(), 0.0);
        assert!(reply.from_rustcode());
        assert!(!reply.from_grok_fallback());
    }

    #[test]
    fn chat_reply_cost_estimate_nonzero_for_grok_fallback() {
        let reply = ChatReply {
            content: "BTC is in a bull trend.".to_string(),
            answered_by: "grok".to_string(),
            model_used: "grok-4".to_string(),
            total_tokens: 1000,
            prompt_tokens: 800,
            completion_tokens: 200,
            used_internal_fallback: false,
            cached: false,
            task_kind: String::new(),
            rag_chunks_used: 0,
        };
        assert!(reply.estimated_cost_usd() > 0.0);
        assert!(!reply.from_rustcode());
        assert!(reply.from_grok_fallback());
    }

    #[test]
    fn chat_reply_cost_estimate_nonzero_for_internal_fallback() {
        // RUSTCODE answered but routed internally to Grok — still has a cost.
        let reply = ChatReply {
            content: "Analysis complete.".to_string(),
            answered_by: "rustcode".to_string(),
            model_used: "grok-4".to_string(),
            total_tokens: 600,
            prompt_tokens: 500,
            completion_tokens: 100,
            used_internal_fallback: true,
            cached: false,
            task_kind: "ArchitecturalReason".to_string(),
            rag_chunks_used: 1,
        };
        assert!(reply.estimated_cost_usd() > 0.0);
    }

    #[test]
    fn request_builder_accumulates_messages() {
        let client = ProxyClient::new(default_config());
        let builder = client
            .request()
            .system("You are a futures analyst.")
            .user("What is the BTC funding rate?")
            .assistant("The funding rate is currently 0.01%.")
            .user("Is that bullish?");

        assert_eq!(builder.messages.len(), 4);
        assert_eq!(builder.messages[0].role, "system");
        assert_eq!(builder.messages[3].role, "user");
        assert_eq!(builder.messages[3].content, "Is that bullish?");
    }

    #[test]
    fn request_builder_overrides_apply() {
        let client = ProxyClient::new(default_config());
        let builder = client
            .request()
            .user("Hello")
            .model("remote")
            .temperature(0.1)
            .max_tokens(512)
            .repo_id("futures-bot")
            .no_cache(true)
            .force_remote(true);

        assert_eq!(builder.model.as_deref(), Some("remote"));
        assert_eq!(builder.temperature, Some(0.1));
        assert_eq!(builder.max_tokens, Some(512));
        assert_eq!(builder.repo_id.as_deref(), Some("futures-bot"));
        assert!(builder.no_cache);
        assert!(builder.force_remote);
    }

    #[tokio::test]
    async fn is_ra_available_returns_false_for_unreachable_host() {
        let mut cfg = default_config();
        cfg.ra_base_url = "http://127.0.0.1:19999".to_string(); // nothing listening here
        cfg.ra_timeout = Duration::from_millis(200);
        let client = ProxyClient::new(cfg);
        assert!(!client.is_ra_available().await);
    }

    #[tokio::test]
    async fn is_ra_available_returns_false_when_disabled() {
        let mut cfg = default_config();
        cfg.disable_ra = true;
        let client = ProxyClient::new(cfg);
        assert!(!client.is_ra_available().await);
    }

    #[tokio::test]
    async fn chat_falls_back_when_ra_unreachable_and_grok_key_absent() {
        let mut cfg = default_config();
        cfg.ra_base_url = "http://127.0.0.1:19999".to_string();
        cfg.ra_timeout = Duration::from_millis(200);
        cfg.xai_api_key = None; // no fallback key either

        let client = ProxyClient::new(cfg);
        let result = client.ask("test").await;

        match result {
            Err(ProxyClientError::FallbackUnconfigured { .. }) => {} // expected
            other => panic!("Expected FallbackUnconfigured, got {:?}", other),
        }
    }
}
