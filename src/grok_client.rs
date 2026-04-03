//! # Grok Client Module
//!
//! Simplified Grok API client with cost tracking integration for Rustassistant.
//!
//! ## Features
//!
//! - Direct xAI API integration using reqwest
//! - Automatic cost tracking to database
//! - File scoring and analysis
//! - Retry logic with exponential backoff
//! - Response caching support
//!
//! ## Usage
//!
//! ```rust,no_run
//! use rustcode::grok_client::GrokClient;
//! use rustcode::db::Database;
//!
//! #[tokio::main]
//! async fn main() -> anyhow::Result<()> {
//!     let db = Database::new("data/rustcode.db").await?;
//!     let client = GrokClient::new("your-api-key", db);
//!
//!     let result = client.score_file("path/to/file.rs", "fn main() {}").await?;
//!     println!("Score: {}", result.overall_score);
//!
//!     Ok(())
//! }
//! ```

use crate::db::Database;
use crate::response_cache::ResponseCache;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::time::Duration;
use tracing::{debug, error, info, warn};

/// Grok API base URL
const GROK_API_BASE: &str = "https://api.x.ai/v1";

/// Grok model for fast reasoning
const GROK_MODEL: &str = "grok-4-1-fast-reasoning";

/// Pricing per million tokens for Grok 4.1 Fast
const COST_PER_MILLION_INPUT_TOKENS: f64 = 0.20;
const COST_PER_MILLION_OUTPUT_TOKENS: f64 = 0.50;
#[allow(dead_code)]
const COST_PER_MILLION_CACHED_TOKENS: f64 = 0.05;

/// Maximum retries for API calls
const MAX_RETRIES: usize = 3;

/// Initial retry delay in milliseconds
const INITIAL_RETRY_DELAY_MS: u64 = 1000;

/// Grok API client with cost tracking and caching
pub struct GrokClient {
    /// HTTP client
    client: reqwest::Client,
    /// API key
    api_key: String,
    /// Database for cost tracking
    db: Database,
    /// Model to use
    model: String,
    /// Response cache
    cache: Option<ResponseCache>,
    /// Enable caching
    caching_enabled: bool,
}

/// File scoring request
#[derive(Debug, Serialize)]
struct ChatCompletionRequest {
    model: String,
    messages: Vec<Message>,
    temperature: f64,
    max_tokens: usize,
}

/// Chat message
#[derive(Debug, Serialize, Deserialize)]
struct Message {
    role: String,
    content: String,
}

/// API response
#[derive(Debug, Deserialize)]
struct ChatCompletionResponse {
    #[allow(dead_code)]
    id: String,
    choices: Vec<Choice>,
    usage: Usage,
}

/// Response choice
#[derive(Debug, Deserialize)]
struct Choice {
    message: Message,
    #[allow(dead_code)]
    finish_reason: String,
}

/// Token usage information
#[derive(Debug, Clone, Deserialize)]
struct Usage {
    prompt_tokens: i64,
    completion_tokens: i64,
    total_tokens: i64,
}

/// Public response from ask_tracked() with cost/token info
#[derive(Debug, Clone)]
pub struct AskResponse {
    pub content: String,
    pub total_tokens: i64,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub cost_usd: f64,
}

/// File scoring result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileScoreResult {
    /// Overall quality score (0-100)
    pub overall_score: f64,
    /// Security score (0-100)
    pub security_score: f64,
    /// Code quality score (0-100)
    pub quality_score: f64,
    /// Complexity score (0-100, lower is better)
    pub complexity_score: f64,
    /// Maintainability score (0-100)
    pub maintainability_score: f64,
    /// Summary of findings
    pub summary: String,
    /// Specific issues found
    pub issues: Vec<String>,
    /// Suggestions for improvement
    pub suggestions: Vec<String>,
}

impl Default for FileScoreResult {
    fn default() -> Self {
        Self {
            overall_score: 50.0,
            security_score: 50.0,
            quality_score: 50.0,
            complexity_score: 50.0,
            maintainability_score: 50.0,
            summary: String::new(),
            issues: Vec::new(),
            suggestions: Vec::new(),
        }
    }
}

/// Quick analysis result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuickAnalysisResult {
    /// Main findings
    pub findings: String,
    /// Estimated quality (1-10)
    pub quality_rating: i32,
    /// Key concerns
    pub concerns: Vec<String>,
}

/// Repository-wide analysis result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepositoryAnalysis {
    /// Overall health score (0-100)
    pub overall_health: f64,
    /// Identified strengths
    pub strengths: Vec<String>,
    /// Identified weaknesses
    pub weaknesses: Vec<String>,
    /// Security concerns
    pub security_concerns: Vec<String>,
    /// Architecture notes
    pub architecture_notes: String,
    /// Technical debt areas
    pub tech_debt_areas: Vec<String>,
    /// Recommendations
    pub recommendations: Vec<String>,
}

