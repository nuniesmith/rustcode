//! Todo worker — execute a single gameplan batch
//!
//! This module is the backend for `rustcode todo-work <batch-json>`.
//! It reads a [`GamePlanBatch`] JSON file, generates code changes via the
//! xAI LLM, applies them to disk, and updates the `todo.md` status markers.
//!
//! # Workflow
//!
//! ```text
//! 1. Load batch JSON  →  GamePlanBatch
//! 2. For each BatchWorkItem:
//!    a. Read referenced source files
//!    b. Build a targeted LLM prompt (file content + description + approach)
//!    c. Parse LLM response into FileChange list
//!    d. Validate changes (syntax check where possible, size guard)
//!    e. Write changes to disk (backup originals)
//! 3. Update todo.md — mark batch items ✅ Done (or ⚠️ partial on failure)
//! 4. Emit WorkResult JSON for the caller / workflow
//! ```
//!
//! # Dry-run mode
//!
//! When `WorkConfig::dry_run` is `true` the worker performs every step
//! except actually writing files or updating `todo.md`.  It still returns a
//! fully populated `WorkResult` so the caller can inspect what *would* happen.
//!
//! # Output shape
//!
//! ```json
//! {
//!   "batch_id": "batch-001",
//!   "executed_at": "2024-01-01T00:00:00Z",
//!   "dry_run": false,
//!   "items_attempted": 2,
//!   "items_succeeded": 2,
//!   "items_failed": 0,
//!   "file_changes": [
//!     {
//!       "file": "src/api/mod.rs",
//!       "change_type": "modified",
//!       "lines_added": 3,
//!       "lines_removed": 1,
//!       "backed_up_to": ".rustcode/backups/src_api_mod.rs.bak"
//!     }
//!   ],
//!   "item_results": [
//!     {
//!       "todo_id": "deadbeef",
//!       "status": "success",
//!       "message": "Applied 1 change to src/api/mod.rs",
//!       "files_changed": ["src/api/mod.rs"]
//!     }
//!   ],
//!   "errors": []
//! }
//! ```

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::error::{AuditError, Result};
use crate::grok_client::GrokClient;
use crate::todo::planner::{BatchWorkItem, GamePlanBatch};
use crate::todo::todo_file::TodoFile;

// ============================================================================
// Configuration
// ============================================================================

/// Configuration for the todo worker
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkConfig {
    /// When `true`, generate and validate changes but do NOT write to disk
    pub dry_run: bool,
    /// Root of the repository being modified
    pub repo_root: PathBuf,
    /// Path to `todo.md` to update after successful items
    pub todo_md_path: PathBuf,
    /// Directory used for backup files (relative to `repo_root`)
    pub backup_dir: PathBuf,
    /// Maximum number of lines the LLM may change per file in one pass
    pub max_lines_changed: usize,
    /// Maximum raw bytes of a single source file to send to the LLM
    pub max_file_bytes: u64,
    /// LLM temperature for code generation
    pub temperature: f32,
    /// Whether to create backups of files before modifying them
    pub create_backups: bool,
    /// Maximum token budget for a single LLM code-generation call
    pub max_tokens: Option<u32>,
    /// When `true`, skip the automatic `todo.md` update after a successful
    /// work run.  Set this when you intend to run `todo-sync` as a separate
    /// step — keeping the IDs stable so the syncer can find the items.
    pub skip_todo_md_update: bool,
}

impl WorkConfig {
    /// Create a default config pointing at the current working directory
    pub fn for_repo(repo_root: impl AsRef<Path>) -> Self {
        let repo_root = repo_root.as_ref().to_path_buf();
        let todo_md_path = repo_root.join("todo.md");
        let backup_dir = repo_root.join(".rustcode").join("backups");
        Self {
            dry_run: false,
            repo_root,
            todo_md_path,
            backup_dir,
            max_lines_changed: 200,
            max_file_bytes: 512 * 1024, // 512 KiB
            temperature: 0.1,
            create_backups: true,
            max_tokens: Some(4096),
            // Default: let the worker update todo.md in-line (legacy behaviour).
            // CLI `todo work` sets this to `true` so that `todo-sync` can run
            // afterwards and find the items by their original (unchanged) IDs.
            skip_todo_md_update: false,
        }
    }

