//! Parser module for language-specific code analysis
//!
//! This module provides Rust-aware parsing for static analysis,
//! using regex patterns to extract function signatures, types,
//! imports, and calculate complexity metrics.

use crate::error::Result;
use crate::types::Category;
use regex::Regex;
use std::path::Path;

/// Code parser for Rust source files
pub struct Parser {
    /// Regex for function signatures
    fn_regex: Regex,
    /// Regex for async function signatures
    async_fn_regex: Regex,
    /// Regex for struct definitions
    struct_regex: Regex,
    /// Regex for enum definitions
    enum_regex: Regex,
    /// Regex for trait definitions
    trait_regex: Regex,
    /// Regex for impl blocks (reserved for future use)
    #[allow(dead_code)]
    impl_regex: Regex,
    /// Regex for use statements
    use_regex: Regex,
    /// Regex for pub use (re-exports)
    pub_use_regex: Regex,
    /// Regex for mod declarations (reserved for future use)
    #[allow(dead_code)]
    mod_regex: Regex,
}

impl Parser {
    /// Create a new parser
    pub fn new() -> Result<Self> {
        Ok(Self {
            // Match: pub fn name(...) or fn name(...)
            fn_regex: Regex::new(
                r"(?m)^\s*(pub(?:\s*\([^)]*\))?\s+)?fn\s+(\w+)\s*(?:<[^>]*>)?\s*\(([^)]*)\)",
            )
            .expect("Invalid fn regex"),

            // Match: pub async fn name(...) or async fn name(...)
            async_fn_regex: Regex::new(
                r"(?m)^\s*(pub(?:\s*\([^)]*\))?\s+)?async\s+fn\s+(\w+)\s*(?:<[^>]*>)?\s*\(([^)]*)\)",
            )
            .expect("Invalid async fn regex"),

            // Match: pub struct Name or struct Name
            struct_regex: Regex::new(
                r"(?m)^\s*(pub(?:\s*\([^)]*\))?\s+)?struct\s+(\w+)",
            )
            .expect("Invalid struct regex"),

            // Match: pub enum Name or enum Name
            enum_regex: Regex::new(
                r"(?m)^\s*(pub(?:\s*\([^)]*\))?\s+)?enum\s+(\w+)",
            )
            .expect("Invalid enum regex"),

            // Match: pub trait Name or trait Name
            trait_regex: Regex::new(
                r"(?m)^\s*(pub(?:\s*\([^)]*\))?\s+)?trait\s+(\w+)",
            )
            .expect("Invalid trait regex"),

            // Match: impl Name for Type or impl Type (reserved for future use)
            impl_regex: Regex::new(
                r"(?m)^\s*impl(?:<[^>]*>)?\s+(?:(\w+)\s+for\s+)?(\w+)",
            )
            .expect("Invalid impl regex"),

            // Match: use path::to::module;
            use_regex: Regex::new(
                r"(?m)^\s*use\s+([\w:]+(?:::\{[^}]+\})?(?:::\*)?)\s*;",
            )
            .expect("Invalid use regex"),

            // Match: pub use path::to::module;
            pub_use_regex: Regex::new(
                r"(?m)^\s*pub\s+use\s+([\w:]+(?:::\{[^}]+\})?(?:::\*)?)\s*;",
            )
            .expect("Invalid pub use regex"),

            // Match: pub mod name or mod name (reserved for future use)
            mod_regex: Regex::new(
                r"(?m)^\s*(pub(?:\s*\([^)]*\))?\s+)?mod\s+(\w+)",
            )
            .expect("Invalid mod regex"),
        })
    }

    /// Parse a file and extract symbols
    pub fn parse_file(
        &self,
        path: &Path,
        content: &str,
        _category: Category,
    ) -> Result<ParseResult> {
        let functions = self.extract_functions(content, _category)?;
        let types = self.extract_types(content)?;
        let imports = self.extract_imports(content);
        let exports = self.extract_exports(content);

        tracing::debug!(
            "Parsed {}: {} functions, {} types, {} imports",
            path.display(),
            functions.len(),
            types.len(),
            imports.len()
        );

        Ok(ParseResult {
            functions,
            types,
            imports,
            exports,
        })
    }

