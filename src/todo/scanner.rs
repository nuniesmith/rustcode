//! Todo comment scanner — walk source trees and extract TODO/FIXME/HACK/XXX comments
//!
//! This module is the backend for `rustcode todo-scan <repo-path>`.
//! It walks a directory tree, finds all source files, and extracts inline
//! comment annotations with surrounding context, then serialises them to JSON.
//!
//! # Output shape
//!
//! ```json
//! {
//!   "repo_path": "/path/to/repo",
//!   "scanned_at": "2024-01-01T00:00:00Z",
//!   "total_files_scanned": 42,
//!   "items": [
//!     {
//!       "id": "a1b2c3d4",
//!       "kind": "TODO",
//!       "priority": "medium",
//!       "file": "src/api/handlers.rs",
//!       "line": 132,
//!       "text": "Implement type counts",
//!       "context_before": ["    let response = StatsResponse {"],
//!       "context_after": ["        chunks: ChunkStats {"],
//!       "raw_comment": "// TODO: Implement type counts"
//!     }
//!   ],
//!   "summary": {
//!     "total": 1,
//!     "by_kind": { "TODO": 1 },
//!     "by_priority": { "high": 0, "medium": 1, "low": 0 },
//!     "by_extension": { "rs": 1 },
//!     "files_with_todos": 1
//!   }
//! }
//! ```

use chrono::{DateTime, Utc};
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

use crate::error::{AuditError, Result};

// ============================================================================
// Configuration
// ============================================================================

/// Configuration for the todo comment scanner
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScanConfig {
    /// Number of lines of context to capture before the TODO line
    pub context_lines_before: usize,
    /// Number of lines of context to capture after the TODO line
    pub context_lines_after: usize,
    /// File extensions to scan (without leading dot)
    pub extensions: Vec<String>,
    /// Directory/path fragments to skip entirely
    pub skip_paths: Vec<String>,
    /// Minimum priority to include in output (`low` includes everything)
    pub min_priority: CommentPriority,
    /// Whether to include NOTE comments (low priority)
    pub include_notes: bool,
    /// Whether paths in output should be relative to `repo_path`
    pub relative_paths: bool,
    /// Maximum file size in bytes to attempt reading (default 1 MiB)
    pub max_file_bytes: u64,
}

impl Default for ScanConfig {
    fn default() -> Self {
        Self {
            context_lines_before: 2,
            context_lines_after: 2,
            extensions: vec![
                "rs".into(),
                "py".into(),
                "ts".into(),
                "tsx".into(),
                "js".into(),
                "jsx".into(),
                "go".into(),
                "java".into(),
                "kt".into(),
                "kts".into(),
                "swift".into(),
                "c".into(),
                "cpp".into(),
                "h".into(),
                "hpp".into(),
                "cs".into(),
                "rb".into(),
                "sh".into(),
                "yaml".into(),
                "yml".into(),
                "toml".into(),
            ],
            skip_paths: vec![
                "target/".into(),
                "node_modules/".into(),
                ".git/".into(),
                "__pycache__/".into(),
                ".pytest_cache/".into(),
                "build/".into(),
                "dist/".into(),
                "vendor/".into(),
                ".cargo/".into(),
                ".rustcode/".into(),
            ],
            min_priority: CommentPriority::Low,
            include_notes: false,
            relative_paths: true,
            max_file_bytes: 1024 * 1024, // 1 MiB
        }
    }
}

// ============================================================================
// Comment kinds and priority
// ============================================================================

/// The annotation keyword found in the comment
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum CommentKind {
    Todo,
    Fixme,
    Hack,
    Xxx,
    Note,
    Bug,
    Optimize,
    Refactor,
    Deprecated,
}