    /// Produce a dry-run variant of this config
    pub fn as_dry_run(mut self) -> Self {
        self.dry_run = true;
        self
    }

    /// Instruct the worker to skip the automatic `todo.md` update so that a
    /// subsequent `todo-sync` run can find items by their original IDs.
    pub fn with_skip_todo_md_update(mut self) -> Self {
        self.skip_todo_md_update = true;
        self
    }
}

// ============================================================================
// Work batch input
// ============================================================================

/// A single work batch to execute (loaded from the gameplan JSON)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkBatch {
    pub batch: GamePlanBatch,
    /// Optional path to the gameplan JSON file this batch came from
    pub source_file: Option<PathBuf>,
}

impl WorkBatch {
    /// Load a `WorkBatch` from a JSON file that contains a single `GamePlanBatch`
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let content = fs::read_to_string(path).map_err(AuditError::Io)?;
        let batch: GamePlanBatch = serde_json::from_str(&content)
            .map_err(|e| AuditError::other(format!("Failed to parse batch JSON: {}", e)))?;
        Ok(Self {
            batch,
            source_file: Some(path.to_path_buf()),
        })
    }

    /// Load from a full gameplan JSON file by batch ID
    pub fn load_from_gameplan(gameplan_path: impl AsRef<Path>, batch_id: &str) -> Result<Self> {
        use crate::todo::planner::GamePlan;
        let plan = GamePlan::load(gameplan_path)?;
        let batch = plan
            .batches
            .into_iter()
            .find(|b| b.id == batch_id)
            .ok_or_else(|| {
                AuditError::other(format!("Batch '{}' not found in gameplan", batch_id))
            })?;
        Ok(Self {
            batch,
            source_file: None,
        })
    }
}

// ============================================================================
// Work result output
// ============================================================================

/// Outcome of a single `BatchWorkItem`
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ItemStatus {
    Success,
    Partial,
    Failed,
    Skipped,
}

impl ItemStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            ItemStatus::Success => "success",
            ItemStatus::Partial => "partial",
            ItemStatus::Failed => "failed",
            ItemStatus::Skipped => "skipped",
        }
    }
}

impl std::fmt::Display for ItemStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// Result for a single `BatchWorkItem`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ItemResult {
    /// Stable ID from the gameplan
    pub todo_id: String,
    /// Outcome
    pub status: ItemStatus,
    /// Human-readable summary of what happened
    pub message: String,
    /// Files that were changed (or would be, in dry-run)
    pub files_changed: Vec<String>,
    /// Any error message on failure
    pub error: Option<String>,
}

/// Describes a single file change applied (or planned) by the worker
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileChange {
    /// Relative path to the file
    pub file: String,
    /// Type of change
    pub change_type: FileChangeType,
    /// Approximate lines added
    pub lines_added: usize,
    /// Approximate lines removed
    pub lines_removed: usize,
    /// Path to the backup file, if one was created
    pub backed_up_to: Option<String>,
}

/// The kind of change applied to a file
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FileChangeType {
    Created,
    Modified,
    Deleted,
}

/// Aggregated result of executing one `WorkBatch`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkResult {
    pub batch_id: String,
    pub executed_at: DateTime<Utc>,
    pub dry_run: bool,
    pub items_attempted: usize,
    pub items_succeeded: usize,
    pub items_failed: usize,
    pub items_skipped: usize,
    pub file_changes: Vec<FileChange>,
    pub item_results: Vec<ItemResult>,
    pub errors: Vec<String>,
    pub todo_md_updated: bool,
}

impl WorkResult {
    fn new(batch_id: String, dry_run: bool) -> Self {
        Self {
            batch_id,
            executed_at: Utc::now(),
            dry_run,
            items_attempted: 0,
            items_succeeded: 0,
            items_failed: 0,
            items_skipped: 0,
            file_changes: Vec::new(),
            item_results: Vec::new(),
            errors: Vec::new(),
            todo_md_updated: false,
        }
    }

    /// Serialise to pretty-printed JSON
    pub fn to_json_pretty(&self) -> Result<String> {
        serde_json::to_string_pretty(self)
            .map_err(|e| AuditError::other(format!("JSON serialisation failed: {}", e)))
    }

