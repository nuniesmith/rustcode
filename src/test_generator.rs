//! # Test Generator Module
//!
//! AI-powered test generation from existing code.
//!
//! ## Features
//!
//! - Generate unit tests from functions
//! - Identify test gaps in coverage
//! - Create test fixtures and mock data
//! - Support for multiple test frameworks
//! - Property-based test suggestions
//!
//! ## Usage
//!
//! ```rust,no_run
//! use rustcode::test_generator::TestGenerator;
//! use rustcode::db::Database;
//!
//! #[tokio::main]
//! async fn main() -> anyhow::Result<()> {
//!     let db = Database::new("data/rustcode.db").await?;
//!     let generator = TestGenerator::new(db).await?;
//!
//!     // Generate tests for a file
//!     let tests = generator.generate_tests_for_file("src/utils.rs").await?;
//!     println!("{}", tests.format_as_code());
//!
//!     Ok(())
//! }
//! ```

use crate::db::Database;
use crate::grok_client::GrokClient;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Test generator with AI-powered analysis
pub struct TestGenerator {
    grok_client: GrokClient,
}

/// Generated test suite for a file or function
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeneratedTests {
    /// Source file path
    pub source_file: String,
    /// Target function (if specific)
    pub target_function: Option<String>,
    /// Generated test cases
    pub test_cases: Vec<TestCase>,
    /// Test framework recommendation
    pub framework: TestFramework,
    /// Additional setup code needed
    pub setup_code: Option<String>,
    /// Fixture data needed
    pub fixtures: Vec<Fixture>,
    /// Estimated coverage improvement
    pub coverage_improvement: f64,
}

/// Individual test case
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestCase {
    /// Test name
    pub name: String,
    /// Test description/purpose
    pub description: String,
    /// Test code
    pub code: String,
    /// Test type
    pub test_type: TestType,
    /// Expected assertions
    pub assertions: Vec<String>,
    /// Dependencies/mocks needed
    pub dependencies: Vec<String>,
}

/// Type of test
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum TestType {
    /// Unit test
    Unit,
    /// Integration test
    Integration,
    /// Property-based test
    Property,
    /// Edge case test
    EdgeCase,
    /// Error handling test
    ErrorHandling,
    /// Performance test
    Performance,
}

/// Test framework
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum TestFramework {
    /// Built-in Rust test framework
    RustTest,
    /// Tokio test for async
    TokioTest,
    /// Proptest for property-based testing
    Proptest,
    /// Criterion for benchmarking
    Criterion,
}

/// Test fixture data
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Fixture {
    /// Fixture name
    pub name: String,
    /// Fixture type
    pub fixture_type: String,
    /// Sample data
    pub sample_data: String,
    /// Creation code
    pub creation_code: String,
}

/// Test gap analysis result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestGapAnalysis {
    /// File analyzed
    pub file_path: String,
    /// Total functions
    pub total_functions: usize,
    /// Tested functions
    pub tested_functions: usize,
    /// Untested functions
    pub untested_functions: Vec<UntestFunction>,
    /// Missing test types
    pub missing_test_types: Vec<TestType>,
    /// Coverage estimate
    pub estimated_coverage: f64,
    /// Recommendations
    pub recommendations: Vec<String>,
}

/// Untested function details
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UntestFunction {
    /// Function name
    pub name: String,
    /// Function signature
    pub signature: String,
    /// Complexity score (higher = more important to test)
    pub complexity: u32,
    /// Public visibility
    pub is_public: bool,
    /// Recommended test cases
    pub recommended_tests: Vec<String>,
}

impl TestGenerator {
    /// Create a new test generator
    pub async fn new(db: Database) -> Result<Self> {
        let grok_client = GrokClient::from_env(db).await?;
        Ok(Self { grok_client })
    }

