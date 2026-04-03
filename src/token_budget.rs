//! Token budget tracking and cost estimation
//!
//! This module provides utilities for tracking token usage across LLM API calls,
//! estimating costs, and managing budget constraints.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Token pricing per million tokens (in USD)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenPricing {
    /// Cost per million input tokens
    pub input_per_million: f64,
    /// Cost per million output tokens
    pub output_per_million: f64,
}

impl TokenPricing {
    /// xAI Grok pricing (as of 2024)
    pub fn grok() -> Self {
        Self {
            input_per_million: 5.0,   // $5 per 1M input tokens
            output_per_million: 15.0, // $15 per 1M output tokens
        }
    }

    /// OpenAI GPT-4 pricing
    pub fn gpt4() -> Self {
        Self {
            input_per_million: 30.0,
            output_per_million: 60.0,
        }
    }

    /// OpenAI GPT-3.5 Turbo pricing
    pub fn gpt35_turbo() -> Self {
        Self {
            input_per_million: 0.5,
            output_per_million: 1.5,
        }
    }

    /// Anthropic Claude 3.5 Sonnet pricing
    pub fn claude_sonnet() -> Self {
        Self {
            input_per_million: 3.0,
            output_per_million: 15.0,
        }
    }

    /// Google Gemini Pro pricing
    pub fn gemini_pro() -> Self {
        Self {
            input_per_million: 0.5,
            output_per_million: 1.5,
        }
    }

    /// Get pricing for a provider/model
    pub fn for_provider(provider: &str, model: &str) -> Self {
        let model_lower = model.to_lowercase();

        match provider.to_lowercase().as_str() {
            "xai" => Self::grok(),
            "openai" if model_lower.contains("gpt-4") => Self::gpt4(),
            "openai" if model_lower.contains("gpt-3.5") => Self::gpt35_turbo(),
            "openai" => Self::gpt35_turbo(), // Default OpenAI
            "anthropic" => Self::claude_sonnet(),
            "google" => Self::gemini_pro(),
            _ => {
                // Check model name for provider hints
                if model_lower.contains("grok") {
                    Self::grok()
                } else if model_lower.contains("gpt-4") {
                    Self::gpt4()
                } else if model_lower.contains("gpt") {
                    Self::gpt35_turbo()
                } else if model_lower.contains("claude") {
                    Self::claude_sonnet()
                } else if model_lower.contains("gemini") {
                    Self::gemini_pro()
                } else {
                    Self::grok() // Default to Grok pricing
                }
            }
        }
    }

    /// Calculate cost for token count (assuming average 50/50 split input/output)
    pub fn estimate_cost(&self, total_tokens: usize) -> f64 {
        let tokens = total_tokens as f64;
        let input_tokens = tokens * 0.5;
        let output_tokens = tokens * 0.5;

        (input_tokens / 1_000_000.0) * self.input_per_million
            + (output_tokens / 1_000_000.0) * self.output_per_million
    }

    /// Calculate cost with explicit input/output token counts
    pub fn calculate_cost(&self, input_tokens: usize, output_tokens: usize) -> f64 {
        (input_tokens as f64 / 1_000_000.0) * self.input_per_million
            + (output_tokens as f64 / 1_000_000.0) * self.output_per_million
    }
}

/// Token usage statistics
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenStats {
    /// Total tokens used
    pub total_tokens: usize,
    /// Estimated cost in USD
    pub estimated_cost: f64,
    /// Number of API calls
    pub api_calls: usize,
    /// Breakdown by model
    pub by_model: HashMap<String, ModelTokenStats>,
}

/// Token statistics per model
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelTokenStats {
    /// Total tokens for this model
    pub tokens: usize,
    /// Estimated cost for this model
    pub cost: f64,
    /// Number of calls
    pub calls: usize,
}

impl TokenStats {
    /// Create new empty stats
    pub fn new() -> Self {
        Self {
            total_tokens: 0,
            estimated_cost: 0.0,
            api_calls: 0,
            by_model: HashMap::new(),
        }
    }

    /// Add token usage for a model
    pub fn add_usage(&mut self, provider: &str, model: &str, tokens: usize) {
        let pricing = TokenPricing::for_provider(provider, model);
        let cost = pricing.estimate_cost(tokens);

        self.total_tokens += tokens;
        self.estimated_cost += cost;
        self.api_calls += 1;

        let model_stats =
            self.by_model
                .entry(model.to_string())
                .or_insert_with(|| ModelTokenStats {
                    tokens: 0,
                    cost: 0.0,
                    calls: 0,
                });

        model_stats.tokens += tokens;
        model_stats.cost += cost;
        model_stats.calls += 1;
    }

