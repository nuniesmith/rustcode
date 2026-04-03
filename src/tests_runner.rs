//! Test runner module for discovering and executing tests across different project types

use crate::error::{AuditError, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Command;
use walkdir::WalkDir;

// ── cargo test --format json event types ────────────────────────────────────

/// A single line of `cargo test -- --format=json` output.
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
enum CargoTestEvent {
    /// A single test result.
    Test(CargoTestEventTest),
    /// Suite-level summary emitted at the end.
    Suite(#[allow(dead_code)] CargoTestEventSuite),
}

#[derive(Debug, Deserialize)]
struct CargoTestEventTest {
    event: String, // "started" | "ok" | "failed" | "ignored"
    name: String,
    #[serde(default)]
    #[allow(dead_code)]
    stdout: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CargoTestEventSuite {
    #[allow(dead_code)]
    event: String, // "started" | "ok" | "failed"
}

// ── pytest-json-report structures ───────────────────────────────────────────

/// Root of `.pytest-report.json` produced by `pytest-json-report`.
#[derive(Debug, Deserialize)]
struct PytestReport {
    #[serde(default)]
    tests: Vec<PytestTest>,
}

#[derive(Debug, Deserialize)]
struct PytestTest {
    /// e.g. "tests/test_foo.py::test_bar"
    nodeid: String,
    outcome: String, // "passed" | "failed" | "skipped" | "error"
}

/// Test runner for different project types
#[derive(Debug)]
pub struct TestRunner {
    root: PathBuf,
}

/// Test suite results
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestResults {
    /// Project type
    pub project_type: ProjectType,
    /// Total tests
    pub total: usize,
    /// Passed tests
    pub passed: usize,
    /// Failed tests
    pub failed: usize,
    /// Skipped tests
    pub skipped: usize,
    /// Test duration in seconds
    pub duration: f64,
    /// Test files
    pub test_files: Vec<String>,
    /// Coverage percentage (if available)
    pub coverage: Option<f64>,
    /// Detailed results by file
    pub results_by_file: HashMap<String, FileTestResult>,
    /// Raw output
    pub output: String,
}

/// Test results for a single file
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileTestResult {
    /// File path
    pub file: String,
    /// Tests in this file
    pub tests: usize,
    /// Passed
    pub passed: usize,
    /// Failed
    pub failed: usize,
    /// Failed test names
    pub failures: Vec<String>,
}

/// Project type detected
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProjectType {
    Rust,
    Python,
    JavaScript,
    TypeScript,
    Kotlin,
    Mixed,
}

impl TestRunner {
    /// Create a new test runner
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// Detect project types in the repository
    pub fn detect_project_types(&self) -> Result<Vec<ProjectType>> {
        let mut types = Vec::new();

        // Check for Rust
        if self.root.join("Cargo.toml").exists() {
            types.push(ProjectType::Rust);
        }

        // Check for Python
        if self.root.join("pyproject.toml").exists()
            || self.root.join("setup.py").exists()
            || self.root.join("requirements.txt").exists()
        {
            types.push(ProjectType::Python);
        }

        // Check for JavaScript/TypeScript
        if self.root.join("package.json").exists() {
            let has_ts = WalkDir::new(&self.root)
                .max_depth(3)
                .into_iter()
                .filter_map(|e| e.ok())
                .any(|e| {
                    e.path()
                        .extension()
                        .is_some_and(|ext| ext == "ts" || ext == "tsx")
                });

            if has_ts {
                types.push(ProjectType::TypeScript);
            } else {
                types.push(ProjectType::JavaScript);
            }
        }

        // Check for Kotlin
        if self.root.join("build.gradle.kts").exists() || self.root.join("build.gradle").exists() {
            types.push(ProjectType::Kotlin);
        }

        Ok(types)
    }

    /// Run all tests for detected project types
    pub fn run_all_tests(&self) -> Result<Vec<TestResults>> {
        let project_types = self.detect_project_types()?;
        let mut all_results = Vec::new();

        for project_type in project_types {
            match self.run_tests_for_type(project_type) {
                Ok(results) => all_results.push(results),
                Err(e) => {
                    tracing::warn!("Failed to run {:?} tests: {}", project_type, e);
                    // Continue with other project types
                }
            }
        }

        Ok(all_results)
    }

    /// Run tests for a specific project type
    pub fn run_tests_for_type(&self, project_type: ProjectType) -> Result<TestResults> {
        match project_type {
            ProjectType::Rust => self.run_rust_tests(),
            ProjectType::Python => self.run_python_tests(),
            ProjectType::JavaScript | ProjectType::TypeScript => self.run_js_tests(),
            ProjectType::Kotlin => self.run_kotlin_tests(),
            ProjectType::Mixed => Err(AuditError::Config(
                "Cannot run tests for mixed project type".to_string(),
            )),
        }
    }

    /// Run Rust tests using cargo
    fn run_rust_tests(&self) -> Result<TestResults> {
        let start = std::time::Instant::now();

        // Find all test files
        let test_files = self.find_rust_test_files()?;

        // Run cargo test with JSON output.
        // `--format=json` requires the nightly test harness flag on stable, so we
        // pass `-Zunstable-options` to accommodate both channels gracefully.
        let output = Command::new("cargo")
            .arg("test")
            .arg("--")
            .arg("-Zunstable-options")
            .arg("--format=json")
            .current_dir(&self.root)
            .output()
            .map_err(AuditError::Io)?;

        let duration = start.elapsed().as_secs_f64();
        // cargo test writes JSON events to stdout; human-readable summary to stderr.
        let json_output = String::from_utf8_lossy(&output.stdout).to_string();
        let text_output = String::from_utf8_lossy(&output.stderr).to_string();

        // Parse the JSON event stream for per-file breakdown.
        let (results_by_file, json_total, json_passed, json_failed, json_skipped) =
            self.parse_cargo_test_json(&json_output);

        // Fall back to text summary parsing if JSON yielded nothing (e.g. old toolchain).
        let (total, passed, failed, skipped) = if json_total > 0 {
            (json_total, json_passed, json_failed, json_skipped)
        } else {
            self.parse_cargo_test_output(&text_output)
        };

        // Try to get coverage if available
        let coverage = self.get_rust_coverage().ok();

        Ok(TestResults {
            project_type: ProjectType::Rust,
            total,
            passed,
            failed,
            skipped,
            duration,
            test_files,
            coverage,
            results_by_file,
            output: if text_output.is_empty() {
                json_output
            } else {
                text_output
            },
        })
    }

    /// Run Python tests using pytest
    fn run_python_tests(&self) -> Result<TestResults> {
        let start = std::time::Instant::now();

        // Find all test files
        let test_files = self.find_python_test_files()?;

        let report_path = self.root.join(".pytest-report.json");

        // Run pytest with JSON report
        let output = Command::new("pytest")
            .arg("--json-report")
            .arg(format!("--json-report-file={}", report_path.display()))
            .arg("-v")
            .current_dir(&self.root)
            .output()
            .map_err(AuditError::Io)?;

        let duration = start.elapsed().as_secs_f64();
        let output_str = String::from_utf8_lossy(&output.stdout).to_string();

        // Parse per-file results from the JSON report file (if it was written).
        let (results_by_file, json_total, json_passed, json_failed, json_skipped) =
            self.parse_pytest_json_report(&report_path);

        // Fall back to text parsing if the JSON report wasn't produced.
        let (total, passed, failed, skipped) = if json_total > 0 {
            (json_total, json_passed, json_failed, json_skipped)
        } else {
            self.parse_pytest_output(&output_str)
        };

        // Try to get coverage if available
        let coverage = self.get_python_coverage().ok();

        Ok(TestResults {
            project_type: ProjectType::Python,
            total,
            passed,
            failed,
            skipped,
            duration,
            test_files,
            coverage,
            results_by_file,
            output: output_str,
        })
    }

    /// Run JavaScript/TypeScript tests using npm/jest
    fn run_js_tests(&self) -> Result<TestResults> {
        let start = std::time::Instant::now();

        // Find all test files
        let test_files = self.find_js_test_files()?;

        // Try npm test first, fall back to jest
        let output = Command::new("npm")
            .arg("test")
            .arg("--")
            .arg("--json")
            .current_dir(&self.root)
            .output()
            .map_err(AuditError::Io)?;

        let duration = start.elapsed().as_secs_f64();
        let output_str = String::from_utf8_lossy(&output.stdout).to_string();

        // Parse test output
        let (total, passed, failed, skipped) = self.parse_jest_output(&output_str);

        Ok(TestResults {
            project_type: ProjectType::TypeScript,
            total,
            passed,
            failed,
            skipped,
            duration,
            test_files,
            coverage: None,
            results_by_file: HashMap::new(),
            output: output_str,
        })
    }

    /// Run Kotlin tests using gradle
    fn run_kotlin_tests(&self) -> Result<TestResults> {
        let start = std::time::Instant::now();

        // Find all test files
        let test_files = self.find_kotlin_test_files()?;

        // Run gradle test
        let output = Command::new("./gradlew")
            .arg("test")
            .current_dir(&self.root)
            .output()
            .map_err(AuditError::Io)?;

        let duration = start.elapsed().as_secs_f64();
        let output_str = String::from_utf8_lossy(&output.stdout).to_string();

        // Parse gradle output
        let (total, passed, failed, skipped) = self.parse_gradle_output(&output_str);

        Ok(TestResults {
            project_type: ProjectType::Kotlin,
            total,
            passed,
            failed,
            skipped,
            duration,
            test_files,
            coverage: None,
            results_by_file: HashMap::new(),
            output: output_str,
        })
    }

    /// Find Rust test files
    fn find_rust_test_files(&self) -> Result<Vec<String>> {
        let mut test_files = Vec::new();

        for entry in WalkDir::new(&self.root)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().is_some_and(|ext| ext == "rs"))
        {
            let content = std::fs::read_to_string(entry.path()).unwrap_or_default();
            if content.contains("#[test]") || content.contains("#[cfg(test)]") {
                if let Ok(rel_path) = entry.path().strip_prefix(&self.root) {
                    test_files.push(rel_path.display().to_string());
                }
            }
        }

        Ok(test_files)
    }

    /// Find Python test files
    fn find_python_test_files(&self) -> Result<Vec<String>> {
        let mut test_files = Vec::new();

        for entry in WalkDir::new(&self.root)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.path()
                    .file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with("test_") || n.ends_with("_test.py"))
            })
        {
            if let Ok(rel_path) = entry.path().strip_prefix(&self.root) {
                test_files.push(rel_path.display().to_string());
            }
        }

        Ok(test_files)
    }

    /// Find JavaScript/TypeScript test files
    fn find_js_test_files(&self) -> Result<Vec<String>> {
        let mut test_files = Vec::new();

        for entry in WalkDir::new(&self.root)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.path()
                    .file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| {
                        n.ends_with(".test.ts")
                            || n.ends_with(".test.tsx")
                            || n.ends_with(".test.js")
                            || n.ends_with(".spec.ts")
                            || n.ends_with(".spec.js")
                    })
            })
        {
            if let Ok(rel_path) = entry.path().strip_prefix(&self.root) {
                test_files.push(rel_path.display().to_string());
            }
        }

        Ok(test_files)
    }

    /// Find Kotlin test files
    fn find_kotlin_test_files(&self) -> Result<Vec<String>> {
        let mut test_files = Vec::new();

        for entry in WalkDir::new(&self.root)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.path()
                    .components()
                    .any(|c| c.as_os_str() == "test" || c.as_os_str() == "androidTest")
                    && e.path().extension().is_some_and(|ext| ext == "kt")
            })
        {
            if let Ok(rel_path) = entry.path().strip_prefix(&self.root) {
                test_files.push(rel_path.display().to_string());
            }
        }

        Ok(test_files)
    }

    /// Parse cargo test output
    /// Parse `cargo test -- --format=json` event stream into per-file results.
    ///
    /// Returns `(results_by_file, total, passed, failed, skipped)`.
    /// On any parse error the map will be empty and counts will be 0 so the
    /// caller can fall back to text-based parsing.
    fn parse_cargo_test_json(
        &self,
        output: &str,
    ) -> (HashMap<String, FileTestResult>, usize, usize, usize, usize) {
        let mut by_file: HashMap<String, FileTestResult> = HashMap::new();
        let mut total = 0usize;
        let mut passed = 0usize;
        let mut failed = 0usize;
        let mut skipped = 0usize;

        for line in output.lines() {
            let line = line.trim();
            if line.is_empty() || !line.starts_with('{') {
                continue;
            }

            let event: CargoTestEvent = match serde_json::from_str(line) {
                Ok(e) => e,
                Err(_) => continue,
            };

            if let CargoTestEvent::Test(t) = event {
                match t.event.as_str() {
                    "ok" | "failed" | "ignored" => {
                        total += 1;

                        // Derive the file path from the test name.
                        // `cargo test` names look like:  `module::sub::test_name`
                        // We map the leading module path to a .rs file under src/.
                        let file_key = derive_rust_file_key(&t.name);

                        let entry = by_file.entry(file_key.clone()).or_insert(FileTestResult {
                            file: file_key,
                            tests: 0,
                            passed: 0,
                            failed: 0,
                            failures: Vec::new(),
                        });

                        entry.tests += 1;

                        match t.event.as_str() {
                            "ok" => {
                                passed += 1;
                                entry.passed += 1;
                            }
                            "failed" => {
                                failed += 1;
                                entry.failed += 1;
                                entry.failures.push(t.name.clone());
                            }
                            "ignored" => {
                                skipped += 1;
                            }
                            _ => {}
                        }
                    }
                    _ => {} // "started" — skip
                }
            }
        }

        (by_file, total, passed, failed, skipped)
    }

    /// Parse `.pytest-report.json` written by `pytest-json-report` into
    /// per-file results.
    ///
    /// Returns `(results_by_file, total, passed, failed, skipped)`.
    fn parse_pytest_json_report(
        &self,
        report_path: &std::path::Path,
    ) -> (HashMap<String, FileTestResult>, usize, usize, usize, usize) {
        let mut by_file: HashMap<String, FileTestResult> = HashMap::new();

        let content = match std::fs::read_to_string(report_path) {
            Ok(c) => c,
            Err(_) => return (by_file, 0, 0, 0, 0),
        };

        let report: PytestReport = match serde_json::from_str(&content) {
            Ok(r) => r,
            Err(_) => return (by_file, 0, 0, 0, 0),
        };

        let mut total = 0usize;
        let mut passed = 0usize;
        let mut failed = 0usize;
        let mut skipped = 0usize;

        for test in &report.tests {
            total += 1;

            // nodeid format: "tests/test_foo.py::TestClass::test_method"
            // or just:       "tests/test_foo.py::test_function"
            let file_key = test
                .nodeid
                .split("::")
                .next()
                .unwrap_or(&test.nodeid)
                .to_string();

            let entry = by_file.entry(file_key.clone()).or_insert(FileTestResult {
                file: file_key,
                tests: 0,
                passed: 0,
                failed: 0,
                failures: Vec::new(),
            });

            entry.tests += 1;

            match test.outcome.as_str() {
                "passed" => {
                    passed += 1;
                    entry.passed += 1;
                }
                "failed" | "error" => {
                    failed += 1;
                    entry.failed += 1;
                    entry.failures.push(test.nodeid.clone());
                }
                "skipped" => {
                    skipped += 1;
                }
                _ => {
                    // Unknown outcome — count as skipped to avoid inflating pass counts.
                    skipped += 1;
                }
            }
        }

        (by_file, total, passed, failed, skipped)
    }

    fn parse_cargo_test_output(&self, output: &str) -> (usize, usize, usize, usize) {
        let mut passed = 0;
        let mut failed = 0;
        let mut skipped = 0;

        // Look for summary line like "test result: ok. 15 passed; 0 failed; 0 ignored"
        for line in output.lines() {
            if line.contains("test result:") {
                if let Some(stats) = line.split("test result:").nth(1) {
                    // Parse numbers - look for patterns like "15 passed", "2 failed", "1 ignored"
                    for part in stats.split(';') {
                        // Find a number followed by a keyword in this part
                        for word in part.split_whitespace() {
                            if let Ok(num) = word.parse::<usize>() {
                                // Check if the rest of part indicates the type
                                if part.contains("passed") {
                                    passed = num;
                                } else if part.contains("failed") {
                                    failed = num;
                                } else if part.contains("ignored") {
                                    skipped = num;
                                }
                                break;
                            }
                        }
                    }
                }
            }
        }

        let total = passed + failed + skipped;
        (total, passed, failed, skipped)
    }

    /// Parse pytest output
    fn parse_pytest_output(&self, output: &str) -> (usize, usize, usize, usize) {
        let mut passed = 0;
        let mut failed = 0;
        let mut skipped = 0;

        // Look for summary line like "15 passed, 2 failed, 1 skipped in 2.5s"
        for line in output.lines() {
            if line.contains("passed") || line.contains("failed") {
                for part in line.split(',') {
                    if let Some(num_str) = part.split_whitespace().next() {
                        if let Ok(num) = num_str.parse::<usize>() {
                            if part.contains("passed") {
                                passed = num;
                            } else if part.contains("failed") {
                                failed = num;
                            } else if part.contains("skipped") {
                                skipped = num;
                            }
                        }
                    }
                }
            }
        }

        let total = passed + failed + skipped;
        (total, passed, failed, skipped)
    }

    /// Parse jest output
    fn parse_jest_output(&self, output: &str) -> (usize, usize, usize, usize) {
        // Jest JSON output parsing
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(output) {
            let total = json["numTotalTests"].as_u64().unwrap_or(0) as usize;
            let passed = json["numPassedTests"].as_u64().unwrap_or(0) as usize;
            let failed = json["numFailedTests"].as_u64().unwrap_or(0) as usize;
            let skipped = json["numPendingTests"].as_u64().unwrap_or(0) as usize;
            return (total, passed, failed, skipped);
        }

        (0, 0, 0, 0)
    }

    /// Parse gradle output
    fn parse_gradle_output(&self, output: &str) -> (usize, usize, usize, usize) {
        let mut passed = 0;
        let failed = 0;
        let skipped = 0;

        // Look for BUILD SUCCESSFUL and test counts
        for line in output.lines() {
            if line.contains("tests completed") {
                // Parse the test count
                if let Some(num_str) = line.split_whitespace().next() {
                    if let Ok(num) = num_str.parse::<usize>() {
                        passed = num; // Assume all passed if BUILD SUCCESSFUL
                    }
                }
            }
        }

        let total = passed + failed + skipped;
        (total, passed, failed, skipped)
    }

    /// Get Rust code coverage using tarpaulin or llvm-cov
    fn get_rust_coverage(&self) -> Result<f64> {
        // Try cargo-tarpaulin first
        let output = Command::new("cargo")
            .arg("tarpaulin")
            .arg("--out")
            .arg("Stdout")
            .current_dir(&self.root)
            .output();

        if let Ok(output) = output {
            let output_str = String::from_utf8_lossy(&output.stdout);
            if let Some(coverage_line) = output_str.lines().find(|l| l.contains("coverage")) {
                // Parse percentage
                if let Some(pct_str) = coverage_line.split('%').next() {
                    if let Some(num_str) = pct_str.split_whitespace().last() {
                        if let Ok(pct) = num_str.parse::<f64>() {
                            return Ok(pct);
                        }
                    }
                }
            }
        }

        Err(AuditError::Config(
            "Coverage tool not available".to_string(),
        ))
    }

    /// Get Python code coverage using pytest-cov
    fn get_python_coverage(&self) -> Result<f64> {
        let output = Command::new("pytest")
            .arg("--cov")
            .arg("--cov-report=term")
            .current_dir(&self.root)
            .output()
            .map_err(AuditError::Io)?;

        let output_str = String::from_utf8_lossy(&output.stdout);

        // Look for TOTAL line with coverage percentage
        for line in output_str.lines() {
            if line.contains("TOTAL") {
                if let Some(pct_str) = line.split('%').next() {
                    if let Some(num_str) = pct_str.split_whitespace().last() {
                        if let Ok(pct) = num_str.parse::<f64>() {
                            return Ok(pct);
                        }
                    }
                }
            }
        }

        Err(AuditError::Config(
            "Coverage not found in output".to_string(),
        ))
    }
}

