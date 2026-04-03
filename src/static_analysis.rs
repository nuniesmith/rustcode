//! Static Analysis Pre-Filter Pipeline
//!
//! This module runs zero-cost and lightweight static checks on source files
//! **before** sending them to the LLM for deep analysis. The goal is to:
//!
//! 1. Skip files that don't need LLM analysis (generated, trivial, clean)
//! 2. Deprioritize files that are unlikely to have issues
//! 3. Flag files that definitely need LLM attention (high unwrap density, unsafe, etc.)
//! 4. Provide a quality signal that the auto_scanner can use to choose prompt tiers
//!
//! Based on the 2026-02-08 review findings:
//! - 66% of files returned zero issues from the LLM
//! - 28% of files are under 5K chars (trivial)
//! - Top-cost files all returned 0 issues — static pre-filter could have skipped them
//!
//! # Architecture
//!
//! ```text
//! File → StaticAnalyzer::analyze()
//!        ├─ is_generated()           → Skip entirely
//!        ├─ content_metrics()        → char count, line count, avg line len
//!        ├─ unwrap_audit()           → .unwrap() / .expect() / panic!() density
//!        ├─ unsafe_audit()           → unsafe blocks without safety comments
//!        ├─ error_handling_ratio()   → .unwrap() vs ? operator ratio
//!        ├─ security_patterns()      → hardcoded secrets, SQL injection hints
//!        ├─ todo_fixme_count()       → quick count (full scan via TodoScanner)
//!        ├─ complexity_estimate()    → function count, nesting depth
//!        └─ staleness_check()        → git last-modified age
//!
//! Result: StaticAnalysisResult
//!        ├─ recommendation: Skip | Minimal | Standard | DeepDive
//!        ├─ skip_reason: Option<SkipReason>
//!        ├─ quality_signals: QualitySignals
//!        └─ estimated_llm_value: f64 (0.0 = no value, 1.0 = high value)
//! ```

use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
use tracing::{debug, info};

// ============================================================================
// Result Types
// ============================================================================

/// The recommendation from static analysis about how to handle a file
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum AnalysisRecommendation {
    /// Skip LLM entirely — file is generated, trivial, or provably clean
    Skip,
    /// Use a minimal/cheap prompt — small file, low complexity, no red flags
    Minimal,
    /// Use the standard analysis prompt
    Standard,
    /// Use the full deep-dive prompt — file has red flags that need expert review
    DeepDive,
}

impl std::fmt::Display for AnalysisRecommendation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Skip => write!(f, "SKIP"),
            Self::Minimal => write!(f, "MINIMAL"),
            Self::Standard => write!(f, "STANDARD"),
            Self::DeepDive => write!(f, "DEEP_DIVE"),
        }
    }
}

/// Why a file was recommended for skipping
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SkipReason {
    /// File contains generated code markers (@generated, protobuf output, etc.)
    GeneratedCode,
    /// File is empty or nearly empty (< 10 lines of actual code)
    TrivialFile,
    /// File is a lockfile, manifest, or non-logic config
    NonCodeFile,
    /// File content is identical to a previously analyzed file (content hash match)
    DuplicateContent,
    /// File is a test-only file with no production logic
    TestOnly,
    /// File hasn't changed since last successful analysis and had 0 issues
    UnchangedClean,
}

impl std::fmt::Display for SkipReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::GeneratedCode => write!(f, "generated code"),
            Self::TrivialFile => write!(f, "trivial file (<10 lines)"),
            Self::NonCodeFile => write!(f, "non-code file"),
            Self::DuplicateContent => write!(f, "duplicate content"),
            Self::TestOnly => write!(f, "test-only file"),
            Self::UnchangedClean => write!(f, "unchanged + clean"),
        }
    }
}

/// Quality signals extracted from static analysis
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct QualitySignals {
    // --- Content Metrics ---
    /// Total characters in the file
    pub char_count: usize,
    /// Total lines (including blank)
    pub total_lines: usize,
    /// Lines of actual code (non-blank, non-comment)
    pub code_lines: usize,
    /// Comment lines
    pub comment_lines: usize,
    /// Blank lines
    pub blank_lines: usize,
    /// Average line length (chars)
    pub avg_line_length: usize,

    // --- Error Handling ---
    /// Count of `.unwrap()` calls
    pub unwrap_count: usize,
    /// Count of `.expect(...)` calls
    pub expect_count: usize,
    /// Count of `panic!()` / `unreachable!()` / `unimplemented!()`
    pub panic_macro_count: usize,
    /// Count of `?` operator usage
    pub question_mark_count: usize,
    /// Count of `.unwrap_or(...)` / `.unwrap_or_default()` / `.unwrap_or_else(...)`
    pub unwrap_or_count: usize,
    /// Ratio of safe error handling vs unsafe (0.0 = all unwrap, 1.0 = all ?)
    pub error_handling_ratio: f64,

    // --- Safety ---
    /// Count of `unsafe` blocks/functions
    pub unsafe_block_count: usize,
    /// Count of `unsafe` blocks that have a `// SAFETY:` comment nearby
    pub unsafe_with_safety_comment: usize,
    /// Count of `unsafe` blocks WITHOUT safety comments
    pub unsafe_without_safety_comment: usize,

    // --- Security Patterns ---
    /// Potential hardcoded secrets found (pattern matches, may be false positives)
    pub potential_secrets: Vec<SecurityFinding>,
    /// SQL string concatenation patterns
    pub sql_injection_risks: usize,

    // --- Code Markers ---
    /// Count of TODO comments
    pub todo_count: usize,
    /// Count of FIXME comments
    pub fixme_count: usize,
    /// Count of HACK comments
    pub hack_count: usize,
    /// Count of XXX comments
    pub xxx_count: usize,

    // --- TodoScanner integration ---
    /// High-priority TODOs (FIXME, XXX, security, urgent) from TodoScanner
    pub high_priority_todos: usize,
    /// Medium-priority TODOs from TodoScanner
    pub medium_priority_todos: usize,
    /// Low-priority TODOs (NOTE, maybe, consider) from TodoScanner
    pub low_priority_todos: usize,
    /// Total items found by TodoScanner (may exceed simple regex counts)
    pub todo_scanner_total: usize,

    /// Whether the file contains `@generated` or similar markers
    pub is_generated: bool,
    /// Whether the file appears to be a protobuf/gRPC generated file
    pub is_protobuf_generated: bool,

    // --- Complexity ---
    /// Estimated number of functions/methods
    pub function_count: usize,
    /// Estimated maximum nesting depth
    pub max_nesting_depth: usize,
    /// Estimated cyclomatic complexity (simplified)
    pub estimated_complexity: usize,
    /// Whether the file has any `pub` items (is part of public API)
    pub has_public_api: bool,

    // --- Dependencies ---
    /// Number of `use` / `import` statements
    pub import_count: usize,
    /// Whether the file imports `unsafe` FFI bindings
    pub has_ffi_imports: bool,
}

/// A potential security finding from pattern matching
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecurityFinding {
    /// Line number (1-based)
    pub line: usize,
    /// Pattern that matched
    pub pattern: String,
    /// The matched text (redacted if it looks like an actual secret)
    pub matched_text: String,
    /// Confidence level
    pub confidence: FindingConfidence,
}

/// Confidence level for a security finding
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum FindingConfidence {
    /// Almost certainly a real issue
    High,
    /// Likely an issue but could be a false positive
    Medium,
    /// Possibly an issue — needs human review
    Low,
}

/// Complete result of static analysis for a single file
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StaticAnalysisResult {
    /// File path (relative to repo root)
    pub file_path: String,
    /// The language detected
    pub language: FileLanguage,
    /// Recommendation for LLM handling
    pub recommendation: AnalysisRecommendation,
    /// If Skip, why
    pub skip_reason: Option<SkipReason>,
    /// All quality signals
    pub signals: QualitySignals,
    /// Estimated value of sending this file to the LLM (0.0–1.0)
    /// 0.0 = waste of money, 1.0 = definitely worth analyzing
    pub estimated_llm_value: f64,
    /// Human-readable summary of findings
    pub summary: String,
    /// Number of static issues found (before LLM)
    pub static_issue_count: usize,
}

