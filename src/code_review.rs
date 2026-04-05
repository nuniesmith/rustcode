// # Code Review Module
//
// Automated code review with AI-powered analysis and structured feedback.
//
// ## Features
//
// - Git diff integration
// - Batch analysis of changed files
// - Structured review feedback
// - GitHub/GitLab compatible output
// - Security and quality focus
//
// ## Usage
//
// ```rust,no_run
// use rustcode::code_review::CodeReviewer;
// use rustcode::db::Database;
//
// #[tokio::main]
// async fn main() -> anyhow::Result<()> {
//     let db = Database::new("data/rustcode.db").await?;
//     let reviewer = CodeReviewer::new(db).await?;
//
//     // Review git diff
//     let review = reviewer.review_diff(".", None).await?;
//     println!("{}", review.format_markdown());
//
//     Ok(())
// }
// ```

use crate::db::Database;
use crate::grok_client::{FileScoreResult, GrokClient};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Command;

// Code reviewer with AI-powered analysis
pub struct CodeReviewer {
    grok_client: GrokClient,
}

// Review result for a single file
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileReview {
    // File path
    pub path: String,
    // Overall quality score (0-100)
    pub score: f64,
    // Security score (0-100)
    pub security_score: f64,
    // Issues found
    pub issues: Vec<ReviewIssue>,
    // Suggestions for improvement
    pub suggestions: Vec<String>,
    // Lines changed
    pub lines_changed: usize,
}

// Review issue with severity
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewIssue {
    // Issue severity
    pub severity: IssueSeverity,
    // Issue description
    pub description: String,
    // Optional line number
    pub line: Option<usize>,
}

// Issue severity levels
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub enum IssueSeverity {
    // Critical security or correctness issue
    Critical,
    // High priority issue
    High,
    // Medium priority issue
    Medium,
    // Low priority issue
    Low,
    // Informational note
    Info,
}

// Complete code review result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodeReview {
    // Repository path
    pub repo_path: String,
    // Base branch (if comparing)
    pub base_branch: Option<String>,
    // Files reviewed
    pub files: Vec<FileReview>,
    // Overall statistics
    pub stats: ReviewStats,
    // High-level summary
    pub summary: String,
    // Timestamp
    pub timestamp: chrono::DateTime<chrono::Utc>,
}

// Review statistics
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewStats {
    // Total files reviewed
    pub total_files: usize,
    // Files with issues
    pub files_with_issues: usize,
    // Total issues found
    pub total_issues: usize,
    // Critical issues
    pub critical_issues: usize,
    // High priority issues
    pub high_issues: usize,
    // Medium priority issues
    pub medium_issues: usize,
    // Low priority issues
    pub low_issues: usize,
    // Average quality score
    pub avg_quality: f64,
    // Average security score
    pub avg_security: f64,
    // Total lines changed
    pub total_lines_changed: usize,
}

impl CodeReviewer {
    // Create a new code reviewer
    pub async fn new(db: Database) -> Result<Self> {
        let grok_client = GrokClient::from_env(db).await?;
        Ok(Self { grok_client })
    }

    // Review changes in git diff
    pub async fn review_diff(
        &self,
        repo_path: impl AsRef<Path>,
        base_branch: Option<&str>,
    ) -> Result<CodeReview> {
        let repo_path = repo_path.as_ref();
        let changed_files = self.get_changed_files(repo_path, base_branch)?;

        if changed_files.is_empty() {
            return Ok(CodeReview {
                repo_path: repo_path.to_string_lossy().to_string(),
                base_branch: base_branch.map(String::from),
                files: vec![],
                stats: ReviewStats::default(),
                summary: "No changes detected.".to_string(),
                timestamp: chrono::Utc::now(),
            });
        }

        // Review each changed file
        let mut file_reviews = Vec::new();
        for (file_path, lines_changed) in changed_files {
            if let Ok(review) = self.review_file(&file_path, lines_changed).await {
                file_reviews.push(review);
            }
        }

        let stats = self.calculate_stats(&file_reviews);
        let summary = self.generate_summary(&stats);

        Ok(CodeReview {
            repo_path: repo_path.to_string_lossy().to_string(),
            base_branch: base_branch.map(String::from),
            files: file_reviews,
            stats,
            summary,
            timestamp: chrono::Utc::now(),
        })
    }

    // Review specific files
    pub async fn review_files(&self, files: Vec<PathBuf>) -> Result<CodeReview> {
        let mut file_reviews = Vec::new();

        for file_path in files {
            if let Ok(review) = self.review_file(&file_path, 0).await {
                file_reviews.push(review);
            }
        }

        let stats = self.calculate_stats(&file_reviews);
        let summary = self.generate_summary(&stats);

        Ok(CodeReview {
            repo_path: ".".to_string(),
            base_branch: None,
            files: file_reviews,
            stats,
            summary,
            timestamp: chrono::Utc::now(),
        })
    }

