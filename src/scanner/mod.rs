//! Scanner module
//!
//! Provides repository scanning functionality for TODOs, file analysis, and directory trees.

pub mod compat;
pub mod github;

// Re-export main types and functions
pub use github::{
    build_dir_tree, fetch_user_repos, get_dir_tree, get_unanalyzed_files, save_dir_tree,
    save_file_analysis, scan_directory_for_todos, scan_repo_for_todos, sync_repos_to_db,
    DetectedTodo, GitHubRepo, ScanResult, TreeNode,
};

// Re-export compatibility scanner
pub use compat::Scanner;
