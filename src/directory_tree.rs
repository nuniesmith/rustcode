//! Directory tree builder for visualizing codebase structure with audit tags
//!
//! Creates a hierarchical view of the codebase with:
//! - Tag distribution
//! - Issue counts
//! - Code statistics
//! - Age/status indicators

use crate::error::Result;
use crate::tag_schema::{
    CodeStatus, DirectoryNode, IssuesSummary, NodeStats, NodeType, SimpleIssueDetector,
};
use crate::types::AuditTag;
use regex::Regex;
use std::fs;
use std::path::{Path, PathBuf};

/// Directory tree builder
pub struct DirectoryTreeBuilder {
    /// Root path
    root: PathBuf,
    /// Issue detector
    issue_detector: SimpleIssueDetector,
    /// Exclude patterns
    exclude_patterns: Vec<String>,
}

impl DirectoryTreeBuilder {
    /// Create a new directory tree builder
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            issue_detector: SimpleIssueDetector::new(),
            exclude_patterns: vec![
                "target".to_string(),
                "node_modules".to_string(),
                ".git".to_string(),
                "__pycache__".to_string(),
                ".pytest_cache".to_string(),
                "build".to_string(),
                "dist".to_string(),
            ],
        }
    }

    /// Build the directory tree
    pub fn build(&self) -> Result<DirectoryNode> {
        self.build_node(&self.root)
    }

    /// Build the tree with tags
    pub fn build_with_tags(&self, tags: &[AuditTag]) -> Result<DirectoryNode> {
        let mut node = self.build_node(&self.root)?;
        self.attach_tags(&mut node, tags);
        Ok(node)
    }

    /// Build a node for a path
    fn build_node(&self, path: &Path) -> Result<DirectoryNode> {
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_string();

        let node_type = if path.is_dir() {
            NodeType::Directory
        } else {
            NodeType::File
        };

        let mut node = DirectoryNode {
            name,
            path: path.to_path_buf(),
            node_type,
            status: None,
            tags: Vec::new(),
            stats: NodeStats::default(),
            children: Vec::new(),
            issues: IssuesSummary::default(),
        };

        match node_type {
            NodeType::File => {
                self.analyze_file(&mut node)?;
            }
            NodeType::Directory => {
                self.analyze_directory(&mut node)?;
            }
        }

        Ok(node)
    }

    /// Analyze a file node
    fn analyze_file(&self, node: &mut DirectoryNode) -> Result<()> {
        if !self.is_source_file(&node.path) {
            return Ok(());
        }

        let content = fs::read_to_string(&node.path)?;
        let lines: Vec<&str> = content.lines().collect();

        node.stats.lines_of_code = lines.len();
        node.stats.file_count = 1;

        // Count TODOs and FIXMEs
        for line in &lines {
            let line_lower = line.to_lowercase();
            if line_lower.contains("todo") {
                node.stats.todos += 1;
            }
            if line_lower.contains("fixme") {
                node.stats.fixmes += 1;
            }
        }

        // Detect simple issues
        self.detect_issues(&content, &mut node.issues);

        // Get file modification time
        if let Ok(metadata) = fs::metadata(&node.path) {
            if let Ok(modified) = metadata.modified() {
                if let Ok(duration) = modified.duration_since(std::time::UNIX_EPOCH) {
                    node.stats.last_modified = Some(duration.as_secs() as i64);
                }
            }
        }

        Ok(())
    }

    /// Analyze a directory node
    fn analyze_directory(&self, node: &mut DirectoryNode) -> Result<()> {
        let mut children = Vec::new();

        if let Ok(entries) = fs::read_dir(&node.path) {
            for entry in entries.flatten() {
                let path = entry.path();

                // Skip excluded directories
                if self.should_exclude(&path) {
                    continue;
                }

                if let Ok(child) = self.build_node(&path) {
                    children.push(child);
                }
            }
        }

        // Sort children: directories first, then files, alphabetically
        children.sort_by(|a, b| match (a.node_type, b.node_type) {
            (NodeType::Directory, NodeType::File) => std::cmp::Ordering::Less,
            (NodeType::File, NodeType::Directory) => std::cmp::Ordering::Greater,
            _ => a.name.cmp(&b.name),
        });

        // Aggregate stats from children
        for child in &children {
            node.stats.file_count += child.stats.file_count;
            node.stats.lines_of_code += child.stats.lines_of_code;
            node.stats.todos += child.stats.todos;
            node.stats.fixmes += child.stats.fixmes;
            node.stats.audit_tags += child.stats.audit_tags;

            node.issues.critical += child.issues.critical;
            node.issues.high += child.issues.high;
            node.issues.medium += child.issues.medium;
            node.issues.low += child.issues.low;

            // Track most recent modification
            if let (Some(child_mod), node_mod) =
                (child.stats.last_modified, node.stats.last_modified)
            {
                node.stats.last_modified = Some(node_mod.unwrap_or(0).max(child_mod));
            } else if child.stats.last_modified.is_some() {
                node.stats.last_modified = child.stats.last_modified;
            }
        }

        node.children = children;
        Ok(())
    }

    /// Detect simple issues in file content
    fn detect_issues(&self, content: &str, issues: &mut IssuesSummary) {
        for pattern in self.issue_detector.patterns() {
            if let Ok(re) = Regex::new(pattern.pattern) {
                let count = re.find_iter(content).count();
                if count > 0 {
                    match pattern.severity {
                        "critical" => issues.critical += count,
                        "high" => issues.high += count,
                        "medium" => issues.medium += count,
                        "low" | "info" => issues.low += count,
                        _ => {}
                    }
                }
            }
        }
    }

    /// Attach audit tags to the tree
    fn attach_tags(&self, node: &mut DirectoryNode, tags: &[AuditTag]) {
        match node.node_type {
            NodeType::File => {
                // Find tags for this file
                let file_tags: Vec<_> = tags.iter().filter(|t| t.file == node.path).collect();

                node.stats.audit_tags = file_tags.len();
                node.tags = file_tags
                    .iter()
                    .map(|t| format!("{:?}: {}", t.tag_type, t.value))
                    .collect();

                // Determine status from tags
                if let Some(tag) = file_tags.first() {
                    node.status = Some(CodeStatus::from_tag_value(&tag.value));
                }
            }
            NodeType::Directory => {
                // Recursively attach tags to children
                for child in &mut node.children {
                    self.attach_tags(child, tags);
                }

                // Aggregate tag count from children
                node.stats.audit_tags = node.children.iter().map(|c| c.stats.audit_tags).sum();
            }
        }
    }

    /// Check if path should be excluded
    fn should_exclude(&self, path: &Path) -> bool {
        let path_str = path.to_string_lossy();
        self.exclude_patterns
            .iter()
            .any(|pattern| path_str.contains(pattern))
    }

    /// Check if file is a source file
    fn is_source_file(&self, path: &Path) -> bool {
        if !path.is_file() {
            return false;
        }

        let extension = path.extension().and_then(|e| e.to_str());
        matches!(
            extension,
            Some("rs")
                | Some("py")
                | Some("kt")
                | Some("kts")
                | Some("swift")
                | Some("ts")
                | Some("tsx")
                | Some("js")
                | Some("toml")
        )
    }

    /// Generate a summary report
    pub fn generate_summary(&self, node: &DirectoryNode) -> TreeSummary {
        TreeSummary {
            total_files: node.stats.file_count,
            total_lines: node.stats.lines_of_code,
            total_todos: node.stats.todos,
            total_fixmes: node.stats.fixmes,
            total_tags: node.stats.audit_tags,
            total_issues: node.issues.total(),
            critical_issues: node.issues.critical,
            high_issues: node.issues.high,
            directories_analyzed: self.count_directories(node),
        }
    }

    /// Count directories in tree
    fn count_directories(&self, node: &DirectoryNode) -> usize {
        match node.node_type {
            NodeType::File => 0,
            NodeType::Directory => {
                1 + node
                    .children
                    .iter()
                    .map(|c| self.count_directories(c))
                    .sum::<usize>()
            }
        }
    }

    /// Find nodes with most issues
    pub fn find_hotspots(&self, node: &DirectoryNode, limit: usize) -> Vec<Hotspot> {
        let mut hotspots = Vec::new();
        self.collect_hotspots(node, &mut hotspots);

        // Sort by total issues (descending)
        hotspots.sort_by(|a, b| b.total_issues.cmp(&a.total_issues));
        hotspots.truncate(limit);
        hotspots
    }

    /// Collect hotspots recursively
    fn collect_hotspots(&self, node: &DirectoryNode, hotspots: &mut Vec<Hotspot>) {
        let total = node.issues.total();
        if total > 0 {
            hotspots.push(Hotspot {
                path: node.path.clone(),
                name: node.name.clone(),
                node_type: node.node_type,
                total_issues: total,
                critical: node.issues.critical,
                high: node.issues.high,
                lines_of_code: node.stats.lines_of_code,
            });
        }

        for child in &node.children {
            self.collect_hotspots(child, hotspots);
        }
    }

    /// Generate ASCII tree visualization
    pub fn to_ascii_tree(&self, node: &DirectoryNode, max_depth: usize) -> String {
        let mut output = String::new();
        self.render_ascii_node(node, &mut output, "", true, 0, max_depth);
        output
    }

    /// Render a node as ASCII tree
    fn render_ascii_node(
        &self,
        node: &DirectoryNode,
        output: &mut String,
        prefix: &str,
        is_last: bool,
        depth: usize,
        max_depth: usize,
    ) {
        if depth > max_depth {
            return;
        }

        // Node connector
        let connector = if is_last { "└── " } else { "├── " };

        // Node icon
        let icon = match node.node_type {
            NodeType::Directory => "📁",
            NodeType::File => "📄",
        };

        // Node info
        let mut info = format!("{}{} {}", prefix, connector, icon);
        info.push_str(&node.name);

        // Add stats
        if node.stats.lines_of_code > 0 {
            info.push_str(&format!(" [{} LOC]", node.stats.lines_of_code));
        }

        // Add issue indicators
        if node.issues.critical > 0 {
            info.push_str(&format!(" 🔴{}", node.issues.critical));
        }
        if node.issues.high > 0 {
            info.push_str(&format!(" 🟠{}", node.issues.high));
        }

        // Add tags indicator
        if node.stats.audit_tags > 0 {
            info.push_str(&format!(" 🏷️{}", node.stats.audit_tags));
        }

        output.push_str(&info);
        output.push('\n');

        // Render children
        if let NodeType::Directory = node.node_type {
            let child_prefix = format!("{}{}", prefix, if is_last { "    " } else { "│   " });

            for (i, child) in node.children.iter().enumerate() {
                let is_last_child = i == node.children.len() - 1;
                self.render_ascii_node(
                    child,
                    output,
                    &child_prefix,
                    is_last_child,
                    depth + 1,
                    max_depth,
                );
            }
        }
    }
}

