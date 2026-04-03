//! TODO scanner for detecting TODO comments and tasks in source code

use crate::error::{AuditError, Result};
use crate::types::Category;
use regex::Regex;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

/// A TODO item found in code
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TodoItem {
    /// File path
    pub file: PathBuf,
    /// Line number
    pub line: usize,
    /// TODO text/description
    pub text: String,
    /// Code category
    pub category: Category,
    /// Context (surrounding lines)
    pub context: Option<String>,
    /// Priority inferred from text (high/medium/low)
    pub priority: TodoPriority,
}

/// Priority level for TODO items
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum TodoPriority {
    High,
    Medium,
    Low,
}

/// Scanner for TODO comments in source code
pub struct TodoScanner {
    /// Regex patterns for different comment styles
    patterns: Vec<Regex>,
}

impl TodoScanner {
    /// Create a new TODO scanner
    pub fn new() -> Result<Self> {
        let patterns = vec![
            // Standard TODO: comment
            Regex::new(r"(?i)(?://|#)\s*TODO:?\s*(.+)")
                .map_err(|e| AuditError::other(format!("Invalid regex: {}", e)))?,
            // Block comment TODO
            Regex::new(r"(?i)/\*\s*TODO:?\s*(.+)\*/")
                .map_err(|e| AuditError::other(format!("Invalid regex: {}", e)))?,
            // Python docstring TODO
            Regex::new(r#"(?i)["']{3}\s*TODO:?\s*(.+)["']{3}"#)
                .map_err(|e| AuditError::other(format!("Invalid regex: {}", e)))?,
            // FIXME (treat as high priority)
            Regex::new(r"(?i)(?://|#)\s*FIXME:?\s*(.+)")
                .map_err(|e| AuditError::other(format!("Invalid regex: {}", e)))?,
            // HACK (treat as medium priority)
            Regex::new(r"(?i)(?://|#)\s*HACK:?\s*(.+)")
                .map_err(|e| AuditError::other(format!("Invalid regex: {}", e)))?,
            // XXX (treat as high priority)
            Regex::new(r"(?i)(?://|#)\s*XXX:?\s*(.+)")
                .map_err(|e| AuditError::other(format!("Invalid regex: {}", e)))?,
            // NOTE (treat as low priority)
            Regex::new(r"(?i)(?://|#)\s*NOTE:?\s*(.+)")
                .map_err(|e| AuditError::other(format!("Invalid regex: {}", e)))?,
        ];

        Ok(Self { patterns })
    }

    /// Scan a file for TODO items
    pub fn scan_file(&self, path: &Path) -> Result<Vec<TodoItem>> {
        if !self.should_scan_file(path) {
            return Ok(Vec::new());
        }

        let content = fs::read_to_string(path)?;
        let mut todos = Vec::new();
        let category = Category::from_path(&path.to_string_lossy());

        for (line_num, line) in content.lines().enumerate() {
            for pattern in &self.patterns {
                if let Some(captures) = pattern.captures(line) {
                    if let Some(text_match) = captures.get(1) {
                        let text = text_match.as_str().trim().to_string();
                        let priority = self.infer_priority(line, &text);

                        let todo = TodoItem {
                            file: path.to_path_buf(),
                            line: line_num + 1,
                            text,
                            category,
                            context: self.extract_context(&content, line_num),
                            priority,
                        };

                        todos.push(todo);
                        break; // Only match one pattern per line
                    }
                }
            }
        }

        Ok(todos)
    }

    /// Scan a directory recursively for TODO items
    pub fn scan_directory(&self, dir: &Path) -> Result<Vec<TodoItem>> {
        let mut all_todos = Vec::new();

        for entry in WalkDir::new(dir)
            .follow_links(false)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            let path = entry.path();

            if !self.is_source_file(path) || self.should_skip(path) {
                continue;
            }

            if let Ok(todos) = self.scan_file(path) {
                all_todos.extend(todos);
            }
        }

        Ok(all_todos)
    }

