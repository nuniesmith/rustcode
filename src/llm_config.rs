//! LLM configuration module
//!
//! Provides configuration for LLM-based audits with:
//! - Master enable/disable switch
//! - File selection criteria
//! - Cost limits and quotas
//! - Provider preferences

use crate::error::{AuditError, Result};
use serde::{Deserialize, Serialize};
use std::env;
use std::fs;
use std::path::Path;
use tracing::{info, warn};

/// LLM audit configuration file name
pub const LLM_CONFIG_FILE: &str = ".llm-audit.toml";

/// Configuration for LLM audits
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct LlmConfig {
    /// Master switch - enable/disable all LLM audits
    pub enabled: bool,

    /// File selection settings
    pub file_selection: FileSelectionConfig,

    /// Provider settings
    pub provider: ProviderConfig,

    /// Cost and quota settings
    pub limits: LimitsConfig,

    /// Cache settings
    pub cache: CacheConfig,
}

/// File selection configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileSelectionConfig {
    /// Maximum number of files to analyze per run
    pub max_files_per_run: usize,

    /// Minimum file importance score (0-100) to analyze
    pub min_importance_score: f64,

    /// Minimum file risk score (0-100) to analyze
    pub min_risk_score: f64,

    /// Analyze only files changed in last N commits
    pub changed_in_last_n_commits: Option<usize>,

    /// Skip files larger than N bytes
    pub max_file_size_bytes: usize,

    /// File patterns to exclude (glob patterns)
    pub exclude_patterns: Vec<String>,

    /// File patterns to include (glob patterns)
    pub include_patterns: Vec<String>,

    /// Prioritize files with these extensions
    pub priority_extensions: Vec<String>,
}

/// Provider configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderConfig {
    /// Default provider (xai, google, anthropic)
    pub default_provider: String,

    /// Default model name
    pub default_model: String,

    /// API key (can be overridden by env var)
    pub api_key: Option<String>,

    /// Max tokens per request
    pub max_tokens: usize,

    /// Temperature for LLM responses
    pub temperature: f64,
}

/// Cost and quota limits
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LimitsConfig {
    /// Maximum daily API calls
    pub max_daily_calls: Option<usize>,

    /// Maximum monthly cost in USD
    pub max_monthly_cost_usd: Option<f64>,

    /// Maximum tokens per day
    pub max_daily_tokens: Option<usize>,

    /// Maximum tokens per month
    pub max_monthly_tokens: Option<usize>,

    /// Warn when approaching limits (percentage)
    pub warn_threshold_pct: f64,

    /// Cost per 1M input tokens (USD) - for default provider
    pub cost_per_1m_input_tokens: f64,

    /// Cost per 1M output tokens (USD) - for default provider
    pub cost_per_1m_output_tokens: f64,

    /// Anthropic/Claude specific pricing (USD per 1M tokens)
    /// Claude Opus 4.5: $15 input, $75 output
    pub anthropic_cost_per_1m_input_tokens: Option<f64>,
    pub anthropic_cost_per_1m_output_tokens: Option<f64>,

    /// Maximum retries for API calls
    pub max_retries: usize,

    /// Retry delay in milliseconds
    pub retry_delay_ms: u64,

    /// Enable exponential backoff for retries
    pub exponential_backoff: bool,
}

/// Cache configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheConfig {
    /// Enable caching
    pub enabled: bool,

    /// Auto-prune stale entries older than N days
    pub auto_prune_days: Option<usize>,

    /// Maximum cache size in MB
    pub max_size_mb: Option<usize>,
}

impl Default for FileSelectionConfig {
    fn default() -> Self {
        Self {
            max_files_per_run: 50,
            min_importance_score: 50.0,
            min_risk_score: 40.0,
            changed_in_last_n_commits: None,
            max_file_size_bytes: 100_000, // 100KB
            exclude_patterns: vec![
                "**/target/**".to_string(),
                "**/node_modules/**".to_string(),
                "**/build/**".to_string(),
                "**/*.test.*".to_string(),
                "**/*_test.*".to_string(),
                "**/tests/**".to_string(),
            ],
            include_patterns: vec![
                "**/*.rs".to_string(),
                "**/*.kt".to_string(),
                "**/*.py".to_string(),
                "**/*.ts".to_string(),
                "**/*.tsx".to_string(),
            ],
            priority_extensions: vec!["rs".to_string(), "kt".to_string(), "py".to_string()],
        }
    }
}

impl Default for ProviderConfig {
    fn default() -> Self {
        Self {
            default_provider: "xai".to_string(),
            default_model: "grok-4-1-fast-reasoning".to_string(),
            api_key: None,
            max_tokens: 16000,
            temperature: 0.2,
        }
    }
}

