//! Task generator for converting audit findings into actionable tasks

use crate::error::{AuditError, Result};
use crate::types::{
    AuditTag, AuditTagType, Category, FileAnalysis, Issue, IssueSeverity, Task, TaskPriority,
};
use std::collections::HashMap;

/// Task generator
pub struct TaskGenerator {
    /// Task ID counter
    counter: usize,
    /// Generated tasks
    tasks: Vec<Task>,
}

impl TaskGenerator {
    /// Create a new task generator
    pub fn new() -> Self {
        Self {
            counter: 0,
            tasks: Vec::new(),
        }
    }

    /// Generate tasks from audit tags
    pub fn generate_from_tags(&mut self, tags: &[AuditTag]) -> Result<Vec<Task>> {
        for tag in tags {
            match tag.tag_type {
                AuditTagType::Todo => {
                    self.add_todo_task(tag)?;
                }
                AuditTagType::Tag => {
                    if tag.value.contains("incomplete") || tag.value.contains("TODO") {
                        self.add_incomplete_task(tag)?;
                    }
                }
                AuditTagType::Security => {
                    self.add_security_task(tag)?;
                }
                AuditTagType::Review => {
                    self.add_review_task(tag)?;
                }
                AuditTagType::Freeze => {
                    // Frozen sections don't generate tasks
                }
            }
        }

        Ok(self.tasks.clone())
    }

    /// Generate tasks from file analyses
    pub fn generate_from_analyses(&mut self, analyses: &[FileAnalysis]) -> Result<Vec<Task>> {
        for analysis in analyses {
            // Check for frozen code violations first
            if analysis
                .tags
                .iter()
                .any(|t| t.tag_type == AuditTagType::Freeze)
                && !analysis.issues.is_empty()
            {
                self.add_frozen_violation_task(analysis)?;
            }

            // Generate tasks from issues with severity-based filtering
            for issue in &analysis.issues {
                match issue.severity {
                    IssueSeverity::Critical | IssueSeverity::High => {
                        // Always generate tasks for critical/high severity
                        self.add_issue_task(issue, &analysis.category)?;
                    }
                    IssueSeverity::Medium => {
                        // Generate tasks for medium severity in critical files
                        if self.is_critical_file(&analysis.path) {
                            self.add_issue_task(issue, &analysis.category)?;
                        }
                    }
                    IssueSeverity::Low | IssueSeverity::Info => {
                        // Only generate tasks if file has many issues
                        if analysis.issues.len() > 5 {
                            self.add_issue_task(issue, &analysis.category)?;
                        }
                    }
                }
            }

            // Check documentation coverage
            if analysis.lines > 100 && analysis.doc_blocks == 0 {
                self.add_documentation_task(analysis)?;
            }
        }

        Ok(self.tasks.clone())
    }

    /// Add a TODO task
    fn add_todo_task(&mut self, tag: &AuditTag) -> Result<()> {
        let task = Task::new(
            format!("TODO: {}", tag.value),
            tag.value.clone(),
            tag.file.clone(),
            Some(tag.line),
            TaskPriority::Medium,
            Category::from_path(&tag.file.to_string_lossy()),
        )
        .with_tag("todo")
        .with_tag("from-tag");

        self.tasks.push(task);
        self.counter += 1;
        Ok(())
    }

    /// Add an incomplete implementation task
    fn add_incomplete_task(&mut self, tag: &AuditTag) -> Result<()> {
        let task = Task::new(
            format!("Complete implementation: {}", tag.file.display()),
            format!("File marked as incomplete: {}", tag.value),
            tag.file.clone(),
            Some(tag.line),
            TaskPriority::High,
            Category::from_path(&tag.file.to_string_lossy()),
        )
        .with_tag("incomplete")
        .with_tag("implementation")
        .with_tag("from-tag");

        self.tasks.push(task);
        self.counter += 1;
        Ok(())
    }

    /// Add a security task
    fn add_security_task(&mut self, tag: &AuditTag) -> Result<()> {
        let task = Task::new(
            format!("Security: {}", tag.value),
            format!(
                "Security concern found in {}: {}",
                tag.file.display(),
                tag.value
            ),
            tag.file.clone(),
            Some(tag.line),
            TaskPriority::Critical,
            Category::from_path(&tag.file.to_string_lossy()),
        )
        .with_tag("security")
        .with_tag("from-tag");

        self.tasks.push(task);
        self.counter += 1;
        Ok(())
    }