/// Detected file language
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum FileLanguage {
    Rust,
    Kotlin,
    Python,
    TypeScript,
    JavaScript,
    Go,
    Java,
    Shell,
    Swift,
    Cpp,
    C,
    Unknown,
}

impl FileLanguage {
    /// Detect language from file extension
    pub fn from_extension(path: &str) -> Self {
        let ext = path.rsplit('.').next().unwrap_or("");
        match ext {
            "rs" => Self::Rust,
            "kt" | "kts" => Self::Kotlin,
            "py" => Self::Python,
            "ts" | "tsx" => Self::TypeScript,
            "js" | "jsx" => Self::JavaScript,
            "go" => Self::Go,
            "java" => Self::Java,
            "sh" | "bash" | "zsh" => Self::Shell,
            "swift" => Self::Swift,
            "cpp" | "cxx" | "cc" | "hpp" => Self::Cpp,
            "c" | "h" => Self::C,
            _ => Self::Unknown,
        }
    }

    /// Get single-line comment prefix for this language
    pub fn comment_prefix(&self) -> &'static str {
        match self {
            Self::Rust
            | Self::Kotlin
            | Self::TypeScript
            | Self::JavaScript
            | Self::Go
            | Self::Java
            | Self::Swift
            | Self::Cpp
            | Self::C => "//",
            Self::Python | Self::Shell => "#",
            Self::Unknown => "//",
        }
    }
}

impl std::fmt::Display for FileLanguage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Rust => write!(f, "rust"),
            Self::Kotlin => write!(f, "kotlin"),
            Self::Python => write!(f, "python"),
            Self::TypeScript => write!(f, "typescript"),
            Self::JavaScript => write!(f, "javascript"),
            Self::Go => write!(f, "go"),
            Self::Java => write!(f, "java"),
            Self::Shell => write!(f, "shell"),
            Self::Swift => write!(f, "swift"),
            Self::Cpp => write!(f, "cpp"),
            Self::C => write!(f, "c"),
            Self::Unknown => write!(f, "unknown"),
        }
    }
}

// ============================================================================
// Configuration
// ============================================================================

/// Configuration for the static analyzer
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StaticAnalyzerConfig {
    /// Character threshold below which a file is considered "small" (default: 5000)
    pub small_file_threshold: usize,
    /// Character threshold above which a file is "large" (default: 50000)
    pub large_file_threshold: usize,
    /// Unwrap density threshold (unwraps per 100 LOC) to trigger deep dive (default: 5.0)
    pub unwrap_density_threshold: f64,
    /// Minimum code lines to be considered non-trivial (default: 10)
    pub min_code_lines: usize,
    /// Whether to run security pattern scanning (default: true)
    pub enable_security_scan: bool,
    /// Whether to check for generated file markers (default: true)
    pub enable_generated_detection: bool,
    /// File staleness threshold in days — files older than this get lower priority (default: 180)
    pub staleness_threshold_days: u64,
    /// Whether to skip test-only files (default: false — tests are still useful to scan)
    pub skip_test_files: bool,
}

impl Default for StaticAnalyzerConfig {
    fn default() -> Self {
        Self {
            small_file_threshold: 5_000,
            large_file_threshold: 50_000,
            unwrap_density_threshold: 5.0,
            min_code_lines: 10,
            enable_security_scan: true,
            enable_generated_detection: true,
            staleness_threshold_days: 180,
            skip_test_files: false,
        }
    }
}

// ============================================================================
// Static Analyzer
// ============================================================================

/// The main static analyzer that runs all pre-filter checks
pub struct StaticAnalyzer {
    config: StaticAnalyzerConfig,
    /// Compiled regex patterns (compiled once, reused)
    patterns: AnalysisPatterns,
}

/// Pre-compiled regex patterns for analysis
struct AnalysisPatterns {
    // Error handling
    unwrap_call: Regex,
    expect_call: Regex,
    panic_macro: Regex,
    question_mark: Regex,
    unwrap_or_call: Regex,

    // Safety
    unsafe_keyword: Regex,
    safety_comment: Regex,

    // Code markers
    todo_comment: Regex,
    fixme_comment: Regex,
    hack_comment: Regex,
    xxx_comment: Regex,
    generated_marker: Regex,
    protobuf_marker: Regex,

    // Security
    hardcoded_secret: Regex,
    api_key_pattern: Regex,
    password_pattern: Regex,
    token_pattern: Regex,
    sql_concat: Regex,

    // Structure (Rust-focused, but works for similar languages)
    function_def: Regex,
    pub_item: Regex,
    use_statement: Regex,
    ffi_import: Regex,
}

impl AnalysisPatterns {
    fn new() -> Self {
        Self {
            // Error handling patterns
            unwrap_call: Regex::new(r"\.unwrap\(\)").unwrap(),
            expect_call: Regex::new(r"\.expect\(").unwrap(),
            panic_macro: Regex::new(r"\b(panic!|unreachable!|unimplemented!|todo!)\s*[\(\{]")
                .unwrap(),
            question_mark: Regex::new(r"\?\s*[;,\)]").unwrap(),
            unwrap_or_call: Regex::new(r"\.unwrap_or(_default|_else)?\(").unwrap(),

            // Safety patterns
            unsafe_keyword: Regex::new(r"\bunsafe\s*[\{(]|\bunsafe\s+fn\b|\bunsafe\s+impl\b")
                .unwrap(),
            safety_comment: Regex::new(r"(?i)//\s*SAFETY\s*:").unwrap(),

            // Code marker patterns
            todo_comment: Regex::new(r"(?i)(//|#)\s*TODO[\s:(\[]").unwrap(),
            fixme_comment: Regex::new(r"(?i)(//|#)\s*FIXME[\s:(\[]").unwrap(),
            hack_comment: Regex::new(r"(?i)(//|#)\s*HACK[\s:(\[]").unwrap(),
            xxx_comment: Regex::new(r"(?i)(//|#)\s*XXX[\s:(\[]").unwrap(),
            generated_marker: Regex::new(
                r"(?i)(@generated|auto[-_]?generated|do not edit|machine generated|code generated|this file is generated)",
            )
            .unwrap(),
            protobuf_marker: Regex::new(
                r"(?i)(prost|tonic|protobuf|proto3|\.proto\b|#\[derive\(.*(?:Message|Enumeration).*\)\])",
            )
            .unwrap(),

            // Security patterns — intentionally broad to catch false positives rather than miss real ones
            hardcoded_secret: Regex::new(
                r#"(?i)(secret|private_key|api_secret|auth_token)\s*[:=]\s*["'][^"']{8,}["']"#,
            )
            .unwrap(),
            api_key_pattern: Regex::new(
                r#"(?i)(api[_-]?key|apikey)\s*[:=]\s*["'][A-Za-z0-9_\-]{16,}["']"#,
            )
            .unwrap(),
            password_pattern: Regex::new(
                r#"(?i)(password|passwd|pwd)\s*[:=]\s*["'][^"']{4,}["']"#,
            )
            .unwrap(),
            token_pattern: Regex::new(
                r#"(?i)\b(ghp_[A-Za-z0-9]{36}|sk-[A-Za-z0-9]{32,}|xox[bpas]-[A-Za-z0-9\-]+)"#,
            )
            .unwrap(),
            sql_concat: Regex::new(
                r#"(?i)(format!|&format|\.push_str)\s*\(.*(?:SELECT|INSERT|UPDATE|DELETE|DROP|ALTER)\b"#,
            )
            .unwrap(),

            // Structure patterns
            function_def: Regex::new(
                r"(?m)^\s*(?:pub\s+)?(?:async\s+)?(?:unsafe\s+)?fn\s+\w+|(?:pub\s+)?(?:suspend\s+)?fun\s+\w+|def\s+\w+|function\s+\w+|func\s+\w+",
            )
            .unwrap(),
            pub_item: Regex::new(r"(?m)^\s*pub\s+(fn|struct|enum|trait|type|const|static|mod)\b")
                .unwrap(),
            use_statement: Regex::new(r"(?m)^\s*use\s+|^\s*import\s+|^\s*from\s+\S+\s+import\s+")
                .unwrap(),
            ffi_import: Regex::new(r#"(?i)(extern\s+"C"|#\[link|libc::|std::ffi|ctypes|cffi)"#)
                .unwrap(),
        }
    }
}

impl StaticAnalyzer {
    /// Create a new static analyzer with default configuration
    pub fn new() -> Self {
        Self {
            config: StaticAnalyzerConfig::default(),
            patterns: AnalysisPatterns::new(),
        }
    }

