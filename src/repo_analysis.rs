//! # Repository Analysis Module
//!
//! Provides directory tree caching and file metadata extraction for tracked repositories.
//!
//! ## Features
//!
//! - Directory tree caching with git-aware filtering
//! - File metadata extraction (size, language, modified date)
//! - Incremental updates (only scan changed files)
//! - Language detection based on file extensions
//! - Repository statistics and metrics
//!
//! ## Usage
//!
//! ```rust,no_run
//! use rustcode::repo_analysis::RepoAnalyzer;
//!
//! #[tokio::main]
//! async fn main() -> anyhow::Result<()> {
//!     let analyzer = RepoAnalyzer::new("/path/to/repo");
//!     let tree = analyzer.build_tree().await?;
//!
//!     println!("Total files: {}", tree.total_files);
//!     println!("Total size: {} bytes", tree.total_size);
//!
//!     Ok(())
//! }
//! ```

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

/// Repository analyzer for building directory trees and extracting metadata
pub struct RepoAnalyzer {
    /// Repository root path
    root: PathBuf,
    /// Exclude patterns (defaults to common build/cache dirs)
    exclude_patterns: Vec<String>,
}

/// A node in the directory tree
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TreeNode {
    /// Node name (file or directory name)
    pub name: String,
    /// Full path
    pub path: PathBuf,
    /// Node type
    pub node_type: RepoNodeType,
    /// File metadata (if file)
    pub metadata: Option<FileMetadata>,
    /// Child nodes (if directory)
    pub children: Vec<TreeNode>,
}

/// Node type in the tree
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RepoNodeType {
    File,
    Directory,
}

/// File metadata
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileMetadata {
    /// File size in bytes
    pub size: u64,
    /// Detected programming language
    pub language: Option<String>,
    /// Last modified timestamp
    pub modified: DateTime<Utc>,
    /// Number of lines (for text files)
    pub lines: Option<usize>,
    /// Is binary file
    pub is_binary: bool,
}

/// Repository tree summary
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepoTree {
    /// Repository path
    pub repo_path: PathBuf,
    /// Root node
    pub root: TreeNode,
    /// Total number of files
    pub total_files: usize,
    /// Total number of directories
    pub total_dirs: usize,
    /// Total size in bytes
    pub total_size: u64,
    /// Language breakdown
    pub languages: HashMap<String, LanguageStats>,
    /// Generated timestamp
    pub generated_at: DateTime<Utc>,
}

/// Statistics for a programming language
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LanguageStats {
    /// Number of files
    pub file_count: usize,
    /// Total size in bytes
    pub total_size: u64,
    /// Total lines of code
    pub total_lines: usize,
}

impl RepoAnalyzer {
    /// Create a new repository analyzer
    pub fn new<P: Into<PathBuf>>(root: P) -> Self {
        Self {
            root: root.into(),
            exclude_patterns: vec![
                "target".to_string(),
                "node_modules".to_string(),
                ".git".to_string(),
                "__pycache__".to_string(),
                ".pytest_cache".to_string(),
                "build".to_string(),
                "dist".to_string(),
                ".idea".to_string(),
                ".vscode".to_string(),
                ".next".to_string(),
                "vendor".to_string(),
                "deps".to_string(),
            ],
        }
    }

    /// Add custom exclude patterns
    pub fn with_exclude_patterns(mut self, patterns: Vec<String>) -> Self {
        self.exclude_patterns.extend(patterns);
        self
    }

    /// Build the directory tree asynchronously
    pub async fn build_tree(&self) -> Result<RepoTree> {
        let root = self.root.clone();
        let exclude_patterns = self.exclude_patterns.clone();

        // Run blocking I/O in a spawn_blocking task
        let result =
            tokio::task::spawn_blocking(move || Self::build_tree_sync(&root, &exclude_patterns))
                .await
                .context("Failed to spawn blocking task")??;

        Ok(result)
    }

    /// Build the directory tree synchronously
    fn build_tree_sync(root: &Path, exclude_patterns: &[String]) -> Result<RepoTree> {
        let root_node = Self::build_node_sync(root, exclude_patterns)?;

        let mut total_files = 0;
        let mut total_dirs = 0;
        let mut total_size = 0;
        let mut languages: HashMap<String, LanguageStats> = HashMap::new();

        Self::collect_stats(
            &root_node,
            &mut total_files,
            &mut total_dirs,
            &mut total_size,
            &mut languages,
        );

        Ok(RepoTree {
            repo_path: root.to_path_buf(),
            root: root_node,
            total_files,
            total_dirs,
            total_size,
            languages,
            generated_at: Utc::now(),
        })
    }

