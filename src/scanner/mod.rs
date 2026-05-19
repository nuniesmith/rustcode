// Scanner module
//
// Provides repository scanning functionality for TODOs, file analysis, and directory trees.

pub mod compat;
// `enhanced` is the audit-flow scanner that wraps `compat::Scanner` with
// `TestRunner` + `ContextBuilder` for deeper analysis. Folded under
// `scanner/` in RC-CLEANUP-F — used to live at `src/enhanced_scanner.rs`.
pub mod enhanced;
pub mod github;

// Re-export main types and functions
pub use github::{
    DetectedTodo, GitHubRepo, ScanResult, TreeNode, build_dir_tree, fetch_user_repos, get_dir_tree,
    get_unanalyzed_files, save_dir_tree, save_file_analysis, scan_directory_for_todos,
    scan_repo_for_todos, sync_repos_to_db,
};

// Re-export compatibility scanner
pub use compat::Scanner;
pub use enhanced::EnhancedScanner;
