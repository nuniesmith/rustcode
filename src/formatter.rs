//! Code formatting and auto-fix utilities
//!
//! This module provides functionality to automatically format code across different
//! languages and tools, integrating with CI/CD pipelines for automated code quality.

use crate::error::AuditError;
use std::path::{Path, PathBuf};
use std::process::Command;
use tracing::{debug, info, warn};

/// Supported formatters
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Formatter {
    /// Rust: cargo fmt
    RustFmt,
    /// Kotlin: ktlint
    KtLint,
    /// TypeScript/JavaScript: prettier
    Prettier,
    /// Python: black
    Black,
}

impl Formatter {
    /// Get all available formatters
    pub fn all() -> Vec<Self> {
        vec![Self::RustFmt, Self::KtLint, Self::Prettier, Self::Black]
    }

    /// Get formatter name
    pub fn name(&self) -> &'static str {
        match self {
            Self::RustFmt => "cargo-fmt",
            Self::KtLint => "ktlint",
            Self::Prettier => "prettier",
            Self::Black => "black",
        }
    }

    /// Get file extensions this formatter handles
    pub fn extensions(&self) -> &[&str] {
        match self {
            Self::RustFmt => &["rs"],
            Self::KtLint => &["kt", "kts"],
            Self::Prettier => &["ts", "tsx", "js", "jsx", "json", "md", "yaml", "yml"],
            Self::Black => &["py"],
        }
    }

    /// Check if formatter is available on the system
    pub fn is_available(&self) -> bool {
        match self {
            Self::RustFmt => Command::new("cargo")
                .args(["fmt", "--version"])
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false),
            Self::KtLint => Command::new("ktlint")
                .arg("--version")
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false),
            Self::Prettier => Command::new("npx")
                .args(["prettier", "--version"])
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false),
            Self::Black => Command::new("black")
                .arg("--version")
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false),
        }
    }
}

/// Formatting operation mode
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormatMode {
    /// Check formatting without making changes
    Check,
    /// Apply formatting changes
    Fix,
}

/// Result of a formatting operation
#[derive(Debug, Clone)]
pub struct FormatResult {
    /// Formatter used
    pub formatter: Formatter,
    /// Files that were checked/formatted
    pub files_processed: usize,
    /// Files that needed formatting (in check mode) or were formatted (in fix mode)
    pub files_changed: usize,
    /// Whether formatting passed (check mode) or succeeded (fix mode)
    pub success: bool,
    /// Any errors encountered
    pub errors: Vec<String>,
    /// Warnings (e.g., formatter not available)
    pub warnings: Vec<String>,
}

impl FormatResult {
    /// Create a successful result
    pub fn success(formatter: Formatter, files_processed: usize, files_changed: usize) -> Self {
        Self {
            formatter,
            files_processed,
            files_changed,
            success: true,
            errors: Vec::new(),
            warnings: Vec::new(),
        }
    }

    /// Create a failed result
    pub fn failed(formatter: Formatter, error: String) -> Self {
        Self {
            formatter,
            files_processed: 0,
            files_changed: 0,
            success: false,
            errors: vec![error],
            warnings: Vec::new(),
        }
    }

    /// Create a skipped result (formatter not available)
    pub fn skipped(formatter: Formatter, reason: String) -> Self {
        Self {
            formatter,
            files_processed: 0,
            files_changed: 0,
            success: true,
            errors: Vec::new(),
            warnings: vec![reason],
        }
    }
}

/// Batch formatting results
#[derive(Debug, Clone)]
pub struct BatchFormatResult {
    /// Individual formatter results
    pub results: Vec<FormatResult>,
    /// Total files processed
    pub total_files: usize,
    /// Total files changed
    pub total_changed: usize,
    /// Overall success
    pub success: bool,
}