impl CommentKind {
    /// Return the canonical uppercase string
    pub fn as_str(self) -> &'static str {
        match self {
            CommentKind::Todo => "TODO",
            CommentKind::Fixme => "FIXME",
            CommentKind::Hack => "HACK",
            CommentKind::Xxx => "XXX",
            CommentKind::Note => "NOTE",
            CommentKind::Bug => "BUG",
            CommentKind::Optimize => "OPTIMIZE",
            CommentKind::Refactor => "REFACTOR",
            CommentKind::Deprecated => "DEPRECATED",
        }
    }

    /// Infer default priority for this kind (may be overridden by content analysis)
    pub fn default_priority(self) -> CommentPriority {
        match self {
            CommentKind::Fixme | CommentKind::Xxx | CommentKind::Bug => CommentPriority::High,
            CommentKind::Todo | CommentKind::Hack | CommentKind::Refactor => {
                CommentPriority::Medium
            }
            CommentKind::Note | CommentKind::Optimize | CommentKind::Deprecated => {
                CommentPriority::Low
            }
        }
    }

    fn try_from_str(s: &str) -> Option<Self> {
        match s.to_ascii_uppercase().as_str() {
            "TODO" => Some(CommentKind::Todo),
            "FIXME" | "FIX" => Some(CommentKind::Fixme),
            "HACK" => Some(CommentKind::Hack),
            "XXX" => Some(CommentKind::Xxx),
            "NOTE" => Some(CommentKind::Note),
            "BUG" => Some(CommentKind::Bug),
            "OPTIMIZE" | "OPTIM" | "OPT" => Some(CommentKind::Optimize),
            "REFACTOR" => Some(CommentKind::Refactor),
            "DEPRECATED" | "DEPRECATE" => Some(CommentKind::Deprecated),
            _ => None,
        }
    }
}

/// Priority level derived from kind + content heuristics
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CommentPriority {
    Low = 0,
    Medium = 1,
    High = 2,
}

impl CommentPriority {
    pub fn as_str(self) -> &'static str {
        match self {
            CommentPriority::High => "high",
            CommentPriority::Medium => "medium",
            CommentPriority::Low => "low",
        }
    }
}

impl std::fmt::Display for CommentPriority {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

// ============================================================================
// Output types
// ============================================================================

/// A single extracted TODO comment item
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TodoCommentItem {
    /// Stable 8-char hex ID derived from file+line
    pub id: String,
    /// The annotation keyword
    pub kind: CommentKind,
    /// Inferred priority
    pub priority: CommentPriority,
    /// File path (relative or absolute depending on config)
    pub file: PathBuf,
    /// 1-based line number
    pub line: usize,
    /// The extracted comment text (stripped of comment markers and keyword)
    pub text: String,
    /// Lines of source code before the TODO line
    pub context_before: Vec<String>,
    /// Lines of source code after the TODO line
    pub context_after: Vec<String>,
    /// The raw, unmodified comment line as it appears in the file
    pub raw_comment: String,
    /// File extension (language hint)
    pub extension: String,
    /// Optional author/assignee extracted from `TODO(name):` syntax
    pub assignee: Option<String>,
}

/// Summary statistics for a scan run
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ScanSummary {
    pub total: usize,
    pub by_kind: HashMap<String, usize>,
    pub by_priority: HashMap<String, usize>,
    pub by_extension: HashMap<String, usize>,
    pub files_with_todos: usize,
}

impl ScanSummary {
    fn build(items: &[TodoCommentItem]) -> Self {
        let mut summary = ScanSummary::default();
        let mut files_seen = std::collections::HashSet::new();

        for item in items {
            summary.total += 1;
            *summary
                .by_kind
                .entry(item.kind.as_str().to_string())
                .or_insert(0) += 1;
            *summary
                .by_priority
                .entry(item.priority.as_str().to_string())
                .or_insert(0) += 1;
            *summary
                .by_extension
                .entry(item.extension.clone())
                .or_insert(0) += 1;
            files_seen.insert(item.file.clone());
        }

        summary.files_with_todos = files_seen.len();
        summary
    }
}

/// Complete output of a single scan run
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScanOutput {
    pub repo_path: PathBuf,
    pub scanned_at: DateTime<Utc>,
    pub total_files_scanned: usize,
    pub items: Vec<TodoCommentItem>,
    pub summary: ScanSummary,
}

impl ScanOutput {
    /// Serialise to pretty-printed JSON
    pub fn to_json_pretty(&self) -> Result<String> {
        serde_json::to_string_pretty(self)
            .map_err(|e| AuditError::other(format!("JSON serialisation failed: {}", e)))
    }

    /// Serialise to compact JSON
    pub fn to_json(&self) -> Result<String> {
        serde_json::to_string(self)
            .map_err(|e| AuditError::other(format!("JSON serialisation failed: {}", e)))
    }

