//! Tree State Tracking Module
//!
//! Tracks file system state changes between audit runs:
//! - New files added
//! - Modified files (content hash changed)
//! - Deleted files
//! - Audit tag changes
//! - TODO/FIXME changes
//!
//! Integrates with `.audit-cache` for persistence and CI/CD workflows.

use crate::cache::CACHE_DIR;
use crate::error::{AuditError, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use tracing::info;

/// Tree state file name
pub const TREE_STATE_FILE: &str = "tree_state.json";

/// Audit tags index file name
pub const AUDIT_TAGS_INDEX_FILE: &str = "audit_tags_index.json";

/// TODOs index file name
pub const TODOS_INDEX_FILE: &str = "todos_index.json";

/// File state snapshot
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FileState {
    /// Relative path from project root
    pub path: String,

    /// SHA-256 hash of file content
    pub content_hash: String,

    /// File size in bytes
    pub size: usize,

    /// Lines of code
    pub lines: usize,

    /// Last modified timestamp (Unix epoch seconds)
    pub last_modified: i64,

    /// Number of audit tags in this file
    pub audit_tag_count: usize,

    /// Number of TODOs in this file
    pub todo_count: usize,

    /// Category (audit, clients, execution, janus)
    pub category: FileCategory,

    /// File importance score (from scoring module)
    pub importance_score: Option<f64>,

    /// LLM analysis hash (if analyzed)
    pub llm_analysis_hash: Option<String>,
}

/// File category for organization
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum FileCategory {
    /// Audit service code
    Audit,
    /// KMP client code
    Clients,
    /// Rust execution app
    Execution,
    /// Janus trading/ML system
    Janus,
    /// Configuration files
    Config,
    /// Documentation
    Docs,
    /// Test files
    Tests,
    /// Unknown/Other
    Other,
}

impl FileCategory {
    /// Detect category from file path
    pub fn from_path(path: &Path) -> Self {
        let path_str = path.to_string_lossy().to_lowercase();

        if path_str.contains("/audit/") || path_str.contains("\\audit\\") {
            FileCategory::Audit
        } else if path_str.contains("/clients/") || path_str.contains("\\clients\\") {
            FileCategory::Clients
        } else if path_str.contains("/execution/") || path_str.contains("\\execution\\") {
            FileCategory::Execution
        } else if path_str.contains("/janus/") || path_str.contains("\\janus\\") {
            FileCategory::Janus
        } else if path_str.contains("/config/")
            || path_str.ends_with(".toml")
            || path_str.ends_with(".yaml")
            || path_str.ends_with(".yml")
        {
            FileCategory::Config
        } else if path_str.contains("/docs/")
            || path_str.ends_with(".md")
            || path_str.ends_with(".txt")
        {
            FileCategory::Docs
        } else if path_str.contains("/tests/")
            || path_str.starts_with("tests/")
            || path_str.contains("_test.")
            || path_str.contains(".test.")
            || path_str.contains("/test/")
            || path_str.starts_with("test/")
        {
            FileCategory::Tests
        } else {
            FileCategory::Other
        }
    }

    /// Get display name
    pub fn display_name(&self) -> &'static str {
        match self {
            FileCategory::Audit => "Audit",
            FileCategory::Clients => "Clients (KMP)",
            FileCategory::Execution => "Execution",
            FileCategory::Janus => "Janus",
            FileCategory::Config => "Config",
            FileCategory::Docs => "Docs",
            FileCategory::Tests => "Tests",
            FileCategory::Other => "Other",
        }
    }
}

/// Change type for a file
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum ChangeType {
    /// Newly added file
    Added,
    /// Modified file (content changed)
    Modified {
        /// Previous content hash
        previous_hash: String,
        /// Lines added
        lines_added: i32,
        /// Lines removed
        lines_removed: i32,
    },
    /// Deleted file
    Deleted,
    /// Unchanged
    Unchanged,
}