    /// Build a tree node synchronously
    fn build_node_sync(path: &Path, exclude_patterns: &[String]) -> Result<TreeNode> {
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_string();

        if path.is_file() {
            let metadata = Self::extract_file_metadata(path)?;
            Ok(TreeNode {
                name,
                path: path.to_path_buf(),
                node_type: RepoNodeType::File,
                metadata: Some(metadata),
                children: Vec::new(),
            })
        } else {
            let mut children = Vec::new();

            if let Ok(entries) = fs::read_dir(path) {
                for entry in entries.flatten() {
                    let entry_path = entry.path();
                    let entry_name = entry_path
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or("");

                    // Skip excluded directories
                    if exclude_patterns.contains(&entry_name.to_string()) {
                        continue;
                    }

                    // Skip hidden files/directories (except .gitignore, .env.example, etc.)
                    if entry_name.starts_with('.') && !Self::is_important_dotfile(entry_name) {
                        continue;
                    }

                    if let Ok(child_node) = Self::build_node_sync(&entry_path, exclude_patterns) {
                        children.push(child_node);
                    }
                }
            }

            // Sort children: directories first, then files, alphabetically
            children.sort_by(|a, b| match (a.node_type, b.node_type) {
                (RepoNodeType::Directory, RepoNodeType::File) => std::cmp::Ordering::Less,
                (RepoNodeType::File, RepoNodeType::Directory) => std::cmp::Ordering::Greater,
                _ => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
            });

            Ok(TreeNode {
                name,
                path: path.to_path_buf(),
                node_type: RepoNodeType::Directory,
                metadata: None,
                children,
            })
        }
    }

    /// Extract file metadata
    fn extract_file_metadata(path: &Path) -> Result<FileMetadata> {
        let metadata = fs::metadata(path)
            .context(format!("Failed to read metadata for {}", path.display()))?;

        let size = metadata.len();
        let modified = metadata
            .modified()
            .ok()
            .and_then(|t| DateTime::<Utc>::from(t).into())
            .unwrap_or_else(Utc::now);

        let language = Self::detect_language(path);
        let is_binary = Self::is_binary_file(path);

        let lines = if !is_binary && size < 10_000_000 {
            // Only count lines for text files under 10MB
            Self::count_lines(path).ok()
        } else {
            None
        };

        Ok(FileMetadata {
            size,
            language,
            modified,
            lines,
            is_binary,
        })
    }

    /// Detect programming language from file extension
    fn detect_language(path: &Path) -> Option<String> {
        let extension = path.extension()?.to_str()?;

        let language = match extension.to_lowercase().as_str() {
            "rs" => "Rust",
            "py" => "Python",
            "js" => "JavaScript",
            "ts" => "TypeScript",
            "jsx" => "JavaScript (JSX)",
            "tsx" => "TypeScript (TSX)",
            "java" => "Java",
            "kt" => "Kotlin",
            "go" => "Go",
            "c" => "C",
            "cpp" | "cc" | "cxx" => "C++",
            "h" | "hpp" => "C/C++ Header",
            "cs" => "C#",
            "rb" => "Ruby",
            "php" => "PHP",
            "swift" => "Swift",
            "scala" => "Scala",
            "r" => "R",
            "m" => "Objective-C",
            "sh" | "bash" => "Shell",
            "sql" => "SQL",
            "html" | "htm" => "HTML",
            "css" => "CSS",
            "scss" | "sass" => "SCSS/Sass",
            "json" => "JSON",
            "yaml" | "yml" => "YAML",
            "toml" => "TOML",
            "xml" => "XML",
            "md" | "markdown" => "Markdown",
            "txt" => "Text",
            "dockerfile" => "Dockerfile",
            _ => return None,
        };

        Some(language.to_string())
    }

    /// Check if a file is binary
    fn is_binary_file(path: &Path) -> bool {
        // Check by extension first
        if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
            let binary_extensions = [
                "png", "jpg", "jpeg", "gif", "bmp", "ico", "svg", "pdf", "zip", "tar", "gz", "bz2",
                "xz", "7z", "exe", "dll", "so", "dylib", "bin", "wasm", "class", "jar", "mp3",
                "mp4", "avi", "mov", "webm", "ttf", "woff", "woff2", "eot",
            ];

            if binary_extensions.contains(&ext.to_lowercase().as_str()) {
                return true;
            }
        }

