//! # Ollama Client
//!
//! OpenAI-compatible HTTP client for local Ollama inference.
//!
//! ## Features
//! - `POST /api/chat` against a local Ollama instance
//! - Automatic fallback to `GrokClient` when Ollama is unreachable or unconfigured
//! - Retry with exponential back-off (same pattern as `grok_client.rs`)
//! - Token-count pass-through (Ollama `eval_count` / `prompt_eval_count`)
//! - Shared `CompletionRequest` / `CompletionResponse` types with `model_router`
//!
//! ## Environment variables
//! | Variable | Default | Description |
//! |---|---|---|
//! | `OLLAMA_BASE_URL` | `http://localhost:11434` | Base URL of the Ollama server |
//! | `LOCAL_MODEL` | `qwen2.5-coder:7b` | Model tag to request |
//! | `OLLAMA_TIMEOUT_SECS` | `120` | Per-request timeout |
//! | `OLLAMA_MAX_RETRIES` | `2` | Retry attempts before fallback |

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const DEFAULT_BASE_URL: &str = "http://localhost:11434";
const DEFAULT_MODEL: &str = "qwen2.5-coder:7b";
const DEFAULT_TIMEOUT_SECS: u64 = 180;
const DEFAULT_MAX_RETRIES: usize = 2;
const INITIAL_RETRY_DELAY_MS: u64 = 500;

// ---------------------------------------------------------------------------
// Wire types — Ollama `/api/chat` shape
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
struct OllamaChatRequest {
    model: String,
    messages: Vec<OllamaMessage>,
    stream: bool,
    options: OllamaOptions,
}

/// A single chunk from Ollama's streaming `/api/chat` NDJSON response.
#[derive(Debug, Deserialize)]
struct OllamaStreamChunk {
    message: Option<OllamaMessage>,
    /// `true` on the final chunk (contains token counts, no new content).
    #[serde(default)]
    done: bool,
    /// Tokens in the generated response — present only on the final chunk.
    #[serde(default)]
    eval_count: Option<u32>,
    /// Tokens in the prompt — present only on the final chunk.
    #[serde(default)]
    prompt_eval_count: Option<u32>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct OllamaMessage {
    role: String,
    content: String,
}

#[derive(Debug, Serialize)]
struct OllamaOptions {
    temperature: f32,
    /// Maximum tokens to generate (maps to `num_predict` in Ollama).
    num_predict: u32,
    /// KV-cache / context window size passed to llama.cpp via Ollama.
    /// Ollama defaults to 4096; with 16 GB VRAM and a 7B model we can
    /// safely use 16384 (uses ~896 MiB extra VRAM for KV cache).
    num_ctx: u32,
}

/// Ollama non-streaming response body.
#[derive(Debug, Deserialize)]
struct OllamaChatResponse {
    message: OllamaMessage,
    /// Tokens used to generate the response.
    #[serde(default)]
    eval_count: Option<u32>,
    /// Tokens in the prompt.
    #[serde(default)]
    prompt_eval_count: Option<u32>,
    /// Whether Ollama reports the model finished normally.
    #[serde(default)]
    done: bool,
}

// ---------------------------------------------------------------------------
// Public completion types (shared with `model_router` / `api/repos`)
// ---------------------------------------------------------------------------

/// Outcome of a completion call, regardless of which backend served it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OllamaCompletionResponse {
    pub content: String,
    pub model_used: String,
    /// True when Ollama was down and the Grok fallback was used.
    pub used_fallback: bool,
    pub prompt_tokens: Option<u32>,
    pub completion_tokens: Option<u32>,
}

/// A token/text delta sent through the streaming channel.
#[derive(Debug, Clone)]
pub enum StreamChunk {
    /// A text delta to forward to the client.
    Delta(String),
    /// The stream has finished; carries final token counts (may be `None`
    /// when served from a Grok fallback that doesn't report them).
    Done {
        model_used: String,
        used_fallback: bool,
        prompt_tokens: Option<u32>,
        completion_tokens: Option<u32>,
    },
    /// A fatal error terminated the stream.
    Error(String),
}

// ---------------------------------------------------------------------------
// Client config
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct OllamaClientConfig {
    pub base_url: String,
    pub model: String,
    pub timeout: Duration,
    pub max_retries: usize,
}

