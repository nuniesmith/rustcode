//! Full Audit Engine
//!
//! Orchestrates a complete, file-by-file LLM audit of a repository.
//! Progress is written to the `audit_runs` database table so the API can
//! poll for live updates.  The final report (Markdown + JSON) is stored in the
//! same row so it can be rendered without touching the filesystem.
//!
//! # Pipeline
//!
//! 1. Insert an `audit_runs` row with `status = 'running'`
//! 2. Collect every source file (respecting skip config)
//! 3. For each file: read → LLM score → accumulate findings → update DB progress
//! 4. When all files are processed: call LLM for the master synthesis report
//! 5. Render Markdown report, store in `audit_runs.report_markdown` + `report_json`
//! 6. Update `status = 'completed'`
//!
//! On any unrecoverable error the row is updated to `status = 'failed'` with an
//! `error_message` so the UI can surface it.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use tracing::{debug, error, info, warn};
use uuid::Uuid;

use crate::audit::runner::AuditRunnerConfig;
use crate::grok_client::{FileScoreResult, GrokClient};

// ============================================================================
// Public types
// ============================================================================

/// Severity bucket for a single file finding.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FileSeverity {
    Info,
    Low,
    Medium,
    High,
    Critical,
}

impl FileSeverity {
    pub fn from_score(score: f64) -> Self {
        match score as u32 {
            0..=29 => Self::Critical,
            30..=49 => Self::High,
            50..=64 => Self::Medium,
            65..=79 => Self::Low,
            _ => Self::Info,
        }
    }

    pub fn emoji(&self) -> &'static str {
        match self {
            Self::Critical => "🔴",
            Self::High => "🟠",
            Self::Medium => "🟡",
            Self::Low => "🔵",
            Self::Info => "⚪",
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Critical => "critical",
            Self::High => "high",
            Self::Medium => "medium",
            Self::Low => "low",
            Self::Info => "info",
        }
    }
}

impl std::fmt::Display for FileSeverity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The per-file analysis result stored in the final report.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileAuditResult {
    /// Relative path inside the repo.
    pub path: String,
    /// Overall score 0-100 (higher = better).
    pub overall_score: f64,
    /// Security sub-score.
    pub security_score: f64,
    /// Code quality sub-score.
    pub quality_score: f64,
    /// Complexity sub-score.
    pub complexity_score: f64,
    /// Maintainability sub-score.
    pub maintainability_score: f64,
    /// Derived severity from `overall_score`.
    pub severity: FileSeverity,
    /// LLM-generated one-paragraph summary.
    pub summary: String,
    /// Concrete issues found.
    pub issues: Vec<String>,
    /// Improvement suggestions.
    pub suggestions: Vec<String>,
    /// Whether this file was actually scored by the LLM (vs. static-only).
    pub llm_scored: bool,
}

impl FileAuditResult {
    fn from_score(path: String, score: FileScoreResult, llm_scored: bool) -> Self {
        let severity = FileSeverity::from_score(score.overall_score);
        Self {
            path,
            overall_score: score.overall_score,
            security_score: score.security_score,
            quality_score: score.quality_score,
            complexity_score: score.complexity_score,
            maintainability_score: score.maintainability_score,
            severity,
            summary: score.summary,
            issues: score.issues,
            suggestions: score.suggestions,
            llm_scored,
        }
    }

    /// Placeholder result used when a file is skipped or cannot be read.
    fn skipped(path: String, reason: &str) -> Self {
        Self {
            path,
            overall_score: 75.0,
            security_score: 75.0,
            quality_score: 75.0,
            complexity_score: 75.0,
            maintainability_score: 75.0,
            severity: FileSeverity::Info,
            summary: format!("Skipped: {}", reason),
            issues: vec![],
            suggestions: vec![],
            llm_scored: false,
        }
    }
}

/// Final synthesized report written to `audit_runs`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FullAuditReport {
    pub run_id: String,
    pub repo_name: String,
    pub repo_path: String,
    pub started_at: i64,
    pub completed_at: i64,
    pub duration_secs: f64,
    pub files_total: usize,
    pub files_scored: usize,
    pub estimated_cost_usd: f64,

    // Per-file results (sorted: worst first)
    pub files: Vec<FileAuditResult>,

    // Aggregate scores
    pub avg_overall: f64,
    pub avg_security: f64,
    pub avg_quality: f64,
    pub avg_complexity: f64,
    pub avg_maintainability: f64,

    // Severity distribution
    pub count_critical: usize,
    pub count_high: usize,
    pub count_medium: usize,
    pub count_low: usize,
    pub count_info: usize,

    // Master synthesis from LLM
    pub executive_summary: String,
    pub scope_assessment: String,
    pub scope_drift_notes: String,
    pub broken_code_notes: String,
    pub consolidation_opportunities: Vec<String>,
    pub deletion_candidates: Vec<String>,
    pub layout_improvements: Vec<String>,
    pub top_priorities: Vec<String>,
    pub strengths: Vec<String>,
    pub weaknesses: Vec<String>,
    pub overall_health: f64,
}

