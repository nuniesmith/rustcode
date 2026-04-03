//! Compatibility layer for old Scanner interface
//!
//! This module provides backward compatibility with the old Scanner interface
//! that was used by enhanced_scanner and server modules.

use crate::error::Result;
use crate::tags::TagScanner;
use crate::types::{
    AuditReport, AuditRequest, AuditSummary, Category, FileAnalysis, FilePriority, Issue,
    IssueCategory, IssueSeverity, SystemMap,
};
use ignore::WalkBuilder;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use tracing::{debug, info};

/// Scanner for analyzing codebases (compatibility layer)
pub struct Scanner {
    /// Root directory to scan
    root: PathBuf,
    /// Tag scanner
    tag_scanner: TagScanner,
    /// Maximum file size to scan (bytes)
    max_file_size: usize,
    /// Whether to include tests
    include_tests: bool,
}

impl Scanner {
    /// Create a new scanner
    pub fn new(root: PathBuf, max_file_size: usize, include_tests: bool) -> Result<Self> {
        let tag_scanner = TagScanner::new()?;

        Ok(Self {
            root,
            tag_scanner,
            max_file_size,
            include_tests,
        })
    }

    /// Scan the codebase and generate a report
    pub fn scan(&self, _request: &AuditRequest) -> Result<AuditReport> {
        info!("Starting codebase scan at {}", self.root.display());

        // Build system map
        let system_map = self.build_system_map()?;

        // Scan all files
        let files = self.scan_files()?;

        // Calculate summary
        let summary = self.calculate_summary(&files);

        // Generate tasks from analyses (placeholder - will be filled by TaskGenerator)
        let tasks = Vec::new();

        // Count issues by severity
        let mut issues_by_severity = HashMap::new();
        for file in &files {
            for issue in &file.issues {
                *issues_by_severity.entry(issue.severity).or_insert(0) += 1;
            }
        }

        Ok(AuditReport {
            id: uuid::Uuid::new_v4().to_string(),
            repository: self.root.to_string_lossy().to_string(),
            branch: "main".to_string(),
            created_at: chrono::Utc::now(),
            system_map,
            files,
            tasks,
            issues_by_severity,
            summary,
            test_results: None,
            context_bundle: None,
        })
    }

    /// Build a system map of the codebase
    fn build_system_map(&self) -> Result<SystemMap> {
        debug!("Building system map");

        // Create a simple system map compatible with current types
        Ok(SystemMap {
            total_files: 0,
            files_by_category: HashMap::new(),
            lines_by_category: HashMap::new(),
            dependencies: Vec::new(),
            mermaid_diagram: None,
        })
    }

    /// Scan all files in the codebase
    fn scan_files(&self) -> Result<Vec<FileAnalysis>> {
        let mut analyses = Vec::new();

        let walk = WalkBuilder::new(&self.root)
            .hidden(false)
            .git_ignore(true)
            .build();

        for entry in walk.flatten() {
            let path = entry.path();
            if path.is_file() {
                if let Some(analysis) = self.scan_file(path)? {
                    analyses.push(analysis);
                }
            }
        }

        info!("Scanned {} files", analyses.len());
        Ok(analyses)
    }

    /// Scan a single file
    fn scan_file(&self, path: &Path) -> Result<Option<FileAnalysis>> {
        // Skip files that are too large
        if let Ok(metadata) = fs::metadata(path) {
            if metadata.len() > self.max_file_size as u64 {
                debug!("Skipping large file: {}", path.display());
                return Ok(None);
            }
        }

        // Skip test files if not included
        if !self.include_tests && is_test_file(path) {
            return Ok(None);
        }

        // Read file content
        let content = match fs::read_to_string(path) {
            Ok(c) => c,
            Err(_) => {
                debug!("Skipping non-UTF8 file: {}", path.display());
                return Ok(None);
            }
        };

        // Determine category
        let category = categorize_file(path);

        // Scan for tags and issues
        let tags = self.tag_scanner.scan_file(path)?;
        let issues = detect_issues(path, &content);

        // Calculate priority
        let priority = calculate_priority(&issues, &category);

        let rel_path = path
            .strip_prefix(&self.root)
            .unwrap_or(path)
            .to_string_lossy()
            .to_string();

        Ok(Some(FileAnalysis {
            path: PathBuf::from(rel_path),
            category,
            priority,
            lines: content.lines().count(),
            doc_blocks: 0,
            security_rating: None,
            issues,
            llm_analysis: None,
            tags,
        }))
    }

