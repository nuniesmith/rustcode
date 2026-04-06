// CLI module
//
// Provides command-line interface functionality for queue, scan, and report operations.

pub mod github_commands;
pub mod queue_commands;
pub mod research_backup_commands;
pub mod task_commands;

// Re-export command types
pub use github_commands::{GithubCommands, handle_github_command};

pub use queue_commands::{
    QueueCommands, ReportCommands, ScanCommands, handle_queue_command, handle_report_command,
    handle_scan_command,
};

pub use research_backup_commands::{
    BackupCommands, ResearchCommands, handle_backup_command, handle_research_command,
};

pub use task_commands::{TaskCommands, handle_task_command};