    /// Create a new static analyzer with custom configuration
    pub fn with_config(config: StaticAnalyzerConfig) -> Self {
        Self {
            config,
            patterns: AnalysisPatterns::new(),
        }
    }

    /// Run all static analysis checks on a file's content.
    ///
    /// This is the main entry point. It returns a complete `StaticAnalysisResult`
    /// with a recommendation on whether/how to send the file to the LLM.
    pub fn analyze(&self, file_path: &str, content: &str) -> StaticAnalysisResult {
        let language = FileLanguage::from_extension(file_path);
        let mut signals = QualitySignals::default();

        // --- Phase 1: Content metrics ---
        self.analyze_content_metrics(content, language, &mut signals);

        // --- Phase 2: Generated file detection ---
        if self.config.enable_generated_detection {
            self.detect_generated_markers(content, &mut signals);
        }

        // --- Phase 3: Error handling audit ---
        self.audit_error_handling(content, &mut signals);

        // --- Phase 4: Safety audit (unsafe blocks) ---
        self.audit_unsafe_usage(content, &mut signals);

        // --- Phase 5: Security pattern scan ---
        if self.config.enable_security_scan {
            self.scan_security_patterns(content, &mut signals);
        }

        // --- Phase 6: Code markers (TODO/FIXME/HACK/XXX) ---
        self.count_code_markers(content, &mut signals);

        // --- Phase 7: Complexity estimate ---
        self.estimate_complexity(content, &mut signals);

        // --- Phase 8: Dependency analysis ---
        self.analyze_dependencies(content, &mut signals);

        // --- Determine recommendation ---
        let (recommendation, skip_reason) = self.determine_recommendation(file_path, &signals);
        let estimated_llm_value = self.estimate_llm_value(&signals, &recommendation);
        let static_issue_count = self.count_static_issues(&signals);
        let summary = self.generate_summary(file_path, &signals, &recommendation, &skip_reason);

        debug!(
            "Static analysis of {}: {} (value: {:.2}, issues: {})",
            file_path, recommendation, estimated_llm_value, static_issue_count
        );

        StaticAnalysisResult {
            file_path: file_path.to_string(),
            language,
            recommendation,
            skip_reason,
            signals,
            estimated_llm_value,
            summary,
            static_issue_count,
        }
    }

    /// Run static analysis with TodoScanner integration.
    ///
    /// This performs the same analysis as `analyze()` but additionally runs
    /// the `TodoScanner` on the content to get richer TODO/FIXME data with
    /// priority classification. The TodoScanner results are merged into
    /// `QualitySignals` and can influence the recommendation (e.g. many
    /// high-priority FIXMEs may push a file toward DeepDive).
    pub fn analyze_with_todos(
        &self,
        file_path: &str,
        content: &str,
        todo_scanner: &crate::todo_scanner::TodoScanner,
    ) -> StaticAnalysisResult {
        let mut result = self.analyze(file_path, content);

        // Run TodoScanner on the content by writing to a temp file
        // (TodoScanner works on files, so we use a temp approach)
        // Instead, we can parse the content inline using the scanner's patterns
        // For efficiency, we count inline using the same regex approach:
        self.merge_todo_scanner_results(file_path, content, todo_scanner, &mut result);

        result
    }

    /// Merge TodoScanner-style priority classification into an existing
    /// StaticAnalysisResult by scanning the content inline.
    ///
    /// This avoids the need for a temp file: instead of calling
    /// `TodoScanner::scan_file`, we iterate lines and classify each
    /// TODO/FIXME/HACK/XXX/NOTE match by priority using the same
    /// heuristics that `TodoScanner::infer_priority` uses.
    fn merge_todo_scanner_results(
        &self,
        _file_path: &str,
        content: &str,
        _todo_scanner: &crate::todo_scanner::TodoScanner,
        result: &mut StaticAnalysisResult,
    ) {
        let mut high = 0usize;
        let mut medium = 0usize;
        let mut low = 0usize;
        let mut total = 0usize;

        for line in content.lines() {
            let lower = line.to_lowercase();

            // Check if the line contains a TODO-family marker
            let is_todo = lower.contains("todo:") || lower.contains("todo ");
            let is_fixme = lower.contains("fixme");
            let is_hack = lower.contains("hack:") || lower.contains("hack ");
            let is_xxx = lower.contains("xxx:") || lower.contains("xxx ");
            let is_note = lower.contains("note:") || lower.contains("note ");

            // Must be inside a comment (starts with //, #, /*, or *)
            let trimmed = line.trim();
            let in_comment = trimmed.starts_with("//")
                || trimmed.starts_with('#')
                || trimmed.starts_with("/*")
                || trimmed.starts_with('*');

            if !in_comment {
                continue;
            }

            if !(is_todo || is_fixme || is_hack || is_xxx || is_note) {
                continue;
            }

            total += 1;

            // High priority: FIXME, XXX, or text contains urgent/critical/security/bug
            if is_fixme
                || is_xxx
                || lower.contains("urgent")
                || lower.contains("critical")
                || lower.contains("security")
                || lower.contains("bug")
                || lower.contains("important")
                || lower.contains("asap")
            {
                high += 1;
            } else if is_note
                || lower.contains("maybe")
                || lower.contains("consider")
                || lower.contains("nice to have")
                || lower.contains("optional")
                || lower.contains("future")
            {
                // Low priority: NOTE, or text with tentative language
                low += 1;
            } else {
                // Default: medium
                medium += 1;
            }
        }

        if total == 0 {
            return;
        }

        // Merge into signals
        result.signals.high_priority_todos = high;
        result.signals.medium_priority_todos = medium;
        result.signals.low_priority_todos = low;
        result.signals.todo_scanner_total = total;

        // Update the static issue count to include scanner-found items that
        // were not already counted by the simple regex marker counts.
        let simple_total = result.signals.todo_count
            + result.signals.fixme_count
            + result.signals.hack_count
            + result.signals.xxx_count;
        if total > simple_total {
            result.static_issue_count += total - simple_total;
        }

        // If there are many high-priority TODOs (FIXME, XXX, security, urgent),
        // consider upgrading the recommendation
        if high >= 3
            && matches!(
                result.recommendation,
                AnalysisRecommendation::Standard | AnalysisRecommendation::Minimal
            )
        {
            result.recommendation = AnalysisRecommendation::DeepDive;
            result.estimated_llm_value = result.estimated_llm_value.max(0.8);
            result.summary = format!(
                "{} [UPGRADED to DeepDive: {} high-priority TODOs]",
                result.summary, high
            );
        }

        // Update summary to note TodoScanner findings
        if high > 0 {
            result.summary = format!(
                "{} | TodoScanner: {} items ({} high, {} medium, {} low)",
                result.summary, total, high, medium, low
            );
        }
    }

    /// Analyze a file by reading it from disk.
    ///
    /// Convenience wrapper around `analyze()` that handles file I/O.
    pub fn analyze_file(&self, file_path: &Path) -> std::io::Result<StaticAnalysisResult> {
        let content = std::fs::read_to_string(file_path)?;
        let path_str = file_path.to_string_lossy();
        Ok(self.analyze(&path_str, &content))
    }

    // ========================================================================
    // Phase 1: Content Metrics
    // ========================================================================