    /// Calculate summary statistics
    fn calculate_summary(&self, files: &[FileAnalysis]) -> AuditSummary {
        let total_files = files.len();
        let total_lines: usize = files.iter().map(|f| f.lines).sum();

        let mut total_issues = 0;
        let mut critical_files = 0;
        for file in files {
            total_issues += file.issues.len();
            if file
                .issues
                .iter()
                .any(|i| i.severity == IssueSeverity::Critical)
            {
                critical_files += 1;
            }
        }

        AuditSummary {
            total_files,
            total_lines,
            total_issues,
            total_tasks: 0, // Will be filled by TaskGenerator
            critical_files,
            avg_security_rating: None,
            total_tests: None,
            test_pass_rate: None,
            code_coverage: None,
        }
    }
}

/// Check if a file is a test file
fn is_test_file(path: &Path) -> bool {
    let path_str = path.to_string_lossy();
    path_str.contains("/test/")
        || path_str.contains("/tests/")
        || path_str.ends_with("_test.rs")
        || path_str.ends_with(".test.js")
        || path_str.ends_with(".spec.ts")
}

/// Categorize a file based on its path and name
fn categorize_file(path: &Path) -> Category {
    let path_str = path.to_string_lossy().to_lowercase();

    if path_str.contains("/test/") || path_str.contains("/tests/") {
        Category::Tests
    } else if path_str.contains("/docs/") || path_str.ends_with(".md") {
        Category::Documentation
    } else if path_str.contains("/config/")
        || path_str.contains(".toml")
        || path_str.contains(".yaml")
        || path_str.contains(".json")
    {
        Category::Config
    } else if path_str.contains("/infra/") || path_str.contains("/docker/") {
        Category::Infra
    } else if path_str.contains("/audit/") {
        Category::Audit
    } else if path_str.contains("/client/") {
        Category::Clients
    } else if path_str.contains("/execution/") {
        Category::Execution
    } else if path_str.contains("/janus/") || path_str.contains("/core/") {
        Category::Janus
    } else {
        Category::Other
    }
}

/// Detect issues in file content
fn detect_issues(path: &Path, content: &str) -> Vec<Issue> {
    let mut issues = Vec::new();

    // Check for TODO/FIXME/HACK comments
    for (line_num, line) in content.lines().enumerate() {
        let line_lower = line.to_lowercase();

        if line_lower.contains("todo") {
            issues.push(Issue {
                severity: IssueSeverity::Low,
                category: IssueCategory::CodeQuality,
                message: format!("TODO found at line {}", line_num + 1),
                file: PathBuf::from(path.to_string_lossy().to_string()),
                line: line_num + 1,
                suggestion: Some("Complete or remove TODO".to_string()),
            });
        }

        if line_lower.contains("fixme") {
            issues.push(Issue {
                severity: IssueSeverity::Medium,
                category: IssueCategory::CodeQuality,
                message: format!("FIXME found at line {}", line_num + 1),
                file: PathBuf::from(path.to_string_lossy().to_string()),
                line: line_num + 1,
                suggestion: Some("Fix the issue".to_string()),
            });
        }

        if line_lower.contains("hack") {
            issues.push(Issue {
                severity: IssueSeverity::Medium,
                category: IssueCategory::CodeQuality,
                message: format!("HACK found at line {}", line_num + 1),
                file: PathBuf::from(path.to_string_lossy().to_string()),
                line: line_num + 1,
                suggestion: Some("Replace hack with proper solution".to_string()),
            });
        }

        // Security patterns
        if line_lower.contains("unsafe") && path.extension().is_some_and(|e| e == "rs") {
            issues.push(Issue {
                severity: IssueSeverity::High,
                category: IssueCategory::Security,
                message: format!("Unsafe code at line {}", line_num + 1),
                file: PathBuf::from(path.to_string_lossy().to_string()),
                line: line_num + 1,
                suggestion: Some("Review unsafe code for safety".to_string()),
            });
        }

        if line_lower.contains("unwrap()") && path.extension().is_some_and(|e| e == "rs") {
            issues.push(Issue {
                severity: IssueSeverity::Low,
                category: IssueCategory::CodeQuality,
                message: format!("unwrap() at line {}", line_num + 1),
                file: PathBuf::from(path.to_string_lossy().to_string()),
                line: line_num + 1,
                suggestion: Some("Use proper error handling".to_string()),
            });
        }
    }

    issues
}

/// Calculate file priority based on issues and category
fn calculate_priority(issues: &[Issue], category: &Category) -> FilePriority {
    let has_critical = issues.iter().any(|i| i.severity == IssueSeverity::Critical);
    let has_high = issues.iter().any(|i| i.severity == IssueSeverity::High);

    if has_critical {
        FilePriority::Critical
    } else if has_high || *category == Category::Janus {
        FilePriority::High
    } else if issues.len() > 5 {
        FilePriority::Medium
    } else {
        FilePriority::Low
    }
}
