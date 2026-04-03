//! Grok 4.1 Reasoning Client Module
//!
//! Specialized client for xAI's Grok 4.1 with reasoning capabilities:
//! - Agentic tool calling (code_execution, web_search)
//! - 2M context window optimization with intelligent batching
//! - max_turns control for cost management
//! - File-by-file review with scoring integration
//! - CI/CD integration with progress tracking
//! - Retry logic with exponential backoff

use crate::cache::{AuditCache, CacheEntry};
use crate::error::{AuditError, Result};
use crate::llm_config::LimitsConfig;
use crate::scoring::FileScore;
use crate::tree_state::FileCategory;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::Duration;
use tokio::time::sleep;
use tracing::{debug, error, info, warn};

/// Default Grok 4.1 model for reasoning tasks
pub const GROK_REASONING_MODEL: &str = "grok-4-1-fast-reasoning";

/// Alternative fast model (non-reasoning)
pub const GROK_FAST_MODEL: &str = "grok-4-1-fast";

/// Maximum context window (2M tokens)
pub const MAX_CONTEXT_TOKENS: usize = 2_000_000;

/// Estimated tokens per character (rough approximation)
pub const TOKENS_PER_CHAR: f64 = 0.25;

/// Default max turns for agentic requests
pub const DEFAULT_MAX_TURNS: usize = 5;

/// Batch size thresholds based on file size
pub const SMALL_FILE_LOC: usize = 100;
pub const MEDIUM_FILE_LOC: usize = 500;
pub const LARGE_FILE_LOC: usize = 1000;

/// Retry configuration for API calls
#[derive(Debug, Clone)]
pub struct RetryConfig {
    /// Maximum number of retry attempts
    pub max_retries: usize,
    /// Initial delay between retries in milliseconds
    pub initial_delay_ms: u64,
    /// Whether to use exponential backoff
    pub exponential_backoff: bool,
    /// Maximum delay cap in milliseconds
    pub max_delay_ms: u64,
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            max_retries: 3,
            initial_delay_ms: 1000,
            exponential_backoff: true,
            max_delay_ms: 30000,
        }
    }
}

impl RetryConfig {
    /// Create from LimitsConfig
    pub fn from_limits(limits: &LimitsConfig) -> Self {
        Self {
            max_retries: limits.max_retries,
            initial_delay_ms: limits.retry_delay_ms,
            exponential_backoff: limits.exponential_backoff,
            max_delay_ms: 30000,
        }
    }

    /// Calculate delay for a given attempt (0-indexed)
    pub fn delay_for_attempt(&self, attempt: usize) -> Duration {
        let delay_ms = if self.exponential_backoff {
            let exp_delay = self.initial_delay_ms * 2u64.pow(attempt as u32);
            exp_delay.min(self.max_delay_ms)
        } else {
            self.initial_delay_ms
        };
        Duration::from_millis(delay_ms)
    }
}

/// Grok 4.1 Reasoning Client
pub struct GrokReasoningClient {
    /// HTTP client
    client: Client,

    /// API key
    api_key: String,

    /// Model to use
    model: String,

    /// Base URL for xAI API
    base_url: String,

    /// Maximum tokens per request
    max_tokens: usize,

    /// Temperature for responses
    temperature: f64,

    /// Max turns for agentic requests
    max_turns: usize,

    /// Enable code execution tool
    enable_code_execution: bool,

    /// Enable reasoning mode
    enable_reasoning: bool,

    /// Request timeout (stored for future use)
    _timeout: Duration,

    /// Retry configuration
    retry_config: RetryConfig,
}

/// Batch of files for analysis
#[derive(Debug, Clone)]
pub struct FileBatch {
    /// Files in this batch
    pub files: Vec<FileForAnalysis>,

    /// Batch ID
    pub batch_id: usize,

    /// Total estimated tokens
    pub estimated_tokens: usize,

    /// Priority score (higher = analyze first)
    pub priority: f64,

    /// Category of files in this batch
    pub category: FileCategory,
}

/// Single file prepared for analysis
#[derive(Debug, Clone, Serialize)]
pub struct FileForAnalysis {
    /// File path
    pub path: String,

    /// File content
    pub content: String,

    /// Lines of code
    pub lines: usize,

    /// File score (if available)
    pub score: Option<FileScore>,

    /// Category
    pub category: FileCategory,

    /// Content hash for caching
    pub content_hash: String,
}

/// Analysis result for a single file
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileAnalysisResult {
    /// File path
    pub path: String,

    /// Overall score (0-100)
    #[serde(default = "default_score")]
    pub overall_score: f64,

    /// Security score (0-100)
    #[serde(default = "default_score")]
    pub security_score: f64,

    /// Quality score (0-100)
    #[serde(default = "default_score")]
    pub quality_score: f64,

    /// Complexity score (0-100, lower is better)
    #[serde(default = "default_score")]
    pub complexity_score: f64,

    /// Maintainability score (0-100)
    #[serde(default = "default_score")]
    pub maintainability_score: f64,

    /// Summary assessment
    #[serde(default)]
    pub summary: String,

    /// Identified issues
    #[serde(default)]
    pub issues: Vec<IdentifiedIssue>,

    /// Suggested improvements
    #[serde(default)]
    pub improvements: Vec<Improvement>,

    /// Detected patterns (good and bad)
    #[serde(default)]
    pub patterns: Vec<DetectedPattern>,

    /// Dependencies identified
    #[serde(default)]
    pub dependencies: Vec<String>,

    /// Test coverage assessment
    #[serde(default)]
    pub test_coverage: Option<String>,

    /// Reasoning trace (if enabled)
    #[serde(default)]
    pub reasoning_trace: Option<String>,

    /// Tokens used (populated by client, not LLM response)
    #[serde(default)]
    pub tokens_used: TokenUsage,
}

