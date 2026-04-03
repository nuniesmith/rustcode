//! Audit report — render audit findings to Markdown and JSON
//!
//! Transforms a completed `AuditResponse` into human-readable Markdown (for
//! committing to `docs/audit/`) or structured JSON (for downstream tooling).
//!
//! # Usage
//!
//! ```rust,ignore
//! let report = AuditReport::new(response, ReportFormat::Markdown);
//! report.save_to("docs/audit/2024-01-01-my-repo.md")?;
//! println!("{}", report.render()?);
//! ```
//!
//! # TODO(scaffolder): implement
//!
//! Implement the `render_markdown` and `render_json` methods once
//! `AuditResponse` and `AuditFinding` are defined in `src/audit/types.rs`.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::fs;
use std::path::Path;

use crate::error::{AuditError, Result};

// ============================================================================
// Format selector
// ============================================================================

/// Output format for a rendered audit report
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ReportFormat {
    /// Human-readable Markdown — committed to `docs/audit/`
    #[default]
    Markdown,
    /// Structured JSON — consumed by downstream tooling / workflow steps
    Json,
    /// Both Markdown and JSON side-by-side
    Both,
}

impl fmt::Display for ReportFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ReportFormat::Markdown => write!(f, "markdown"),
            ReportFormat::Json => write!(f, "json"),
            ReportFormat::Both => write!(f, "both"),
        }
    }
}

impl std::str::FromStr for ReportFormat {
    type Err = ();
    fn from_str(s: &str) -> std::result::Result<Self, ()> {
        match s.to_ascii_lowercase().as_str() {
            "markdown" | "md" => Ok(ReportFormat::Markdown),
            "json" => Ok(ReportFormat::Json),
            "both" => Ok(ReportFormat::Both),
            _ => Ok(ReportFormat::Markdown),
        }
    }
}

// ============================================================================
// Severity
// ============================================================================

/// Re-exported here so callers don't need to import both `types` and `report`
pub use crate::audit::types::AuditSeverity;

// ============================================================================
// Report configuration
// ============================================================================

/// Configuration for report generation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReportConfig {
    /// Output format
    pub format: ReportFormat,
    /// Whether to include the full raw LLM response in the output
    pub include_raw_response: bool,
    /// Whether to include a summary table at the top
    pub include_summary_table: bool,
    /// Whether to group findings by file
    pub group_by_file: bool,
    /// Whether to group findings by severity
    pub group_by_severity: bool,
    /// Minimum severity to include in the report
    pub min_severity: AuditSeverity,
    /// Maximum number of findings to include (0 = unlimited)
    pub max_findings: usize,
    /// Repository name shown in the report header
    pub repo_name: Option<String>,
    /// Optional link to the repo (used in Markdown headers)
    pub repo_url: Option<String>,
}

impl Default for ReportConfig {
    fn default() -> Self {
        Self {
            format: ReportFormat::Markdown,
            include_raw_response: false,
            include_summary_table: true,
            group_by_file: true,
            group_by_severity: false,
            min_severity: AuditSeverity::Info,
            max_findings: 0,
            repo_name: None,
            repo_url: None,
        }
    }
}

// ============================================================================
// AuditReport
// ============================================================================

/// A rendered audit report ready to be written to disk
#[derive(Debug, Clone)]
pub struct AuditReport {
    /// The underlying response data
    pub response: crate::audit::types::AuditResponse,
    /// Rendering configuration
    pub config: ReportConfig,
}

impl AuditReport {
    /// Create a new report with default config
    pub fn new(response: crate::audit::types::AuditResponse) -> Self {
        Self {
            response,
            config: ReportConfig::default(),
        }
    }

    /// Create a new report with explicit config
    pub fn with_config(response: crate::audit::types::AuditResponse, config: ReportConfig) -> Self {
        Self { response, config }
    }

    // -----------------------------------------------------------------------
    // Rendering
    // -----------------------------------------------------------------------