    /// Serialise to compact JSON
    pub fn to_json(&self) -> Result<String> {
        serde_json::to_string(self)
            .map_err(|e| AuditError::other(format!("JSON serialisation failed: {}", e)))
    }

    /// Whether the batch completed with all items successful
    pub fn is_fully_successful(&self) -> bool {
        self.items_failed == 0 && self.items_skipped == 0 && self.items_succeeded > 0
    }
}

// ============================================================================
// LLM response types
// ============================================================================

/// A single file change proposed by the LLM
#[derive(Debug, Clone, Deserialize)]
struct LlmFileChange {
    /// Relative path to the file
    file: String,
    /// The complete new content of the file (preferred) …
    new_content: Option<String>,
    /// … or a list of hunks to apply
    hunks: Option<Vec<LlmHunk>>,
    /// Type of operation
    #[serde(default = "default_op")]
    operation: String,
    /// Brief explanation of the change
    #[allow(dead_code)]
    explanation: Option<String>,
}

fn default_op() -> String {
    "modify".to_string()
}

/// A search-and-replace hunk from the LLM
#[derive(Debug, Clone, Deserialize)]
struct LlmHunk {
    /// The exact text to search for (must match verbatim)
    search: String,
    /// The replacement text
    replace: String,
}

/// Top-level structure of the LLM code-generation response
#[derive(Debug, Clone, Deserialize)]
struct LlmCodeResponse {
    changes: Vec<LlmFileChange>,
    #[serde(default)]
    #[allow(dead_code)]
    summary: String,
}

// ============================================================================
// Worker
// ============================================================================

/// Executes a single `WorkBatch` by generating and applying code changes
pub struct TodoWorker {
    config: WorkConfig,
    client: GrokClient,
}

impl TodoWorker {
    /// Create a worker from environment (`XAI_API_KEY`)
    pub async fn from_env(config: WorkConfig, db: crate::db::Database) -> Result<Self> {
        let client = GrokClient::from_env(db)
            .await
            .map_err(|e| AuditError::other(format!("Failed to create GrokClient: {}", e)))?;
        Ok(Self { config, client })
    }

    /// Create a worker with an explicit `GrokClient`
    pub fn new(config: WorkConfig, client: GrokClient) -> Self {
        Self { config, client }
    }

    // -----------------------------------------------------------------------
    // Public API
    // -----------------------------------------------------------------------

    /// Execute all items in a `WorkBatch`.
    ///
    /// Returns a `WorkResult` regardless of individual item outcomes.
    pub async fn execute(&self, work_batch: &WorkBatch) -> Result<WorkResult> {
        let batch = &work_batch.batch;
        let mut result = WorkResult::new(batch.id.clone(), self.config.dry_run);

        // Ensure backup dir exists (even in dry-run, for logging)
        if self.config.create_backups && !self.config.dry_run {
            fs::create_dir_all(&self.config.backup_dir).map_err(AuditError::Io)?;
        }

        // Process each item
        for item in &batch.items {
            result.items_attempted += 1;
            match self.execute_item(item, &mut result).await {
                Ok(item_result) => {
                    match item_result.status {
                        ItemStatus::Success => result.items_succeeded += 1,
                        ItemStatus::Partial => {
                            result.items_succeeded += 1; // counts as attempted success
                        }
                        ItemStatus::Failed => result.items_failed += 1,
                        ItemStatus::Skipped => result.items_skipped += 1,
                    }
                    result.item_results.push(item_result);
                }
                Err(e) => {
                    result.items_failed += 1;
                    let err_msg = format!("Item {} error: {}", item.todo_id, e);
                    result.errors.push(err_msg.clone());
                    result.item_results.push(ItemResult {
                        todo_id: item.todo_id.clone(),
                        status: ItemStatus::Failed,
                        message: "Unexpected error during execution".to_string(),
                        files_changed: Vec::new(),
                        error: Some(err_msg),
                    });
                }
            }
        }

        // Update todo.md — skipped when `skip_todo_md_update` is set so that
        // a subsequent `todo-sync` run can match items by their original IDs.
        if !self.config.dry_run && !self.config.skip_todo_md_update {
            match self.update_todo_md(&result) {
                Ok(_) => result.todo_md_updated = true,
                Err(e) => {
                    let msg = format!("Failed to update todo.md: {}", e);
                    tracing::warn!("{}", msg);
                    result.errors.push(msg);
                }
            }
        }

        Ok(result)
    }