    /// Format statistics as a readable string
    pub fn format(&self) -> String {
        let mut output = String::new();
        output.push_str(&format!("Total tokens: {}\n", self.total_tokens));
        output.push_str(&format!("Estimated cost: ${:.4}\n", self.estimated_cost));
        output.push_str(&format!("API calls: {}\n", self.api_calls));

        if !self.by_model.is_empty() {
            output.push_str("\nBreakdown by model:\n");
            for (model, stats) in &self.by_model {
                output.push_str(&format!(
                    "  {}: {} tokens, ${:.4}, {} calls\n",
                    model, stats.tokens, stats.cost, stats.calls
                ));
            }
        }

        output
    }
}

impl Default for TokenStats {
    fn default() -> Self {
        Self::new()
    }
}

/// Budget configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BudgetConfig {
    /// Monthly budget in USD
    pub monthly_budget: f64,
    /// Warning threshold (percentage)
    pub warning_threshold: f64,
    /// Alert threshold (percentage)
    pub alert_threshold: f64,
}

impl Default for BudgetConfig {
    fn default() -> Self {
        Self {
            monthly_budget: 3.0,     // $3/month default
            warning_threshold: 0.75, // 75% warning
            alert_threshold: 0.90,   // 90% alert
        }
    }
}

impl BudgetConfig {
    /// Create a new budget configuration
    pub fn new(monthly_budget: f64) -> Self {
        Self {
            monthly_budget,
            ..Default::default()
        }
    }

    /// Check if spending is within budget
    pub fn check_spending(&self, current_spending: f64) -> BudgetStatus {
        let percentage = current_spending / self.monthly_budget;

        if percentage >= 1.0 {
            BudgetStatus::Exceeded {
                spent: current_spending,
                budget: self.monthly_budget,
                percentage,
            }
        } else if percentage >= self.alert_threshold {
            BudgetStatus::Alert {
                spent: current_spending,
                budget: self.monthly_budget,
                percentage,
            }
        } else if percentage >= self.warning_threshold {
            BudgetStatus::Warning {
                spent: current_spending,
                budget: self.monthly_budget,
                percentage,
            }
        } else {
            BudgetStatus::Ok {
                spent: current_spending,
                budget: self.monthly_budget,
                percentage,
            }
        }
    }

    /// Calculate remaining budget
    pub fn remaining(&self, current_spending: f64) -> f64 {
        (self.monthly_budget - current_spending).max(0.0)
    }

    /// Estimate tokens remaining in budget
    pub fn tokens_remaining(&self, current_spending: f64, pricing: &TokenPricing) -> usize {
        let remaining = self.remaining(current_spending);
        // Assume average cost per token
        let avg_cost_per_million = (pricing.input_per_million + pricing.output_per_million) / 2.0;
        ((remaining / avg_cost_per_million) * 1_000_000.0) as usize
    }
}

/// Budget status
#[derive(Debug, Clone)]
pub enum BudgetStatus {
    /// Within budget
    Ok {
        spent: f64,
        budget: f64,
        percentage: f64,
    },
    /// Warning threshold exceeded
    Warning {
        spent: f64,
        budget: f64,
        percentage: f64,
    },
    /// Alert threshold exceeded
    Alert {
        spent: f64,
        budget: f64,
        percentage: f64,
    },
    /// Budget exceeded
    Exceeded {
        spent: f64,
        budget: f64,
        percentage: f64,
    },
}

impl BudgetStatus {
    /// Get a colored emoji indicator
    pub fn emoji(&self) -> &str {
        match self {
            Self::Ok { .. } => "✅",
            Self::Warning { .. } => "⚠️",
            Self::Alert { .. } => "🔶",
            Self::Exceeded { .. } => "🚨",
        }
    }

    /// Get a status message
    pub fn message(&self) -> String {
        match self {
            Self::Ok {
                spent,
                budget,
                percentage,
            } => {
                format!(
                    "Budget OK: ${:.2} / ${:.2} ({:.1}%)",
                    spent,
                    budget,
                    percentage * 100.0
                )
            }
            Self::Warning {
                spent,
                budget,
                percentage,
            } => {
                format!(
                    "Budget Warning: ${:.2} / ${:.2} ({:.1}%)",
                    spent,
                    budget,
                    percentage * 100.0
                )
            }
            Self::Alert {
                spent,
                budget,
                percentage,
            } => {
                format!(
                    "Budget Alert: ${:.2} / ${:.2} ({:.1}%)",
                    spent,
                    budget,
                    percentage * 100.0
                )
            }
            Self::Exceeded {
                spent,
                budget,
                percentage,
            } => {
                format!(
                    "Budget Exceeded: ${:.2} / ${:.2} ({:.1}%)",
                    spent,
                    budget,
                    percentage * 100.0
                )
            }
        }
    }
}

