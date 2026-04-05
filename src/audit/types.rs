// Audit types — request/response structs for the `/api/audit` endpoint
//
// These types are shared across all audit sub-modules:
// - `endpoint`  — deserialises `AuditRequest`, serialises `AuditResponse`
// - `runner`    — produces `Vec<AuditFinding>` during the audit run
// - `report`    — renders `AuditReport` from findings
// - `cache`     — keys the Redis cache on `AuditRequest` fields
//
// # TODO(scaffolder): implement
//
// The structs below are complete enough for compilation and wiring.
// Flesh out the fields once the runner logic is clearer.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

// ============================================================================
// Severity
// ============================================================================

// How serious a finding is
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AuditSeverity {
    // Informational — no action required
    Info,
    // Low — worth noting but not urgent
    Low,
    // Medium — should be addressed in the next planning cycle
    Medium,
    // High — address before next release
    High,
    // Critical — address immediately; blocks deployment
    Critical,
}

impl AuditSeverity {
    pub fn as_str(self) -> &'static str {
        match self {
            AuditSeverity::Info => "info",
            AuditSeverity::Low => "low",
            AuditSeverity::Medium => "medium",
            AuditSeverity::High => "high",
            AuditSeverity::Critical => "critical",
        }
    }

    // Emoji indicator used in Markdown reports
    pub fn emoji(self) -> &'static str {
        match self {
            AuditSeverity::Info => "ℹ️",
            AuditSeverity::Low => "🟢",
            AuditSeverity::Medium => "🟡",
            AuditSeverity::High => "🔴",
            AuditSeverity::Critical => "🚨",
        }
    }
}

impl std::fmt::Display for AuditSeverity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl std::str::FromStr for AuditSeverity {
    type Err = ();
    fn from_str(s: &str) -> std::result::Result<Self, ()> {
        match s.to_ascii_lowercase().as_str() {
            "info" => Ok(AuditSeverity::Info),
            "low" => Ok(AuditSeverity::Low),
            "medium" => Ok(AuditSeverity::Medium),
            "high" => Ok(AuditSeverity::High),
            "critical" => Ok(AuditSeverity::Critical),
            _ => Err(()),
        }
    }
}

// ============================================================================
// Status
// ============================================================================

// Overall status of an audit run
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum AuditStatus {
    // Audit has been queued but not started
    #[default]
    Pending,
    // Audit is currently running
    Running,
    // Audit completed successfully
    Completed,
    // Audit completed but with non-fatal errors
    CompletedWithErrors,
    // Audit failed entirely
    Failed,
    // Result was served from cache (no new LLM calls made)
    Cached,
}

impl AuditStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            AuditStatus::Pending => "pending",
            AuditStatus::Running => "running",
            AuditStatus::Completed => "completed",
            AuditStatus::CompletedWithErrors => "completed_with_errors",
            AuditStatus::Failed => "failed",
            AuditStatus::Cached => "cached",
        }
    }

    // Whether this status represents a terminal state
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            AuditStatus::Completed
                | AuditStatus::CompletedWithErrors
                | AuditStatus::Failed
                | AuditStatus::Cached
        )
    }
}

impl std::fmt::Display for AuditStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

// ============================================================================
// Finding category
// ============================================================================

// What aspect of the codebase a finding relates to
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FindingCategory {
    Security,
    CodeQuality,
    Performance,
    Architecture,
    Documentation,
    Testing,
    Dependencies,
    Configuration,
    Other,
}

impl FindingCategory {
    pub fn as_str(self) -> &'static str {
        match self {
            FindingCategory::Security => "security",
            FindingCategory::CodeQuality => "code_quality",
            FindingCategory::Performance => "performance",
            FindingCategory::Architecture => "architecture",
            FindingCategory::Documentation => "documentation",
            FindingCategory::Testing => "testing",
            FindingCategory::Dependencies => "dependencies",
            FindingCategory::Configuration => "configuration",
            FindingCategory::Other => "other",
        }
    }
}

impl std::fmt::Display for FindingCategory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

// ============================================================================
// AuditFinding — a single issue discovered during the audit
// ============================================================================

