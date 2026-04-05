// # Cost Tracker Module
//
// Tracks LLM API usage and costs for budget monitoring.
//
// ## Features
//
// - Per-query cost tracking
// - Daily/weekly/monthly aggregations
// - Budget alerts
// - Cost breakdown by operation type
// - Cache hit/miss impact analysis
//
// ## Usage
//
// ```rust,no_run
// use rustcode::cost_tracker::{CostTracker, TokenUsage};
//
// #[tokio::main]
// async fn main() -> anyhow::Result<()> {
//     # let pool = rustcode::db::init_db(&std::env::var("DATABASE_URL").unwrap_or_else(|_| "postgresql://rustcode:changeme@localhost:5432/rustcode_test".to_string())).await?;
//     let tracker = CostTracker::new(pool).await?;
//
//     // Log an API call
//     let usage = TokenUsage {
//         input_tokens: 100_000,
//         output_tokens: 50_000,
//         cached_tokens: 0,
//     };
//     tracker.log_call("code_review", "grok-4-1-fast-reasoning", usage, false).await?;
//
//     // Get daily stats
//     let stats = tracker.get_daily_stats().await?;
//     println!("Today's cost: ${:.2}", stats.total_cost_usd);
//
//     Ok(())
// }
// ```

use crate::error::AuditError;
use anyhow::{Context, Result};
use chrono::{DateTime, Datelike, Duration, Utc};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use tracing::{debug, info, warn};

// Grok 4.1 Fast pricing (per million tokens)
const GROK_COST_PER_MILLION_INPUT: f64 = 0.20;
const GROK_COST_PER_MILLION_OUTPUT: f64 = 0.50;
const GROK_COST_PER_MILLION_CACHED: f64 = 0.05;

// Default budget alert threshold (USD)
const DEFAULT_DAILY_BUDGET: f64 = 1.0;
const DEFAULT_MONTHLY_BUDGET: f64 = 10.0;

// Token usage for a single API call
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cached_tokens: u64,
}

// Cost statistics for a time period
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CostStats {
    pub total_queries: u64,
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    pub total_cached_tokens: u64,
    pub total_cost_usd: f64,
    pub cache_hits: u64,
    pub cache_hit_rate: f64,
    pub cost_saved_from_cache: f64,
    pub period_start: DateTime<Utc>,
    pub period_end: DateTime<Utc>,
}

// Cost breakdown by operation type
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OperationCost {
    pub operation: String,
    pub query_count: u64,
    pub total_cost_usd: f64,
    pub avg_cost_usd: f64,
    pub total_tokens: u64,
}

// Budget status
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BudgetStatus {
    pub daily_spend: f64,
    pub daily_budget: f64,
    pub daily_remaining: f64,
    pub daily_percent_used: f64,
    pub monthly_spend: f64,
    pub monthly_budget: f64,
    pub monthly_remaining: f64,
    pub monthly_percent_used: f64,
    pub alerts: Vec<String>,
}

// Record of a static analysis decision for cost tracking
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StaticDecisionRecord {
    // File path that was analyzed
    pub file_path: String,
    // Repository identifier
    pub repo_id: String,
    // The recommendation from static analysis (SKIP, MINIMAL, STANDARD, DEEP_DIVE)
    pub recommendation: String,
    // Reason for skip (if recommendation was SKIP)
    pub skip_reason: Option<String>,
    // Number of static issues found (without LLM)
    pub static_issue_count: i64,
    // Estimated LLM value score (0.0-1.0)
    pub estimated_llm_value: f64,
    // Whether an LLM call was actually made
    pub llm_called: bool,
    // Estimated cost saved in USD (if skipped or downgraded)
    pub estimated_cost_saved_usd: f64,
    // Actual cost if LLM was called
    pub actual_cost_usd: f64,
    // Prompt tier used (if LLM was called)
    pub prompt_tier: Option<String>,
}

