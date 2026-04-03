//! File scoring system for audit analysis
//!
//! Provides comprehensive scoring of files based on:
//! - Audit tags (@audit-tag, @audit-security, etc.)
//! - TODO comments and priorities
//! - Code complexity metrics
//! - Dependencies and relationships
//! - Security concerns

use crate::error::Result;
use crate::todo_scanner::{TodoItem, TodoPriority};
use crate::types::AuditTag;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// File score with multiple dimensions
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileScore {
    /// File path
    pub path: PathBuf,

    /// Overall importance score (0-100)
    pub importance: f64,

    /// Risk score (0-100, higher = more risky)
    pub risk: f64,

    /// Quality score (0-100)
    pub quality: f64,

    /// Complexity score (0-100)
    pub complexity: f64,

    /// Technical debt score (0-100)
    pub tech_debt: f64,

    /// Security concern level (0-100)
    pub security: f64,

    /// Maintenance priority (0-100)
    pub maintenance_priority: f64,

    /// Breakdown of score components
    pub breakdown: ScoreBreakdown,
}

/// Detailed breakdown of score components
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ScoreBreakdown {
    /// Audit tags found
    pub audit_tags: Vec<String>,

    /// TODO count by priority
    pub todos: TodoBreakdown,

    /// Security tags count
    pub security_tags: usize,

    /// Freeze tags (critical code)
    pub freeze_tags: usize,

    /// Experimental tags
    pub experimental_tags: usize,

    /// Deprecated tags
    pub deprecated_tags: usize,

    /// File size in lines
    pub lines_of_code: usize,

    /// Estimated complexity (based on patterns)
    pub complexity_indicators: ComplexityIndicators,

    /// Critical issues count
    pub critical_issues: usize,

    /// High priority issues
    pub high_priority_issues: usize,
}

/// TODO breakdown by priority
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TodoBreakdown {
    pub high: usize,
    pub medium: usize,
    pub low: usize,
    pub total: usize,
}

/// Complexity indicators
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComplexityIndicators {
    /// Unwrap/panic patterns (Rust)
    pub unwraps_and_panics: usize,

    /// Unsafe blocks (Rust)
    pub unsafe_blocks: usize,

    /// Nested depth estimate
    pub estimated_nesting: usize,

    /// Function count estimate
    pub estimated_functions: usize,

    /// Comment density (0-100)
    pub comment_density: f64,
}

impl FileScore {
    /// Create a new file score
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            importance: 0.0,
            risk: 0.0,
            quality: 100.0,
            complexity: 0.0,
            tech_debt: 0.0,
            security: 0.0,
            maintenance_priority: 0.0,
            breakdown: ScoreBreakdown::default(),
        }
    }

    /// Calculate overall health score (0-100, higher is better)
    pub fn health_score(&self) -> f64 {
        // Weighted average: quality high weight, risk/debt reduce score
        let quality_weight = 0.4;
        let risk_penalty = 0.3;
        let debt_penalty = 0.2;
        let security_penalty = 0.1;

        let score = (self.quality * quality_weight)
            - (self.risk * risk_penalty)
            - (self.tech_debt * debt_penalty)
            - (self.security * security_penalty);

        score.clamp(0.0, 100.0)
    }

    /// Get priority rating (Critical, High, Medium, Low, Minimal)
    pub fn priority_rating(&self) -> &'static str {
        let priority = self.maintenance_priority;
        if priority >= 80.0 {
            "Critical"
        } else if priority >= 60.0 {
            "High"
        } else if priority >= 40.0 {
            "Medium"
        } else if priority >= 20.0 {
            "Low"
        } else {
            "Minimal"
        }
    }

    /// Get risk rating
    pub fn risk_rating(&self) -> &'static str {
        if self.risk >= 80.0 {
            "Critical"
        } else if self.risk >= 60.0 {
            "High"
        } else if self.risk >= 40.0 {
            "Medium"
        } else if self.risk >= 20.0 {
            "Low"
        } else {
            "Minimal"
        }
    }

    /// Whether this file needs immediate attention
    pub fn needs_immediate_attention(&self) -> bool {
        self.risk >= 80.0
            || self.security >= 80.0
            || self.breakdown.critical_issues > 0
            || self.breakdown.todos.high > 0
    }
}

