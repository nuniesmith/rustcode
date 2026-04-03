//! Tag scanner for detecting audit annotations in source code

use crate::error::{AuditError, Result};
use crate::types::{AuditTag, AuditTagType};
use regex::Regex;
use std::fs;
use std::path::Path;
use walkdir::WalkDir;

/// Scanner for audit tags in source code
pub struct TagScanner {
    /// Regex patterns for each tag type
    patterns: Vec<(AuditTagType, Regex)>,
}

impl TagScanner {
    /// Create a new tag scanner
    pub fn new() -> Result<Self> {
        let patterns = vec![
            (
                AuditTagType::Tag,
                Regex::new(r"@audit-tag:\s*(.+)")
                    .map_err(|e| AuditError::other(format!("Invalid regex: {}", e)))?,
            ),
            (
                AuditTagType::Todo,
                Regex::new(r"@audit-todo:\s*(.+)")
                    .map_err(|e| AuditError::other(format!("Invalid regex: {}", e)))?,
            ),
            (
                AuditTagType::Freeze,
                Regex::new(r"@audit-freeze")
                    .map_err(|e| AuditError::other(format!("Invalid regex: {}", e)))?,
            ),
            (
                AuditTagType::Review,
                Regex::new(r"@audit-review:\s*(.+)")
                    .map_err(|e| AuditError::other(format!("Invalid regex: {}", e)))?,
            ),
            (
                AuditTagType::Security,
                Regex::new(r"@audit-security:\s*(.+)")
                    .map_err(|e| AuditError::other(format!("Invalid regex: {}", e)))?,
            ),
        ];

        Ok(Self { patterns })
    }

    /// Scan a file for audit tags
    pub fn scan_file(&self, path: &Path) -> Result<Vec<AuditTag>> {
        // Skip files that define the tag system itself
        if !self.should_scan_for_tags(path) {
            return Ok(Vec::new());
        }

        let content = fs::read_to_string(path)?;
        let mut tags = Vec::new();

        for (line_num, line) in content.lines().enumerate() {
            for (tag_type, pattern) in &self.patterns {
                if let Some(captures) = pattern.captures(line) {
                    let value = if captures.len() > 1 {
                        captures.get(1).map(|m| m.as_str().trim().to_string())
                    } else {
                        None
                    };

                    let tag = AuditTag {
                        tag_type: *tag_type,
                        file: path.to_path_buf(),
                        line: line_num + 1,
                        value: value.unwrap_or_default(),
                        context: self.extract_context(&content, line_num),
                    };

                    tags.push(tag);
                }
            }
        }

        Ok(tags)
    }

    /// Scan a directory recursively for audit tags
    pub fn scan_directory(&self, dir: &Path) -> Result<Vec<AuditTag>> {
        let mut all_tags = Vec::new();

        for entry in WalkDir::new(dir)
            .follow_links(false)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            let path = entry.path();

            // Skip non-source files
            if !self.is_source_file(path) {
                continue;
            }

            // Skip excluded directories
            if self.should_skip(path) {
                continue;
            }

            if let Ok(tags) = self.scan_file(path) {
                all_tags.extend(tags);
            }
        }