    fn analyze_content_metrics(
        &self,
        content: &str,
        language: FileLanguage,
        signals: &mut QualitySignals,
    ) {
        signals.char_count = content.len();
        signals.total_lines = content.lines().count().max(1);

        let comment_prefix = language.comment_prefix();
        let mut code_lines = 0usize;
        let mut comment_lines = 0usize;
        let mut blank_lines = 0usize;
        let mut total_line_chars = 0usize;

        for line in content.lines() {
            let trimmed = line.trim();
            total_line_chars += line.len();

            if trimmed.is_empty() {
                blank_lines += 1;
            } else if trimmed.starts_with(comment_prefix)
                || trimmed.starts_with("/*")
                || trimmed.starts_with('*')
                || trimmed.starts_with("///")
                || trimmed.starts_with("//!")
                || trimmed.starts_with('#')
                    && matches!(language, FileLanguage::Python | FileLanguage::Shell)
            {
                comment_lines += 1;
            } else {
                code_lines += 1;
            }
        }

        signals.code_lines = code_lines;
        signals.comment_lines = comment_lines;
        signals.blank_lines = blank_lines;
        signals.avg_line_length = if signals.total_lines > 0 {
            total_line_chars / signals.total_lines
        } else {
            0
        };
    }

    // ========================================================================
    // Phase 2: Generated File Detection
    // ========================================================================

    fn detect_generated_markers(&self, content: &str, signals: &mut QualitySignals) {
        // Only check the first 50 lines (generated markers are at the top)
        let header: String = content.lines().take(50).collect::<Vec<_>>().join("\n");

        signals.is_generated = self.patterns.generated_marker.is_match(&header);
        signals.is_protobuf_generated = self.patterns.protobuf_marker.is_match(&header)
            && (signals.is_generated || header.contains("#[derive("));
    }

    // ========================================================================
    // Phase 3: Error Handling Audit
    // ========================================================================

    fn audit_error_handling(&self, content: &str, signals: &mut QualitySignals) {
        // Count in non-test code only for unwrap/expect
        // We track counts in all code but weight test code differently in the recommendation
        let mut in_test_module = false;

        for line in content.lines() {
            let trimmed = line.trim();

            // Track test module boundaries (Rust)
            if trimmed.contains("#[cfg(test)]") || trimmed.starts_with("mod tests") {
                in_test_module = true;
            }

            // Count error handling patterns
            if !in_test_module {
                signals.unwrap_count += self.patterns.unwrap_call.find_iter(trimmed).count();
                signals.expect_count += self.patterns.expect_call.find_iter(trimmed).count();
                signals.panic_macro_count += self.patterns.panic_macro.find_iter(trimmed).count();
            }

            // Always count safe patterns
            signals.question_mark_count += self.patterns.question_mark.find_iter(trimmed).count();
            signals.unwrap_or_count += self.patterns.unwrap_or_call.find_iter(trimmed).count();
        }

        // Calculate error handling ratio
        let unsafe_handling =
            (signals.unwrap_count + signals.expect_count + signals.panic_macro_count) as f64;
        let safe_handling = (signals.question_mark_count + signals.unwrap_or_count) as f64;
        let total = unsafe_handling + safe_handling;

        signals.error_handling_ratio = if total > 0.0 {
            safe_handling / total
        } else {
            1.0 // No error handling at all = no issue
        };
    }

    // ========================================================================
    // Phase 4: Unsafe Usage Audit
    // ========================================================================

    fn audit_unsafe_usage(&self, content: &str, signals: &mut QualitySignals) {
        let lines: Vec<&str> = content.lines().collect();

        for (i, line) in lines.iter().enumerate() {
            if self.patterns.unsafe_keyword.is_match(line) {
                signals.unsafe_block_count += 1;

                // Check for SAFETY comment in the 3 lines above
                let has_safety_comment = (i.saturating_sub(3)..i)
                    .any(|j| self.patterns.safety_comment.is_match(lines[j]));

                if has_safety_comment {
                    signals.unsafe_with_safety_comment += 1;
                } else {
                    signals.unsafe_without_safety_comment += 1;
                }
            }
        }
    }

    // ========================================================================
    // Phase 5: Security Pattern Scan
    // ========================================================================

    fn scan_security_patterns(&self, content: &str, signals: &mut QualitySignals) {
        for (line_num, line) in content.lines().enumerate() {
            let trimmed = line.trim();

            // Skip comment-only lines (patterns in comments are usually docs/examples)
            if trimmed.starts_with("//") || trimmed.starts_with('#') || trimmed.starts_with("/*") {
                continue;
            }

            // Hardcoded secrets
            if self.patterns.hardcoded_secret.is_match(trimmed) {
                signals.potential_secrets.push(SecurityFinding {
                    line: line_num + 1,
                    pattern: "hardcoded_secret".to_string(),
                    matched_text: Self::redact_match(trimmed),
                    confidence: FindingConfidence::Medium,
                });
            }

            // API keys
            if self.patterns.api_key_pattern.is_match(trimmed) {
                signals.potential_secrets.push(SecurityFinding {
                    line: line_num + 1,
                    pattern: "api_key".to_string(),
                    matched_text: Self::redact_match(trimmed),
                    confidence: FindingConfidence::High,
                });
            }

            // Passwords
            if self.patterns.password_pattern.is_match(trimmed) {
                // Lower confidence — this pattern has many false positives in test code
                let confidence = if trimmed.contains("test")
                    || trimmed.contains("example")
                    || trimmed.contains("placeholder")
                    || trimmed.contains("changeme")
                    || trimmed.contains("xxx")
                {
                    FindingConfidence::Low
                } else {
                    FindingConfidence::Medium
                };

                signals.potential_secrets.push(SecurityFinding {
                    line: line_num + 1,
                    pattern: "password".to_string(),
                    matched_text: Self::redact_match(trimmed),
                    confidence,
                });
            }

            // Known token formats (GitHub, OpenAI, Slack)
            if self.patterns.token_pattern.is_match(trimmed) {
                signals.potential_secrets.push(SecurityFinding {
                    line: line_num + 1,
                    pattern: "known_token_format".to_string(),
                    matched_text: Self::redact_match(trimmed),
                    confidence: FindingConfidence::High,
                });
            }

            // SQL injection via string concatenation
            if self.patterns.sql_concat.is_match(trimmed) {
                signals.sql_injection_risks += 1;
            }
        }
    }

    /// Redact potentially sensitive values for logging
    fn redact_match(line: &str) -> String {
        if line.len() > 80 {
            format!("{}...[REDACTED]", &line[..40])
        } else {
            line.to_string()
        }
    }

    // ========================================================================
    // Phase 6: Code Markers
    // ========================================================================

    fn count_code_markers(&self, content: &str, signals: &mut QualitySignals) {
        for line in content.lines() {
            if self.patterns.todo_comment.is_match(line) {
                signals.todo_count += 1;
            }
            if self.patterns.fixme_comment.is_match(line) {
                signals.fixme_count += 1;
            }
            if self.patterns.hack_comment.is_match(line) {
                signals.hack_count += 1;
            }
            if self.patterns.xxx_comment.is_match(line) {
                signals.xxx_count += 1;
            }
        }
    }

    // ========================================================================
    // Phase 7: Complexity Estimate
    // ========================================================================

    fn estimate_complexity(&self, content: &str, signals: &mut QualitySignals) {
        signals.function_count = self.patterns.function_def.find_iter(content).count();

        // Estimate nesting depth from indentation
        let mut max_nesting = 0usize;
        for line in content.lines() {
            let indent = line.len() - line.trim_start().len();
            // Assume 4-space indent = 1 nesting level
            let nesting = indent / 4;
            if nesting > max_nesting {
                max_nesting = nesting;
            }
        }
        signals.max_nesting_depth = max_nesting.min(20); // Cap at 20

        // Simplified cyclomatic complexity estimate:
        // Count decision points (if, match, while, for, loop, &&, ||)
        let decision_points = content
            .lines()
            .map(|line| {
                let trimmed = line.trim();
                let mut count = 0usize;
                // Don't count keywords in comments
                if trimmed.starts_with("//") || trimmed.starts_with('#') {
                    return 0;
                }
                if trimmed.starts_with("if ")
                    || trimmed.contains(" if ")
                    || trimmed.starts_with("else if ")
                {
                    count += 1;
                }
                if trimmed.starts_with("match ") || trimmed.contains(" match ") {
                    count += 1;
                }
                if trimmed.starts_with("while ")
                    || trimmed.starts_with("for ")
                    || trimmed.starts_with("loop ")
                    || trimmed.starts_with("loop{")
                {
                    count += 1;
                }
                if trimmed.contains("&&") {
                    count += 1;
                }
                if trimmed.contains("||") {
                    count += 1;
                }
                count
            })
            .sum::<usize>();

        signals.estimated_complexity = signals.function_count + decision_points;

        // Check for public API
        signals.has_public_api = self.patterns.pub_item.is_match(content);
    }