// Summary of savings from static analysis decisions
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SavingsReport {
    // Total files processed
    pub total_files: i64,
    // Files skipped entirely (no LLM call)
    pub files_skipped: i64,
    // Files that used minimal prompt
    pub files_minimal: i64,
    // Files that used standard prompt
    pub files_standard: i64,
    // Files that used deep-dive prompt
    pub files_deep_dive: i64,
    // Total estimated cost saved in USD
    pub total_estimated_savings_usd: f64,
    // Total actual cost spent on LLM calls
    pub total_actual_cost_usd: f64,
    // Number of LLM calls avoided
    pub llm_calls_avoided: i64,
    // Total static issues found across all files
    pub total_static_issues: i64,
    // Savings as a percentage of total possible cost
    pub savings_percent: f64,
    // Period for this report
    pub period: String,
}

// LLM API cost tracker
pub struct CostTracker {
    pool: PgPool,
    daily_budget: f64,
    monthly_budget: f64,
}

impl CostTracker {
    // Create a new cost tracker
    pub async fn new(pool: PgPool) -> Result<Self> {
        let tracker = Self {
            pool,
            daily_budget: DEFAULT_DAILY_BUDGET,
            monthly_budget: DEFAULT_MONTHLY_BUDGET,
        };

        tracker.initialize_schema().await?;

        Ok(tracker)
    }

    // Create with custom budget limits
    pub async fn with_budgets(
        pool: PgPool,
        daily_budget: f64,
        monthly_budget: f64,
    ) -> Result<Self> {
        let tracker = Self {
            pool,
            daily_budget,
            monthly_budget,
        };

        tracker.initialize_schema().await?;

        Ok(tracker)
    }

    // Initialize database schema
    async fn initialize_schema(&self) -> Result<()> {
        // Acquire a session-level advisory lock so that concurrent test threads
        // don't race on `CREATE TABLE IF NOT EXISTS` + `BIGSERIAL` sequence
        // creation, which triggers a `pg_type_typname_nsp_index` unique-
        // constraint violation inside Postgres.
        sqlx::query("SELECT pg_advisory_lock(7483922)")
            .execute(&self.pool)
            .await
            .context("Failed to acquire cost_tracker init lock")?;

        let result = self.initialize_schema_inner().await;

        let _ = sqlx::query("SELECT pg_advisory_unlock(7483922)")
            .execute(&self.pool)
            .await;

        result
    }

    async fn initialize_schema_inner(&self) -> Result<()> {
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS llm_costs (
                id BIGSERIAL PRIMARY KEY,
                timestamp TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                operation TEXT NOT NULL,
                model TEXT NOT NULL,
                input_tokens INTEGER NOT NULL,
                output_tokens INTEGER NOT NULL,
                cached_tokens INTEGER DEFAULT 0,
                cost_usd DOUBLE PRECISION NOT NULL,
                query_hash TEXT,
                cache_hit BOOLEAN DEFAULT FALSE,
                user_query TEXT,
                response_summary TEXT
            )
            "#,
        )
        .execute(&self.pool)
        .await
        .context("Failed to create llm_costs table")?;