    /// Group TODOs by file
    pub fn group_by_file<'a>(&self, todos: &'a [TodoItem]) -> HashMap<PathBuf, Vec<&'a TodoItem>> {
        let mut grouped = HashMap::new();
        for todo in todos {
            grouped
                .entry(todo.file.clone())
                .or_insert_with(Vec::new)
                .push(todo);
        }
        grouped
    }

    /// Group TODOs by category
    pub fn group_by_category<'a>(
        &self,
        todos: &'a [TodoItem],
    ) -> HashMap<Category, Vec<&'a TodoItem>> {
        let mut grouped = HashMap::new();
        for todo in todos {
            grouped
                .entry(todo.category)
                .or_insert_with(Vec::new)
                .push(todo);
        }
        grouped
    }

    /// Group TODOs by priority
    pub fn group_by_priority<'a>(
        &self,
        todos: &'a [TodoItem],
    ) -> HashMap<TodoPriority, Vec<&'a TodoItem>> {
        let mut grouped = HashMap::new();
        for todo in todos {
            grouped
                .entry(todo.priority)
                .or_insert_with(Vec::new)
                .push(todo);
        }
        grouped
    }

    /// Infer priority from comment content
    fn infer_priority(&self, line: &str, text: &str) -> TodoPriority {
        let lower_line = line.to_lowercase();
        let lower_text = text.to_lowercase();

        // High priority indicators
        if lower_line.contains("fixme")
            || lower_line.contains("xxx")
            || lower_line.contains("urgent")
            || lower_line.contains("critical")
            || lower_text.contains("bug")
            || lower_text.contains("security")
            || lower_text.contains("urgent")
            || lower_text.contains("critical")
            || lower_text.contains("important")
            || lower_text.contains("asap")
        {
            return TodoPriority::High;
        }

        // Low priority indicators
        if lower_line.contains("note")
            || lower_text.contains("maybe")
            || lower_text.contains("consider")
            || lower_text.contains("nice to have")
            || lower_text.contains("optional")
            || lower_text.contains("future")
        {
            return TodoPriority::Low;
        }

        // Default to medium
        TodoPriority::Medium
    }

    /// Extract context around a line
    fn extract_context(&self, content: &str, line_num: usize) -> Option<String> {
        let lines: Vec<&str> = content.lines().collect();
        let start = line_num.saturating_sub(2);
        let end = (line_num + 3).min(lines.len());

        if start < lines.len() {
            let context = lines[start..end].join("\n");
            Some(context)
        } else {
            None
        }
    }

    /// Check if a file should be scanned
    fn should_scan_file(&self, path: &Path) -> bool {
        self.is_source_file(path) && !self.should_skip(path)
    }

    /// Check if a file is a source file
    fn is_source_file(&self, path: &Path) -> bool {
        if !path.is_file() {
            return false;
        }

        let extension = path.extension().and_then(|e| e.to_str());
        matches!(
            extension,
            Some("rs")
                | Some("py")
                | Some("kt")
                | Some("kts")
                | Some("swift")
                | Some("ts")
                | Some("tsx")
                | Some("js")
                | Some("jsx")
                | Some("go")
                | Some("java")
                | Some("c")
                | Some("cpp")
                | Some("h")
                | Some("hpp")
        )
    }

    /// Check if a path should be skipped
    fn should_skip(&self, path: &Path) -> bool {
        let path_str = path.to_string_lossy();
        path_str.contains("target/")
            || path_str.contains("node_modules/")
            || path_str.contains(".git/")
            || path_str.contains("__pycache__")
            || path_str.contains(".pytest_cache")
            || path_str.contains("build/")
            || path_str.contains("dist/")
            || path_str.contains("vendor/")
            || path_str.contains(".cargo/")
    }

    /// Generate a summary report
    pub fn generate_summary(&self, todos: &[TodoItem]) -> TodoSummary {
        let total = todos.len();
        let by_priority = self.group_by_priority(todos);
        let by_category = self.group_by_category(todos);
        let by_file = self.group_by_file(todos);

        TodoSummary {
            total,
            high_priority: by_priority.get(&TodoPriority::High).map_or(0, |v| v.len()),
            medium_priority: by_priority
                .get(&TodoPriority::Medium)
                .map_or(0, |v| v.len()),
            low_priority: by_priority.get(&TodoPriority::Low).map_or(0, |v| v.len()),
            by_category: by_category.into_iter().map(|(k, v)| (k, v.len())).collect(),
            files_with_todos: by_file.len(),
        }
    }
}