    /// Render the report to a string in the configured format.
    ///
    /// Returns the rendered Markdown when format is `Markdown` or `Both`.
    /// Returns JSON when format is `Json`.
    /// Returns Markdown + `\n---\n` + JSON when format is `Both`.
    pub fn render(&self) -> Result<String> {
        match self.config.format {
            ReportFormat::Markdown => self.render_markdown(),
            ReportFormat::Json => self.render_json(),
            ReportFormat::Both => {
                let md = self.render_markdown()?;
                let json = self.render_json()?;
                Ok(format!("{}\n\n---\n\n```json\n{}\n```\n", md, json))
            }
        }
    }

    /// Render to Markdown
    pub fn render_markdown(&self) -> Result<String> {
        // TODO(scaffolder): implement full Markdown rendering once AuditResponse
        // fields are finalised in src/audit/types.rs.
        //
        // Planned structure:
        //
        //   # Audit Report — <repo_name>
        //   > Generated: <timestamp> | Model: <model> | Status: <status>
        //
        //   ## Summary
        //   | Severity | Count |
        //   |----------|-------|
        //   | Critical |   2   |
        //   | High     |   5   |
        //   ...
        //
        //   ## Findings
        //   ### `src/api/handlers.rs`
        //   #### [HIGH] Unsanitised query parameter at line 132
        //   > **Recommendation:** ...
        //
        //   ## Metadata
        //   - Files scanned: N
        //   - Duration: Xs
        //   - Cost: $0.00XX

        let repo = self
            .config
            .repo_name
            .as_deref()
            .unwrap_or("Unknown Repository");

        let ts = self
            .response
            .completed_at
            .map(|t| t.format("%Y-%m-%d %H:%M UTC").to_string())
            .unwrap_or_else(|| "—".to_string());

        let status = format!("{}", self.response.status);

        let mut md = String::new();

        // Header
        if let Some(ref url) = self.config.repo_url {
            md.push_str(&format!("# Audit Report — [{}]({})\n\n", repo, url));
        } else {
            md.push_str(&format!("# Audit Report — {}\n\n", repo));
        }
        md.push_str(&format!("> Generated: {} | Status: {}\n\n", ts, status));

        // Summary table
        if self.config.include_summary_table {
            md.push_str("## Summary\n\n");
            md.push_str("| Severity | Count |\n");
            md.push_str("|----------|-------|\n");

            for (severity, count) in self.severity_counts() {
                md.push_str(&format!("| {} | {} |\n", severity, count));
            }
            md.push('\n');
        }

        // Findings
        let findings = self.filtered_findings();

        if findings.is_empty() {
            md.push_str("## Findings\n\n_No findings above the minimum severity threshold._\n\n");
        } else {
            md.push_str("## Findings\n\n");

            if self.config.group_by_file {
                // Group by file
                let mut by_file: std::collections::HashMap<
                    String,
                    Vec<&crate::audit::types::AuditFinding>,
                > = std::collections::HashMap::new();

                for finding in &findings {
                    let key = finding
                        .file
                        .as_ref()
                        .map(|p| p.display().to_string())
                        .unwrap_or_else(|| "(unknown)".to_string());
                    by_file.entry(key).or_default().push(finding);
                }

                let mut file_keys: Vec<String> = by_file.keys().cloned().collect();
                file_keys.sort();

                for file in &file_keys {
                    md.push_str(&format!("### `{}`\n\n", file));
                    for finding in &by_file[file] {
                        md.push_str(&render_finding_markdown(finding));
                    }
                }
            } else if self.config.group_by_severity {
                for sev in severity_order() {
                    let sev_findings: Vec<&crate::audit::types::AuditFinding> = findings
                        .iter()
                        .filter(|f| f.severity == sev)
                        .copied()
                        .collect();

                    if !sev_findings.is_empty() {
                        md.push_str(&format!("### {} Severity\n\n", sev));
                        for finding in sev_findings {
                            md.push_str(&render_finding_markdown(finding));
                        }
                    }
                }
            } else {
                for finding in &findings {
                    md.push_str(&render_finding_markdown(finding));
                }
            }
        }

        // Metadata footer
        md.push_str("## Metadata\n\n");
        md.push_str(&format!(
            "- Files scanned: {}\n",
            self.response.files_scanned
        ));
        if let Some(dur) = self.response.duration_secs {
            md.push_str(&format!("- Duration: {:.1}s\n", dur));
        }
        md.push_str(&format!(
            "- Estimated cost: ${:.4}\n",
            self.response.estimated_cost_usd
        ));

        Ok(md)
    }