impl Default for ComplexityIndicators {
    fn default() -> Self {
        Self {
            unwraps_and_panics: 0,
            unsafe_blocks: 0,
            estimated_nesting: 0,
            estimated_functions: 0,
            comment_density: 0.0,
        }
    }
}

/// File scorer - calculates scores for files
pub struct FileScorer {
    /// Weights for different scoring components
    weights: ScoringWeights,
}

/// Configurable weights for scoring
#[derive(Debug, Clone)]
pub struct ScoringWeights {
    /// Weight for freeze tags (critical code)
    pub freeze_importance: f64,

    /// Weight for security tags
    pub security_importance: f64,

    /// Weight for file size
    pub size_importance: f64,

    /// Weight for TODO density
    pub todo_risk: f64,

    /// Weight for experimental code
    pub experimental_risk: f64,

    /// Weight for deprecated code
    pub deprecated_debt: f64,

    /// Weight for complexity
    pub complexity_factor: f64,
}

impl Default for ScoringWeights {
    fn default() -> Self {
        Self {
            freeze_importance: 30.0,
            security_importance: 25.0,
            size_importance: 15.0,
            todo_risk: 20.0,
            experimental_risk: 15.0,
            deprecated_debt: 25.0,
            complexity_factor: 1.0,
        }
    }
}

impl FileScorer {
    /// Create a new file scorer with default weights
    pub fn new() -> Self {
        Self {
            weights: ScoringWeights::default(),
        }
    }

    /// Create with custom weights
    pub fn with_weights(weights: ScoringWeights) -> Self {
        Self { weights }
    }

    /// Score a file based on tags, TODOs, and content
    pub fn score_file(
        &self,
        path: &Path,
        content: &str,
        tags: &[AuditTag],
        todos: &[TodoItem],
    ) -> Result<FileScore> {
        let mut score = FileScore::new(path.to_path_buf());
        let mut breakdown = ScoreBreakdown::default();

        // Analyze audit tags
        use crate::types::AuditTagType;

        for tag in tags {
            breakdown.audit_tags.push(format!("{:?}", tag.tag_type));

            match tag.tag_type {
                AuditTagType::Freeze => breakdown.freeze_tags += 1,
                AuditTagType::Security => breakdown.security_tags += 1,
                AuditTagType::Tag => {
                    // Check the value for experimental/deprecated
                    if tag.value.contains("experimental") {
                        breakdown.experimental_tags += 1;
                    } else if tag.value.contains("deprecated") {
                        breakdown.deprecated_tags += 1;
                    }
                }
                _ => {}
            }
        }

        // Analyze TODOs
        for todo in todos {
            breakdown.todos.total += 1;
            match todo.priority {
                TodoPriority::High => {
                    breakdown.todos.high += 1;
                    breakdown.high_priority_issues += 1;
                    // Treat High as critical for now
                    breakdown.critical_issues += 1;
                }
                TodoPriority::Medium => breakdown.todos.medium += 1,
                TodoPriority::Low => breakdown.todos.low += 1,
            }
        }

        // Analyze content
        breakdown.lines_of_code = content.lines().count();
        breakdown.complexity_indicators = self.analyze_complexity(content);

        score.breakdown = breakdown.clone();

        // Calculate scores
        score.importance = self.calculate_importance(&breakdown);
        score.risk = self.calculate_risk(&breakdown);
        score.quality = self.calculate_quality(&breakdown);
        score.complexity = self.calculate_complexity(&breakdown);
        score.tech_debt = self.calculate_tech_debt(&breakdown);
        score.security = self.calculate_security(&breakdown);
        score.maintenance_priority = self.calculate_maintenance_priority(&breakdown);

        Ok(score)
    }