/// Available Claude models for auditing
pub mod claude_models {
    /// Claude Opus 4.5 - Anthropic's most capable model
    /// Best for: deep analysis, whitepaper conformity, high-stakes auditing
    /// Context: 200K tokens, excellent reasoning capabilities
    pub const CLAUDE_OPUS_4_5: &str = "claude-opus-4-20250514";

    /// Claude Sonnet 4 - Balanced performance and cost
    /// Good for: routine audits, code review, general analysis
    pub const CLAUDE_SONNET_4: &str = "claude-sonnet-4-20250514";

    /// Claude Haiku 3.5 - Fast and cost-effective
    /// Good for: quick scans, simple checks, high-volume analysis
    pub const CLAUDE_HAIKU_3_5: &str = "claude-3-5-haiku-20241022";
}

impl Default for LimitsConfig {
    fn default() -> Self {
        Self {
            max_daily_calls: Some(200),
            max_monthly_cost_usd: Some(20.0), // $20 USD monthly budget
            max_daily_tokens: Some(2_000_000),
            max_monthly_tokens: Some(40_000_000), // ~$20 at Grok pricing
            warn_threshold_pct: 80.0,
            // Grok 4.1 Fast pricing (as of Jan 2025)
            cost_per_1m_input_tokens: 0.30,
            cost_per_1m_output_tokens: 0.50,
            // Claude Opus 4.5 pricing (as of 2025) - premium model for deep analysis
            anthropic_cost_per_1m_input_tokens: Some(15.0),
            anthropic_cost_per_1m_output_tokens: Some(75.0),
            max_retries: 3,
            retry_delay_ms: 1000,
            exponential_backoff: true,
        }
    }
}

impl Default for CacheConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            auto_prune_days: Some(30),
            max_size_mb: Some(100),
        }
    }
}

impl LlmConfig {
    /// Load configuration from file or create default
    pub fn load(project_root: &Path) -> Result<Self> {
        let config_path = project_root.join(LLM_CONFIG_FILE);

        if config_path.exists() {
            info!("Loading LLM config from: {}", config_path.display());
            let content = fs::read_to_string(&config_path)
                .map_err(|e| AuditError::other(format!("Failed to read LLM config: {}", e)))?;

            let config: Self = toml::from_str(&content)
                .map_err(|e| AuditError::other(format!("Failed to parse LLM config: {}", e)))?;

            if !config.enabled {
                info!("⚠️  LLM audits are DISABLED in config");
            } else {
                info!("✅ LLM audits are ENABLED");
            }

            Ok(config)
        } else {
            warn!(
                "No LLM config found at {}, using defaults (DISABLED)",
                config_path.display()
            );
            Ok(Self::default())
        }
    }

    /// Save configuration to file
    pub fn save(&self, project_root: &Path) -> Result<()> {
        let config_path = project_root.join(LLM_CONFIG_FILE);

        let content = toml::to_string_pretty(self)
            .map_err(|e| AuditError::other(format!("Failed to serialize LLM config: {}", e)))?;

        fs::write(&config_path, content)
            .map_err(|e| AuditError::other(format!("Failed to write LLM config: {}", e)))?;

        info!("LLM config saved to: {}", config_path.display());
        Ok(())
    }

    /// Create a new enabled configuration with sensible defaults
    pub fn enabled_default() -> Self {
        Self {
            enabled: true,
            ..Self::default()
        }
    }

    /// Get API key from config or environment
    /// Get the appropriate API key for the given provider
    pub fn get_api_key(&self) -> Result<String> {
        self.get_api_key_for_provider(&self.provider.default_provider)
    }

    /// Get API key for a specific provider
    pub fn get_api_key_for_provider(&self, provider: &str) -> Result<String> {
        // Determine which env var to check based on provider
        let env_var = match provider.to_lowercase().as_str() {
            "anthropic" | "claude" => "ANTHROPIC_API_KEY",
            "google" | "gemini" => "GOOGLE_API_KEY",
            "xai" | "grok" => "XAI_API_KEY",
            _ => "XAI_API_KEY",
        };

        // Try environment variable first
        if let Ok(key) = std::env::var(env_var) {
            if !key.is_empty() {
                return Ok(key);
            }
        }

        // Fall back to config file
        if let Some(ref key) = self.provider.api_key {
            if !key.is_empty() {
                return Ok(key.clone());
            }
        }

        Err(crate::error::AuditError::config(format!(
            "No API key found. Set {} environment variable or api_key in .llm-audit.toml",
            env_var
        )))
    }