    /// Filter items by minimum priority
    pub fn filter_by_priority(&self, min: CommentPriority) -> Vec<&TodoCommentItem> {
        self.items.iter().filter(|i| i.priority >= min).collect()
    }

    /// Filter items by kind
    pub fn filter_by_kind(&self, kind: CommentKind) -> Vec<&TodoCommentItem> {
        self.items.iter().filter(|i| i.kind == kind).collect()
    }
}

// ============================================================================
// Scanner
// ============================================================================

/// Compiled regex patterns used to detect TODO-style comments
struct CompiledPatterns {
    /// Matches: `//`, `#`, `--`, `*` style single-line comments with keyword
    /// Captures: (keyword, optional_assignee, text)
    line_comment: Regex,
    /// Matches block comment openers: `/* TODO: …`
    block_comment: Regex,
}

impl CompiledPatterns {
    fn new() -> Result<Self> {
        // Covers:
        //   // TODO: text
        //   // TODO(name): text
        //   # TODO: text
        //   -- TODO: text  (SQL / Lua)
        //   * TODO: text   (inside block comments)
        //   <!-- TODO: text -->  (HTML)
        let line_comment = Regex::new(
            r#"(?ix)
            (?:
                (?://|\#|--|;|%) \s*      # single-line comment starters (\# = literal hash in x-mode)
                |
                /\* \s*                   # block comment opener
                |
                \*\s+                     # continuation of block comment
                |
                <!--\s*                   # HTML comment
            )
            (TODO|FIXME|FIX|HACK|XXX|NOTE|BUG|OPTIMIZE|OPTIM|OPT|REFACTOR|DEPRECATED|DEPRECATE)
            (?:\(([^)]*)\))?             # optional (assignee)
            :?\s*                        # optional colon + space
            (.*)                         # captured text
            "#,
        )
        .map_err(|e| AuditError::other(format!("Invalid line_comment regex: {}", e)))?;

        let block_comment = Regex::new(
            r#"(?ix)
            /\* \s*
            (TODO|FIXME|FIX|HACK|XXX|NOTE|BUG|OPTIMIZE|OPTIM|OPT|REFACTOR|DEPRECATED|DEPRECATE)
            (?:\(([^)]*)\))?
            :?\s*
            (.+?)
            \s*\*/
            "#,
        )
        .map_err(|e| AuditError::other(format!("Invalid block_comment regex: {}", e)))?;

        Ok(Self {
            line_comment,
            block_comment,
        })
    }
}

/// The main scanner struct
pub struct TodoCommentScanner {
    config: ScanConfig,
    patterns: CompiledPatterns,
}

impl TodoCommentScanner {
    /// Create a scanner with default configuration
    pub fn new() -> Result<Self> {
        Ok(Self {
            config: ScanConfig::default(),
            patterns: CompiledPatterns::new()?,
        })
    }

    /// Create a scanner with explicit configuration
    pub fn with_config(config: ScanConfig) -> Result<Self> {
        Ok(Self {
            config,
            patterns: CompiledPatterns::new()?,
        })
    }

    // -----------------------------------------------------------------------
    // Public API
    // -----------------------------------------------------------------------

    /// Scan an entire repository and return structured output
    pub fn scan_repo(&self, repo_path: impl AsRef<Path>) -> Result<ScanOutput> {
        let repo_path = repo_path.as_ref().to_path_buf();
        let mut items: Vec<TodoCommentItem> = Vec::new();
        let mut total_files_scanned = 0usize;

        for entry in WalkDir::new(&repo_path)
            .follow_links(false)
            .sort_by_file_name()
            .into_iter()
            .filter_map(|e| e.ok())
        {
            let path = entry.path();

            if !path.is_file() {
                continue;
            }
            if !self.should_scan(path) {
                continue;
            }

            total_files_scanned += 1;

            match self.scan_file(path, &repo_path) {
                Ok(file_items) => items.extend(file_items),
                Err(e) => {
                    // Non-fatal: log and skip the file
                    tracing::warn!("Skipping {:?}: {}", path, e);
                }
            }
        }

        // Apply priority filter
        let min = self.config.min_priority;
        items.retain(|i| i.priority >= min);

        // Drop NOTE items if not requested
        if !self.config.include_notes {
            items.retain(|i| i.kind != CommentKind::Note);
        }

        let summary = ScanSummary::build(&items);

        Ok(ScanOutput {
            repo_path,
            scanned_at: Utc::now(),
            total_files_scanned,
            items,
            summary,
        })
    }