impl FullAuditReport {
    /// Compute aggregate stats from the per-file results.
    fn compute_aggregates(files: &[FileAuditResult]) -> (f64, f64, f64, f64, f64) {
        if files.is_empty() {
            return (75.0, 75.0, 75.0, 75.0, 75.0);
        }
        let n = files.len() as f64;
        let sum = |f: &dyn Fn(&FileAuditResult) -> f64| -> f64 { files.iter().map(f).sum::<f64>() };
        (
            sum(&|f| f.overall_score) / n,
            sum(&|f| f.security_score) / n,
            sum(&|f| f.quality_score) / n,
            sum(&|f| f.complexity_score) / n,
            sum(&|f| f.maintainability_score) / n,
        )
    }

    /// Render the report as a Markdown document.
    pub fn render_markdown(&self) -> String {
        let mut md = String::with_capacity(16 * 1024);

        let started = chrono::DateTime::from_timestamp(self.started_at, 0)
            .map(|dt: chrono::DateTime<chrono::Utc>| dt.format("%Y-%m-%d %H:%M UTC").to_string())
            .unwrap_or_else(|| "unknown".to_string());

        let completed = chrono::DateTime::from_timestamp(self.completed_at, 0)
            .map(|dt: chrono::DateTime<chrono::Utc>| dt.format("%Y-%m-%d %H:%M UTC").to_string())
            .unwrap_or_else(|| "unknown".to_string());

        // Header
        md.push_str(&format!("# 🔬 Full Code Audit — {}\n\n", self.repo_name));
        md.push_str(&format!(
            "> **Run ID:** `{}`  \n> **Started:** {}  \n> **Completed:** {}  \n> **Duration:** {:.1}s  \n> **Files Analysed:** {}  \n> **Estimated Cost:** ${:.4}\n\n",
            self.run_id, started, completed, self.duration_secs, self.files_total, self.estimated_cost_usd
        ));
        md.push_str("---\n\n");

        // Health score
        let health_bar = health_bar(self.overall_health);
        md.push_str(&format!(
            "## Overall Health: {:.1}/100 {}\n\n",
            self.overall_health, health_bar
        ));

        // Severity summary table
        md.push_str("## Severity Summary\n\n");
        md.push_str("| Severity | Count |\n|----------|-------|\n");
        md.push_str(&format!("| 🔴 Critical | {} |\n", self.count_critical));
        md.push_str(&format!("| 🟠 High     | {} |\n", self.count_high));
        md.push_str(&format!("| 🟡 Medium   | {} |\n", self.count_medium));
        md.push_str(&format!("| 🔵 Low      | {} |\n", self.count_low));
        md.push_str(&format!("| ⚪ Info     | {} |\n\n", self.count_info));

        // Aggregate scores
        md.push_str("## Aggregate Scores\n\n");
        md.push_str("| Dimension | Score |\n|-----------|-------|\n");
        md.push_str(&format!("| Overall          | {:.1} |\n", self.avg_overall));
        md.push_str(&format!(
            "| Security         | {:.1} |\n",
            self.avg_security
        ));
        md.push_str(&format!("| Quality          | {:.1} |\n", self.avg_quality));
        md.push_str(&format!(
            "| Complexity       | {:.1} |\n",
            self.avg_complexity
        ));
        md.push_str(&format!(
            "| Maintainability  | {:.1} |\n\n",
            self.avg_maintainability
        ));

        // Executive summary
        if !self.executive_summary.is_empty() {
            md.push_str("## Executive Summary\n\n");
            md.push_str(&self.executive_summary);
            md.push_str("\n\n");
        }

        // Scope
        if !self.scope_assessment.is_empty() {
            md.push_str("## Scope Assessment\n\n");
            md.push_str(&self.scope_assessment);
            md.push_str("\n\n");
        }
        if !self.scope_drift_notes.is_empty() {
            md.push_str("## Scope Drift\n\n");
            md.push_str(&self.scope_drift_notes);
            md.push_str("\n\n");
        }

        // Broken / problematic code
        if !self.broken_code_notes.is_empty() {
            md.push_str("## Broken / Problematic Code\n\n");
            md.push_str(&self.broken_code_notes);
            md.push_str("\n\n");
        }

        // Top priorities
        if !self.top_priorities.is_empty() {
            md.push_str("## Top Priorities\n\n");
            for p in &self.top_priorities {
                md.push_str(&format!("- {}\n", p));
            }
            md.push('\n');
        }

        // Strengths / Weaknesses
        if !self.strengths.is_empty() {
            md.push_str("## Strengths\n\n");
            for s in &self.strengths {
                md.push_str(&format!("- {}\n", s));
            }
            md.push('\n');
        }
        if !self.weaknesses.is_empty() {
            md.push_str("## Weaknesses\n\n");
            for w in &self.weaknesses {
                md.push_str(&format!("- {}\n", w));
            }
            md.push('\n');
        }

        // Layout improvements
        if !self.layout_improvements.is_empty() {
            md.push_str("## Layout / Architecture Improvements\n\n");
            for l in &self.layout_improvements {
                md.push_str(&format!("- {}\n", l));
            }
            md.push('\n');
        }

        // Consolidation opportunities
        if !self.consolidation_opportunities.is_empty() {
            md.push_str("## Consolidation Opportunities\n\n");
            for c in &self.consolidation_opportunities {
                md.push_str(&format!("- {}\n", c));
            }
            md.push('\n');
        }

        // Deletion candidates
        if !self.deletion_candidates.is_empty() {
            md.push_str("## Deletion Candidates\n\n");
            for d in &self.deletion_candidates {
                md.push_str(&format!("- {}\n", d));
            }
            md.push('\n');
        }

        // Per-file results
        md.push_str("## Per-File Analysis\n\n");
        md.push_str("| File | Overall | Security | Quality | Severity |\n");
        md.push_str("|------|---------|----------|---------|----------|\n");
        for f in &self.files {
            md.push_str(&format!(
                "| `{}` | {:.0} | {:.0} | {:.0} | {} {} |\n",
                f.path,
                f.overall_score,
                f.security_score,
                f.quality_score,
                f.severity.emoji(),
                f.severity,
            ));
        }
        md.push('\n');

        // Detailed per-file sections (only for medium+ severity)
        let detailed: Vec<&FileAuditResult> = self
            .files
            .iter()
            .filter(|f| f.severity >= FileSeverity::Medium)
            .collect();

        if !detailed.is_empty() {
            md.push_str("## Detailed Findings\n\n");
            for f in detailed {
                md.push_str(&format!(
                    "### {} `{}` — {:.0}/100\n\n",
                    f.severity.emoji(),
                    f.path,
                    f.overall_score
                ));
                if !f.summary.is_empty() {
                    md.push_str(&format!("**Summary:** {}\n\n", f.summary));
                }
                if !f.issues.is_empty() {
                    md.push_str("**Issues:**\n");
                    for issue in &f.issues {
                        md.push_str(&format!("- {}\n", issue));
                    }
                    md.push('\n');
                }
                if !f.suggestions.is_empty() {
                    md.push_str("**Suggestions:**\n");
                    for s in &f.suggestions {
                        md.push_str(&format!("- {}\n", s));
                    }
                    md.push('\n');
                }
            }
        }

        md.push_str("---\n");
        md.push_str(&format!(
            "*Generated by RustCode Full Audit · Run `{}`*\n",
            self.run_id
        ));

        md
    }
}

