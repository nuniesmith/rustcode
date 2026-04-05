// Todo sync — apply `WorkResult` outcomes back to `todo.md`
//
// This module is the backend for `rustcode todo-sync <todo-md> <results-json>`.
// It reads a [`WorkResult`] JSON file produced by `todo-work`, then updates
// the corresponding items in `todo.md` with the correct status markers:
//
// | Outcome      | Marker | Checkbox |
// |--------------|--------|----------|
// | `success`    | ✅     | `[x]`    |
// | `partial`    | ⚠️     | `[ ]`    |
// | `failed`     | ❌     | `[ ]`    |
// | `skipped`    | —      | `[ ]`    |
//
// Items are matched by their stable 8-char hex `todo_id`. If an item cannot
// be found by ID the sync logs a warning and moves on — it never hard-fails
// on a missing item so that a partially-completed batch still produces a
// meaningful `todo.md` update.
//
// # CLI usage
//
// ```text
// rustcode todo-sync todo.md .rustcode/results/batch-001.json
// rustcode todo-sync todo.md .rustcode/results/batch-001.json --dry-run
// rustcode todo-sync todo.md .rustcode/results/batch-001.json --append-summary
// ```
//
// # Output shape
//
// ```json
// {
//   "synced_at": "2024-01-01T00:00:00Z",
//   "todo_path": "todo.md",
//   "results_path": ".rustcode/results/batch-001.json",
//   "dry_run": false,
//   "items_updated": 3,
//   "items_not_found": 1,
//   "items_skipped": 0,
//   "changes": [
//     {
//       "todo_id": "deadbeef",
//       "old_status": "pending",
//       "new_status": "done",
//       "text_preview": "Fix admin module — accessing non-existent ApiState fields"
//     }
//   ],
//   "not_found_ids": ["cafebabe"],
//   "summary_appended": false
// }
// ```

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

use crate::error::{AuditError, Result};
use crate::todo::todo_file::{CheckboxState, StatusMarker, TodoFile};
use crate::todo::worker::{ItemStatus, WorkResult};

// ============================================================================
// Configuration
// ============================================================================

// Configuration for the sync operation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncConfig {
    // When `true`, compute all changes but do NOT write to disk
    pub dry_run: bool,
    // Whether to append a `## Batch Summary` section to `todo.md`
    pub append_summary: bool,
    // Whether to also sync items whose outcome is `skipped`
    pub sync_skipped: bool,
    // Preview length for item text in the sync report (characters)
    pub text_preview_len: usize,
}

impl Default for SyncConfig {
    fn default() -> Self {
        Self {
            dry_run: false,
            append_summary: false,
            sync_skipped: false,
            text_preview_len: 80,
        }
    }
}

impl SyncConfig {
    pub fn dry_run() -> Self {
        Self {
            dry_run: true,
            ..Default::default()
        }
    }

    pub fn with_summary() -> Self {
        Self {
            append_summary: true,
            ..Default::default()
        }
    }
}

// ============================================================================
// Output types
// ============================================================================

// Human-readable label for the item's state before the sync
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OldStatus {
    Pending,
    Done,
    Partial,
    Blocked,
    Unknown,
}

impl OldStatus {
    fn from_item(checkbox: CheckboxState, marker: StatusMarker) -> Self {
        match marker {
            StatusMarker::Done => OldStatus::Done,
            StatusMarker::Partial => OldStatus::Partial,
            StatusMarker::Blocked => OldStatus::Blocked,
            StatusMarker::None => {
                if checkbox == CheckboxState::Checked {
                    OldStatus::Done
                } else {
                    OldStatus::Pending
                }
            }
        }
    }
}

// Description of a single item change applied during sync
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncChange {
    // Stable todo item ID
    pub todo_id: String,
    // Status before sync
    pub old_status: OldStatus,
    // Status after sync (mirrors `ItemStatus` names)
    pub new_status: String,
    // Truncated item text for human readability
    pub text_preview: String,
    // The note/message attached to the new status
    pub note: Option<String>,
}

// Complete result of one `todo-sync` invocation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncResult {
    pub synced_at: DateTime<Utc>,
    pub todo_path: PathBuf,
    pub results_path: Option<PathBuf>,
    pub dry_run: bool,
    pub items_updated: usize,
    pub items_not_found: usize,
    pub items_skipped: usize,
    pub changes: Vec<SyncChange>,
    pub not_found_ids: Vec<String>,
    pub summary_appended: bool,
}

