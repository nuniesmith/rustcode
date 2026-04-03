//! Tag schema definitions for structured audit annotations
//!
//! Provides a robust schema for categorizing code, tracking technical debt,
//! and building a comprehensive directory tree of codebase status.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

/// Schema for audit tags with strict validation
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TagSchema {
    /// Tag category
    pub category: TagCategory,
    /// Tag status
    pub status: CodeStatus,
    /// Tag age (if applicable)
    pub age: Option<CodeAge>,
    /// Tag complexity
    pub complexity: Option<Complexity>,
    /// Tag priority
    pub priority: Priority,
    /// Additional metadata
    pub metadata: HashMap<String, String>,
}

/// Tag categories for organization
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TagCategory {
    /// Code organization/structure
    Organization,
    /// Security concerns
    Security,
    /// Performance optimization
    Performance,
    /// Risk management
    Risk,
    /// Technical debt
    TechnicalDebt,
    /// Documentation
    Documentation,
    /// Testing
    Testing,
    /// Deprecated/old code
    Legacy,
    /// Experimental/new code
    Experimental,
    /// Configuration
    Configuration,
}

impl TagCategory {
    /// Get all possible categories
    pub fn all() -> Vec<Self> {
        vec![
            Self::Organization,
            Self::Security,
            Self::Performance,
            Self::Risk,
            Self::TechnicalDebt,
            Self::Documentation,
            Self::Testing,
            Self::Legacy,
            Self::Experimental,
            Self::Configuration,
        ]
    }

    /// Get category from string
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "organization" | "org" => Some(Self::Organization),
            "security" | "sec" => Some(Self::Security),
            "performance" | "perf" => Some(Self::Performance),
            "risk" => Some(Self::Risk),
            "technical-debt" | "debt" | "tech-debt" => Some(Self::TechnicalDebt),
            "documentation" | "docs" => Some(Self::Documentation),
            "testing" | "tests" => Some(Self::Testing),
            "legacy" | "old" => Some(Self::Legacy),
            "experimental" | "new" | "exp" => Some(Self::Experimental),
            "configuration" | "config" => Some(Self::Configuration),
            _ => None,
        }
    }
}

/// Code status indicators
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CodeStatus {
    /// New code (< 3 months)
    New,
    /// Active development
    Active,
    /// Stable/production
    Stable,
    /// Deprecated but still used
    Deprecated,
    /// Old code (> 1 year)
    Old,
    /// Very old code (> 2 years)
    VeryOld,
    /// Needs review
    NeedsReview,
    /// Frozen (do not modify)
    Frozen,
    /// Experimental/prototype
    Experimental,
    /// Unknown status
    Unknown,
}

impl CodeStatus {
    /// Get status from string tag value
    pub fn from_tag_value(value: &str) -> Self {
        match value.to_lowercase().as_str() {
            "new" => Self::New,
            "active" => Self::Active,
            "stable" | "production" | "prod" => Self::Stable,
            "deprecated" | "dep" => Self::Deprecated,
            "old" => Self::Old,
            "very-old" | "ancient" => Self::VeryOld,
            "needs-review" | "review" => Self::NeedsReview,
            "frozen" | "freeze" => Self::Frozen,
            "experimental" | "exp" | "proto" => Self::Experimental,
            _ => Self::Unknown,
        }
    }

    /// Check if status indicates technical debt
    pub fn is_technical_debt(&self) -> bool {
        matches!(
            self,
            Self::Deprecated | Self::Old | Self::VeryOld | Self::NeedsReview
        )
    }

    /// Check if code is stable/production ready
    pub fn is_production_ready(&self) -> bool {
        matches!(self, Self::Stable | Self::Frozen)
    }
}

/// Code age classification
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CodeAge {
    /// Less than 1 month
    Fresh,
    /// 1-3 months
    Recent,
    /// 3-6 months
    Moderate,
    /// 6-12 months
    Mature,
    /// 1-2 years
    Old,
    /// 2+ years
    VeryOld,
}

impl CodeAge {
    /// Determine age from months
    pub fn from_months(months: u32) -> Self {
        match months {
            0..=1 => Self::Fresh,
            2..=3 => Self::Recent,
            4..=6 => Self::Moderate,
            7..=12 => Self::Mature,
            13..=24 => Self::Old,
            _ => Self::VeryOld,
        }
    }

    /// Get estimated months
    pub fn to_months(&self) -> u32 {
        match self {
            Self::Fresh => 0,
            Self::Recent => 2,
            Self::Moderate => 5,
            Self::Mature => 9,
            Self::Old => 18,
            Self::VeryOld => 30,
        }
    }
}

/// Code complexity level
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Complexity {
    /// Simple, straightforward code
    Simple,
    /// Moderate complexity
    Moderate,
    /// Complex logic
    Complex,
    /// Very complex/critical
    Critical,
}