    // Review a single file
    async fn review_file(&self, path: &Path, lines_changed: usize) -> Result<FileReview> {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read file: {}", path.display()))?;

        // Skip very large files
        if content.len() > 100_000 {
            return Ok(FileReview {
                path: path.to_string_lossy().to_string(),
                score: 0.0,
                security_score: 0.0,
                issues: vec![ReviewIssue {
                    severity: IssueSeverity::Info,
                    description: "File too large for analysis (>100KB)".to_string(),
                    line: None,
                }],
                suggestions: vec![],
                lines_changed,
            });
        }

        // Use Grok to score the file
        let score_result = self
            .grok_client
            .score_file(path.to_str().unwrap(), &content)
            .await?;

        // Convert to review format
        Ok(self.convert_to_file_review(path, score_result, lines_changed))
    }

    // Convert FileScoreResult to FileReview
    fn convert_to_file_review(
        &self,
        path: &Path,
        score: FileScoreResult,
        lines_changed: usize,
    ) -> FileReview {
        let mut issues = Vec::new();

        // Categorize issues by severity
        for issue in &score.issues {
            let severity = self.determine_severity(issue, score.security_score);
            issues.push(ReviewIssue {
                severity,
                description: issue.clone(),
                line: None,
            });
        }

        FileReview {
            path: path.to_string_lossy().to_string(),
            score: score.overall_score,
            security_score: score.security_score,
            issues,
            suggestions: score.suggestions.clone(),
            lines_changed,
        }
    }

    // Determine issue severity based on content and security score
    fn determine_severity(&self, issue: &str, security_score: f64) -> IssueSeverity {
        let issue_lower = issue.to_lowercase();

        // Critical security issues
        if issue_lower.contains("sql injection")
            || issue_lower.contains("xss")
            || issue_lower.contains("csrf")
            || issue_lower.contains("authentication bypass")
            || issue_lower.contains("authorization")
        {
            return IssueSeverity::Critical;
        }

        // High priority issues
        if issue_lower.contains("security")
            || issue_lower.contains("vulnerability")
            || issue_lower.contains("unsafe")
            || issue_lower.contains("panic")
            || issue_lower.contains("unwrap")
        {
            return IssueSeverity::High;
        }

        // Medium issues
        if issue_lower.contains("error handling")
            || issue_lower.contains("complexity")
            || issue_lower.contains("performance")
            || issue_lower.contains("refactor")
        {
            return IssueSeverity::Medium;
        }

        // Low issues
        if issue_lower.contains("style")
            || issue_lower.contains("naming")
            || issue_lower.contains("documentation")
        {
            return IssueSeverity::Low;
        }

        // Security score affects default severity
        if security_score < 50.0 {
            IssueSeverity::High
        } else if security_score < 70.0 {
            IssueSeverity::Medium
        } else {
            IssueSeverity::Low
        }
    }

    // Get list of changed files from git
    fn get_changed_files(
        &self,
        repo_path: &Path,
        base_branch: Option<&str>,
    ) -> Result<Vec<(PathBuf, usize)>> {
        let mut files = Vec::new();

        // Build git diff command
        let mut cmd = Command::new("git");
        cmd.current_dir(repo_path);
        cmd.arg("diff");
        cmd.arg("--name-status");

        if let Some(branch) = base_branch {
            cmd.arg(branch);
        }

        let output = cmd.output().context("Failed to execute git diff command")?;

        if !output.status.success() {
            return Err(anyhow::anyhow!(
                "Git diff failed: {}",
                String::from_utf8_lossy(&output.stderr)
            ));
        }

        let diff_output = String::from_utf8_lossy(&output.stdout);

        for line in diff_output.lines() {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 2 {
                let status = parts[0];
                let file_path = parts[1];

                // Only process added or modified files
                if status == "A" || status == "M" {
                    let full_path = repo_path.join(file_path);
                    if full_path.exists() && self.is_reviewable_file(&full_path) {
                        let lines = self.count_changed_lines(repo_path, file_path, base_branch)?;
                        files.push((full_path, lines));
                    }
                }
            }
        }

        Ok(files)
    }

    // Check if file should be reviewed
    fn is_reviewable_file(&self, path: &Path) -> bool {
        if let Some(ext) = path.extension() {
            matches!(
                ext.to_str().unwrap_or(""),
                "rs" | "py" | "js" | "ts" | "java" | "kt" | "go" | "c" | "cpp" | "h" | "hpp"
            )
        } else {
            false
        }
    }

