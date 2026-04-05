// Simple Grok Client Adapter
//
// Wraps the `api` crate's `OpenAiCompatClient` so the rest of the rustcode
// codebase has a stable, high-level interface while all HTTP/retry/auth logic
// lives in the shared `api` crate.  This eliminates the raw `reqwest` calls
// that previously lived here.

use anyhow::{Context as _, Result};
use api::{InputMessage, MessageRequest, OpenAiCompatClient, OpenAiCompatConfig, OutputContentBlock};

// Default model when `XAI_MODEL` env-var is unset.
const DEFAULT_MODEL: &str = "grok-4.20-multi-agent-0309";

// Default max-tokens for simple completions.
const DEFAULT_MAX_TOKENS: u32 = 8_000;

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
    pub fn model(&self) -> &str {
        &self.model
    }

    // Generate a completion. Returns the assistant text content.
    pub async fn generate(&self, prompt: &str, max_tokens: usize) -> Result<String> {
        let request = MessageRequest {
            model:       self.model.clone(),
            max_tokens:  u32::try_from(max_tokens).unwrap_or(DEFAULT_MAX_TOKENS),
            messages:    vec![InputMessage::user_text(prompt)],
            system:      None,
            tools:       None,
            tool_choice: None,
            stream:      false,
        };

        let response = self
            .inner
            .send_message(&request)
            .await
            .context("Grok API request failed")?;

        let text = response
            .content
            .iter()
            .filter_map(|block| {
                if let OutputContentBlock::Text { text } = block {
                    Some(text.as_str())
                } else {
                    None
                }
            })
            .collect::<Vec<_>>()
            .join("");

        Ok(text)
    }

    // Convenience: generate with the default token limit.
    pub async fn complete(&self, prompt: &str) -> Result<String> {
        self.generate(prompt, DEFAULT_MAX_TOKENS as usize).await
    }

    // Chat with an explicit system prompt.
    pub async fn chat(&self, system: &str, user: &str) -> Result<String> {
        let request = MessageRequest {
            model:       self.model.clone(),
            max_tokens:  DEFAULT_MAX_TOKENS,
            messages:    vec![InputMessage::user_text(user)],
            system:      Some(system.to_string()),
            tools:       None,
            tool_choice: None,
            stream:      false,
        };

        let response = self
            .inner
            .send_message(&request)
            .await
            .context("Grok chat request failed")?;

        let text = response
            .content
            .iter()
            .filter_map(|block| {
                if let OutputContentBlock::Text { text } = block {
                    Some(text.as_str())
                } else {
                    None
                }
            })
            .collect::<Vec<_>>()
            .join("");

        Ok(text)
    }
}
