//! Compatibility layer for old LLM client interface
//!
//! This module provides backward compatibility with the old LlmClient interface
//! that was used by enhanced_scanner, llm_audit, research, and server modules.

use crate::error::{AuditError, Result};
use crate::types::Category;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::time::Duration;
use tracing::{info, warn};

/// LLM client for code analysis (compatibility layer)
pub struct LlmClient {
    /// HTTP client
    client: Client,
    /// API key
    api_key: String,
    /// Model name
    model: String,
    /// LLM provider (xai, google)
    provider: String,
    /// Base URL
    base_url: String,
    /// Max tokens
    max_tokens: usize,
    /// Temperature
    temperature: f64,
}

impl LlmClient {
    /// Create a new LLM client with provider detection
    pub fn new(
        api_key: String,
        model: String,
        max_tokens: usize,
        temperature: f64,
    ) -> Result<Self> {
        // Auto-detect provider from model name
        let provider = if model.starts_with("gemini") {
            "google".to_string()
        } else if model.starts_with("grok") {
            "xai".to_string()
        } else if model.starts_with("claude") {
            "anthropic".to_string()
        } else {
            // Default to XAI
            "xai".to_string()
        };

        Self::new_with_provider(api_key, provider, model, max_tokens, temperature)
    }

    /// Create a new LLM client with explicit provider
    pub fn new_with_provider(
        api_key: String,
        provider: String,
        model: String,
        max_tokens: usize,
        temperature: f64,
    ) -> Result<Self> {
        let client = Client::builder()
            .timeout(Duration::from_secs(300))
            .build()
            .map_err(|e| AuditError::other(format!("Failed to create HTTP client: {}", e)))?;

        let base_url = match provider.as_str() {
            "google" | "gemini" => "https://generativelanguage.googleapis.com/v1beta".to_string(),
            "xai" | "grok" => "https://api.x.ai/v1".to_string(),
            "anthropic" | "claude" => "https://api.anthropic.com/v1".to_string(),
            _ => {
                warn!("Unknown provider '{}', defaulting to XAI", provider);
                "https://api.x.ai/v1".to_string()
            }
        };

        info!(
            "LLM client initialized: provider={}, model={}, base_url={}",
            provider, model, base_url
        );

        Ok(Self {
            client,
            api_key,
            model,
            provider,
            base_url,
            max_tokens,
            temperature,
        })
    }

    /// Analyze a file with LLM
    pub async fn analyze_file(
        &self,
        file_path: &Path,
        content: &str,
        category: Category,
    ) -> Result<LlmAnalysisResult> {
        let system_prompt = self.build_system_prompt(category);
        let user_prompt = self.build_file_prompt(file_path, content);

        self.call_llm(&system_prompt, &user_prompt).await
    }

    /// Build system prompt based on category
    fn build_system_prompt(&self, category: Category) -> String {
        format!(
            "You are an expert code analyst. Analyze the following {} code and provide insights.",
            match category {
                Category::Janus => "core",
                Category::Execution => "execution",
                Category::Clients => "client",
                Category::Audit => "audit",
                Category::Infra => "infrastructure",
                Category::Config => "configuration",
                Category::Documentation => "documentation",
                Category::Tests => "test",
                Category::Other => "other",
            }
        )
    }

    /// Build file-specific prompt
    fn build_file_prompt(&self, file_path: &Path, content: &str) -> String {
        format!(
            "File: {}\n\nContent:\n```\n{}\n```\n\nProvide a structured analysis.",
            file_path.display(),
            content
        )
    }

    /// Call the LLM API
    async fn call_llm(&self, system: &str, user: &str) -> Result<LlmAnalysisResult> {
        match self.provider.as_str() {
            "xai" | "grok" => self.call_xai(system, user).await,
            "google" | "gemini" => self.call_google(system, user).await,
            "anthropic" | "claude" => self.call_anthropic(system, user).await,
            _ => Err(AuditError::other(format!(
                "Unsupported provider: {}",
                self.provider
            ))),
        }
    }