    /// Extract function signatures
    pub fn extract_functions(
        &self,
        content: &str,
        _category: Category,
    ) -> Result<Vec<FunctionInfo>> {
        let mut functions = Vec::new();
        let _lines: Vec<&str> = content.lines().collect();

        // Extract regular functions
        for caps in self.fn_regex.captures_iter(content) {
            let full_match = caps.get(0).unwrap();
            let line = content[..full_match.start()].matches('\n').count() + 1;

            let is_public = caps.get(1).is_some_and(|m| m.as_str().contains("pub"));
            let name = caps.get(2).map_or("", |m| m.as_str()).to_string();
            let params = caps.get(3).map_or("", |m| m.as_str());
            let param_count = self.count_parameters(params);

            functions.push(FunctionInfo {
                name,
                line,
                param_count,
                is_public,
                is_async: false,
            });
        }

        // Extract async functions
        for caps in self.async_fn_regex.captures_iter(content) {
            let full_match = caps.get(0).unwrap();
            let line = content[..full_match.start()].matches('\n').count() + 1;

            let is_public = caps.get(1).is_some_and(|m| m.as_str().contains("pub"));
            let name = caps.get(2).map_or("", |m| m.as_str()).to_string();
            let params = caps.get(3).map_or("", |m| m.as_str());
            let param_count = self.count_parameters(params);

            functions.push(FunctionInfo {
                name,
                line,
                param_count,
                is_public,
                is_async: true,
            });
        }

        // Sort by line number
        functions.sort_by_key(|f| f.line);

        Ok(functions)
    }

    /// Count function parameters
    fn count_parameters(&self, params: &str) -> usize {
        if params.trim().is_empty() {
            return 0;
        }

        // Handle self parameter
        let has_self = params.contains("self");

        // Count commas for other parameters
        // This is a simple heuristic - doesn't handle complex generic types perfectly
        let comma_count = params.matches(',').count();

        if has_self {
            comma_count + 1 // self + comma-separated params
        } else if comma_count == 0 && !params.trim().is_empty() {
            1 // single parameter
        } else {
            comma_count + 1
        }
    }

    /// Extract type definitions (structs, enums, traits)
    fn extract_types(&self, content: &str) -> Result<Vec<TypeInfo>> {
        let mut types = Vec::new();

        // Extract structs
        for caps in self.struct_regex.captures_iter(content) {
            let full_match = caps.get(0).unwrap();
            let line = content[..full_match.start()].matches('\n').count() + 1;

            let is_public = caps.get(1).is_some_and(|m| m.as_str().contains("pub"));
            let name = caps.get(2).map_or("", |m| m.as_str()).to_string();

            types.push(TypeInfo {
                name,
                line,
                is_public,
                kind: TypeKind::Struct,
            });
        }

        // Extract enums
        for caps in self.enum_regex.captures_iter(content) {
            let full_match = caps.get(0).unwrap();
            let line = content[..full_match.start()].matches('\n').count() + 1;

            let is_public = caps.get(1).is_some_and(|m| m.as_str().contains("pub"));
            let name = caps.get(2).map_or("", |m| m.as_str()).to_string();

            types.push(TypeInfo {
                name,
                line,
                is_public,
                kind: TypeKind::Enum,
            });
        }

        // Extract traits
        for caps in self.trait_regex.captures_iter(content) {
            let full_match = caps.get(0).unwrap();
            let line = content[..full_match.start()].matches('\n').count() + 1;

            let is_public = caps.get(1).is_some_and(|m| m.as_str().contains("pub"));
            let name = caps.get(2).map_or("", |m| m.as_str()).to_string();

            types.push(TypeInfo {
                name,
                line,
                is_public,
                kind: TypeKind::Trait,
            });
        }

        // Sort by line number
        types.sort_by_key(|t| t.line);

        Ok(types)
    }

    /// Extract import statements
    fn extract_imports(&self, content: &str) -> Vec<String> {
        self.use_regex
            .captures_iter(content)
            .filter_map(|caps| caps.get(1).map(|m| m.as_str().to_string()))
            .collect()
    }

    /// Extract re-exports (pub use statements)
    fn extract_exports(&self, content: &str) -> Vec<String> {
        self.pub_use_regex
            .captures_iter(content)
            .filter_map(|caps| caps.get(1).map(|m| m.as_str().to_string()))
            .collect()
    }

    /// Detect unused code patterns
    pub fn detect_unused(&self, content: &str, _category: Category) -> Result<Vec<UnusedCode>> {
        let mut unused = Vec::new();

        // Look for #[allow(dead_code)] annotations
        let dead_code_regex = Regex::new(r"(?m)^\s*#\[allow\(dead_code\)\]").unwrap();

        for mat in dead_code_regex.find_iter(content) {
            let line = content[..mat.start()].matches('\n').count() + 1;

            // Get the next line to identify what's marked as dead code
            let lines: Vec<&str> = content.lines().collect();
            let next_line = lines.get(line).map_or("unknown", |l| l.trim());

            unused.push(UnusedCode {
                element_type: "annotated".to_string(),
                name: next_line.chars().take(50).collect(),
                line,
                reason: "Explicitly marked with #[allow(dead_code)]".to_string(),
            });
        }

        // Look for underscore-prefixed identifiers (conventional unused markers)
        let underscore_regex = Regex::new(r"(?m)\blet\s+_(\w+)\s*[=:]").unwrap();

        for caps in underscore_regex.captures_iter(content) {
            let full_match = caps.get(0).unwrap();
            let line = content[..full_match.start()].matches('\n').count() + 1;
            let name = caps.get(1).map_or("", |m| m.as_str()).to_string();

            unused.push(UnusedCode {
                element_type: "variable".to_string(),
                name: format!("_{}", name),
                line,
                reason: "Underscore prefix indicates intentionally unused".to_string(),
            });
        }

        Ok(unused)
    }

