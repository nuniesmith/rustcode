//! Audit runner — orchestrates StaticAnalyzer → GrokClient → result serialisation
//!
//! This is the core of the `/api/audit` pipeline. It accepts a repo path,
//! runs static analysis for fast file triage, then feeds interesting files
//! through the LLM for deep analysis, and serialises the results.
//!
//! # Workflow
//!
//! ```text
//! 1. Collect source files via walkdir (respecting skip-extensions config)
//! 2. StaticAnalyzer scores each file (pattern matching, no LLM cost)
//! 3. PromptRouter decides which files warrant an LLM call
//! 4. GrokClient::score_file() for each prioritised file (with cost cap)
//! 5. Aggregate findings into AuditResult
//! 6. Optionally append new findings to repo's todo.md via TodoFile::append_item
//! ```

use crate::audit::types::{
    AuditFinding, AuditRequest, AuditResponse, AuditSeverity, AuditStatus, AuditSummary,
    FindingCategory,
};
use crate::error::{AuditError, Result};
use crate::grok_client::{FileScoreResult, GrokClient};
use crate::static_analysis::{AnalysisRecommendation, StaticAnalyzer};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;
use tracing::{debug, info, warn};

// ============================================================================
// Configuration
// ============================================================================

/// Configuration for the audit runner
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditRunnerConfig {
    /// Maximum number of files to send to the LLM in one audit run
    pub max_llm_files: usize,
    /// Maximum cost budget in USD for LLM calls in one run
    pub max_cost_usd: f64,
    /// Whether to append new findings to the repo's `todo.md`
    pub append_to_todo_md: bool,
    /// Minimum static analysis score to escalate a file to the LLM (0.0–1.0)
    pub llm_escalation_threshold: f32,
    /// File extensions to skip entirely (binary, generated, model files)
    pub skip_extensions: Vec<String>,
    /// Path fragments to skip (vendor dirs, build output, etc.)
    pub skip_paths: Vec<String>,
    /// Maximum file size in bytes to analyse
    pub max_file_bytes: u64,
}

impl Default for AuditRunnerConfig {
    fn default() -> Self {
        Self {
            max_llm_files: 20,
            max_cost_usd: 0.50,
            append_to_todo_md: false,
            llm_escalation_threshold: 0.4,
            skip_extensions: vec![
                "onnx".into(),
                "pt".into(),
                "pth".into(),
                "bin".into(),
                "h5".into(),
                "safetensors".into(),
                "pkl".into(),
                "pb".into(),
                "tflite".into(),
                "ckpt".into(),
                "weights".into(),
                "npy".into(),
                "npz".into(),
                "lock".into(),
                "svg".into(),
                "png".into(),
                "jpg".into(),
                "jpeg".into(),
                "gif".into(),
                "ico".into(),
                "woff".into(),
                "woff2".into(),
                "ttf".into(),
                "eot".into(),
            ],
            skip_paths: vec![
                "target/".into(),
                "node_modules/".into(),
                ".git/".into(),
                "__pycache__/".into(),
                "build/".into(),
                "dist/".into(),
                "vendor/".into(),
                ".rustcode/cache/".into(),
            ],
            max_file_bytes: 256 * 1024, // 256 KiB
        }
    }
}

// ============================================================================
// AuditRunner
// ============================================================================

/// Orchestrates the full audit pipeline for a repository
pub struct AuditRunner {
    config: AuditRunnerConfig,
}

impl AuditRunner {
    pub fn new(config: AuditRunnerConfig) -> Self {
        Self { config }
    }

    pub fn with_defaults() -> Self {
        Self::new(AuditRunnerConfig::default())
    }

    /// Create a runner with a custom config and an optional Grok client.
    pub fn with_grok(config: AuditRunnerConfig, grok: Arc<GrokClient>) -> AuditRunnerWithGrok {
        AuditRunnerWithGrok {
            runner: Self { config },
            grok,
        }
    }