    /// Calculate importance score (0-100)
    fn calculate_importance(&self, breakdown: &ScoreBreakdown) -> f64 {
        let mut importance = 0.0;

        // Freeze tags indicate critical code
        importance += breakdown.freeze_tags as f64 * self.weights.freeze_importance;

        // Security tags indicate important security code
        importance += breakdown.security_tags as f64 * self.weights.security_importance;

        // Large files are often important (but with diminishing returns)
        let size_factor = (breakdown.lines_of_code as f64 / 100.0).min(5.0);
        importance += size_factor * self.weights.size_importance;

        // High complexity might indicate important/core logic
        if breakdown.complexity_indicators.estimated_functions > 10 {
            importance += 10.0;
        }

        importance.min(100.0)
    }

    /// Calculate risk score (0-100)
    fn calculate_risk(&self, breakdown: &ScoreBreakdown) -> f64 {
        let mut risk = 0.0;

        // High priority TODOs are high risk
        risk += breakdown.todos.high as f64 * 20.0;
        risk += breakdown.todos.medium as f64 * 5.0;

        // Experimental code is risky
        risk += breakdown.experimental_tags as f64 * self.weights.experimental_risk;

        // Security tags indicate potential vulnerabilities
        risk += breakdown.security_tags as f64 * 20.0;

        // Unwraps and panics are risky in Rust
        risk += breakdown.complexity_indicators.unwraps_and_panics as f64 * 5.0;

        // Unsafe blocks are risky
        risk += breakdown.complexity_indicators.unsafe_blocks as f64 * 15.0;

        // Critical issues
        risk += breakdown.critical_issues as f64 * 30.0;

        risk.min(100.0)
    }

    /// Calculate quality score (0-100, starts at 100)
    fn calculate_quality(&self, breakdown: &ScoreBreakdown) -> f64 {
        let mut quality = 100.0;

        // TODOs reduce quality
        quality -= breakdown.todos.total as f64 * 2.0;

        // Low comment density reduces quality
        if breakdown.complexity_indicators.comment_density < 10.0 {
            quality -= 15.0;
        }

        // Deprecated code reduces quality
        quality -= breakdown.deprecated_tags as f64 * 10.0;

        // Unwraps/panics reduce quality
        quality -= breakdown.complexity_indicators.unwraps_and_panics as f64 * 3.0;

        quality.max(0.0)
    }

    /// Calculate complexity score (0-100)
    fn calculate_complexity(&self, breakdown: &ScoreBreakdown) -> f64 {
        let mut complexity = 0.0;

        // File size
        complexity += (breakdown.lines_of_code as f64 / 50.0).min(30.0);

        // Function count
        complexity += (breakdown.complexity_indicators.estimated_functions as f64 * 2.0).min(20.0);

        // Nesting depth
        complexity += breakdown.complexity_indicators.estimated_nesting as f64 * 10.0;

        // Unsafe blocks add complexity
        complexity += breakdown.complexity_indicators.unsafe_blocks as f64 * 5.0;

        complexity.min(100.0)
    }

    /// Calculate technical debt score (0-100)
    fn calculate_tech_debt(&self, breakdown: &ScoreBreakdown) -> f64 {
        let mut debt = 0.0;

        // TODOs are debt
        debt += breakdown.todos.total as f64 * 5.0;
        debt += breakdown.todos.high as f64 * 10.0;

        // Deprecated code is debt
        debt += breakdown.deprecated_tags as f64 * self.weights.deprecated_debt;

        // Experimental code can be debt
        debt += breakdown.experimental_tags as f64 * 10.0;

        debt.min(100.0)
    }

    /// Calculate security concern score (0-100)
    fn calculate_security(&self, breakdown: &ScoreBreakdown) -> f64 {
        let mut security = 0.0;

        // Security tags indicate concerns
        security += breakdown.security_tags as f64 * 30.0;

        // Unsafe blocks are security concerns
        security += breakdown.complexity_indicators.unsafe_blocks as f64 * 20.0;

        security.min(100.0)
    }

    /// Calculate maintenance priority (0-100)
    fn calculate_maintenance_priority(&self, breakdown: &ScoreBreakdown) -> f64 {
        let mut priority = 0.0;

        // High priority TODOs need immediate attention
        priority += breakdown.todos.high as f64 * 30.0;

        // Security concerns
        priority += breakdown.security_tags as f64 * 25.0;

        // Deprecated code should be updated
        priority += breakdown.deprecated_tags as f64 * 15.0;

        // Critical issues
        priority += breakdown.critical_issues as f64 * 50.0;

        priority.min(100.0)
    }

