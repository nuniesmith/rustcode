//! Context builder for LLM analysis with 2M context window support
//!
//! This module implements multi-layered context injection for deep codebase understanding:
//! - Signature Map: All function/struct/trait definitions
//! - Dependency Graph: Cross-file imports and relationships
//! - Architectural Invariants: Project rules and constraints
//! - Diff Context: Recent changes
//! - Test Coverage: Test results and metrics

use crate::error::{AuditError, Result};
use crate::tests_runner::{TestResults, TestRunner};
use crate::types::{Category, SystemMap};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

/// Global context bundle for 2M window LLM analysis
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GlobalContextBundle {
    /// Project metadata
    pub metadata: ProjectMetadata,
    /// Signature map of all code symbols
    pub signature_map: SignatureMap,
    /// Dependency graph
    pub dependency_graph: DependencyGraph,
    /// Architectural rules and invariants
    pub architectural_rules: ArchitecturalRules,
    /// Recent changes (git diff)
    pub diff_context: Option<DiffContext>,
    /// Test coverage data
    pub test_coverage: Option<TestCoverageData>,
    /// System architecture map
    pub system_map: SystemMap,
    /// Full source code bundle
    pub source_bundle: SourceBundle,
}

/// Project metadata
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectMetadata {
    /// Project name
    pub name: String,
    /// Repository URL
    pub repository: Option<String>,
    /// Current branch
    pub branch: String,
    /// Total files
    pub total_files: usize,
    /// Total lines of code
    pub total_lines: usize,
    /// Languages detected
    pub languages: Vec<String>,
    /// Build timestamp
    pub built_at: chrono::DateTime<chrono::Utc>,
}

/// Signature map containing all code symbols
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignatureMap {
    /// Functions by file
    pub functions: HashMap<String, Vec<FunctionSignature>>,
    /// Structs/Classes by file
    pub types: HashMap<String, Vec<TypeSignature>>,
    /// Traits/Interfaces by file
    pub traits: HashMap<String, Vec<TraitSignature>>,
    /// Constants by file
    pub constants: HashMap<String, Vec<ConstantSignature>>,
    /// Total symbols
    pub total_symbols: usize,
}

/// Function signature
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionSignature {
    /// Function name
    pub name: String,
    /// File path
    pub file: String,
    /// Line number
    pub line: usize,
    /// Is public
    pub is_public: bool,
    /// Is async
    pub is_async: bool,
    /// Is test
    pub is_test: bool,
    /// Parameters
    pub params: Vec<String>,
    /// Return type
    pub return_type: Option<String>,
    /// Documentation
    pub docs: Option<String>,
}

/// Type signature (struct, class, enum)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TypeSignature {
    /// Type name
    pub name: String,
    /// File path
    pub file: String,
    /// Line number
    pub line: usize,
    /// Type kind
    pub kind: TypeKind,
    /// Is public
    pub is_public: bool,
    /// Fields
    pub fields: Vec<String>,
    /// Methods
    pub methods: Vec<String>,
    /// Documentation
    pub docs: Option<String>,
}

/// Type kind
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TypeKind {
    Struct,
    Class,
    Enum,
    Interface,
    Trait,
}

/// Trait/Interface signature
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraitSignature {
    /// Trait name
    pub name: String,
    /// File path
    pub file: String,
    /// Line number
    pub line: usize,
    /// Is public
    pub is_public: bool,
    /// Required methods
    pub methods: Vec<String>,
    /// Documentation
    pub docs: Option<String>,
}

/// Constant signature
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConstantSignature {
    /// Constant name
    pub name: String,
    /// File path
    pub file: String,
    /// Line number
    pub line: usize,
    /// Is public
    pub is_public: bool,
    /// Type
    pub const_type: Option<String>,
    /// Value (if literal)
    pub value: Option<String>,
}

