//! # Context Builder for RAG
//!
//! Smart context management for Grok's 2M token context window.
//! Uses "context stuffing" approach - loads relevant code into context
//! instead of using vector embeddings.
//!
//! ## Features
//!
//! - Load entire repositories into context
//! - Smart filtering by language, path, or recency
//! - Query-aware context selection
//! - Cross-repository analysis
//! - Token budget management
//! - Response caching
//!
//! ## Usage
//!
//! ```rust,no_run
//! use rustcode::context_builder::ContextBuilder;
//! use rustcode::db::Database;
//!
//! #[tokio::main]
//! async fn main() -> anyhow::Result<()> {
//!     let db = Database::new("data/rustcode.db").await?;
//!     let builder = ContextBuilder::new(db);
//!
//!     // Build context for a query
//!     let context = builder
//!         .with_repository("myapp")
//!         .with_language("Rust")
//!         .with_recent_files(20)
//!         .build()
//!         .await?;
//!
//!     println!("Context: {} tokens", context.estimated_tokens());
//!
//!     Ok(())
//! }
//! ```

use crate::db::Database;
use crate::repo_analysis::RepoAnalyzer;
use anyhow::Result;
use serde::{Deserialize, Serialize};

/// Maximum tokens for Grok context window (grok-4-1-fast has 2M limit, use 1.5M to be safe)
const MAX_CONTEXT_TOKENS: usize = 1_500_000;

/// Estimated tokens per character (conservative)
const TOKENS_PER_CHAR: f64 = 0.3;

/// Context builder for RAG queries
#[derive(Clone)]
pub struct ContextBuilder {
    db: Database,
    repositories: Vec<String>,
    languages: Vec<String>,
    paths: Vec<String>,
    exclude_patterns: Vec<String>,
    max_files: Option<usize>,
    recent_only: Option<usize>,
    include_notes: bool,
    max_tokens: usize,
}

/// Built context ready for LLM
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Context {
    /// Files included in context
    pub files: Vec<ContextFile>,
    /// Notes included in context
    pub notes: Vec<String>,
    /// Repositories included
    pub repositories: Vec<String>,
    /// Total character count
    pub total_chars: usize,
    /// Estimated token count
    pub estimated_tokens: usize,
    /// Metadata about context
    pub metadata: ContextMetadata,
}

/// A file in the context
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextFile {
    /// Repository name
    pub repository: String,
    /// File path (relative to repo)
    pub path: String,
    /// File content
    pub content: String,
    /// Programming language
    pub language: Option<String>,
    /// File size in bytes
    pub size: usize,
}

/// Context metadata
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextMetadata {
    /// Number of files included
    pub file_count: usize,
    /// Number of notes included
    pub note_count: usize,
    /// Number of repositories
    pub repository_count: usize,
    /// Languages represented
    pub languages: Vec<String>,
    /// Total size in bytes
    pub total_bytes: usize,
    /// Estimated tokens
    pub estimated_tokens: usize,
    /// Whether context was truncated
    pub truncated: bool,
}

impl ContextBuilder {
    /// Create a new context builder
    pub fn new(db: Database) -> Self {
        Self {
            db,
            repositories: Vec::new(),
            languages: Vec::new(),
            paths: Vec::new(),
            exclude_patterns: vec![
                "target/".to_string(),
                "node_modules/".to_string(),
                ".git/".to_string(),
                "test".to_string(), // Can exclude tests to save tokens
            ],
            max_files: None,
            recent_only: None,
            include_notes: false,
            max_tokens: MAX_CONTEXT_TOKENS, // Default to safe limit
        }
    }

    /// Add a repository to the context
    pub fn with_repository(mut self, name: impl Into<String>) -> Self {
        self.repositories.push(name.into());
        self
    }

    /// Add all tracked repositories
    pub fn with_all_repositories(mut self) -> Self {
        self.repositories.clear(); // Empty means all
        self
    }

    /// Filter by programming language
    pub fn with_language(mut self, language: impl Into<String>) -> Self {
        self.languages.push(language.into());
        self
    }

    /// Filter by path pattern (glob-like)
    pub fn with_path(mut self, path: impl Into<String>) -> Self {
        self.paths.push(path.into());
        self
    }

    /// Exclude pattern from context
    pub fn exclude_pattern(mut self, pattern: impl Into<String>) -> Self {
        self.exclude_patterns.push(pattern.into());
        self
    }

    /// Include only N most recent files
    pub fn with_recent_files(mut self, count: usize) -> Self {
        self.recent_only = Some(count);
        self
    }

    /// Limit maximum number of files
    pub fn max_files(mut self, count: usize) -> Self {
        self.max_files = Some(count);
        self
    }

    /// Include notes in context
    pub fn with_notes(mut self) -> Self {
        self.include_notes = true;
        self
    }