    /// Add a review task
    fn add_review_task(&mut self, tag: &AuditTag) -> Result<()> {
        let task = Task::new(
            format!("Review: {}", tag.value),
            format!("Code review needed: {}", tag.value),
            tag.file.clone(),
            Some(tag.line),
            TaskPriority::Medium,
            Category::from_path(&tag.file.to_string_lossy()),
        )
        .with_tag("review")
        .with_tag("from-tag");

        self.tasks.push(task);
        self.counter += 1;
        Ok(())
    }

    /// Add a task from an issue
    fn add_issue_task(&mut self, issue: &Issue, category: &Category) -> Result<()> {
        let priority = match issue.severity {
            IssueSeverity::Critical => TaskPriority::Critical,
            IssueSeverity::High => TaskPriority::High,
            IssueSeverity::Medium => TaskPriority::Medium,
            IssueSeverity::Low | IssueSeverity::Info => TaskPriority::Low,
        };

        let mut task = Task::new(
            format!("{:?}: {}", issue.category, issue.message),
            issue.message.clone(),
            issue.file.clone(),
            Some(issue.line),
            priority,
            *category,
        )
        .with_tag(format!("{:?}", issue.severity).to_lowercase())
        .with_tag(format!("{:?}", issue.category).to_lowercase())
        .with_tag("from-issue");

        if let Some(suggestion) = &issue.suggestion {
            task.description = format!("{}\n\nSuggestion: {}", task.description, suggestion);
        }

        self.tasks.push(task);
        self.counter += 1;
        Ok(())
    }

    /// Add a documentation task
    fn add_documentation_task(&mut self, analysis: &FileAnalysis) -> Result<()> {
        let task = Task::new(
            format!("Add documentation: {}", analysis.path.display()),
            format!(
                "File has {} lines but no documentation blocks",
                analysis.lines
            ),
            analysis.path.clone(),
            None,
            TaskPriority::Low,
            analysis.category,
        )
        .with_tag("documentation")
        .with_tag("from-analysis");

        self.tasks.push(task);
        self.counter += 1;
        Ok(())
    }

    /// Check if a file is critical for the system
    fn is_critical_file(&self, path: &std::path::Path) -> bool {
        let path_str = path.to_string_lossy();
        path_str.contains("kill_switch")
            || path_str.contains("circuit_breaker")
            || path_str.contains("conscience")
            || path_str.contains("risk")
            || path_str.contains("execution")
            || path_str.contains("amygdala")
            || path_str.contains("cerebellum")
            || path_str.ends_with("main.rs")
            || path_str.ends_with("main.py")
    }

    /// Add a task for frozen code violations
    fn add_frozen_violation_task(&mut self, analysis: &FileAnalysis) -> Result<()> {
        let task = Task::new(
            format!("FROZEN CODE VIOLATION: {}", analysis.path.display()),
            format!(
                "File marked as @audit-freeze has {} issues. Frozen code should not be modified or should have no issues.\n\nIssues found:\n{}",
                analysis.issues.len(),
                analysis.issues.iter()
                    .take(3)
                    .map(|i| format!("  - Line {}: {}", i.line, i.message))
                    .collect::<Vec<_>>()
                    .join("\n")
            ),
            analysis.path.clone(),
            None,
            TaskPriority::Critical,
            analysis.category,
        )
        .with_tag("frozen-violation")
        .with_tag("critical")
        .with_tag("audit-freeze");

        self.tasks.push(task);
        self.counter += 1;
        Ok(())
    }

    /// Get all tasks
    pub fn tasks(&self) -> &[Task] {
        &self.tasks
    }

    /// Get tasks by priority
    pub fn tasks_by_priority(&self, priority: TaskPriority) -> Vec<&Task> {
        self.tasks
            .iter()
            .filter(|t| t.priority == priority)
            .collect()
    }

    /// Get tasks by category
    pub fn tasks_by_category(&self, category: Category) -> Vec<&Task> {
        self.tasks
            .iter()
            .filter(|t| t.category == category)
            .collect()
    }