    /// Generate tests for a file
    pub async fn generate_tests_for_file(
        &self,
        file_path: impl AsRef<Path>,
    ) -> Result<GeneratedTests> {
        let file_path = file_path.as_ref();
        let content = std::fs::read_to_string(file_path)
            .with_context(|| format!("Failed to read file: {}", file_path.display()))?;

        self.generate_tests_from_content(file_path.to_string_lossy().to_string(), &content, None)
            .await
    }

    /// Generate tests for a specific function
    pub async fn generate_tests_for_function(
        &self,
        file_path: impl AsRef<Path>,
        function_name: &str,
    ) -> Result<GeneratedTests> {
        let file_path = file_path.as_ref();
        let content = std::fs::read_to_string(file_path)
            .with_context(|| format!("Failed to read file: {}", file_path.display()))?;

        self.generate_tests_from_content(
            file_path.to_string_lossy().to_string(),
            &content,
            Some(function_name),
        )
        .await
    }

    /// Generate tests from code content
    async fn generate_tests_from_content(
        &self,
        file_path: String,
        content: &str,
        target_function: Option<&str>,
    ) -> Result<GeneratedTests> {
        let prompt = self.build_test_generation_prompt(content, target_function);

        let response = self
            .grok_client
            .ask(&prompt, Some(content))
            .await
            .context("Failed to generate tests with AI")?;

        self.parse_test_response(&response, file_path, target_function)
    }

    /// Analyze test gaps in a file or directory
    pub async fn analyze_test_gaps(&self, path: impl AsRef<Path>) -> Result<Vec<TestGapAnalysis>> {
        let path = path.as_ref();
        let mut analyses = Vec::new();

        if path.is_file() {
            if let Some(analysis) = self.analyze_file_test_gaps(path).await? {
                analyses.push(analysis);
            }
        } else if path.is_dir() {
            analyses = self.analyze_directory_test_gaps(path).await?;
        }

        Ok(analyses)
    }

    /// Analyze test gaps for a single file
    async fn analyze_file_test_gaps(&self, file_path: &Path) -> Result<Option<TestGapAnalysis>> {
        if !self.is_source_file(file_path) {
            return Ok(None);
        }

        let content = std::fs::read_to_string(file_path)
            .with_context(|| format!("Failed to read file: {}", file_path.display()))?;

        let prompt = format!(
            r#"Analyze this code for test coverage gaps. Return a JSON object with:
{{
  "total_functions": <number>,
  "tested_functions": <number>,
  "untested_functions": [
    {{
      "name": "function_name",
      "signature": "fn signature",
      "complexity": <1-10>,
      "is_public": <bool>,
      "recommended_tests": ["test description"]
    }}
  ],
  "missing_test_types": ["unit", "integration", "edge_case", "error_handling"],
  "estimated_coverage": <0-100>,
  "recommendations": ["recommendation text"]
}}

Code to analyze:
```
{}
```

Focus on:
1. Functions without tests
2. Edge cases not covered
3. Error handling gaps
4. Missing integration tests
5. Complex functions needing more tests"#,
            content
        );

        let response = self.grok_client.ask(&prompt, Some(&content)).await?;

        self.parse_gap_analysis(&response, file_path.to_string_lossy().to_string())
    }

    /// Analyze test gaps for a directory
    async fn analyze_directory_test_gaps(&self, dir_path: &Path) -> Result<Vec<TestGapAnalysis>> {
        let mut analyses = Vec::new();

        for entry in walkdir::WalkDir::new(dir_path)
            .follow_links(true)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_file())
        {
            let path = entry.path();
            if self.is_source_file(path) {
                if let Some(analysis) = self.analyze_file_test_gaps(path).await? {
                    analyses.push(analysis);
                }
            }
        }