    // Count changed lines for a file
    fn count_changed_lines(
        &self,
        repo_path: &Path,
        file_path: &str,
        base_branch: Option<&str>,
    ) -> Result<usize> {
        let mut cmd = Command::new("git");
        cmd.current_dir(repo_path);
        cmd.arg("diff");
        cmd.arg("--numstat");

        if let Some(branch) = base_branch {
            cmd.arg(branch);
        }

        cmd.arg("--");
        cmd.arg(file_path);

        let output = cmd.output().context("Failed to get diff stats")?;
        let stats = String::from_utf8_lossy(&output.stdout);

        if let Some(line) = stats.lines().next() {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 2 {
                let added: usize = parts[0].parse().unwrap_or(0);
                let removed: usize = parts[1].parse().unwrap_or(0);
                return Ok(added + removed);
            }
        }

        Ok(0)
    }

    // Calculate review statistics
    fn calculate_stats(&self, reviews: &[FileReview]) -> ReviewStats {
        if reviews.is_empty() {
            return ReviewStats::default();
        }

        let total_files = reviews.len();
        let files_with_issues = reviews.iter().filter(|r| !r.issues.is_empty()).count();

        let mut total_issues = 0;
        let mut critical_issues = 0;
        let mut high_issues = 0;
        let mut medium_issues = 0;
        let mut low_issues = 0;

        for review in reviews {
            total_issues += review.issues.len();
            for issue in &review.issues {
                match issue.severity {
                    IssueSeverity::Critical => critical_issues += 1,
                    IssueSeverity::High => high_issues += 1,
                    IssueSeverity::Medium => medium_issues += 1,
                    IssueSeverity::Low => low_issues += 1,
                    IssueSeverity::Info => {}
                }
            }
        }

        let avg_quality = reviews.iter().map(|r| r.score).sum::<f64>() / total_files as f64;
        let avg_security =
            reviews.iter().map(|r| r.security_score).sum::<f64>() / total_files as f64;
        let total_lines_changed = reviews.iter().map(|r| r.lines_changed).sum();

        ReviewStats {
            total_files,
            files_with_issues,
            total_issues,
            critical_issues,
            high_issues,
            medium_issues,
            low_issues,
            avg_quality,
            avg_security,
            total_lines_changed,
        }
    }

    // Generate high-level summary
    fn generate_summary(&self, stats: &ReviewStats) -> String {
        let mut summary = String::new();

        if stats.total_files == 0 {
            return "No files to review.".to_string();
        }

        // Overall assessment
        if stats.critical_issues > 0 {
            summary.push_str("🔴 **Critical issues found** - Immediate action required.\n\n");
        } else if stats.high_issues > 0 {
            summary.push_str(
                "⚠️  **High priority issues found** - Should be addressed before merge.\n\n",
            );
        } else if stats.medium_issues > 0 || stats.low_issues > 0 {
            summary.push_str("✅ **No critical issues** - Some improvements recommended.\n\n");
        } else {
            summary.push_str("🎉 **Excellent!** - No significant issues found.\n\n");
        }

        // Quality metrics
        summary.push_str(&format!(
            "**Quality Score:** {:.1}/100 ({})\n",
            stats.avg_quality,
            self.quality_rating(stats.avg_quality)
        ));

        summary.push_str(&format!(
            "**Security Score:** {:.1}/100 ({})\n",
            stats.avg_security,
            self.quality_rating(stats.avg_security)
        ));

        summary
    }

    // Get quality rating label
    fn quality_rating(&self, score: f64) -> &'static str {
        if score >= 90.0 {
            "Excellent"
        } else if score >= 75.0 {
            "Good"
        } else if score >= 60.0 {
            "Acceptable"
        } else if score >= 40.0 {
            "Needs Improvement"
        } else {
            "Poor"
        }
    }
}