    /// Get task statistics
    pub fn statistics(&self) -> TaskStatistics {
        let mut stats = TaskStatistics {
            total: self.tasks.len(),
            ..Default::default()
        };

        for task in &self.tasks {
            match task.priority {
                TaskPriority::Critical => stats.critical += 1,
                TaskPriority::High => stats.high += 1,
                TaskPriority::Medium => stats.medium += 1,
                TaskPriority::Low => stats.low += 1,
            }

            *stats.by_category.entry(task.category).or_insert(0) += 1;
        }

        stats
    }

    /// Export tasks to JSON
    pub fn to_json(&self) -> Result<String> {
        serde_json::to_string_pretty(&self.tasks).map_err(AuditError::Json)
    }

    /// Export tasks to CSV
    pub fn to_csv(&self) -> Result<String> {
        let mut csv = String::from("ID,Title,File,Line,Priority,Category,Tags\n");

        for task in &self.tasks {
            let line = task.line.map(|l| l.to_string()).unwrap_or_default();
            let tags = task.tags.join(";");
            csv.push_str(&format!(
                "{},{},{},{},{:?},{:?},{}\n",
                task.id,
                task.title.replace(",", ";"),
                task.file.display(),
                line,
                task.priority,
                task.category,
                tags
            ));
        }

        Ok(csv)
    }

    /// Clear all tasks
    pub fn clear(&mut self) {
        self.tasks.clear();
        self.counter = 0;
    }
}

impl Default for TaskGenerator {
    fn default() -> Self {
        Self::new()
    }
}

/// Task statistics
#[derive(Debug, Clone, Default)]
pub struct TaskStatistics {
    /// Total tasks
    pub total: usize,
    /// Critical priority
    pub critical: usize,
    /// High priority
    pub high: usize,
    /// Medium priority
    pub medium: usize,
    /// Low priority
    pub low: usize,
    /// Tasks by category
    pub by_category: HashMap<Category, usize>,
}

impl TaskStatistics {
    /// Get a summary string
    pub fn summary(&self) -> String {
        format!(
            "Total: {}, Critical: {}, High: {}, Medium: {}, Low: {}",
            self.total, self.critical, self.high, self.medium, self.low
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn test_generate_from_todo_tag() {
        let mut generator = TaskGenerator::new();
        let tags = vec![AuditTag {
            tag_type: AuditTagType::Todo,
            file: PathBuf::from("test.rs"),
            line: 10,
            value: "Implement error handling".to_string(),
            context: None,
        }];

        let tasks = generator.generate_from_tags(&tags).unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].priority, TaskPriority::Medium);
        assert!(tasks[0].tags.contains(&"todo".to_string()));
    }