        Ok(analyses)
    }

    /// Generate test fixtures for data structures
    pub async fn generate_fixtures(&self, file_path: impl AsRef<Path>) -> Result<Vec<Fixture>> {
        let file_path = file_path.as_ref();
        let content = std::fs::read_to_string(file_path)
            .with_context(|| format!("Failed to read file: {}", file_path.display()))?;

        let prompt = format!(
            r#"Analyze this code and generate test fixtures for the data structures. Return JSON array:
[
  {{
    "name": "fixture_name",
    "fixture_type": "StructName",
    "sample_data": "example values",
    "creation_code": "Rust code to create fixture"
  }}
]

Code:
```
{}
```

Generate fixtures for:
1. Struct instances with realistic test data
2. Valid and invalid examples
3. Edge cases (empty, max, min values)
4. Common use case scenarios"#,
            content
        );

        let response = self.grok_client.ask(&prompt, Some(&content)).await?;

        self.parse_fixtures(&response)
    }

    /// Build test generation prompt
    fn build_test_generation_prompt(&self, content: &str, target_function: Option<&str>) -> String {
        let focus = if let Some(func) = target_function {
            format!(
                "Generate comprehensive tests specifically for the `{}` function.",
                func
            )
        } else {
            "Generate comprehensive tests for all public functions in this file.".to_string()
        };

        format!(
            r#"You are a test generation expert. Analyze this Rust code and generate comprehensive unit tests.

{}

Return a JSON object with this structure:
{{
  "test_cases": [
    {{
      "name": "test_function_name",
      "description": "what this test validates",
      "code": "complete Rust test code",
      "test_type": "unit|integration|property|edge_case|error_handling|performance",
      "assertions": ["what is being asserted"],
      "dependencies": ["mocks or fixtures needed"]
    }}
  ],
  "framework": "rust_test|tokio_test|proptest|criterion",
  "setup_code": "optional common setup code",
  "fixtures": [
    {{
      "name": "fixture_name",
      "fixture_type": "Type",
      "sample_data": "example data",
      "creation_code": "code to create fixture"
    }}
  ],
  "coverage_improvement": <estimated percentage improvement>
}}

Code to test:
```rust
{}
```

Generate tests that cover:
1. Happy path scenarios
2. Edge cases (empty, null, boundary values)
3. Error conditions and error handling
4. Invalid inputs
5. Concurrent access (if applicable)
6. Performance edge cases

Make tests:
- Clear and readable
- Independent (no test depends on another)
- Deterministic (same result every time)
- Fast to execute
- Using idiomatic Rust test patterns"#,
            focus, content
        )
    }

    /// Parse test generation response
    fn parse_test_response(
        &self,
        response: &str,
        file_path: String,
        target_function: Option<&str>,
    ) -> Result<GeneratedTests> {
        // Try to extract JSON from response
        let json_str = self.extract_json(response);

        match serde_json::from_str::<serde_json::Value>(&json_str) {
            Ok(json) => {
                let test_cases = json["test_cases"]
                    .as_array()
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|tc| {
                                Some(TestCase {
                                    name: tc["name"].as_str()?.to_string(),
                                    description: tc["description"].as_str()?.to_string(),
                                    code: tc["code"].as_str()?.to_string(),
                                    test_type: self.parse_test_type(tc["test_type"].as_str()?),
                                    assertions: tc["assertions"]
                                        .as_array()?
                                        .iter()
                                        .filter_map(|a| a.as_str().map(String::from))
                                        .collect(),
                                    dependencies: tc["dependencies"]
                                        .as_array()
                                        .map(|deps| {
                                            deps.iter()
                                                .filter_map(|d| d.as_str().map(String::from))
                                                .collect()
                                        })
                                        .unwrap_or_default(),
                                })
                            })
                            .collect()
                    })
                    .unwrap_or_default();

                let framework = json["framework"]
                    .as_str()
                    .map(|s| self.parse_framework(s))
                    .unwrap_or(TestFramework::RustTest);

                let setup_code = json["setup_code"].as_str().map(String::from);

                let fixtures = json["fixtures"]
                    .as_array()
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|f| {
                                Some(Fixture {
                                    name: f["name"].as_str()?.to_string(),
                                    fixture_type: f["fixture_type"].as_str()?.to_string(),
                                    sample_data: f["sample_data"].as_str()?.to_string(),
                                    creation_code: f["creation_code"].as_str()?.to_string(),
                                })
                            })
                            .collect()
                    })
                    .unwrap_or_default();

                let coverage_improvement = json["coverage_improvement"].as_f64().unwrap_or(0.0);

                Ok(GeneratedTests {
                    source_file: file_path,
                    target_function: target_function.map(String::from),
                    test_cases,
                    framework,
                    setup_code,
                    fixtures,
                    coverage_improvement,
                })
            }
            Err(_) => {
                // Fallback: create basic test from response
                Ok(GeneratedTests {
                    source_file: file_path,
                    target_function: target_function.map(String::from),
                    test_cases: vec![TestCase {
                        name: "generated_test".to_string(),
                        description: "AI-generated test".to_string(),
                        code: response.to_string(),
                        test_type: TestType::Unit,
                        assertions: vec![],
                        dependencies: vec![],
                    }],
                    framework: TestFramework::RustTest,
                    setup_code: None,
                    fixtures: vec![],
                    coverage_improvement: 0.0,
                })
            }
        }
    }

    /// Parse gap analysis response
    fn parse_gap_analysis(
        &self,
        response: &str,
        file_path: String,
    ) -> Result<Option<TestGapAnalysis>> {
        let json_str = self.extract_json(response);

        match serde_json::from_str::<serde_json::Value>(&json_str) {
            Ok(json) => {
                let total_functions = json["total_functions"].as_u64().unwrap_or(0) as usize;
                let tested_functions = json["tested_functions"].as_u64().unwrap_or(0) as usize;

                let untested_functions = json["untested_functions"]
                    .as_array()
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|uf| {
                                Some(UntestFunction {
                                    name: uf["name"].as_str()?.to_string(),
                                    signature: uf["signature"].as_str()?.to_string(),
                                    complexity: uf["complexity"].as_u64().unwrap_or(1) as u32,
                                    is_public: uf["is_public"].as_bool().unwrap_or(false),
                                    recommended_tests: uf["recommended_tests"]
                                        .as_array()?
                                        .iter()
                                        .filter_map(|t| t.as_str().map(String::from))
                                        .collect(),
                                })
                            })
                            .collect()
                    })
                    .unwrap_or_default();

                let missing_test_types = json["missing_test_types"]
                    .as_array()
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|t| t.as_str().map(|s| self.parse_test_type(s)))
                            .collect()
                    })
                    .unwrap_or_default();

                let estimated_coverage = json["estimated_coverage"].as_f64().unwrap_or(0.0);

                let recommendations = json["recommendations"]
                    .as_array()
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|r| r.as_str().map(String::from))
                            .collect()
                    })
                    .unwrap_or_default();

                Ok(Some(TestGapAnalysis {
                    file_path,
                    total_functions,
                    tested_functions,
                    untested_functions,
                    missing_test_types,
                    estimated_coverage,
                    recommendations,
                }))
            }
            Err(_) => Ok(None),
        }
    }

    /// Parse fixtures response
    fn parse_fixtures(&self, response: &str) -> Result<Vec<Fixture>> {
        let json_str = self.extract_json(response);

        match serde_json::from_str::<serde_json::Value>(&json_str) {
            Ok(json) => {
                if let Some(arr) = json.as_array() {
                    Ok(arr
                        .iter()
                        .filter_map(|f| {
                            Some(Fixture {
                                name: f["name"].as_str()?.to_string(),
                                fixture_type: f["fixture_type"].as_str()?.to_string(),
                                sample_data: f["sample_data"].as_str()?.to_string(),
                                creation_code: f["creation_code"].as_str()?.to_string(),
                            })
                        })
                        .collect())
                } else {
                    Ok(vec![])
                }
            }
            Err(_) => Ok(vec![]),
        }
    }

    /// Extract JSON from response (handles code blocks)
    fn extract_json(&self, response: &str) -> String {
        // Try to find JSON in code blocks
        if let Some(start) = response.find("```json") {
            if let Some(end) = response[start..].find("```") {
                let json_start = start + 7; // length of "```json"
                return response[json_start..start + end].trim().to_string();
            }
        }

        // Try to find JSON object
        if let Some(start) = response.find('{') {
            if let Some(end) = response.rfind('}') {
                return response[start..=end].to_string();
            }
        }

        // Try to find JSON array
        if let Some(start) = response.find('[') {
            if let Some(end) = response.rfind(']') {
                return response[start..=end].to_string();
            }
        }

        response.to_string()
    }

    /// Parse test type from string
    fn parse_test_type(&self, s: &str) -> TestType {
        match s.to_lowercase().as_str() {
            "unit" => TestType::Unit,
            "integration" => TestType::Integration,
            "property" => TestType::Property,
            "edge_case" | "edge" => TestType::EdgeCase,
            "error_handling" | "error" => TestType::ErrorHandling,
            "performance" | "perf" => TestType::Performance,
            _ => TestType::Unit,
        }
    }

    /// Parse test framework from string
    fn parse_framework(&self, s: &str) -> TestFramework {
        match s.to_lowercase().as_str() {
            "rust_test" | "test" => TestFramework::RustTest,
            "tokio_test" | "tokio" => TestFramework::TokioTest,
            "proptest" | "property" => TestFramework::Proptest,
            "criterion" | "bench" => TestFramework::Criterion,
            _ => TestFramework::RustTest,
        }
    }

    /// Check if file is a source file
    fn is_source_file(&self, path: &Path) -> bool {
        if let Some(ext) = path.extension() {
            matches!(ext.to_str().unwrap_or(""), "rs" | "py" | "js" | "ts")
        } else {
            false
        }
    }
}