impl BatchFormatResult {
    /// Create from individual results
    pub fn from_results(results: Vec<FormatResult>) -> Self {
        let total_files = results.iter().map(|r| r.files_processed).sum();
        let total_changed = results.iter().map(|r| r.files_changed).sum();
        let success = results.iter().all(|r| r.success);

        Self {
            results,
            total_files,
            total_changed,
            success,
        }
    }

    /// Get summary string
    pub fn summary(&self) -> String {
        format!(
            "Processed {} files, {} needed formatting. Status: {}",
            self.total_files,
            self.total_changed,
            if self.success { "✓ PASS" } else { "✗ FAIL" }
        )
    }

    /// Get all errors
    pub fn all_errors(&self) -> Vec<String> {
        self.results
            .iter()
            .flat_map(|r| r.errors.iter().cloned())
            .collect()
    }

    /// Get all warnings
    pub fn all_warnings(&self) -> Vec<String> {
        self.results
            .iter()
            .flat_map(|r| r.warnings.iter().cloned())
            .collect()
    }
}

/// Main formatter orchestrator
pub struct CodeFormatter {
    /// Root directory to format
    root: PathBuf,
    /// Formatters to use (empty = all available)
    formatters: Vec<Formatter>,
    /// Formatting mode
    mode: FormatMode,
}

impl CodeFormatter {
    /// Create a new code formatter
    pub fn new(root: impl AsRef<Path>, mode: FormatMode) -> Self {
        Self {
            root: root.as_ref().to_path_buf(),
            formatters: Vec::new(),
            mode,
        }
    }

    /// Set specific formatters to use
    pub fn with_formatters(mut self, formatters: Vec<Formatter>) -> Self {
        self.formatters = formatters;
        self
    }

    /// Run formatting on all configured formatters
    pub fn run(&self) -> Result<BatchFormatResult, AuditError> {
        let formatters = if self.formatters.is_empty() {
            Formatter::all()
        } else {
            self.formatters.clone()
        };

        let mut results = Vec::new();

        for formatter in formatters {
            info!("Running {} in {:?} mode...", formatter.name(), self.mode);

            if !formatter.is_available() {
                warn!("{} is not available, skipping", formatter.name());
                results.push(FormatResult::skipped(
                    formatter,
                    format!("{} not installed", formatter.name()),
                ));
                continue;
            }

            let result = match formatter {
                Formatter::RustFmt => self.format_rust(),
                Formatter::KtLint => self.format_kotlin(),
                Formatter::Prettier => self.format_prettier(),
                Formatter::Black => self.format_python(),
            };

            match result {
                Ok(r) => results.push(r),
                Err(e) => {
                    results.push(FormatResult::failed(
                        formatter,
                        format!("Formatting failed: {}", e),
                    ));
                }
            }
        }

        Ok(BatchFormatResult::from_results(results))
    }

    /// Format Rust code using cargo fmt
    fn format_rust(&self) -> Result<FormatResult, AuditError> {
        debug!("Looking for Rust workspace in {:?}", self.root);

        // Find Cargo.toml in root or subdirectories
        let cargo_paths = self.find_cargo_workspaces()?;

        if cargo_paths.is_empty() {
            return Ok(FormatResult::skipped(
                Formatter::RustFmt,
                "No Cargo.toml found".to_string(),
            ));
        }

        let mut total_changed = 0;
        let mut errors = Vec::new();

        for cargo_dir in &cargo_paths {
            debug!("Running cargo fmt in {:?}", cargo_dir);

            let mut cmd = Command::new("cargo");
            cmd.current_dir(cargo_dir).arg("fmt").arg("--all");

            match self.mode {
                FormatMode::Check => {
                    cmd.arg("--check");
                }
                FormatMode::Fix => {
                    // cargo fmt defaults to fix mode
                }
            }

            let output = cmd
                .output()
                .map_err(|e| AuditError::Other(format!("Failed to run cargo fmt: {}", e)))?;

            if !output.status.success() {
                if self.mode == FormatMode::Check {
                    // In check mode, non-zero exit means files need formatting
                    total_changed += 1;
                } else {
                    // In fix mode, non-zero exit is an error
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    errors.push(format!("cargo fmt failed in {:?}: {}", cargo_dir, stderr));
                }
            }
        }

        if !errors.is_empty() {
            return Ok(FormatResult {
                formatter: Formatter::RustFmt,
                files_processed: cargo_paths.len(),
                files_changed: total_changed,
                success: false,
                errors,
                warnings: Vec::new(),
            });
        }

        Ok(FormatResult::success(
            Formatter::RustFmt,
            cargo_paths.len(),
            total_changed,
        ))
    }