// A single finding from the audit run
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditFinding {
    // Stable 8-char hex ID (CRC32 of file+line+title)
    pub id: String,
    // Severity level
    pub severity: AuditSeverity,
    // Category of the finding
    pub category: FindingCategory,
    // Short title (used as the TODO item text if appended to todo.md)
    pub title: String,
    // Detailed description of the problem
    pub description: String,
    // Suggested fix or next action
    pub recommendation: String,
    // Relative path to the affected file, if applicable
    pub file: Option<PathBuf>,
    // 1-based line number, if applicable
    pub line: Option<usize>,
    // Code snippet showing the problematic area
    pub code_snippet: Option<String>,
    // Whether this finding was already present in a previous audit run
    pub is_recurring: bool,
    // Tags for grouping (e.g. `["auth", "input-validation"]`)
    pub tags: Vec<String>,
    // LLM confidence score 0.0–1.0 for this finding
    pub confidence: f32,
}

impl AuditFinding {
    // Whether this finding is severe enough to fail a CI gate
    pub fn is_blocking(&self) -> bool {
        self.severity >= AuditSeverity::High
    }

    // Format as a `todo.md` list item
    pub fn to_todo_item_text(&self) -> String {
        let loc = match (&self.file, self.line) {
            (Some(f), Some(l)) => format!(" (`{}:{}`)", f.display(), l),
            (Some(f), None) => format!(" (`{}`)", f.display()),
            _ => String::new(),
        };
        format!(
            "{} [{}] {}{}",
            self.severity.emoji(),
            self.category,
            self.title,
            loc
        )
    }
}

// ============================================================================
// AuditRequest — what the caller sends to POST /api/audit
// ============================================================================

// Request body for `POST /api/audit`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditRequest {
    // Path to the repository root on the server's file system,
    // or a GitHub `owner/repo` slug if the server should clone it.
    pub repo: String,
    // Optional branch/tag/SHA to check out (defaults to HEAD)
    pub git_ref: Option<String>,
    // Audit mode: `"full"` (all files) or `"changed"` (only diff from base)
    #[serde(default = "default_audit_mode")]
    pub mode: String,
    // Minimum severity to include in results (default: `"low"`)
    #[serde(default = "default_min_severity")]
    pub min_severity: AuditSeverity,
    // Whether to append new findings to the repo's `todo.md`
    #[serde(default)]
    pub append_to_todo: bool,
    // Whether to force a fresh audit even if a cached result exists
    #[serde(default)]
    pub force_refresh: bool,
    // Maximum number of files to audit (0 = unlimited)
    #[serde(default)]
    pub max_files: usize,
    // File path patterns to exclude (glob syntax)
    #[serde(default)]
    pub exclude_patterns: Vec<String>,
    // Caller-supplied metadata attached to the result as-is
    #[serde(default)]
    pub metadata: HashMap<String, String>,
}

fn default_audit_mode() -> String {
    "full".to_string()
}

fn default_min_severity() -> AuditSeverity {
    AuditSeverity::Low
}

impl Default for AuditRequest {
    fn default() -> Self {
        Self {
            repo: String::new(),
            git_ref: None,
            mode: default_audit_mode(),
            min_severity: default_min_severity(),
            append_to_todo: false,
            force_refresh: false,
            max_files: 0,
            exclude_patterns: Vec::new(),
            metadata: HashMap::new(),
        }
    }
}

// ============================================================================
// AuditResponse — what the server returns
// ============================================================================

// Response body for `GET /api/audit` (status poll) and
// `POST /api/audit` (immediate result or async job ID)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditResponse {
    // Unique audit run ID (UUID v4)
    pub id: String,
    // Current status of the audit
    pub status: AuditStatus,
    // When the audit was requested
    pub requested_at: DateTime<Utc>,
    // When the audit completed (None if still running)
    pub completed_at: Option<DateTime<Utc>>,
    // Wall-clock duration in seconds
    pub duration_secs: Option<f64>,
    // Total files scanned
    pub files_scanned: usize,
    // All findings from this run, ordered by severity (Critical first)
    pub findings: Vec<AuditFinding>,
    // Summary counts by severity
    pub summary: AuditSummary,
    // Whether the result came from cache
    pub from_cache: bool,
    // Estimated LLM cost for this run (USD)
    pub estimated_cost_usd: f64,
    // Non-fatal errors encountered during the run
    pub errors: Vec<String>,
    // Echo of the original request
    pub request: AuditRequest,
}

