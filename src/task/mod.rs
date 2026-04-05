// Simplified Task Management System
//
// This module provides a consolidated task management system that replaces
// the previous queue-based architecture with a simpler, more maintainable approach.
//
// ## Overview
//
// - **Models**: Core task types and database operations
// - **Grouping**: Smart task grouping for efficient batch processing
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

pub mod grouping;
pub mod models;

// Re-export commonly used types
pub use grouping::{
    filter_by_priority, filter_ready_groups, get_next_group, get_top_groups, group_tasks,
    tasks_are_similar, GroupingStrategy,
};

pub use models::{
    assign_group, check_duplicate, create_task, get_pending_tasks, get_task, get_task_stats,
    get_tasks_by_status, mark_task_failed, update_task_analysis, update_task_status, Task,
    TaskCategory, TaskGroup, TaskSource, TaskStats, TaskStatus,
};