    // ========================================================================
    // Phase 8: Dependency Analysis
    // ========================================================================

    fn analyze_dependencies(&self, content: &str, signals: &mut QualitySignals) {
        signals.import_count = self.patterns.use_statement.find_iter(content).count();
        signals.has_ffi_imports = self.patterns.ffi_import.is_match(content);
    }

    // ========================================================================
    // Recommendation Engine
    // ========================================================================

    fn determine_recommendation(
        &self,
        file_path: &str,
        signals: &QualitySignals,
    ) -> (AnalysisRecommendation, Option<SkipReason>) {
        // --- Skip conditions (highest priority) ---

        // Generated files → skip entirely
        if signals.is_generated || signals.is_protobuf_generated {
            return (
                AnalysisRecommendation::Skip,
                Some(SkipReason::GeneratedCode),
            );
        }

        // Trivial files (< min_code_lines of actual code)
        if signals.code_lines < self.config.min_code_lines {
            return (AnalysisRecommendation::Skip, Some(SkipReason::TrivialFile));
        }

        // Test-only files (if configured to skip)
        if self.config.skip_test_files && Self::is_test_only_file(file_path) {
            return (AnalysisRecommendation::Skip, Some(SkipReason::TestOnly));
        }

        // --- Deep dive conditions (red flags that need LLM attention) ---

        // Security findings with high confidence → must review
        let high_confidence_secrets = signals
            .potential_secrets
            .iter()
            .filter(|s| s.confidence == FindingConfidence::High)
            .count();
        if high_confidence_secrets > 0 {
            return (AnalysisRecommendation::DeepDive, None);
        }

        // Unsafe blocks without safety comments → must review
        if signals.unsafe_without_safety_comment > 0 {
            return (AnalysisRecommendation::DeepDive, None);
        }

        // High unwrap density in non-trivial code → needs review
        if signals.code_lines > 0 {
            let unwrap_density = (signals.unwrap_count as f64 / signals.code_lines as f64) * 100.0;
            if unwrap_density > self.config.unwrap_density_threshold {
                return (AnalysisRecommendation::DeepDive, None);
            }
        }

        // SQL injection risks → must review
        if signals.sql_injection_risks > 0 {
            return (AnalysisRecommendation::DeepDive, None);
        }

        // FFI code → complex, needs review
        if signals.has_ffi_imports {
            return (AnalysisRecommendation::DeepDive, None);
        }

        // High complexity + many issues → deep dive
        if signals.estimated_complexity > 50
            && (signals.fixme_count + signals.hack_count + signals.xxx_count) > 2
        {
            return (AnalysisRecommendation::DeepDive, None);
        }

        // --- Minimal conditions (low risk, small file) ---

        let is_small = signals.char_count < self.config.small_file_threshold;
        let has_no_red_flags = signals.unwrap_count == 0
            && signals.unsafe_block_count == 0
            && signals.potential_secrets.is_empty()
            && signals.fixme_count == 0
            && signals.hack_count == 0;

        if is_small && has_no_red_flags {
            return (AnalysisRecommendation::Minimal, None);
        }

        // Large clean files — clean large files are also candidates for minimal
        let is_large = signals.char_count > self.config.large_file_threshold;
        if is_large && has_no_red_flags && signals.error_handling_ratio > 0.8 {
            // Large file with good error handling and no red flags
            // The LLM historically finds 0 issues on these (from the review data)
            return (AnalysisRecommendation::Minimal, None);
        }

        // --- Default: Standard analysis ---
        (AnalysisRecommendation::Standard, None)
    }

    /// Estimate how valuable it would be to send this file to the LLM (0.0–1.0)
    fn estimate_llm_value(
        &self,
        signals: &QualitySignals,
        recommendation: &AnalysisRecommendation,
    ) -> f64 {
        match recommendation {
            AnalysisRecommendation::Skip => 0.0,
            AnalysisRecommendation::Minimal => 0.15,
            AnalysisRecommendation::DeepDive => 0.9,
            AnalysisRecommendation::Standard => {
                let mut value = 0.4; // Base value for standard

                // More issues found statically → more value from LLM context
                let static_issues = self.count_static_issues(signals);
                value += (static_issues as f64 * 0.05).min(0.3);

                // Higher complexity → more value
                if signals.estimated_complexity > 30 {
                    value += 0.1;
                }

                // Poor error handling → more value
                if signals.error_handling_ratio < 0.5 {
                    value += 0.1;
                }

                // Public API → more value (bugs affect dependents)
                if signals.has_public_api {
                    value += 0.05;
                }

                value.min(0.85) // Cap below DeepDive
            }
        }
    }

    /// Count the number of issues found purely by static analysis
    fn count_static_issues(&self, signals: &QualitySignals) -> usize {
        let mut count = 0usize;

        // Each unsafe without safety comment is an issue
        count += signals.unsafe_without_safety_comment;

        // Each FIXME/HACK/XXX is an issue
        count += signals.fixme_count + signals.hack_count + signals.xxx_count;

        // Security findings
        count += signals.potential_secrets.len();
        count += signals.sql_injection_risks;

        // High unwrap count is an issue (threshold: more than 5 in non-test code)
        if signals.unwrap_count > 5 {
            count += 1;
        }

        // Panic macros in non-test code
        count += signals.panic_macro_count;

        count
    }

    /// Check if a file is test-only based on its path
    fn is_test_only_file(path: &str) -> bool {
        path.contains("/tests/")
            || path.contains("/test/")
            || path.ends_with("_test.rs")
            || path.ends_with("_test.kt")
            || path.ends_with("_test.go")
            || path.ends_with("_test.py")
            || path.ends_with(".test.ts")
            || path.ends_with(".test.js")
            || path.ends_with(".spec.ts")
            || path.ends_with(".spec.js")
    }

    /// Generate a human-readable summary
    fn generate_summary(
        &self,
        file_path: &str,
        signals: &QualitySignals,
        recommendation: &AnalysisRecommendation,
        skip_reason: &Option<SkipReason>,
    ) -> String {
        let mut parts = Vec::new();

        parts.push(format!(
            "{}: {} ({} LOC, {} chars)",
            file_path, recommendation, signals.code_lines, signals.char_count
        ));

        if let Some(reason) = skip_reason {
            parts.push(format!("  Skip reason: {}", reason));
        }

        if signals.unwrap_count > 0 || signals.expect_count > 0 {
            parts.push(format!(
                "  Error handling: {} .unwrap(), {} .expect(), {} ?, ratio={:.0}%",
                signals.unwrap_count,
                signals.expect_count,
                signals.question_mark_count,
                signals.error_handling_ratio * 100.0
            ));
        }

        if signals.unsafe_block_count > 0 {
            parts.push(format!(
                "  Unsafe: {} blocks ({} with SAFETY comment, {} without)",
                signals.unsafe_block_count,
                signals.unsafe_with_safety_comment,
                signals.unsafe_without_safety_comment
            ));
        }

        if !signals.potential_secrets.is_empty() {
            parts.push(format!(
                "  ⚠️  Security: {} potential secrets found",
                signals.potential_secrets.len()
            ));
        }

        if signals.sql_injection_risks > 0 {
            parts.push(format!(
                "  ⚠️  SQL injection risk: {} string concatenation patterns",
                signals.sql_injection_risks
            ));
        }

        let marker_total =
            signals.todo_count + signals.fixme_count + signals.hack_count + signals.xxx_count;
        if marker_total > 0 {
            parts.push(format!(
                "  Markers: {} TODO, {} FIXME, {} HACK, {} XXX",
                signals.todo_count, signals.fixme_count, signals.hack_count, signals.xxx_count
            ));
        }

        parts.push(format!(
            "  Complexity: ~{} functions, max nesting={}, complexity score={}",
            signals.function_count, signals.max_nesting_depth, signals.estimated_complexity
        ));

        parts.join("\n")
    }