impl Complexity {
    /// Determine from lines of code and other metrics
    pub fn from_metrics(lines: usize, cyclomatic: usize) -> Self {
        if lines > 500 || cyclomatic > 20 {
            Self::Critical
        } else if lines > 250 || cyclomatic > 10 {
            Self::Complex
        } else if lines > 100 || cyclomatic > 5 {
            Self::Moderate
        } else {
            Self::Simple
        }
    }
}

/// Priority level
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Priority {
    Critical,
    High,
    Medium,
    Low,
}

impl Priority {
    /// Determine from status and category
    pub fn from_status_and_category(status: CodeStatus, category: TagCategory) -> Self {
        match (status, category) {
            (CodeStatus::NeedsReview, TagCategory::Security) => Self::Critical,
            (CodeStatus::Deprecated, TagCategory::Security) => Self::High,
            (CodeStatus::VeryOld, _) => Self::High,
            (CodeStatus::Old, TagCategory::Security | TagCategory::Risk) => Self::High,
            (CodeStatus::Old, _) => Self::Medium,
            (CodeStatus::NeedsReview, _) => Self::Medium,
            (CodeStatus::Experimental, _) => Self::Medium,
            _ => Self::Low,
        }
    }
}

/// Directory tree node for codebase visualization
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DirectoryNode {
    /// Node name (file or directory)
    pub name: String,
    /// Full path
    pub path: PathBuf,
    /// Node type
    pub node_type: NodeType,
    /// Code status (if applicable)
    pub status: Option<CodeStatus>,
    /// Tags found in this file/directory
    pub tags: Vec<String>,
    /// Statistics
    pub stats: NodeStats,
    /// Child nodes (for directories)
    pub children: Vec<DirectoryNode>,
    /// Issues summary
    pub issues: IssuesSummary,
}

/// Type of directory tree node
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NodeType {
    Directory,
    File,
}

/// Node statistics
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NodeStats {
    /// Total files (for directories)
    pub file_count: usize,
    /// Total lines of code
    pub lines_of_code: usize,
    /// Number of TODO comments
    pub todos: usize,
    /// Number of FIXME comments
    pub fixmes: usize,
    /// Number of audit tags
    pub audit_tags: usize,
    /// Last modified (Unix timestamp)
    pub last_modified: Option<i64>,
}

/// Issues summary for a node
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct IssuesSummary {
    /// Critical issues
    pub critical: usize,
    /// High severity issues
    pub high: usize,
    /// Medium severity issues
    pub medium: usize,
    /// Low severity issues
    pub low: usize,
}

impl IssuesSummary {
    /// Total issues
    pub fn total(&self) -> usize {
        self.critical + self.high + self.medium + self.low
    }

    /// Has any critical or high issues
    pub fn has_critical_or_high(&self) -> bool {
        self.critical > 0 || self.high > 0
    }
}

/// Simple issue detection patterns
#[derive(Debug, Clone)]
pub struct SimpleIssueDetector {
    patterns: Vec<SimpleIssuePattern>,
}

/// Pattern for detecting simple issues
#[derive(Debug, Clone)]
pub struct SimpleIssuePattern {
    pub name: &'static str,
    pub pattern: &'static str,
    pub severity: &'static str,
    pub category: &'static str,
    pub description: &'static str,
}

impl SimpleIssueDetector {
    /// Create detector with common patterns
    pub fn new() -> Self {
        let patterns = vec![
            SimpleIssuePattern {
                name: "unwrap",
                pattern: r"\.unwrap\(\)",
                severity: "medium",
                category: "error-handling",
                description:
                    "Using unwrap() can cause panics - consider using proper error handling",
            },
            SimpleIssuePattern {
                name: "expect_without_context",
                pattern: r#"\.expect\(""\)"#,
                severity: "low",
                category: "code-quality",
                description: "Empty expect message - add meaningful context",
            },
            SimpleIssuePattern {
                name: "todo_comment",
                pattern: r"(?i)//\s*TODO",
                severity: "info",
                category: "technical-debt",
                description: "TODO comment found - track as task",
            },
            SimpleIssuePattern {
                name: "fixme_comment",
                pattern: r"(?i)//\s*FIXME",
                severity: "medium",
                category: "technical-debt",
                description: "FIXME comment found - requires attention",
            },
            SimpleIssuePattern {
                name: "xxx_comment",
                pattern: r"(?i)//\s*XXX",
                severity: "high",
                category: "technical-debt",
                description: "XXX comment found - critical issue marker",
            },
            SimpleIssuePattern {
                name: "unsafe_block",
                pattern: r"unsafe\s*\{",
                severity: "high",
                category: "safety",
                description: "Unsafe block - requires careful review",
            },
            SimpleIssuePattern {
                name: "println_debug",
                pattern: r"println!\(",
                severity: "low",
                category: "code-quality",
                description: "println! found - should use proper logging",
            },
            SimpleIssuePattern {
                name: "sleep_blocking",
                pattern: r"thread::sleep|std::thread::sleep",
                severity: "medium",
                category: "performance",
                description: "Blocking sleep - consider async alternatives in async contexts",
            },
            SimpleIssuePattern {
                name: "deprecated_annotation",
                pattern: r"#\[deprecated",
                severity: "info",
                category: "legacy",
                description: "Deprecated code - plan for removal",
            },
            SimpleIssuePattern {
                name: "clone_in_loop",
                pattern: r"for\s+.+\{[^}]*\.clone\(\)",
                severity: "medium",
                category: "performance",
                description: "Clone in loop - may impact performance",
            },
        ];

        Self { patterns }
    }