    // -----------------------------------------------------------------------
    // Item execution
    // -----------------------------------------------------------------------

    async fn execute_item(
        &self,
        item: &BatchWorkItem,
        result: &mut WorkResult,
    ) -> Result<ItemResult> {
        tracing::info!(
            "Working on todo item {} — {}",
            item.todo_id,
            item.description
        );

        // Read referenced source files
        let file_contents = self.read_referenced_files(item)?;

        if file_contents.is_empty() && item.files.is_empty() {
            return Ok(ItemResult {
                todo_id: item.todo_id.clone(),
                status: ItemStatus::Skipped,
                message: "No source files referenced — skipped".to_string(),
                files_changed: Vec::new(),
                error: None,
            });
        }

        // Build the LLM prompt
        let prompt = self.build_code_prompt(item, &file_contents);

        // Call the LLM
        let raw = self
            .client
            .ask(&prompt, None)
            .await
            .map_err(|e| AuditError::other(format!("LLM call failed: {}", e)))?;

        // Parse the LLM response into file changes
        let llm_changes = match self.parse_llm_response(&raw) {
            Ok(changes) => changes,
            Err(e) => {
                return Ok(ItemResult {
                    todo_id: item.todo_id.clone(),
                    status: ItemStatus::Failed,
                    message: format!("Could not parse LLM response: {}", e),
                    files_changed: Vec::new(),
                    error: Some(e.to_string()),
                });
            }
        };

        if llm_changes.changes.is_empty() {
            return Ok(ItemResult {
                todo_id: item.todo_id.clone(),
                status: ItemStatus::Skipped,
                message: "LLM returned no file changes".to_string(),
                files_changed: Vec::new(),
                error: None,
            });
        }

        // Apply changes
        let mut files_changed = Vec::new();
        let mut apply_errors = Vec::new();

        for change in &llm_changes.changes {
            match self.apply_file_change(change, result) {
                Ok(fc) => {
                    files_changed.push(fc.file.clone());
                    result.file_changes.push(fc);
                }
                Err(e) => {
                    apply_errors.push(format!("{}: {}", change.file, e));
                }
            }
        }

        if apply_errors.is_empty() {
            Ok(ItemResult {
                todo_id: item.todo_id.clone(),
                status: ItemStatus::Success,
                message: format!(
                    "Applied {} change(s): {}",
                    files_changed.len(),
                    files_changed.join(", ")
                ),
                files_changed,
                error: None,
            })
        } else if !files_changed.is_empty() {
            Ok(ItemResult {
                todo_id: item.todo_id.clone(),
                status: ItemStatus::Partial,
                message: format!(
                    "Partial: {} succeeded, {} failed",
                    files_changed.len(),
                    apply_errors.len()
                ),
                files_changed,
                error: Some(apply_errors.join("; ")),
            })
        } else {
            Ok(ItemResult {
                todo_id: item.todo_id.clone(),
                status: ItemStatus::Failed,
                message: "All file changes failed".to_string(),
                files_changed: Vec::new(),
                error: Some(apply_errors.join("; ")),
            })
        }
    }

    // -----------------------------------------------------------------------
    // File reading
    // -----------------------------------------------------------------------

    fn read_referenced_files(&self, item: &BatchWorkItem) -> Result<HashMap<String, String>> {
        let mut contents = HashMap::new();

        for file_ref in &item.files {
            // Strip optional `:line` suffix
            let rel_path = file_ref.split(':').next().unwrap_or(file_ref.as_str());

            let abs_path = self.config.repo_root.join(rel_path);

            if !abs_path.exists() {
                tracing::warn!("Referenced file not found: {}", abs_path.display());
                continue;
            }

            let metadata = fs::metadata(&abs_path).map_err(AuditError::Io)?;
            if metadata.len() > self.config.max_file_bytes {
                tracing::warn!(
                    "File {} ({} bytes) exceeds max_file_bytes — skipping",
                    abs_path.display(),
                    metadata.len()
                );
                continue;
            }

            let content = fs::read_to_string(&abs_path).map_err(AuditError::Io)?;
            contents.insert(rel_path.to_string(), content);
        }

        Ok(contents)
    }