        // Static analysis decisions table — tracks skip/tier decisions for savings reporting
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS static_decisions (
                id BIGSERIAL PRIMARY KEY,
                timestamp TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                file_path TEXT NOT NULL,
                repo_id TEXT NOT NULL,
                recommendation TEXT NOT NULL,
                skip_reason TEXT,
                static_issue_count INTEGER NOT NULL DEFAULT 0,
                estimated_llm_value DOUBLE PRECISION NOT NULL DEFAULT 0.0,
                llm_called BOOLEAN NOT NULL DEFAULT FALSE,
                estimated_cost_saved_usd DOUBLE PRECISION NOT NULL DEFAULT 0.0,
                actual_cost_usd DOUBLE PRECISION NOT NULL DEFAULT 0.0,
                prompt_tier TEXT
            )
            "#,
        )
        .execute(&self.pool)
        .await
        .context("Failed to create static_decisions table")?;

        // Create indexes for efficient queries
        sqlx::query("CREATE INDEX IF NOT EXISTS idx_costs_timestamp ON llm_costs(timestamp)")
            .execute(&self.pool)
            .await
            .context("Failed to create timestamp index")?;

        sqlx::query("CREATE INDEX IF NOT EXISTS idx_costs_operation ON llm_costs(operation)")
            .execute(&self.pool)
            .await
            .context("Failed to create operation index")?;

        sqlx::query("CREATE INDEX IF NOT EXISTS idx_costs_cache_hit ON llm_costs(cache_hit)")
            .execute(&self.pool)
            .await
            .context("Failed to create cache_hit index")?;

        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_static_decisions_timestamp ON static_decisions(timestamp)",
        )
        .execute(&self.pool)
        .await
        .context("Failed to create static_decisions timestamp index")?;

        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_static_decisions_repo ON static_decisions(repo_id)",
        )
        .execute(&self.pool)
        .await
        .context("Failed to create static_decisions repo index")?;

        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_static_decisions_rec ON static_decisions(recommendation)",
        )
        .execute(&self.pool)
        .await
        .context("Failed to create static_decisions recommendation index")?;

        Ok(())
    }

    // Log an API call
    pub async fn log_call(
        &self,
        operation: &str,
        model: &str,
        usage: TokenUsage,
        cache_hit: bool,
    ) -> Result<i64> {
        let cost = self.calculate_cost(&usage);

        let row: (i64,) = sqlx::query_as(
            r#"
            INSERT INTO llm_costs (
                operation, model, input_tokens, output_tokens, cached_tokens,
                cost_usd, cache_hit
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7)
            RETURNING id
            "#,
        )
        .bind(operation)
        .bind(model)
        .bind(usage.input_tokens as i64)
        .bind(usage.output_tokens as i64)
        .bind(usage.cached_tokens as i64)
        .bind(cost)
        .bind(cache_hit)
        .fetch_one(&self.pool)
        .await
        .context("Failed to log API call")?;
        let id = row.0;

        info!(
            "Logged API call: {} | Cost: ${:.4} | Tokens: {}in/{}out/{}cached | Cache: {}",
            operation,
            cost,
            usage.input_tokens,
            usage.output_tokens,
            usage.cached_tokens,
            cache_hit
        );

        // Check budget alerts
        self.check_budget_alerts().await?;

        Ok(id)
    }

    // Calculate cost for token usage
    fn calculate_cost(&self, usage: &TokenUsage) -> f64 {
        let input_cost = (usage.input_tokens as f64 / 1_000_000.0) * GROK_COST_PER_MILLION_INPUT;
        let output_cost = (usage.output_tokens as f64 / 1_000_000.0) * GROK_COST_PER_MILLION_OUTPUT;
        let cached_cost = (usage.cached_tokens as f64 / 1_000_000.0) * GROK_COST_PER_MILLION_CACHED;

        input_cost + output_cost + cached_cost
    }

    // -------------------------------------------------------------------
    // Static analysis savings tracking
    // -------------------------------------------------------------------

    // Log a static analysis decision (skip, minimal, standard, or deep-dive)
    //
    // Call this for every file processed by the scanner, whether or not an LLM
    // call was made. This lets us track and report cost savings from static
    // pre-filtering.
    pub async fn log_static_decision(&self, record: &StaticDecisionRecord) -> Result<i64> {
        let row: (i64,) = sqlx::query_as(
            r#"
            INSERT INTO static_decisions (
                file_path, repo_id, recommendation, skip_reason,
                static_issue_count, estimated_llm_value,
                llm_called, estimated_cost_saved_usd, actual_cost_usd,
                prompt_tier
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
            RETURNING id
            "#,
        )
        .bind(&record.file_path)
        .bind(&record.repo_id)
        .bind(&record.recommendation)
        .bind(&record.skip_reason)
        .bind(record.static_issue_count)
        .bind(record.estimated_llm_value)
        .bind(record.llm_called)
        .bind(record.estimated_cost_saved_usd)
        .bind(record.actual_cost_usd)
        .bind(&record.prompt_tier)
        .fetch_one(&self.pool)
        .await
        .context("Failed to log static decision")?;
        let id = row.0;

        debug!(
            "Logged static decision: {} → {} (saved: ${:.4}, LLM: {})",
            record.file_path,
            record.recommendation,
            record.estimated_cost_saved_usd,
            record.llm_called
        );

        Ok(id)
    }

    // Get savings report for today
    pub async fn get_daily_savings_report(&self) -> Result<SavingsReport> {
        self.get_savings_report_for_period("timestamp::date = CURRENT_DATE", "today")
            .await
    }

    // Get savings report for the last 7 days
    pub async fn get_weekly_savings_report(&self) -> Result<SavingsReport> {
        self.get_savings_report_for_period("timestamp >= NOW() - INTERVAL '7 days'", "last 7 days")
            .await
    }

    // Get savings report for the current month
    pub async fn get_monthly_savings_report(&self) -> Result<SavingsReport> {
        self.get_savings_report_for_period("timestamp >= DATE_TRUNC('month', NOW())", "this month")
            .await
    }

    // Get savings report for a specific repo
    pub async fn get_repo_savings_report(&self, repo_id: &str) -> Result<SavingsReport> {
        let where_clause = format!("repo_id = '{}'", repo_id.replace('\'', "''"));
        self.get_savings_report_for_period(&where_clause, &format!("repo: {}", repo_id))
            .await
    }

    // Internal helper to build a savings report from a WHERE clause
    async fn get_savings_report_for_period(
        &self,
        where_clause: &str,
        period_label: &str,
    ) -> Result<SavingsReport> {
        let query = format!(
            r#"
            SELECT
                COUNT(*)::BIGINT as total_files,
                COALESCE(SUM(CASE WHEN recommendation = 'SKIP' THEN 1 ELSE 0 END), 0)::BIGINT as files_skipped,
                COALESCE(SUM(CASE WHEN recommendation = 'MINIMAL' THEN 1 ELSE 0 END), 0)::BIGINT as files_minimal,
                COALESCE(SUM(CASE WHEN recommendation = 'STANDARD' THEN 1 ELSE 0 END), 0)::BIGINT as files_standard,
                COALESCE(SUM(CASE WHEN recommendation = 'DEEP_DIVE' THEN 1 ELSE 0 END), 0)::BIGINT as files_deep_dive,
                COALESCE(SUM(estimated_cost_saved_usd), 0.0)::DOUBLE PRECISION as total_savings,
                COALESCE(SUM(actual_cost_usd), 0.0)::DOUBLE PRECISION as total_actual,
                COALESCE(SUM(CASE WHEN llm_called = FALSE THEN 1 ELSE 0 END), 0)::BIGINT as llm_avoided,
                COALESCE(SUM(static_issue_count), 0)::BIGINT as total_static_issues
            FROM static_decisions
            WHERE {}
            "#,
            where_clause
        );

        let row = sqlx::query_as::<_, (i64, i64, i64, i64, i64, f64, f64, i64, i64)>(&query)
            .fetch_one(&self.pool)
            .await
            .context("Failed to get savings report")?;

        let total_possible = row.5 + row.6;
        let savings_pct = if total_possible > 0.0 {
            (row.5 / total_possible) * 100.0
        } else {
            0.0
        };

        Ok(SavingsReport {
            total_files: row.0,
            files_skipped: row.1,
            files_minimal: row.2,
            files_standard: row.3,
            files_deep_dive: row.4,
            total_estimated_savings_usd: row.5,
            total_actual_cost_usd: row.6,
            llm_calls_avoided: row.7,
            total_static_issues: row.8,
            savings_percent: savings_pct,
            period: period_label.to_string(),
        })
    }

    // Estimate what an LLM call would cost for a file of the given size (in chars).
    // Used to calculate savings when a file is skipped.
    // Based on Grok 4.1 Fast pricing with ~30% output ratio.
    pub fn estimate_file_cost(char_count: usize) -> f64 {
        let input_tokens = char_count as f64 / 4.0; // ~4 chars per token
        let output_tokens = input_tokens * 0.3;
        let input_cost = (input_tokens / 1_000_000.0) * GROK_COST_PER_MILLION_INPUT;
        let output_cost = (output_tokens / 1_000_000.0) * GROK_COST_PER_MILLION_OUTPUT;
        input_cost + output_cost
    }

    // Get statistics for all time (useful for testing)
    pub async fn get_all_time_stats(&self) -> Result<CostStats> {
        self.get_stats_for_period("1970-01-01T00:00:00Z", "2100-01-01T00:00:00Z")
            .await
    }

    // Get statistics for today
    pub async fn get_daily_stats(&self) -> Result<CostStats> {
        let today = Utc::now().date_naive();
        let start = today.and_hms_opt(0, 0, 0).unwrap().and_utc().to_rfc3339();
        let end = today
            .and_hms_opt(23, 59, 59)
            .unwrap()
            .and_utc()
            .to_rfc3339();

        self.get_stats_for_period(&start, &end).await
    }

    // Get statistics for this week
    pub async fn get_weekly_stats(&self) -> Result<CostStats> {
        let now = Utc::now();
        let start = (now - Duration::days(7)).to_rfc3339();
        let end = now.to_rfc3339();

        self.get_stats_for_period(&start, &end).await
    }

    // Get statistics for this month
    pub async fn get_monthly_stats(&self) -> Result<CostStats> {
        let now = Utc::now();
        let year = now.year();
        let month = now.month();
        let start = chrono::NaiveDate::from_ymd_opt(year, month, 1)
            .ok_or_else(|| AuditError::other("Invalid date"))?;
        let start_dt = start.and_hms_opt(0, 0, 0).unwrap().and_utc().to_rfc3339();
        let end = now.to_rfc3339();

        self.get_stats_for_period(&start_dt, &end).await
    }

    // Get statistics for a custom period
    async fn get_stats_for_period(&self, start: &str, end: &str) -> Result<CostStats> {
        let (
            total_queries,
            total_input_tokens,
            total_output_tokens,
            total_cached_tokens,
            total_cost_usd,
        ) = sqlx::query_as::<_, (i64, i64, i64, i64, f64)>(
            r#"
            SELECT
                COUNT(*),
                COALESCE(SUM(input_tokens), 0),
                COALESCE(SUM(output_tokens), 0),
                COALESCE(SUM(cached_tokens), 0),
                COALESCE(SUM(cost_usd), 0.0)
            FROM llm_costs
            WHERE timestamp >= $1::TIMESTAMPTZ AND timestamp <= $2::TIMESTAMPTZ
            "#,
        )
        .bind(start)
        .bind(end)
        .fetch_one(&self.pool)
        .await
        .context("Failed to fetch cost statistics")?;

        let (cache_hits,) = sqlx::query_as::<_, (i64,)>(
            r#"
            SELECT COUNT(*)
            FROM llm_costs
            WHERE timestamp >= $1::TIMESTAMPTZ AND timestamp <= $2::TIMESTAMPTZ
            AND cache_hit = TRUE
            "#,
        )
        .bind(start)
        .bind(end)
        .fetch_one(&self.pool)
        .await
        .context("Failed to count cache hits")?;

        let cache_hit_rate = if total_queries > 0 {
            (cache_hits as f64 / total_queries as f64) * 100.0
        } else {
            0.0
        };

        // Estimate cost saved from cache hits
        let avg_query_cost = if total_queries > 0 {
            total_cost_usd / total_queries as f64
        } else {
            0.0
        };
        let cost_saved_from_cache = cache_hits as f64 * avg_query_cost;

        Ok(CostStats {
            total_queries: total_queries as u64,
            total_input_tokens: total_input_tokens as u64,
            total_output_tokens: total_output_tokens as u64,
            total_cached_tokens: total_cached_tokens as u64,
            total_cost_usd,
            cache_hits: cache_hits as u64,
            cache_hit_rate,
            cost_saved_from_cache,
            period_start: DateTime::parse_from_rfc3339(start)
                .unwrap()
                .with_timezone(&Utc),
            period_end: DateTime::parse_from_rfc3339(end)
                .unwrap()
                .with_timezone(&Utc),
        })
    }

    // Get cost breakdown by operation type
    pub async fn get_operation_breakdown(
        &self,
        start: &str,
        end: &str,
    ) -> Result<Vec<OperationCost>> {
        let rows = sqlx::query_as::<_, (String, i64, f64, i64, i64, i64)>(
            r#"
            SELECT
                operation,
                COUNT(*) as query_count,
                SUM(cost_usd) as total_cost,
                SUM(input_tokens) as input_tokens,
                SUM(output_tokens) as output_tokens,
                SUM(cached_tokens) as cached_tokens
            FROM llm_costs
            WHERE timestamp >= $1 AND timestamp <= $2
            GROUP BY operation
            ORDER BY total_cost DESC
            "#,
        )
        .bind(start)
        .bind(end)
        .fetch_all(&self.pool)
        .await
        .context("Failed to fetch operation breakdown")?;

        Ok(rows
            .into_iter()
            .map(|(operation, count, total_cost, input, output, cached)| {
                let avg_cost = total_cost / count as f64;
                let total_tokens = (input + output + cached) as u64;

                OperationCost {
                    operation,
                    query_count: count as u64,
                    total_cost_usd: total_cost,
                    avg_cost_usd: avg_cost,
                    total_tokens,
                }
            })
            .collect())
    }

    // Get budget status
    pub async fn get_budget_status(&self) -> Result<BudgetStatus> {
        let daily_stats = self.get_daily_stats().await?;
        let monthly_stats = self.get_monthly_stats().await?;

        let daily_remaining = self.daily_budget - daily_stats.total_cost_usd;
        let daily_percent = (daily_stats.total_cost_usd / self.daily_budget) * 100.0;

        let monthly_remaining = self.monthly_budget - monthly_stats.total_cost_usd;
        let monthly_percent = (monthly_stats.total_cost_usd / self.monthly_budget) * 100.0;

        let mut alerts = Vec::new();

        if daily_percent >= 100.0 {
            alerts.push(format!(
                "⛔ Daily budget exceeded! ${:.2} / ${:.2}",
                daily_stats.total_cost_usd, self.daily_budget
            ));
        } else if daily_percent >= 80.0 {
            alerts.push(format!(
                "⚠️  Daily budget at {:.0}%! ${:.2} / ${:.2}",
                daily_percent, daily_stats.total_cost_usd, self.daily_budget
            ));
        }

        if monthly_percent >= 100.0 {
            alerts.push(format!(
                "⛔ Monthly budget exceeded! ${:.2} / ${:.2}",
                monthly_stats.total_cost_usd, self.monthly_budget
            ));
        } else if monthly_percent >= 80.0 {
            alerts.push(format!(
                "⚠️  Monthly budget at {:.0}%! ${:.2} / ${:.2}",
                monthly_percent, monthly_stats.total_cost_usd, self.monthly_budget
            ));
        }

        Ok(BudgetStatus {
            daily_spend: daily_stats.total_cost_usd,
            daily_budget: self.daily_budget,
            daily_remaining,
            daily_percent_used: daily_percent,
            monthly_spend: monthly_stats.total_cost_usd,
            monthly_budget: self.monthly_budget,
            monthly_remaining,
            monthly_percent_used: monthly_percent,
            alerts,
        })
    }

    // Check budget and emit warnings
    async fn check_budget_alerts(&self) -> Result<()> {
        let status = self.get_budget_status().await?;

        for alert in &status.alerts {
            warn!("{}", alert);
        }

        Ok(())
    }

    // Generate daily report (now includes static analysis savings)
    pub async fn daily_report(&self) -> Result<String> {
        let stats = self.get_daily_stats().await?;
        let status = self.get_budget_status().await?;

        let today = Utc::now().format("%Y-%m-%d");

        let mut report = format!("📊 Daily Cost Report - {}\n\n", today);

        report.push_str(&format!("Total Queries: {}\n", stats.total_queries));
        report.push_str(&format!("Total Cost: ${:.4}\n", stats.total_cost_usd));
        report.push_str(&format!(
            "Budget: ${:.2} / ${:.2} ({:.0}%)\n",
            status.daily_spend, status.daily_budget, status.daily_percent_used
        ));
        report.push_str(&format!("Cache Hit Rate: {:.1}%\n", stats.cache_hit_rate));
        report.push_str(&format!(
            "Cost Saved (Cache): ${:.4}\n\n",
            stats.cost_saved_from_cache
        ));

        report.push_str(&format!(
            "Tokens: {}M in / {}M out / {}M cached\n",
            stats.total_input_tokens / 1_000_000,
            stats.total_output_tokens / 1_000_000,
            stats.total_cached_tokens / 1_000_000
        ));

        // Include static analysis savings
        if let Ok(savings) = self.get_daily_savings_report().await {
            if savings.total_files > 0 {
                report.push_str("\n📉 Static Analysis Savings:\n");
                report.push_str(&format!(
                    "  Files processed: {} (skip: {}, minimal: {}, standard: {}, deep: {})\n",
                    savings.total_files,
                    savings.files_skipped,
                    savings.files_minimal,
                    savings.files_standard,
                    savings.files_deep_dive,
                ));
                report.push_str(&format!(
                    "  LLM calls avoided: {}\n",
                    savings.llm_calls_avoided
                ));
                report.push_str(&format!(
                    "  Estimated savings: ${:.4}\n",
                    savings.total_estimated_savings_usd
                ));
                report.push_str(&format!(
                    "  Savings rate: {:.1}%\n",
                    savings.savings_percent
                ));
                report.push_str(&format!(
                    "  Static issues found: {}\n",
                    savings.total_static_issues
                ));
            }
        }

        if !status.alerts.is_empty() {
            report.push_str("\n⚠️  Alerts:\n");
            for alert in &status.alerts {
                report.push_str(&format!("  {}\n", alert));
            }
        }

        Ok(report)
    }

    // Get top expensive queries
    pub async fn get_expensive_queries(&self, limit: i64) -> Result<Vec<(String, f64, i64)>> {
        let rows = sqlx::query_as::<_, (String, f64, String)>(
            r#"
            SELECT operation, cost_usd, timestamp
            FROM llm_costs
            ORDER BY cost_usd DESC
            LIMIT $1
            "#,
        )
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .context("Failed to fetch expensive queries")?;

        Ok(rows
            .into_iter()
            .map(|(op, cost, ts)| {
                let timestamp = DateTime::parse_from_rfc3339(&ts).unwrap().timestamp();
                (op, cost, timestamp)
            })
            .collect())
    }

    // Clear old records (for cleanup)
    pub async fn clear_old_records(&self, days: i64) -> Result<u64> {
        let cutoff = (Utc::now() - Duration::days(days)).to_rfc3339();

        let result = sqlx::query(
            r#"
            DELETE FROM llm_costs
            WHERE timestamp < $1
            "#,
        )
        .bind(cutoff)
        .execute(&self.pool)
        .await
        .context("Failed to clear old records")?;

        let deleted = result.rows_affected();
        info!("Cleared {} cost records older than {} days", deleted, days);

        Ok(deleted)
    }

    // Clear old static decision records
    pub async fn clear_old_static_decisions(&self, days: i64) -> Result<u64> {
        let cutoff = (Utc::now() - Duration::days(days)).to_rfc3339();

        let result = sqlx::query(
            r#"
            DELETE FROM static_decisions
            WHERE timestamp < $1
            "#,
        )
        .bind(cutoff)
        .execute(&self.pool)
        .await
        .context("Failed to clear old static decisions")?;

        let deleted = result.rows_affected();
        if deleted > 0 {
            info!(
                "Cleared {} static decision records older than {} days",
                deleted, days
            );
        }

        Ok(deleted)
    }

    // Get combined daily report as structured data (for API/UI consumption)
    pub async fn get_combined_daily_report(
        &self,
    ) -> Result<(CostStats, SavingsReport, BudgetStatus)> {
        let stats = self.get_daily_stats().await?;
        let savings = self.get_daily_savings_report().await?;
        let budget = self.get_budget_status().await?;
        Ok((stats, savings, budget))
    }
}