    /// Scan a single file and return its TODO items.
    ///
    /// `repo_root` is used to produce relative paths in output when
    /// `config.relative_paths` is true.
    pub fn scan_file(
        &self,
        path: impl AsRef<Path>,
        repo_root: impl AsRef<Path>,
    ) -> Result<Vec<TodoCommentItem>> {
        let path = path.as_ref();
        let repo_root = repo_root.as_ref();

        // Guard: size check
        let metadata = fs::metadata(path).map_err(AuditError::Io)?;
        if metadata.len() > self.config.max_file_bytes {
            return Ok(Vec::new());
        }

        let content = fs::read_to_string(path).map_err(AuditError::Io)?;
        let lines: Vec<&str> = content.lines().collect();

        let extension = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_string();

        let display_path = if self.config.relative_paths {
            path.strip_prefix(repo_root).unwrap_or(path).to_path_buf()
        } else {
            path.to_path_buf()
        };

        let mut items = Vec::new();

        for (line_idx, &line) in lines.iter().enumerate() {
            // Try line comment pattern
            if let Some(caps) = self.patterns.line_comment.captures(line) {
                let keyword = caps.get(1).map_or("", |m| m.as_str());
                let assignee = caps.get(2).and_then(|m| {
                    let s = m.as_str().trim().to_string();
                    if s.is_empty() {
                        None
                    } else {
                        Some(s)
                    }
                });
                let text = caps
                    .get(3)
                    .map_or("", |m| m.as_str())
                    .trim()
                    // strip trailing block-comment closers
                    .trim_end_matches("*/")
                    .trim_end_matches("-->")
                    .trim()
                    .to_string();

                if let Some(kind) = CommentKind::try_from_str(keyword) {
                    let priority = self.infer_priority(kind, &text);
                    let item = self.build_item(
                        &display_path,
                        line_idx,
                        line,
                        kind,
                        priority,
                        text,
                        assignee,
                        &lines,
                        &extension,
                    );
                    items.push(item);
                    continue; // don't double-match same line
                }
            }

            // Try inline block comment `/* TODO: … */`
            if let Some(caps) = self.patterns.block_comment.captures(line) {
                let keyword = caps.get(1).map_or("", |m| m.as_str());
                let assignee = caps.get(2).and_then(|m| {
                    let s = m.as_str().trim().to_string();
                    if s.is_empty() {
                        None
                    } else {
                        Some(s)
                    }
                });
                let text = caps.get(3).map_or("", |m| m.as_str()).trim().to_string();

                if let Some(kind) = CommentKind::try_from_str(keyword) {
                    let priority = self.infer_priority(kind, &text);
                    let item = self.build_item(
                        &display_path,
                        line_idx,
                        line,
                        kind,
                        priority,
                        text,
                        assignee,
                        &lines,
                        &extension,
                    );
                    items.push(item);
                }
            }
        }

        // Drop NOTE items if not requested (same filter as scan_repo)
        if !self.config.include_notes {
            items.retain(|i| i.kind != CommentKind::Note);
        }

        Ok(items)
    }

    // -----------------------------------------------------------------------
    // Private helpers
    // -----------------------------------------------------------------------

    /// Decide whether a file should be scanned
    fn should_scan(&self, path: &Path) -> bool {
        let path_str = path.to_string_lossy();

        // Skip configured path fragments
        for skip in &self.config.skip_paths {
            if path_str.contains(skip.as_str()) {
                return false;
            }
        }

        // Check extension whitelist
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");

        self.config
            .extensions
            .iter()
            .any(|e| e.eq_ignore_ascii_case(ext))
    }

