// # Query Templates Module
//
// Reusable query patterns for common analysis tasks.
//
// ## Features
//
// - Pre-defined templates for common queries
// - Variable substitution
// - Template customization
// - Cost-optimized patterns
// - Batch-friendly templates
//
// ## Usage
//
// ```rust,no_run
// use rustcode::query_templates::{QueryTemplate, TemplateRegistry};
//
// #[tokio::main]
// async fn main() -> anyhow::Result<()> {
//     let registry = TemplateRegistry::default();
//
//     // Get a template
//     let template = registry.get("security_audit")?;
//
//     // Render with variables
//     let query = template.render(&[("file", "auth.rs")])?;
//
//     Ok(())
// }
// ```

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// Query template with variable substitution
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryTemplate {
    // Template name
    pub name: String,
    // Template description
    pub description: String,
    // Template pattern with {variable} placeholders
    pub pattern: String,
    // Required variables
    pub required_vars: Vec<String>,
    // Optional variables with defaults
    pub optional_vars: HashMap<String, String>,
    // Expected operation type
    pub operation: String,
    // Estimated tokens (for budgeting)
    pub estimated_tokens: usize,
    // Recommended TTL in hours
    pub cache_ttl: i64,
}

// Template category
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TemplateCategory {
    Security,
    Quality,
    Performance,
    Architecture,
    Documentation,
    Refactoring,
    Testing,
    General,
}

impl TemplateCategory {
    pub fn as_str(&self) -> &'static str {
        match self {
            TemplateCategory::Security => "security",
            TemplateCategory::Quality => "quality",
            TemplateCategory::Performance => "performance",
            TemplateCategory::Architecture => "architecture",
            TemplateCategory::Documentation => "documentation",
            TemplateCategory::Refactoring => "refactoring",
            TemplateCategory::Testing => "testing",
            TemplateCategory::General => "general",
        }
    }
}

// Registry of query templates
pub struct TemplateRegistry {
    templates: HashMap<String, QueryTemplate>,
}

impl Default for TemplateRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl TemplateRegistry {
    // Create a new template registry with built-in templates
    pub fn new() -> Self {
        let mut registry = Self {
            templates: HashMap::new(),
        };
        registry.register_builtin_templates();
        registry
    }

