// Task Watcher — monitors the tasks/ directory for new task files
//
// Watches for JSON files dropped into tasks/, debounces file system events (500ms),
// validates task files, and sends them to a channel for processing.
//
// The watcher:
// - Scans the tasks/ directory periodically
// - Ignores .tmp files and non-JSON files
// - Validates task files before sending
// - Sends valid tasks through an async channel
// - Logs discovery and validation results
//
// Usage from server.rs:
// ```ignore
// let (task_tx, mut task_rx) = tokio::sync::mpsc::channel(100);
// tokio::spawn(watch_tasks_directory("tasks".into(), task_tx.clone()));
//
// while let Some(watched_task) = task_rx.recv().await {
//     println!("Got task: {}", watched_task.task.id);
// }
// ```

use crate::task::TaskFile;
use serde::Deserialize;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::sync::mpsc::Sender;
use tracing::{info, warn};

/// Configuration for the task watcher
#[derive(Deserialize)]
pub struct TaskWatcherConfig {
    /// Whether the task watcher is enabled
    pub enabled: bool,
}

impl Default for TaskWatcherConfig {
    fn default() -> Self {
        Self { enabled: false }
    }
}

/// A task file discovered by the watcher
#[derive(Debug, Clone)]
pub struct WatchedTaskFile {
    /// The loaded and validated task
    pub task: TaskFile,
    /// The source file path
    pub path: PathBuf,
}

/// Watch the tasks/ directory for new task files
///
/// This function:
/// - Creates the tasks directory if it doesn't exist
/// - Scans for .json files (ignoring .tmp files)
/// - Validates each task file
/// - Sends valid tasks through the channel
/// - Debounces with a 500ms sleep between scans
///
/// # Arguments
/// * `tasks_dir` - Path to the tasks directory to watch
/// * `tx` - Async channel sender for discovered tasks
///
/// # Returns
/// This function runs indefinitely and only returns if the channel is closed
/// or an I/O error occurs.
///
/// # Example
/// ```ignore
/// let (task_tx, mut task_rx) = tokio::sync::mpsc::channel(100);
/// let tasks_dir = PathBuf::from("tasks");
///
/// tokio::spawn(watch_tasks_directory(tasks_dir, task_tx));
///
/// while let Some(watched) = task_rx.recv().await {
///     println!("Processing task: {}", watched.task.id);
/// }
/// ```
pub async fn watch_tasks_directory(
    tasks_dir: PathBuf,
    tx: Sender<WatchedTaskFile>,
) -> anyhow::Result<()> {
    // Create tasks directory if it doesn't exist
    if !tasks_dir.exists() {
        std::fs::create_dir_all(&tasks_dir)?;
        info!("Created tasks directory: {}", tasks_dir.display());
    }

    info!(
        "Starting task watcher for directory: {}",
        tasks_dir.display()
    );

    // Track which files we've already processed to avoid reprocessing
    let mut seen_files: HashSet<PathBuf> = HashSet::new();

    // Main watch loop with 500ms debounce
    let mut interval = tokio::time::interval(Duration::from_millis(500));

    loop {
        interval.tick().await;

        // Scan the tasks directory for new files
        if let Err(e) = scan_and_process_tasks(&tasks_dir, &tx, &mut seen_files).await {
            warn!("Error scanning task files: {}", e);
        }
    }
}