/// File change record
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileChange {
    /// File path
    pub path: String,

    /// Change type
    pub change_type: ChangeType,

    /// Category
    pub category: FileCategory,

    /// Current state (None if deleted)
    pub current_state: Option<FileState>,

    /// Previous state (None if new)
    pub previous_state: Option<FileState>,

    /// Tag changes
    pub tag_changes: TagChanges,

    /// TODO changes
    pub todo_changes: TodoChanges,

    /// Needs LLM re-analysis
    pub needs_llm_analysis: bool,
}

/// Tag changes summary
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TagChanges {
    pub added: Vec<String>,
    pub removed: Vec<String>,
    pub modified: Vec<String>,
}

/// TODO changes summary
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TodoChanges {
    pub added: i32,
    pub removed: i32,
    pub net_change: i32,
}

/// Tree state snapshot
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TreeState {
    /// Snapshot timestamp
    pub timestamp: String,

    /// Git commit hash (if available)
    pub commit_hash: Option<String>,

    /// Git branch name
    pub branch: Option<String>,

    /// CI/CD run ID (if in CI)
    pub ci_run_id: Option<String>,

    /// All file states
    pub files: HashMap<String, FileState>,

    /// Summary statistics
    pub summary: TreeSummaryStats,
}

/// Summary statistics for tree state
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TreeSummaryStats {
    /// Total files
    pub total_files: usize,

    /// Total lines of code
    pub total_lines: usize,

    /// Files by category
    pub files_by_category: HashMap<String, usize>,

    /// Lines by category
    pub lines_by_category: HashMap<String, usize>,

    /// Total audit tags
    pub total_audit_tags: usize,

    /// Total TODOs
    pub total_todos: usize,

    /// Files needing LLM analysis
    pub files_pending_llm: usize,
}

/// Diff between two tree states
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TreeDiff {
    /// Timestamp of comparison
    pub compared_at: String,

    /// Previous state timestamp
    pub previous_timestamp: Option<String>,

    /// Current state timestamp
    pub current_timestamp: String,

    /// Git commit range (if available)
    pub commit_range: Option<String>,

    /// All file changes
    pub changes: Vec<FileChange>,

    /// Summary of changes
    pub summary: DiffSummary,
}

/// Summary of changes between states
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DiffSummary {
    /// Files added
    pub files_added: usize,

    /// Files modified
    pub files_modified: usize,

    /// Files deleted
    pub files_deleted: usize,

    /// Files unchanged
    pub files_unchanged: usize,

    /// Lines added (net)
    pub lines_added: i32,

    /// Lines removed (net)
    pub lines_removed: i32,

    /// Tags added
    pub tags_added: usize,

    /// Tags removed
    pub tags_removed: usize,

    /// TODOs added (net)
    pub todos_added: i32,

    /// TODOs removed (net)
    pub todos_removed: i32,

    /// Files needing LLM re-analysis
    pub files_needing_analysis: usize,

    /// Changes by category
    pub changes_by_category: HashMap<String, CategoryChangeSummary>,
}

/// Change summary per category
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CategoryChangeSummary {
    pub added: usize,
    pub modified: usize,
    pub deleted: usize,
    pub lines_changed: i32,
}

/// Tree state manager
pub struct TreeStateManager {
    /// Project root
    root: PathBuf,

    /// Cache directory
    cache_dir: PathBuf,

    /// Exclude patterns
    exclude_patterns: Vec<String>,

    /// Include patterns (file extensions)
    include_extensions: Vec<String>,
}

impl TreeStateManager {
    /// Create a new tree state manager
    pub fn new(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        let cache_dir = root.join(CACHE_DIR);

        Self {
            root,
            cache_dir,
            exclude_patterns: vec![
                "target".to_string(),
                "node_modules".to_string(),
                ".git".to_string(),
                "__pycache__".to_string(),
                ".pytest_cache".to_string(),
                "build".to_string(),
                "dist".to_string(),
                ".audit-cache".to_string(),
                ".idea".to_string(),
                ".vscode".to_string(),
            ],
            include_extensions: vec![
                "rs".to_string(),
                "kt".to_string(),
                "kts".to_string(),
                "py".to_string(),
                "ts".to_string(),
                "tsx".to_string(),
                "js".to_string(),
                "jsx".to_string(),
                "swift".to_string(),
                "toml".to_string(),
                "yaml".to_string(),
                "yml".to_string(),
                "md".to_string(),
            ],
        }
    }