/// Tree summary statistics
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TreeSummary {
    pub total_files: usize,
    pub total_lines: usize,
    pub total_todos: usize,
    pub total_fixmes: usize,
    pub total_tags: usize,
    pub total_issues: usize,
    pub critical_issues: usize,
    pub high_issues: usize,
    pub directories_analyzed: usize,
}

/// Code hotspot (file or directory with many issues)
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Hotspot {
    pub path: PathBuf,
    pub name: String,
    pub node_type: NodeType,
    pub total_issues: usize,
    pub critical: usize,
    pub high: usize,
    pub lines_of_code: usize,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    #[test]
    fn test_build_tree() {
        let temp = TempDir::new().unwrap();
        let root = temp.path();

        // Create test structure
        fs::create_dir(root.join("src")).unwrap();
        let mut file = fs::File::create(root.join("src/main.rs")).unwrap();
        writeln!(file, "fn main() {{\n    println!(\"Hello\");\n}}").unwrap();

        let builder = DirectoryTreeBuilder::new(root);
        let tree = builder.build().unwrap();

        assert_eq!(tree.node_type, NodeType::Directory);
        assert!(tree.stats.file_count > 0);
    }

    #[test]
    fn test_detect_issues() {
        let content = r#"
fn foo() {
    let x = some_func().unwrap();
    println!("Debug: {}", x);
    // TODO: Add error handling
}
"#;

        let temp = TempDir::new().unwrap();
        let file_path = temp.path().join("test.rs");
        fs::write(&file_path, content).unwrap();

        let builder = DirectoryTreeBuilder::new(temp.path());
        let node = builder.build_node(&file_path).unwrap();

        // Should detect unwrap, println, and TODO
        assert!(node.issues.medium > 0 || node.issues.low > 0);
        assert!(node.stats.todos > 0);
    }

    #[test]
    fn test_ascii_tree() {
        let temp = TempDir::new().unwrap();
        let root = temp.path();

        fs::create_dir(root.join("src")).unwrap();
        fs::write(root.join("src/main.rs"), "fn main() {}").unwrap();

        let builder = DirectoryTreeBuilder::new(root);
        let tree = builder.build().unwrap();
        let ascii = builder.to_ascii_tree(&tree, 3);

        assert!(ascii.contains("📁"));
        assert!(ascii.contains("main.rs"));
    }
}