impl SyncResult {
    fn new(todo_path: PathBuf, results_path: Option<PathBuf>, dry_run: bool) -> Self {
        Self {
            synced_at: Utc::now(),
            todo_path,
            results_path,
            dry_run,
            items_updated: 0,
            items_not_found: 0,
            items_skipped: 0,
            changes: Vec::new(),
            not_found_ids: Vec::new(),
            summary_appended: false,
        }
    }

    // Serialise to pretty-printed JSON
    pub fn to_json_pretty(&self) -> Result<String> {
        serde_json::to_string_pretty(self)
            .map_err(|e| AuditError::other(format!("JSON serialisation failed: {}", e)))
    }

    // Serialise to compact JSON
    pub fn to_json(&self) -> Result<String> {
        serde_json::to_string(self)
            .map_err(|e| AuditError::other(format!("JSON serialisation failed: {}", e)))
    }

    // Whether the sync updated at least one item without any missing IDs
    pub fn is_clean(&self) -> bool {
        self.items_not_found == 0 && self.items_updated > 0
    }

    // Print a human-readable summary to stdout
    pub fn print_summary(&self) {
        println!(
            "\n📋 todo-sync {}",
            if self.dry_run { "(dry-run)" } else { "" }
        );
        println!("   todo.md : {}", self.todo_path.display());
        if let Some(p) = &self.results_path {
            println!("   results : {}", p.display());
        }
        println!("   updated : {}", self.items_updated);
        if self.items_skipped > 0 {
            println!("   skipped : {}", self.items_skipped);
        }
        if self.items_not_found > 0 {
            println!("   ⚠ not found : {}", self.items_not_found);
            for id in &self.not_found_ids {
                println!("       - {}", id);
            }
        }
        println!();
        for change in &self.changes {
            let arrow = match change.new_status.as_str() {
                "success" | "done" => "✅",
                "partial" => "⚠️ ",
                "failed" | "blocked" => "❌",
                _ => "  ",
            };
            println!(
                "   {} [{}] → {} — {}",
                arrow, change.todo_id, change.new_status, change.text_preview
            );
        }
        println!();
    }
}

// ============================================================================
// Syncer
// ============================================================================

// Applies a `WorkResult` to a `todo.md` file
pub struct TodoSyncer {
    config: SyncConfig,
}

impl Default for TodoSyncer {
    fn default() -> Self {
        Self::new(SyncConfig::default())
    }
}

impl TodoSyncer {
    pub fn new(config: SyncConfig) -> Self {
        Self { config }
    }

    // -----------------------------------------------------------------------
    // Public API
    // -----------------------------------------------------------------------

    // Load a `WorkResult` from a JSON file and apply it to `todo_path`.
    pub fn sync_from_file(
        &self,
        todo_path: impl AsRef<Path>,
        results_path: impl AsRef<Path>,
    ) -> Result<SyncResult> {
        let results_path = results_path.as_ref();
        let content = fs::read_to_string(results_path).map_err(AuditError::Io)?;
        let work_result: WorkResult = serde_json::from_str(&content)
            .map_err(|e| AuditError::other(format!("Failed to parse WorkResult JSON: {}", e)))?;

        self.sync(todo_path, &work_result, Some(results_path))
    }