fn default_score() -> f64 {
    50.0
}

/// Identified issue in code
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IdentifiedIssue {
    /// Severity (critical, high, medium, low)
    #[serde(default)]
    pub severity: String,

    /// Issue category
    /// Category (security, quality, performance, etc)
    #[serde(default)]
    pub category: String,

    /// Line number (if applicable)
    #[serde(default)]
    pub line: Option<usize>,

    /// Description
    #[serde(default)]
    pub description: String,

    /// Suggested fix
    #[serde(default)]
    pub suggested_fix: Option<String>,
}

/// Suggested improvement
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Improvement {
    /// Priority (1 = highest)
    #[serde(default = "default_priority")]
    pub priority: usize,

    /// Category
    #[serde(default)]
    pub category: String,

    /// Description
    #[serde(default)]
    pub description: String,

    /// Effort estimate
    #[serde(default)]
    pub effort: String,

    /// Impact estimate
    #[serde(default)]
    pub impact: String,
}

fn default_priority() -> usize {
    1
}

/// Detected pattern
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DetectedPattern {
    /// Pattern name
    #[serde(default)]
    pub name: String,

    /// Whether it's positive or negative
    #[serde(default)]
    pub is_positive: bool,

    /// Description
    #[serde(default)]
    pub description: String,

    /// Number of occurrences
    #[serde(default)]
    pub occurrences: usize,
}

/// Token usage tracking
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TokenUsage {
    /// Prompt tokens
    pub prompt_tokens: usize,

    /// Completion tokens
    pub completion_tokens: usize,

    /// Reasoning tokens (Grok-specific)
    pub reasoning_tokens: usize,

    /// Cached prompt tokens
    pub cached_tokens: usize,

    /// Total tokens
    pub total_tokens: usize,
}

/// Batch analysis result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BatchAnalysisResult {
    /// Batch ID
    pub batch_id: usize,

    /// Individual file results
    pub file_results: Vec<FileAnalysisResult>,

    /// Batch-level insights
    pub batch_insights: Option<String>,

    /// Total tokens used
    pub total_tokens: TokenUsage,

    /// Processing time in milliseconds
    pub processing_time_ms: u64,

    /// Number of tool calls made
    pub tool_calls_count: usize,
}

/// Request for xAI Responses API
#[derive(Debug, Serialize)]
struct ResponsesRequest {
    model: String,
    input: Vec<Message>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<Tool>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_turns: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f64>,
}

/// Message for API request
#[derive(Debug, Serialize, Deserialize)]
struct Message {
    role: String,
    content: String,
}

/// Tool definition
#[derive(Debug, Serialize)]
struct Tool {
    #[serde(rename = "type")]
    tool_type: String,
}

/// Response from xAI API
#[derive(Debug, Deserialize)]
struct ResponsesResponse {
    #[serde(default)]
    output: Vec<OutputItem>,
    #[serde(default)]
    usage: Option<UsageInfo>,
    #[allow(dead_code)]
    #[serde(default)]
    status: Option<String>,
}

/// Output item in response - this is a message object
#[derive(Debug, Deserialize)]
struct OutputItem {
    #[serde(rename = "type")]
    output_type: Option<String>,
    /// Content can be a string (legacy) or an array of content items (current API)
    #[serde(default)]
    content: Option<OutputContent>,
    /// Direct text field (legacy format)
    text: Option<String>,
    #[serde(default)]
    role: Option<String>,
}

/// Output content - can be array of content items or a string
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum OutputContent {
    /// Array of content items (current API format)
    Items(Vec<ContentItem>),
    /// Direct string content (legacy format)
    Text(String),
}

/// Content item within an output message
#[derive(Debug, Deserialize)]
struct ContentItem {
    #[allow(dead_code)]
    #[serde(rename = "type")]
    content_type: Option<String>,
    #[serde(default)]
    text: Option<String>,
}

/// Usage information
#[derive(Debug, Deserialize)]
struct UsageInfo {
    #[serde(default)]
    input_tokens: usize,
    #[serde(default)]
    output_tokens: usize,
    #[serde(default)]
    total_tokens: usize,
    #[serde(default)]
    input_tokens_details: Option<TokenDetails>,
    #[serde(default)]
    output_tokens_details: Option<OutputTokenDetails>,
}

/// Input token details
#[derive(Debug, Deserialize)]
struct TokenDetails {
    #[serde(default)]
    cached_tokens: usize,
}

/// Output token details
#[derive(Debug, Deserialize)]
struct OutputTokenDetails {
    #[serde(default)]
    reasoning_tokens: usize,
}