impl Default for OllamaClientConfig {
    fn default() -> Self {
        Self {
            base_url: std::env::var("OLLAMA_BASE_URL")
                .unwrap_or_else(|_| DEFAULT_BASE_URL.to_string()),
            model: std::env::var("LOCAL_MODEL").unwrap_or_else(|_| DEFAULT_MODEL.to_string()),
            timeout: Duration::from_secs(
                std::env::var("OLLAMA_TIMEOUT_SECS")
                    .ok()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(DEFAULT_TIMEOUT_SECS),
            ),
            max_retries: std::env::var("OLLAMA_MAX_RETRIES")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(DEFAULT_MAX_RETRIES),
        }
    }
}

// ---------------------------------------------------------------------------
// OllamaClient
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct OllamaClient {
    config: OllamaClientConfig,
    http: reqwest::Client,
    /// Optional GrokClient used when Ollama is unreachable.
    fallback: Option<Arc<crate::grok_client::GrokClient>>,
}

impl OllamaClient {
    /// Build a client from environment variables using default config.
    pub fn from_env() -> Self {
        Self::new(OllamaClientConfig::default(), None)
    }

    /// Build a client with an explicit config and optional Grok fallback.
    pub fn new(
        config: OllamaClientConfig,
        fallback: Option<Arc<crate::grok_client::GrokClient>>,
    ) -> Self {
        let http = reqwest::Client::builder()
            .timeout(config.timeout)
            .build()
            .expect("Failed to build Ollama HTTP client");

        Self {
            config,
            http,
            fallback,
        }
    }

    /// Attach a Grok fallback after construction.
    pub fn with_fallback(mut self, client: Arc<crate::grok_client::GrokClient>) -> Self {
        self.fallback = Some(client);
        self
    }

    // -----------------------------------------------------------------------
    // Public API
    // -----------------------------------------------------------------------

    /// Send a streaming completion request.
    ///
    /// Returns a `tokio::sync::mpsc::Receiver` that yields [`StreamChunk`]
    /// values as the model generates tokens.  The final message is always
    /// `StreamChunk::Done` (on success) or `StreamChunk::Error` (on failure).
    ///
    /// Falls back to the blocking [`complete`] path (then re-emits the full
    /// response as a single delta) when Ollama streaming fails or a Grok
    /// fallback is active.
    pub async fn complete_streaming(
        &self,
        system_prompt: Option<&str>,
        user_prompt: &str,
        temperature: f32,
        max_tokens: u32,
    ) -> mpsc::Receiver<StreamChunk> {
        self.complete_streaming_with_ctx(system_prompt, user_prompt, temperature, max_tokens, 16384)
            .await
    }