    /// Render to compact JSON
    pub fn render_json(&self) -> Result<String> {
        serde_json::to_string_pretty(&self.response)
            .map_err(|e| AuditError::other(format!("JSON render error: {}", e)))
    }

    // -----------------------------------------------------------------------
    // Disk I/O
    // -----------------------------------------------------------------------

    /// Write the rendered report to a file on disk.
    ///
    /// Creates parent directories if they do not exist.
    pub fn save_to(&self, path: impl AsRef<Path>) -> Result<()> {
        let path = path.as_ref();
        let content = self.render()?;

        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(AuditError::Io)?;
        }

        fs::write(path, &content).map_err(AuditError::Io)?;
        tracing::info!("Audit report written to {}", path.display());
        Ok(())
    }

    /// Save Markdown and JSON side-by-side.
    ///
    /// Given `base_path = "docs/audit/report"`, writes:
    /// - `docs/audit/report.md`
    /// - `docs/audit/report.json`
    pub fn save_both(&self, base_path: impl AsRef<Path>) -> Result<()> {
        let base = base_path.as_ref();
        let md_path = base.with_extension("md");
        let json_path = base.with_extension("json");

        let md = self.render_markdown()?;
        let json = self.render_json()?;

        if let Some(parent) = md_path.parent() {
            fs::create_dir_all(parent).map_err(AuditError::Io)?;
        }

        fs::write(&md_path, &md).map_err(AuditError::Io)?;
        fs::write(&json_path, &json).map_err(AuditError::Io)?;

        tracing::info!(
            "Audit report written to {} and {}",
            md_path.display(),
            json_path.display()
        );
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Helpers
    // -----------------------------------------------------------------------

    /// Return findings filtered by minimum severity and capped by max_findings
    fn filtered_findings(&self) -> Vec<&crate::audit::types::AuditFinding> {
        let min = self.config.min_severity;
        let max = self.config.max_findings;
        let mut findings: Vec<&crate::audit::types::AuditFinding> = self
            .response
            .findings
            .iter()
            .filter(|f| f.severity >= min)
            .collect();

        // Sort: critical first, then high, medium, low, info
        findings.sort_by_key(|f| severity_sort_key(f.severity));

        if max > 0 {
            findings.truncate(max);
        }

        findings
    }

    /// Count findings grouped by severity
    fn severity_counts(&self) -> Vec<(AuditSeverity, usize)> {
        let mut counts: std::collections::HashMap<AuditSeverity, usize> =
            std::collections::HashMap::new();

        for finding in &self.response.findings {
            *counts.entry(finding.severity).or_insert(0) += 1;
        }

        let result: Vec<(AuditSeverity, usize)> = severity_order()
            .into_iter()
            .filter_map(|sev| counts.remove(&sev).map(|c| (sev, c)))
            .collect();

        result
    }
}

// ============================================================================
// Free functions
// ============================================================================

/// Generate a default filename for an audit report.
///
/// Format: `YYYY-MM-DD-<repo-slug>.md`
pub fn default_report_filename(repo_name: &str, format: ReportFormat) -> String {
    let slug = repo_name
        .to_ascii_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect::<String>();

    let date = chrono::Utc::now().format("%Y-%m-%d");

    let ext = match format {
        ReportFormat::Json => "json",
        _ => "md",
    };

    format!("{}-{}.{}", date, slug, ext)
}

/// Render a single finding as a Markdown section
fn render_finding_markdown(finding: &crate::audit::types::AuditFinding) -> String {
    let severity_badge = match finding.severity {
        AuditSeverity::Critical => "🔴 **CRITICAL**",
        AuditSeverity::High => "🟠 **HIGH**",
        AuditSeverity::Medium => "🟡 **MEDIUM**",
        AuditSeverity::Low => "🟢 **LOW**",
        AuditSeverity::Info => "🔵 **INFO**",
    };

    let location = match (&finding.file, finding.line) {
        (Some(f), Some(l)) => format!("`{}:{}`", f.display(), l),
        (Some(f), None) => format!("`{}`", f.display()),
        (None, _) => "_(no location)_".to_string(),
    };

    let mut md = String::new();
    md.push_str(&format!(
        "#### {} — {} {}\n\n",
        severity_badge, finding.title, location
    ));
    md.push_str(&format!("{}\n\n", finding.description));

    md.push_str(&format!(
        "> **Recommendation:** {}\n\n",
        finding.recommendation
    ));

    if let Some(ref snippet) = finding.code_snippet {
        md.push_str(&format!("```\n{}\n```\n\n", snippet));
    }

    md
}