    // Apply an in-memory `WorkResult` to `todo_path`.
    pub fn sync(
        &self,
        todo_path: impl AsRef<Path>,
        work_result: &WorkResult,
        results_path: Option<&Path>,
    ) -> Result<SyncResult> {
        let todo_path = todo_path.as_ref().to_path_buf();
        let mut sync_result = SyncResult::new(
            todo_path.clone(),
            results_path.map(|p| p.to_path_buf()),
            self.config.dry_run,
        );

        let mut todo_file = TodoFile::load(&todo_path)?;

        for item_result in &work_result.item_results {
            // Optionally skip "skipped" outcomes
            if item_result.status == ItemStatus::Skipped && !self.config.sync_skipped {
                sync_result.items_skipped += 1;
                continue;
            }

            // Find the item in todo.md by ID
            let found = todo_file.find_by_id(&item_result.todo_id);

            match found {
                None => {
                    tracing::warn!(
                        "todo-sync: item '{}' not found in todo.md — skipping",
                        item_result.todo_id
                    );
                    sync_result.items_not_found += 1;
                    sync_result.not_found_ids.push(item_result.todo_id.clone());
                }
                Some(existing) => {
                    // Capture before-state
                    let old_status = OldStatus::from_item(existing.checkbox, existing.marker);
                    let text_preview = truncate(&existing.text, self.config.text_preview_len);

                    // Determine the note to attach
                    let note = self.build_note(item_result);

                    // Record the change
                    let new_status_str = match item_result.status {
                        ItemStatus::Success => "done",
                        ItemStatus::Partial => "partial",
                        ItemStatus::Failed => "blocked",
                        ItemStatus::Skipped => "skipped",
                    };

                    sync_result.changes.push(SyncChange {
                        todo_id: item_result.todo_id.clone(),
                        old_status,
                        new_status: new_status_str.to_string(),
                        text_preview,
                        note: Some(note.clone()),
                    });

                    sync_result.items_updated += 1;

                    // Apply the mutation (skipped if dry_run)
                    if !self.config.dry_run {
                        match item_result.status {
                            ItemStatus::Success => {
                                todo_file.mark_done(&item_result.todo_id, &note);
                            }
                            ItemStatus::Partial => {
                                todo_file.mark_partial(&item_result.todo_id, &note);
                            }
                            ItemStatus::Failed => {
                                todo_file.mark_blocked(&item_result.todo_id, &note);
                            }
                            ItemStatus::Skipped => {
                                // sync_skipped is true here; leave checkbox as-is
                            }
                        }
                    }
                }
            }
        }

        // Optionally append a batch summary section
        if self.config.append_summary && !self.config.dry_run {
            self.append_summary_section(&mut todo_file, work_result, &sync_result)?;
            sync_result.summary_appended = true;
        }

        // Write back to disk
        if !self.config.dry_run {
            todo_file.save()?;
            tracing::info!(
                "todo-sync: wrote {} item update(s) to {}",
                sync_result.items_updated,
                todo_path.display()
            );
        }

        Ok(sync_result)
    }

    // -----------------------------------------------------------------------
    // Helpers
    // -----------------------------------------------------------------------

    // Build the note string to attach to the updated item
    fn build_note(&self, item_result: &crate::todo::worker::ItemResult) -> String {
        match item_result.status {
            ItemStatus::Success => {
                if item_result.files_changed.is_empty() {
                    "Done — no files changed".to_string()
                } else {
                    format!("Done — {}", item_result.files_changed.join(", "))
                }
            }
            ItemStatus::Partial => {
                let base = item_result.message.clone();
                match &item_result.error {
                    Some(e) => format!("{} ({})", base, e),
                    None => base,
                }
            }
            ItemStatus::Failed => item_result
                .error
                .clone()
                .unwrap_or_else(|| "unknown error".to_string()),
            ItemStatus::Skipped => "Skipped — no changes generated".to_string(),
        }
    }

    // Append a human-readable `### Batch <id> Summary` block to the footer
    fn append_summary_section(
        &self,
        todo_file: &mut TodoFile,
        work_result: &WorkResult,
        sync_result: &SyncResult,
    ) -> Result<()> {
        let ts = sync_result.synced_at.format("%Y-%m-%d %H:%M UTC");
        let lines = vec![
            String::new(),
            format!(
                "---\n\n### Batch `{}` Summary — {}",
                work_result.batch_id, ts
            ),
            format!(
                "- Attempted: {} | Succeeded: {} | Failed: {} | Skipped: {}",
                work_result.items_attempted,
                work_result.items_succeeded,
                work_result.items_failed,
                work_result.items_skipped
            ),
        ];

        for line in lines {
            todo_file.footer.push(line);
        }

        for change in &sync_result.changes {
            let marker = match change.new_status.as_str() {
                "done" => "✅",
                "partial" => "⚠️",
                "blocked" => "❌",
                _ => "—",
            };
            todo_file.footer.push(format!(
                "- {} `{}` — {}",
                marker, change.todo_id, change.text_preview
            ));
        }

        todo_file.footer.push(String::new());
        Ok(())
    }
}

// ============================================================================
// Convenience free functions
// ============================================================================

// Sync a `WorkResult` JSON file to `todo.md` with default config
pub fn sync_work_result(
    todo_path: impl AsRef<Path>,
    results_path: impl AsRef<Path>,
) -> Result<SyncResult> {
    TodoSyncer::default().sync_from_file(todo_path, results_path)
}

// Sync an in-memory `WorkResult` to `todo.md` with default config
pub fn sync_work_result_direct(
    todo_path: impl AsRef<Path>,
    work_result: &WorkResult,
) -> Result<SyncResult> {
    TodoSyncer::default().sync(todo_path, work_result, None)
}

// ============================================================================
// Helpers
// ============================================================================