/// Monthly spending tracker
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MonthlyTracker {
    /// Current month (YYYY-MM format)
    pub month: String,
    /// Token statistics for the month
    pub stats: TokenStats,
    /// When the tracking started
    pub started_at: DateTime<Utc>,
}

impl MonthlyTracker {
    /// Create a new monthly tracker
    pub fn new() -> Self {
        Self {
            month: Utc::now().format("%Y-%m").to_string(),
            stats: TokenStats::new(),
            started_at: Utc::now(),
        }
    }

    /// Check if this tracker is for the current month
    pub fn is_current_month(&self) -> bool {
        self.month == Utc::now().format("%Y-%m").to_string()
    }

    /// Add usage to the tracker (creates new tracker if month changed)
    pub fn add_usage(&mut self, provider: &str, model: &str, tokens: usize) {
        // Reset if month changed
        if !self.is_current_month() {
            *self = Self::new();
        }

        self.stats.add_usage(provider, model, tokens);
    }

    /// Get spending for the current month
    pub fn current_spending(&self) -> f64 {
        if self.is_current_month() {
            self.stats.estimated_cost
        } else {
            0.0
        }
    }
}

impl Default for MonthlyTracker {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_token_pricing() {
        let pricing = TokenPricing::grok();

        // Test with 1000 tokens (should be small cost)
        let cost = pricing.estimate_cost(1000);
        assert!(cost > 0.0);
        assert!(cost < 0.02); // Should be less than 2 cents

        // Test with 1 million tokens
        let cost = pricing.estimate_cost(1_000_000);
        assert!(cost > 5.0); // Should be at least $5 (input only)
        assert!(cost < 20.0); // Should be less than $20
    }

    #[test]
    fn test_token_stats() {
        let mut stats = TokenStats::new();

        stats.add_usage("xai", "grok-beta", 1000);
        stats.add_usage("xai", "grok-beta", 2000);

        assert_eq!(stats.total_tokens, 3000);
        assert_eq!(stats.api_calls, 2);
        assert!(stats.estimated_cost > 0.0);

        assert_eq!(stats.by_model.len(), 1);
        assert_eq!(stats.by_model["grok-beta"].tokens, 3000);
        assert_eq!(stats.by_model["grok-beta"].calls, 2);
    }

    #[test]
    fn test_budget_config() {
        let config = BudgetConfig::new(10.0);

        // Within budget
        match config.check_spending(5.0) {
            BudgetStatus::Ok { percentage, .. } => {
                assert_eq!(percentage, 0.5);
            }
            _ => panic!("Expected Ok status"),
        }

        // At warning threshold
        match config.check_spending(7.6) {
            BudgetStatus::Warning { .. } => {}
            _ => panic!("Expected Warning status"),
        }

        // Over budget
        match config.check_spending(12.0) {
            BudgetStatus::Exceeded { .. } => {}
            _ => panic!("Expected Exceeded status"),
        }
    }

    #[test]
    fn test_budget_remaining() {
        let config = BudgetConfig::new(10.0);

        assert_eq!(config.remaining(5.0), 5.0);
        assert_eq!(config.remaining(10.0), 0.0);
        assert_eq!(config.remaining(15.0), 0.0);
    }

    #[test]
    fn test_monthly_tracker() {
        let mut tracker = MonthlyTracker::new();

        tracker.add_usage("xai", "grok-beta", 1000);

        assert!(tracker.is_current_month());
        assert_eq!(tracker.stats.total_tokens, 1000);
        assert!(tracker.current_spending() > 0.0);
    }

    #[test]
    fn test_token_pricing_providers() {
        let grok = TokenPricing::for_provider("xai", "grok-beta");
        let gpt4 = TokenPricing::for_provider("openai", "gpt-4");
        let claude = TokenPricing::for_provider("anthropic", "claude-3.5-sonnet");

        // Grok should be cheaper than GPT-4
        assert!(grok.input_per_million < gpt4.input_per_million);

        // All should have positive pricing
        assert!(grok.input_per_million > 0.0);
        assert!(gpt4.output_per_million > 0.0);
        assert!(claude.input_per_million > 0.0);
    }
}