fn health_bar(score: f64) -> &'static str {
    match score as u32 {
        0..=29 => "🔴🔴🔴🔴🔴",
        30..=49 => "🟠🟠🟠⚫⚫",
        50..=64 => "🟡🟡🟡⚫⚫",
        65..=79 => "🟢🟢🟢🟢⚫",
        _ => "🟢🟢🟢🟢🟢",
    }
}

// ============================================================================
// Master synthesis prompt
// ============================================================================

fn build_master_synthesis_prompt(
    repo_name: &str,
    files: &[FileAuditResult],
    avg_overall: f64,
) -> String {
    let file_summaries: String = files
        .iter()
        .take(60) // keep prompt manageable
        .map(|f| {
            format!(
                "- `{}` [score={:.0}, sev={}]: {}",
                f.path,
                f.overall_score,
                f.severity,
                if f.summary.len() > 120 {
                    format!("{}…", &f.summary[..120])
                } else {
                    f.summary.clone()
                }
            )
        })
        .collect::<Vec<_>>()
        .join("\n");

    let issue_dump: String = files
        .iter()
        .filter(|f| f.severity >= FileSeverity::High)
        .flat_map(|f| f.issues.iter().map(move |i| format!("[{}] {}", f.path, i)))
        .take(80)
        .collect::<Vec<_>>()
        .join("\n");

    format!(
        r#"You are a senior software architect performing a full code audit of the repository "{repo_name}".

Average overall score across {file_count} files: {avg_overall:.1}/100

## Per-file summaries
{file_summaries}

## High/Critical issues found
{issue_dump}

Based on all of the above, produce a comprehensive master audit synthesis. Return ONLY valid JSON (no markdown fences) with this exact structure:
{{
  "executive_summary": "2-4 paragraph overview of the codebase state",
  "scope_assessment": "What this codebase does and whether the files stay on-mission",
  "scope_drift_notes": "Files or modules that appear out of scope or mission-creep, or 'None detected' if clean",
  "broken_code_notes": "Specific files or functions that appear broken, incomplete, or non-functional",
  "consolidation_opportunities": ["file_a.rs and file_b.rs duplicate X logic and should be merged", ...],
  "deletion_candidates": ["path/to/file.rs — reason", ...],
  "layout_improvements": ["suggestion about module structure", ...],
  "top_priorities": ["most important action item 1", "action item 2", ...],
  "strengths": ["strength 1", "strength 2", ...],
  "weaknesses": ["weakness 1", "weakness 2", ...],
  "overall_health": 0-100
}}

Be specific and actionable. Name actual files. Keep each list item concise (one line max)."#,
        repo_name = repo_name,
        file_count = files.len(),
        avg_overall = avg_overall,
        file_summaries = file_summaries,
        issue_dump = issue_dump,
    )
}