/// Return severities in descending order (critical first)
fn severity_order() -> Vec<AuditSeverity> {
    vec![
        AuditSeverity::Critical,
        AuditSeverity::High,
        AuditSeverity::Medium,
        AuditSeverity::Low,
        AuditSeverity::Info,
    ]
}

/// Numeric sort key — lower = higher priority (critical = 0)
fn severity_sort_key(sev: AuditSeverity) -> u8 {
    match sev {
        AuditSeverity::Critical => 0,
        AuditSeverity::High => 1,
        AuditSeverity::Medium => 2,
        AuditSeverity::Low => 3,
        AuditSeverity::Info => 4,
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audit::types::{
        AuditFinding, AuditRequest, AuditResponse, AuditSeverity, AuditStatus, AuditSummary,
        FindingCategory,
    };
    use chrono::Utc;

    use std::path::PathBuf;

    fn make_finding(
        id: &str,
        severity: AuditSeverity,
        title: &str,
        file: &str,
        line: usize,
    ) -> AuditFinding {
        AuditFinding {
            id: id.to_string(),
            severity,
            category: FindingCategory::CodeQuality,
            title: title.to_string(),
            description: format!("Description for {}", title),
            recommendation: format!("Recommendation for {}", title),
            file: Some(PathBuf::from(file)),
            line: Some(line),
            code_snippet: None,
            is_recurring: false,
            tags: vec![],
            confidence: 0.9,
        }
    }

    fn sample_response() -> AuditResponse {
        let findings = vec![
            make_finding(
                "f001",
                AuditSeverity::High,
                "Unsanitised input",
                "src/api/handlers.rs",
                132,
            ),
            make_finding(
                "f002",
                AuditSeverity::Low,
                "Missing docs on public function",
                "src/lib.rs",
                42,
            ),
        ];
        let summary = AuditSummary::from_findings(&findings);
        AuditResponse {
            id: "test-001".to_string(),
            status: AuditStatus::Completed,
            requested_at: Utc::now(),
            completed_at: Some(Utc::now()),
            duration_secs: Some(3.7),
            files_scanned: 24,
            findings,
            summary,
            from_cache: false,
            estimated_cost_usd: 0.0012,
            errors: vec![],
            request: AuditRequest::default(),
        }
    }

    #[test]
    fn test_render_markdown_basic() {
        let report = AuditReport::new(sample_response());
        let md = report.render_markdown().unwrap();

        assert!(md.contains("# Audit Report"));
        assert!(md.contains("## Summary"));
        assert!(md.contains("## Findings"));
        assert!(md.contains("Unsanitised input"));
        assert!(md.contains("## Metadata"));
        assert!(md.contains("Files scanned: 24"));
    }

    #[test]
    fn test_render_json_is_valid() {
        let report = AuditReport::new(sample_response());
        let json = report.render_json().unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["id"], "test-001");
        assert_eq!(parsed["findings"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn test_render_both_contains_both() {
        let cfg = ReportConfig {
            format: ReportFormat::Both,
            ..ReportConfig::default()
        };
        let report = AuditReport::with_config(sample_response(), cfg);
        let output = report.render().unwrap();
        assert!(output.contains("# Audit Report"));
        assert!(output.contains("```json"));
        assert!(output.contains("\"id\""));
    }

    #[test]
    fn test_min_severity_filter() {
        let cfg = ReportConfig {
            min_severity: AuditSeverity::High,
            ..ReportConfig::default()
        };
        let report = AuditReport::with_config(sample_response(), cfg);

        let findings = report.filtered_findings();
        // Only the High finding should pass; Low is filtered out
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].severity, AuditSeverity::High);
    }

    #[test]
    fn test_max_findings_cap() {
        let cfg = ReportConfig {
            max_findings: 1,
            ..ReportConfig::default()
        };
        let report = AuditReport::with_config(sample_response(), cfg);

        let findings = report.filtered_findings();
        assert_eq!(findings.len(), 1);
    }

    #[test]
    fn test_severity_counts() {
        let report = AuditReport::new(sample_response());
        let counts = report.severity_counts();

        let high = counts.iter().find(|(s, _)| *s == AuditSeverity::High);
        let low = counts.iter().find(|(s, _)| *s == AuditSeverity::Low);

        assert_eq!(high.map(|(_, c)| *c), Some(1));
        assert_eq!(low.map(|(_, c)| *c), Some(1));
    }

    #[test]
    fn test_findings_sorted_by_severity() {
        let report = AuditReport::new(sample_response());
        let findings = report.filtered_findings();
        // High comes before Low in the sorted output
        assert_eq!(findings[0].severity, AuditSeverity::High);
        assert_eq!(findings[1].severity, AuditSeverity::Low);
    }

    #[test]
    fn test_group_by_file_renders_file_headers() {
        let cfg = ReportConfig {
            group_by_file: true,
            ..ReportConfig::default()
        };
        let report = AuditReport::with_config(sample_response(), cfg);
        let md = report.render_markdown().unwrap();

        assert!(md.contains("src/api/handlers.rs") || md.contains("handlers.rs"));
        assert!(md.contains("src/lib.rs") || md.contains("lib.rs"));
    }

    #[test]
    fn test_default_report_filename() {
        let name = default_report_filename("my-repo", ReportFormat::Markdown);
        assert!(name.ends_with(".md"));
        assert!(name.contains("my-repo"));

        let json_name = default_report_filename("my-repo", ReportFormat::Json);
        assert!(json_name.ends_with(".json"));
    }

    #[test]
    fn test_save_to_creates_parent_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/dir/report.md");

        let report = AuditReport::new(sample_response());
        report.save_to(&path).unwrap();

        assert!(path.exists());
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("# Audit Report"));
    }

    #[test]
    fn test_save_both_creates_md_and_json() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().join("report");

        let report = AuditReport::new(sample_response());
        report.save_both(&base).unwrap();

        assert!(base.with_extension("md").exists());
        assert!(base.with_extension("json").exists());
    }

    #[test]
    fn test_report_format_parse() {
        use std::str::FromStr;
        assert_eq!(
            ReportFormat::from_str("markdown").unwrap(),
            ReportFormat::Markdown
        );
        assert_eq!(
            ReportFormat::from_str("md").unwrap(),
            ReportFormat::Markdown
        );
        assert_eq!(ReportFormat::from_str("json").unwrap(), ReportFormat::Json);
        assert_eq!(ReportFormat::from_str("both").unwrap(), ReportFormat::Both);
        // Unknown defaults to Markdown
        assert_eq!(
            ReportFormat::from_str("xml").unwrap(),
            ReportFormat::Markdown
        );
    }

    #[test]
    fn test_render_finding_markdown_with_all_fields() {
        let finding = AuditFinding {
            id: "f999".to_string(),
            severity: AuditSeverity::Critical,
            category: FindingCategory::Security,
            title: "Critical vuln".to_string(),
            description: "This is very bad.".to_string(),
            recommendation: "Fix immediately".to_string(),
            file: Some(PathBuf::from("src/main.rs")),
            line: Some(1),
            code_snippet: Some("unsafe { *ptr }".to_string()),
            is_recurring: false,
            tags: vec![],
            confidence: 1.0,
        };

        let md = render_finding_markdown(&finding);
        assert!(md.contains("CRITICAL"));
        assert!(md.contains("src/main.rs"));
        assert!(md.contains("Fix immediately"));
        assert!(md.contains("unsafe { *ptr }"));
    }

    #[test]
    fn test_render_finding_markdown_minimal() {
        let finding = AuditFinding {
            id: "f000".to_string(),
            severity: AuditSeverity::Info,
            category: FindingCategory::Documentation,
            title: "Just a note".to_string(),
            description: "Nothing urgent.".to_string(),
            recommendation: "No action needed".to_string(),
            file: None,
            line: None,
            code_snippet: None,
            is_recurring: false,
            tags: vec![],
            confidence: 0.5,
        };

        let md = render_finding_markdown(&finding);
        assert!(md.contains("INFO"));
        assert!(md.contains("no location"));
    }
}
