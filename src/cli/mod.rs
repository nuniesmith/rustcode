//! CLI module
//!
//! Provides command-line interface functionality for queue, scan, and report operations.

pub mod github_commands;
pub mod queue_commands;
pub mod research_backup_commands;
pub mod task_commands;

// Re-export command types
pub use github_commands::{handle_github_command, GithubCommands};

pub use queue_commands::{
    handle_queue_command, handle_report_command, handle_scan_command, QueueCommands,
    ReportCommands, ScanCommands,
};

pub use research_backup_commands::{
    handle_backup_command, handle_research_command, BackupCommands, ResearchCommands,
};

pub use task_commands::{handle_task_command, TaskCommands};
