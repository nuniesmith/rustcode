//! Simple Grok Client Adapter
//!
//! Provides a simplified interface for the research and backup systems
//! to use the Grok LLM API.

use anyhow::Result;

/// Simple Grok client for research system
#[derive(Clone)]
pub struct GrokClient {
    api_key: String,
    model: String,
    base_url: String,
}

impl GrokClient {
    /// Create a new Grok client
    pub fn new(api_key: String) -> Self {
        Self {
            api_key,
            model: "grok-4.1".to_string(),
            base_url: "https://api.x.ai/v1".to_string(),
        }
    }

    /// Create from environment variables
    pub fn from_env() -> Result<Self> {
        let api_key = std::env::var("XAI_API_KEY")
            .map_err(|_| anyhow::anyhow!("XAI_API_KEY not set in environment"))?;

        let model = std::env::var("XAI_MODEL").unwrap_or_else(|_| "grok-4.1".to_string());

        Ok(Self {
            api_key,
            model,
            base_url: "https://api.x.ai/v1".to_string(),
        })
    }

    /// Generate a completion from Grok
    pub async fn generate(&self, prompt: &str, max_tokens: usize) -> Result<String> {
        let client = reqwest::Client::new();

        let body = serde_json::json!({
            "messages": [
                {
                    "role": "user",
                    "content": prompt
                }
            ],
            "model": self.model,
            "max_tokens": max_tokens,
            "temperature": 0.7,
        });

        let response = client
            .post(format!("{}/chat/completions", self.base_url))
            .header("Content-Type", "application/json")
            .header("Authorization", format!("Bearer {}", self.api_key))
            .json(&body)
            .send()
            .await?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await?;
            return Err(anyhow::anyhow!("Grok API error {}: {}", status, text));
        }

        let json: serde_json::Value = response.json().await?;

        let content = json["choices"][0]["message"]["content"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("No content in response"))?
            .to_string();

        Ok(content)
    }

    /// Set the model to use
    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = model.into();
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_client_creation() {
        let client = GrokClient::new("test-key".to_string());
        assert_eq!(client.model, "grok-4.1");
    }

    #[test]
    fn test_with_model() {
        let client = GrokClient::new("test-key".to_string()).with_model("grok-beta");
        assert_eq!(client.model, "grok-beta");
    }
}