impl GeneratedTests {
    /// Format tests as compilable Rust code
    pub fn format_as_code(&self) -> String {
        let mut output = String::new();

        // Add module header
        output.push_str(&format!("// Tests generated for: {}\n", self.source_file));
        if let Some(func) = &self.target_function {
            output.push_str(&format!("// Target function: {}\n", func));
        }
        output.push_str(&format!("// Framework: {:?}\n", self.framework));
        output.push_str(&format!(
            "// Estimated coverage improvement: {:.1}%\n\n",
            self.coverage_improvement
        ));

        // Add test module
        output.push_str("#[cfg(test)]\n");
        output.push_str("mod tests {\n");
        output.push_str("    use super::*;\n\n");

        // Add setup code if any
        if let Some(setup) = &self.setup_code {
            output.push_str("    // Common setup\n");
            output.push_str(&format!("    {}\n\n", setup));
        }

        // Add fixtures
        for fixture in &self.fixtures {
            output.push_str(&format!("    // Fixture: {}\n", fixture.name));
            output.push_str(&format!("    {}\n\n", fixture.creation_code));
        }

        // Add test cases
        for test in &self.test_cases {
            output.push_str(&format!("    // {}\n", test.description));
            output.push_str(&format!("    // Type: {:?}\n", test.test_type));
            if !test.assertions.is_empty() {
                output.push_str(&format!(
                    "    // Assertions: {}\n",
                    test.assertions.join(", ")
                ));
            }
            output.push_str(&format!("    {}\n\n", test.code));
        }

        output.push_str("}\n");

        output
    }