    /// Get API key (deprecated - use get_api_key or get_api_key_for_provider)
    #[deprecated(note = "Use get_api_key() or get_api_key_for_provider() instead")]
    pub fn get_api_key_legacy(&self) -> Result<String> {
        // First check environment variable
        let env_key = match self.provider.default_provider.as_str() {
            "google" | "gemini" => "GOOGLE_API_KEY",
            "xai" | "grok" => "XAI_API_KEY",
            _ => "LLM_API_KEY",
        };

        if let Ok(key) = env::var(env_key) {
            return Ok(key);
        }

        // Then check config file
        if let Some(ref key) = self.provider.api_key {
            return Ok(key.clone());
        }

        Err(AuditError::other(format!(
            "No API key found. Set {} environment variable or add to config",
            env_key
        )))
    }

    /// Check if file should be analyzed based on selection criteria
    pub fn should_analyze_file(
        &self,
        file_path: &Path,
        file_size: usize,
        importance_score: f64,
        risk_score: f64,
    ) -> bool {
        // Check file size
        if file_size > self.file_selection.max_file_size_bytes {
            return false;
        }

        // Check scores
        if importance_score < self.file_selection.min_importance_score
            && risk_score < self.file_selection.min_risk_score
        {
            return false;
        }

        let path_str = file_path.to_string_lossy();

        // Check exclude patterns
        for pattern in &self.file_selection.exclude_patterns {
            if glob_match(pattern, &path_str) {
                return false;
            }
        }

        // Check include patterns (if any specified)
        if !self.file_selection.include_patterns.is_empty() {
            let mut matches = false;
            for pattern in &self.file_selection.include_patterns {
                if glob_match(pattern, &path_str) {
                    matches = true;
                    break;
                }
            }
            if !matches {
                return false;
            }
        }

        true
    }

    /// Print configuration summary
    /// Check if using Anthropic/Claude provider
    pub fn is_anthropic(&self) -> bool {
        matches!(
            self.provider.default_provider.to_lowercase().as_str(),
            "anthropic" | "claude"
        )
    }

    /// Get the cost per 1M input tokens for the current provider
    pub fn get_input_cost_per_1m(&self) -> f64 {
        if self.is_anthropic() {
            self.limits
                .anthropic_cost_per_1m_input_tokens
                .unwrap_or(15.0)
        } else {
            self.limits.cost_per_1m_input_tokens
        }
    }

    /// Get the cost per 1M output tokens for the current provider
    pub fn get_output_cost_per_1m(&self) -> f64 {
        if self.is_anthropic() {
            self.limits
                .anthropic_cost_per_1m_output_tokens
                .unwrap_or(75.0)
        } else {
            self.limits.cost_per_1m_output_tokens
        }
    }

    pub fn print_summary(&self) {
        println!("\n⚙️  LLM Audit Configuration");
        println!(
            "  Status: {}",
            if self.enabled {
                "✅ ENABLED"
            } else {
                "🔴 DISABLED"
            }
        );
        println!("  Provider: {}", self.provider.default_provider);
        println!("  Model: {}", self.provider.default_model);
        println!("  Max Files/Run: {}", self.file_selection.max_files_per_run);
        println!(
            "  Min Importance: {:.0}",
            self.file_selection.min_importance_score
        );
        println!("  Min Risk: {:.0}", self.file_selection.min_risk_score);
        println!(
            "  Cache: {}",
            if self.cache.enabled {
                "enabled"
            } else {
                "disabled"
            }
        );

        if let Some(max_calls) = self.limits.max_daily_calls {
            println!("  Daily Limit: {} calls", max_calls);
        }

        if let Some(max_cost) = self.limits.max_monthly_cost_usd {
            println!("  Monthly Budget: ${:.2}", max_cost);
        }

        if let Some(max_tokens) = self.limits.max_monthly_tokens {
            println!("  Monthly Token Limit: {}M", max_tokens / 1_000_000);
        }

        println!(
            "  Pricing: ${:.2}/1M input, ${:.2}/1M output",
            self.limits.cost_per_1m_input_tokens, self.limits.cost_per_1m_output_tokens
        );
        println!(
            "  Retry: {} attempts, {}ms delay, backoff={}",
            self.limits.max_retries, self.limits.retry_delay_ms, self.limits.exponential_backoff
        );
    }

    /// Calculate estimated cost for token usage
    pub fn estimate_cost(&self, input_tokens: usize, output_tokens: usize) -> f64 {
        let input_cost = (input_tokens as f64 / 1_000_000.0) * self.limits.cost_per_1m_input_tokens;
        let output_cost =
            (output_tokens as f64 / 1_000_000.0) * self.limits.cost_per_1m_output_tokens;
        input_cost + output_cost
    }