    /// Set maximum token budget
    pub fn max_tokens(mut self, tokens: usize) -> Self {
        self.max_tokens = tokens;
        self
    }

    /// Build the context
    pub async fn build(self) -> Result<Context> {
        let mut files = Vec::new();
        let mut total_chars = 0usize;
        let mut languages_set = std::collections::HashSet::new();

        // Get repositories to include
        let repos = if self.repositories.is_empty() {
            self.db.list_repositories().await?
        } else {
            let mut result = Vec::new();
            for name in &self.repositories {
                match self.db.get_repository(name).await {
                    Ok(repo) => result.push(repo),
                    Err(_) => continue, // Skip if repository not found
                }
            }
            result
        };

        // Analyze each repository
        for repo in &repos {
            let analyzer = RepoAnalyzer::new(&repo.path);
            let tree = analyzer.build_tree().await?;

            // Get files from tree
            let mut repo_files = RepoAnalyzer::get_all_files(&tree);

            // Apply language filter
            if !self.languages.is_empty() {
                repo_files.retain(|f| {
                    f.metadata
                        .as_ref()
                        .and_then(|m| m.language.as_ref())
                        .map(|l| self.languages.iter().any(|filter| l.contains(filter)))
                        .unwrap_or(false)
                });
            }

            // Apply path filter
            if !self.paths.is_empty() {
                repo_files.retain(|f| {
                    let path_str = f.path.to_string_lossy();
                    self.paths.iter().any(|pattern| path_str.contains(pattern))
                });
            }

            // Apply exclude patterns
            repo_files.retain(|f| {
                let path_str = f.path.to_string_lossy();
                !self
                    .exclude_patterns
                    .iter()
                    .any(|pattern| path_str.contains(pattern))
            });

            // Sort by recency if requested
            if self.recent_only.is_some() {
                repo_files.sort_by(|a, b| {
                    let time_a = a
                        .metadata
                        .as_ref()
                        .map(|m| m.modified)
                        .unwrap_or_else(chrono::Utc::now);
                    let time_b = b
                        .metadata
                        .as_ref()
                        .map(|m| m.modified)
                        .unwrap_or_else(chrono::Utc::now);
                    time_b.cmp(&time_a)
                });

                if let Some(limit) = self.recent_only {
                    repo_files.truncate(limit);
                }
            }

            // Apply max files limit
            if let Some(max) = self.max_files {
                repo_files.truncate(max);
            }

            // Load file contents
            for file_node in repo_files {
                // Check token budget
                let estimated_tokens = (total_chars as f64 * TOKENS_PER_CHAR) as usize;
                if estimated_tokens >= self.max_tokens {
                    break;
                }

                // Skip binary files
                if let Some(ref metadata) = file_node.metadata {
                    if metadata.is_binary {
                        continue;
                    }
                }

                // Read file content
                if let Ok(content) = tokio::fs::read_to_string(&file_node.path).await {
                    let relative_path = file_node
                        .path
                        .strip_prefix(&repo.path)
                        .unwrap_or(&file_node.path)
                        .to_string_lossy()
                        .to_string();

                    let language = file_node.metadata.as_ref().and_then(|m| m.language.clone());

                    if let Some(ref lang) = language {
                        languages_set.insert(lang.clone());
                    }

                    let size = content.len();
                    total_chars += size;

                    files.push(ContextFile {
                        repository: repo.name.clone(),
                        path: relative_path,
                        content,
                        language,
                        size,
                    });
                }
            }
        }

        // Include notes if requested
        let notes = if self.include_notes {
            let inbox_notes = self
                .db
                .list_notes(Some(crate::db::NoteStatus::Inbox), None, None)
                .await?;
            let active_notes = self
                .db
                .list_notes(Some(crate::db::NoteStatus::Active), None, None)
                .await?;

            let mut note_contents = Vec::new();
            for note in inbox_notes.iter().chain(active_notes.iter()) {
                let note_text = format!("[{}] {}", note.status, note.content);
                total_chars += note_text.len();
                note_contents.push(note_text);
            }
            note_contents
        } else {
            Vec::new()
        };

        let estimated_tokens = (total_chars as f64 * TOKENS_PER_CHAR) as usize;
        let truncated = estimated_tokens >= self.max_tokens;

        let metadata = ContextMetadata {
            file_count: files.len(),
            note_count: notes.len(),
            repository_count: repos.len(),
            languages: languages_set.into_iter().collect(),
            total_bytes: total_chars,
            estimated_tokens,
            truncated,
        };

        Ok(Context {
            files,
            notes,
            repositories: repos.into_iter().map(|r| r.name).collect(),
            total_chars,
            estimated_tokens,
            metadata,
        })
    }
}