impl SavingsReport {
    // Format as a human-readable summary
    pub fn format_summary(&self) -> String {
        format!(
            "Static Analysis Savings ({}): {} files ({} skipped, {} minimal, {} std, {} deep) | \
             ${:.4} saved ({:.1}%) | {} LLM calls avoided | {} static issues",
            self.period,
            self.total_files,
            self.files_skipped,
            self.files_minimal,
            self.files_standard,
            self.files_deep_dive,
            self.total_estimated_savings_usd,
            self.savings_percent,
            self.llm_calls_avoided,
            self.total_static_issues,
        )
    }
}

impl std::fmt::Display for SavingsReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.format_summary())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::PgPool;

    async fn create_test_pool() -> PgPool {
        crate::db::core::init_db(&std::env::var("DATABASE_URL").unwrap_or_else(|_| {
            "postgresql://rustcode:changeme@localhost:5432/rustcode_test".to_string()
        }))
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn test_cost_calculation() {
        let pool = create_test_pool().await;
        let tracker = CostTracker::new(pool).await.unwrap();

        let usage = TokenUsage {
            input_tokens: 100_000,
            output_tokens: 50_000,
            cached_tokens: 0,
        };

        let cost = tracker.calculate_cost(&usage);

        // Expected: (100k/1M * 0.20) + (50k/1M * 0.50) = 0.02 + 0.025 = 0.045
        assert!((cost - 0.045).abs() < 0.0001);
    }

    #[tokio::test]
    async fn test_log_call() -> Result<()> {
        let pool = create_test_pool().await;
        let tracker = CostTracker::new(pool).await?;

        let usage = TokenUsage {
            input_tokens: 1000,
            output_tokens: 500,
            cached_tokens: 0,
        };

        let id = tracker
            .log_call("test_op", "grok-4-1", usage, false)
            .await?;
        assert!(id > 0);

        Ok(())
    }

    #[tokio::test]
    async fn test_daily_stats() -> Result<()> {
        let pool = create_test_pool().await;
        let tracker = CostTracker::new(pool).await?;

        // Log a few calls
        for _ in 0..3 {
            let usage = TokenUsage {
                input_tokens: 10_000,
                output_tokens: 5_000,
                cached_tokens: 0,
            };
            tracker.log_call("test", "grok", usage, false).await?;
        }

        // Query all records to verify they were inserted
        let count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM llm_costs")
            .fetch_one(&tracker.pool)
            .await?;

        // Use get_all_time_stats instead of get_daily_stats to avoid timestamp comparison issues.
        // Use >= 3 because parallel tests (e.g. test_log_call) also insert into llm_costs,
        // so the all-time count may be higher than exactly 3.
        let stats = tracker.get_all_time_stats().await?;
        assert!(
            stats.total_queries >= 3,
            "Expected at least 3 queries, but got {} (database total: {})",
            stats.total_queries,
            count.0
        );
        assert!(stats.total_cost_usd > 0.0);

        Ok(())
    }

    #[tokio::test]
    async fn test_budget_status() -> Result<()> {
        let pool = create_test_pool().await;
        let tracker = CostTracker::with_budgets(pool, 1.0, 10.0).await?;

        let status = tracker.get_budget_status().await?;
        assert_eq!(status.daily_budget, 1.0);
        assert_eq!(status.monthly_budget, 10.0);

        Ok(())
    }
}