    // Register all built-in templates
    fn register_builtin_templates(&mut self) {
        // Security templates
        self.add_template(QueryTemplate {
            name: "security_audit".to_string(),
            description: "Comprehensive security audit of code".to_string(),
            pattern: "Perform a security audit of {file}. Focus on:\n\
                      - Input validation\n\
                      - SQL injection risks\n\
                      - Authentication/authorization\n\
                      - Sensitive data handling\n\
                      - Cryptography usage\n\
                      List specific vulnerabilities with line numbers."
                .to_string(),
            required_vars: vec!["file".to_string()],
            optional_vars: HashMap::new(),
            operation: "security_audit".to_string(),
            estimated_tokens: 500,
            cache_ttl: 168, // 1 week
        });

        self.add_template(QueryTemplate {
            name: "find_unsafe".to_string(),
            description: "Find all unsafe code blocks and explain risks".to_string(),
            pattern: "Find all 'unsafe' blocks in {repo}. For each:\n\
                      - Explain why it's marked unsafe\n\
                      - Assess if it's necessary\n\
                      - Suggest safer alternatives\n\
                      - Rate risk level (low/medium/high)"
                .to_string(),
            required_vars: vec!["repo".to_string()],
            optional_vars: HashMap::new(),
            operation: "pattern_search".to_string(),
            estimated_tokens: 1000,
            cache_ttl: 24,
        });

        // Quality templates
        self.add_template(QueryTemplate {
            name: "code_quality".to_string(),
            description: "Assess code quality metrics".to_string(),
            pattern: "Analyze code quality of {file}. Evaluate:\n\
                      - Readability and clarity\n\
                      - Naming conventions\n\
                      - Function complexity\n\
                      - Documentation completeness\n\
                      - Error handling\n\
                      Provide a score (0-100) and specific improvements."
                .to_string(),
            required_vars: vec!["file".to_string()],
            optional_vars: HashMap::new(),
            operation: "file_scoring".to_string(),
            estimated_tokens: 600,
            cache_ttl: 168,
        });

        self.add_template(QueryTemplate {
            name: "find_duplicates".to_string(),
            description: "Find duplicated code patterns".to_string(),
            pattern: "Identify duplicated code in {repo}. Look for:\n\
                      - Similar functions or logic\n\
                      - Copy-pasted code blocks\n\
                      - Opportunities for DRY refactoring\n\
                      Suggest how to consolidate."
                .to_string(),
            required_vars: vec!["repo".to_string()],
            optional_vars: HashMap::new(),
            operation: "repository_analysis".to_string(),
            estimated_tokens: 2000,
            cache_ttl: 48,
        });

        // Performance templates
        self.add_template(QueryTemplate {
            name: "performance_bottlenecks".to_string(),
            description: "Identify performance bottlenecks".to_string(),
            pattern: "Analyze {file} for performance issues. Check:\n\
                      - Inefficient algorithms (O(n²) or worse)\n\
                      - Unnecessary allocations\n\
                      - Missing caching opportunities\n\
                      - Blocking I/O in critical paths\n\
                      - Database query optimization\n\
                      Suggest specific optimizations."
                .to_string(),
            required_vars: vec!["file".to_string()],
            optional_vars: HashMap::new(),
            operation: "file_scoring".to_string(),
            estimated_tokens: 800,
            cache_ttl: 72,
        });

        // Architecture templates
        self.add_template(QueryTemplate {
            name: "architecture_review".to_string(),
            description: "Review overall architecture".to_string(),
            pattern: "Review the architecture of {repo}. Analyze:\n\
                      - Module organization and separation of concerns\n\
                      - Dependency relationships\n\
                      - Design patterns used\n\
                      - Scalability considerations\n\
                      - Maintainability\n\
                      Provide architectural recommendations."
                .to_string(),
            required_vars: vec!["repo".to_string()],
            optional_vars: HashMap::new(),
            operation: "repository_analysis".to_string(),
            estimated_tokens: 3000,
            cache_ttl: 168,
        });

        self.add_template(QueryTemplate {
            name: "api_design_review".to_string(),
            description: "Review API design and contracts".to_string(),
            pattern: "Review API design in {file}. Evaluate:\n\
                      - Function signatures and clarity\n\
                      - Error handling strategy\n\
                      - Input validation\n\
                      - Return types and consistency\n\
                      - Documentation quality\n\
                      Suggest improvements for better API design."
                .to_string(),
            required_vars: vec!["file".to_string()],
            optional_vars: HashMap::new(),
            operation: "file_scoring".to_string(),
            estimated_tokens: 700,
            cache_ttl: 168,
        });

        // Documentation templates
        self.add_template(QueryTemplate {
            name: "generate_docs".to_string(),
            description: "Generate documentation for code".to_string(),
            pattern: "Generate comprehensive documentation for {file}. Include:\n\
                      - Module overview\n\
                      - Function descriptions with parameters and return values\n\
                      - Usage examples\n\
                      - Important notes or warnings\n\
                      Format as Rust doc comments (//)."
                .to_string(),
            required_vars: vec!["file".to_string()],
            optional_vars: HashMap::new(),
            operation: "documentation_generation".to_string(),
            estimated_tokens: 1500,
            cache_ttl: 336, // 2 weeks
        });

        self.add_template(QueryTemplate {
            name: "explain_code".to_string(),
            description: "Explain what code does in plain English".to_string(),
            pattern: "Explain what {file} does in simple terms. Cover:\n\
                      - Overall purpose\n\
                      - Main components and their roles\n\
                      - Key algorithms or logic\n\
                      - Important dependencies\n\
                      - Usage scenarios\n\
                      Make it understandable for someone new to the codebase."
                .to_string(),
            required_vars: vec!["file".to_string()],
            optional_vars: HashMap::new(),
            operation: "context_query".to_string(),
            estimated_tokens: 1000,
            cache_ttl: 168,
        });

        // Refactoring templates
        self.add_template(QueryTemplate {
            name: "refactor_suggestions".to_string(),
            description: "Suggest refactoring opportunities".to_string(),
            pattern: "Analyze {file} for refactoring opportunities. Suggest:\n\
                      - Functions that are too long or complex\n\
                      - Code that violates SOLID principles\n\
                      - Opportunities to extract common patterns\n\
                      - Better naming for clarity\n\
                      - Simplified logic flows\n\
                      Prioritize by impact and effort."
                .to_string(),
            required_vars: vec!["file".to_string()],
            optional_vars: HashMap::new(),
            operation: "file_scoring".to_string(),
            estimated_tokens: 900,
            cache_ttl: 48,
        });

        self.add_template(QueryTemplate {
            name: "modernize_code".to_string(),
            description: "Suggest modern Rust patterns and idioms".to_string(),
            pattern: "Review {file} for outdated patterns. Suggest:\n\
                      - Modern Rust idioms (2021 edition)\n\
                      - Better use of std library features\n\
                      - Async/await opportunities\n\
                      - More ergonomic APIs\n\
                      - Latest best practices\n\
                      Provide code examples for key improvements."
                .to_string(),
            required_vars: vec!["file".to_string()],
            optional_vars: HashMap::new(),
            operation: "file_scoring".to_string(),
            estimated_tokens: 1200,
            cache_ttl: 168,
        });

        // Testing templates
        self.add_template(QueryTemplate {
            name: "test_coverage_analysis".to_string(),
            description: "Analyze test coverage and gaps".to_string(),
            pattern: "Analyze test coverage for {file}. Identify:\n\
                      - Functions without tests\n\
                      - Edge cases not covered\n\
                      - Missing error path tests\n\
                      - Integration test opportunities\n\
                      - Property-based test candidates\n\
                      Suggest specific tests to add."
                .to_string(),
            required_vars: vec!["file".to_string()],
            optional_vars: HashMap::new(),
            operation: "file_scoring".to_string(),
            estimated_tokens: 800,
            cache_ttl: 72,
        });

        self.add_template(QueryTemplate {
            name: "generate_tests".to_string(),
            description: "Generate unit tests for code".to_string(),
            pattern: "Generate unit tests for {file}. Include:\n\
                      - Happy path tests\n\
                      - Edge case tests\n\
                      - Error handling tests\n\
                      - Property-based tests where applicable\n\
                      Use Rust test syntax with #[test] and assertions."
                .to_string(),
            required_vars: vec!["file".to_string()],
            optional_vars: HashMap::new(),
            operation: "test_generation".to_string(),
            estimated_tokens: 2000,
            cache_ttl: 336,
        });

        // General templates
        self.add_template(QueryTemplate {
            name: "quick_review".to_string(),
            description: "Quick code review focusing on common issues".to_string(),
            pattern: "Quick review of {file}. Check for:\n\
                      - Obvious bugs or logic errors\n\
                      - Common anti-patterns\n\
                      - Missing error handling\n\
                      - Security red flags\n\
                      - Style inconsistencies\n\
                      Provide top 3-5 issues found."
                .to_string(),
            required_vars: vec!["file".to_string()],
            optional_vars: HashMap::new(),
            operation: "quick_analysis".to_string(),
            estimated_tokens: 400,
            cache_ttl: 24,
        });

        self.add_template(QueryTemplate {
            name: "compare_approaches".to_string(),
            description: "Compare different implementation approaches".to_string(),
            pattern: "Compare these approaches for {task}:\n\
                      Approach 1: {approach1}\n\
                      Approach 2: {approach2}\n\
                      Evaluate based on:\n\
                      - Performance\n\
                      - Maintainability\n\
                      - Complexity\n\
                      - Trade-offs\n\
                      Recommend the better choice with reasoning."
                .to_string(),
            required_vars: vec![
                "task".to_string(),
                "approach1".to_string(),
                "approach2".to_string(),
            ],
            optional_vars: HashMap::new(),
            operation: "context_query".to_string(),
            estimated_tokens: 800,
            cache_ttl: 336,
        });
    }