    /// Call XAI/Grok API
    async fn call_xai(&self, system: &str, user: &str) -> Result<LlmAnalysisResult> {
        #[derive(Serialize)]
        struct XaiRequest {
            model: String,
            messages: Vec<XaiMessage>,
            temperature: f64,
            max_tokens: usize,
        }

        #[derive(Serialize)]
        struct XaiMessage {
            role: String,
            content: String,
        }

        #[derive(Deserialize)]
        struct XaiResponse {
            choices: Vec<XaiChoice>,
            usage: Option<XaiUsage>,
        }

        #[derive(Deserialize)]
        struct XaiUsage {
            #[allow(dead_code)]
            prompt_tokens: Option<usize>,
            #[allow(dead_code)]
            completion_tokens: Option<usize>,
            total_tokens: Option<usize>,
        }

        #[derive(Deserialize)]
        struct XaiChoice {
            message: XaiResponseMessage,
        }

        #[derive(Deserialize)]
        struct XaiResponseMessage {
            content: String,
        }

        let request = XaiRequest {
            model: self.model.clone(),
            messages: vec![
                XaiMessage {
                    role: "system".to_string(),
                    content: system.to_string(),
                },
                XaiMessage {
                    role: "user".to_string(),
                    content: user.to_string(),
                },
            ],
            temperature: self.temperature,
            max_tokens: self.max_tokens,
        };

        let response = self
            .client
            .post(format!("{}/chat/completions", self.base_url))
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .json(&request)
            .send()
            .await
            .map_err(|e| AuditError::other(format!("XAI API request failed: {}", e)))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(AuditError::other(format!(
                "XAI API error {}: {}",
                status, body
            )));
        }

        let data: XaiResponse = response
            .json()
            .await
            .map_err(|e| AuditError::other(format!("Failed to parse XAI response: {}", e)))?;

        let content = data
            .choices
            .first()
            .map(|c| c.message.content.clone())
            .unwrap_or_default();

        let tokens_used = data.usage.and_then(|u| u.total_tokens);