    // -----------------------------------------------------------------------
    // Prompt construction
    // -----------------------------------------------------------------------

    fn build_code_prompt(
        &self,
        item: &BatchWorkItem,
        file_contents: &HashMap<String, String>,
    ) -> String {
        let files_section = if file_contents.is_empty() {
            "No existing files provided — create new file(s) as needed.".to_string()
        } else {
            file_contents
                .iter()
                .map(|(path, content)| {
                    // Add line numbers for easier referencing
                    let numbered: String = content
                        .lines()
                        .enumerate()
                        .map(|(i, l)| format!("{:4} | {}", i + 1, l))
                        .collect::<Vec<_>>()
                        .join("\n");
                    format!("### {}\n```rust\n{}\n```", path, numbered)
                })
                .collect::<Vec<_>>()
                .join("\n\n")
        };

        let acceptance = item
            .acceptance_criteria
            .as_deref()
            .map(|ac| format!("\n## Acceptance criteria\n\n{}", ac))
            .unwrap_or_default();

        format!(
            r#"You are a senior Rust engineer making a targeted code change.

## Task

{description}

## Approach

{approach}
{acceptance}

## Source files

{files_section}

## Instructions

1. Make the minimal change required to complete the task.
2. Preserve existing code style, formatting, and comments.
3. Do NOT change code unrelated to the task.
4. Return ONLY valid JSON — no markdown fences, no prose.
5. Each change must include EITHER `new_content` (full file replacement) OR
   `hunks` (list of search/replace pairs).  Prefer `hunks` for small changes.
6. `search` strings in hunks must match the file content EXACTLY (whitespace included).
7. `operation` must be one of: `create`, `modify`, `delete`.

## Required output shape

{{
  "changes": [
    {{
      "file": "src/path/to/file.rs",
      "operation": "modify",
      "hunks": [
        {{
          "search": "<exact text to find>",
          "replace": "<replacement text>"
        }}
      ],
      "explanation": "<one-line summary>"
    }}
  ],
  "summary": "<overall summary of all changes>"
}}
"#,
            description = item.description,
            approach = item.approach,
            acceptance = acceptance,
            files_section = files_section,
        )
    }

    // -----------------------------------------------------------------------
    // LLM response parsing
    // -----------------------------------------------------------------------

    fn parse_llm_response(&self, raw: &str) -> Result<LlmCodeResponse> {
        // Strategy 1: parse as-is
        if let Ok(r) = serde_json::from_str::<LlmCodeResponse>(raw) {
            return Ok(r);
        }

        // Strategy 2: strip markdown fences
        let stripped = strip_markdown_fences(raw);
        if let Ok(r) = serde_json::from_str::<LlmCodeResponse>(&stripped) {
            return Ok(r);
        }

        // Strategy 3: find outermost `{ … }`
        if let Some(start) = raw.find('{') {
            if let Some(end) = raw.rfind('}') {
                if end > start {
                    let slice = &raw[start..=end];
                    if let Ok(r) = serde_json::from_str::<LlmCodeResponse>(slice) {
                        return Ok(r);
                    }
                }
            }
        }

        Err(AuditError::other(format!(
            "Could not parse LLM code response. First 300 chars: {}",
            &raw[..raw.len().min(300)]
        )))
    }

    // -----------------------------------------------------------------------
    // Applying changes
    // -----------------------------------------------------------------------

    fn apply_file_change(
        &self,
        change: &LlmFileChange,
        _result: &mut WorkResult,
    ) -> Result<FileChange> {
        let abs_path = self.config.repo_root.join(&change.file);

        // Safety guard: must stay inside repo_root
        if !abs_path.starts_with(&self.config.repo_root) {
            return Err(AuditError::other(format!(
                "Refusing to write outside repo root: {}",
                abs_path.display()
            )));
        }

        let op = change.operation.as_str();

        match op {
            "delete" => self.apply_delete(&abs_path, change),
            _ => {
                if let Some(new_content) = &change.new_content {
                    self.apply_full_replace(&abs_path, change, new_content)
                } else if let Some(hunks) = &change.hunks {
                    self.apply_hunks(&abs_path, change, hunks)
                } else {
                    Err(AuditError::other(format!(
                        "Change for {} has neither new_content nor hunks",
                        change.file
                    )))
                }
            }
        }
    }

