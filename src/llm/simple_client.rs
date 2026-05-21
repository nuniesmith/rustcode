// Simple Grok Client Adapter
//
// Wraps the `api` crate's `OpenAiCompatClient` so the research and backup
// systems have a stable, high-level interface while all HTTP/retry/auth
// logic lives in the shared `api` crate.
//
// RC-CLEANUP-A (2026-05-21): consolidates the migrated top-level
// `src/simple_client.rs` orphan into `src/llm/`, replacing the previous
// raw-reqwest implementation that this module used to export.

use anyhow::{Context as _, Result};
use api::{InputMessage, MessageRequest, OpenAiCompatClient, OpenAiCompatConfig, OutputContentBlock};

// Default model when `XAI_MODEL` env-var is unset. Preserves the value the
// raw-reqwest predecessor shipped so existing deployments see no change.
const DEFAULT_MODEL: &str = "grok-4.1";

// Simple Grok client for the research and backup systems.
//
// Backed by [`api::OpenAiCompatClient`] — no manual HTTP required.
#[derive(Clone)]
pub struct GrokClient {
    inner: OpenAiCompatClient,
    model: String,
}

impl GrokClient {
    // Create a new client with an explicit API key.
    #[must_use]
    pub fn new(api_key: String) -> Self {
        let model = std::env::var("XAI_MODEL").unwrap_or_else(|_| DEFAULT_MODEL.to_string());
        let inner = OpenAiCompatClient::new(api_key, OpenAiCompatConfig::xai());
        Self { inner, model }
    }

    // Create from `XAI_API_KEY` environment variable.
    pub fn from_env() -> Result<Self> {
        let model = std::env::var("XAI_MODEL").unwrap_or_else(|_| DEFAULT_MODEL.to_string());
        let inner = OpenAiCompatClient::from_env(OpenAiCompatConfig::xai())
            .context("Failed to initialise xAI client from env")?;
        Ok(Self { inner, model })
    }

    // Override the model for this client.
    #[must_use]
    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = model.into();
        self
    }

    // The active model string.
    #[must_use]
    pub fn model(&self) -> &str {
        &self.model
    }

    // Generate a completion. Returns the assistant text content.
    pub async fn generate(&self, prompt: &str, max_tokens: usize) -> Result<String> {
        let request = MessageRequest {
            model: self.model.clone(),
            max_tokens: u32::try_from(max_tokens).unwrap_or(u32::MAX),
            messages: vec![InputMessage::user_text(prompt)],
            system: None,
            tools: None,
            tool_choice: None,
            temperature: None,
            response_format: None,
            stream: false,
        };

        let response = self
            .inner
            .send_message(&request)
            .await
            .context("Grok API request failed")?;

        let text = response
            .content
            .iter()
            .filter_map(|block| match block {
                OutputContentBlock::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("");

        Ok(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_to_grok_4_1_when_env_unset() {
        // SAFETY: tests run single-threaded for env mutation, and the
        // workspace test runner serializes env access via runtime helpers
        // elsewhere; this test only reads.
        let prior = std::env::var("XAI_MODEL").ok();
        // Best-effort: skip the assertion if another test has set XAI_MODEL.
        if prior.is_none() {
            let client = GrokClient::new("test-key".to_string());
            assert_eq!(client.model(), DEFAULT_MODEL);
        }
    }

    #[test]
    fn with_model_overrides_default() {
        let client = GrokClient::new("test-key".to_string()).with_model("grok-beta");
        assert_eq!(client.model(), "grok-beta");
    }
}