    // -----------------------------------------------------------------------
    // Public API
    // -----------------------------------------------------------------------

    /// Run a static-only audit on the repo path given in `request.repo`.
    ///
    /// This is the no-API-key path. It runs the same file collection and static
    /// analysis as [`run_static_only`] but accepts a full [`AuditRequest`] so
    /// that callers don't need to branch on whether they have a `GrokClient`.
    ///
    /// For LLM-assisted scoring use [`AuditRunner::with_grok`] to get an
    /// [`AuditRunnerWithGrok`] and call its `run()` method.
    pub async fn run(&self, request: AuditRequest) -> Result<AuditResponse> {
        let repo_path = std::path::PathBuf::from(&request.repo);
        let mut response = self.run_static_only(&repo_path).await?;
        // Carry the original request back in the response so callers can
        // inspect mode/filters without having to hold onto it separately.
        response.request = request;
        Ok(response)
    }

    /// Run only the static analysis stage (no LLM calls, free).
    ///
    /// Useful for the `/api/audit?mode=static` fast path.
    pub async fn run_static_only(&self, repo_path: impl AsRef<Path>) -> Result<AuditResponse> {
        let repo_path = repo_path.as_ref();
        let start = Instant::now();
        let run_id = uuid::Uuid::new_v4().to_string();

        let files = self.collect_files(repo_path)?;
        let analyzer = StaticAnalyzer::new();
        let mut all_findings: Vec<AuditFinding> = Vec::new();

        for rel_path in &files {
            let abs_path = repo_path.join(rel_path);
            match analyzer.analyze_file(&abs_path) {
                Ok(result) => {
                    if result.recommendation != AnalysisRecommendation::Skip {
                        let findings = self.findings_from_static(&result, rel_path);
                        all_findings.extend(findings);
                    }
                }
                Err(e) => {
                    debug!(path = %rel_path.display(), error = %e, "static analysis I/O error — skipping");
                }
            }
        }

        let summary = AuditSummary::from_findings(&all_findings);
        let duration = start.elapsed().as_secs_f64();

        Ok(AuditResponse {
            id: run_id,
            status: AuditStatus::Completed,
            requested_at: chrono::Utc::now(),
            completed_at: Some(chrono::Utc::now()),
            duration_secs: Some(duration),
            files_scanned: files.len(),
            findings: all_findings,
            summary,
            from_cache: false,
            estimated_cost_usd: 0.0,
            errors: Vec::new(),
            request: AuditRequest::default(),
        })
    }

    // -----------------------------------------------------------------------
    // Internal helpers
    // -----------------------------------------------------------------------