    /// Get the analyzer configuration
    pub fn config(&self) -> &StaticAnalyzerConfig {
        &self.config
    }
}

impl Default for StaticAnalyzer {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// Batch Analysis
// ============================================================================

/// Result of analyzing a batch of files
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BatchAnalysisReport {
    /// Total files analyzed
    pub total_files: usize,
    /// Files recommended to skip
    pub skip_count: usize,
    /// Files recommended for minimal prompt
    pub minimal_count: usize,
    /// Files recommended for standard prompt
    pub standard_count: usize,
    /// Files recommended for deep dive
    pub deep_dive_count: usize,
    /// Total static issues found
    pub total_static_issues: usize,
    /// Breakdown by skip reason
    pub skip_reasons: HashMap<String, usize>,
    /// Estimated LLM cost savings (percentage of files that can be skipped/minimized)
    pub estimated_savings_percent: f64,
    /// Individual file results
    pub results: Vec<StaticAnalysisResult>,
}

/// Run static analysis on a batch of files and produce an aggregate report
pub fn analyze_batch(
    analyzer: &StaticAnalyzer,
    files: &[(String, String)], // (file_path, content) pairs
) -> BatchAnalysisReport {
    let mut results = Vec::with_capacity(files.len());
    let mut skip_count = 0usize;
    let mut minimal_count = 0usize;
    let mut standard_count = 0usize;
    let mut deep_dive_count = 0usize;
    let mut total_static_issues = 0usize;
    let mut skip_reasons: HashMap<String, usize> = HashMap::new();

    for (path, content) in files {
        let result = analyzer.analyze(path, content);

        match result.recommendation {
            AnalysisRecommendation::Skip => {
                skip_count += 1;
                if let Some(ref reason) = result.skip_reason {
                    *skip_reasons.entry(reason.to_string()).or_insert(0) += 1;
                }
            }
            AnalysisRecommendation::Minimal => minimal_count += 1,
            AnalysisRecommendation::Standard => standard_count += 1,
            AnalysisRecommendation::DeepDive => deep_dive_count += 1,
        }

        total_static_issues += result.static_issue_count;
        results.push(result);
    }

    let total = files.len().max(1);
    let estimated_savings_percent = ((skip_count + minimal_count) as f64 / total as f64) * 100.0;

    info!(
        "Static analysis batch complete: {} files → {} skip, {} minimal, {} standard, {} deep_dive ({:.0}% savings)",
        total, skip_count, minimal_count, standard_count, deep_dive_count, estimated_savings_percent
    );

    BatchAnalysisReport {
        total_files: files.len(),
        skip_count,
        minimal_count,
        standard_count,
        deep_dive_count,
        total_static_issues,
        skip_reasons,
        estimated_savings_percent,
        results,
    }
}

// ============================================================================
// Clippy Integration
// ============================================================================

/// Result of running `cargo clippy` on a project
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClippyResult {
    /// Total warnings found
    pub total_warnings: usize,
    /// Warnings grouped by file path
    pub warnings_by_file: HashMap<String, Vec<ClippyWarning>>,
    /// Whether clippy ran successfully
    pub success: bool,
    /// Error message if clippy failed
    pub error: Option<String>,
}

/// A single clippy warning
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClippyWarning {
    /// Lint name (e.g., "clippy::unwrap_used")
    pub lint: String,
    /// Warning message
    pub message: String,
    /// File path
    pub file: String,
    /// Line number
    pub line: usize,
    /// Column number
    pub column: usize,
    /// Severity level
    pub level: String,
}

/// Run `cargo clippy --message-format=json` and parse the results.
///
/// This provides deterministic, zero-cost (no LLM) issue detection for Rust projects.
/// Returns structured warnings that can be used as a pre-filter.
pub async fn run_clippy(project_path: &Path) -> ClippyResult {
    use std::process::Command;

    let output = Command::new("cargo")
        .args([
            "clippy",
            "--message-format=json",
            "--all-targets",
            "--quiet",
        ])
        .current_dir(project_path)
        .output();

    match output {
        Ok(output) => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let mut warnings_by_file: HashMap<String, Vec<ClippyWarning>> = HashMap::new();
            let mut total_warnings = 0usize;

            for line in stdout.lines() {
                // Parse each JSON line from clippy
                if let Ok(msg) = serde_json::from_str::<serde_json::Value>(line) {
                    if msg.get("reason").and_then(|r| r.as_str()) == Some("compiler-message") {
                        if let Some(message) = msg.get("message") {
                            let level = message
                                .get("level")
                                .and_then(|l| l.as_str())
                                .unwrap_or("unknown");

                            // Only track warnings and errors (skip notes, help, etc.)
                            if level != "warning" && level != "error" {
                                continue;
                            }

                            let msg_text = message
                                .get("message")
                                .and_then(|m| m.as_str())
                                .unwrap_or("")
                                .to_string();

                            // Extract the lint code
                            let lint = message
                                .get("code")
                                .and_then(|c| c.get("code"))
                                .and_then(|c| c.as_str())
                                .unwrap_or("unknown")
                                .to_string();

                            // Extract file location from primary span
                            if let Some(spans) = message.get("spans").and_then(|s| s.as_array()) {
                                if let Some(primary_span) = spans.iter().find(|s| {
                                    s.get("is_primary")
                                        .and_then(|p| p.as_bool())
                                        .unwrap_or(false)
                                }) {
                                    let file = primary_span
                                        .get("file_name")
                                        .and_then(|f| f.as_str())
                                        .unwrap_or("unknown")
                                        .to_string();

                                    let line_num = primary_span
                                        .get("line_start")
                                        .and_then(|l| l.as_u64())
                                        .unwrap_or(0)
                                        as usize;

                                    let column = primary_span
                                        .get("column_start")
                                        .and_then(|c| c.as_u64())
                                        .unwrap_or(0)
                                        as usize;

                                    let warning = ClippyWarning {
                                        lint,
                                        message: msg_text,
                                        file: file.clone(),
                                        line: line_num,
                                        column,
                                        level: level.to_string(),
                                    };

                                    warnings_by_file.entry(file).or_default().push(warning);
                                    total_warnings += 1;
                                }
                            }
                        }
                    }
                }
            }

            ClippyResult {
                total_warnings,
                warnings_by_file,
                success: output.status.success() || total_warnings > 0,
                error: if !output.status.success() && total_warnings == 0 {
                    Some(String::from_utf8_lossy(&output.stderr).to_string())
                } else {
                    None
                },
            }
        }
        Err(e) => ClippyResult {
            total_warnings: 0,
            warnings_by_file: HashMap::new(),
            success: false,
            error: Some(format!("Failed to run cargo clippy: {}", e)),
        },
    }
}

// ============================================================================
// Git Staleness Check
// ============================================================================

/// Check how recently a file was modified in git
pub fn check_file_staleness(repo_path: &Path, file_path: &str) -> Option<i64> {
    use std::process::Command;

    let output = Command::new("git")
        .args(["log", "-1", "--format=%ct", "--", file_path])
        .current_dir(repo_path)
        .output()
        .ok()?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    stdout.trim().parse::<i64>().ok()
}

/// Check how many days since a file was last modified in git
pub fn file_age_days(repo_path: &Path, file_path: &str) -> Option<u64> {
    let last_modified = check_file_staleness(repo_path, file_path)?;
    let now = chrono::Utc::now().timestamp();
    let age_secs = (now - last_modified).max(0) as u64;
    Some(age_secs / 86400) // Convert seconds to days
}

// ============================================================================
// Content Hash for Deduplication
// ============================================================================

/// Generate a content hash for deduplication across repos
pub fn content_hash(content: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(content.as_bytes());
    hex::encode(hasher.finalize())
}

/// Strip comments and blank lines from content to reduce LLM token usage.
/// Returns the stripped content and the ratio of content removed.
pub fn strip_for_prompt(content: &str, language: FileLanguage) -> (String, f64) {
    let original_len = content.len();
    let comment_prefix = language.comment_prefix();

    let stripped: String = content
        .lines()
        .filter(|line| {
            let trimmed = line.trim();
            #[allow(clippy::nonminimal_bool)]
            // Keep non-empty, non-comment lines
            let keep = !trimmed.is_empty()
                && !trimmed.starts_with(comment_prefix)
                && !trimmed.starts_with("/*")
                && !trimmed.starts_with("*/")
                && !(trimmed.starts_with('*') && !trimmed.starts_with("*/"))
                // Keep doc comments (they carry semantic value)
                || trimmed.starts_with("///")
                || trimmed.starts_with("//!");
            keep
        })
        .collect::<Vec<_>>()
        .join("\n");

    let reduction = if original_len > 0 {
        1.0 - (stripped.len() as f64 / original_len as f64)
    } else {
        0.0
    };

    (stripped, reduction)
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn analyzer() -> StaticAnalyzer {
        StaticAnalyzer::new()
    }

    #[test]
    fn test_analyze_with_todos_merges_priorities() {
        let analyzer = StaticAnalyzer::new();
        let todo_scanner = crate::todo_scanner::TodoScanner::new().unwrap();

        let content = r#"
fn process_data() {
    // FIXME: critical security issue here
    // FIXME: urgent memory leak
    // FIXME: broken auth check
    // TODO: refactor this later
    // NOTE: consider caching
    let x = 1;
    let y = 2;
    let z = 3;
    let w = 4;
    let v = 5;
    let u = 6;
    let t = 7;
    let s = 8;
    let r = 9;
    let q = 10;
}
"#;

        let result = analyzer.analyze_with_todos("src/critical.rs", content, &todo_scanner);

        // Should have found high-priority items
        assert!(
            result.signals.high_priority_todos >= 3,
            "Expected >=3 high priority, got {}",
            result.signals.high_priority_todos
        );
        assert!(result.signals.todo_scanner_total >= 4);

        // With >=3 high-priority TODOs, should be upgraded to DeepDive
        assert_eq!(
            result.recommendation,
            AnalysisRecommendation::DeepDive,
            "Should be upgraded to DeepDive with 3+ high-priority TODOs"
        );
    }

    #[test]
    fn test_analyze_with_todos_no_upgrade_for_low_priority() {
        let analyzer = StaticAnalyzer::new();
        let todo_scanner = crate::todo_scanner::TodoScanner::new().unwrap();

        let content = r#"
fn process_data() {
    // NOTE: maybe consider this
    // NOTE: optional improvement
    // TODO: refactor later
    let x = 1;
    let y = 2;
    let z = 3;
    let w = 4;
    let v = 5;
    let u = 6;
    let t = 7;
    let s = 8;
    let r = 9;
    let q = 10;
}
"#;

        let result = analyzer.analyze_with_todos("src/clean.rs", content, &todo_scanner);

        // Should NOT be upgraded to DeepDive for low/medium priority
        assert_ne!(
            result.recommendation,
            AnalysisRecommendation::DeepDive,
            "Should not be DeepDive for only low/medium priority TODOs"
        );
    }

    #[test]
    fn test_generated_file_detection() {
        let a = analyzer();

        // Protobuf-generated file
        let content = r#"// @generated by protobuf-codegen
// DO NOT EDIT
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct MyMessage {
    #[prost(string, tag = "1")]
    pub name: String,
}
"#;
        let result = a.analyze("janus.v1.rs", content);
        assert_eq!(result.recommendation, AnalysisRecommendation::Skip);
        assert_eq!(result.skip_reason, Some(SkipReason::GeneratedCode));
        assert!(result.signals.is_generated);
    }

    #[test]
    fn test_trivial_file_detection() {
        let a = analyzer();

        let content = r#"pub const VERSION: &str = "1.0.0";
"#;
        let result = a.analyze("version.rs", content);
        assert_eq!(result.recommendation, AnalysisRecommendation::Skip);
        assert_eq!(result.skip_reason, Some(SkipReason::TrivialFile));
    }

    #[test]
    fn test_small_clean_file() {
        let a = analyzer();

        // Small file with good error handling, no red flags — needs >10 code lines
        let content = r#"use std::fs;
use std::path::Path;

pub fn read_config(path: &str) -> Result<String, std::io::Error> {
    let content = fs::read_to_string(path)?;
    Ok(content)
}

pub fn parse_value(s: &str) -> Option<i32> {
    s.trim().parse().ok()
}

pub fn file_exists(path: &str) -> bool {
    Path::new(path).exists()
}

pub fn default_path() -> String {
    String::from("config.toml")
}
"#;
        let result = a.analyze("config_reader.rs", content);
        assert_eq!(result.recommendation, AnalysisRecommendation::Minimal);
        assert!(result.estimated_llm_value < 0.3);
    }

    #[test]
    fn test_unwrap_heavy_file_deep_dive() {
        let a = analyzer();

        // File with many unwraps → should trigger deep dive
        let content = r#"use std::fs;

pub fn process_data(path: &str) -> String {
    let content = fs::read_to_string(path).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
    let name = parsed.get("name").unwrap().as_str().unwrap();
    let age = parsed.get("age").unwrap().as_u64().unwrap();
    let items = parsed.get("items").unwrap().as_array().unwrap();
    let first = items.first().unwrap();
    format!("{}: {} ({})", name, age, first)
}

pub fn load_config() -> Config {
    let raw = std::fs::read_to_string("config.toml").unwrap();
    toml::from_str(&raw).unwrap()
}

pub fn connect_db() -> Connection {
    let url = std::env::var("DATABASE_URL").unwrap();
    Connection::new(&url).unwrap()
}
"#;
        let result = a.analyze("processor.rs", content);
        assert_eq!(result.recommendation, AnalysisRecommendation::DeepDive);
        assert!(result.signals.unwrap_count >= 8);
        assert!(result.estimated_llm_value > 0.8);
    }

    #[test]
    fn test_unsafe_without_safety_comment() {
        let a = analyzer();

        let content = r#"use std::ptr;

pub fn dangerous_copy(src: *const u8, dst: *mut u8, len: usize) {
    unsafe {
        ptr::copy_nonoverlapping(src, dst, len);
    }
}

pub fn also_dangerous() {
    unsafe {
        let x = *(0x1234 as *const i32);
        println!("{}", x);
    }
}
"#;
        let result = a.analyze("ffi_helpers.rs", content);
        assert_eq!(result.recommendation, AnalysisRecommendation::DeepDive);
        assert_eq!(result.signals.unsafe_without_safety_comment, 2);
    }

    #[test]
    fn test_unsafe_with_safety_comment() {
        let a = analyzer();

        let content = r#"use std::ptr;

pub fn safe_copy(src: *const u8, dst: *mut u8, len: usize) {
    // SAFETY: src and dst are valid and non-overlapping, len is correct
    unsafe {
        ptr::copy_nonoverlapping(src, dst, len);
    }
}
"#;
        let result = a.analyze("safe_ffi.rs", content);
        assert_eq!(result.signals.unsafe_with_safety_comment, 1);
        assert_eq!(result.signals.unsafe_without_safety_comment, 0);
        // With safety comment, it shouldn't force deep dive for unsafe alone
        assert_ne!(result.recommendation, AnalysisRecommendation::DeepDive);
    }

    #[test]
    fn test_security_pattern_detection() {
        let a = analyzer();

        let content = r#"use std::collections::HashMap;

pub struct DatabaseClient {
    url: String,
    pool_size: usize,
}

impl DatabaseClient {
    pub fn new(url: String) -> Self {
        Self { url, pool_size: 10 }
    }

    pub fn connect(&self) {
        let api_key = "sk-1234567890abcdef1234567890abcdef12";
        let password = "super_secret_password_123";
        let query = format!("SELECT * FROM users WHERE name = '{}'", user_input);
        println!("{}", query);
    }
}
"#;
        let result = a.analyze("database.rs", content);
        assert!(!result.signals.potential_secrets.is_empty());
        assert_eq!(result.recommendation, AnalysisRecommendation::DeepDive);
    }

    #[test]
    fn test_error_handling_ratio() {
        let a = analyzer();

        let content = r#"
pub fn good_error_handling(path: &str) -> anyhow::Result<Config> {
    let content = std::fs::read_to_string(path)?;
    let parsed: Config = toml::from_str(&content)?;
    let validated = parsed.validate()?;
    Ok(validated)
}

pub fn also_good(data: &[u8]) -> Result<String, Error> {
    let text = std::str::from_utf8(data)?;
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err(Error::Empty);
    }
    Ok(trimmed.to_string())
}
"#;
        let result = a.analyze("handlers.rs", content);
        assert!(result.signals.error_handling_ratio > 0.8);
        assert_eq!(result.signals.unwrap_count, 0);
    }