        // For small files, check content
        if let Ok(content) = fs::read(path) {
            if content.len() > 8192 {
                // Only check first 8KB
                return content[..8192].contains(&0);
            }
            return content.contains(&0);
        }

        false
    }

    /// Count lines in a text file
    fn count_lines(path: &Path) -> Result<usize> {
        let content = fs::read_to_string(path).context(format!(
            "Failed to read file for line counting: {}",
            path.display()
        ))?;
        Ok(content.lines().count())
    }

    /// Check if a dotfile is important and should be included
    fn is_important_dotfile(name: &str) -> bool {
        matches!(
            name,
            ".gitignore" | ".env.example" | ".dockerignore" | ".editorconfig" | ".npmrc"
        )
    }

    /// Collect statistics from the tree
    fn collect_stats(
        node: &TreeNode,
        total_files: &mut usize,
        total_dirs: &mut usize,
        total_size: &mut u64,
        languages: &mut HashMap<String, LanguageStats>,
    ) {
        match node.node_type {
            RepoNodeType::File => {
                *total_files += 1;
                if let Some(ref metadata) = node.metadata {
                    *total_size += metadata.size;

                    if let Some(ref lang) = metadata.language {
                        let stats = languages.entry(lang.clone()).or_insert(LanguageStats {
                            file_count: 0,
                            total_size: 0,
                            total_lines: 0,
                        });
                        stats.file_count += 1;
                        stats.total_size += metadata.size;
                        if let Some(lines) = metadata.lines {
                            stats.total_lines += lines;
                        }
                    }
                }
            }
            RepoNodeType::Directory => {
                *total_dirs += 1;
                for child in &node.children {
                    Self::collect_stats(child, total_files, total_dirs, total_size, languages);
                }
            }
        }
    }

    /// Get a flat list of all files in the tree
    pub fn get_all_files(tree: &RepoTree) -> Vec<&TreeNode> {
        let mut files = Vec::new();
        Self::collect_files(&tree.root, &mut files);
        files
    }

    /// Recursively collect all file nodes
    fn collect_files<'a>(node: &'a TreeNode, files: &mut Vec<&'a TreeNode>) {
        match node.node_type {
            RepoNodeType::File => files.push(node),
            RepoNodeType::Directory => {
                for child in &node.children {
                    Self::collect_files(child, files);
                }
            }
        }
    }

    /// Get files filtered by language
    pub fn get_files_by_language<'a>(tree: &'a RepoTree, language: &str) -> Vec<&'a TreeNode> {
        Self::get_all_files(tree)
            .into_iter()
            .filter(|node| {
                node.metadata
                    .as_ref()
                    .and_then(|m| m.language.as_ref())
                    .map(|l| l == language)
                    .unwrap_or(false)
            })
            .collect()
    }

    /// Get largest files in the repository
    pub fn get_largest_files(tree: &RepoTree, limit: usize) -> Vec<&TreeNode> {
        let mut files = Self::get_all_files(tree);
        files.sort_by(|a, b| {
            let size_a = a.metadata.as_ref().map(|m| m.size).unwrap_or(0);
            let size_b = b.metadata.as_ref().map(|m| m.size).unwrap_or(0);
            size_b.cmp(&size_a)
        });
        files.truncate(limit);
        files
    }

    /// Get recently modified files
    pub fn get_recently_modified(tree: &RepoTree, limit: usize) -> Vec<&TreeNode> {
        let mut files = Self::get_all_files(tree);
        files.sort_by(|a, b| {
            let time_a = a
                .metadata
                .as_ref()
                .map(|m| m.modified)
                .unwrap_or_else(|| DateTime::from_timestamp(0, 0).unwrap());
            let time_b = b
                .metadata
                .as_ref()
                .map(|m| m.modified)
                .unwrap_or_else(|| DateTime::from_timestamp(0, 0).unwrap());
            time_b.cmp(&time_a)
        });
        files.truncate(limit);
        files
    }
}

impl RepoTree {
    /// Print a tree view to stdout
    pub fn print_tree(&self, max_depth: Option<usize>) {
        println!("Repository: {}", self.repo_path.display());
        println!(
            "Generated: {}",
            self.generated_at.format("%Y-%m-%d %H:%M:%S")
        );
        println!();
        self.print_node(&self.root, "", true, 0, max_depth);
        println!();
        self.print_summary();
    }