impl Context {
    /// Get the context as a formatted string for LLM
    pub fn to_prompt(&self) -> String {
        let mut prompt = String::new();

        prompt.push_str("# Codebase Context\n\n");

        // Summary
        prompt.push_str(&format!(
            "Files: {}, Repositories: {}, Languages: {}\n\n",
            self.metadata.file_count,
            self.metadata.repository_count,
            self.metadata.languages.join(", ")
        ));

        // Notes if included
        if !self.notes.is_empty() {
            prompt.push_str("## Current Notes\n\n");
            for note in &self.notes {
                prompt.push_str(&format!("- {}\n", note));
            }
            prompt.push('\n');
        }

        // Files
        prompt.push_str("## Source Files\n\n");
        for file in &self.files {
            let lang = file.language.as_deref().unwrap_or("text");
            prompt.push_str(&format!("### {}/{}\n", file.repository, file.path));
            prompt.push_str(&format!("```{}\n", lang.to_lowercase()));
            prompt.push_str(&file.content);
            if !file.content.ends_with('\n') {
                prompt.push('\n');
            }
            prompt.push_str("```\n\n");
        }

        if self.metadata.truncated {
            prompt.push_str("*Note: Context was truncated to fit token budget*\n");
        }

        prompt
    }

    /// Get estimated token count
    pub fn estimated_tokens(&self) -> usize {
        self.estimated_tokens
    }

    /// Get character count
    pub fn char_count(&self) -> usize {
        self.total_chars
    }

    /// Get file count
    pub fn file_count(&self) -> usize {
        self.metadata.file_count
    }

    /// Check if context was truncated
    pub fn is_truncated(&self) -> bool {
        self.metadata.truncated
    }

    /// Get files by language
    pub fn files_by_language(&self, language: &str) -> Vec<&ContextFile> {
        self.files
            .iter()
            .filter(|f| f.language.as_ref().map(|l| l == language).unwrap_or(false))
            .collect()
    }

    /// Find files matching a path pattern
    pub fn find_files(&self, pattern: &str) -> Vec<&ContextFile> {
        self.files
            .iter()
            .filter(|f| f.path.contains(pattern))
            .collect()
    }

    /// Get summary statistics
    pub fn summary(&self) -> String {
        format!(
            "{} files, {} notes, {} repos, ~{} tokens, {} languages",
            self.metadata.file_count,
            self.metadata.note_count,
            self.metadata.repository_count,
            self.estimated_tokens,
            self.metadata.languages.len()
        )
    }
}

/// Query builder for context-aware LLM queries
pub struct QueryBuilder {
    context_builder: ContextBuilder,
    question: String,
    focus_files: Vec<String>,
    focus_language: Option<String>,
}

impl QueryBuilder {
    /// Create a new query builder
    pub fn new(db: Database, question: impl Into<String>) -> Self {
        Self {
            context_builder: ContextBuilder::new(db),
            question: question.into(),
            focus_files: Vec::new(),
            focus_language: None,
        }
    }

    /// Focus on specific files
    pub fn focus_on_files(mut self, paths: Vec<String>) -> Self {
        self.focus_files = paths;
        self
    }

    /// Focus on a specific language
    pub fn focus_on_language(mut self, language: impl Into<String>) -> Self {
        self.focus_language = Some(language.into());
        self
    }

    /// Set repository
    pub fn in_repository(mut self, repo: impl Into<String>) -> Self {
        self.context_builder = self.context_builder.with_repository(repo);
        self
    }

    /// Include notes
    pub fn with_notes(mut self) -> Self {
        self.context_builder = self.context_builder.with_notes();
        self
    }

    /// Build the query with context
    pub async fn build(mut self) -> Result<String> {
        // Apply focus filters
        if let Some(lang) = self.focus_language {
            self.context_builder = self.context_builder.with_language(lang);
        }

        for path in self.focus_files {
            self.context_builder = self.context_builder.with_path(path);
        }

        // Build context
        let context = self.context_builder.build().await?;

        // Construct query
        let prompt = format!(
            "{}\n\n# Question\n\n{}\n\nPlease analyze the codebase context above and answer the question.",
            context.to_prompt(),
            self.question
        );

        Ok(prompt)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_context_builder() -> Result<()> {
        let db = Database::new(&std::env::var("DATABASE_URL").unwrap_or_else(|_| {
            "postgresql://rustcode:changeme@localhost:5432/rustcode_test".to_string()
        }))
        .await?;

        let builder = ContextBuilder::new(db)
            .with_language("Rust")
            .max_files(10)
            .max_tokens(50000);

        // This would fail without actual repos, but tests the API
        assert!(builder.repositories.is_empty());

        Ok(())
    }

    #[test]
    fn test_token_estimation() {
        let chars = 10000;
        let estimated = (chars as f64 * TOKENS_PER_CHAR) as usize;
        assert_eq!(estimated, 3000);
    }
}