    /// Analyze code complexity from content
    fn analyze_complexity(&self, content: &str) -> ComplexityIndicators {
        let mut indicators = ComplexityIndicators::default();

        let lines: Vec<&str> = content.lines().collect();
        let total_lines = lines.len();
        let mut comment_lines = 0;
        let mut max_indent = 0;

        for line in &lines {
            let trimmed = line.trim();

            // Count comments
            if trimmed.starts_with("//") || trimmed.starts_with('#') || trimmed.starts_with("/*") {
                comment_lines += 1;
            }

            // Count unwraps and panics (Rust)
            if trimmed.contains(".unwrap()") || trimmed.contains(".expect(") {
                indicators.unwraps_and_panics += 1;
            }
            if trimmed.contains("panic!") {
                indicators.unwraps_and_panics += 1;
            }

            // Count unsafe blocks
            if trimmed.starts_with("unsafe ") || trimmed.contains("unsafe {") {
                indicators.unsafe_blocks += 1;
            }

            // Estimate nesting from indentation
            let indent = line.len() - line.trim_start().len();
            if indent > max_indent {
                max_indent = indent;
            }

            // Estimate function count
            if trimmed.starts_with("fn ") || trimmed.starts_with("pub fn ")
                || trimmed.starts_with("async fn ") || trimmed.starts_with("pub async fn ")
                || trimmed.starts_with("def ") // Python
                || trimmed.starts_with("function ") // JS
                || trimmed.starts_with("func ")
            // Kotlin/Go
            {
                indicators.estimated_functions += 1;
            }
        }

        // Calculate comment density
        if total_lines > 0 {
            indicators.comment_density = (comment_lines as f64 / total_lines as f64) * 100.0;
        }

        // Estimate nesting level (assuming 4-space indents)
        indicators.estimated_nesting = (max_indent / 4).min(10);

        indicators
    }