impl AuditResponse {
    // Whether the audit found any blocking (High/Critical) issues
    pub fn has_blocking_findings(&self) -> bool {
        self.findings.iter().any(|f| f.is_blocking())
    }

    // Count findings by severity
    pub fn count_by_severity(&self, severity: AuditSeverity) -> usize {
        self.findings
            .iter()
            .filter(|f| f.severity == severity)
            .count()
    }

    // Return findings at or above the given severity, sorted Critical → Info
    pub fn findings_above(&self, min: AuditSeverity) -> Vec<&AuditFinding> {
        let mut found: Vec<&AuditFinding> =
            self.findings.iter().filter(|f| f.severity >= min).collect();
        found.sort_by(|a, b| b.severity.cmp(&a.severity));
        found
    }
}

// Aggregated counts for the audit response
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AuditSummary {
    pub total: usize,
    pub critical: usize,
    pub high: usize,
    pub medium: usize,
    pub low: usize,
    pub info: usize,
    // Breakdown by category
    pub by_category: HashMap<String, usize>,
    // Files with at least one finding
    pub files_with_findings: usize,
}

impl AuditSummary {
    // Build a summary from a slice of findings
    pub fn from_findings(findings: &[AuditFinding]) -> Self {
        let mut summary = AuditSummary {
            total: findings.len(),
            ..Default::default()
        };

        let mut files_seen = std::collections::HashSet::new();

        for f in findings {
            match f.severity {
                AuditSeverity::Critical => summary.critical += 1,
                AuditSeverity::High => summary.high += 1,
                AuditSeverity::Medium => summary.medium += 1,
                AuditSeverity::Low => summary.low += 1,
                AuditSeverity::Info => summary.info += 1,
            }
            *summary
                .by_category
                .entry(f.category.as_str().to_string())
                .or_insert(0) += 1;
            if let Some(file) = &f.file {
                files_seen.insert(file.clone());
            }
        }

        summary.files_with_findings = files_seen.len();
        summary
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_severity_ordering() {
        assert!(AuditSeverity::Critical > AuditSeverity::High);
        assert!(AuditSeverity::High > AuditSeverity::Medium);
        assert!(AuditSeverity::Medium > AuditSeverity::Low);
        assert!(AuditSeverity::Low > AuditSeverity::Info);
    }

    #[test]
    fn test_severity_from_str() {
        use std::str::FromStr;
        assert_eq!(
            AuditSeverity::from_str("critical"),
            Ok(AuditSeverity::Critical)
        );
        assert_eq!(AuditSeverity::from_str("HIGH"), Ok(AuditSeverity::High));
        assert_eq!(AuditSeverity::from_str("unknown"), Err(()));
    }

    #[test]
    fn test_audit_status_is_terminal() {
        assert!(AuditStatus::Completed.is_terminal());
        assert!(AuditStatus::Failed.is_terminal());
        assert!(AuditStatus::Cached.is_terminal());
        assert!(!AuditStatus::Running.is_terminal());
        assert!(!AuditStatus::Pending.is_terminal());
    }

    #[test]
    fn test_finding_is_blocking() {
        let make = |sev| AuditFinding {
            id: "test".to_string(),
            severity: sev,
            category: FindingCategory::Security,
            title: "test".to_string(),
            description: String::new(),
            recommendation: String::new(),
            file: None,
            line: None,
            code_snippet: None,
            is_recurring: false,
            tags: vec![],
            confidence: 1.0,
        };

        assert!(make(AuditSeverity::Critical).is_blocking());
        assert!(make(AuditSeverity::High).is_blocking());
        assert!(!make(AuditSeverity::Medium).is_blocking());
        assert!(!make(AuditSeverity::Low).is_blocking());
    }

    #[test]
    fn test_finding_to_todo_item_text() {
        let finding = AuditFinding {
            id: "deadbeef".to_string(),
            severity: AuditSeverity::High,
            category: FindingCategory::Security,
            title: "SQL injection in search handler".to_string(),
            description: String::new(),
            recommendation: String::new(),
            file: Some(PathBuf::from("src/api/handlers.rs")),
            line: Some(142),
            code_snippet: None,
            is_recurring: false,
            tags: vec![],
            confidence: 0.95,
        };

        let text = finding.to_todo_item_text();
        assert!(text.contains("SQL injection"));
        assert!(text.contains("src/api/handlers.rs:142"));
        assert!(text.contains("🔴"));
    }

    #[test]
    fn test_audit_summary_from_findings() {
        let findings = vec![
            AuditFinding {
                id: "a".to_string(),
                severity: AuditSeverity::Critical,
                category: FindingCategory::Security,
                title: "critical".to_string(),
                description: String::new(),
                recommendation: String::new(),
                file: Some(PathBuf::from("src/lib.rs")),
                line: None,
                code_snippet: None,
                is_recurring: false,
                tags: vec![],
                confidence: 1.0,
            },
            AuditFinding {
                id: "b".to_string(),
                severity: AuditSeverity::Medium,
                category: FindingCategory::CodeQuality,
                title: "medium".to_string(),
                description: String::new(),
                recommendation: String::new(),
                file: Some(PathBuf::from("src/api/handlers.rs")),
                line: None,
                code_snippet: None,
                is_recurring: false,
                tags: vec![],
                confidence: 0.8,
            },
            AuditFinding {
                id: "c".to_string(),
                severity: AuditSeverity::Medium,
                category: FindingCategory::CodeQuality,
                title: "medium 2".to_string(),
                description: String::new(),
                recommendation: String::new(),
                file: Some(PathBuf::from("src/api/handlers.rs")),
                line: None,
                code_snippet: None,
                is_recurring: false,
                tags: vec![],
                confidence: 0.7,
            },
        ];

        let summary = AuditSummary::from_findings(&findings);
        assert_eq!(summary.total, 3);
        assert_eq!(summary.critical, 1);
        assert_eq!(summary.medium, 2);
        assert_eq!(summary.high, 0);
        assert_eq!(summary.files_with_findings, 2);
        assert_eq!(summary.by_category.get("security"), Some(&1));
        assert_eq!(summary.by_category.get("code_quality"), Some(&2));
    }

    #[test]
    fn test_audit_request_defaults() {
        let req = AuditRequest::default();
        assert_eq!(req.mode, "full");
        assert_eq!(req.min_severity, AuditSeverity::Low);
        assert!(!req.append_to_todo);
        assert!(!req.force_refresh);
    }

    #[test]
    fn test_audit_response_findings_above() {
        let findings = vec![
            AuditFinding {
                id: "a".to_string(),
                severity: AuditSeverity::Info,
                category: FindingCategory::Other,
                title: "info".to_string(),
                description: String::new(),
                recommendation: String::new(),
                file: None,
                line: None,
                code_snippet: None,
                is_recurring: false,
                tags: vec![],
                confidence: 1.0,
            },
            AuditFinding {
                id: "b".to_string(),
                severity: AuditSeverity::High,
                category: FindingCategory::Security,
                title: "high".to_string(),
                description: String::new(),
                recommendation: String::new(),
                file: None,
                line: None,
                code_snippet: None,
                is_recurring: false,
                tags: vec![],
                confidence: 1.0,
            },
        ];

        let response = AuditResponse {
            id: "test-run".to_string(),
            status: AuditStatus::Completed,
            requested_at: Utc::now(),
            completed_at: Some(Utc::now()),
            duration_secs: Some(1.5),
            files_scanned: 10,
            summary: AuditSummary::from_findings(&findings),
            findings,
            from_cache: false,
            estimated_cost_usd: 0.01,
            errors: vec![],
            request: AuditRequest::default(),
        };

        let above_medium = response.findings_above(AuditSeverity::Medium);
        assert_eq!(above_medium.len(), 1);
        assert_eq!(above_medium[0].severity, AuditSeverity::High);
    }

    #[test]
    fn test_json_round_trip() {
        let req = AuditRequest {
            repo: "nuniesmith/rustcode".to_string(),
            git_ref: Some("main".to_string()),
            mode: "full".to_string(),
            min_severity: AuditSeverity::Medium,
            append_to_todo: true,
            force_refresh: false,
            max_files: 100,
            exclude_patterns: vec!["target/**".to_string()],
            metadata: HashMap::new(),
        };

        let json = serde_json::to_string_pretty(&req).unwrap();
        let parsed: AuditRequest = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed.repo, "nuniesmith/rustcode");
        assert_eq!(parsed.min_severity, AuditSeverity::Medium);
        assert!(parsed.append_to_todo);
        assert_eq!(parsed.exclude_patterns, vec!["target/**"]);
    }
}