    /// Like [`complete_streaming`] but with an explicit context-window size.
    pub async fn complete_streaming_with_ctx(
        &self,
        system_prompt: Option<&str>,
        user_prompt: &str,
        temperature: f32,
        max_tokens: u32,
        num_ctx: u32,
    ) -> mpsc::Receiver<StreamChunk> {
        // Channel with a generous buffer so a slow reader doesn't stall Ollama.
        let (tx, rx) = mpsc::channel::<StreamChunk>(256);

        let url = format!("{}/api/chat", self.config.base_url);
        let model = self.config.model.clone();

        // Build the message list.
        let mut messages: Vec<OllamaMessage> = Vec::new();
        if let Some(sys) = system_prompt {
            messages.push(OllamaMessage {
                role: "system".to_string(),
                content: sys.to_string(),
            });
        }
        messages.push(OllamaMessage {
            role: "user".to_string(),
            content: user_prompt.to_string(),
        });

        let request = OllamaChatRequest {
            model: model.clone(),
            messages,
            stream: true, // request NDJSON streaming
            options: OllamaOptions {
                temperature,
                num_predict: max_tokens,
                num_ctx,
            },
        };

        let http = self.http.clone();
        let fallback = self.fallback.clone();
        let system_owned = system_prompt.map(str::to_owned);
        let user_owned = user_prompt.to_owned();

        tokio::spawn(async move {
            // ── Try Ollama streaming ─────────────────────────────────────────
            let http_result = http.post(&url).json(&request).send().await;

            match http_result {
                Err(e) => {
                    warn!(error = %e, "Ollama stream: connection failed");
                    // Try Grok fallback (non-streaming, then emit as one delta).
                    Self::streaming_grok_fallback(
                        fallback.as_deref(),
                        system_owned.as_deref(),
                        &user_owned,
                        &model,
                        tx,
                    )
                    .await;
                }
                Ok(resp) if !resp.status().is_success() => {
                    let status = resp.status();
                    let body = resp.text().await.unwrap_or_default();
                    warn!(status = %status, body = %body, "Ollama stream: non-success status");
                    Self::streaming_grok_fallback(
                        fallback.as_deref(),
                        system_owned.as_deref(),
                        &user_owned,
                        &model,
                        tx,
                    )
                    .await;
                }
                Ok(mut resp) => {
                    // Consume the NDJSON response chunk by chunk.
                    // Each HTTP chunk may contain one or more newline-delimited
                    // JSON objects; we buffer across chunks to handle splits.
                    let mut buf = String::new();
                    let mut prompt_tokens: Option<u32> = None;
                    let mut completion_tokens: Option<u32> = None;

                    'outer: loop {
                        match resp.chunk().await {
                            Err(e) => {
                                warn!(error = %e, "Ollama stream: chunk read error");
                                let _ = tx.send(StreamChunk::Error(e.to_string())).await;
                                return;
                            }
                            Ok(None) => {
                                // HTTP body fully consumed without a `done: true`
                                // chunk — treat as a normal end.
                                break 'outer;
                            }
                            Ok(Some(bytes)) => {
                                buf.push_str(&String::from_utf8_lossy(&bytes));

                                // Ollama sends one JSON object per line.
                                while let Some(newline_pos) = buf.find('\n') {
                                    let line: String = buf.drain(..=newline_pos).collect();
                                    let line = line.trim();
                                    if line.is_empty() {
                                        continue;
                                    }

                                    match serde_json::from_str::<OllamaStreamChunk>(line) {
                                        Err(e) => {
                                            warn!(
                                                error = %e,
                                                raw = %line,
                                                "Ollama stream: failed to parse chunk"
                                            );
                                        }
                                        Ok(chunk) => {
                                            if chunk.done {
                                                prompt_tokens = chunk.prompt_eval_count;
                                                completion_tokens = chunk.eval_count;
                                                break 'outer;
                                            }
                                            if let Some(msg) = chunk.message {
                                                if !msg.content.is_empty()
                                                    && tx
                                                        .send(StreamChunk::Delta(msg.content))
                                                        .await
                                                        .is_err()
                                                {
                                                    // Receiver dropped — client disconnected.
                                                    return;
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }

                    let _ = tx
                        .send(StreamChunk::Done {
                            model_used: model.clone(),
                            used_fallback: false,
                            prompt_tokens,
                            completion_tokens,
                        })
                        .await;
                }
            }
        });

        rx
    }

    /// Emit the entire Grok (or error) response as a single `Delta` followed
    /// by `Done`.  Used when Ollama is unreachable during a streaming call.
    async fn streaming_grok_fallback(
        fallback: Option<&crate::grok_client::GrokClient>,
        system_prompt: Option<&str>,
        user_prompt: &str,
        ollama_model: &str,
        tx: mpsc::Sender<StreamChunk>,
    ) {
        match fallback {
            None => {
                let _ = tx
                    .send(StreamChunk::Error(
                        "Ollama unavailable and no Grok fallback configured.".to_string(),
                    ))
                    .await;
            }
            Some(grok) => {
                let prompt = match system_prompt {
                    Some(sys) => format!("{}\n\n{}", sys, user_prompt),
                    None => user_prompt.to_owned(),
                };
                match grok
                    .ask_tracked(&prompt, None, "ollama_stream_fallback")
                    .await
                {
                    Err(e) => {
                        let _ = tx.send(StreamChunk::Error(e.to_string())).await;
                    }
                    Ok(resp) => {
                        let _ = tx.send(StreamChunk::Delta(resp.content)).await;
                        let _ = tx
                            .send(StreamChunk::Done {
                                model_used: format!("grok-fallback/{}", ollama_model),
                                used_fallback: true,
                                prompt_tokens: Some(resp.prompt_tokens as u32),
                                completion_tokens: Some(resp.completion_tokens as u32),
                            })
                            .await;
                    }
                }
            }
        }
    }

    /// Send a completion request.
    ///
    /// * Tries Ollama up to `config.max_retries` times.
    /// * Falls back to `GrokClient` if all attempts fail and a fallback is set.
    /// * Returns an error only if both Ollama and the fallback are unavailable.
    pub async fn complete(
        &self,
        system_prompt: Option<&str>,
        user_prompt: &str,
        temperature: f32,
        max_tokens: u32,
    ) -> Result<OllamaCompletionResponse> {
        self.complete_with_ctx(system_prompt, user_prompt, temperature, max_tokens, 16384)
            .await
    }

    /// Like [`complete`] but with an explicit context-window size (`num_ctx`).
    pub async fn complete_with_ctx(
        &self,
        system_prompt: Option<&str>,
        user_prompt: &str,
        temperature: f32,
        max_tokens: u32,
        num_ctx: u32,
    ) -> Result<OllamaCompletionResponse> {
        match self
            .try_ollama(system_prompt, user_prompt, temperature, max_tokens, num_ctx)
            .await
        {
            Ok(resp) => Ok(resp),
            Err(ollama_err) => {
                warn!(
                    error = %ollama_err,
                    base_url = %self.config.base_url,
                    "Ollama request failed"
                );

                if let Some(ref grok) = self.fallback {
                    info!("Falling back to GrokClient for completion");
                    self.grok_fallback(grok, system_prompt, user_prompt).await
                } else {
                    Err(ollama_err.context("Ollama failed and no fallback is configured"))
                }
            }
        }
    }

    /// Check whether the Ollama server is reachable by hitting `/api/tags`.
    pub async fn health_check(&self) -> bool {
        let url = format!("{}/api/tags", self.config.base_url);
        self.http
            .get(&url)
            .timeout(Duration::from_secs(5))
            .send()
            .await
            .map(|r| r.status().is_success())
            .unwrap_or(false)
    }

    /// List models available on the connected Ollama instance.
    pub async fn list_models(&self) -> Result<Vec<String>> {
        let url = format!("{}/api/tags", self.config.base_url);

        #[derive(Deserialize)]
        struct TagsResponse {
            models: Vec<ModelEntry>,
        }
        #[derive(Deserialize)]
        struct ModelEntry {
            name: String,
        }

        let resp: TagsResponse = self
            .http
            .get(&url)
            .send()
            .await
            .context("GET /api/tags failed")?
            .json()
            .await
            .context("Failed to parse /api/tags response")?;

        Ok(resp.models.into_iter().map(|m| m.name).collect())
    }

    // -----------------------------------------------------------------------
    // Internals
    // -----------------------------------------------------------------------

    async fn try_ollama(
        &self,
        system_prompt: Option<&str>,
        user_prompt: &str,
        temperature: f32,
        max_tokens: u32,
        num_ctx: u32,
    ) -> Result<OllamaCompletionResponse> {
        let url = format!("{}/api/chat", self.config.base_url);

        let mut messages = Vec::new();
        if let Some(sys) = system_prompt {
            messages.push(OllamaMessage {
                role: "system".to_string(),
                content: sys.to_string(),
            });
        }
        messages.push(OllamaMessage {
            role: "user".to_string(),
            content: user_prompt.to_string(),
        });

        let request = OllamaChatRequest {
            model: self.config.model.clone(),
            messages,
            stream: false,
            options: OllamaOptions {
                temperature,
                num_predict: max_tokens,
                num_ctx,
            },
        };

        let mut last_err: Option<anyhow::Error> = None;

        for attempt in 0..=self.config.max_retries {
            if attempt > 0 {
                let delay =
                    Duration::from_millis(INITIAL_RETRY_DELAY_MS * 2u64.pow(attempt as u32 - 1));
                info!(
                    attempt,
                    max = self.config.max_retries,
                    ?delay,
                    "Retrying Ollama request"
                );
                tokio::time::sleep(delay).await;
            }

            debug!(
                model = %self.config.model,
                url = %url,
                prompt_len = user_prompt.len(),
                "Sending Ollama chat request"
            );

            match self.send_once(&url, &request).await {
                Ok(chat_resp) => {
                    let prompt_tokens = chat_resp.prompt_eval_count;
                    let completion_tokens = chat_resp.eval_count;
                    let total = prompt_tokens.unwrap_or(0) + completion_tokens.unwrap_or(0);

                    info!(
                        model = %self.config.model,
                        total_tokens = total,
                        done = chat_resp.done,
                        "Ollama request succeeded"
                    );

                    return Ok(OllamaCompletionResponse {
                        content: chat_resp.message.content,
                        model_used: self.config.model.clone(),
                        used_fallback: false,
                        prompt_tokens,
                        completion_tokens,
                    });
                }
                Err(e) => {
                    warn!(attempt, error = %e, "Ollama attempt failed");
                    last_err = Some(e);
                }
            }
        }

        Err(last_err.unwrap_or_else(|| anyhow::anyhow!("Ollama: exhausted retries")))
    }

    async fn send_once(
        &self,
        url: &str,
        request: &OllamaChatRequest,
    ) -> Result<OllamaChatResponse> {
        let http_resp = self
            .http
            .post(url)
            .json(request)
            .send()
            .await
            .context("Failed to connect to Ollama")?;

        let status = http_resp.status();
        if !status.is_success() {
            let body = http_resp
                .text()
                .await
                .unwrap_or_else(|_| "<unreadable body>".to_string());
            return Err(anyhow::anyhow!("Ollama returned {}: {}", status, body));
        }

        http_resp
            .json::<OllamaChatResponse>()
            .await
            .context("Failed to parse Ollama response JSON")
    }

    async fn grok_fallback(
        &self,
        grok: &crate::grok_client::GrokClient,
        system_prompt: Option<&str>,
        user_prompt: &str,
    ) -> Result<OllamaCompletionResponse> {
        let prompt = match system_prompt {
            Some(sys) => format!("{}\n\n{}", sys, user_prompt),
            None => user_prompt.to_string(),
        };

        let resp = grok
            .ask_tracked(&prompt, None, "ollama_fallback")
            .await
            .context("GrokClient fallback failed")?;

        info!(
            tokens = resp.total_tokens,
            cost_usd = resp.cost_usd,
            "Grok fallback completed"
        );

        Ok(OllamaCompletionResponse {
            content: resp.content,
            model_used: format!("grok-fallback/{}", grok.model_name()),
            used_fallback: true,
            prompt_tokens: Some(resp.prompt_tokens as u32),
            completion_tokens: Some(resp.completion_tokens as u32),
        })
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_reads_env() {
        // Without env vars, defaults should be stable.
        let config = OllamaClientConfig::default();
        assert!(!config.base_url.is_empty());
        assert!(!config.model.is_empty());
        assert!(config.timeout.as_secs() > 0);
        assert!(config.max_retries <= 10);
    }

    #[test]
    fn client_builds_without_fallback() {
        let client = OllamaClient::from_env();
        assert!(client.fallback.is_none());
    }

    #[test]
    fn request_serialises_correctly() {
        let req = OllamaChatRequest {
            model: "qwen2.5-coder:7b".to_string(),
            messages: vec![
                OllamaMessage {
                    role: "system".to_string(),
                    content: "You are helpful.".to_string(),
                },
                OllamaMessage {
                    role: "user".to_string(),
                    content: "Hello".to_string(),
                },
            ],
            stream: false,
            options: OllamaOptions {
                temperature: 0.2,
                num_predict: 512,
                num_ctx: 16384,
            },
        };

        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("\"stream\":false"));
        assert!(json.contains("\"num_predict\":512"));
        assert!(json.contains("\"num_ctx\":"));
        assert!(json.contains("qwen2.5-coder:7b"));
    }

    #[test]
    fn response_deserialises_partial() {
        // Ollama sometimes omits eval counts on very short responses.
        let raw = r#"{
            "model": "qwen2.5-coder:7b",
            "message": {"role": "assistant", "content": "Hello!"},
            "done": true
        }"#;
        let resp: OllamaChatResponse = serde_json::from_str(raw).unwrap();
        assert_eq!(resp.message.content, "Hello!");
        assert!(resp.eval_count.is_none());
        assert!(resp.done);
    }
}