/// Dependency graph
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DependencyGraph {
    /// Import relationships (file -> imported files)
    pub imports: HashMap<String, Vec<String>>,
    /// Reverse dependencies (file -> files that import it)
    pub imported_by: HashMap<String, Vec<String>>,
    /// Dead code candidates (files with no incoming edges)
    pub dead_code_candidates: Vec<String>,
    /// Hub files (high fan-out)
    pub hub_files: Vec<String>,
    /// Orphan files (no imports or exports)
    pub orphan_files: Vec<String>,
}

/// Architectural rules from llms.txt and risk_rules.json
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArchitecturalRules {
    /// Project-wide rules
    pub global_rules: Vec<Rule>,
    /// Category-specific rules
    pub category_rules: HashMap<Category, Vec<Rule>>,
    /// Risk management rules
    pub risk_rules: Vec<RiskRule>,
    /// Performance constraints
    pub performance_constraints: Vec<String>,
}

/// A single rule
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Rule {
    /// Rule ID
    pub id: String,
    /// Rule description
    pub description: String,
    /// Severity if violated
    pub severity: String,
    /// Pattern to detect (regex)
    pub pattern: Option<String>,
}

/// Risk management rule
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RiskRule {
    /// Rule name
    pub name: String,
    /// Risk level
    pub level: String,
    /// Condition
    pub condition: String,
    /// Action required
    pub action: String,
}

/// Diff context for recent changes
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiffContext {
    /// Time range (hours)
    pub hours: u32,
    /// Files changed
    pub files_changed: Vec<String>,
    /// Lines added
    pub lines_added: usize,
    /// Lines removed
    pub lines_removed: usize,
    /// Commits
    pub commits: Vec<CommitInfo>,
    /// Full diff
    pub diff: String,
}

/// Commit information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommitInfo {
    /// Commit hash
    pub hash: String,
    /// Author
    pub author: String,
    /// Timestamp
    pub timestamp: chrono::DateTime<chrono::Utc>,
    /// Message
    pub message: String,
}

/// Test coverage data
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestCoverageData {
    /// All test results
    pub test_results: Vec<TestResults>,
    /// Total coverage percentage
    pub total_coverage: Option<f64>,
    /// Uncovered files
    pub uncovered_files: Vec<String>,
    /// Files with failing tests
    pub files_with_failures: Vec<String>,
}

/// Source code bundle
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceBundle {
    /// Files included
    pub files: Vec<SourceFile>,
    /// Total size in bytes
    pub total_size: usize,
    /// Concatenated content for LLM
    pub content: String,
}

/// Single source file
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceFile {
    /// Relative path
    pub path: String,
    /// Category
    pub category: Category,
    /// Lines of code
    pub lines: usize,
    /// File content
    pub content: String,
}

/// Context builder
#[derive(Clone)]
pub struct ContextBuilder {
    root: PathBuf,
    include_tests: bool,
    max_file_size: usize,
}