        Ok(all_tags)
    }

    /// Extract context around a line
    fn extract_context(&self, content: &str, line_num: usize) -> Option<String> {
        let lines: Vec<&str> = content.lines().collect();
        let start = line_num.saturating_sub(2);
        let end = (line_num + 3).min(lines.len());

        if start < lines.len() {
            let context = lines[start..end].join("\n");
            Some(context)
        } else {
            None
        }
    }

    /// Check if a file is a source file we should scan
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
        )
    }

    /// Check if a path should be skipped
    fn should_skip(&self, path: &Path) -> bool {
        let path_str = path.to_string_lossy();
        path_str.contains("target/")
            || path_str.contains("node_modules/")
            || path_str.contains(".git/")
            || path_str.contains("__pycache__")
            || path_str.contains(".pytest_cache")
            || path_str.contains("build/")
            || path_str.contains("dist/")
    }

    /// Check if a file should be scanned for tags (exclude tag definition files)
    fn should_scan_for_tags(&self, path: &Path) -> bool {
        let path_str = path.to_string_lossy();

        // Don't scan files that define the tag system
        if path_str.contains("tags.rs")
            || path_str.contains("types.rs")
            || path_str.contains("/test")
            || path_str.contains("_test.rs")
            || path_str.ends_with("_test.py")
            || path_str.contains("test_")
            || path_str.contains("/tests/")
        {
            return false;
        }

        true
    }

    /// Group tags by type
    pub fn group_by_type<'a>(
        &self,
        tags: &'a [AuditTag],
    ) -> std::collections::HashMap<AuditTagType, Vec<&'a AuditTag>> {
        let mut grouped = std::collections::HashMap::new();

        for tag in tags {
            grouped
                .entry(tag.tag_type)
                .or_insert_with(Vec::new)
                .push(tag);
        }

        grouped
    }

    /// Get all TODO tags
    pub fn get_todos<'a>(&self, tags: &'a [AuditTag]) -> Vec<&'a AuditTag> {
        tags.iter()
            .filter(|t| t.tag_type == AuditTagType::Todo)
            .collect()
    }

    /// Get all frozen sections
    pub fn get_frozen<'a>(&self, tags: &'a [AuditTag]) -> Vec<&'a AuditTag> {
        tags.iter()
            .filter(|t| t.tag_type == AuditTagType::Freeze)
            .collect()
    }

    /// Get all security tags
    pub fn get_security<'a>(&self, tags: &'a [AuditTag]) -> Vec<&'a AuditTag> {
        tags.iter()
            .filter(|t| t.tag_type == AuditTagType::Security)
            .collect()
    }

    /// Check if a file has a freeze tag
    pub fn is_frozen(&self, path: &Path, tags: &[AuditTag]) -> bool {
        tags.iter()
            .any(|t| t.file == path && t.tag_type == AuditTagType::Freeze)
    }
}

impl Default for TagScanner {
    fn default() -> Self {
        Self::new().expect("Failed to create default TagScanner")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::path::PathBuf;
    use tempfile::NamedTempFile;

    #[test]
    fn test_scan_rust_file() {
        let mut file = NamedTempFile::new().unwrap();
        writeln!(
            file,
            r#"
// @audit-tag: new
fn foo() {{
    // @audit-todo: Implement error handling
    println!("Hello");
}}

// @audit-freeze
const MAGIC: u32 = 42;
"#
        )
        .unwrap();

        let scanner = TagScanner::new().unwrap();
        let tags = scanner.scan_file(file.path()).unwrap();

        assert_eq!(tags.len(), 3);
        assert_eq!(tags[0].tag_type, AuditTagType::Tag);
        assert_eq!(tags[0].value, "new");
        assert_eq!(tags[1].tag_type, AuditTagType::Todo);
        assert!(tags[1].value.contains("Implement error handling"));
        assert_eq!(tags[2].tag_type, AuditTagType::Freeze);
    }

    #[test]
    fn test_scan_python_file() {
        let mut file = NamedTempFile::new().unwrap();
        writeln!(
            file,
            r#"
# @audit-tag: experimental
def process_data(data):
    # @audit-security: Validate input data
    return data.strip()
"#
        )
        .unwrap();

        let scanner = TagScanner::new().unwrap();
        let tags = scanner.scan_file(file.path()).unwrap();

        assert_eq!(tags.len(), 2);
        assert_eq!(tags[0].tag_type, AuditTagType::Tag);
        assert_eq!(tags[0].value, "experimental");
        assert_eq!(tags[1].tag_type, AuditTagType::Security);
    }

    #[test]
    fn test_group_by_type() {
        let scanner = TagScanner::new().unwrap();
        let tags = vec![
            AuditTag {
                tag_type: AuditTagType::Todo,
                file: PathBuf::from("test.rs"),
                line: 1,
                value: "Fix this".to_string(),
                context: None,
            },
            AuditTag {
                tag_type: AuditTagType::Todo,
                file: PathBuf::from("test2.rs"),
                line: 2,
                value: "Fix that".to_string(),
                context: None,
            },
            AuditTag {
                tag_type: AuditTagType::Freeze,
                file: PathBuf::from("test3.rs"),
                line: 3,
                value: String::new(),
                context: None,
            },
        ];

        let grouped = scanner.group_by_type(&tags);
        assert_eq!(grouped.get(&AuditTagType::Todo).unwrap().len(), 2);
        assert_eq!(grouped.get(&AuditTagType::Freeze).unwrap().len(), 1);
    }

    #[test]
    fn test_is_frozen() {
        let scanner = TagScanner::new().unwrap();
        let path = PathBuf::from("frozen.rs");
        let tags = vec![AuditTag {
            tag_type: AuditTagType::Freeze,
            file: path.clone(),
            line: 1,
            value: String::new(),
            context: None,
        }];

        assert!(scanner.is_frozen(&path, &tags));
        assert!(!scanner.is_frozen(&PathBuf::from("other.rs"), &tags));
    }
}