    #[test]
    fn test_code_markers_count() {
        let a = analyzer();

        let content = r#"
// TODO: Implement caching
pub fn fetch_data() -> Vec<u8> {
    // FIXME: This is broken on Windows
    let data = vec![];
    // HACK: Temporary workaround for #123
    // XXX: Race condition here
    data
}

// TODO: Add error handling
pub fn process() {}
"#;
        let result = a.analyze("data.rs", content);
        assert_eq!(result.signals.todo_count, 2);
        assert_eq!(result.signals.fixme_count, 1);
        assert_eq!(result.signals.hack_count, 1);
        assert_eq!(result.signals.xxx_count, 1);
    }

    #[test]
    fn test_complexity_estimate() {
        let a = analyzer();

        let content = r#"
pub fn complex_function(input: &str) -> Result<Output, Error> {
    if input.is_empty() {
        return Err(Error::Empty);
    }

    for item in input.split(',') {
        if item.starts_with('#') {
            match item.len() {
                1 => continue,
                2 => {
                    if item.ends_with('!') {
                        while let Some(c) = chars.next() {
                            if c == 'x' || c == 'y' {
                                process(c);
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }

    Ok(Output::default())
}

fn helper_a() -> bool { true }
fn helper_b() -> bool { false }
pub fn helper_c(x: i32) -> i32 { x + 1 }
"#;
        let result = a.analyze("complex.rs", content);
        assert!(result.signals.function_count >= 3);
        assert!(result.signals.estimated_complexity > 10);
        assert!(result.signals.max_nesting_depth >= 3);
    }

    #[test]
    fn test_content_hash() {
        let hash1 = content_hash("fn main() {}");
        let hash2 = content_hash("fn main() {}");
        let hash3 = content_hash("fn main() { println!(\"hello\"); }");

        assert_eq!(hash1, hash2);
        assert_ne!(hash1, hash3);
        assert_eq!(hash1.len(), 64); // SHA-256 = 64 hex chars
    }

    #[test]
    fn test_strip_for_prompt() {
        let content = r#"// This is a comment
/// Doc comment (should be kept)
use std::fs;

// Another comment
pub fn read_file(path: &str) -> String {
    // inline comment
    fs::read_to_string(path).unwrap()
}
"#;
        let (stripped, reduction) = strip_for_prompt(content, FileLanguage::Rust);
        assert!(reduction > 0.0);
        assert!(stripped.contains("/// Doc comment"));
        assert!(stripped.contains("pub fn read_file"));
        assert!(!stripped.contains("// This is a comment"));
    }

    #[test]
    fn test_batch_analysis() {
        let a = analyzer();
        let files = vec![
            (
                "generated.rs".to_string(),
                "// @generated\n#[derive(Message)]\npub struct Foo {}".to_string(),
            ),
            (
                "tiny.rs".to_string(),
                "pub const X: i32 = 42;".to_string(),
            ),
            (
                "normal.rs".to_string(),
                "use std::fs;\n\npub fn read(p: &str) -> Result<String, std::io::Error> {\n    let c = fs::read_to_string(p)?;\n    Ok(c)\n}\n\npub fn write(p: &str, data: &str) -> Result<(), std::io::Error> {\n    fs::write(p, data)?;\n    Ok(())\n}\n\npub fn exists(p: &str) -> bool {\n    std::path::Path::new(p).exists()\n}\n".to_string(),
            ),
        ];

        let report = analyze_batch(&a, &files);
        assert_eq!(report.total_files, 3);
        assert!(report.skip_count >= 2); // generated + tiny
        assert!(report.estimated_savings_percent > 50.0);
    }

    #[test]
    fn test_language_detection() {
        assert_eq!(FileLanguage::from_extension("main.rs"), FileLanguage::Rust);
        assert_eq!(FileLanguage::from_extension("App.kt"), FileLanguage::Kotlin);
        assert_eq!(
            FileLanguage::from_extension("index.tsx"),
            FileLanguage::TypeScript
        );
        assert_eq!(
            FileLanguage::from_extension("script.sh"),
            FileLanguage::Shell
        );
        assert_eq!(
            FileLanguage::from_extension("data.json"),
            FileLanguage::Unknown
        );
    }

    #[test]
    fn test_test_only_file_detection() {
        assert!(StaticAnalyzer::is_test_only_file("src/tests/unit_test.rs"));
        assert!(StaticAnalyzer::is_test_only_file("foo_test.rs"));
        assert!(StaticAnalyzer::is_test_only_file("component.test.ts"));
        assert!(StaticAnalyzer::is_test_only_file("component.spec.js"));
        assert!(!StaticAnalyzer::is_test_only_file("src/main.rs"));
        assert!(!StaticAnalyzer::is_test_only_file("src/config.rs"));
    }

    #[test]
    fn test_file_staleness_returns_none_for_nonexistent() {
        // This should gracefully return None for a non-existent repo
        let result = check_file_staleness(Path::new("/nonexistent/repo"), "main.rs");
        assert!(result.is_none());
    }

    #[test]
    fn test_standard_recommendation_for_medium_files() {
        let a = analyzer();

        // Medium-sized file with some issues but nothing extreme
        let content = r#"use std::collections::HashMap;
use std::fs;

pub struct ConfigManager {
    cache: HashMap<String, String>,
    path: String,
}

impl ConfigManager {
    pub fn new(path: String) -> Self {
        Self {
            cache: HashMap::new(),
            path,
        }
    }

    pub fn load(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        let content = fs::read_to_string(&self.path)?;
        for line in content.lines() {
            if let Some((key, value)) = line.split_once('=') {
                self.cache.insert(key.trim().to_string(), value.trim().to_string());
            }
        }
        Ok(())
    }

    pub fn get(&self, key: &str) -> Option<&String> {
        self.cache.get(key)
    }

    // TODO: Add write support
    pub fn set(&mut self, key: String, value: String) {
        self.cache.insert(key, value);
    }

    pub fn save(&self) -> Result<(), Box<dyn std::error::Error>> {
        let mut output = String::new();
        for (k, v) in &self.cache {
            output.push_str(&format!("{}={}\n", k, v));
        }
        fs::write(&self.path, output)?;
        Ok(())
    }

    pub fn remove(&mut self, key: &str) -> Option<String> {
        self.cache.remove(key)
    }

    pub fn keys(&self) -> Vec<&String> {
        self.cache.keys().collect()
    }

    pub fn len(&self) -> usize {
        self.cache.len()
    }

    pub fn is_empty(&self) -> bool {
        self.cache.is_empty()
    }
}
"#;
        let result = a.analyze("config_manager.rs", content);
        // Should be Minimal (small, clean, no red flags) or Standard
        assert!(
            result.recommendation == AnalysisRecommendation::Minimal
                || result.recommendation == AnalysisRecommendation::Standard
        );
    }
}