    /// Calculate complexity metrics
    pub fn calculate_complexity(
        &self,
        content: &str,
        _category: Category,
    ) -> Result<ComplexityMetrics> {
        let lines: Vec<&str> = content.lines().collect();
        let loc = lines.len();

        // Logical lines of code (non-empty, non-comment lines)
        let lloc = lines
            .iter()
            .filter(|line| {
                let trimmed = line.trim();
                !trimmed.is_empty()
                    && !trimmed.starts_with("//")
                    && !trimmed.starts_with("/*")
                    && !trimmed.starts_with("*")
            })
            .count();

        // Cyclomatic complexity estimation
        // Count decision points: if, else if, match arms, for, while, loop, &&, ||, ?
        let cyclomatic = self.estimate_cyclomatic_complexity(content);

        // Cognitive complexity estimation
        // Similar to cyclomatic but with nesting penalties
        let cognitive = self.estimate_cognitive_complexity(content);

        Ok(ComplexityMetrics {
            cyclomatic,
            cognitive,
            loc,
            lloc,
        })
    }

    /// Estimate cyclomatic complexity
    fn estimate_cyclomatic_complexity(&self, content: &str) -> usize {
        let mut complexity = 1; // Base complexity

        // Count decision points
        let decision_patterns = [
            r"\bif\s+",
            r"\belse\s+if\b",
            r"\bwhile\s+",
            r"\bfor\s+",
            r"\bloop\b",
            r"\bmatch\b",
            r"\b\?\s*$", // try operator at end of line
            r"\&\&",
            r"\|\|",
        ];

        for pattern in &decision_patterns {
            let regex = Regex::new(pattern).unwrap();
            complexity += regex.find_iter(content).count();
        }

        // Count match arms (each => is a decision point)
        let match_arm_regex = Regex::new(r"=>\s*[{]?").unwrap();
        complexity += match_arm_regex.find_iter(content).count();

        complexity
    }

    /// Estimate cognitive complexity
    fn estimate_cognitive_complexity(&self, content: &str) -> usize {
        let mut complexity = 0;
        let mut nesting_level = 0;

        for line in content.lines() {
            let trimmed = line.trim();

            // Skip comments
            if trimmed.starts_with("//") || trimmed.starts_with("/*") || trimmed.starts_with("*") {
                continue;
            }

            // Track nesting
            let opens = line.matches('{').count();
            let closes = line.matches('}').count();

            // Add complexity for control flow with nesting penalty
            if trimmed.starts_with("if ")
                || trimmed.starts_with("else if ")
                || trimmed.starts_with("while ")
                || trimmed.starts_with("for ")
                || trimmed.starts_with("loop ")
            {
                complexity += 1 + nesting_level;
            }

            if trimmed.starts_with("match ") {
                complexity += 1 + nesting_level;
            }

            // Logical operators add complexity
            complexity += line.matches("&&").count();
            complexity += line.matches("||").count();

            // Update nesting level
            nesting_level = nesting_level.saturating_add(opens).saturating_sub(closes);
        }

        complexity
    }
}

impl Default for Parser {
    fn default() -> Self {
        Self::new().expect("Failed to create default Parser")
    }
}

/// Parse result containing AST information
#[derive(Debug, Clone, Default)]
pub struct ParseResult {
    /// Functions/methods found
    pub functions: Vec<FunctionInfo>,
    /// Types/classes found
    pub types: Vec<TypeInfo>,
    /// Imports found
    pub imports: Vec<String>,
    /// Exports found
    pub exports: Vec<String>,
}

/// Function information
#[derive(Debug, Clone)]
pub struct FunctionInfo {
    /// Function name
    pub name: String,
    /// Line number
    pub line: usize,
    /// Parameter count
    pub param_count: usize,
    /// Whether it's public/exported
    pub is_public: bool,
    /// Whether it's async
    pub is_async: bool,
}

/// Type kind
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TypeKind {
    Struct,
    Enum,
    Trait,
}

/// Type information
#[derive(Debug, Clone)]
pub struct TypeInfo {
    /// Type name
    pub name: String,
    /// Line number
    pub line: usize,
    /// Whether it's public/exported
    pub is_public: bool,
    /// Type kind (struct, enum, trait)
    pub kind: TypeKind,
}

