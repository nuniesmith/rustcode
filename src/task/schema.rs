// Task File Schema
//
// Defines the JSON schema for task files that are dropped into the `tasks/` directory.
// The async task agent picks these up and executes them: create files, run tests, commit, open PR.
//
// Example task file (tasks/my-task.json):
// ```json
// {
//   "id": "feat-module-registry",
//   "repo": "nuniesmith/fks-ruby",
//   "description": "Add ModuleRegistry base classes for plugin system",
//   "steps": [
//     "Create src/ruby/src/core/module_registry.py with FKSModule ABC",
//     "Add __fks_module__ sentinel to indicators/trend/exponential_moving_average.py",
//     "Add unit test in tests/test_module_registry.py"
//   ],
//   "branch": "feat/module-registry",
//   "labels": ["feature", "auto-pr"],
//   "auto_merge": true
// }
// ```

use serde::{Deserialize, Serialize};
use std::path::Path;

// ============================================================================
// Task File Schema
// ============================================================================

/// A task file that can be dropped into the `tasks/` directory.
///
/// The async task agent will:
/// 1. Clone the repository
/// 2. Create a new branch
/// 3. Execute each step (create files, edit files, run tests)
/// 4. Commit changes
/// 5. Open a GitHub PR
/// 6. Auto-merge if `auto_merge: true` and all checks pass
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskFile {
    /// Unique task identifier (alphanumeric, hyphens OK)
    pub id: String,

    /// GitHub repository in `owner/repo` format
    pub repo: String,

    /// Human-readable description of what this task accomplishes
    pub description: String,

    /// Ordered list of steps to execute
    /// Each step is a natural-language instruction:
    /// - "Create src/file.rs with ..."
    /// - "Edit src/lib.rs to add ..."
    /// - "Run `cargo test` to verify ..."
    pub steps: Vec<String>,

    /// Git branch name for the PR (e.g., "feat/my-feature")
    pub branch: String,

    /// Labels to apply to the PR (e.g., ["feature", "auto-pr"])
    #[serde(default)]
    pub labels: Vec<String>,

    /// If true, automatically merge the PR when all CI checks pass
    #[serde(default)]
    pub auto_merge: bool,
}

impl TaskFile {
    /// Validate the task file
    pub fn validate(&self) -> Result<(), Vec<String>> {
        let mut errors = Vec::new();

        if self.id.trim().is_empty() {
            errors.push("id cannot be empty".to_string());
        }

        if !self
            .id
            .chars()
            .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
        {
            errors.push("id must be alphanumeric (hyphens and underscores allowed)".to_string());
        }

        if self.repo.trim().is_empty() {
            errors.push("repo cannot be empty".to_string());
        }

        if !self.repo.contains('/') {
            errors.push("repo must be in format 'owner/repo'".to_string());
        }

        if self.description.trim().is_empty() {
            errors.push("description cannot be empty".to_string());
        }

        if self.steps.is_empty() {
            errors.push("steps must have at least one step".to_string());
        }

        if self.steps.iter().any(|s| s.trim().is_empty()) {
            errors.push("steps cannot contain empty strings".to_string());
        }

        if self.branch.trim().is_empty() {
            errors.push("branch cannot be empty".to_string());
        }

        if !errors.is_empty() {
            return Err(errors);
        }

        Ok(())
    }

    /// Load and validate a task file from a JSON file path
    pub fn from_file(path: &Path) -> anyhow::Result<Self> {
        let content = std::fs::read_to_string(path)?;
        let task: TaskFile = serde_json::from_str(&content)?;
        task.validate()
            .map_err(|errors| anyhow::anyhow!("Validation failed: {}", errors.join("; ")))?;
        Ok(task)
    }

    /// Load and validate a task file from JSON string
    pub fn from_json(json: &str) -> anyhow::Result<Self> {
        let task: TaskFile = serde_json::from_str(json)?;
        task.validate()
            .map_err(|errors| anyhow::anyhow!("Validation failed: {}", errors.join("; ")))?;
        Ok(task)
    }
}

// ============================================================================
// Task Result
// ============================================================================