/// Intermediate JSON shape returned by the LLM for the master synthesis.
#[derive(Debug, Deserialize, Default)]
struct MasterSynthesisResponse {
    #[serde(default)]
    executive_summary: String,
    #[serde(default)]
    scope_assessment: String,
    #[serde(default)]
    scope_drift_notes: String,
    #[serde(default)]
    broken_code_notes: String,
    #[serde(default)]
    consolidation_opportunities: Vec<String>,
    #[serde(default)]
    deletion_candidates: Vec<String>,
    #[serde(default)]
    layout_improvements: Vec<String>,
    #[serde(default)]
    top_priorities: Vec<String>,
    #[serde(default)]
    strengths: Vec<String>,
    #[serde(default)]
    weaknesses: Vec<String>,
    #[serde(default = "default_health")]
    overall_health: f64,
}

fn default_health() -> f64 {
    65.0
}

// ============================================================================
// Engine
// ============================================================================

/// Configuration for a single full-audit run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FullAuditConfig {
    /// Maximum number of files to send to the LLM (most-valuable first).
    pub max_llm_files: usize,
    /// Hard cost cap for LLM calls in this run.
    pub max_cost_usd: f64,
    /// File extensions to skip.
    pub skip_extensions: Vec<String>,
    /// Path fragments to skip.
    pub skip_paths: Vec<String>,
    /// Maximum file size in bytes to read.
    pub max_file_bytes: u64,
}

impl Default for FullAuditConfig {
    fn default() -> Self {
        // Borrow sensible defaults from AuditRunnerConfig — more files here
        // because we want to cover the whole repo.
        let base = AuditRunnerConfig::default();
        Self {
            max_llm_files: 200,
            max_cost_usd: 5.0,
            skip_extensions: base.skip_extensions,
            skip_paths: base.skip_paths,
            max_file_bytes: base.max_file_bytes,
        }
    }
}

impl FullAuditConfig {
    /// Serialise to JSON string for storage in `audit_runs.config_json`.
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }
}

/// The public entry point — drives the whole audit and persists results to DB.
pub struct FullAuditEngine {
    pool: PgPool,
    grok: Option<Arc<GrokClient>>,
    config: FullAuditConfig,
}

impl FullAuditEngine {
    pub fn new(pool: PgPool, grok: Option<Arc<GrokClient>>) -> Self {
        Self {
            pool,
            grok,
            config: FullAuditConfig::default(),
        }
    }

    pub fn with_config(mut self, config: FullAuditConfig) -> Self {
        self.config = config;
        self
    }

    /// Launch an audit run in the background; returns the `run_id` immediately.
    ///
    /// The caller can poll `GET /audit/:run_id/status` for progress.
    pub async fn start_background(
        self: Arc<Self>,
        repo_id: Option<String>,
        repo_path: String,
        repo_name: String,
    ) -> Result<String> {
        let run_id = Uuid::new_v4().to_string();
        let run_id_clone = run_id.clone();

        // Persist initial row
        let now = chrono::Utc::now().timestamp();
        let config_json = self.config.to_json();
        sqlx::query(
            r#"INSERT INTO audit_runs
               (id, repo_id, repo_path, repo_name, status, created_at, started_at,
                files_total, files_done, config_json)
               VALUES ($1, $2, $3, $4, 'running', $5, $5, 0, 0, $6)"#,
        )
        .bind(&run_id)
        .bind(&repo_id)
        .bind(&repo_path)
        .bind(&repo_name)
        .bind(now)
        .bind(&config_json)
        .execute(&self.pool)
        .await
        .context("Failed to insert audit_runs row")?;

        info!(run_id = %run_id, repo = %repo_name, "Full audit started — running in background");

        // Spawn background task
        tokio::spawn(async move {
            if let Err(e) = self
                .run_audit(run_id_clone.clone(), repo_path, repo_name)
                .await
            {
                error!(run_id = %run_id_clone, error = %e, "Full audit failed");
                let ts = chrono::Utc::now().timestamp();
                let _ = sqlx::query(
                    "UPDATE audit_runs SET status='failed', error_message=$1, completed_at=$2 WHERE id=$3",
                )
                .bind(e.to_string())
                .bind(ts)
                .bind(&run_id_clone)
                .execute(&self.pool)
                .await;
            }
        });

        Ok(run_id)
    }

    // ------------------------------------------------------------------
    // Core pipeline
    // ------------------------------------------------------------------