    /// Format Kotlin code using ktlint
    fn format_kotlin(&self) -> Result<FormatResult, AuditError> {
        debug!("Looking for Kotlin files in {:?}", self.root);

        // Find Kotlin files
        let kt_files = self.find_files_by_extension(&["kt", "kts"])?;

        if kt_files.is_empty() {
            return Ok(FormatResult::skipped(
                Formatter::KtLint,
                "No Kotlin files found".to_string(),
            ));
        }

        let mut cmd = Command::new("ktlint");

        match self.mode {
            FormatMode::Check => {
                // ktlint default is check mode
            }
            FormatMode::Fix => {
                cmd.arg("-F");
            }
        }

        // Add all Kotlin files
        for file in &kt_files {
            cmd.arg(file);
        }

        let output = cmd
            .output()
            .map_err(|e| AuditError::Other(format!("Failed to run ktlint: {}", e)))?;

        let files_changed = if self.mode == FormatMode::Check {
            if output.status.success() {
                0
            } else {
                // Parse ktlint output to count files with issues
                String::from_utf8_lossy(&output.stdout)
                    .lines()
                    .filter(|line| line.contains(".kt:"))
                    .count()
            }
        } else {
            // In fix mode, we don't know how many were changed
            // ktlint doesn't provide this info easily
            if output.status.success() {
                0
            } else {
                kt_files.len()
            }
        };

        Ok(FormatResult::success(
            Formatter::KtLint,
            kt_files.len(),
            files_changed,
        ))
    }

    /// Format code using prettier
    fn format_prettier(&self) -> Result<FormatResult, AuditError> {
        debug!(
            "Looking for files that prettier can format in {:?}",
            self.root
        );

        let extensions = Formatter::Prettier.extensions();
        let files = self.find_files_by_extension(extensions)?;

        if files.is_empty() {
            return Ok(FormatResult::skipped(
                Formatter::Prettier,
                "No files found for prettier".to_string(),
            ));
        }

        let mut cmd = Command::new("npx");
        cmd.arg("prettier");

        match self.mode {
            FormatMode::Check => {
                cmd.arg("--check");
            }
            FormatMode::Fix => {
                cmd.arg("--write");
            }
        }

        // Add all files
        for file in &files {
            cmd.arg(file);
        }

        let output = cmd
            .output()
            .map_err(|e| AuditError::Other(format!("Failed to run prettier: {}", e)))?;

        let files_changed = if !output.status.success() {
            files.len()
        } else {
            0
        };

        Ok(FormatResult::success(
            Formatter::Prettier,
            files.len(),
            files_changed,
        ))
    }

    /// Format Python code using black
    fn format_python(&self) -> Result<FormatResult, AuditError> {
        debug!("Looking for Python files in {:?}", self.root);

        let py_files = self.find_files_by_extension(&["py"])?;

        if py_files.is_empty() {
            return Ok(FormatResult::skipped(
                Formatter::Black,
                "No Python files found".to_string(),
            ));
        }

        let mut cmd = Command::new("black");

        match self.mode {
            FormatMode::Check => {
                cmd.arg("--check");
            }
            FormatMode::Fix => {
                // black default is fix mode
            }
        }

        // Add all Python files
        for file in &py_files {
            cmd.arg(file);
        }

        let output = cmd
            .output()
            .map_err(|e| AuditError::Other(format!("Failed to run black: {}", e)))?;

        let files_changed = if !output.status.success() {
            py_files.len()
        } else {
            0
        };

        Ok(FormatResult::success(
            Formatter::Black,
            py_files.len(),
            files_changed,
        ))
    }