impl CodeReview {
    // Format review as markdown
    pub fn format_markdown(&self) -> String {
        let mut output = String::new();

        // Header
        output.push_str("# Code Review Report\n\n");
        output.push_str(&format!(
            "**Generated:** {}\n",
            self.timestamp.format("%Y-%m-%d %H:%M:%S UTC")
        ));
        if let Some(branch) = &self.base_branch {
            output.push_str(&format!("**Base Branch:** {}\n", branch));
        }
        output.push_str("\n---\n\n");

        // Summary
        output.push_str("## Summary\n\n");
        output.push_str(&self.summary);
        output.push('\n');

        // Statistics
        output.push_str("## Statistics\n\n");
        output.push_str(&format!(
            "- **Files Reviewed:** {}\n",
            self.stats.total_files
        ));
        output.push_str(&format!(
            "- **Files with Issues:** {}\n",
            self.stats.files_with_issues
        ));
        output.push_str(&format!(
            "- **Total Issues:** {}\n",
            self.stats.total_issues
        ));
        output.push_str(&format!(
            "- **Lines Changed:** {}\n",
            self.stats.total_lines_changed
        ));
        output.push('\n');

        // Issue breakdown
        if self.stats.total_issues > 0 {
            output.push_str("### Issues by Severity\n\n");
            if self.stats.critical_issues > 0 {
                output.push_str(&format!(
                    "- 🔴 **Critical:** {}\n",
                    self.stats.critical_issues
                ));
            }
            if self.stats.high_issues > 0 {
                output.push_str(&format!("- 🟠 **High:** {}\n", self.stats.high_issues));
            }
            if self.stats.medium_issues > 0 {
                output.push_str(&format!("- 🟡 **Medium:** {}\n", self.stats.medium_issues));
            }
            if self.stats.low_issues > 0 {
                output.push_str(&format!("- 🔵 **Low:** {}\n", self.stats.low_issues));
            }
            output.push('\n');
        }

        // File reviews
        if !self.files.is_empty() {
            output.push_str("## File Reviews\n\n");

            for file in &self.files {
                output.push_str(&format!("### {}\n\n", file.path));
                output.push_str(&format!("- **Quality Score:** {:.1}/100\n", file.score));
                output.push_str(&format!(
                    "- **Security Score:** {:.1}/100\n",
                    file.security_score
                ));
                if file.lines_changed > 0 {
                    output.push_str(&format!("- **Lines Changed:** {}\n", file.lines_changed));
                }
                output.push('\n');

                // Issues
                if !file.issues.is_empty() {
                    output.push_str("**Issues Found:**\n\n");
                    for issue in &file.issues {
                        let icon = match issue.severity {
                            IssueSeverity::Critical => "🔴",
                            IssueSeverity::High => "🟠",
                            IssueSeverity::Medium => "🟡",
                            IssueSeverity::Low => "🔵",
                            IssueSeverity::Info => "ℹ️",
                        };
                        output.push_str(&format!(
                            "- {} **{:?}:** {}\n",
                            icon, issue.severity, issue.description
                        ));
                    }
                    output.push('\n');
                }

                // Suggestions
                if !file.suggestions.is_empty() {
                    output.push_str("**Suggestions:**\n\n");
                    for suggestion in &file.suggestions {
                        output.push_str(&format!("- {}\n", suggestion));
                    }
                    output.push('\n');
                }
            }
        }

        output
    }

    // Format as GitHub PR comment
    pub fn format_github_comment(&self) -> String {
        let mut output = String::new();

        // Summary with emoji
        if self.stats.critical_issues > 0 {
            output.push_str("## 🔴 Code Review - Action Required\n\n");
        } else if self.stats.high_issues > 0 {
            output.push_str("## ⚠️  Code Review - Issues Found\n\n");
        } else {
            output.push_str("## ✅ Code Review - Looks Good\n\n");
        }

        output.push_str(&self.summary);

        // Quick stats
        output.push_str(&format!(
            "\n📊 **{} files** | {} issues | {:.1}% quality | {:.1}% security\n\n",
            self.stats.total_files,
            self.stats.total_issues,
            self.stats.avg_quality,
            self.stats.avg_security
        ));

        // Critical/High issues only
        let important_files: Vec<_> = self
            .files
            .iter()
            .filter(|f| f.issues.iter().any(|i| i.severity <= IssueSeverity::High))
            .collect();

        if !important_files.is_empty() {
            output.push_str("### 🔍 Files Requiring Attention\n\n");
            for file in important_files {
                output.push_str(&format!("**{}**\n", file.path));
                for issue in &file.issues {
                    if issue.severity <= IssueSeverity::High {
                        output
                            .push_str(&format!("- {:?}: {}\n", issue.severity, issue.description));
                    }
                }
                output.push('\n');
            }
        }

        output
    }
}

impl Default for ReviewStats {
    fn default() -> Self {
        Self {
            total_files: 0,
            files_with_issues: 0,
            total_issues: 0,
            critical_issues: 0,
            high_issues: 0,
            medium_issues: 0,
            low_issues: 0,
            avg_quality: 0.0,
            avg_security: 0.0,
            total_lines_changed: 0,
        }
    }
}

impl std::fmt::Display for IssueSeverity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            IssueSeverity::Critical => write!(f, "CRITICAL"),
            IssueSeverity::High => write!(f, "HIGH"),
            IssueSeverity::Medium => write!(f, "MEDIUM"),
            IssueSeverity::Low => write!(f, "LOW"),
            IssueSeverity::Info => write!(f, "INFO"),
        }
    }
}