    /// Get all patterns
    pub fn patterns(&self) -> &[SimpleIssuePattern] {
        &self.patterns
    }
}

impl Default for SimpleIssueDetector {
    fn default() -> Self {
        Self::new()
    }
}

/// Tag validation result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TagValidation {
    /// Is tag valid according to schema
    pub is_valid: bool,
    /// Validation errors
    pub errors: Vec<String>,
    /// Suggested corrections
    pub suggestions: Vec<String>,
}

impl TagValidation {
    /// Create a valid result
    pub fn valid() -> Self {
        Self {
            is_valid: true,
            errors: Vec::new(),
            suggestions: Vec::new(),
        }
    }

    /// Create an invalid result with error
    pub fn invalid(error: impl Into<String>) -> Self {
        Self {
            is_valid: false,
            errors: vec![error.into()],
            suggestions: Vec::new(),
        }
    }

    /// Add a suggestion
    pub fn with_suggestion(mut self, suggestion: impl Into<String>) -> Self {
        self.suggestions.push(suggestion.into());
        self
    }
}

/// Validate a tag value against the schema
pub fn validate_tag(tag_value: &str) -> TagValidation {
    let parts: Vec<&str> = tag_value.split(',').map(|s| s.trim()).collect();

    if parts.is_empty() {
        return TagValidation::invalid("Tag value is empty")
            .with_suggestion("Use format: 'status[,category][,priority]'");
    }

    // First part should be a valid status
    let status = CodeStatus::from_tag_value(parts[0]);
    if matches!(status, CodeStatus::Unknown) && !parts[0].is_empty() {
        return TagValidation::invalid(format!("Unknown status: '{}'", parts[0])).with_suggestion(
            "Valid statuses: new, active, stable, deprecated, old, very-old, frozen, experimental",
        );
    }

    // Optional: second part is category
    if parts.len() > 1 && TagCategory::from_str(parts[1]).is_none() {
        return TagValidation::invalid(format!("Unknown category: '{}'", parts[1]))
                .with_suggestion("Valid categories: security, performance, risk, debt, docs, testing, legacy, experimental");
    }

    TagValidation::valid()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_code_status_from_tag() {
        assert_eq!(CodeStatus::from_tag_value("new"), CodeStatus::New);
        assert_eq!(CodeStatus::from_tag_value("old"), CodeStatus::Old);
        assert_eq!(CodeStatus::from_tag_value("frozen"), CodeStatus::Frozen);
    }

    #[test]
    fn test_code_age_from_months() {
        assert_eq!(CodeAge::from_months(0), CodeAge::Fresh);
        assert_eq!(CodeAge::from_months(5), CodeAge::Moderate);
        assert_eq!(CodeAge::from_months(18), CodeAge::Old);
        assert_eq!(CodeAge::from_months(30), CodeAge::VeryOld);
    }

    #[test]
    fn test_complexity_from_metrics() {
        assert_eq!(Complexity::from_metrics(50, 3), Complexity::Simple);
        assert_eq!(Complexity::from_metrics(200, 8), Complexity::Moderate);
        assert_eq!(Complexity::from_metrics(600, 25), Complexity::Critical);
    }

    #[test]
    fn test_tag_validation() {
        let valid = validate_tag("new,security,high");
        assert!(valid.is_valid);

        let invalid = validate_tag("invalid-status");
        assert!(!invalid.is_valid);
        assert!(!invalid.errors.is_empty());
    }

    #[test]
    fn test_status_technical_debt() {
        assert!(CodeStatus::Deprecated.is_technical_debt());
        assert!(CodeStatus::Old.is_technical_debt());
        assert!(!CodeStatus::New.is_technical_debt());
        assert!(!CodeStatus::Stable.is_technical_debt());
    }

    #[test]
    fn test_issues_summary() {
        let summary = IssuesSummary {
            critical: 2,
            high: 5,
            medium: 10,
            low: 3,
        };

        assert_eq!(summary.total(), 20);
        assert!(summary.has_critical_or_high());
    }
}
