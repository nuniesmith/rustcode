// Simplified Task Management System
//
// This module provides a consolidated task management system that replaces
// the previous queue-based architecture with a simpler, more maintainable approach.
//
// ## Overview
//
// - **Models**: Core task types and database operations
// - **Grouping**: Smart task grouping for efficient batch processing
// - **Schema**: JSON schema for task files dropped into the tasks/ directory
//
// ## Usage
//
// ```rust,no_run
// use rustcode::task::{Task, TaskSource, TaskStatus};
//
// // Create a new task
// let task = Task::new("Fix memory leak", TaskSource::Manual)
//     .with_priority(8)
//     .with_source_file("rustcode", "src/processor.rs", Some(45));
// ```
//
// ## Task Files
//
// Drop JSON files into the `tasks/` directory to trigger async execution:
//
// ```rust,no_run
// use rustcode::task::TaskFile;
//
// let task_file = TaskFile {
//     id: "feat-001".to_string(),
//     repo: "owner/repo".to_string(),
//     description: "Add new feature".to_string(),
//     steps: vec!["Create src/lib.rs".to_string()],
//     branch: "feat/new".to_string(),
//     labels: vec!["enhancement".to_string()],
//     auto_merge: true,
// };
// ```

pub mod grouping;
pub mod models;
pub mod schema;

// Re-export commonly used types
pub use grouping::{
    GroupingStrategy, filter_by_priority, filter_ready_groups, get_next_group, get_top_groups,
    group_tasks, tasks_are_similar,
};

pub use models::{
    Task, TaskCategory, TaskGroup, TaskSource, TaskStats, TaskStatus, assign_group,
    check_duplicate, create_task, get_pending_tasks, get_task, get_task_stats, get_tasks_by_status,
    mark_task_failed, update_task_analysis, update_task_status,
};

pub use schema::{StepResult, TaskFile, TaskResult};