    /// Find root Cargo workspaces (not workspace members)
    ///
    /// This finds Cargo.toml files that are either:
    /// 1. Workspace roots (contain [workspace] section)
    /// 2. Standalone crates (not part of a parent workspace)
    fn find_cargo_workspaces(&self) -> Result<Vec<PathBuf>, AuditError> {
        let mut workspace_roots = Vec::new();
        let mut all_cargo_dirs = Vec::new();

        // First pass: find all Cargo.toml files and identify workspace roots
        for entry in walkdir::WalkDir::new(&self.root)
            .follow_links(false)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            // Skip common non-project directories
            let path = entry.path();
            if path.components().any(|c| {
                let s = c.as_os_str().to_string_lossy();
                s == "target" || s == "node_modules" || s == ".git"
            }) {
                continue;
            }

            if entry.file_name() == "Cargo.toml" {
                if let Some(parent) = path.parent() {
                    let parent_path = parent.to_path_buf();

                    // Check if this is a workspace root
                    if let Ok(content) = std::fs::read_to_string(path) {
                        if content.contains("[workspace]") {
                            workspace_roots.push(parent_path.clone());
                        }
                    }

                    all_cargo_dirs.push(parent_path);
                }
            }
        }

        // If we found workspace roots, only return those (they handle their members)
        if !workspace_roots.is_empty() {
            // Filter out any workspace root that is a child of another
            workspace_roots.sort_by_key(|p| p.components().count());
            let mut final_roots = Vec::new();
            for ws in workspace_roots {
                let is_child = final_roots
                    .iter()
                    .any(|parent: &PathBuf| ws.starts_with(parent) && ws != *parent);
                if !is_child {
                    final_roots.push(ws);
                }
            }
            return Ok(final_roots);
        }

        // No workspace roots found - return standalone crates
        // Filter out nested crates (child of another Cargo.toml directory)
        all_cargo_dirs.sort_by_key(|p| p.components().count());
        let mut final_dirs = Vec::new();
        for dir in all_cargo_dirs {
            let is_child = final_dirs
                .iter()
                .any(|parent: &PathBuf| dir.starts_with(parent) && dir != *parent);
            if !is_child {
                final_dirs.push(dir);
            }
        }

        Ok(final_dirs)
    }

    /// Find all files with given extensions
    fn find_files_by_extension(&self, extensions: &[&str]) -> Result<Vec<PathBuf>, AuditError> {
        let mut files = Vec::new();

        for entry in walkdir::WalkDir::new(&self.root)
            .follow_links(false)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            if entry.file_type().is_file() {
                if let Some(ext) = entry.path().extension() {
                    if extensions.iter().any(|&e| e == ext) {
                        files.push(entry.path().to_path_buf());
                    }
                }
            }
        }

        Ok(files)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_formatter_extensions() {
        assert!(Formatter::RustFmt.extensions().contains(&"rs"));
        assert!(Formatter::KtLint.extensions().contains(&"kt"));
        assert!(Formatter::Prettier.extensions().contains(&"ts"));
        assert!(Formatter::Black.extensions().contains(&"py"));
    }

    #[test]
    fn test_formatter_names() {
        assert_eq!(Formatter::RustFmt.name(), "cargo-fmt");
        assert_eq!(Formatter::KtLint.name(), "ktlint");
    }

    #[test]
    fn test_batch_result_summary() {
        let results = vec![
            FormatResult::success(Formatter::RustFmt, 10, 2),
            FormatResult::success(Formatter::KtLint, 5, 0),
        ];

        let batch = BatchFormatResult::from_results(results);
        assert_eq!(batch.total_files, 15);
        assert_eq!(batch.total_changed, 2);
        assert!(batch.success);
    }
}