    async fn run_audit(&self, run_id: String, repo_path: String, repo_name: String) -> Result<()> {
        let start = Instant::now();
        let repo = Path::new(&repo_path);

        // 1. Collect files
        let files = self.collect_files(repo)?;
        let total = files.len();
        info!(run_id = %run_id, total, "File collection complete");

        // Update total count in DB
        sqlx::query("UPDATE audit_runs SET files_total=$1 WHERE id=$2")
            .bind(total as i32)
            .bind(&run_id)
            .execute(&self.pool)
            .await?;

        // 2. Score every file
        let mut file_results: Vec<FileAuditResult> = Vec::with_capacity(total);
        let mut total_cost: f64 = 0.0;
        let mut llm_files_used: usize = 0;

        for (idx, rel_path) in files.iter().enumerate() {
            let rel_str = rel_path.to_string_lossy().to_string();
            let abs_path = repo.join(rel_path);

            // Update current file in DB every file (throttle: only every 3 to reduce write pressure)
            if idx % 3 == 0 {
                let done = idx as i32;
                let _ =
                    sqlx::query("UPDATE audit_runs SET files_done=$1, current_file=$2 WHERE id=$3")
                        .bind(done)
                        .bind(&rel_str)
                        .bind(&run_id)
                        .execute(&self.pool)
                        .await;
            }

            // Read file content
            let content = match std::fs::read_to_string(&abs_path) {
                Ok(c) => c,
                Err(e) => {
                    debug!(path = %rel_str, error = %e, "Cannot read file — skipping");
                    file_results.push(FileAuditResult::skipped(rel_str, &e.to_string()));
                    continue;
                }
            };

            // Decide whether to LLM-score this file
            let use_llm = self.grok.is_some()
                && llm_files_used < self.config.max_llm_files
                && total_cost < self.config.max_cost_usd
                && !content.trim().is_empty();

            let result = if use_llm {
                let grok = self.grok.as_ref().unwrap();
                match grok.score_file(&rel_str, &content).await {
                    Ok(score) => {
                        // Rough cost estimate: ~$5/M input tokens, ~4 chars/token
                        let est_tokens = (content.len() as f64 / 4.0).max(1.0);
                        total_cost += est_tokens / 1_000_000.0 * 5.0;
                        llm_files_used += 1;
                        debug!(
                            path = %rel_str,
                            score = score.overall_score,
                            cost_so_far = total_cost,
                            "LLM scored"
                        );

                        // Update per-severity counters incrementally
                        let sev = FileSeverity::from_score(score.overall_score);
                        self.increment_severity_counter(&run_id, &sev).await;

                        FileAuditResult::from_score(rel_str.clone(), score, true)
                    }
                    Err(e) => {
                        warn!(path = %rel_str, error = %e, "LLM score failed — using static fallback");
                        FileAuditResult::skipped(rel_str, &format!("LLM error: {}", e))
                    }
                }
            } else {
                // Static-only fallback: simple heuristic scoring
                let score = static_heuristic_score(&rel_str, &content);
                let sev = FileSeverity::from_score(score.overall_score);
                self.increment_severity_counter(&run_id, &sev).await;
                FileAuditResult::from_score(rel_str.clone(), score, false)
            };

            file_results.push(result);
        }

        // Final progress update
        let files_done = file_results.len() as i32;
        sqlx::query(
            "UPDATE audit_runs SET files_done=$1, current_file=NULL, estimated_cost_usd=$2 WHERE id=$3",
        )
        .bind(files_done)
        .bind(total_cost)
        .bind(&run_id)
        .execute(&self.pool)
        .await?;

        // 3. Sort by severity (worst first)
        file_results.sort_by(|a, b| {
            a.overall_score
                .partial_cmp(&b.overall_score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        // 4. Master synthesis
        let (avg_o, avg_s, avg_q, avg_c, avg_m) =
            FullAuditReport::compute_aggregates(&file_results);

        let synthesis = if let Some(ref grok) = self.grok {
            let prompt = build_master_synthesis_prompt(&repo_name, &file_results, avg_o);
            match grok.ask(&prompt, None).await {
                Ok(raw) => {
                    // Strip potential markdown fences the LLM might add
                    let cleaned = strip_json_fences(&raw);
                    match serde_json::from_str::<MasterSynthesisResponse>(&cleaned) {
                        Ok(s) => s,
                        Err(e) => {
                            warn!(error = %e, "Failed to parse master synthesis JSON — using fallback");
                            MasterSynthesisResponse {
                                executive_summary: raw.chars().take(500).collect(),
                                overall_health: avg_o,
                                ..Default::default()
                            }
                        }
                    }
                }
                Err(e) => {
                    warn!(error = %e, "Master synthesis LLM call failed — using empty synthesis");
                    MasterSynthesisResponse {
                        overall_health: avg_o,
                        ..Default::default()
                    }
                }
            }
        } else {
            // No LLM: generate a deterministic summary from scores
            MasterSynthesisResponse {
                executive_summary: format!(
                    "Static-only audit of {} files completed. Average overall score: {:.1}/100. \
                     No LLM key configured — per-file scores are derived from static heuristics only.",
                    file_results.len(),
                    avg_o
                ),
                scope_assessment: "LLM not available — scope assessment skipped.".to_string(),
                overall_health: avg_o,
                ..Default::default()
            }
        };

        let completed_at = chrono::Utc::now().timestamp();
        let duration_secs = start.elapsed().as_secs_f64();

        // Count severity distribution
        let count_critical = file_results
            .iter()
            .filter(|f| f.severity == FileSeverity::Critical)
            .count();
        let count_high = file_results
            .iter()
            .filter(|f| f.severity == FileSeverity::High)
            .count();
        let count_medium = file_results
            .iter()
            .filter(|f| f.severity == FileSeverity::Medium)
            .count();
        let count_low = file_results
            .iter()
            .filter(|f| f.severity == FileSeverity::Low)
            .count();
        let count_info = file_results
            .iter()
            .filter(|f| f.severity == FileSeverity::Info)
            .count();

        // Build the final report struct
        let report = FullAuditReport {
            run_id: run_id.clone(),
            repo_name: repo_name.clone(),
            repo_path: repo_path.clone(),
            started_at: chrono::Utc::now().timestamp() - duration_secs as i64,
            completed_at,
            duration_secs,
            files_total: total,
            files_scored: llm_files_used,
            estimated_cost_usd: total_cost,
            files: file_results,
            avg_overall: avg_o,
            avg_security: avg_s,
            avg_quality: avg_q,
            avg_complexity: avg_c,
            avg_maintainability: avg_m,
            count_critical,
            count_high,
            count_medium,
            count_low,
            count_info,
            executive_summary: synthesis.executive_summary,
            scope_assessment: synthesis.scope_assessment,
            scope_drift_notes: synthesis.scope_drift_notes,
            broken_code_notes: synthesis.broken_code_notes,
            consolidation_opportunities: synthesis.consolidation_opportunities,
            deletion_candidates: synthesis.deletion_candidates,
            layout_improvements: synthesis.layout_improvements,
            top_priorities: synthesis.top_priorities,
            strengths: synthesis.strengths,
            weaknesses: synthesis.weaknesses,
            overall_health: synthesis.overall_health,
        };

        let report_markdown = report.render_markdown();
        let report_json = serde_json::to_string(&report).unwrap_or_default();

        // 5. Persist final report
        sqlx::query(
            r#"UPDATE audit_runs SET
               status            = 'completed',
               completed_at      = $1,
               files_total       = $2,
               files_done        = $2,
               current_file      = NULL,
               findings_critical = $3,
               findings_high     = $4,
               findings_medium   = $5,
               findings_low      = $6,
               findings_info     = $7,
               report_markdown   = $8,
               report_json       = $9,
               estimated_cost_usd= $10
             WHERE id = $11"#,
        )
        .bind(completed_at)
        .bind(total as i32)
        .bind(count_critical as i32)
        .bind(count_high as i32)
        .bind(count_medium as i32)
        .bind(count_low as i32)
        .bind(count_info as i32)
        .bind(&report_markdown)
        .bind(&report_json)
        .bind(total_cost)
        .bind(&run_id)
        .execute(&self.pool)
        .await?;

        info!(
            run_id = %run_id,
            files = total,
            llm_files = llm_files_used,
            cost = total_cost,
            duration_secs = duration_secs,
            health = report.overall_health,
            "Full audit complete"
        );

        Ok(())
    }

    // ------------------------------------------------------------------
    // Helpers
    // ------------------------------------------------------------------

    /// Collect all source files under `repo_path` respecting skip config.
    fn collect_files(&self, repo_path: &Path) -> Result<Vec<PathBuf>> {
        use walkdir::WalkDir;

        let mut files: Vec<PathBuf> = Vec::new();

        for entry in WalkDir::new(repo_path)
            .follow_links(false)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            if !entry.file_type().is_file() {
                continue;
            }

            let abs_path = entry.path();
            let rel_path = abs_path
                .strip_prefix(repo_path)
                .unwrap_or(abs_path)
                .to_path_buf();
            let rel_str = rel_path.to_string_lossy();

            // Skip by path fragment
            if self
                .config
                .skip_paths
                .iter()
                .any(|skip| rel_str.contains(skip.as_str()))
            {
                continue;
            }

            // Skip by extension
            if let Some(ext) = abs_path.extension().and_then(|e| e.to_str()) {
                if self.config.skip_extensions.iter().any(|s| s == ext) {
                    continue;
                }
            }

            // Skip oversized files
            if let Ok(meta) = std::fs::metadata(abs_path) {
                if meta.len() > self.config.max_file_bytes {
                    debug!(path = %rel_str, size = meta.len(), "Skipping oversized file");
                    continue;
                }
            }

            files.push(rel_path);
        }

        // Deterministic ordering
        files.sort();
        Ok(files)
    }

    /// Increment the appropriate per-severity counter in the DB.
    async fn increment_severity_counter(&self, run_id: &str, sev: &FileSeverity) {
        let col = match sev {
            FileSeverity::Critical => "findings_critical",
            FileSeverity::High => "findings_high",
            FileSeverity::Medium => "findings_medium",
            FileSeverity::Low => "findings_low",
            FileSeverity::Info => "findings_info",
        };
        // Dynamic column names can't use $1 placeholders in sqlx — use format! safely
        // (col is controlled by our match arm, not user input).
        let sql = format!("UPDATE audit_runs SET {} = {} + 1 WHERE id = $1", col, col);
        let _ = sqlx::query(&sql).bind(run_id).execute(&self.pool).await;
    }
}

// ============================================================================
// Static heuristic scorer (fallback when no LLM is available)
// ============================================================================

/// Produce a basic `FileScoreResult` from simple text heuristics when the LLM
/// is not available (no API key, cost cap reached, etc.).
fn static_heuristic_score(_path: &str, content: &str) -> FileScoreResult {
    let lines: Vec<&str> = content.lines().collect();
    let loc = lines.len() as f64;

    // Heuristics
    let todo_count = content.matches("TODO").count()
        + content.matches("FIXME").count()
        + content.matches("HACK").count()
        + content.matches("XXX").count();

    let unwrap_count = content.matches(".unwrap()").count();
    let panic_count = content.matches("panic!(").count() + content.matches("unreachable!(").count();
    let unsafe_count = content.matches("unsafe").count();
    let clone_count = content.matches(".clone()").count();

    // Comment density (rough doc quality proxy)
    let comment_lines = lines
        .iter()
        .filter(|l| {
            let t = l.trim();
            t.starts_with("//") || t.starts_with("///") || t.starts_with("/*") || t.starts_with('#')
        })
        .count() as f64;
    let comment_ratio = if loc > 0.0 { comment_lines / loc } else { 0.0 };

    // Score penalties
    let mut quality_score: f64 = 80.0;
    quality_score -= (todo_count as f64) * 2.0;
    quality_score -= (unwrap_count as f64) * 0.5;
    quality_score -= (panic_count as f64) * 1.0;
    quality_score -= if comment_ratio < 0.05 { 5.0 } else { 0.0 };
    quality_score = quality_score.clamp(20.0, 95.0);

    let mut security_score: f64 = 85.0;
    security_score -= (unsafe_count as f64) * 3.0;
    security_score -= (unwrap_count as f64) * 0.3;
    security_score = security_score.clamp(20.0, 95.0);

    // Complexity penalty for very long files
    let complexity_score: f64 = if loc > 1000.0 {
        60.0
    } else if loc > 500.0 {
        70.0
    } else {
        85.0
    };

    let maintainability_score: f64 = (quality_score * 0.5
        + complexity_score * 0.3
        + (100.0 - clone_count as f64 * 0.2).clamp(50.0, 100.0) * 0.2)
        .clamp(20.0, 95.0);

    let overall_score = (security_score * 0.3 + quality_score * 0.4 + maintainability_score * 0.3)
        .clamp(20.0, 95.0);

    let mut issues: Vec<String> = Vec::new();
    if todo_count > 0 {
        issues.push(format!(
            "{} TODO/FIXME/HACK marker(s) — unfinished work",
            todo_count
        ));
    }
    if unwrap_count > 5 {
        issues.push(format!(
            "{} .unwrap() calls — potential panics on None/Err",
            unwrap_count
        ));
    }
    if unsafe_count > 0 {
        issues.push(format!(
            "{} unsafe block(s) — requires manual safety review",
            unsafe_count
        ));
    }
    if panic_count > 0 {
        issues.push(format!(
            "{} panic!/unreachable! macro(s) found",
            panic_count
        ));
    }
    if loc > 1000.0 {
        issues.push(format!(
            "File is {:.0} lines — consider splitting into smaller modules",
            loc
        ));
    }

    let mut suggestions: Vec<String> = Vec::new();
    if unwrap_count > 3 {
        suggestions.push("Replace .unwrap() with ? or proper error handling".to_string());
    }
    if comment_ratio < 0.05 && loc > 50.0 {
        suggestions.push("Add documentation comments to public API surfaces".to_string());
    }
    if clone_count > 10 {
        suggestions.push(
            "Audit .clone() usage — many clones may indicate ownership design issues".to_string(),
        );
    }

    let summary = format!(
        "Static heuristic analysis: {:.0} LOC, {} TODOs, {} unwraps, {} unsafe blocks. No LLM scoring applied.",
        loc, todo_count, unwrap_count, unsafe_count,
    );

    FileScoreResult {
        overall_score,
        security_score,
        quality_score,
        complexity_score,
        maintainability_score,
        summary,
        issues,
        suggestions,
    }
}

// ============================================================================
// Helpers
// ============================================================================

/// Strip ```json ... ``` fences that some models add around JSON responses.
fn strip_json_fences(s: &str) -> String {
    let s = s.trim();
    // Remove leading ```json or ``` with optional newline
    let s = if s.starts_with("```json") {
        s.trim_start_matches("```json")
    } else if s.starts_with("```") {
        s.trim_start_matches("```")
    } else {
        s
    };
    // Remove trailing ```
    let s = s.trim_end_matches("```").trim();
    s.to_string()
}

// ============================================================================
// DB query helpers (used by the web handler)
// ============================================================================

/// Snapshot of an audit run's live state — used for polling.
#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct AuditRunStatus {
    pub id: String,
    pub repo_name: String,
    pub repo_path: String,
    pub status: String,
    pub files_total: i32,
    pub files_done: i32,
    pub current_file: Option<String>,
    pub findings_critical: i32,
    pub findings_high: i32,
    pub findings_medium: i32,
    pub findings_low: i32,
    pub findings_info: i32,
    pub estimated_cost_usd: f64,
    pub error_message: Option<String>,
    pub created_at: i64,
    pub started_at: Option<i64>,
    pub completed_at: Option<i64>,
}

/// Summary row for the audit list page (no large report_json/markdown).
#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct AuditRunSummary {
    pub id: String,
    pub repo_name: String,
    pub repo_path: String,
    pub repo_id: Option<String>,
    pub status: String,
    pub files_total: i32,
    pub files_done: i32,
    pub findings_critical: i32,
    pub findings_high: i32,
    pub findings_medium: i32,
    pub findings_low: i32,
    pub findings_info: i32,
    pub estimated_cost_usd: f64,
    pub error_message: Option<String>,
    pub created_at: i64,
    pub completed_at: Option<i64>,
}