        Ok(LlmAnalysisResult {
            summary: content.lines().take(3).collect::<Vec<_>>().join(" "),
            content,
            model: self.model.clone(),
            importance: 5.0,
            security_rating: "B".to_string(),
            issues: Vec::new(),
            deprecated_files: Vec::new(),
            missing_types: Vec::new(),
            security_concerns: Vec::new(),
            architecture_issues: Vec::new(),
            tokens_used,
        })
    }

    /// Call Google/Gemini API
    async fn call_google(&self, system: &str, user: &str) -> Result<LlmAnalysisResult> {
        // Simplified implementation - in production, use proper Gemini API
        let combined = format!("{}\n\n{}", system, user);

        #[derive(Serialize)]
        struct GeminiRequest {
            contents: Vec<GeminiContent>,
        }

        #[derive(Serialize)]
        struct GeminiContent {
            parts: Vec<GeminiPart>,
        }

        #[derive(Serialize)]
        struct GeminiPart {
            text: String,
        }

        #[derive(Deserialize)]
        struct GeminiResponse {
            candidates: Vec<GeminiCandidate>,
            #[serde(rename = "usageMetadata")]
            usage_metadata: Option<GeminiUsage>,
        }

        #[derive(Deserialize)]
        struct GeminiUsage {
            #[serde(rename = "promptTokenCount")]
            #[allow(dead_code)]
            prompt_token_count: Option<usize>,
            #[serde(rename = "candidatesTokenCount")]
            #[allow(dead_code)]
            candidates_token_count: Option<usize>,
            #[serde(rename = "totalTokenCount")]
            total_token_count: Option<usize>,
        }

        #[derive(Deserialize)]
        struct GeminiCandidate {
            content: GeminiResponseContent,
        }

        #[derive(Deserialize)]
        struct GeminiResponseContent {
            parts: Vec<GeminiResponsePart>,
        }

        #[derive(Deserialize)]
        struct GeminiResponsePart {
            text: String,
        }

        let request = GeminiRequest {
            contents: vec![GeminiContent {
                parts: vec![GeminiPart { text: combined }],
            }],
        };

        let url = format!(
            "{}/models/{}:generateContent?key={}",
            self.base_url, self.model, self.api_key
        );

        let response = self
            .client
            .post(&url)
            .header("Content-Type", "application/json")
            .json(&request)
            .send()
            .await
            .map_err(|e| AuditError::other(format!("Gemini API request failed: {}", e)))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(AuditError::other(format!(
                "Gemini API error {}: {}",
                status, body
            )));
        }

        let data: GeminiResponse = response
            .json()
            .await
            .map_err(|e| AuditError::other(format!("Failed to parse Gemini response: {}", e)))?;

        let content = data
            .candidates
            .first()
            .and_then(|c| c.content.parts.first())
            .map(|p| p.text.clone())
            .unwrap_or_default();

        let tokens_used = data.usage_metadata.and_then(|u| u.total_token_count);

        Ok(LlmAnalysisResult {
            summary: content.lines().take(3).collect::<Vec<_>>().join(" "),
            content,
            model: self.model.clone(),
            importance: 5.0,
            security_rating: "B".to_string(),
            issues: Vec::new(),
            deprecated_files: Vec::new(),
            missing_types: Vec::new(),
            security_concerns: Vec::new(),
            architecture_issues: Vec::new(),
            tokens_used,
        })
    }

    /// Call Anthropic/Claude API
    async fn call_anthropic(&self, system: &str, user: &str) -> Result<LlmAnalysisResult> {
        #[derive(Serialize)]
        struct ClaudeRequest {
            model: String,
            max_tokens: usize,
            messages: Vec<ClaudeMessage>,
            system: String,
        }

        #[derive(Serialize)]
        struct ClaudeMessage {
            role: String,
            content: String,
        }

        #[derive(Deserialize)]
        struct ClaudeResponse {
            content: Vec<ClaudeContent>,
            usage: Option<ClaudeUsage>,
        }

        #[derive(Deserialize)]
        struct ClaudeUsage {
            input_tokens: Option<usize>,
            output_tokens: Option<usize>,
        }

        #[derive(Deserialize)]
        struct ClaudeContent {
            text: String,
        }

        let request = ClaudeRequest {
            model: self.model.clone(),
            max_tokens: self.max_tokens,
            system: system.to_string(),
            messages: vec![ClaudeMessage {
                role: "user".to_string(),
                content: user.to_string(),
            }],
        };

        let response = self
            .client
            .post(format!("{}/messages", self.base_url))
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .header("Content-Type", "application/json")
            .json(&request)
            .send()
            .await
            .map_err(|e| AuditError::other(format!("Claude API request failed: {}", e)))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(AuditError::other(format!(
                "Claude API error {}: {}",
                status, body
            )));
        }

        let data: ClaudeResponse = response
            .json()
            .await
            .map_err(|e| AuditError::other(format!("Failed to parse Claude response: {}", e)))?;

        let content = data
            .content
            .first()
            .map(|c| c.text.clone())
            .unwrap_or_default();

        let tokens_used = data
            .usage
            .map(|u| u.input_tokens.unwrap_or(0) + u.output_tokens.unwrap_or(0));

        Ok(LlmAnalysisResult {
            summary: content.lines().take(3).collect::<Vec<_>>().join(" "),
            content,
            model: self.model.clone(),
            importance: 5.0,
            security_rating: "B".to_string(),
            issues: Vec::new(),
            deprecated_files: Vec::new(),
            missing_types: Vec::new(),
            security_concerns: Vec::new(),
            architecture_issues: Vec::new(),
            tokens_used,
        })
    }

    /// Analyze content with global context
    pub async fn analyze_with_global_context(
        &self,
        context: &str,
        question: &str,
    ) -> Result<String> {
        let system = "You are analyzing a codebase with global context. Provide detailed, actionable insights.";
        let user = format!(
            "Global Context:\n{}\n\nQuestion: {}\n\nProvide a comprehensive answer.",
            context, question
        );
        let result = self.call_llm(system, &user).await?;
        Ok(result.content)
    }

    /// Run standard questionnaire for codebase analysis
    pub async fn run_standard_questionnaire(
        &self,
        codebase_context: &str,
    ) -> Result<Vec<FileAuditResult>> {
        let system = "You are a senior software architect performing a code audit. Analyze the codebase and identify issues, improvements, and quality metrics.";
        let user = format!(
            "Codebase Context:\n{}\n\nProvide a comprehensive analysis including:\n1. Code quality issues\n2. Security concerns\n3. Improvement suggestions\n4. Overall quality assessment",
            codebase_context
        );

        let result = self.call_llm(system, &user).await?;

        // Create a single FileAuditResult from the analysis
        let audit = FileAuditResult {
            summary: result.summary.clone(),
            issues: vec![],
            suggestions: vec![
                "Review code quality".to_string(),
                "Address security concerns".to_string(),
                "Improve test coverage".to_string(),
            ],
            quality_score: ((result.importance / 10.0) * 10.0) as u8,
            file: "codebase".to_string(),
            improvement: result.content.clone(),
            reachable: true,
            incomplete: false,
            compliance_issues: result.security_concerns.clone(),
        };

        Ok(vec![audit])
    }

    /// Analyze entire codebase
    pub async fn analyze_codebase(&self, files: &[(&str, &str)]) -> Result<LlmAnalysisResult> {
        let system = "You are analyzing an entire codebase. Provide a comprehensive analysis covering architecture, quality, security, and recommendations.";

        let files_summary = files
            .iter()
            .take(20) // Limit to first 20 files to avoid token limits
            .map(|(path, content)| {
                let preview = content.lines().take(10).collect::<Vec<_>>().join("\n");
                format!("File: {}\n{}\n...", path, preview)
            })
            .collect::<Vec<_>>()
            .join("\n\n");

        let user = format!(
            "Codebase Files:\n{}\n\nProvide analysis including:\n1. Architecture overview\n2. Code quality assessment\n3. Security concerns\n4. Performance considerations\n5. Recommendations",
            files_summary
        );

        self.call_llm(system, &user).await
    }
}