/// Result of executing a task file (saved to `tasks/results/{id}.json`)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskResult {
    /// The task that was executed
    pub task_id: String,

    /// Status: "success", "failed", "cancelled"
    pub status: String,

    /// GitHub PR URL if one was created
    pub pr_url: Option<String>,

    /// Branch that was created
    pub branch: String,

    /// Log of each step execution
    pub step_results: Vec<StepResult>,

    /// Overall error message if status is "failed"
    pub error: Option<String>,

    /// Timestamp when execution started
    pub started_at: i64,

    /// Timestamp when execution completed
    pub completed_at: i64,

    /// Total time in seconds
    pub duration_secs: u64,
}

/// Result of a single step execution
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepResult {
    /// The original step instruction
    pub step: String,

    /// "pending", "running", "success", "failed"
    pub status: String,

    /// LLM-generated actions taken (e.g., code changes, commands run)
    pub actions: Vec<String>,

    /// Test results if applicable
    pub test_output: Option<String>,

    /// Error message if status is "failed"
    pub error: Option<String>,

    /// Timestamp
    pub completed_at: Option<i64>,
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_valid_task_file() {
        let json = r#"{
            "id": "feat-001",
            "repo": "owner/repo",
            "description": "Add new feature",
            "steps": ["Step 1", "Step 2"],
            "branch": "feat/new-feature",
            "labels": ["enhancement"],
            "auto_merge": true
        }"#;

        let task = TaskFile::from_json(json).expect("valid task should parse");
        assert_eq!(task.id, "feat-001");
        assert_eq!(task.repo, "owner/repo");
        assert_eq!(task.description, "Add new feature");
        assert_eq!(task.steps.len(), 2);
        assert_eq!(task.branch, "feat/new-feature");
        assert!(task.auto_merge);
    }

    #[test]
    fn test_default_values() {
        let json = r#"{
            "id": "feat-001",
            "repo": "owner/repo",
            "description": "Add new feature",
            "steps": ["Step 1"],
            "branch": "feat/new"
        }"#;

        let task = TaskFile::from_json(json).expect("valid task should parse");
        assert_eq!(task.labels, Vec::<String>::new());
        assert!(!task.auto_merge);
    }

    #[test]
    fn test_invalid_repo_format() {
        let json = r#"{
            "id": "feat-001",
            "repo": "invalid",
            "description": "Add new feature",
            "steps": ["Step 1"],
            "branch": "feat/new"
        }"#;

        let err = TaskFile::from_json(json).expect_err("should fail validation");
        let err_str = err.to_string();
        assert!(err_str.contains("format") || err_str.contains("invalid"));
    }

    #[test]
    fn test_empty_steps() {
        let json = r#"{
            "id": "feat-001",
            "repo": "owner/repo",
            "description": "Add new feature",
            "steps": [],
            "branch": "feat/new"
        }"#;

        let err = TaskFile::from_json(json).expect_err("should fail validation");
        assert!(err.to_string().contains("at least one step"));
    }

    #[test]
    fn test_empty_description() {
        let json = r#"{
            "id": "feat-001",
            "repo": "owner/repo",
            "description": "",
            "steps": ["Step 1"],
            "branch": "feat/new"
        }"#;

        let err = TaskFile::from_json(json).expect_err("should fail validation");
        assert!(err.to_string().contains("description"));
    }

    #[test]
    fn test_empty_id() {
        let json = r#"{
            "id": "",
            "repo": "owner/repo",
            "description": "Add new feature",
            "steps": ["Step 1"],
            "branch": "feat/new"
        }"#;

        let err = TaskFile::from_json(json).expect_err("should fail validation");
        assert!(err.to_string().contains("id"));
    }

    #[test]
    fn test_minimal_valid_task() {
        let json = r#"{
            "id": "x",
            "repo": "a/b",
            "description": "d",
            "steps": ["s"],
            "branch": "b"
        }"#;

        let task = TaskFile::from_json(json).expect("minimal valid task should parse");
        assert_eq!(task.id, "x");
        assert!(!task.auto_merge);
        assert!(task.labels.is_empty());
    }

    #[test]
    fn test_invalid_id_characters() {
        let json = r#"{
            "id": "feat@001!",
            "repo": "owner/repo",
            "description": "Add new feature",
            "steps": ["Step 1"],
            "branch": "feat/new"
        }"#;

        let err = TaskFile::from_json(json).expect_err("should fail validation");
        assert!(err.to_string().contains("alphanumeric"));
    }
}