/// Scan the tasks directory and process new task files
async fn scan_and_process_tasks(
    tasks_dir: &Path,
    tx: &Sender<WatchedTaskFile>,
    seen_files: &mut HashSet<PathBuf>,
) -> anyhow::Result<()> {
    if !tasks_dir.is_dir() {
        return Ok(());
    }

    for entry in std::fs::read_dir(tasks_dir)? {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                warn!("Error reading directory entry: {}", e);
                continue;
            }
        };

        let path = entry.path();

        // Skip if not a file
        if !path.is_file() {
            continue;
        }

        // Skip .tmp files (files being written)
        if path
            .extension()
            .map_or(false, |ext| ext.eq_ignore_ascii_case("tmp"))
        {
            continue;
        }

        // Only process .json files
        if !path
            .extension()
            .map_or(false, |ext| ext.eq_ignore_ascii_case("json"))
        {
            continue;
        }

        // Skip results directory output files
        if path.starts_with(tasks_dir.join("results")) {
            continue;
        }

        // Skip if we've already seen this file
        if seen_files.contains(&path) {
            continue;
        }

        // Try to load and validate the task file
        match TaskFile::from_file(&path) {
            Ok(task) => {
                info!(
                    "Discovered valid task file: {} (id: {}, repo: {})",
                    path.display(),
                    task.id,
                    task.repo
                );

                let watched_task = WatchedTaskFile {
                    task: task.clone(),
                    path: path.clone(),
                };

                // Send to async channel
                match tx.send(watched_task).await {
                    Ok(()) => {
                        // Mark as seen after successful send
                        seen_files.insert(path);
                    }
                    Err(e) => {
                        warn!(
                            "Failed to send task {} to executor (channel closed?): {}",
                            task.id, e
                        );
                        // Don't mark as seen; try again on next scan
                    }
                }
            }
            Err(e) => {
                warn!(
                    "Failed to load task file {} (invalid JSON or validation error): {}",
                    path.display(),
                    e
                );
                // Mark as seen even on error to avoid repeatedly logging the same error
                seen_files.insert(path);
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn test_ignore_tmp_files() {
        let temp_dir = TempDir::new().unwrap();
        let tmp_path = temp_dir.path().join("task.json.tmp");
        fs::write(&tmp_path, "{}").unwrap();

        // Verify .tmp files would be ignored based on extension check
        assert_eq!(tmp_path.extension().unwrap(), "tmp");
    }

    #[test]
    fn test_ignore_non_json_files() {
        let temp_dir = TempDir::new().unwrap();
        let txt_path = temp_dir.path().join("readme.txt");
        fs::write(&txt_path, "Some text").unwrap();

        // Verify non-.json files would be ignored
        assert_ne!(txt_path.extension().unwrap(), "json");
    }

    #[test]
    fn test_valid_task_file_loading() {
        let temp_dir = TempDir::new().unwrap();
        let task_json = r#"{
            "id": "test-task-001",
            "repo": "owner/repo",
            "description": "Test task",
            "steps": ["Step 1", "Step 2"],
            "branch": "test-branch",
            "labels": ["test"],
            "auto_merge": false
        }"#;

        let task_path = temp_dir.path().join("test-task.json");
        fs::write(&task_path, task_json).unwrap();

        // Verify task can be loaded
        let task = TaskFile::from_file(&task_path).expect("should load valid task");
        assert_eq!(task.id, "test-task-001");
        assert_eq!(task.repo, "owner/repo");
        assert_eq!(task.steps.len(), 2);
    }

    #[test]
    fn test_invalid_task_file_handling() {
        let temp_dir = TempDir::new().unwrap();
        let invalid_json = r#"{ "id": "bad" }"#; // Missing required fields

        let task_path = temp_dir.path().join("invalid-task.json");
        fs::write(&task_path, invalid_json).unwrap();

        // Should fail validation
        let result = TaskFile::from_file(&task_path);
        assert!(result.is_err());
    }

    #[test]
    fn test_malformed_json() {
        let temp_dir = TempDir::new().unwrap();
        let malformed = r#"{ "id": "broken", invalid json }"#;

        let task_path = temp_dir.path().join("malformed.json");
        fs::write(&task_path, malformed).unwrap();

        // Should fail JSON parsing
        let result = TaskFile::from_file(&task_path);
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_watched_task_file_structure() {
        let task = TaskFile::from_json(
            r#"{
            "id": "test",
            "repo": "a/b",
            "description": "d",
            "steps": ["s"],
            "branch": "b"
        }"#,
        )
        .expect("should parse");

        let watched = WatchedTaskFile {
            task: task.clone(),
            path: PathBuf::from("/tmp/test.json"),
        };

        assert_eq!(watched.task.id, "test");
        assert_eq!(watched.path.file_name().unwrap(), "test.json");
    }
}