pub async fn db_list_audit_runs(pool: &PgPool) -> Result<Vec<AuditRunSummary>> {
    let rows = sqlx::query_as::<_, AuditRunSummary>(
        r#"SELECT id, repo_name, repo_path, repo_id, status,
                  files_total, files_done,
                  findings_critical, findings_high, findings_medium, findings_low, findings_info,
                  estimated_cost_usd, error_message, created_at, completed_at
           FROM audit_runs
           ORDER BY created_at DESC
           LIMIT 100"#,
    )
    .fetch_all(pool)
    .await
    .context("Failed to list audit runs")?;
    Ok(rows)
}

pub async fn db_get_audit_status(pool: &PgPool, run_id: &str) -> Result<Option<AuditRunStatus>> {
    let row = sqlx::query_as::<_, AuditRunStatus>(
        r#"SELECT id, repo_name, repo_path, status,
                  files_total, files_done, current_file,
                  findings_critical, findings_high, findings_medium, findings_low, findings_info,
                  estimated_cost_usd, error_message,
                  created_at, started_at, completed_at
           FROM audit_runs
           WHERE id = $1"#,
    )
    .bind(run_id)
    .fetch_optional(pool)
    .await
    .context("Failed to query audit run status")?;
    Ok(row)
}

pub async fn db_get_audit_report_markdown(pool: &PgPool, run_id: &str) -> Result<Option<String>> {
    let row: Option<(Option<String>,)> =
        sqlx::query_as("SELECT report_markdown FROM audit_runs WHERE id = $1")
            .bind(run_id)
            .fetch_optional(pool)
            .await
            .context("Failed to fetch audit report")?;
    Ok(row.and_then(|(md,)| md))
}