impl ContextBuilder {
    /// Create a new context builder
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            include_tests: false,
            max_file_size: 1_000_000, // 1MB default
        }
    }

    /// Set whether to include tests
    pub fn with_tests(mut self, include: bool) -> Self {
        self.include_tests = include;
        self
    }

    /// Set max file size
    pub fn with_max_file_size(mut self, size: usize) -> Self {
        self.max_file_size = size;
        self
    }

    /// Build the complete global context bundle
    pub fn build(&self, system_map: SystemMap) -> Result<GlobalContextBundle> {
        tracing::info!("Building global context bundle for 2M window");

        let metadata = self.build_metadata()?;
        let signature_map = self.build_signature_map()?;
        let dependency_graph = self.build_dependency_graph(&signature_map)?;
        let architectural_rules = self.load_architectural_rules()?;
        let diff_context = self.build_diff_context().ok();
        let test_coverage = self.build_test_coverage().ok();
        let source_bundle = self.build_source_bundle()?;

        Ok(GlobalContextBundle {
            metadata,
            signature_map,
            dependency_graph,
            architectural_rules,
            diff_context,
            test_coverage,
            system_map,
            source_bundle,
        })
    }

    /// Build project metadata
    fn build_metadata(&self) -> Result<ProjectMetadata> {
        let mut total_files = 0;
        let mut total_lines = 0;
        let mut languages = HashSet::new();

        for entry in WalkDir::new(&self.root)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_file())
        {
            total_files += 1;

            if let Ok(content) = std::fs::read_to_string(entry.path()) {
                total_lines += content.lines().count();
            }

            if let Some(ext) = entry.path().extension().and_then(|e| e.to_str()) {
                match ext {
                    "rs" => languages.insert("Rust".to_string()),
                    "py" => languages.insert("Python".to_string()),
                    "ts" | "tsx" => languages.insert("TypeScript".to_string()),
                    "js" | "jsx" => languages.insert("JavaScript".to_string()),
                    "kt" => languages.insert("Kotlin".to_string()),
                    "swift" => languages.insert("Swift".to_string()),
                    _ => false,
                };
            }
        }

        Ok(ProjectMetadata {
            name: self
                .root
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("unknown")
                .to_string(),
            repository: None,
            branch: "main".to_string(),
            total_files,
            total_lines,
            languages: languages.into_iter().collect(),
            built_at: chrono::Utc::now(),
        })
    }

    /// Build signature map by parsing source files
    fn build_signature_map(&self) -> Result<SignatureMap> {
        let mut functions: HashMap<String, Vec<FunctionSignature>> = HashMap::new();
        let mut types: HashMap<String, Vec<TypeSignature>> = HashMap::new();
        let mut traits: HashMap<String, Vec<TraitSignature>> = HashMap::new();
        let mut constants: HashMap<String, Vec<ConstantSignature>> = HashMap::new();
        let mut total_symbols = 0;

        for entry in WalkDir::new(&self.root)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_file())
        {
            let path = entry.path();
            let rel_path = path
                .strip_prefix(&self.root)
                .unwrap_or(path)
                .display()
                .to_string();

            // Parse based on file extension
            if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                match ext {
                    "rs" => {
                        if let Ok((funcs, typs, trts, consts)) = self.parse_rust_file(path) {
                            total_symbols += funcs.len() + typs.len() + trts.len() + consts.len();
                            if !funcs.is_empty() {
                                functions.insert(rel_path.clone(), funcs);
                            }
                            if !typs.is_empty() {
                                types.insert(rel_path.clone(), typs);
                            }
                            if !trts.is_empty() {
                                traits.insert(rel_path.clone(), trts);
                            }
                            if !consts.is_empty() {
                                constants.insert(rel_path.clone(), consts);
                            }
                        }
                    }
                    "py" => {
                        if let Ok((funcs, typs)) = self.parse_python_file(path) {
                            total_symbols += funcs.len() + typs.len();
                            if !funcs.is_empty() {
                                functions.insert(rel_path.clone(), funcs);
                            }
                            if !typs.is_empty() {
                                types.insert(rel_path.clone(), typs);
                            }
                        }
                    }
                    _ => {}
                }
            }
        }

        Ok(SignatureMap {
            functions,
            types,
            traits,
            constants,
            total_symbols,
        })
    }

    /// Parse Rust file for signatures (basic regex-based parsing)
    #[allow(clippy::type_complexity)]
    fn parse_rust_file(
        &self,
        path: &Path,
    ) -> Result<(
        Vec<FunctionSignature>,
        Vec<TypeSignature>,
        Vec<TraitSignature>,
        Vec<ConstantSignature>,
    )> {
        let content = std::fs::read_to_string(path)?;
        let rel_path = path
            .strip_prefix(&self.root)
            .unwrap_or(path)
            .display()
            .to_string();

        let mut functions = Vec::new();
        let mut types = Vec::new();
        let mut traits = Vec::new();
        let mut constants = Vec::new();

        for (i, line) in content.lines().enumerate() {
            let trimmed = line.trim();

            // Function signatures
            if trimmed.starts_with("pub fn") || trimmed.starts_with("fn ") {
                let is_public = trimmed.starts_with("pub");
                let is_async = trimmed.contains("async");
                let is_test = content
                    .lines()
                    .nth(i.saturating_sub(1))
                    .is_some_and(|l| l.contains("#[test]"));

                if let Some(name_start) = trimmed.find("fn ") {
                    let rest = &trimmed[name_start + 3..];
                    if let Some(name_end) = rest.find(['(', '<']) {
                        let name = rest[..name_end].trim().to_string();
                        functions.push(FunctionSignature {
                            name,
                            file: rel_path.clone(),
                            line: i + 1,
                            is_public,
                            is_async,
                            is_test,
                            params: Vec::new(),
                            return_type: None,
                            docs: None,
                        });
                    }
                }
            }

            // Struct signatures
            if trimmed.starts_with("pub struct") || trimmed.starts_with("struct ") {
                let is_public = trimmed.starts_with("pub");
                if let Some(name_start) = trimmed.find("struct ") {
                    let rest = &trimmed[name_start + 7..];
                    if let Some(name_end) =
                        rest.find(|c: char| c.is_whitespace() || c == '<' || c == '{')
                    {
                        let name = rest[..name_end].trim().to_string();
                        types.push(TypeSignature {
                            name,
                            file: rel_path.clone(),
                            line: i + 1,
                            kind: TypeKind::Struct,
                            is_public,
                            fields: Vec::new(),
                            methods: Vec::new(),
                            docs: None,
                        });
                    }
                }
            }

            // Trait signatures
            if trimmed.starts_with("pub trait") || trimmed.starts_with("trait ") {
                let is_public = trimmed.starts_with("pub");
                if let Some(name_start) = trimmed.find("trait ") {
                    let rest = &trimmed[name_start + 6..];
                    if let Some(name_end) =
                        rest.find(|c: char| c.is_whitespace() || c == '<' || c == '{')
                    {
                        let name = rest[..name_end].trim().to_string();
                        traits.push(TraitSignature {
                            name,
                            file: rel_path.clone(),
                            line: i + 1,
                            is_public,
                            methods: Vec::new(),
                            docs: None,
                        });
                    }
                }
            }

            // Constants
            if trimmed.starts_with("pub const") || trimmed.starts_with("const ") {
                let is_public = trimmed.starts_with("pub");
                if let Some(name_start) = trimmed.find("const ") {
                    let rest = &trimmed[name_start + 6..];
                    if let Some(name_end) = rest.find(':') {
                        let name = rest[..name_end].trim().to_string();
                        constants.push(ConstantSignature {
                            name,
                            file: rel_path.clone(),
                            line: i + 1,
                            is_public,
                            const_type: None,
                            value: None,
                        });
                    }
                }
            }
        }

        Ok((functions, types, traits, constants))
    }

    /// Parse Python file for signatures
    fn parse_python_file(
        &self,
        path: &Path,
    ) -> Result<(Vec<FunctionSignature>, Vec<TypeSignature>)> {
        let content = std::fs::read_to_string(path)?;
        let rel_path = path
            .strip_prefix(&self.root)
            .unwrap_or(path)
            .display()
            .to_string();

        let mut functions = Vec::new();
        let mut types = Vec::new();

        for (i, line) in content.lines().enumerate() {
            let trimmed = line.trim();

            // Function/method signatures
            if trimmed.starts_with("def ") || trimmed.starts_with("async def ") {
                let is_async = trimmed.starts_with("async");
                let def_start = if is_async { 10 } else { 4 };

                if let Some(name_end) = trimmed[def_start..].find('(') {
                    let name = trimmed[def_start..def_start + name_end].trim().to_string();
                    let is_public = !name.starts_with('_');

                    functions.push(FunctionSignature {
                        name,
                        file: rel_path.clone(),
                        line: i + 1,
                        is_public,
                        is_async,
                        is_test: false,
                        params: Vec::new(),
                        return_type: None,
                        docs: None,
                    });
                }
            }

            // Class signatures
            if let Some(stripped) = trimmed.strip_prefix("class ") {
                if let Some(name_end) = stripped.find(['(', ':']) {
                    let name = stripped[..name_end].trim().to_string();
                    let is_public = !name.starts_with('_');

                    types.push(TypeSignature {
                        name,
                        file: rel_path.clone(),
                        line: i + 1,
                        kind: TypeKind::Class,
                        is_public,
                        fields: Vec::new(),
                        methods: Vec::new(),
                        docs: None,
                    });
                }
            }
        }

        Ok((functions, types))
    }

    /// Build dependency graph from imports
    fn build_dependency_graph(&self, signature_map: &SignatureMap) -> Result<DependencyGraph> {
        let mut imports: HashMap<String, Vec<String>> = HashMap::new();
        let mut imported_by: HashMap<String, Vec<String>> = HashMap::new();

        // Parse imports from all files
        for entry in WalkDir::new(&self.root)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_file())
        {
            let path = entry.path();
            let rel_path = path
                .strip_prefix(&self.root)
                .unwrap_or(path)
                .display()
                .to_string();

            if let Ok(content) = std::fs::read_to_string(path) {
                let file_imports = self.extract_imports(&content, path);
                if !file_imports.is_empty() {
                    for imported in &file_imports {
                        imported_by
                            .entry(imported.clone())
                            .or_default()
                            .push(rel_path.clone());
                    }
                    imports.insert(rel_path, file_imports);
                }
            }
        }

        // Find dead code candidates
        let all_files: HashSet<String> = signature_map
            .functions
            .keys()
            .chain(signature_map.types.keys())
            .cloned()
            .collect();

        let dead_code_candidates: Vec<String> = all_files
            .iter()
            .filter(|file| !imported_by.contains_key(*file))
            .cloned()
            .collect();

        // Find hub files (high fan-out)
        let mut hub_files: Vec<(String, usize)> = imports
            .iter()
            .map(|(file, deps)| (file.clone(), deps.len()))
            .collect();
        hub_files.sort_by(|a, b| b.1.cmp(&a.1));
        let hub_files = hub_files.iter().take(10).map(|(f, _)| f.clone()).collect();

        // Find orphan files (no imports or exports)
        let orphan_files: Vec<String> = all_files
            .iter()
            .filter(|file| !imports.contains_key(*file) && !imported_by.contains_key(*file))
            .cloned()
            .collect();

        Ok(DependencyGraph {
            imports,
            imported_by,
            dead_code_candidates,
            hub_files,
            orphan_files,
        })
    }

    /// Extract imports from file content
    fn extract_imports(&self, content: &str, path: &Path) -> Vec<String> {
        let mut imports = Vec::new();

        if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
            match ext {
                "rs" => {
                    for line in content.lines() {
                        let trimmed = line.trim();
                        if trimmed.starts_with("use ") || trimmed.starts_with("pub use ") {
                            // Extract module name
                            if let Some(use_start) = trimmed.find("use ") {
                                let rest = &trimmed[use_start + 4..];
                                if let Some(semi) = rest.find(';') {
                                    let import = rest[..semi].trim().to_string();
                                    imports.push(import);
                                }
                            }
                        }
                    }
                }
                "py" => {
                    for line in content.lines() {
                        let trimmed = line.trim();
                        if trimmed.starts_with("import ") || trimmed.starts_with("from ") {
                            imports.push(trimmed.to_string());
                        }
                    }
                }
                _ => {}
            }
        }

        imports
    }

    /// Load architectural rules from llms.txt and risk_rules.json
    fn load_architectural_rules(&self) -> Result<ArchitecturalRules> {
        let mut global_rules = Vec::new();
        let category_rules = HashMap::new();
        let mut risk_rules = Vec::new();
        let mut performance_constraints = Vec::new();

        // Load llms.txt if exists
        let llms_path = self.root.join("llms.txt");
        if llms_path.exists() {
            if let Ok(content) = std::fs::read_to_string(&llms_path) {
                for line in content.lines() {
                    if line.starts_with("- ") || line.starts_with("* ") {
                        global_rules.push(Rule {
                            id: format!("LLMS-{}", global_rules.len() + 1),
                            description: line[2..].to_string(),
                            severity: "Medium".to_string(),
                            pattern: None,
                        });
                    }
                }
            }
        }

        // Load risk_rules.json if exists
        let risk_path = self.root.join("risk_rules.json");
        if risk_path.exists() {
            if let Ok(content) = std::fs::read_to_string(&risk_path) {
                if let Ok(json) = serde_json::from_str::<serde_json::Value>(&content) {
                    if let Some(rules) = json.get("rules").and_then(|r| r.as_array()) {
                        for rule in rules {
                            risk_rules.push(RiskRule {
                                name: rule["name"].as_str().unwrap_or("").to_string(),
                                level: rule["level"].as_str().unwrap_or("").to_string(),
                                condition: rule["condition"].as_str().unwrap_or("").to_string(),
                                action: rule["action"].as_str().unwrap_or("").to_string(),
                            });
                        }
                    }
                }
            }
        }

        // Default Janus rules
        performance_constraints.push("No unwrap() in hot-path execution code".to_string());
        performance_constraints.push("Async functions must not block".to_string());
        performance_constraints.push("LTN logic compliance required".to_string());

        Ok(ArchitecturalRules {
            global_rules,
            category_rules,
            risk_rules,
            performance_constraints,
        })
    }

    /// Build diff context from git
    fn build_diff_context(&self) -> Result<DiffContext> {
        use std::process::Command;

        let hours = 48;

        // Get git log for last 48 hours
        let since = format!("{}hours", hours);
        let log_output = Command::new("git")
            .args(["log", "--since", &since, "--oneline"])
            .current_dir(&self.root)
            .output()
            .map_err(AuditError::Io)?;

        let log_str = String::from_utf8_lossy(&log_output.stdout);
        let mut commits = Vec::new();

        for line in log_str.lines() {
            if let Some((hash, message)) = line.split_once(' ') {
                commits.push(CommitInfo {
                    hash: hash.to_string(),
                    author: "unknown".to_string(),
                    timestamp: chrono::Utc::now(),
                    message: message.to_string(),
                });
            }
        }

        // Get diff stats
        let diff_output = Command::new("git")
            .args(["diff", "--stat", &format!("HEAD@{{{}hours ago}}", hours)])
            .current_dir(&self.root)
            .output()
            .map_err(AuditError::Io)?;

        let diff_str = String::from_utf8_lossy(&diff_output.stdout);
        let mut files_changed = Vec::new();
        let lines_added = 0;
        let lines_removed = 0;

        for line in diff_str.lines() {
            if line.contains("|") {
                if let Some(file) = line.split('|').next() {
                    files_changed.push(file.trim().to_string());
                }
            }
        }

        Ok(DiffContext {
            hours,
            files_changed,
            lines_added,
            lines_removed,
            commits,
            diff: diff_str.to_string(),
        })
    }

    /// Build test coverage data
    fn build_test_coverage(&self) -> Result<TestCoverageData> {
        let test_runner = TestRunner::new(&self.root);
        let test_results = test_runner.run_all_tests()?;

        let total_coverage =
            test_results.iter().filter_map(|r| r.coverage).sum::<f64>() / test_results.len() as f64;

        let files_with_failures: Vec<String> = test_results
            .iter()
            .filter(|r| r.failed > 0)
            .flat_map(|r| r.test_files.clone())
            .collect();

        Ok(TestCoverageData {
            test_results,
            total_coverage: Some(total_coverage),
            uncovered_files: Vec::new(),
            files_with_failures,
        })
    }

    /// Build source code bundle
    fn build_source_bundle(&self) -> Result<SourceBundle> {
        let mut files = Vec::new();
        let mut total_size = 0;
        let mut content = String::new();

        content.push_str("=== COMPLETE SOURCE CODE BUNDLE ===\n\n");

        for entry in WalkDir::new(&self.root)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_file())
        {
            let path = entry.path();
            let metadata = entry.metadata().map_err(|e| AuditError::Io(e.into()))?;

            // Skip large files
            if metadata.len() > self.max_file_size as u64 {
                continue;
            }

            if let Ok(file_content) = std::fs::read_to_string(path) {
                let rel_path = path
                    .strip_prefix(&self.root)
                    .unwrap_or(path)
                    .display()
                    .to_string();

                let category = Category::from_path(&rel_path);
                let lines = file_content.lines().count();

                // Add to bundle
                content.push_str(&format!("\n--- FILE: {} ---\n", rel_path));
                content.push_str(&file_content);
                content.push_str("\n\n");

                total_size += file_content.len();

                files.push(SourceFile {
                    path: rel_path,
                    category,
                    lines,
                    content: file_content,
                });
            }
        }

        Ok(SourceBundle {
            files,
            total_size,
            content,
        })
    }

    /// Generate formatted context for LLM prompt
    pub fn format_for_llm(bundle: &GlobalContextBundle) -> String {
        let mut prompt = String::new();

        prompt.push_str("# GLOBAL CONTEXT BUNDLE - COMPLETE CODEBASE ANALYSIS\n\n");

        // Metadata
        prompt.push_str("## PROJECT METADATA\n");
        prompt.push_str(&format!("- Name: {}\n", bundle.metadata.name));
        prompt.push_str(&format!("- Total Files: {}\n", bundle.metadata.total_files));
        prompt.push_str(&format!("- Total Lines: {}\n", bundle.metadata.total_lines));
        prompt.push_str(&format!(
            "- Languages: {}\n",
            bundle.metadata.languages.join(", ")
        ));
        prompt.push('\n');

        // Architectural Rules
        prompt.push_str("## ARCHITECTURAL RULES & CONSTRAINTS\n");
        for rule in &bundle.architectural_rules.global_rules {
            prompt.push_str(&format!("- [{}] {}\n", rule.id, rule.description));
        }
        for constraint in &bundle.architectural_rules.performance_constraints {
            prompt.push_str(&format!("- PERFORMANCE: {}\n", constraint));
        }
        prompt.push('\n');

        // Signature Map Summary
        prompt.push_str("## SIGNATURE MAP\n");
        prompt.push_str(&format!(
            "Total Symbols: {}\n",
            bundle.signature_map.total_symbols
        ));
        prompt.push_str(&format!(
            "Functions: {}\n",
            bundle
                .signature_map
                .functions
                .values()
                .map(|v| v.len())
                .sum::<usize>()
        ));
        prompt.push_str(&format!(
            "Types: {}\n",
            bundle
                .signature_map
                .types
                .values()
                .map(|v| v.len())
                .sum::<usize>()
        ));
        prompt.push('\n');

        // Dependency Graph
        prompt.push_str("## DEPENDENCY GRAPH ANALYSIS\n");
        prompt.push_str(&format!(
            "Dead Code Candidates: {}\n",
            bundle.dependency_graph.dead_code_candidates.len()
        ));
        prompt.push_str(&format!(
            "Hub Files: {}\n",
            bundle.dependency_graph.hub_files.len()
        ));
        prompt.push_str(&format!(
            "Orphan Files: {}\n",
            bundle.dependency_graph.orphan_files.len()
        ));
        prompt.push('\n');

        // Test Coverage
        if let Some(coverage) = &bundle.test_coverage {
            prompt.push_str("## TEST COVERAGE\n");
            if let Some(total) = coverage.total_coverage {
                prompt.push_str(&format!("Total Coverage: {:.1}%\n", total));
            }
            prompt.push_str(&format!(
                "Files with Failures: {}\n",
                coverage.files_with_failures.len()
            ));
            prompt.push('\n');
        }

        // Full Source Code
        prompt.push_str("## COMPLETE SOURCE CODE\n\n");
        prompt.push_str(&bundle.source_bundle.content);

        prompt
    }
}
