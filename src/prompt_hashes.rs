//! Prompt template hashing for cache invalidation
//!
//! This module computes stable hashes of prompt templates to ensure cache
//! invalidation when prompts change. Each prompt template is hashed using
//! SHA-256 and the first 16 characters are used as a cache key component.

use sha2::{Digest, Sha256};

/// Compute SHA-256 hash of a string and return first 16 characters
fn hash_str(s: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(s.as_bytes());
    format!("{:x}", hasher.finalize())[..16].to_string()
}

// ============================================================================
// Refactor Prompts
// ============================================================================

/// Prompt template for refactoring analysis
pub const REFACTOR_PROMPT: &str = r#"
Analyze this Rust code for refactoring opportunities. Focus on:

1. Code smells (duplicated code, long functions, god objects)
2. Design patterns that could improve maintainability
3. Performance optimizations
4. Error handling improvements
5. Type safety enhancements

Provide specific, actionable suggestions with examples.

Code:
{code}

Respond in JSON format:
{
  "code_smells": [
    {
      "smell_type": "string",
      "severity": "High|Medium|Low",
      "description": "string",
      "location": {"line_start": number, "line_end": number},
      "suggestion": "string"
    }
  ],
  "suggestions": [
    {
      "category": "string",
      "priority": "High|Medium|Low",
      "description": "string",
      "example": "string"
    }
  ]
}
"#;

/// Hash of refactor prompt template (first 16 chars of SHA-256)
pub fn refactor_prompt_hash() -> String {
    hash_str(REFACTOR_PROMPT)
}

// ============================================================================
// Documentation Prompts
// ============================================================================

/// Prompt template for module documentation generation
pub const DOCS_MODULE_PROMPT: &str = r#"
Generate comprehensive documentation for this Rust module. Include:

1. Module purpose and overview
2. Key types and their responsibilities
3. Public API documentation
4. Usage examples
5. Common patterns and best practices

Code:
{code}

Respond in JSON format:
{
  "module_name": "string",
  "summary": "string",
  "description": "string",
  "types": [
    {
      "name": "string",
      "kind": "struct|enum|trait|function",
      "description": "string",
      "examples": ["string"]
    }
  ],
  "examples": ["string"]
}
"#;

/// Hash of docs module prompt template
pub fn docs_module_prompt_hash() -> String {
    hash_str(DOCS_MODULE_PROMPT)
}

/// Prompt template for README generation
pub const DOCS_README_PROMPT: &str = r#"
Generate a comprehensive README.md for this project. Include:

1. Project title and description
2. Key features
3. Installation instructions
4. Quick start guide
5. Architecture overview
6. Contributing guidelines

Project info:
{project_info}

Respond in JSON format:
{
  "title": "string",
  "description": "string",
  "features": ["string"],
  "installation": "string",
  "quick_start": "string",
  "architecture": "string",
  "contributing": "string"
}
"#;

/// Hash of docs readme prompt template
pub fn docs_readme_prompt_hash() -> String {
    hash_str(DOCS_README_PROMPT)
}

// ============================================================================
// Analysis Prompts
// ============================================================================

/// Prompt template for general code analysis
pub const ANALYSIS_PROMPT: &str = r#"
Perform a comprehensive analysis of this Rust code. Evaluate:

1. Code quality (0-100)
2. Security concerns
3. Complexity metrics
4. Maintainability issues
5. Test coverage recommendations

Code:
{code}

Respond in JSON format:
{
  "quality_score": number,
  "security_issues": [
    {
      "severity": "Critical|High|Medium|Low",
      "description": "string",
      "location": {"line_start": number, "line_end": number},
      "recommendation": "string"
    }
  ],
  "complexity_score": number,
  "maintainability_issues": ["string"],
  "test_recommendations": ["string"]
}
"#;

/// Hash of analysis prompt template
pub fn analysis_prompt_hash() -> String {
    hash_str(ANALYSIS_PROMPT)
}

// ============================================================================
// Helper Functions
// ============================================================================

/// Get prompt hash for a given cache type
pub fn get_prompt_hash(cache_type: &str) -> String {
    match cache_type {
        "refactor" => refactor_prompt_hash(),
        "docs" => docs_module_prompt_hash(),
        "analysis" => analysis_prompt_hash(),
        "todos" => "default".to_string(), // TODO scanner doesn't use prompts yet
        _ => "default".to_string(),
    }
}

/// Get prompt hash for cache type enum
pub fn get_prompt_hash_for_type(cache_type: crate::repo_cache::CacheType) -> String {
    use crate::repo_cache::CacheType;

    match cache_type {
        CacheType::Refactor => refactor_prompt_hash(),
        CacheType::Docs => docs_module_prompt_hash(),
        CacheType::Analysis => analysis_prompt_hash(),
        CacheType::Todos => "default".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hash_stability() {
        // Hashes should be stable across runs
        let hash1 = refactor_prompt_hash();
        let hash2 = refactor_prompt_hash();
        assert_eq!(hash1, hash2);
    }

    #[test]
    fn test_hash_length() {
        // All hashes should be 16 characters
        assert_eq!(refactor_prompt_hash().len(), 16);
        assert_eq!(docs_module_prompt_hash().len(), 16);
        assert_eq!(docs_readme_prompt_hash().len(), 16);
        assert_eq!(analysis_prompt_hash().len(), 16);
    }

    #[test]
    fn test_hash_uniqueness() {
        // Different prompts should produce different hashes
        let refactor = refactor_prompt_hash();
        let docs = docs_module_prompt_hash();
        let analysis = analysis_prompt_hash();

        assert_ne!(refactor, docs);
        assert_ne!(refactor, analysis);
        assert_ne!(docs, analysis);
    }

    #[test]
    fn test_hash_format() {
        // Hashes should be valid hex
        let hash = refactor_prompt_hash();
        assert!(hash.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn test_get_prompt_hash() {
        assert_eq!(get_prompt_hash("refactor"), refactor_prompt_hash());
        assert_eq!(get_prompt_hash("docs"), docs_module_prompt_hash());
        assert_eq!(get_prompt_hash("analysis"), analysis_prompt_hash());
        assert_eq!(get_prompt_hash("unknown"), "default");
    }
}