/// Unused code detection result
#[derive(Debug, Clone)]
pub struct UnusedCode {
    /// Code element type
    pub element_type: String,
    /// Element name
    pub name: String,
    /// Line number
    pub line: usize,
    /// Reason why it appears unused
    pub reason: String,
}

/// Complexity metrics
#[derive(Debug, Clone, Default)]
pub struct ComplexityMetrics {
    /// Cyclomatic complexity
    pub cyclomatic: usize,
    /// Cognitive complexity
    pub cognitive: usize,
    /// Lines of code
    pub loc: usize,
    /// Logical lines of code
    pub lloc: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parser_new() {
        let parser = Parser::new().unwrap();
        assert!(parser
            .parse_file(Path::new("test.rs"), "", Category::Janus)
            .is_ok());
    }

    #[test]
    fn test_extract_functions() {
        let parser = Parser::new().unwrap();
        let content = r#"
pub fn public_function(x: i32, y: String) -> bool {
    true
}

fn private_function() {
}

pub async fn async_handler(req: Request) -> Response {
    Response::ok()
}
"#;

        let functions = parser.extract_functions(content, Category::Janus).unwrap();

        assert_eq!(functions.len(), 3);

        assert_eq!(functions[0].name, "public_function");
        assert!(functions[0].is_public);
        assert!(!functions[0].is_async);
        assert_eq!(functions[0].param_count, 2);

        assert_eq!(functions[1].name, "private_function");
        assert!(!functions[1].is_public);
        assert_eq!(functions[1].param_count, 0);

        assert_eq!(functions[2].name, "async_handler");
        assert!(functions[2].is_public);
        assert!(functions[2].is_async);
    }

    #[test]
    fn test_extract_types() {
        let parser = Parser::new().unwrap();
        let content = r#"
pub struct PublicStruct {
    field: i32,
}

struct PrivateStruct;

pub enum Status {
    Active,
    Inactive,
}

pub trait Handler {
    fn handle(&self);
}
"#;

        let types = parser.extract_types(content).unwrap();

        assert_eq!(types.len(), 4);

        assert_eq!(types[0].name, "PublicStruct");
        assert!(types[0].is_public);
        assert_eq!(types[0].kind, TypeKind::Struct);

        assert_eq!(types[1].name, "PrivateStruct");
        assert!(!types[1].is_public);

        assert_eq!(types[2].name, "Status");
        assert_eq!(types[2].kind, TypeKind::Enum);

        assert_eq!(types[3].name, "Handler");
        assert_eq!(types[3].kind, TypeKind::Trait);
    }

    #[test]
    fn test_extract_imports() {
        let parser = Parser::new().unwrap();
        let content = r#"
use std::collections::HashMap;
use crate::error::Result;
use super::types::{Config, Settings};
"#;

        let imports = parser.extract_imports(content);

        assert_eq!(imports.len(), 3);
        assert!(imports.contains(&"std::collections::HashMap".to_string()));
        assert!(imports.contains(&"crate::error::Result".to_string()));
    }

    #[test]
    fn test_complexity_metrics() {
        let parser = Parser::new().unwrap();
        let content = r#"
fn simple() {
    println!("hello");
}

fn complex(x: i32) -> i32 {
    if x > 0 {
        if x > 10 {
            return x * 2;
        } else {
            return x + 1;
        }
    } else if x < 0 {
        return -x;
    }

    match x {
        0 => 0,
        _ => 1,
    }
}
"#;

        let metrics = parser
            .calculate_complexity(content, Category::Janus)
            .unwrap();

        assert!(metrics.cyclomatic > 1);
        assert!(metrics.cognitive > 0);
        assert!(metrics.loc > 0);
        assert!(metrics.lloc > 0);
        assert!(metrics.lloc < metrics.loc);
    }

    #[test]
    fn test_parse_result_default() {
        let result = ParseResult::default();
        assert_eq!(result.functions.len(), 0);
        assert_eq!(result.types.len(), 0);
    }

    #[test]
    fn test_detect_unused() {
        let parser = Parser::new().unwrap();
        let content = r#"
#[allow(dead_code)]
fn unused_function() {}

fn used_function() {
    let _ignored = 42;
    let used = 10;
}
"#;

        let unused = parser.detect_unused(content, Category::Janus).unwrap();

        assert!(!unused.is_empty());
    }

    #[test]
    fn test_function_with_self() {
        let parser = Parser::new().unwrap();
        let content = r#"
impl MyStruct {
    pub fn method(&self, x: i32) -> bool {
        true
    }

    fn private_method(&mut self) {
    }
}
"#;

        let functions = parser.extract_functions(content, Category::Janus).unwrap();

        assert_eq!(functions.len(), 2);
        assert_eq!(functions[0].name, "method");
        assert_eq!(functions[0].param_count, 2); // &self + x
        assert_eq!(functions[1].param_count, 1); // &mut self only
    }
}