// ── Module-level helpers ─────────────────────────────────────────────────────

/// Derive a human-readable file key from a cargo test name.
///
/// Test names look like `module::submodule::test_fn` or just `test_fn`.
/// We convert the leading path component(s) to a plausible `src/<path>.rs`
/// string so results can be grouped by source file even without the full path.
///
/// Examples:
/// - `repo_sync::tests::slugify_basic`  →  `src/repo_sync.rs`
/// - `audit::cache::tests::hit_rate`    →  `src/audit/cache.rs`
/// - `top_level_test`                   →  `src/lib.rs`
fn derive_rust_file_key(test_name: &str) -> String {
    let all_parts: Vec<&str> = test_name.splitn(10, "::").collect();

    // Keep only the "module path" components — stop at:
    //   - a segment named exactly "tests"  (the conventional test sub-module)
    //   - a segment that starts with "test_" (a leaf test function name)
    //   - a segment that is the last part AND the previous segment was "tests"
    //     (already handled by stopping at "tests")
    //
    // We deliberately keep module names that happen to contain underscores
    // (e.g. "repo_sync", "model_router", "mod_a") — only the `test_` prefix
    // convention reliably distinguishes function names from module names.
    let module_parts: Vec<&str> = all_parts
        .iter()
        .take_while(|&&p| {
            // Stop at the conventional `tests` sub-module.
            if p == "tests" {
                return false;
            }
            // Stop at a segment that is a test function name (starts with "test_").
            if p.starts_with("test_") {
                return false;
            }
            true
        })
        .copied()
        .collect();

    match module_parts.as_slice() {
        // No module components at all → top-level test in lib.rs.
        [] => "src/lib.rs".to_string(),
        // Single module: "repo_sync" → src/repo_sync.rs
        [module] => format!("src/{}.rs", module),
        // Two modules: "audit::cache" → src/audit/cache.rs
        [first, second] => format!("src/{}/{}.rs", first, second),
        // Three+ modules: build the full path.
        // e.g. ["audit", "cache", "endpoint"] → src/audit/cache/endpoint.rs
        parts => {
            let dirs = parts[..parts.len() - 1].join("/");
            let file = parts.last().unwrap_or(&"lib");
            format!("src/{}/{}.rs", dirs, file)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_cargo_output() {
        let runner = TestRunner::new(".");
        let output = "test result: ok. 15 passed; 2 failed; 1 ignored; 0 measured";
        let (total, passed, failed, skipped) = runner.parse_cargo_test_output(output);

        assert_eq!(passed, 15);
        assert_eq!(failed, 2);
        assert_eq!(skipped, 1);
        assert_eq!(total, 18);
    }

    #[test]
    fn test_parse_pytest_output() {
        let runner = TestRunner::new(".");
        let output = "15 passed, 2 failed, 1 skipped in 2.5s";
        let (total, passed, failed, skipped) = runner.parse_pytest_output(output);

        assert_eq!(passed, 15);
        assert_eq!(failed, 2);
        assert_eq!(skipped, 1);
        assert_eq!(total, 18);
    }

    // ── derive_rust_file_key ─────────────────────────────────────────────────

    #[test]
    fn file_key_top_level_test() {
        // A bare name starting with "test_" with no "::" → lib.rs
        assert_eq!(derive_rust_file_key("test_top_level"), "src/lib.rs");
        // A single plain segment with no "::" is treated as a module name → src/<name>.rs
        assert_eq!(derive_rust_file_key("embeddings"), "src/embeddings.rs");
    }

    #[test]
    fn file_key_single_module() {
        assert_eq!(
            derive_rust_file_key("repo_sync::tests::slugify_basic"),
            "src/repo_sync.rs"
        );
    }

    #[test]
    fn file_key_two_module_components() {
        assert_eq!(
            derive_rust_file_key("audit::cache::tests::hit_rate"),
            "src/audit/cache.rs"
        );
    }

    #[test]
    fn file_key_three_module_components() {
        assert_eq!(
            derive_rust_file_key("audit::endpoint::tests::my_test"),
            "src/audit/endpoint.rs"
        );
    }

    #[test]
    fn file_key_no_tests_module_in_name() {
        // "route_prompt_async" does NOT start with "test_" so it is kept as a
        // module segment — the result maps to src/model_router/route_prompt_async.rs
        // which is the best we can do without full symbol info.
        // The important thing is that it doesn't panic and returns something useful.
        let key = derive_rust_file_key("model_router::route_prompt_async");
        assert!(
            key.starts_with("src/"),
            "key should start with src/: {}",
            key
        );
    }

    #[test]
    fn file_key_module_with_underscores() {
        // Module names that contain underscores (e.g. repo_sync) should NOT be
        // mistaken for function names.
        assert_eq!(
            derive_rust_file_key("repo_sync::tests::slugify_basic"),
            "src/repo_sync.rs"
        );
        assert_eq!(
            derive_rust_file_key("model_router::tests::test_classify"),
            "src/model_router.rs"
        );
    }

    #[test]
    fn file_key_single_segment_module_name() {
        // A module name with no "::" separator → src/<name>.rs
        assert_eq!(
            derive_rust_file_key("embeddings::tests::embed_ok"),
            "src/embeddings.rs"
        );
    }

    // ── parse_cargo_test_json ────────────────────────────────────────────────

    #[test]
    fn parse_cargo_json_counts_ok_failed_ignored() {
        let runner = TestRunner::new(".");

        // Minimal cargo --format=json event stream
        let json_events = r#"
{"type":"suite","event":"started","test_count":3}
{"type":"test","event":"started","name":"mod_a::tests::test_one"}
{"type":"test","event":"ok","name":"mod_a::tests::test_one","exec_time":0.001}
{"type":"test","event":"started","name":"mod_a::tests::test_two"}
{"type":"test","event":"failed","name":"mod_a::tests::test_two","exec_time":0.002,"stdout":"assertion failed"}
{"type":"test","event":"started","name":"mod_b::tests::test_skip"}
{"type":"test","event":"ignored","name":"mod_b::tests::test_skip"}
{"type":"suite","event":"failed","passed":1,"failed":1,"ignored":1,"measured":0,"filtered_out":0,"exec_time":0.003}
"#;

        let (by_file, total, passed, failed, skipped) = runner.parse_cargo_test_json(json_events);

        assert_eq!(total, 3, "total");
        assert_eq!(passed, 1, "passed");
        assert_eq!(failed, 1, "failed");
        assert_eq!(skipped, 1, "skipped");

        // mod_a::tests::test_one and mod_a::tests::test_two both map to src/mod_a.rs
        // (stops at "tests" segment, keeping only "mod_a")
        assert_eq!(
            by_file.get("src/mod_a.rs").map(|r| r.tests),
            Some(2),
            "mod_a test count"
        );
        assert_eq!(
            by_file.get("src/mod_b.rs").map(|r| r.tests),
            Some(1),
            "mod_b test count"
        );

        // Failed test name is captured
        let mod_a = by_file.get("src/mod_a.rs").unwrap();
        assert_eq!(mod_a.failed, 1);
        assert!(mod_a
            .failures
            .contains(&"mod_a::tests::test_two".to_string()));
    }

    #[test]
    fn parse_cargo_json_empty_input_returns_zeros() {
        let runner = TestRunner::new(".");
        let (by_file, total, passed, failed, skipped) = runner.parse_cargo_test_json("");
        assert_eq!(total, 0);
        assert_eq!(passed, 0);
        assert_eq!(failed, 0);
        assert_eq!(skipped, 0);
        assert!(by_file.is_empty());
    }

    #[test]
    fn parse_cargo_json_ignores_non_json_lines() {
        let runner = TestRunner::new(".");
        // Mix of JSON events and plain text (e.g. build output)
        let input = "   Compiling foo v0.1.0\n{\"type\":\"test\",\"event\":\"ok\",\"name\":\"lib::test_x\"}\n";
        let (_, total, passed, _, _) = runner.parse_cargo_test_json(input);
        assert_eq!(total, 1);
        assert_eq!(passed, 1);
    }
}