    /// Collect all source files under `repo_path` that pass the skip filters.
    ///
    /// Returns paths relative to `repo_path`, sorted for deterministic ordering.
    pub fn collect_files(&self, repo_path: &Path) -> Result<Vec<PathBuf>> {
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
                    debug!(path = %rel_str, size = meta.len(), "skipping oversized file");
                    continue;
                }
            }

            files.push(rel_path);
        }

        files.sort();
        Ok(files)
    }

    /// Run static analysis on a file and return a priority score (0.0–1.0).
    ///
    /// Higher scores indicate the file is more likely to have issues worth
    /// escalating to the LLM.
    pub fn static_score(&self, path: &Path, content: &str) -> f32 {
        let analyzer = StaticAnalyzer::new();
        let result = analyzer.analyze(&path.to_string_lossy(), content);
        result.estimated_llm_value as f32
    }

    /// Convert a `StaticAnalysisResult` into lightweight `AuditFinding`s
    /// (no LLM cost — severity is inferred from static signals).
    fn findings_from_static(
        &self,
        result: &crate::static_analysis::StaticAnalysisResult,
        rel_path: &Path,
    ) -> Vec<AuditFinding> {
        use crate::static_analysis::AnalysisRecommendation;

        let severity = match result.recommendation {
            AnalysisRecommendation::DeepDive => AuditSeverity::High,
            AnalysisRecommendation::Standard => AuditSeverity::Medium,
            AnalysisRecommendation::Minimal => AuditSeverity::Low,
            AnalysisRecommendation::Skip => return Vec::new(),
        };

        if result.static_issue_count == 0 && result.summary.is_empty() {
            return Vec::new();
        }

        vec![AuditFinding {
            id: format!("{:x}", md5::compute(rel_path.to_string_lossy().as_bytes())),
            severity,
            category: FindingCategory::CodeQuality,
            title: format!(
                "Static analysis: {} issue(s) in {}",
                result.static_issue_count,
                rel_path.display()
            ),
            description: result.summary.clone(),
            recommendation: "Review file with `cargo clippy` and the audit LLM pass.".to_string(),
            file: Some(rel_path.to_path_buf()),
            line: None,
            code_snippet: None,
            is_recurring: false,
            tags: vec!["static-analysis".to_string()],
            confidence: result.estimated_llm_value as f32,
        }]
    }

    /// Convert a `FileScoreResult` from GrokClient into `AuditFinding`s.
    pub fn finding_from_score(&self, file: &Path, score: &FileScoreResult) -> Vec<AuditFinding> {
        let mut findings = Vec::new();
        let file_str = file.to_string_lossy().into_owned();

        // Security finding
        if score.security_score < 70.0 {
            let severity = if score.security_score < 40.0 {
                AuditSeverity::Critical
            } else if score.security_score < 55.0 {
                AuditSeverity::High
            } else {
                AuditSeverity::Medium
            };
            findings.push(AuditFinding {
                id: format!("sec-{:x}", md5::compute(file_str.as_bytes())),
                severity,
                category: FindingCategory::Security,
                title: format!("Security concerns in {}", file_str),
                description: score.summary.clone(),
                recommendation: score.suggestions.first().cloned().unwrap_or_default(),
                file: Some(PathBuf::from(&file_str)),
                line: None,
                code_snippet: None,
                is_recurring: false,
                tags: vec!["security".to_string(), "llm-scored".to_string()],
                confidence: (100.0 - score.security_score) as f32 / 100.0,
            });
        }

        // Code quality finding
        if score.quality_score < 60.0 || score.complexity_score > 70.0 {
            let severity = if score.quality_score < 40.0 {
                AuditSeverity::High
            } else {
                AuditSeverity::Medium
            };
            findings.push(AuditFinding {
                id: format!("qual-{:x}", md5::compute(file_str.as_bytes())),
                severity,
                category: FindingCategory::CodeQuality,
                title: format!(
                    "Code quality issues in {} (score: {:.0}/100)",
                    file_str, score.quality_score
                ),
                description: format!(
                    "Quality: {:.0}/100, Complexity: {:.0}/100, Maintainability: {:.0}/100. {}",
                    score.quality_score,
                    score.complexity_score,
                    score.maintainability_score,
                    score.issues.first().cloned().unwrap_or_default()
                ),
                recommendation: score
                    .suggestions
                    .get(1)
                    .or_else(|| score.suggestions.first())
                    .cloned()
                    .unwrap_or_default(),
                file: Some(PathBuf::from(&file_str)),
                line: None,
                code_snippet: None,
                is_recurring: false,
                tags: vec!["quality".to_string(), "llm-scored".to_string()],
                confidence: (100.0 - score.quality_score) as f32 / 100.0,
            });
        }

        findings
    }
}

// ============================================================================
// AuditRunner with Grok (full pipeline)
// ============================================================================

/// `AuditRunner` with a wired `GrokClient` for LLM-assisted scoring.
pub struct AuditRunnerWithGrok {
    pub runner: AuditRunner,
    pub grok: Arc<GrokClient>,
}