    /// Format as markdown documentation
    pub fn format_as_markdown(&self) -> String {
        let mut output = String::new();

        output.push_str(&format!("# Generated Tests for {}\n\n", self.source_file));

        if let Some(func) = &self.target_function {
            output.push_str(&format!("**Target Function:** `{}`\n\n", func));
        }

        output.push_str(&format!("**Framework:** {:?}\n", self.framework));
        output.push_str(&format!(
            "**Coverage Improvement:** {:.1}%\n\n",
            self.coverage_improvement
        ));

        output.push_str("---\n\n");
        output.push_str(&format!("## Test Cases ({})\n\n", self.test_cases.len()));

        for (i, test) in self.test_cases.iter().enumerate() {
            output.push_str(&format!("### {}. {}\n\n", i + 1, test.name));
            output.push_str(&format!("**Description:** {}\n\n", test.description));
            output.push_str(&format!("**Type:** {:?}\n\n", test.test_type));

            if !test.assertions.is_empty() {
                output.push_str("**Assertions:**\n");
                for assertion in &test.assertions {
                    output.push_str(&format!("- {}\n", assertion));
                }
                output.push('\n');
            }

            output.push_str("```rust\n");
            output.push_str(&test.code);
            output.push_str("\n```\n\n");
        }

        if !self.fixtures.is_empty() {
            output.push_str("## Fixtures\n\n");
            for fixture in &self.fixtures {
                output.push_str(&format!("### {}\n\n", fixture.name));
                output.push_str(&format!("**Type:** `{}`\n\n", fixture.fixture_type));
                output.push_str(&format!("**Sample Data:** {}\n\n", fixture.sample_data));
            }
        }

        output
    }
}