    /// Build a `TodoCommentItem` from parsed fields
    #[allow(clippy::too_many_arguments)]
    fn build_item(
        &self,
        display_path: &Path,
        line_idx: usize,
        raw_line: &str,
        kind: CommentKind,
        priority: CommentPriority,
        text: String,
        assignee: Option<String>,
        all_lines: &[&str],
        extension: &str,
    ) -> TodoCommentItem {
        let before_start = line_idx.saturating_sub(self.config.context_lines_before);
        let after_end = (line_idx + 1 + self.config.context_lines_after).min(all_lines.len());

        let context_before = all_lines[before_start..line_idx]
            .iter()
            .map(|l| l.to_string())
            .collect();

        let context_after = all_lines[(line_idx + 1)..after_end]
            .iter()
            .map(|l| l.to_string())
            .collect();

        // Stable ID: hash of path + 1-based line number
        let id_input = format!("{}:{}", display_path.display(), line_idx + 1);
        let id = format!("{:08x}", crc32_simple(id_input.as_bytes()));

        TodoCommentItem {
            id,
            kind,
            priority,
            file: display_path.to_path_buf(),
            line: line_idx + 1,
            text,
            context_before,
            context_after,
            raw_comment: raw_line.to_string(),
            extension: extension.to_string(),
            assignee,
        }
    }

    /// Refine priority using content heuristics on top of the keyword default
    fn infer_priority(&self, kind: CommentKind, text: &str) -> CommentPriority {
        let base = kind.default_priority();
        let lower = text.to_ascii_lowercase();

        // Upgrade to High
        let high_signals = [
            "urgent",
            "critical",
            "security",
            "vuln",
            "crash",
            "panic",
            "data loss",
            "race condition",
            "deadlock",
            "asap",
            "broken",
            "memory leak",
            "overflow",
        ];
        if high_signals.iter().any(|s| lower.contains(s)) {
            return CommentPriority::High;
        }

        // Downgrade to Low
        let low_signals = [
            "maybe",
            "someday",
            "nice to have",
            "optional",
            "future",
            "consider",
            "might",
            "could",
            "when time permits",
            "low priority",
        ];
        if low_signals.iter().any(|s| lower.contains(s)) {
            return CommentPriority::Low;
        }

        base
    }
}

impl Default for TodoCommentScanner {
    fn default() -> Self {
        Self::new().expect("Failed to build default TodoCommentScanner")
    }
}

// ============================================================================
// CRC32 helper (no extra dep — same impl as todo_file)
// ============================================================================