impl AuditRunnerWithGrok {
    /// Run a full audit on a repository path.
    ///
    /// # Pipeline
    /// 1. Collect source files (respecting skip config)
    /// 2. Static analysis triage — cheap, no LLM
    /// 3. Sort by `estimated_llm_value`, cap at `max_llm_files`
    /// 4. For each prioritised file: call `GrokClient::score_file` (with cost cap)
    /// 5. Aggregate `FileScoreResult` → `AuditFinding`
    /// 6. Optionally append High/Critical findings to `todo.md`
    /// 7. Return `AuditResponse`
    pub async fn run(&self, request: AuditRequest) -> Result<AuditResponse> {
        let repo_path = PathBuf::from(&request.repo);
        let start = Instant::now();
        let run_id = uuid::Uuid::new_v4().to_string();

        info!(
            run_id = %run_id,
            repo = %request.repo,
            mode = %request.mode,
            "AuditRunner starting"
        );

        // ------------------------------------------------------------------
        // Step 1: collect files
        // ------------------------------------------------------------------
        let mut files = self.runner.collect_files(&repo_path).map_err(|e| {
            AuditError::other(format!(
                "Failed to collect files in {}: {}",
                request.repo, e
            ))
        })?;

        // Apply request-level exclusion patterns
        if !request.exclude_patterns.is_empty() {
            files.retain(|f| {
                let s = f.to_string_lossy();
                !request
                    .exclude_patterns
                    .iter()
                    .any(|pat| s.contains(pat.as_str()))
            });
        }

        let total_files = files.len();
        info!(run_id = %run_id, total_files, "File collection complete");

        // ------------------------------------------------------------------
        // Step 2: static triage
        // ------------------------------------------------------------------
        let analyzer = StaticAnalyzer::new();
        let mut scored: Vec<(PathBuf, f64)> = Vec::new(); // (rel_path, llm_value)
        let mut static_findings: Vec<AuditFinding> = Vec::new();
        let mut errors: Vec<String> = Vec::new();

        for rel_path in &files {
            let abs_path = repo_path.join(rel_path);
            match analyzer.analyze_file(&abs_path) {
                Ok(result) => {
                    if result.recommendation != AnalysisRecommendation::Skip {
                        scored.push((rel_path.clone(), result.estimated_llm_value));
                        let sf = self.runner.findings_from_static(&result, rel_path);
                        static_findings.extend(sf);
                    }
                }
                Err(e) => {
                    let msg = format!("{}: {}", rel_path.display(), e);
                    debug!(error = %msg, "static analysis I/O error");
                    errors.push(msg);
                }
            }
        }

        // ------------------------------------------------------------------
        // Step 3: rank + cap for LLM escalation
        // ------------------------------------------------------------------
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

        let escalation_threshold = self.runner.config.llm_escalation_threshold as f64;
        let max_llm = if request.max_files > 0 {
            request.max_files.min(self.runner.config.max_llm_files)
        } else {
            self.runner.config.max_llm_files
        };

        let escalated: Vec<PathBuf> = scored
            .into_iter()
            .filter(|(_, score)| *score >= escalation_threshold)
            .take(max_llm)
            .map(|(p, _)| p)
            .collect();

        info!(
            run_id = %run_id,
            escalated = escalated.len(),
            threshold = escalation_threshold,
            "LLM escalation queue built"
        );

        // ------------------------------------------------------------------
        // Step 4: LLM scoring with cost cap
        // ------------------------------------------------------------------
        let mut llm_findings: Vec<AuditFinding> = Vec::new();
        let mut total_cost: f64 = 0.0;

        for rel_path in &escalated {
            if total_cost >= self.runner.config.max_cost_usd {
                warn!(
                    run_id = %run_id,
                    cost = total_cost,
                    budget = self.runner.config.max_cost_usd,
                    "Cost budget reached — stopping LLM escalation"
                );
                break;
            }

            let abs_path = repo_path.join(rel_path);
            let content = match std::fs::read_to_string(&abs_path) {
                Ok(c) => c,
                Err(e) => {
                    let msg = format!("read {}: {}", rel_path.display(), e);
                    debug!(error = %msg, "skipping file for LLM scoring");
                    errors.push(msg);
                    continue;
                }
            };

            match self
                .grok
                .score_file(&rel_path.to_string_lossy(), &content)
                .await
            {
                Ok(score) => {
                    // Estimate cost from token count proxy (content length)
                    let estimated_tokens = (content.len() / 4) as f64;
                    let cost_estimate = estimated_tokens / 1_000_000.0 * 5.0; // ~$5/M tokens
                    total_cost += cost_estimate;

                    debug!(
                        path = %rel_path.display(),
                        overall = score.overall_score,
                        security = score.security_score,
                        quality = score.quality_score,
                        cost_so_far = total_cost,
                        "LLM scored file"
                    );

                    let findings = self.runner.finding_from_score(rel_path, &score);
                    llm_findings.extend(findings);
                }
                Err(e) => {
                    let msg = format!("LLM score {}: {}", rel_path.display(), e);
                    warn!(error = %msg, "GrokClient::score_file failed");
                    errors.push(msg);
                }
            }
        }

        // ------------------------------------------------------------------
        // Step 5: merge and filter by min_severity
        // ------------------------------------------------------------------
        let mut all_findings: Vec<AuditFinding> = static_findings
            .into_iter()
            .chain(llm_findings)
            .filter(|f| f.severity >= request.min_severity)
            .collect();

        // Deduplicate by ID
        all_findings.sort_by(|a, b| a.id.cmp(&b.id));
        all_findings.dedup_by_key(|f| f.id.clone());

        // Sort: Critical first, then High, Medium, Low, Info
        all_findings.sort_by(|a, b| {
            b.severity
                .partial_cmp(&a.severity)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        // ------------------------------------------------------------------
        // Step 6: optionally append to todo.md
        // ------------------------------------------------------------------
        if request.append_to_todo && !all_findings.is_empty() {
            let todo_path = repo_path.join("todo.md");
            if todo_path.exists() {
                let high_plus: Vec<&AuditFinding> = all_findings
                    .iter()
                    .filter(|f| f.severity >= AuditSeverity::High)
                    .collect();

                if !high_plus.is_empty() {
                    match append_findings_to_todo(&todo_path, &high_plus) {
                        Ok(n) => {
                            info!(run_id = %run_id, appended = n, "Appended findings to todo.md")
                        }
                        Err(e) => {
                            warn!(run_id = %run_id, error = %e, "Failed to append to todo.md")
                        }
                    }
                }
            }
        }

        // ------------------------------------------------------------------
        // Step 7: build response
        // ------------------------------------------------------------------
        let summary = AuditSummary::from_findings(&all_findings);
        let duration = start.elapsed().as_secs_f64();

        info!(
            run_id = %run_id,
            files_scanned = total_files,
            findings = all_findings.len(),
            duration_secs = duration,
            cost_usd = total_cost,
            "AuditRunner complete"
        );

        Ok(AuditResponse {
            id: run_id,
            status: if errors.is_empty() {
                AuditStatus::Completed
            } else {
                AuditStatus::CompletedWithErrors
            },
            requested_at: chrono::Utc::now(),
            completed_at: Some(chrono::Utc::now()),
            duration_secs: Some(duration),
            files_scanned: total_files,
            findings: all_findings,
            summary,
            from_cache: false,
            estimated_cost_usd: total_cost,
            errors,
            request,
        })
    }
}

// ============================================================================
// Helpers
// ============================================================================

/// Append High/Critical audit findings to a `todo.md` file as new backlog items.
fn append_findings_to_todo(todo_path: &Path, findings: &[&AuditFinding]) -> Result<usize> {
    use std::io::Write;

    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(todo_path)
        .map_err(|e| AuditError::other(format!("Cannot open todo.md: {}", e)))?;

    let timestamp = chrono::Utc::now().format("%Y-%m-%d");
    writeln!(file, "\n\n### Audit Findings — {}\n", timestamp)
        .map_err(|e| AuditError::other(format!("Write error: {}", e)))?;

    let mut count = 0;
    for finding in findings {
        let loc = finding
            .file
            .as_deref()
            .map(|f| {
                if let Some(line) = finding.line {
                    format!(" (`{}:{}`)", f.display(), line)
                } else {
                    format!(" (`{}`)", f.display())
                }
            })
            .unwrap_or_default();

        writeln!(
            file,
            "- [ ] **[{}]** {}{} — {}",
            finding.severity.as_str().to_uppercase(),
            finding.title,
            loc,
            finding.description
        )
        .map_err(|e| AuditError::other(format!("Write error: {}", e)))?;
        count += 1;
    }

    Ok(count)
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_runner_config_defaults() {
        let cfg = AuditRunnerConfig::default();
        assert_eq!(cfg.max_llm_files, 20);
        assert!(cfg.max_cost_usd > 0.0);
        assert!(!cfg.append_to_todo_md);
        assert!(cfg.skip_extensions.contains(&"onnx".to_string()));
        assert!(cfg.skip_paths.contains(&"target/".to_string()));
    }

    #[test]
    fn test_runner_with_defaults() {
        let runner = AuditRunner::with_defaults();
        assert_eq!(runner.config.max_llm_files, 20);
    }

    #[tokio::test]
    async fn test_run_delegates_to_static_only() {
        let runner = AuditRunner::with_defaults();
        // Point at a real directory so file collection succeeds (may find 0
        // files if the path doesn't exist — that's fine, we just want Ok).
        let req = AuditRequest {
            repo: std::env::temp_dir().to_string_lossy().to_string(),
            ..AuditRequest::default()
        };
        let result = runner.run(req).await;
        // Should succeed (static-only path, no LLM needed).
        assert!(result.is_ok(), "run() should succeed without a GrokClient");
        let response = result.unwrap();
        assert_eq!(response.estimated_cost_usd, 0.0, "no LLM cost expected");
    }

    #[tokio::test]
    async fn test_run_static_only_succeeds_on_real_dir() {
        let runner = AuditRunner::with_defaults();
        // /tmp always exists; may have 0 scannable files but should not error.
        let result = runner.run_static_only(std::env::temp_dir()).await;
        assert!(
            result.is_ok(),
            "run_static_only() should succeed on a real directory"
        );
        let response = result.unwrap();
        assert_eq!(
            response.estimated_cost_usd, 0.0,
            "static-only path must never incur LLM cost"
        );
    }

    #[test]
    fn test_collect_files_returns_entries_for_real_dir() {
        let runner = AuditRunner::with_defaults();
        // /tmp always exists; result may be empty but must not error.
        let result = runner.collect_files(Path::new(std::env::temp_dir().to_str().unwrap()));
        assert!(result.is_ok(), "collect_files() should not error on /tmp");
    }

    #[test]
    fn test_static_score_returns_float() {
        let runner = AuditRunner::with_defaults();
        let score = runner.static_score(Path::new("src/lib.rs"), "fn main() {}");
        // Score is in 0.0–1.0 range — just ensure it's a valid f32.
        assert!(
            score.is_finite(),
            "static_score() should return a finite f32"
        );
    }

    #[test]
    fn test_skip_extensions_config_is_comprehensive() {
        let cfg = AuditRunnerConfig::default();
        // Ensure all the known binary model extensions are in the skip list
        for ext in &["onnx", "pt", "pth", "safetensors", "pkl"] {
            assert!(
                cfg.skip_extensions.contains(&ext.to_string()),
                "missing skip extension: {}",
                ext
            );
        }
    }

    #[test]
    fn test_config_serialise_round_trip() {
        let cfg = AuditRunnerConfig::default();
        let json = serde_json::to_string(&cfg).unwrap();
        let back: AuditRunnerConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(back.max_llm_files, cfg.max_llm_files);
        assert_eq!(back.skip_extensions.len(), cfg.skip_extensions.len());
    }
}