fn truncate(s: &str, max_chars: usize) -> String {
    if s.len() <= max_chars {
        s.to_string()
    } else {
        format!("{}…", &s[..max_chars])
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::todo::worker::{FileChange, FileChangeType, ItemResult, WorkResult};

    const SAMPLE_TODO: &str = r#"# RustCode — TODO Backlog

> A living document.

---

## 🔴 High Priority

### API & Data Layer
- [ ] Fix admin module — accessing non-existent `ApiState` fields (`src/api/mod.rs`)
- [ ] Implement proper document listing with filters (`src/api/handlers.rs:345`)

### Search & RAG
- [ ] Integrate RAG context search with LanceDB vector search

---

## 🟡 Medium Priority

### CLI & Developer Experience
- [ ] Actually test the XAI API connection in `test-api` command
"#;

    fn make_work_result(batch_id: &str, dry_run: bool) -> WorkResult {
        WorkResult {
            batch_id: batch_id.to_string(),
            executed_at: Utc::now(),
            dry_run,
            items_attempted: 2,
            items_succeeded: 1,
            items_failed: 1,
            items_skipped: 0,
            file_changes: vec![FileChange {
                file: "src/api/mod.rs".to_string(),
                change_type: FileChangeType::Modified,
                lines_added: 3,
                lines_removed: 1,
                backed_up_to: None,
            }],
            item_results: vec![
                ItemResult {
                    todo_id: "PLACEHOLDER_ID_1".to_string(), // will be replaced in tests
                    status: ItemStatus::Success,
                    message: "Applied 1 change".to_string(),
                    files_changed: vec!["src/api/mod.rs".to_string()],
                    error: None,
                },
                ItemResult {
                    todo_id: "PLACEHOLDER_ID_2".to_string(),
                    status: ItemStatus::Failed,
                    message: "Could not parse response".to_string(),
                    files_changed: vec![],
                    error: Some("LLM returned no JSON".to_string()),
                },
            ],
            errors: vec![],
            todo_md_updated: false,
        }
    }

    fn write_todo(dir: &tempfile::TempDir) -> PathBuf {
        let path = dir.path().join("todo.md");
        fs::write(&path, SAMPLE_TODO).unwrap();
        path
    }

    // -----------------------------------------------------------------------
    // Dry-run: no file written, result still populated
    // -----------------------------------------------------------------------
    #[test]
    fn test_dry_run_does_not_write() {
        let dir = tempfile::tempdir().unwrap();
        let todo_path = write_todo(&dir);

        // Parse the file to get real IDs
        let todo_file = TodoFile::parse(SAMPLE_TODO);
        let items: Vec<_> = todo_file.all_items().collect();
        assert!(!items.is_empty(), "sample should have items");

        let mut wr = make_work_result("batch-001", true);
        wr.item_results[0].todo_id = items[0].id.clone();
        wr.item_results[1].todo_id = items
            .get(1)
            .map(|i| i.id.clone())
            .unwrap_or_else(|| "unknown".to_string());

        let syncer = TodoSyncer::new(SyncConfig::dry_run());
        let result = syncer.sync(&todo_path, &wr, None).unwrap();

        assert!(result.dry_run);
        assert!(result.items_updated > 0, "should have found items");

        // File on disk must be unchanged
        let on_disk = fs::read_to_string(&todo_path).unwrap();
        assert_eq!(on_disk, SAMPLE_TODO);
    }

    // -----------------------------------------------------------------------
    // Normal sync: file updated on disk
    // -----------------------------------------------------------------------
    #[test]
    fn test_normal_sync_updates_file() {
        let dir = tempfile::tempdir().unwrap();
        let todo_path = write_todo(&dir);

        let todo_file = TodoFile::parse(SAMPLE_TODO);
        let items: Vec<_> = todo_file.all_items().collect();

        let mut wr = make_work_result("batch-001", false);
        wr.item_results[0].todo_id = items[0].id.clone();
        // Set item 2 to an ID that won't exist so we can test not_found path
        wr.item_results[1].todo_id = "nonexistent0".to_string();

        let syncer = TodoSyncer::new(SyncConfig::default());
        let result = syncer.sync(&todo_path, &wr, None).unwrap();

        assert!(!result.dry_run);
        assert_eq!(result.items_updated, 1);
        assert_eq!(result.items_not_found, 1);
        assert_eq!(result.not_found_ids, vec!["nonexistent0"]);

        // Check that the first item is now marked done in the written file
        let on_disk = fs::read_to_string(&todo_path).unwrap();
        assert!(
            on_disk.contains("[x]") || on_disk.contains("✅"),
            "done marker expected"
        );
    }

    // -----------------------------------------------------------------------
    // Not-found IDs are tracked
    // -----------------------------------------------------------------------
    #[test]
    fn test_not_found_ids_tracked() {
        let dir = tempfile::tempdir().unwrap();
        let todo_path = write_todo(&dir);

        let mut wr = make_work_result("batch-002", false);
        wr.item_results[0].todo_id = "00000000".to_string();
        wr.item_results[1].todo_id = "ffffffff".to_string();

        let syncer = TodoSyncer::new(SyncConfig::default());
        let result = syncer.sync(&todo_path, &wr, None).unwrap();

        assert_eq!(result.items_not_found, 2);
        assert!(result.not_found_ids.contains(&"00000000".to_string()));
        assert!(result.not_found_ids.contains(&"ffffffff".to_string()));
    }

    // -----------------------------------------------------------------------
    // Summary section appended to footer
    // -----------------------------------------------------------------------
    #[test]
    fn test_append_summary_section() {
        let dir = tempfile::tempdir().unwrap();
        let todo_path = write_todo(&dir);

        let todo_file = TodoFile::parse(SAMPLE_TODO);
        let items: Vec<_> = todo_file.all_items().collect();

        let mut wr = make_work_result("batch-003", false);
        wr.item_results[0].todo_id = items[0].id.clone();
        wr.item_results[1].todo_id = items
            .get(1)
            .map(|i| i.id.clone())
            .unwrap_or_else(|| "missing".to_string());

        let syncer = TodoSyncer::new(SyncConfig::with_summary());
        let result = syncer.sync(&todo_path, &wr, None).unwrap();

        assert!(result.summary_appended);

        let on_disk = fs::read_to_string(&todo_path).unwrap();
        assert!(on_disk.contains("batch-003"), "summary section expected");
    }

    // -----------------------------------------------------------------------
    // JSON serialisation round-trip
    // -----------------------------------------------------------------------
    #[test]
    fn test_sync_result_json_round_trip() {
        let result = SyncResult {
            synced_at: Utc::now(),
            todo_path: PathBuf::from("todo.md"),
            results_path: Some(PathBuf::from("results.json")),
            dry_run: false,
            items_updated: 2,
            items_not_found: 0,
            items_skipped: 1,
            changes: vec![SyncChange {
                todo_id: "deadbeef".to_string(),
                old_status: OldStatus::Pending,
                new_status: "done".to_string(),
                text_preview: "Fix admin module".to_string(),
                note: Some("Done — src/api/mod.rs".to_string()),
            }],
            not_found_ids: vec![],
            summary_appended: false,
        };

        let json = result.to_json_pretty().unwrap();
        let parsed: SyncResult = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed.items_updated, 2);
        assert_eq!(parsed.changes.len(), 1);
        assert_eq!(parsed.changes[0].todo_id, "deadbeef");
        assert_eq!(parsed.changes[0].old_status, OldStatus::Pending);
    }

    // -----------------------------------------------------------------------
    // Sync from file
    // -----------------------------------------------------------------------
    #[test]
    fn test_sync_from_file() {
        let dir = tempfile::tempdir().unwrap();
        let todo_path = write_todo(&dir);

        let wr = make_work_result("batch-004", false);
        let results_path = dir.path().join("results.json");
        fs::write(&results_path, serde_json::to_string_pretty(&wr).unwrap()).unwrap();

        let syncer = TodoSyncer::new(SyncConfig::default());
        let result = syncer.sync_from_file(&todo_path, &results_path).unwrap();

        assert_eq!(result.results_path, Some(results_path));
    }

    // -----------------------------------------------------------------------
    // Helper: truncate
    // -----------------------------------------------------------------------
    #[test]
    fn test_truncate() {
        assert_eq!(truncate("hello world", 5), "hello…");
        assert_eq!(truncate("hello", 10), "hello");
        assert_eq!(truncate("", 10), "");
    }

    // -----------------------------------------------------------------------
    // OldStatus derivation
    // -----------------------------------------------------------------------
    #[test]
    fn test_old_status_from_item() {
        assert_eq!(
            OldStatus::from_item(CheckboxState::Unchecked, StatusMarker::None),
            OldStatus::Pending
        );
        assert_eq!(
            OldStatus::from_item(CheckboxState::Checked, StatusMarker::None),
            OldStatus::Done
        );
        assert_eq!(
            OldStatus::from_item(CheckboxState::Unchecked, StatusMarker::Done),
            OldStatus::Done
        );
        assert_eq!(
            OldStatus::from_item(CheckboxState::Unchecked, StatusMarker::Partial),
            OldStatus::Partial
        );
        assert_eq!(
            OldStatus::from_item(CheckboxState::Unchecked, StatusMarker::Blocked),
            OldStatus::Blocked
        );
    }
}