    /// Ensure cache directory exists
    fn ensure_cache_dir(&self) -> Result<()> {
        if !self.cache_dir.exists() {
            fs::create_dir_all(&self.cache_dir)
                .map_err(|e| AuditError::other(format!("Failed to create cache dir: {}", e)))?;
        }
        Ok(())
    }

    /// Build current tree state
    pub fn build_current_state(&self) -> Result<TreeState> {
        info!("Building current tree state from: {}", self.root.display());

        let mut files = HashMap::new();
        let mut summary = TreeSummaryStats::default();

        self.scan_directory(&self.root, &mut files, &mut summary)?;

        // Get git info
        let (commit_hash, branch) = self.get_git_info();

        // Get CI info
        let ci_run_id = std::env::var("GITHUB_RUN_ID")
            .or_else(|_| std::env::var("CI_JOB_ID"))
            .or_else(|_| std::env::var("BUILD_ID"))
            .ok();

        Ok(TreeState {
            timestamp: chrono::Utc::now().to_rfc3339(),
            commit_hash,
            branch,
            ci_run_id,
            files,
            summary,
        })
    }

    /// Scan directory recursively
    fn scan_directory(
        &self,
        dir: &Path,
        files: &mut HashMap<String, FileState>,
        summary: &mut TreeSummaryStats,
    ) -> Result<()> {
        if !dir.is_dir() {
            return Ok(());
        }

        let entries = fs::read_dir(dir)
            .map_err(|e| AuditError::other(format!("Failed to read dir: {}", e)))?;

        for entry in entries.flatten() {
            let path = entry.path();

            // Skip excluded directories
            if self.should_exclude(&path) {
                continue;
            }

            if path.is_dir() {
                self.scan_directory(&path, files, summary)?;
            } else if self.should_include(&path) {
                if let Ok(state) = self.build_file_state(&path) {
                    let rel_path = path
                        .strip_prefix(&self.root)
                        .unwrap_or(&path)
                        .to_string_lossy()
                        .to_string();

                    // Update summary
                    summary.total_files += 1;
                    summary.total_lines += state.lines;
                    summary.total_audit_tags += state.audit_tag_count;
                    summary.total_todos += state.todo_count;

                    let category_name = format!("{:?}", state.category);
                    *summary
                        .files_by_category
                        .entry(category_name.clone())
                        .or_insert(0) += 1;
                    *summary.lines_by_category.entry(category_name).or_insert(0) += state.lines;

                    if state.llm_analysis_hash.is_none() {
                        summary.files_pending_llm += 1;
                    }

                    files.insert(rel_path, state);
                }
            }
        }

        Ok(())
    }

    /// Build state for a single file
    fn build_file_state(&self, path: &Path) -> Result<FileState> {
        let content = fs::read_to_string(path)
            .map_err(|e| AuditError::other(format!("Failed to read file: {}", e)))?;

        let content_hash = Self::hash_content(&content);
        let lines = content.lines().count();
        let size = content.len();

        // Count audit tags and TODOs
        let (audit_tag_count, todo_count) = self.count_tags_and_todos(&content);

        // Get modification time
        let last_modified = fs::metadata(path)
            .and_then(|m| m.modified())
            .map(|t| {
                t.duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs() as i64)
                    .unwrap_or(0)
            })
            .unwrap_or(0);

        let rel_path = path
            .strip_prefix(&self.root)
            .unwrap_or(path)
            .to_string_lossy()
            .to_string();