    fn apply_full_replace(
        &self,
        abs_path: &Path,
        change: &LlmFileChange,
        new_content: &str,
    ) -> Result<FileChange> {
        let (old_lines, backed_up_to) = if abs_path.exists() {
            let old = fs::read_to_string(abs_path).map_err(AuditError::Io)?;
            let backed_up = self.maybe_backup(abs_path, &old)?;
            (old.lines().count(), backed_up)
        } else {
            (0, None)
        };

        let new_lines = new_content.lines().count();
        let lines_added = new_lines.saturating_sub(old_lines);
        let lines_removed = old_lines.saturating_sub(new_lines);

        // Guard against oversized changes
        let total_delta = lines_added + lines_removed;
        if total_delta > self.config.max_lines_changed {
            return Err(AuditError::other(format!(
                "Change to {} would modify {} lines (limit {})",
                change.file, total_delta, self.config.max_lines_changed
            )));
        }

        if !self.config.dry_run {
            if let Some(parent) = abs_path.parent() {
                fs::create_dir_all(parent).map_err(AuditError::Io)?;
            }
            fs::write(abs_path, new_content).map_err(AuditError::Io)?;
            tracing::info!(
                "Wrote {} ({} lines, +{} -{})",
                change.file,
                new_lines,
                lines_added,
                lines_removed
            );
        } else {
            tracing::info!(
                "[dry-run] Would write {} ({} lines, +{} -{})",
                change.file,
                new_lines,
                lines_added,
                lines_removed
            );
        }

        let change_type = if old_lines == 0 {
            FileChangeType::Created
        } else {
            FileChangeType::Modified
        };

        Ok(FileChange {
            file: change.file.clone(),
            change_type,
            lines_added,
            lines_removed,
            backed_up_to,
        })
    }

    fn apply_hunks(
        &self,
        abs_path: &Path,
        change: &LlmFileChange,
        hunks: &[LlmHunk],
    ) -> Result<FileChange> {
        let original = if abs_path.exists() {
            fs::read_to_string(abs_path).map_err(AuditError::Io)?
        } else {
            String::new()
        };

        let backed_up_to = self.maybe_backup(abs_path, &original)?;

        let mut patched = original.clone();
        let mut applied = 0usize;

        for hunk in hunks {
            if patched.contains(&hunk.search) {
                patched = patched.replacen(&hunk.search, &hunk.replace, 1);
                applied += 1;
            } else {
                tracing::warn!(
                    "Hunk search string not found in {} — skipping hunk",
                    change.file
                );
            }
        }

        if applied == 0 {
            return Err(AuditError::other(format!(
                "No hunks matched in {}",
                change.file
            )));
        }

        let old_lines = original.lines().count();
        let new_lines = patched.lines().count();
        let lines_added = new_lines.saturating_sub(old_lines);
        let lines_removed = old_lines.saturating_sub(new_lines);

        let total_delta = lines_added + lines_removed;
        if total_delta > self.config.max_lines_changed {
            return Err(AuditError::other(format!(
                "Hunk change to {} would modify {} lines (limit {})",
                change.file, total_delta, self.config.max_lines_changed
            )));
        }

        if !self.config.dry_run {
            fs::write(abs_path, &patched).map_err(AuditError::Io)?;
            tracing::info!(
                "Patched {} with {}/{} hunks (+{} -{})",
                change.file,
                applied,
                hunks.len(),
                lines_added,
                lines_removed
            );
        } else {
            tracing::info!(
                "[dry-run] Would patch {} with {}/{} hunks (+{} -{})",
                change.file,
                applied,
                hunks.len(),
                lines_added,
                lines_removed
            );
        }

        Ok(FileChange {
            file: change.file.clone(),
            change_type: FileChangeType::Modified,
            lines_added,
            lines_removed,
            backed_up_to,
        })
    }