    /// Check if we're within budget
    pub fn check_budget(&self, current_cost: f64) -> BudgetStatus {
        if let Some(max_cost) = self.limits.max_monthly_cost_usd {
            let usage_pct = (current_cost / max_cost) * 100.0;
            if usage_pct >= 100.0 {
                return BudgetStatus::Exceeded {
                    current: current_cost,
                    limit: max_cost,
                };
            } else if usage_pct >= self.limits.warn_threshold_pct {
                return BudgetStatus::Warning {
                    current: current_cost,
                    limit: max_cost,
                    usage_pct,
                };
            }
        }
        BudgetStatus::Ok
    }
}

/// Budget status for cost tracking
#[derive(Debug, Clone)]
pub enum BudgetStatus {
    /// Within budget
    Ok,
    /// Approaching limit
    Warning {
        current: f64,
        limit: f64,
        usage_pct: f64,
    },
    /// Budget exceeded
    Exceeded { current: f64, limit: f64 },
}

impl BudgetStatus {
    pub fn is_ok(&self) -> bool {
        matches!(self, BudgetStatus::Ok)
    }

    pub fn is_exceeded(&self) -> bool {
        matches!(self, BudgetStatus::Exceeded { .. })
    }
}

/// Simple glob pattern matching (basic implementation)
fn glob_match(pattern: &str, path: &str) -> bool {
    // Handle ** for recursive matching
    if pattern.contains("**") {
        let parts: Vec<&str> = pattern.split("**").collect();
        if parts.len() == 2 {
            let prefix = parts[0];
            let suffix = parts[1].trim_start_matches('/');

            if !prefix.is_empty() && !path.starts_with(prefix) {
                return false;
            }

            if !suffix.is_empty() {
                // Simple suffix matching
                if suffix.starts_with('*') {
                    let ext = suffix.trim_start_matches('*');
                    return path.ends_with(ext);
                } else {
                    return path.contains(suffix);
                }
            }

            return true;
        }
    }

    // Handle simple * wildcard
    if pattern.contains('*') {
        let parts: Vec<&str> = pattern.split('*').collect();
        let mut pos = 0;

        for (i, part) in parts.iter().enumerate() {
            if part.is_empty() {
                continue;
            }

            if i == 0 && !path[pos..].starts_with(part) {
                return false;
            }

            if let Some(found_pos) = path[pos..].find(part) {
                pos += found_pos + part.len();
            } else {
                return false;
            }
        }

        return true;
    }

    // Exact match
    pattern == path
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config_disabled() {
        let config = LlmConfig::default();
        assert!(!config.enabled);
        assert!(config.cache.enabled);
    }

    #[test]
    fn test_enabled_default() {
        let config = LlmConfig::enabled_default();
        assert!(config.enabled);
    }

    #[test]
    fn test_glob_match() {
        assert!(glob_match("**/*.rs", "src/main.rs"));
        assert!(glob_match("**/*.rs", "foo/bar/baz.rs"));
        assert!(!glob_match("**/*.rs", "foo/bar/baz.kt"));

        assert!(glob_match("**/target/**", "project/target/debug/file"));
        assert!(!glob_match("**/target/**", "project/src/file"));

        assert!(glob_match("*.test.rs", "foo.test.rs"));
        assert!(!glob_match("*.test.rs", "foo.rs"));
    }

    #[test]
    fn test_should_analyze_file() {
        let config = LlmConfig::default();

        // Should reject large files
        assert!(!config.should_analyze_file(
            Path::new("test.rs"),
            200_000, // Too large (max is 100_000)
            80.0,
            70.0
        ));

        // Should reject when BOTH scores are below thresholds
        // Default: min_importance_score=50.0, min_risk_score=40.0
        // Condition: importance < 50 AND risk < 40 => reject
        assert!(!config.should_analyze_file(
            Path::new("test.rs"),
            1000,
            30.0, // Below min_importance_score (50)
            30.0  // Below min_risk_score (40)
        ));

        // Should accept when importance meets threshold (even if risk is low)
        assert!(config.should_analyze_file(
            Path::new("test.rs"),
            1000,
            50.0, // Meets min_importance_score
            30.0  // Below min_risk_score, but importance passes
        ));

        // Should accept when risk meets threshold (even if importance is low)
        assert!(config.should_analyze_file(
            Path::new("test.rs"),
            1000,
            30.0, // Below min_importance_score
            40.0  // Meets min_risk_score
        ));

        // Should reject excluded patterns (path needs prefix for **/target/** to match)
        assert!(!config.should_analyze_file(
            Path::new("project/target/debug/test.rs"),
            1000,
            80.0,
            70.0
        ));

        // Should accept good candidates
        assert!(config.should_analyze_file(Path::new("src/main.rs"), 1000, 80.0, 70.0));
    }
}