        Ok(FileState {
            path: rel_path,
            content_hash,
            size,
            lines,
            last_modified,
            audit_tag_count,
            todo_count,
            category: FileCategory::from_path(path),
            importance_score: None,
            llm_analysis_hash: None,
        })
    }

    /// Count audit tags and TODOs in content
    fn count_tags_and_todos(&self, content: &str) -> (usize, usize) {
        let mut audit_tags = 0;
        let mut todos = 0;

        for line in content.lines() {
            let line_lower = line.to_lowercase();

            // Count audit tags
            if line.contains("@audit-")
                || line.contains("@audit_")
                || line.contains("// audit:")
                || line.contains("# audit:")
            {
                audit_tags += 1;
            }

            // Count TODOs
            if line_lower.contains("todo")
                || line_lower.contains("fixme")
                || line_lower.contains("xxx")
                || line_lower.contains("hack")
            {
                todos += 1;
            }
        }

        (audit_tags, todos)
    }

    /// Hash content with SHA-256
    fn hash_content(content: &str) -> String {
        let mut hasher = Sha256::new();
        hasher.update(content.as_bytes());
        format!("{:x}", hasher.finalize())
    }

    /// Check if path should be excluded
    fn should_exclude(&self, path: &Path) -> bool {
        let path_str = path.to_string_lossy();
        self.exclude_patterns
            .iter()
            .any(|pattern| path_str.contains(pattern))
    }

    /// Check if file should be included
    fn should_include(&self, path: &Path) -> bool {
        if !path.is_file() {
            return false;
        }

        path.extension()
            .and_then(|e| e.to_str())
            .map(|ext| self.include_extensions.contains(&ext.to_string()))
            .unwrap_or(false)
    }

    /// Get git information
    fn get_git_info(&self) -> (Option<String>, Option<String>) {
        let commit_hash = std::process::Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(&self.root)
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());

        let branch = std::process::Command::new("git")
            .args(["rev-parse", "--abbrev-ref", "HEAD"])
            .current_dir(&self.root)
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());

        (commit_hash, branch)
    }

    /// Load previous tree state from cache
    pub fn load_previous_state(&self) -> Result<Option<TreeState>> {
        let state_file = self.cache_dir.join(TREE_STATE_FILE);

        if !state_file.exists() {
            return Ok(None);
        }

        let content = fs::read_to_string(&state_file)
            .map_err(|e| AuditError::other(format!("Failed to read tree state: {}", e)))?;

        let state: TreeState = serde_json::from_str(&content)
            .map_err(|e| AuditError::other(format!("Failed to parse tree state: {}", e)))?;

        Ok(Some(state))
    }

    /// Save tree state to cache
    pub fn save_state(&self, state: &TreeState) -> Result<()> {
        self.ensure_cache_dir()?;

        let state_file = self.cache_dir.join(TREE_STATE_FILE);
        let content = serde_json::to_string_pretty(state)
            .map_err(|e| AuditError::other(format!("Failed to serialize tree state: {}", e)))?;

        fs::write(&state_file, content)
            .map_err(|e| AuditError::other(format!("Failed to write tree state: {}", e)))?;

        info!("Tree state saved to: {}", state_file.display());
        Ok(())
    }

    /// Compare current state with previous state
    pub fn diff(&self, previous: &TreeState, current: &TreeState) -> TreeDiff {
        let mut changes = Vec::new();
        let mut summary = DiffSummary::default();

        let prev_paths: HashSet<_> = previous.files.keys().cloned().collect();
        let curr_paths: HashSet<_> = current.files.keys().cloned().collect();

        // Find added files
        for path in curr_paths.difference(&prev_paths) {
            if let Some(curr_state) = current.files.get(path) {
                let change = FileChange {
                    path: path.clone(),
                    change_type: ChangeType::Added,
                    category: curr_state.category,
                    current_state: Some(curr_state.clone()),
                    previous_state: None,
                    tag_changes: TagChanges {
                        added: vec![format!("{} tags", curr_state.audit_tag_count)],
                        ..Default::default()
                    },
                    todo_changes: TodoChanges {
                        added: curr_state.todo_count as i32,
                        net_change: curr_state.todo_count as i32,
                        ..Default::default()
                    },
                    needs_llm_analysis: true,
                };

                summary.files_added += 1;
                summary.lines_added += curr_state.lines as i32;
                summary.tags_added += curr_state.audit_tag_count;
                summary.todos_added += curr_state.todo_count as i32;
                summary.files_needing_analysis += 1;

                Self::update_category_summary(
                    &mut summary,
                    curr_state.category,
                    1,
                    0,
                    0,
                    curr_state.lines as i32,
                );

                changes.push(change);
            }
        }

        // Find deleted files
        for path in prev_paths.difference(&curr_paths) {
            if let Some(prev_state) = previous.files.get(path) {
                let change = FileChange {
                    path: path.clone(),
                    change_type: ChangeType::Deleted,
                    category: prev_state.category,
                    current_state: None,
                    previous_state: Some(prev_state.clone()),
                    tag_changes: TagChanges {
                        removed: vec![format!("{} tags", prev_state.audit_tag_count)],
                        ..Default::default()
                    },
                    todo_changes: TodoChanges {
                        removed: prev_state.todo_count as i32,
                        net_change: -(prev_state.todo_count as i32),
                        ..Default::default()
                    },
                    needs_llm_analysis: false,
                };

                summary.files_deleted += 1;
                summary.lines_removed += prev_state.lines as i32;
                summary.tags_removed += prev_state.audit_tag_count;
                summary.todos_removed += prev_state.todo_count as i32;

                Self::update_category_summary(
                    &mut summary,
                    prev_state.category,
                    0,
                    0,
                    1,
                    -(prev_state.lines as i32),
                );

                changes.push(change);
            }
        }

        // Find modified and unchanged files
        for path in prev_paths.intersection(&curr_paths) {
            let prev_state = previous.files.get(path).unwrap();
            let curr_state = current.files.get(path).unwrap();

            if prev_state.content_hash != curr_state.content_hash {
                let lines_diff = curr_state.lines as i32 - prev_state.lines as i32;
                let tag_diff =
                    curr_state.audit_tag_count as i32 - prev_state.audit_tag_count as i32;
                let todo_diff = curr_state.todo_count as i32 - prev_state.todo_count as i32;

                let change = FileChange {
                    path: path.clone(),
                    change_type: ChangeType::Modified {
                        previous_hash: prev_state.content_hash.clone(),
                        lines_added: lines_diff.max(0),
                        lines_removed: (-lines_diff).max(0),
                    },
                    category: curr_state.category,
                    current_state: Some(curr_state.clone()),
                    previous_state: Some(prev_state.clone()),
                    tag_changes: TagChanges {
                        added: if tag_diff > 0 {
                            vec![format!("+{} tags", tag_diff)]
                        } else {
                            vec![]
                        },
                        removed: if tag_diff < 0 {
                            vec![format!("{} tags", tag_diff)]
                        } else {
                            vec![]
                        },
                        ..Default::default()
                    },
                    todo_changes: TodoChanges {
                        added: todo_diff.max(0),
                        removed: (-todo_diff).max(0),
                        net_change: todo_diff,
                    },
                    needs_llm_analysis: true,
                };

                summary.files_modified += 1;
                if lines_diff > 0 {
                    summary.lines_added += lines_diff;
                } else {
                    summary.lines_removed += -lines_diff;
                }
                if tag_diff > 0 {
                    summary.tags_added += tag_diff as usize;
                } else {
                    summary.tags_removed += (-tag_diff) as usize;
                }
                summary.todos_added += todo_diff;
                summary.files_needing_analysis += 1;

                Self::update_category_summary(
                    &mut summary,
                    curr_state.category,
                    0,
                    1,
                    0,
                    lines_diff,
                );

                changes.push(change);
            } else {
                summary.files_unchanged += 1;
            }
        }

        // Build commit range if available
        let commit_range = match (&previous.commit_hash, &current.commit_hash) {
            (Some(prev), Some(curr)) => Some(format!("{}..{}", &prev[..7], &curr[..7])),
            _ => None,
        };

        TreeDiff {
            compared_at: chrono::Utc::now().to_rfc3339(),
            previous_timestamp: Some(previous.timestamp.clone()),
            current_timestamp: current.timestamp.clone(),
            commit_range,
            changes,
            summary,
        }
    }

    /// Update category summary helper
    fn update_category_summary(
        summary: &mut DiffSummary,
        category: FileCategory,
        added: usize,
        modified: usize,
        deleted: usize,
        lines_changed: i32,
    ) {
        let cat_name = category.display_name().to_string();
        let entry = summary.changes_by_category.entry(cat_name).or_default();
        entry.added += added;
        entry.modified += modified;
        entry.deleted += deleted;
        entry.lines_changed += lines_changed;
    }

    /// Get files that need LLM analysis (new or modified)
    pub fn get_files_needing_analysis(&self, diff: &TreeDiff) -> Vec<FileState> {
        diff.changes
            .iter()
            .filter(|c| c.needs_llm_analysis)
            .filter_map(|c| c.current_state.clone())
            .collect()
    }

    /// Get files by category
    pub fn get_files_by_category<'a>(
        &self,
        state: &'a TreeState,
        category: FileCategory,
    ) -> Vec<&'a FileState> {
        state
            .files
            .values()
            .filter(|f| f.category == category)
            .collect()
    }

    /// Update file with LLM analysis result
    pub fn mark_file_analyzed(&self, state: &mut TreeState, path: &str, analysis_hash: String) {
        if let Some(file) = state.files.get_mut(path) {
            file.llm_analysis_hash = Some(analysis_hash);
        }
    }

    /// Generate CI/CD summary report
    pub fn generate_ci_summary(&self, diff: &TreeDiff) -> String {
        let mut report = String::new();

        report.push_str("## 📊 Audit Tree State Changes\n\n");

        // Commit range
        if let Some(ref range) = diff.commit_range {
            report.push_str(&format!("**Commits:** `{}`\n\n", range));
        }

        // Summary table
        report.push_str("### Summary\n\n");
        report.push_str("| Metric | Count |\n");
        report.push_str("|--------|-------|\n");
        report.push_str(&format!("| Files Added | {} |\n", diff.summary.files_added));
        report.push_str(&format!(
            "| Files Modified | {} |\n",
            diff.summary.files_modified
        ));
        report.push_str(&format!(
            "| Files Deleted | {} |\n",
            diff.summary.files_deleted
        ));
        report.push_str(&format!(
            "| Files Unchanged | {} |\n",
            diff.summary.files_unchanged
        ));
        report.push_str(&format!(
            "| Lines Changed | +{} / -{} |\n",
            diff.summary.lines_added, diff.summary.lines_removed
        ));
        report.push_str(&format!(
            "| TODOs Changed | +{} / -{} |\n",
            diff.summary.todos_added, diff.summary.todos_removed
        ));
        report.push_str(&format!(
            "| Files Needing LLM Analysis | {} |\n",
            diff.summary.files_needing_analysis
        ));
        report.push('\n');

        // Changes by category
        if !diff.summary.changes_by_category.is_empty() {
            report.push_str("### Changes by Category\n\n");
            report.push_str("| Category | Added | Modified | Deleted | Lines |\n");
            report.push_str("|----------|-------|----------|---------|-------|\n");

            for (cat, changes) in &diff.summary.changes_by_category {
                let lines_str = if changes.lines_changed >= 0 {
                    format!("+{}", changes.lines_changed)
                } else {
                    format!("{}", changes.lines_changed)
                };
                report.push_str(&format!(
                    "| {} | {} | {} | {} | {} |\n",
                    cat, changes.added, changes.modified, changes.deleted, lines_str
                ));
            }
            report.push('\n');
        }

        // Changed files list
        if !diff.changes.is_empty() {
            report.push_str("### Changed Files\n\n");

            let added: Vec<_> = diff
                .changes
                .iter()
                .filter(|c| matches!(c.change_type, ChangeType::Added))
                .collect();
            if !added.is_empty() {
                report.push_str("**Added:**\n");
                for change in added.iter().take(20) {
                    report.push_str(&format!("- `{}`\n", change.path));
                }
                if added.len() > 20 {
                    report.push_str(&format!("- ... and {} more\n", added.len() - 20));
                }
                report.push('\n');
            }

            let modified: Vec<_> = diff
                .changes
                .iter()
                .filter(|c| matches!(c.change_type, ChangeType::Modified { .. }))
                .collect();
            if !modified.is_empty() {
                report.push_str("**Modified:**\n");
                for change in modified.iter().take(20) {
                    if let ChangeType::Modified {
                        lines_added,
                        lines_removed,
                        ..
                    } = &change.change_type
                    {
                        report.push_str(&format!(
                            "- `{}` (+{} / -{})\n",
                            change.path, lines_added, lines_removed
                        ));
                    }
                }
                if modified.len() > 20 {
                    report.push_str(&format!("- ... and {} more\n", modified.len() - 20));
                }
                report.push('\n');
            }

            let deleted: Vec<_> = diff
                .changes
                .iter()
                .filter(|c| matches!(c.change_type, ChangeType::Deleted))
                .collect();
            if !deleted.is_empty() {
                report.push_str("**Deleted:**\n");
                for change in deleted.iter().take(10) {
                    report.push_str(&format!("- `{}`\n", change.path));
                }
                if deleted.len() > 10 {
                    report.push_str(&format!("- ... and {} more\n", deleted.len() - 10));
                }
                report.push('\n');
            }
        }

        report
    }

    /// Print summary to console
    pub fn print_summary(&self, state: &TreeState) {
        println!("\n📁 Tree State Summary");
        println!("  Timestamp: {}", state.timestamp);
        if let Some(ref commit) = state.commit_hash {
            println!("  Commit: {}", &commit[..7.min(commit.len())]);
        }
        if let Some(ref branch) = state.branch {
            println!("  Branch: {}", branch);
        }
        println!("  Total Files: {}", state.summary.total_files);
        println!("  Total Lines: {}", state.summary.total_lines);
        println!("  Audit Tags: {}", state.summary.total_audit_tags);
        println!("  TODOs: {}", state.summary.total_todos);
        println!(
            "  Pending LLM Analysis: {}",
            state.summary.files_pending_llm
        );

        println!("\n  By Category:");
        for (cat, count) in &state.summary.files_by_category {
            let lines = state.summary.lines_by_category.get(cat).unwrap_or(&0);
            println!("    {}: {} files, {} lines", cat, count, lines);
        }
    }

    /// Print diff summary to console
    pub fn print_diff(&self, diff: &TreeDiff) {
        println!("\n📊 Tree State Diff");
        println!("  Compared at: {}", diff.compared_at);
        if let Some(ref range) = diff.commit_range {
            println!("  Commits: {}", range);
        }
        println!("  Added: {} files", diff.summary.files_added);
        println!("  Modified: {} files", diff.summary.files_modified);
        println!("  Deleted: {} files", diff.summary.files_deleted);
        println!("  Unchanged: {} files", diff.summary.files_unchanged);
        println!(
            "  Lines: +{} / -{}",
            diff.summary.lines_added, diff.summary.lines_removed
        );
        println!(
            "  TODOs: +{} / -{}",
            diff.summary.todos_added, diff.summary.todos_removed
        );
        println!(
            "  Files needing analysis: {}",
            diff.summary.files_needing_analysis
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    #[test]
    fn test_file_category_detection() {
        assert_eq!(
            FileCategory::from_path(Path::new("src/audit/main.rs")),
            FileCategory::Audit
        );
        assert_eq!(
            FileCategory::from_path(Path::new("src/janus/crates/vision/lib.rs")),
            FileCategory::Janus
        );
        // Path must contain "/clients/" (with slashes) to be detected as Clients
        assert_eq!(
            FileCategory::from_path(Path::new("src/clients/android/app.kt")),
            FileCategory::Clients
        );
        assert_eq!(
            FileCategory::from_path(Path::new("config.toml")),
            FileCategory::Config
        );
        // docs/ path -> Docs category
        assert_eq!(
            FileCategory::from_path(Path::new("docs/README.md")),
            FileCategory::Docs
        );
        assert_eq!(
            FileCategory::from_path(Path::new("tests/test_main.rs")),
            FileCategory::Tests
        );
    }

    #[test]
    fn test_hash_content() {
        let hash1 = TreeStateManager::hash_content("hello world");
        let hash2 = TreeStateManager::hash_content("hello world");
        let hash3 = TreeStateManager::hash_content("hello world!");

        assert_eq!(hash1, hash2);
        assert_ne!(hash1, hash3);
        assert_eq!(hash1.len(), 64); // SHA-256 hex = 64 chars
    }

    #[test]
    fn test_build_current_state() {
        let temp = TempDir::new().unwrap();
        let root = temp.path();

        // Create test files
        fs::create_dir(root.join("src")).unwrap();
        let mut file = fs::File::create(root.join("src/main.rs")).unwrap();
        writeln!(
            file,
            "// TODO: implement\nfn main() {{}}\n// @audit-tag: test"
        )
        .unwrap();

        let manager = TreeStateManager::new(root);
        let state = manager.build_current_state().unwrap();

        assert_eq!(state.summary.total_files, 1);
        assert!(state.summary.total_audit_tags >= 1);
        assert!(state.summary.total_todos >= 1);
    }

    #[test]
    fn test_diff_states() {
        let temp = TempDir::new().unwrap();
        let root = temp.path();

        // Create initial state
        fs::create_dir(root.join("src")).unwrap();
        fs::write(root.join("src/main.rs"), "fn main() {}").unwrap();

        let manager = TreeStateManager::new(root);
        let state1 = manager.build_current_state().unwrap();

        // Modify file
        fs::write(
            root.join("src/main.rs"),
            "fn main() { println!(\"hello\"); }",
        )
        .unwrap();
        // Add new file
        fs::write(root.join("src/lib.rs"), "pub fn lib() {}").unwrap();

        let state2 = manager.build_current_state().unwrap();

        let diff = manager.diff(&state1, &state2);

        assert_eq!(diff.summary.files_added, 1);
        assert_eq!(diff.summary.files_modified, 1);
        assert_eq!(diff.summary.files_deleted, 0);
    }

    #[test]
    fn test_ci_summary_generation() {
        let temp = TempDir::new().unwrap();
        let manager = TreeStateManager::new(temp.path());

        let state1 = TreeState {
            timestamp: "2024-01-01T00:00:00Z".to_string(),
            commit_hash: Some("abc1234567890".to_string()),
            branch: Some("main".to_string()),
            ci_run_id: None,
            files: HashMap::new(),
            summary: TreeSummaryStats::default(),
        };

        let mut files = HashMap::new();
        files.insert(
            "src/new.rs".to_string(),
            FileState {
                path: "src/new.rs".to_string(),
                content_hash: "abc123".to_string(),
                size: 100,
                lines: 10,
                last_modified: 0,
                audit_tag_count: 2,
                todo_count: 1,
                category: FileCategory::Audit,
                importance_score: None,
                llm_analysis_hash: None,
            },
        );

        let state2 = TreeState {
            timestamp: "2024-01-02T00:00:00Z".to_string(),
            commit_hash: Some("def4567890123".to_string()),
            branch: Some("main".to_string()),
            ci_run_id: Some("12345".to_string()),
            files,
            summary: TreeSummaryStats {
                total_files: 1,
                total_lines: 10,
                ..Default::default()
            },
        };

        let diff = manager.diff(&state1, &state2);
        let summary = manager.generate_ci_summary(&diff);

        assert!(summary.contains("Audit Tree State Changes"));
        assert!(summary.contains("Files Added"));
        assert!(summary.contains("`src/new.rs`"));
    }
}