    fn apply_delete(&self, abs_path: &Path, change: &LlmFileChange) -> Result<FileChange> {
        if !abs_path.exists() {
            return Ok(FileChange {
                file: change.file.clone(),
                change_type: FileChangeType::Deleted,
                lines_added: 0,
                lines_removed: 0,
                backed_up_to: None,
            });
        }

        let original = fs::read_to_string(abs_path).map_err(AuditError::Io)?;
        let lines_removed = original.lines().count();
        let backed_up_to = self.maybe_backup(abs_path, &original)?;

        if !self.config.dry_run {
            fs::remove_file(abs_path).map_err(AuditError::Io)?;
            tracing::info!("Deleted {}", change.file);
        } else {
            tracing::info!("[dry-run] Would delete {}", change.file);
        }

        Ok(FileChange {
            file: change.file.clone(),
            change_type: FileChangeType::Deleted,
            lines_added: 0,
            lines_removed,
            backed_up_to,
        })
    }

    // -----------------------------------------------------------------------
    // Backup helpers
    // -----------------------------------------------------------------------

    fn maybe_backup(&self, abs_path: &Path, content: &str) -> Result<Option<String>> {
        if !self.config.create_backups || self.config.dry_run {
            return Ok(None);
        }

        // Produce a flat backup filename: slashes → underscores
        let rel = abs_path
            .strip_prefix(&self.config.repo_root)
            .unwrap_or(abs_path);
        let flat = rel.to_string_lossy().replace(['/', '\\'], "_");
        let backup_path = self.config.backup_dir.join(format!("{}.bak", flat));

        if let Some(parent) = backup_path.parent() {
            fs::create_dir_all(parent).map_err(AuditError::Io)?;
        }

        fs::write(&backup_path, content).map_err(AuditError::Io)?;
        Ok(Some(backup_path.to_string_lossy().to_string()))
    }

    // -----------------------------------------------------------------------
    // todo.md update
    // -----------------------------------------------------------------------

    fn update_todo_md(&self, result: &WorkResult) -> Result<()> {
        if !self.config.todo_md_path.exists() {
            tracing::warn!(
                "todo.md not found at {} — skipping update",
                self.config.todo_md_path.display()
            );
            return Ok(());
        }

        let mut todo_file = TodoFile::load(&self.config.todo_md_path)?;

        for item_result in &result.item_results {
            match item_result.status {
                ItemStatus::Success => {
                    let note = format!(
                        "Done — {}",
                        if item_result.files_changed.is_empty() {
                            "no files changed".to_string()
                        } else {
                            item_result.files_changed.join(", ")
                        }
                    );
                    // Try to update by ID; if not found, try text search
                    if !todo_file.mark_done(&item_result.todo_id, &note) {
                        tracing::debug!(
                            "Todo item {} not found by ID in todo.md — skipping mark_done",
                            item_result.todo_id
                        );
                    }
                }
                ItemStatus::Partial => {
                    let note = item_result
                        .error
                        .as_deref()
                        .unwrap_or("partial completion")
                        .to_string();
                    todo_file.mark_partial(&item_result.todo_id, note);
                }
                ItemStatus::Failed => {
                    let reason = item_result
                        .error
                        .as_deref()
                        .unwrap_or("unknown error")
                        .to_string();
                    todo_file.mark_blocked(&item_result.todo_id, reason);
                }
                ItemStatus::Skipped => {
                    // Leave as-is
                }
            }
        }

        todo_file.save()?;
        tracing::info!("Updated todo.md at {}", self.config.todo_md_path.display());
        Ok(())
    }
}

// ============================================================================
// Helpers
// ============================================================================