impl GrokReasoningClient {
    /// Create a new Grok Reasoning client
    pub fn new(api_key: String) -> Result<Self> {
        let client = Client::builder()
            .timeout(Duration::from_secs(300)) // 5 minute timeout for long reasoning
            .build()
            .map_err(|e| AuditError::other(format!("Failed to create HTTP client: {}", e)))?;

        Ok(Self {
            client,
            api_key,
            model: GROK_REASONING_MODEL.to_string(),
            base_url: "https://api.x.ai/v1".to_string(),
            max_tokens: 32000,
            temperature: 0.3,
            max_turns: DEFAULT_MAX_TURNS,
            enable_code_execution: true,
            enable_reasoning: true,
            _timeout: Duration::from_secs(300),
            retry_config: RetryConfig::default(),
        })
    }

    /// Create with custom configuration
    pub fn with_config(
        api_key: String,
        model: Option<String>,
        max_turns: Option<usize>,
        enable_code_execution: bool,
        enable_reasoning: bool,
    ) -> Result<Self> {
        let mut client = Self::new(api_key)?;

        if let Some(m) = model {
            client.model = m;
        }
        if let Some(mt) = max_turns {
            client.max_turns = mt;
        }
        client.enable_code_execution = enable_code_execution;
        client.enable_reasoning = enable_reasoning;

        Ok(client)
    }

    /// Create with custom configuration including retry settings
    pub fn with_full_config(
        api_key: String,
        model: Option<String>,
        max_turns: Option<usize>,
        enable_code_execution: bool,
        enable_reasoning: bool,
        retry_config: Option<RetryConfig>,
    ) -> Result<Self> {
        let mut client = Self::with_config(
            api_key,
            model,
            max_turns,
            enable_code_execution,
            enable_reasoning,
        )?;

        if let Some(rc) = retry_config {
            client.retry_config = rc;
        }

        Ok(client)
    }

    /// Set retry configuration
    pub fn set_retry_config(&mut self, config: RetryConfig) {
        self.retry_config = config;
    }

    /// Set max turns for agentic requests
    pub fn set_max_turns(&mut self, max_turns: usize) {
        self.max_turns = max_turns;
    }

    /// Set temperature
    pub fn set_temperature(&mut self, temperature: f64) {
        self.temperature = temperature;
    }

    /// Estimate tokens for content
    pub fn estimate_tokens(content: &str) -> usize {
        (content.len() as f64 * TOKENS_PER_CHAR) as usize
    }