impl TestGapAnalysis {
    /// Format gap analysis as markdown report
    pub fn format_as_markdown(&self) -> String {
        let mut output = String::new();

        output.push_str(&format!("# Test Gap Analysis: {}\n\n", self.file_path));

        output.push_str("## Coverage Summary\n\n");
        output.push_str(&format!(
            "- **Total Functions:** {}\n",
            self.total_functions
        ));
        output.push_str(&format!(
            "- **Tested Functions:** {}\n",
            self.tested_functions
        ));
        output.push_str(&format!(
            "- **Untested Functions:** {}\n",
            self.untested_functions.len()
        ));
        output.push_str(&format!(
            "- **Estimated Coverage:** {:.1}%\n\n",
            self.estimated_coverage
        ));

        if !self.untested_functions.is_empty() {
            output.push_str("## Untested Functions\n\n");
            for func in &self.untested_functions {
                let visibility = if func.is_public { "pub " } else { "" };
                output.push_str(&format!("### {}{}()\n\n", visibility, func.name));
                output.push_str(&format!("**Signature:** `{}`\n", func.signature));
                output.push_str(&format!("**Complexity:** {}/10\n\n", func.complexity));

                if !func.recommended_tests.is_empty() {
                    output.push_str("**Recommended Tests:**\n");
                    for test in &func.recommended_tests {
                        output.push_str(&format!("- {}\n", test));
                    }
                    output.push('\n');
                }
            }
        }

        if !self.missing_test_types.is_empty() {
            output.push_str("## Missing Test Types\n\n");
            for test_type in &self.missing_test_types {
                output.push_str(&format!("- {:?}\n", test_type));
            }
            output.push('\n');
        }

        if !self.recommendations.is_empty() {
            output.push_str("## Recommendations\n\n");
            for (i, rec) in self.recommendations.iter().enumerate() {
                output.push_str(&format!("{}. {}\n", i + 1, rec));
            }
            output.push('\n');
        }

        output
    }
}

impl std::fmt::Display for TestType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TestType::Unit => write!(f, "Unit"),
            TestType::Integration => write!(f, "Integration"),
            TestType::Property => write!(f, "Property"),
            TestType::EdgeCase => write!(f, "Edge Case"),
            TestType::ErrorHandling => write!(f, "Error Handling"),
            TestType::Performance => write!(f, "Performance"),
        }
    }
}

impl std::fmt::Display for TestFramework {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TestFramework::RustTest => write!(f, "Rust Test"),
            TestFramework::TokioTest => write!(f, "Tokio Test"),
            TestFramework::Proptest => write!(f, "Proptest"),
            TestFramework::Criterion => write!(f, "Criterion"),
        }
    }
}