impl Default for RepositoryAnalysis {
    fn default() -> Self {
        Self {
            overall_health: 50.0,
            strengths: Vec::new(),
            weaknesses: Vec::new(),
            security_concerns: Vec::new(),
            architecture_notes: String::new(),
            tech_debt_areas: Vec::new(),
            recommendations: Vec::new(),
        }
    }
}

impl GrokClient {
    /// Create a new Grok client
    pub fn new(api_key: impl Into<String>, db: Database) -> Self {
        let model = std::env::var("XAI_MODEL").unwrap_or_else(|_| GROK_MODEL.to_string());

        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(180))
            .build()
            .expect("Failed to build HTTP client");

        Self {
            client,
            api_key: api_key.into(),
            db,
            model,
            cache: None,
            caching_enabled: false,
        }
    }

    /// Enable caching with the specified database path
    pub async fn with_cache(mut self, cache_db_path: &str) -> Result<Self> {
        let cache = ResponseCache::new(cache_db_path).await?;
        self.cache = Some(cache);
        self.caching_enabled = true;
        Ok(self)
    }

    /// Return the model name this client is configured to use.
    pub fn model_name(&self) -> &str {
        &self.model
    }

    /// Disable caching
    pub fn without_cache(mut self) -> Self {
        self.caching_enabled = false;
        self
    }

    /// Create client from environment variable
    pub async fn from_env(db: Database) -> Result<Self> {
        let api_key = std::env::var("XAI_API_KEY")
            .or_else(|_| std::env::var("GROK_API_KEY"))
            .context("XAI_API_KEY or GROK_API_KEY environment variable not set")?;

        // Enable caching by default
        let client = Self::new(api_key, db);
        client.with_cache("data/rustcode_cache.db").await
    }

    /// Score a file using Grok (with caching)
    pub async fn score_file(&self, file_path: &str, content: &str) -> Result<FileScoreResult> {
        // Check cache first
        if self.caching_enabled {
            if let Some(ref cache) = self.cache {
                let cache_key = format!("{}:{}", file_path, content);
                if let Some(cached_response) = cache.get(&cache_key, "file_scoring").await? {
                    info!("Using cached response for file scoring: {}", file_path);
                    let result: FileScoreResult = serde_json::from_str(&cached_response)
                        .unwrap_or_else(|_| FileScoreResult::default());
                    return Ok(result);
                }
            }
        }

        let prompt = format!(
            r#"Analyze this code file and provide a detailed scoring. Return ONLY valid JSON with this structure:
{{
  "overall_score": 0-100,
  "security_score": 0-100,
  "quality_score": 0-100,
  "complexity_score": 0-100,
  "maintainability_score": 0-100,
  "summary": "brief summary",
  "issues": ["issue1", "issue2"],
  "suggestions": ["suggestion1", "suggestion2"]
}}

File: {}
Content:
```
{}
```

Provide scores where:
- 90-100: Excellent
- 70-89: Good
- 50-69: Acceptable
- 30-49: Needs improvement
- 0-29: Poor

Focus on: security vulnerabilities, code quality, complexity, and maintainability."#,
            file_path, content
        );

        let response = self
            .call_api(&prompt, "file_scoring", None)
            .await
            .context("Failed to score file with Grok API")?;

        // Parse JSON response
        let result: FileScoreResult = serde_json::from_str(&response.content).unwrap_or_else(|e| {
            warn!(
                "Failed to parse Grok response as JSON: {}. Using default scores.",
                e
            );
            FileScoreResult::default()
        });

        // Cache the result
        if self.caching_enabled {
            if let Some(ref cache) = self.cache {
                let cache_key = format!("{}:{}", file_path, content);
                let result_json = serde_json::to_string(&result).unwrap_or_default();
                if let Err(e) = cache
                    .set(&cache_key, "file_scoring", &result_json, Some(168))
                    .await
                {
                    warn!("Failed to cache response: {}", e);
                }
            }
        }

        Ok(result)
    }

    /// Quick analysis of code
    pub async fn quick_analysis(&self, code: &str) -> Result<QuickAnalysisResult> {
        let prompt = format!(
            r#"Provide a quick analysis of this code. Return ONLY valid JSON:
{{
  "findings": "brief analysis",
  "quality_rating": 1-10,
  "concerns": ["concern1", "concern2"]
}}

Code:
```
{}
```"#,
            code
        );

        let response = self
            .call_api(&prompt, "quick_analysis", None)
            .await
            .context("Failed to analyze code with Grok API")?;

        let result: QuickAnalysisResult =
            serde_json::from_str(&response.content).unwrap_or_else(|_| QuickAnalysisResult {
                findings: response.content.clone(),
                quality_rating: 5,
                concerns: vec![],
            });

        Ok(result)
    }

    /// Ask a question about code or project
    pub async fn ask(&self, question: &str, context: Option<&str>) -> Result<String> {
        let prompt = if let Some(ctx) = context {
            format!("Context:\n{}\n\nQuestion: {}", ctx, question)
        } else {
            question.to_string()
        };

        let response = self
            .call_api(&prompt, "question", None)
            .await
            .context("Failed to ask Grok")?;

        Ok(response.content)
    }

    /// Ask a question and return the response with token/cost tracking info
    pub async fn ask_tracked(
        &self,
        question: &str,
        context: Option<&str>,
        operation: &str,
    ) -> Result<AskResponse> {
        let prompt = if let Some(ctx) = context {
            format!("Context:\n{}\n\nQuestion: {}", ctx, question)
        } else {
            question.to_string()
        };

        let response = self
            .call_api(&prompt, operation, None)
            .await
            .context("Failed to ask Grok (tracked)")?;

        let cost = self.calculate_cost(&response.usage);

        Ok(AskResponse {
            content: response.content,
            total_tokens: response.usage.total_tokens,
            prompt_tokens: response.usage.prompt_tokens,
            completion_tokens: response.usage.completion_tokens,
            cost_usd: cost,
        })
    }

    /// Ask a question with full repository context
    pub async fn ask_with_context(
        &self,
        question: &str,
        context: &crate::context_builder::Context,
        repository_id: Option<i64>,
    ) -> Result<String> {
        let prompt = format!(
            "{}\n\n# Question\n\n{}\n\nPlease analyze the codebase context above and provide a detailed answer.",
            context.to_prompt(),
            question
        );

        let response = self
            .call_api(&prompt, "context_query", repository_id)
            .await
            .context("Failed to ask Grok with context")?;

        Ok(response.content)
    }

    /// Analyze entire repository for patterns and issues
    pub async fn analyze_repository(
        &self,
        context: &crate::context_builder::Context,
        repository_id: Option<i64>,
    ) -> Result<RepositoryAnalysis> {
        let prompt = format!(
            r#"{}\n\n# Task\n\nAnalyze this codebase and provide a comprehensive assessment. Return ONLY valid JSON:
{{
  "overall_health": 0-100,
  "strengths": ["strength1", "strength2"],
  "weaknesses": ["weakness1", "weakness2"],
  "security_concerns": ["concern1", "concern2"],
  "architecture_notes": "notes here",
  "tech_debt_areas": ["area1", "area2"],
  "recommendations": ["rec1", "rec2"]
}}"#,
            context.to_prompt()
        );

        let response = self
            .call_api(&prompt, "repository_analysis", repository_id)
            .await
            .context("Failed to analyze repository")?;

        let result: RepositoryAnalysis = serde_json::from_str(&response.content)
            .unwrap_or_else(|_| RepositoryAnalysis::default());

        Ok(result)
    }

    /// Find code patterns across repository
    pub async fn find_patterns(
        &self,
        context: &crate::context_builder::Context,
        pattern_type: &str,
        repository_id: Option<i64>,
    ) -> Result<Vec<String>> {
        let prompt = format!(
            r#"{}\n\n# Task\n\nFind instances of {} in this codebase. List file paths and line numbers where found."#,
            context.to_prompt(),
            pattern_type
        );

        let response = self
            .call_api(&prompt, "pattern_search", repository_id)
            .await
            .context("Failed to find patterns")?;

        // Parse response into list of findings
        let findings: Vec<String> = response
            .content
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| line.to_string())
            .collect();

        Ok(findings)
    }

    /// Call Grok API with retry logic
    async fn call_api(
        &self,
        prompt: &str,
        operation: &str,
        repository_id: Option<i64>,
    ) -> Result<ApiResponse> {
        let mut last_error = None;

        for attempt in 0..MAX_RETRIES {
            if attempt > 0 {
                let delay =
                    Duration::from_millis(INITIAL_RETRY_DELAY_MS * 2u64.pow(attempt as u32));
                info!(
                    "Retrying API call (attempt {}/{}) after {:?}",
                    attempt + 1,
                    MAX_RETRIES,
                    delay
                );
                tokio::time::sleep(delay).await;
            }

            match self.call_api_once(prompt).await {
                Ok(response) => {
                    // Calculate cost
                    let cost = self.calculate_cost(&response.usage);

                    // Record to database
                    if let Err(e) = self
                        .db
                        .record_llm_cost(
                            &self.model,
                            operation,
                            response.usage.prompt_tokens,
                            response.usage.completion_tokens,
                            cost,
                            repository_id,
                        )
                        .await
                    {
                        warn!("Failed to record LLM cost: {}", e);
                    }

                    info!(
                        "Grok API call successful: {} tokens used, ${:.4} cost",
                        response.usage.total_tokens, cost
                    );

                    return Ok(response);
                }
                Err(e) => {
                    error!("API call failed (attempt {}): {}", attempt + 1, e);
                    last_error = Some(e);
                }
            }
        }

        Err(last_error
            .unwrap_or_else(|| anyhow::anyhow!("API call failed after {} retries", MAX_RETRIES)))
    }

    /// Make a single API call
    async fn call_api_once(&self, prompt: &str) -> Result<ApiResponse> {
        let max_tokens = std::env::var("XAI_MAX_TOKENS")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(8000);

        let request = ChatCompletionRequest {
            model: self.model.clone(),
            messages: vec![Message {
                role: "user".to_string(),
                content: prompt.to_string(),
            }],
            temperature: 0.3,
            max_tokens,
        };

        debug!(
            "Calling Grok API with prompt length: {} chars",
            prompt.len()
        );

        let response = self
            .client
            .post(format!("{}/chat/completions", GROK_API_BASE))
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .json(&request)
            .send()
            .await
            .context("Failed to send request to Grok API")?;

        let status = response.status();
        if !status.is_success() {
            let error_text = response
                .text()
                .await
                .unwrap_or_else(|_| "Unknown error".to_string());
            return Err(anyhow::anyhow!(
                "API returned error {}: {}",
                status,
                error_text
            ));
        }

        let api_response: ChatCompletionResponse = response
            .json()
            .await
            .context("Failed to parse API response")?;

        if api_response.choices.is_empty() {
            return Err(anyhow::anyhow!("API returned no choices"));
        }

        Ok(ApiResponse {
            content: api_response.choices[0].message.content.clone(),
            usage: api_response.usage,
        })
    }

    /// Calculate estimated cost from usage
    fn calculate_cost(&self, usage: &Usage) -> f64 {
        let input_cost = (usage.prompt_tokens as f64 / 1_000_000.0) * COST_PER_MILLION_INPUT_TOKENS;
        let output_cost =
            (usage.completion_tokens as f64 / 1_000_000.0) * COST_PER_MILLION_OUTPUT_TOKENS;
        input_cost + output_cost
    }

    /// Get total cost from database
    pub async fn get_total_cost(&self) -> Result<f64> {
        self.db
            .get_total_llm_cost()
            .await
            .map_err(|e| anyhow::anyhow!("{}", e))
    }

    /// Get cost for last N days
    pub async fn get_cost_last_n_days(&self, days: i64) -> Result<f64> {
        self.db
            .get_llm_cost_by_period(days)
            .await
            .map_err(|e| anyhow::anyhow!("{}", e))
    }

    /// Get cost breakdown by model
    pub async fn get_cost_by_model(&self) -> Result<Vec<(String, f64, i64)>> {
        let map = self
            .db
            .get_cost_by_model()
            .await
            .map_err(|e| anyhow::anyhow!("{}", e))?;
        // Convert HashMap to Vec for backward compatibility
        Ok(map.into_iter().map(|(k, v)| (k, v, 0)).collect())
    }

    /// Get cache statistics
    pub async fn get_cache_stats(&self) -> Result<Option<crate::response_cache::CacheStats>> {
        if let Some(ref cache) = self.cache {
            Ok(Some(cache.get_stats().await?))
        } else {
            Ok(None)
        }
    }

    /// Clear cache
    pub async fn clear_cache(&self) -> Result<u64> {
        if let Some(ref cache) = self.cache {
            cache.clear_all().await
        } else {
            Ok(0)
        }
    }

    /// Clear expired cache entries
    pub async fn clear_expired_cache(&self) -> Result<u64> {
        if let Some(ref cache) = self.cache {
            cache.clear_expired().await
        } else {
            Ok(0)
        }
    }
}

/// Internal API response
struct ApiResponse {
    content: String,
    usage: Usage,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_cost_calculation() {
        let db = Database::new(&std::env::var("DATABASE_URL").unwrap_or_else(|_| {
            "postgresql://rustcode:changeme@localhost:5432/rustcode_test".to_string()
        }))
        .await
        .unwrap();
        let client = GrokClient::new("test-key", db);

        let usage = Usage {
            prompt_tokens: 1000,
            completion_tokens: 500,
            total_tokens: 1500,
        };

        let cost = client.calculate_cost(&usage);

        // 1000 * $0.20/1M + 500 * $0.50/1M = $0.0002 + $0.00025 = $0.00045
        assert!((cost - 0.00045).abs() < 0.00001);
    }
}