fn crc32_simple(data: &[u8]) -> u32 {
    let mut crc: u32 = 0xFFFF_FFFF;
    for &byte in data {
        crc ^= (byte as u32) << 24;
        for _ in 0..8 {
            if crc & 0x8000_0000 != 0 {
                crc = (crc << 1) ^ 0x04C1_1DB7;
            } else {
                crc <<= 1;
            }
        }
    }
    !crc
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_temp(name: &str, content: &str) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join(name);
        let mut f = fs::File::create(&file_path).unwrap();
        f.write_all(content.as_bytes()).unwrap();
        (dir, file_path)
    }

    #[test]
    fn test_scan_rust_todos() {
        let src = r#"
fn foo() {
    // TODO: implement this properly
    let x = 1;
    // FIXME: this crashes on empty input
    unimplemented!()
}

// NOTE: this is just a note
const MAGIC: u32 = 42;

// HACK: temporary workaround
fn bar() {}
"#;
        let (dir, path) = write_temp("test.rs", src);
        let scanner = TodoCommentScanner::new().unwrap();
        let items = scanner.scan_file(&path, dir.path()).unwrap();

        // NOTE is excluded by default (include_notes = false)
        assert_eq!(items.len(), 3);

        let todo = items.iter().find(|i| i.kind == CommentKind::Todo).unwrap();
        assert_eq!(todo.line, 3);
        assert_eq!(todo.text, "implement this properly");
        assert_eq!(todo.priority, CommentPriority::Medium);

        let fixme = items.iter().find(|i| i.kind == CommentKind::Fixme).unwrap();
        assert_eq!(fixme.priority, CommentPriority::High);

        let hack = items.iter().find(|i| i.kind == CommentKind::Hack).unwrap();
        assert_eq!(hack.priority, CommentPriority::Medium);
    }

    #[test]
    fn test_scan_with_assignee() {
        let src = "// TODO(jordan): wire this up\nfn stub() {}\n";
        let (dir, path) = write_temp("stub.rs", src);
        let scanner = TodoCommentScanner::new().unwrap();
        let items = scanner.scan_file(&path, dir.path()).unwrap();

        assert_eq!(items.len(), 1);
        assert_eq!(items[0].assignee.as_deref(), Some("jordan"));
        assert_eq!(items[0].text, "wire this up");
    }

    #[test]
    fn test_priority_upgrade_on_security() {
        let src = "// TODO: fix security vulnerability in auth\n";
        let (dir, path) = write_temp("auth.rs", src);
        let scanner = TodoCommentScanner::new().unwrap();
        let items = scanner.scan_file(&path, dir.path()).unwrap();

        assert_eq!(items[0].priority, CommentPriority::High);
    }

    #[test]
    fn test_priority_downgrade_on_someday() {
        let src = "// TODO: maybe someday add telemetry\n";
        let (dir, path) = write_temp("tele.rs", src);
        let scanner = TodoCommentScanner::new().unwrap();
        let items = scanner.scan_file(&path, dir.path()).unwrap();

        assert_eq!(items[0].priority, CommentPriority::Low);
    }

    #[test]
    fn test_context_lines_captured() {
        let src = "fn before() {}\n// TODO: test context\nfn after() {}\n";
        let (dir, path) = write_temp("ctx.rs", src);
        let scanner = TodoCommentScanner::new().unwrap();
        let items = scanner.scan_file(&path, dir.path()).unwrap();

        assert_eq!(items[0].context_before, vec!["fn before() {}"]);
        assert_eq!(items[0].context_after, vec!["fn after() {}"]);
    }

    #[test]
    fn test_scan_output_summary() {
        let src = r#"
// TODO: one
// FIXME: two
// TODO: three
fn x() {}
"#;
        let (dir, _path) = write_temp("multi.rs", src);
        let scanner = TodoCommentScanner::new().unwrap();
        let output = scanner.scan_repo(dir.path()).unwrap();

        assert_eq!(output.summary.total, 3);
        assert_eq!(output.summary.by_kind.get("TODO"), Some(&2));
        assert_eq!(output.summary.by_kind.get("FIXME"), Some(&1));
        assert_eq!(output.summary.files_with_todos, 1);
    }

    #[test]
    fn test_skip_non_source_files() {
        let dir = tempfile::tempdir().unwrap();
        let txt = dir.path().join("readme.txt");
        fs::write(&txt, "// TODO: this should not be scanned\n").unwrap();

        let scanner = TodoCommentScanner::new().unwrap();
        let output = scanner.scan_repo(dir.path()).unwrap();

        assert_eq!(output.summary.total, 0);
    }

    #[test]
    fn test_json_round_trip() {
        let src = "// TODO: serialise me\n";
        let (dir, _path) = write_temp("serial.rs", src);
        let scanner = TodoCommentScanner::new().unwrap();
        let output = scanner.scan_repo(dir.path()).unwrap();

        let json = output.to_json_pretty().unwrap();
        let parsed: ScanOutput = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.items.len(), output.items.len());
    }

    #[test]
    fn test_python_hash_comment() {
        let src = "# TODO: handle edge case in parser\n";
        let (dir, path) = write_temp("script.py", src);
        let scanner = TodoCommentScanner::new().unwrap();
        let items = scanner.scan_file(&path, dir.path()).unwrap();

        assert_eq!(items.len(), 1);
        assert_eq!(items[0].kind, CommentKind::Todo);
        assert_eq!(items[0].text, "handle edge case in parser");
    }

    #[test]
    fn test_inline_block_comment() {
        let src = "let x = foo(); /* TODO: replace with bar() */\n";
        let (dir, path) = write_temp("block.rs", src);
        let scanner = TodoCommentScanner::new().unwrap();
        let items = scanner.scan_file(&path, dir.path()).unwrap();

        assert_eq!(items.len(), 1);
        assert_eq!(items[0].kind, CommentKind::Todo);
    }

    #[test]
    fn test_relative_paths_in_output() {
        let src = "// TODO: relative path test\n";
        let (dir, path) = write_temp("rel.rs", src);
        let scanner = TodoCommentScanner::new().unwrap();
        let items = scanner.scan_file(&path, dir.path()).unwrap();

        // Should not contain the tempdir prefix
        let p = items[0].file.to_string_lossy();
        assert!(
            !p.starts_with('/') || p == "rel.rs",
            "path should be relative: {}",
            p
        );
    }
}