/// Summary of TODO scan results
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TodoSummary {
    pub total: usize,
    pub high_priority: usize,
    pub medium_priority: usize,
    pub low_priority: usize,
    pub by_category: HashMap<Category, usize>,
    pub files_with_todos: usize,
}

impl Default for TodoScanner {
    fn default() -> Self {
        Self::new().expect("Failed to create default TodoScanner")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_scan_rust_file() {
        // Create temp file with .rs extension so it's recognized as a source file
        let temp_dir = tempfile::tempdir().unwrap();
        let file_path = temp_dir.path().join("test_file.rs");
        std::fs::write(
            &file_path,
            r#"
// TODO: Implement error handling
fn foo() {
    // FIXME: This is broken
    println!("Hello");
}

// NOTE: This is just a note
const MAGIC: u32 = 42;
"#,
        )
        .unwrap();

        let scanner = TodoScanner::new().unwrap();
        let todos = scanner.scan_file(&file_path).unwrap();

        assert_eq!(todos.len(), 3);
        assert!(todos[0].text.contains("Implement error handling"));
        assert_eq!(todos[0].priority, TodoPriority::Medium);
        assert!(todos[1].text.contains("This is broken"));
        assert_eq!(todos[1].priority, TodoPriority::High);
        assert_eq!(todos[2].priority, TodoPriority::Low);
    }

    #[test]
    fn test_priority_inference() {
        let scanner = TodoScanner::new().unwrap();

        // High priority
        assert_eq!(
            scanner.infer_priority("// FIXME: urgent", "urgent"),
            TodoPriority::High
        );
        assert_eq!(
            scanner.infer_priority("// TODO: security issue", "security issue"),
            TodoPriority::High
        );

        // Low priority
        assert_eq!(
            scanner.infer_priority("// NOTE: maybe do this", "maybe do this"),
            TodoPriority::Low
        );

        // Medium priority (default)
        assert_eq!(
            scanner.infer_priority("// TODO: refactor", "refactor"),
            TodoPriority::Medium
        );
    }

    #[test]
    fn test_group_by_priority() {
        let scanner = TodoScanner::new().unwrap();
        let todos = vec![
            TodoItem {
                file: PathBuf::from("test.rs"),
                line: 1,
                text: "Fix this".to_string(),
                category: Category::from_path("test.rs"),
                context: None,
                priority: TodoPriority::High,
            },
            TodoItem {
                file: PathBuf::from("test2.rs"),
                line: 2,
                text: "Refactor that".to_string(),
                category: Category::from_path("test2.rs"),
                context: None,
                priority: TodoPriority::Medium,
            },
            TodoItem {
                file: PathBuf::from("test3.rs"),
                line: 3,
                text: "Maybe improve".to_string(),
                category: Category::from_path("test3.rs"),
                context: None,
                priority: TodoPriority::Low,
            },
        ];

        let grouped = scanner.group_by_priority(&todos);
        assert_eq!(grouped.get(&TodoPriority::High).unwrap().len(), 1);
        assert_eq!(grouped.get(&TodoPriority::Medium).unwrap().len(), 1);
        assert_eq!(grouped.get(&TodoPriority::Low).unwrap().len(), 1);
    }
}