/// Issue found during analysis
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Issue {
    pub severity: String,
    pub description: String,
    pub suggestion: Option<String>,
}

/// Result from LLM analysis
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmAnalysisResult {
    pub content: String,
    pub summary: String,
    pub model: String,
    pub importance: f64,
    pub security_rating: String,
    pub issues: Vec<Issue>,
    pub deprecated_files: Vec<String>,
    pub missing_types: Vec<String>,
    pub security_concerns: Vec<String>,
    pub architecture_issues: Vec<String>,
    pub tokens_used: Option<usize>,
}

/// File audit result (compatibility type)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileAuditResult {
    pub summary: String,
    pub issues: Vec<String>,
    pub suggestions: Vec<String>,
    pub quality_score: u8,
    pub file: String,
    pub improvement: String,
    pub reachable: bool,
    pub incomplete: bool,
    pub compliance_issues: Vec<String>,
}

impl FileAuditResult {
    /// Parse from LLM analysis result
    pub fn from_llm_result(result: &LlmAnalysisResult) -> Self {
        // Simple parsing - in production, use structured output
        Self {
            summary: result.summary.clone(),
            issues: result
                .issues
                .iter()
                .map(|i| i.description.clone())
                .collect(),
            suggestions: result
                .issues
                .iter()
                .filter_map(|i| i.suggestion.clone())
                .collect(),
            quality_score: 7,
            file: String::new(),
            improvement: String::new(),
            reachable: true,
            incomplete: false,
            compliance_issues: Vec::new(),
        }
    }
}