pub async fn db_get_audit_report_json(
    pool: &PgPool,
    run_id: &str,
) -> Result<Option<FullAuditReport>> {
    let row: Option<(Option<String>,)> =
        sqlx::query_as("SELECT report_json FROM audit_runs WHERE id = $1")
            .bind(run_id)
            .fetch_optional(pool)
            .await
            .context("Failed to fetch audit report JSON")?;

    match row.and_then(|(j,)| j) {
        Some(json) => {
            let report: FullAuditReport =
                serde_json::from_str(&json).context("Failed to deserialise FullAuditReport")?;
            Ok(Some(report))
        }
        None => Ok(None),
    }
}

pub async fn db_get_runs_for_repo(pool: &PgPool, repo_id: &str) -> Result<Vec<AuditRunSummary>> {
    let rows = sqlx::query_as::<_, AuditRunSummary>(
        r#"SELECT id, repo_name, repo_path, repo_id, status,
                  files_total, files_done,
                  findings_critical, findings_high, findings_medium, findings_low, findings_info,
                  estimated_cost_usd, error_message, created_at, completed_at
           FROM audit_runs
           WHERE repo_id = $1
           ORDER BY created_at DESC"#,
    )
    .bind(repo_id)
    .fetch_all(pool)
    .await
    .context("Failed to query audit runs for repo")?;
    Ok(rows)
}