    /// Print a single node
    fn print_node(
        &self,
        node: &TreeNode,
        prefix: &str,
        is_last: bool,
        depth: usize,
        max_depth: Option<usize>,
    ) {
        if let Some(max) = max_depth {
            if depth >= max {
                return;
            }
        }

        let connector = if is_last { "└── " } else { "├── " };
        let icon = match node.node_type {
            RepoNodeType::Directory => "📁",
            RepoNodeType::File => "📄",
        };

        print!("{}{}{} {}", prefix, connector, icon, node.name);

        if let Some(ref metadata) = node.metadata {
            if let Some(ref lang) = metadata.language {
                print!(" [{}]", lang);
            }
            if metadata.size > 1024 * 1024 {
                print!(" ({:.2} MB)", metadata.size as f64 / (1024.0 * 1024.0));
            } else if metadata.size > 1024 {
                print!(" ({:.2} KB)", metadata.size as f64 / 1024.0);
            }
        }
        println!();

        if node.node_type == RepoNodeType::Directory {
            let new_prefix = format!("{}{}", prefix, if is_last { "    " } else { "│   " });
            let child_count = node.children.len();
            for (i, child) in node.children.iter().enumerate() {
                self.print_node(
                    child,
                    &new_prefix,
                    i == child_count - 1,
                    depth + 1,
                    max_depth,
                );
            }
        }
    }

    /// Print summary statistics
    fn print_summary(&self) {
        println!("Summary:");
        println!("  Files: {}", self.total_files);
        println!("  Directories: {}", self.total_dirs);
        println!("  Total size: {}", Self::format_size(self.total_size));
        println!();

        if !self.languages.is_empty() {
            println!("Languages:");
            let mut langs: Vec<_> = self.languages.iter().collect();
            langs.sort_by(|a, b| b.1.file_count.cmp(&a.1.file_count));

            for (lang, stats) in langs.iter().take(10) {
                println!(
                    "  {} - {} files, {}, {} lines",
                    lang,
                    stats.file_count,
                    Self::format_size(stats.total_size),
                    stats.total_lines
                );
            }
        }
    }

    /// Format bytes as human-readable size
    fn format_size(bytes: u64) -> String {
        if bytes > 1024 * 1024 * 1024 {
            format!("{:.2} GB", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
        } else if bytes > 1024 * 1024 {
            format!("{:.2} MB", bytes as f64 / (1024.0 * 1024.0))
        } else if bytes > 1024 {
            format!("{:.2} KB", bytes as f64 / 1024.0)
        } else {
            format!("{} bytes", bytes)
        }
    }

    /// Export tree to JSON
    pub fn to_json(&self) -> Result<String> {
        serde_json::to_string_pretty(self).context("Failed to serialize tree to JSON")
    }

    /// Save tree to a file
    pub fn save_to_file<P: AsRef<Path>>(&self, path: P) -> Result<()> {
        let json = self.to_json()?;
        fs::write(path.as_ref(), json).context(format!(
            "Failed to write tree to {}",
            path.as_ref().display()
        ))?;
        Ok(())
    }

    /// Load tree from a file
    pub fn load_from_file<P: AsRef<Path>>(path: P) -> Result<Self> {
        let json = fs::read_to_string(path.as_ref()).context(format!(
            "Failed to read tree from {}",
            path.as_ref().display()
        ))?;
        let tree: RepoTree =
            serde_json::from_str(&json).context("Failed to deserialize tree from JSON")?;
        Ok(tree)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_language_detection() {
        assert_eq!(
            RepoAnalyzer::detect_language(Path::new("test.rs")),
            Some("Rust".to_string())
        );
        assert_eq!(
            RepoAnalyzer::detect_language(Path::new("test.py")),
            Some("Python".to_string())
        );
        assert_eq!(
            RepoAnalyzer::detect_language(Path::new("test.js")),
            Some("JavaScript".to_string())
        );
        assert_eq!(
            RepoAnalyzer::detect_language(Path::new("test.unknown")),
            None
        );
    }

    #[test]
    fn test_binary_detection() {
        assert!(RepoAnalyzer::is_binary_file(Path::new("image.png")));
        assert!(RepoAnalyzer::is_binary_file(Path::new("archive.zip")));
        assert!(!RepoAnalyzer::is_binary_file(Path::new("code.rs")));
    }

    #[test]
    fn test_important_dotfiles() {
        assert!(RepoAnalyzer::is_important_dotfile(".gitignore"));
        assert!(RepoAnalyzer::is_important_dotfile(".env.example"));
        assert!(!RepoAnalyzer::is_important_dotfile(".DS_Store"));
        assert!(!RepoAnalyzer::is_important_dotfile(".cache"));
    }
}