    /// Score multiple files and return sorted by priority
    pub fn score_files(
        &self,
        files: &[(PathBuf, String, Vec<AuditTag>, Vec<TodoItem>)],
    ) -> Result<Vec<FileScore>> {
        let mut scores = Vec::new();

        for (path, content, tags, todos) in files {
            let score = self.score_file(path, content, tags, todos)?;
            scores.push(score);
        }

        // Sort by maintenance priority (highest first)
        scores.sort_by(|a, b| {
            b.maintenance_priority
                .partial_cmp(&a.maintenance_priority)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        Ok(scores)
    }
}

impl Default for FileScorer {
    fn default() -> Self {
        Self::new()
    }
}

/// Aggregate scoring statistics for a codebase
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodebaseScore {
    /// Total files scored
    pub total_files: usize,

    /// Average scores across all files
    pub averages: FileScore,

    /// Files needing immediate attention
    pub critical_files: Vec<PathBuf>,

    /// High priority files
    pub high_priority_files: Vec<PathBuf>,

    /// Files with best health scores
    pub healthiest_files: Vec<PathBuf>,

    /// Files with worst health scores
    pub unhealthiest_files: Vec<PathBuf>,

    /// Total TODOs across codebase
    pub total_todos: TodoBreakdown,

    /// Total technical debt score
    pub total_tech_debt: f64,

    /// Overall codebase health (0-100)
    pub overall_health: f64,
}

impl CodebaseScore {
    /// Create codebase score from individual file scores
    pub fn from_file_scores(scores: &[FileScore]) -> Self {
        if scores.is_empty() {
            return Self::default();
        }

        let total_files = scores.len();

        // Calculate averages
        let sum_importance: f64 = scores.iter().map(|s| s.importance).sum();
        let sum_risk: f64 = scores.iter().map(|s| s.risk).sum();
        let sum_quality: f64 = scores.iter().map(|s| s.quality).sum();
        let sum_complexity: f64 = scores.iter().map(|s| s.complexity).sum();
        let sum_tech_debt: f64 = scores.iter().map(|s| s.tech_debt).sum();
        let sum_security: f64 = scores.iter().map(|s| s.security).sum();
        let sum_maintenance: f64 = scores.iter().map(|s| s.maintenance_priority).sum();

        let count = total_files as f64;
        let mut averages = FileScore::new(PathBuf::from("averages"));
        averages.importance = sum_importance / count;
        averages.risk = sum_risk / count;
        averages.quality = sum_quality / count;
        averages.complexity = sum_complexity / count;
        averages.tech_debt = sum_tech_debt / count;
        averages.security = sum_security / count;
        averages.maintenance_priority = sum_maintenance / count;

        // Collect critical and high priority files
        let mut critical_files: Vec<PathBuf> = scores
            .iter()
            .filter(|s| s.needs_immediate_attention())
            .map(|s| s.path.clone())
            .collect();
        critical_files.truncate(10);

        let mut high_priority_files: Vec<PathBuf> = scores
            .iter()
            .filter(|s| s.maintenance_priority >= 60.0 && !s.needs_immediate_attention())
            .map(|s| s.path.clone())
            .collect();
        high_priority_files.truncate(20);

        // Get healthiest and unhealthiest files
        let mut by_health = scores.to_vec();
        by_health.sort_by(|a, b| {
            b.health_score()
                .partial_cmp(&a.health_score())
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        let healthiest_files: Vec<PathBuf> =
            by_health.iter().take(10).map(|s| s.path.clone()).collect();

        let unhealthiest_files: Vec<PathBuf> = by_health
            .iter()
            .rev()
            .take(10)
            .map(|s| s.path.clone())
            .collect();

        // Aggregate TODOs
        let mut total_todos = TodoBreakdown::default();
        for score in scores {
            total_todos.high += score.breakdown.todos.high;
            total_todos.medium += score.breakdown.todos.medium;
            total_todos.low += score.breakdown.todos.low;
            total_todos.total += score.breakdown.todos.total;
        }

        // Overall health (average of individual health scores)
        let overall_health: f64 = scores.iter().map(|s| s.health_score()).sum::<f64>() / count;

        Self {
            total_files,
            averages,
            critical_files,
            high_priority_files,
            healthiest_files,
            unhealthiest_files,
            total_todos,
            total_tech_debt: sum_tech_debt,
            overall_health,
        }
    }
}

impl Default for CodebaseScore {
    fn default() -> Self {
        Self {
            total_files: 0,
            averages: FileScore::new(PathBuf::from("averages")),
            critical_files: Vec::new(),
            high_priority_files: Vec::new(),
            healthiest_files: Vec::new(),
            unhealthiest_files: Vec::new(),
            total_todos: TodoBreakdown::default(),
            total_tech_debt: 0.0,
            overall_health: 0.0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_file_score_health() {
        let mut score = FileScore::new(PathBuf::from("test.rs"));
        score.quality = 80.0;
        score.risk = 20.0;
        score.tech_debt = 10.0;
        score.security = 5.0;

        let health = score.health_score();
        assert!(health > 0.0 && health <= 100.0);
    }

    #[test]
    fn test_priority_rating() {
        let mut score = FileScore::new(PathBuf::from("test.rs"));

        score.maintenance_priority = 85.0;
        assert_eq!(score.priority_rating(), "Critical");

        score.maintenance_priority = 65.0;
        assert_eq!(score.priority_rating(), "High");

        score.maintenance_priority = 15.0;
        assert_eq!(score.priority_rating(), "Minimal");
    }

    #[test]
    fn test_needs_immediate_attention() {
        let mut score = FileScore::new(PathBuf::from("test.rs"));

        score.risk = 85.0;
        assert!(score.needs_immediate_attention());

        score.risk = 30.0;
        score.breakdown.critical_issues = 1;
        assert!(score.needs_immediate_attention());
    }

    #[test]
    fn test_complexity_analysis() {
        let scorer = FileScorer::new();
        let content = r#"
// This is a comment
fn main() {
    let x = some_value.unwrap();
    unsafe {
        // unsafe code
    }
}
"#;

        let indicators = scorer.analyze_complexity(content);
        assert!(indicators.unwraps_and_panics > 0);
        assert!(indicators.unsafe_blocks > 0);
        assert!(indicators.estimated_functions > 0);
    }
}