/// Strip markdown code fences from LLM output
fn strip_markdown_fences(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_fence = false;
    for line in s.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("```") {
            in_fence = !in_fence;
            continue;
        }
        if !in_fence || trimmed.starts_with('{') || trimmed.starts_with('"') {
            out.push_str(line);
            out.push('\n');
        }
    }
    if out.trim().is_empty() {
        s.to_string()
    } else {
        out
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // WorkConfig tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_work_config_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = WorkConfig::for_repo(dir.path());
        assert!(!cfg.dry_run);
        assert!(cfg.create_backups);
        assert_eq!(cfg.temperature, 0.1);
        assert_eq!(cfg.todo_md_path, dir.path().join("todo.md"));
    }

    #[test]
    fn test_work_config_dry_run() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = WorkConfig::for_repo(dir.path()).as_dry_run();
        assert!(cfg.dry_run);
    }

    // -----------------------------------------------------------------------
    // WorkResult tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_work_result_new() {
        let r = WorkResult::new("batch-001".to_string(), false);
        assert_eq!(r.batch_id, "batch-001");
        assert!(!r.dry_run);
        assert_eq!(r.items_attempted, 0);
        assert!(!r.is_fully_successful());
    }

    #[test]
    fn test_work_result_is_fully_successful() {
        let mut r = WorkResult::new("batch-001".to_string(), false);
        r.items_attempted = 2;
        r.items_succeeded = 2;
        assert!(r.is_fully_successful());

        r.items_failed = 1;
        assert!(!r.is_fully_successful());
    }

    #[test]
    fn test_work_result_json_round_trip() {
        let mut r = WorkResult::new("batch-001".to_string(), true);
        r.items_attempted = 1;
        r.items_succeeded = 1;
        r.file_changes.push(FileChange {
            file: "src/lib.rs".to_string(),
            change_type: FileChangeType::Modified,
            lines_added: 5,
            lines_removed: 2,
            backed_up_to: None,
        });

        let json = r.to_json_pretty().unwrap();
        let parsed: WorkResult = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.batch_id, "batch-001");
        assert_eq!(parsed.file_changes.len(), 1);
        assert_eq!(parsed.file_changes[0].lines_added, 5);
    }

    // -----------------------------------------------------------------------
    // Hunk application tests (via apply_hunks internals — tested indirectly)
    // -----------------------------------------------------------------------

    #[test]
    fn test_strip_markdown_fences() {
        let input = "text\n```json\n{\"changes\":[]}\n```\nmore";
        let result = strip_markdown_fences(input);
        assert!(result.contains("{\"changes\":[]}"));
        assert!(!result.contains("```"));
    }

    #[test]
    fn test_strip_markdown_fences_no_fence() {
        let input = "{\"changes\":[],\"summary\":\"ok\"}";
        let result = strip_markdown_fences(input);
        assert_eq!(result.trim(), input.trim());
    }

    // -----------------------------------------------------------------------
    // WorkBatch load tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_work_batch_load_from_json() {
        use crate::todo::planner::EffortEstimate;

        let batch = GamePlanBatch {
            id: "batch-001".to_string(),
            title: "Test batch".to_string(),
            priority: "high".to_string(),
            estimated_effort: EffortEstimate::Small,
            items: vec![BatchWorkItem {
                todo_id: "deadbeef".to_string(),
                description: "Fix the thing".to_string(),
                files: vec!["src/lib.rs".to_string()],
                approach: "Replace broken call".to_string(),
                acceptance_criteria: None,
            }],
            rationale: "Testing".to_string(),
            dependencies: Vec::new(),
        };

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("batch.json");
        let json = serde_json::to_string_pretty(&batch).unwrap();
        fs::write(&path, &json).unwrap();

        let loaded = WorkBatch::load(&path).unwrap();
        assert_eq!(loaded.batch.id, "batch-001");
        assert_eq!(loaded.batch.items.len(), 1);
        assert_eq!(loaded.batch.items[0].todo_id, "deadbeef");
    }

    // -----------------------------------------------------------------------
    // ItemStatus tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_item_status_display() {
        assert_eq!(ItemStatus::Success.as_str(), "success");
        assert_eq!(ItemStatus::Partial.as_str(), "partial");
        assert_eq!(ItemStatus::Failed.as_str(), "failed");
        assert_eq!(ItemStatus::Skipped.as_str(), "skipped");
    }

    // -----------------------------------------------------------------------
    // FileChangeType serialisation
    // -----------------------------------------------------------------------

    #[test]
    fn test_file_change_type_serde() {
        let json = serde_json::to_string(&FileChangeType::Created).unwrap();
        assert_eq!(json, "\"created\"");

        let back: FileChangeType = serde_json::from_str("\"modified\"").unwrap();
        assert_eq!(back, FileChangeType::Modified);
    }
}