    // Add a template to the registry
    pub fn add_template(&mut self, template: QueryTemplate) {
        self.templates.insert(template.name.clone(), template);
    }

    // Get a template by name
    pub fn get(&self, name: &str) -> Result<&QueryTemplate> {
        self.templates
            .get(name)
            .ok_or_else(|| anyhow::anyhow!("Template '{}' not found", name))
    }

    // List all available templates
    pub fn list(&self) -> Vec<&QueryTemplate> {
        self.templates.values().collect()
    }

    // List templates by category
    pub fn list_by_category(&self, category: TemplateCategory) -> Vec<&QueryTemplate> {
        let category_str = category.as_str();
        self.templates
            .values()
            .filter(|t| t.name.starts_with(category_str) || t.operation.contains(category_str))
            .collect()
    }

    // Search templates by keyword
    pub fn search(&self, keyword: &str) -> Vec<&QueryTemplate> {
        let keyword_lower = keyword.to_lowercase();
        self.templates
            .values()
            .filter(|t| {
                t.name.to_lowercase().contains(&keyword_lower)
                    || t.description.to_lowercase().contains(&keyword_lower)
            })
            .collect()
    }
}

impl QueryTemplate {
    // Render the template with provided variables
    pub fn render(&self, vars: &[(&str, &str)]) -> Result<String> {
        let mut result = self.pattern.clone();

        // Check required variables
        for required in &self.required_vars {
            if !vars.iter().any(|(k, _)| k == required) {
                anyhow::bail!("Required variable '{}' not provided", required);
            }
        }

        // Substitute variables
        for (key, value) in vars {
            let placeholder = format!("{{{}}}", key);
            result = result.replace(&placeholder, value);
        }

        // Apply optional variables (defaults)
        for (key, default_value) in &self.optional_vars {
            let placeholder = format!("{{{}}}", key);
            if result.contains(&placeholder) {
                result = result.replace(&placeholder, default_value);
            }
        }

        // Check for unresolved placeholders
        if result.contains('{') && result.contains('}') {
            anyhow::bail!("Template contains unresolved placeholders");
        }

        Ok(result)
    }