    #[test]
    fn test_generate_from_security_tag() {
        let mut generator = TaskGenerator::new();
        let tags = vec![AuditTag {
            tag_type: AuditTagType::Security,
            file: PathBuf::from("auth.rs"),
            line: 42,
            value: "Validate input data".to_string(),
            context: None,
        }];

        let tasks = generator.generate_from_tags(&tags).unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].priority, TaskPriority::Critical);
        assert!(tasks[0].tags.contains(&"security".to_string()));
    }

    #[test]
    fn test_statistics() {
        let mut generator = TaskGenerator::new();

        // Add tasks with different priorities
        generator.tasks.push(Task::new(
            "Critical Task",
            "Description",
            PathBuf::from("test.rs"),
            None,
            TaskPriority::Critical,
            Category::Janus,
        ));

        generator.tasks.push(Task::new(
            "High Task",
            "Description",
            PathBuf::from("test.rs"),
            None,
            TaskPriority::High,
            Category::Janus,
        ));

        let stats = generator.statistics();
        assert_eq!(stats.total, 2);
        assert_eq!(stats.critical, 1);
        assert_eq!(stats.high, 1);
    }

    #[test]
    fn test_to_json() {
        let mut generator = TaskGenerator::new();
        generator.tasks.push(Task::new(
            "Test Task",
            "Test Description",
            PathBuf::from("test.rs"),
            Some(10),
            TaskPriority::Medium,
            Category::Janus,
        ));

        let json = generator.to_json().unwrap();
        assert!(json.contains("Test Task"));
        assert!(json.contains("test.rs"));
    }

    #[test]
    fn test_tasks_by_priority() {
        let mut generator = TaskGenerator::new();

        generator.tasks.push(Task::new(
            "Critical",
            "Desc",
            PathBuf::from("test.rs"),
            None,
            TaskPriority::Critical,
            Category::Janus,
        ));

        generator.tasks.push(Task::new(
            "Low",
            "Desc",
            PathBuf::from("test.rs"),
            None,
            TaskPriority::Low,
            Category::Janus,
        ));

        let critical = generator.tasks_by_priority(TaskPriority::Critical);
        assert_eq!(critical.len(), 1);
        assert_eq!(critical[0].priority, TaskPriority::Critical);
    }

    #[test]
    fn test_severity_based_filtering() {
        use crate::types::{FileAnalysis, FilePriority, Issue, IssueCategory};

        let mut generator = TaskGenerator::new();

        // Create file with critical and low severity issues
        let analysis = FileAnalysis {
            path: PathBuf::from("normal_file.rs"),
            category: Category::Janus,
            priority: FilePriority::Medium,
            lines: 100,
            doc_blocks: 0,
            security_rating: None,
            issues: vec![
                Issue {
                    severity: IssueSeverity::Critical,
                    category: IssueCategory::Security,
                    file: PathBuf::from("normal_file.rs"),
                    line: 10,
                    message: "Critical issue".to_string(),
                    suggestion: None,
                },
                Issue {
                    severity: IssueSeverity::Low,
                    category: IssueCategory::CodeQuality,
                    file: PathBuf::from("normal_file.rs"),
                    line: 20,
                    message: "Low issue".to_string(),
                    suggestion: None,
                },
            ],
            llm_analysis: None,
            tags: vec![],
        };

        generator.generate_from_analyses(&[analysis]).unwrap();

        // Should only generate task for critical issue, not low
        assert_eq!(generator.tasks().len(), 1);
        assert_eq!(generator.tasks()[0].priority, TaskPriority::Critical);
    }

    #[test]
    fn test_critical_file_detection() {
        use crate::types::{FileAnalysis, FilePriority, Issue, IssueCategory};

        let mut generator = TaskGenerator::new();

        // Medium severity issue in critical file should generate task
        let analysis = FileAnalysis {
            path: PathBuf::from("src/kill_switch.rs"),
            category: Category::Execution,
            priority: FilePriority::Critical,
            lines: 100,
            doc_blocks: 5,
            security_rating: None,
            issues: vec![Issue {
                severity: IssueSeverity::Medium,
                category: IssueCategory::RiskManagement,
                file: PathBuf::from("src/kill_switch.rs"),
                line: 42,
                message: "Medium issue in critical file".to_string(),
                suggestion: None,
            }],
            llm_analysis: None,
            tags: vec![],
        };

        generator.generate_from_analyses(&[analysis]).unwrap();

        // Should generate task because it's a critical file
        assert_eq!(generator.tasks().len(), 1);
        assert_eq!(generator.tasks()[0].priority, TaskPriority::Medium);
    }

    #[test]
    fn test_frozen_code_violation() {
        use crate::types::{FileAnalysis, FilePriority, Issue, IssueCategory};

        let mut generator = TaskGenerator::new();

        // File with freeze tag but has issues
        let analysis = FileAnalysis {
            path: PathBuf::from("frozen.rs"),
            category: Category::Janus,
            priority: FilePriority::High,
            lines: 50,
            doc_blocks: 10,
            security_rating: None,
            issues: vec![Issue {
                severity: IssueSeverity::Medium,
                category: IssueCategory::CodeQuality,
                file: PathBuf::from("frozen.rs"),
                line: 15,
                message: "Issue in frozen code".to_string(),
                suggestion: None,
            }],
            llm_analysis: None,
            tags: vec![AuditTag {
                tag_type: AuditTagType::Freeze,
                file: PathBuf::from("frozen.rs"),
                line: 1,
                value: String::new(),
                context: None,
            }],
        };

        generator.generate_from_analyses(&[analysis]).unwrap();

        // Should generate frozen violation task (critical) plus the medium issue task
        assert!(!generator.tasks().is_empty());

        // Check for frozen violation task
        let frozen_task = generator
            .tasks()
            .iter()
            .find(|t| t.tags.contains(&"frozen-violation".to_string()));
        assert!(frozen_task.is_some());
        assert_eq!(frozen_task.unwrap().priority, TaskPriority::Critical);
    }
}