    /// Create batches from files for optimal context usage
    pub fn create_batches(
        &self,
        files: Vec<FileForAnalysis>,
        max_batch_tokens: usize,
    ) -> Vec<FileBatch> {
        let mut batches = Vec::new();
        let mut current_batch: Vec<FileForAnalysis> = Vec::new();
        #[allow(unused_assignments)]
        let mut current_tokens: usize = 0;
        let mut batch_id: usize = 0;

        // Sort files by priority (importance + risk score)
        let mut sorted_files = files;
        sorted_files.sort_by(|a, b| {
            let score_a = a
                .score
                .as_ref()
                .map(|s| s.importance + s.risk)
                .unwrap_or(50.0);
            let score_b = b
                .score
                .as_ref()
                .map(|s| s.importance + s.risk)
                .unwrap_or(50.0);
            score_b
                .partial_cmp(&score_a)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        // Group by category first, then batch by token count
        let mut by_category: HashMap<FileCategory, Vec<FileForAnalysis>> = HashMap::new();
        for file in sorted_files {
            by_category.entry(file.category).or_default().push(file);
        }

        for (category, category_files) in by_category {
            current_batch.clear();
            current_tokens = 0;

            for file in category_files {
                let file_tokens = Self::estimate_tokens(&file.content) + 500; // Buffer for prompt

                // Check if adding this file would exceed limits
                if current_tokens + file_tokens > max_batch_tokens && !current_batch.is_empty() {
                    // Create batch from current files
                    let priority = current_batch
                        .iter()
                        .map(|f| {
                            f.score
                                .as_ref()
                                .map(|s| s.importance + s.risk)
                                .unwrap_or(50.0)
                        })
                        .sum::<f64>()
                        / current_batch.len() as f64;

                    batches.push(FileBatch {
                        files: std::mem::take(&mut current_batch),
                        batch_id,
                        estimated_tokens: current_tokens,
                        priority,
                        category,
                    });

                    batch_id += 1;
                    current_tokens = 0;
                }

                // Determine batch size based on file size
                let max_files_in_batch = if file.lines < SMALL_FILE_LOC {
                    15
                } else if file.lines < MEDIUM_FILE_LOC {
                    8
                } else if file.lines < LARGE_FILE_LOC {
                    3
                } else {
                    1
                };

                // Check file count limit
                if current_batch.len() >= max_files_in_batch {
                    let priority = current_batch
                        .iter()
                        .map(|f| {
                            f.score
                                .as_ref()
                                .map(|s| s.importance + s.risk)
                                .unwrap_or(50.0)
                        })
                        .sum::<f64>()
                        / current_batch.len() as f64;

                    batches.push(FileBatch {
                        files: std::mem::take(&mut current_batch),
                        batch_id,
                        estimated_tokens: current_tokens,
                        priority,
                        category,
                    });

                    batch_id += 1;
                    current_tokens = 0;
                }

                current_batch.push(file);
                current_tokens += file_tokens;
            }

            // Don't forget the last batch
            if !current_batch.is_empty() {
                let priority = current_batch
                    .iter()
                    .map(|f| {
                        f.score
                            .as_ref()
                            .map(|s| s.importance + s.risk)
                            .unwrap_or(50.0)
                    })
                    .sum::<f64>()
                    / current_batch.len() as f64;

                batches.push(FileBatch {
                    files: std::mem::take(&mut current_batch),
                    batch_id,
                    estimated_tokens: current_tokens,
                    priority,
                    category,
                });

                batch_id += 1;
            }
        }

        // Sort batches by priority
        batches.sort_by(|a, b| {
            b.priority
                .partial_cmp(&a.priority)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        info!(
            "Created {} batches from {} files",
            batches.len(),
            batches.iter().map(|b| b.files.len()).sum::<usize>()
        );

        batches
    }

    /// Build system prompt for code analysis
    fn build_analysis_system_prompt(&self, category: FileCategory) -> String {
        let category_context = match category {
            FileCategory::Audit => {
                "You are analyzing the Audit service - a Rust codebase for code analysis and LLM integration."
            }
            FileCategory::Janus => {
                "You are analyzing JANUS - a neuromorphic trading algorithm system with ML components."
            }
            FileCategory::Clients => {
                "You are analyzing KMP (Kotlin Multiplatform) client code for mobile/cross-platform apps."
            }
            FileCategory::Execution => {
                "You are analyzing the Execution service - a Rust application for trade execution."
            }
            FileCategory::Config => "You are analyzing configuration files.",
            FileCategory::Docs => "You are analyzing documentation files.",
            FileCategory::Tests => "You are analyzing test files.",
            FileCategory::Other => "You are analyzing source code.",
        };

        format!(
            r#"You are an expert code reviewer with deep expertise in software architecture, security, and best practices.

{}

Your task is to analyze each file and provide:
1. **Overall Score** (0-100): Combined assessment of quality, security, and maintainability
2. **Security Score** (0-100): Assessment of security practices and vulnerabilities
3. **Quality Score** (0-100): Code quality, readability, and adherence to best practices
4. **Complexity Score** (0-100, lower is better): Cyclomatic complexity and cognitive load
5. **Maintainability Score** (0-100): Ease of maintenance and future modifications

For each file, identify:
- **Issues**: Critical, high, medium, and low severity problems with line numbers when possible
- **Improvements**: Prioritized suggestions with effort/impact estimates
- **Patterns**: Both positive patterns to preserve and anti-patterns to address
- **Dependencies**: External and internal dependencies

Focus on:
- Security vulnerabilities (injection, auth, crypto, data exposure)
- Error handling and panic safety (especially for Rust)
- Async safety and concurrency issues
- Memory safety and resource management
- Code organization and modularity
- Test coverage gaps
- Documentation completeness
- TODO/FIXME items that need attention

Respond in valid JSON format with the following structure for each file:
{{
  "path": "file/path.rs",
  "overall_score": 75,
  "security_score": 80,
  "quality_score": 70,
  "complexity_score": 40,
  "maintainability_score": 72,
  "summary": "Brief assessment...",
  "issues": [
    {{"severity": "high", "category": "security", "line": 42, "description": "...", "suggested_fix": "..."}}
  ],
  "improvements": [
    {{"priority": 1, "category": "error_handling", "description": "...", "effort": "low", "impact": "high"}}
  ],
  "patterns": [
    {{"name": "Builder Pattern", "is_positive": true, "description": "...", "occurrences": 2}}
  ],
  "dependencies": ["tokio", "serde"],
  "test_coverage": "Moderate - missing edge case tests"
}}

When analyzing multiple files, return a JSON array of file results."#,
            category_context
        )
    }

    /// Analyze a single file
    pub async fn analyze_file(&self, file: &FileForAnalysis) -> Result<FileAnalysisResult> {
        let start = std::time::Instant::now();

        let system_prompt = self.build_analysis_system_prompt(file.category);
        let user_prompt = format!(
            "Analyze this {} file:\n\nPath: {}\nLines: {}\n\n```\n{}\n```",
            file.category.display_name(),
            file.path,
            file.lines,
            file.content
        );

        let (response, token_usage) = self.call_api(&system_prompt, &user_prompt).await?;
        let processing_time = start.elapsed().as_millis() as u64;

        // Parse response
        let mut result = self.parse_single_file_response(&response, &file.path)?;
        result.tokens_used = token_usage;

        info!(
            "Analyzed {} in {}ms - Score: {:.0}",
            file.path, processing_time, result.overall_score
        );

        Ok(result)
    }

    /// Analyze a batch of files
    pub async fn analyze_batch(
        &self,
        batch: &FileBatch,
        cache: Option<&AuditCache>,
    ) -> Result<BatchAnalysisResult> {
        let start = std::time::Instant::now();

        info!(
            "Analyzing batch {} with {} files ({} estimated tokens)",
            batch.batch_id,
            batch.files.len(),
            batch.estimated_tokens
        );

        // Check cache for already-analyzed files
        let mut cached_results: Vec<FileAnalysisResult> = Vec::new();
        let mut files_to_analyze: Vec<&FileForAnalysis> = Vec::new();

        for file in &batch.files {
            if let Some(c) = cache {
                if let Ok(Some(entry)) = c.get(&file.path, &file.content) {
                    // Cache hit - parse stored analysis
                    if let Ok(result) = serde_json::from_value::<FileAnalysisResult>(entry.analysis)
                    {
                        tracing::debug!("Cache hit for: {}", file.path);
                        cached_results.push(result);
                        continue;
                    }
                }
            }
            files_to_analyze.push(file);
        }

        let mut all_results = cached_results;
        let mut total_tokens = TokenUsage::default();
        let tool_calls_count = 0;

        // Analyze uncached files
        if !files_to_analyze.is_empty() {
            let system_prompt = self.build_analysis_system_prompt(batch.category);

            let mut user_prompt = format!(
                "Analyze these {} {:?} files and return a JSON array of results:\n\n",
                files_to_analyze.len(),
                batch.category
            );

            for file in &files_to_analyze {
                user_prompt.push_str(&format!(
                    "--- File: {} ({} lines) ---\n```\n{}\n```\n\n",
                    file.path, file.lines, file.content
                ));
            }

            let (response, batch_token_usage) = self.call_api(&system_prompt, &user_prompt).await?;

            // Parse batch response
            let mut new_results = self.parse_batch_response(&response, &files_to_analyze)?;

            // Distribute token usage across files in batch (proportionally by content size)
            let total_content_size: usize = files_to_analyze.iter().map(|f| f.content.len()).sum();
            for (file, result) in files_to_analyze.iter().zip(new_results.iter_mut()) {
                let proportion = if total_content_size > 0 {
                    file.content.len() as f64 / total_content_size as f64
                } else {
                    1.0 / files_to_analyze.len() as f64
                };
                result.tokens_used = TokenUsage {
                    prompt_tokens: (batch_token_usage.prompt_tokens as f64 * proportion) as usize,
                    completion_tokens: (batch_token_usage.completion_tokens as f64 * proportion)
                        as usize,
                    reasoning_tokens: (batch_token_usage.reasoning_tokens as f64 * proportion)
                        as usize,
                    cached_tokens: (batch_token_usage.cached_tokens as f64 * proportion) as usize,
                    total_tokens: (batch_token_usage.total_tokens as f64 * proportion) as usize,
                };
            }

            // Cache new results
            if let Some(c) = cache {
                for (file, result) in files_to_analyze.iter().zip(new_results.iter()) {
                    if let Ok(analysis_json) = serde_json::to_value(result) {
                        let entry = CacheEntry {
                            file_path: file.path.clone(),
                            content_hash: file.content_hash.clone(),
                            analyzed_at: chrono::Utc::now().to_rfc3339(),
                            provider: "xai".to_string(),
                            model: self.model.clone(),
                            analysis: analysis_json,
                            tokens_used: Some(result.tokens_used.total_tokens),
                            file_size: file.content.len(),
                        };
                        let _ = c.set(file.path.clone(), entry);
                    }
                }
            }

            // Aggregate token usage
            for result in &new_results {
                total_tokens.prompt_tokens += result.tokens_used.prompt_tokens;
                total_tokens.completion_tokens += result.tokens_used.completion_tokens;
                total_tokens.reasoning_tokens += result.tokens_used.reasoning_tokens;
                total_tokens.cached_tokens += result.tokens_used.cached_tokens;
                total_tokens.total_tokens += result.tokens_used.total_tokens;
            }

            all_results.extend(new_results);
        }

        let processing_time = start.elapsed().as_millis() as u64;

        // Generate batch-level insights
        let batch_insights = if all_results.len() > 1 {
            Some(self.generate_batch_insights(&all_results))
        } else {
            None
        };

        Ok(BatchAnalysisResult {
            batch_id: batch.batch_id,
            file_results: all_results,
            batch_insights,
            total_tokens,
            processing_time_ms: processing_time,
            tool_calls_count,
        })
    }

    /// Generate insights across a batch of file results
    fn generate_batch_insights(&self, results: &[FileAnalysisResult]) -> String {
        let avg_score: f64 =
            results.iter().map(|r| r.overall_score).sum::<f64>() / results.len() as f64;
        let avg_security: f64 =
            results.iter().map(|r| r.security_score).sum::<f64>() / results.len() as f64;

        let critical_issues: usize = results
            .iter()
            .flat_map(|r| &r.issues)
            .filter(|i| i.severity == "critical")
            .count();

        let high_issues: usize = results
            .iter()
            .flat_map(|r| &r.issues)
            .filter(|i| i.severity == "high")
            .count();

        let common_patterns: HashMap<String, usize> = results
            .iter()
            .flat_map(|r| &r.patterns)
            .fold(HashMap::new(), |mut acc, p| {
                *acc.entry(p.name.clone()).or_insert(0) += 1;
                acc
            });

        let mut insight = format!("Batch Summary: {} files analyzed\n", results.len());
        insight.push_str(&format!("Average Score: {:.1}\n", avg_score));
        insight.push_str(&format!("Average Security: {:.1}\n", avg_security));
        insight.push_str(&format!(
            "Issues: {} critical, {} high\n",
            critical_issues, high_issues
        ));

        if !common_patterns.is_empty() {
            insight.push_str("Common Patterns: ");
            let patterns: Vec<_> = common_patterns
                .iter()
                .filter(|(_, count)| **count > 1)
                .map(|(name, count)| format!("{} (x{})", name, count))
                .collect();
            insight.push_str(&patterns.join(", "));
        }

        insight
    }

    /// Call the xAI Responses API with retry logic
    async fn call_api(
        &self,
        system_prompt: &str,
        user_prompt: &str,
    ) -> Result<(String, TokenUsage)> {
        let mut last_error: Option<AuditError> = None;

        for attempt in 0..=self.retry_config.max_retries {
            if attempt > 0 {
                let delay = self.retry_config.delay_for_attempt(attempt - 1);
                warn!(
                    "Retry attempt {}/{} after {:?} delay",
                    attempt, self.retry_config.max_retries, delay
                );
                sleep(delay).await;
            }

            match self.call_api_once(system_prompt, user_prompt).await {
                Ok(result) => {
                    if attempt > 0 {
                        info!("API call succeeded on retry attempt {}", attempt);
                    }
                    return Ok(result);
                }
                Err(e) => {
                    let error_str = e.to_string();
                    // Check if error is retryable
                    if Self::is_retryable_error(&error_str) {
                        warn!("Retryable error on attempt {}: {}", attempt, error_str);
                        last_error = Some(e);
                        continue;
                    } else {
                        // Non-retryable error, fail immediately
                        error!("Non-retryable error: {}", error_str);
                        return Err(e);
                    }
                }
            }
        }

        Err(last_error
            .unwrap_or_else(|| AuditError::other("API call failed after all retries".to_string())))
    }

    /// Check if an error is retryable
    fn is_retryable_error(error: &str) -> bool {
        let retryable_patterns = [
            "timeout",
            "connection",
            "temporarily unavailable",
            "rate limit",
            "429",
            "500",
            "502",
            "503",
            "504",
            "too many requests",
            "overloaded",
            "capacity",
        ];

        let error_lower = error.to_lowercase();
        retryable_patterns.iter().any(|p| error_lower.contains(p))
    }

    /// Single API call attempt (no retry)
    async fn call_api_once(
        &self,
        system_prompt: &str,
        user_prompt: &str,
    ) -> Result<(String, TokenUsage)> {
        let mut tools = Vec::new();

        if self.enable_code_execution {
            tools.push(Tool {
                tool_type: "code_execution".to_string(),
            });
        }

        let request = ResponsesRequest {
            model: self.model.clone(),
            input: vec![
                Message {
                    role: "system".to_string(),
                    content: system_prompt.to_string(),
                },
                Message {
                    role: "user".to_string(),
                    content: user_prompt.to_string(),
                },
            ],
            tools: if tools.is_empty() { None } else { Some(tools) },
            max_turns: Some(self.max_turns),
            max_tokens: Some(self.max_tokens),
            temperature: Some(self.temperature),
        };

        debug!("Sending API request to {}/responses", self.base_url);

        let response = self
            .client
            .post(format!("{}/responses", self.base_url))
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .json(&request)
            .send()
            .await
            .map_err(|e| AuditError::other(format!("API request failed: {}", e)))?;

        let status = response.status();
        if !status.is_success() {
            let body = response
                .text()
                .await
                .unwrap_or_else(|_| "Unknown error".to_string());
            return Err(AuditError::other(format!(
                "API error {} (retryable={}): {}",
                status,
                Self::is_retryable_error(&format!("{}", status.as_u16())),
                body
            )));
        }

        let response_body: ResponsesResponse = response
            .json()
            .await
            .map_err(|e| AuditError::other(format!("Failed to parse response: {}", e)))?;

        // Extract text content from response
        // Debug: log raw response structure
        tracing::debug!("API response output items: {}", response_body.output.len());
        for (i, item) in response_body.output.iter().enumerate() {
            tracing::debug!(
                "Output item {}: type={:?}, role={:?}, has_text={}, has_content={}",
                i,
                item.output_type,
                item.role,
                item.text.is_some(),
                item.content.is_some()
            );
        }

        let content = response_body
            .output
            .iter()
            .filter_map(|o| {
                // Try direct text field first (legacy)
                if let Some(text) = &o.text {
                    tracing::debug!("Found direct text field");
                    return Some(text.clone());
                }
                // Try content field
                if let Some(content) = &o.content {
                    match content {
                        OutputContent::Text(s) => {
                            tracing::debug!("Found content as text string");
                            return Some(s.clone());
                        }
                        OutputContent::Items(items) => {
                            tracing::debug!("Found content as {} items", items.len());
                            // Find the first text content item
                            for item in items {
                                if let Some(text) = &item.text {
                                    tracing::debug!(
                                        "Found text in content item: {} chars",
                                        text.len()
                                    );
                                    return Some(text.clone());
                                }
                            }
                        }
                    }
                }
                None
            })
            .next()
            .ok_or_else(|| AuditError::other("No content in response".to_string()))?;

        tracing::debug!("Extracted content length: {} chars", content.len());
        tracing::debug!("Content preview: {}", &content[..content.len().min(500)]);

        // Extract token usage from response
        let token_usage = if let Some(usage) = response_body.usage {
            TokenUsage {
                prompt_tokens: usage.input_tokens,
                completion_tokens: usage.output_tokens,
                reasoning_tokens: usage
                    .output_tokens_details
                    .map(|d| d.reasoning_tokens)
                    .unwrap_or(0),
                cached_tokens: usage
                    .input_tokens_details
                    .map(|d| d.cached_tokens)
                    .unwrap_or(0),
                total_tokens: usage.total_tokens,
            }
        } else {
            TokenUsage::default()
        };

        Ok((content, token_usage))
    }

    /// Parse response for a single file
    fn parse_single_file_response(&self, response: &str, path: &str) -> Result<FileAnalysisResult> {
        // Try to extract JSON from response
        let json_str = self.extract_json(response)?;

        serde_json::from_str(&json_str).map_err(|e| {
            AuditError::other(format!("Failed to parse file analysis for {}: {}", path, e))
        })
    }

    /// Parse response for multiple files
    fn parse_batch_response(
        &self,
        response: &str,
        files: &[&FileForAnalysis],
    ) -> Result<Vec<FileAnalysisResult>> {
        tracing::debug!(
            "Parsing batch response, raw length: {} chars",
            response.len()
        );

        let json_str = match self.extract_json(response) {
            Ok(s) => {
                tracing::debug!("Extracted JSON length: {} chars", s.len());
                tracing::debug!("JSON preview: {}", &s[..s.len().min(1000)]);
                s
            }
            Err(e) => {
                tracing::warn!("Failed to extract JSON: {}", e);
                tracing::debug!("Raw response: {}", &response[..response.len().min(2000)]);
                return Ok(files
                    .iter()
                    .map(|f| FileAnalysisResult {
                        path: f.path.clone(),
                        overall_score: 50.0,
                        security_score: 50.0,
                        quality_score: 50.0,
                        complexity_score: 50.0,
                        maintainability_score: 50.0,
                        summary: format!("JSON extraction failed: {}", e),
                        issues: vec![],
                        improvements: vec![],
                        patterns: vec![],
                        dependencies: vec![],
                        test_coverage: None,
                        reasoning_trace: Some(response.to_string()),
                        tokens_used: TokenUsage::default(),
                    })
                    .collect());
            }
        };

        // Try to parse as array first
        match serde_json::from_str::<Vec<FileAnalysisResult>>(&json_str) {
            Ok(results) => {
                tracing::debug!("Successfully parsed as array of {} results", results.len());
                return Ok(results);
            }
            Err(e) => {
                tracing::debug!("Failed to parse as array: {}", e);
            }
        }

        // Try to parse as single object and wrap in array
        match serde_json::from_str::<FileAnalysisResult>(&json_str) {
            Ok(result) => {
                tracing::debug!("Successfully parsed as single result");
                return Ok(vec![result]);
            }
            Err(e) => {
                tracing::debug!("Failed to parse as single object: {}", e);
            }
        }

        // Create default results if parsing fails
        warn!("Failed to parse batch response, creating defaults");
        tracing::debug!(
            "JSON that failed to parse: {}",
            &json_str[..json_str.len().min(2000)]
        );
        Ok(files
            .iter()
            .map(|f| FileAnalysisResult {
                path: f.path.clone(),
                overall_score: 50.0,
                security_score: 50.0,
                quality_score: 50.0,
                complexity_score: 50.0,
                maintainability_score: 50.0,
                summary: "Analysis parsing failed - manual review recommended".to_string(),
                issues: vec![],
                improvements: vec![],
                patterns: vec![],
                dependencies: vec![],
                test_coverage: None,
                reasoning_trace: None,
                tokens_used: TokenUsage::default(),
            })
            .collect())
    }

    /// Extract JSON from response (may be wrapped in markdown code blocks)
    fn extract_json(&self, response: &str) -> Result<String> {
        let trimmed = response.trim();

        // Check if it starts with JSON directly
        if trimmed.starts_with('{') || trimmed.starts_with('[') {
            return Ok(trimmed.to_string());
        }

        // Look for JSON in markdown code blocks
        if let Some(start) = trimmed.find("```json") {
            let json_start = start + 7;
            if let Some(end) = trimmed[json_start..].find("```") {
                return Ok(trimmed[json_start..json_start + end].trim().to_string());
            }
        }

        // Look for generic code blocks
        if let Some(start) = trimmed.find("```") {
            let json_start = trimmed[start + 3..].find('\n').map(|i| start + 3 + i + 1);
            if let Some(json_start) = json_start {
                if let Some(end) = trimmed[json_start..].find("```") {
                    return Ok(trimmed[json_start..json_start + end].trim().to_string());
                }
            }
        }

        // Try to find JSON object/array boundaries
        if let (Some(start), Some(end)) = (trimmed.find('{'), trimmed.rfind('}')) {
            if start < end {
                return Ok(trimmed[start..=end].to_string());
            }
        }

        if let (Some(start), Some(end)) = (trimmed.find('['), trimmed.rfind(']')) {
            if start < end {
                return Ok(trimmed[start..=end].to_string());
            }
        }

        Err(AuditError::other(
            "Could not extract JSON from response".to_string(),
        ))
    }

    /// Get model info
    pub fn model(&self) -> &str {
        &self.model
    }

    /// Get max turns setting
    pub fn max_turns(&self) -> usize {
        self.max_turns
    }

    /// Get retry config
    pub fn retry_config(&self) -> &RetryConfig {
        &self.retry_config
    }
}

/// Progress callback for batch analysis
pub type ProgressCallback = Box<dyn Fn(usize, usize, &str) + Send + Sync>;

/// Analyze multiple batches with progress reporting
pub async fn analyze_all_batches(
    client: &GrokReasoningClient,
    batches: Vec<FileBatch>,
    cache: Option<&AuditCache>,
    progress: Option<ProgressCallback>,
) -> Result<Vec<BatchAnalysisResult>> {
    let total_batches = batches.len();
    let mut results = Vec::new();

    for (i, batch) in batches.into_iter().enumerate() {
        if let Some(ref cb) = progress {
            cb(
                i + 1,
                total_batches,
                &format!(
                    "Analyzing batch {} ({} files)",
                    batch.batch_id,
                    batch.files.len()
                ),
            );
        }

        match client.analyze_batch(&batch, cache).await {
            Ok(result) => {
                info!(
                    "Batch {} complete: {} files in {}ms",
                    result.batch_id,
                    result.file_results.len(),
                    result.processing_time_ms
                );
                results.push(result);
            }
            Err(e) => {
                warn!("Batch {} failed: {}", batch.batch_id, e);
                // Continue with other batches
            }
        }
    }

    Ok(results)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_estimate_tokens() {
        let tokens = GrokReasoningClient::estimate_tokens("Hello world");
        assert!(tokens > 0);
    }

    #[test]
    fn test_retry_config_delay() {
        let config = RetryConfig {
            max_retries: 3,
            initial_delay_ms: 1000,
            exponential_backoff: true,
            max_delay_ms: 10000,
        };

        assert_eq!(config.delay_for_attempt(0), Duration::from_millis(1000));
        assert_eq!(config.delay_for_attempt(1), Duration::from_millis(2000));
        assert_eq!(config.delay_for_attempt(2), Duration::from_millis(4000));
        assert_eq!(config.delay_for_attempt(3), Duration::from_millis(8000));
        // Should cap at max_delay_ms
        assert_eq!(config.delay_for_attempt(4), Duration::from_millis(10000));
    }

    #[test]
    fn test_is_retryable_error() {
        assert!(GrokReasoningClient::is_retryable_error(
            "connection timeout"
        ));
        assert!(GrokReasoningClient::is_retryable_error(
            "rate limit exceeded"
        ));
        assert!(GrokReasoningClient::is_retryable_error("HTTP 429"));
        assert!(GrokReasoningClient::is_retryable_error(
            "503 Service Unavailable"
        ));
        assert!(!GrokReasoningClient::is_retryable_error("invalid API key"));
        assert!(!GrokReasoningClient::is_retryable_error(
            "malformed request"
        ));
    }

    #[test]
    fn test_create_batches_small_files() {
        let client = GrokReasoningClient {
            client: Client::new(),
            api_key: "test".to_string(),
            model: GROK_REASONING_MODEL.to_string(),
            base_url: "https://api.x.ai/v1".to_string(),
            max_tokens: 32000,
            temperature: 0.3,
            max_turns: 5,
            enable_code_execution: true,
            enable_reasoning: true,
            _timeout: Duration::from_secs(300),
            retry_config: RetryConfig::default(),
        };

        let files: Vec<FileForAnalysis> = (0..20)
            .map(|i| FileForAnalysis {
                path: format!("file{}.rs", i),
                content: "fn test() {}".repeat(5),
                lines: 50,
                score: None,
                category: FileCategory::Audit,
                content_hash: format!("hash{}", i),
            })
            .collect();

        let batches = client.create_batches(files, 100000);

        // Should create multiple batches for small files
        assert!(!batches.is_empty());
        for batch in &batches {
            assert!(batch.files.len() <= 15); // Small file batch limit
        }
    }

    #[test]
    fn test_extract_json_direct() {
        let client = GrokReasoningClient {
            client: Client::new(),
            api_key: "test".to_string(),
            model: GROK_REASONING_MODEL.to_string(),
            base_url: "https://api.x.ai/v1".to_string(),
            max_tokens: 32000,
            temperature: 0.3,
            max_turns: 5,
            enable_code_execution: true,
            enable_reasoning: true,
            _timeout: Duration::from_secs(300),
            retry_config: RetryConfig::default(),
        };

        let response = r#"{"score": 85}"#;
        let json = client.extract_json(response).unwrap();
        assert_eq!(json, r#"{"score": 85}"#);
    }

    #[test]
    fn test_extract_json_markdown() {
        let client = GrokReasoningClient {
            client: Client::new(),
            api_key: "test".to_string(),
            model: GROK_REASONING_MODEL.to_string(),
            base_url: "https://api.x.ai/v1".to_string(),
            max_tokens: 32000,
            temperature: 0.3,
            max_turns: 5,
            enable_code_execution: true,
            enable_reasoning: true,
            _timeout: Duration::from_secs(300),
            retry_config: RetryConfig::default(),
        };

        let response = r#"Here's the analysis:
```json
{"score": 85}
```
"#;
        let json = client.extract_json(response).unwrap();
        assert_eq!(json, r#"{"score": 85}"#);
    }

    #[test]
    fn test_file_category_debug() {
        assert_eq!(format!("{:?}", FileCategory::Audit), "Audit");
        assert_eq!(format!("{:?}", FileCategory::Janus), "Janus");
        assert_eq!(format!("{:?}", FileCategory::Clients), "Clients");
    }
}