    // Render with a hashmap of variables
    pub fn render_map(&self, vars: &HashMap<String, String>) -> Result<String> {
        let vars_vec: Vec<(&str, &str)> =
            vars.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
        self.render(&vars_vec)
    }

    // Get estimated cost based on token count
    pub fn estimated_cost(&self, cost_per_1k_tokens: f64) -> f64 {
        (self.estimated_tokens as f64 / 1000.0) * cost_per_1k_tokens
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_template_rendering() {
        let template = QueryTemplate {
            name: "test".to_string(),
            description: "test template".to_string(),
            pattern: "Analyze {file} for {issue}".to_string(),
            required_vars: vec!["file".to_string(), "issue".to_string()],
            optional_vars: HashMap::new(),
            operation: "test".to_string(),
            estimated_tokens: 100,
            cache_ttl: 24,
        };

        let result = template
            .render(&[("file", "main.rs"), ("issue", "bugs")])
            .unwrap();
        assert_eq!(result, "Analyze main.rs for bugs");
    }

    #[test]
    fn test_missing_required_var() {
        let template = QueryTemplate {
            name: "test".to_string(),
            description: "test template".to_string(),
            pattern: "Analyze {file}".to_string(),
            required_vars: vec!["file".to_string()],
            optional_vars: HashMap::new(),
            operation: "test".to_string(),
            estimated_tokens: 100,
            cache_ttl: 24,
        };

        let result = template.render(&[]);
        assert!(result.is_err());
    }

    #[test]
    fn test_registry() {
        let registry = TemplateRegistry::new();

        // Should have built-in templates
        assert!(!registry.templates.is_empty());

        // Should be able to get a template
        let template = registry.get("security_audit");
        assert!(template.is_ok());
    }

    #[test]
    fn test_template_search() {
        let registry = TemplateRegistry::new();

        let results = registry.search("security");
        assert!(!results.is_empty());

        let results = registry.search("refactor");
        assert!(!results.is_empty());
    }
}
