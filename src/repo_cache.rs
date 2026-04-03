//! Repository-level cache module for storing analysis results
//!
//! This module provides caching at the repository level by storing
//! analysis results in `.rustcode/cache/` directories within each repo.
//!
//! ## Cache Structure
//!
//! ```text
//! <repo>/.rustcode/
//!   ├── cache/
//!   │   ├── analysis/      # General analysis results
//!   │   ├── docs/          # Documentation generation results
//!   │   ├── refactor/      # Refactoring analysis results
//!   │   └── todos/         # TODO scan results
//!   ├── config.toml        # Repo-specific config
//!   └── README.md          # Cache documentation
//! ```
//!
//! ## Features
//!
//! - Content-based invalidation (SHA-256 hashing)
//! - Separate cache directories by analysis type
//! - Automatic cache miss/hit tracking
//! - JSON storage for human readability
//! - Repository-specific caching (no global cache pollution)
//!
//! ## Usage
//!
//! ```rust,no_run
//! use rustcode::repo_cache::{RepoCache, CacheType, CacheSetParams};
//! use std::path::Path;
//!
//! # async fn example() -> anyhow::Result<()> {
//! let repo_path = Path::new("/path/to/repo");
//! let cache = RepoCache::new(repo_path)?;
//!
//! // Check for cached result
//! let file_content = "fn main() {}";
//! if let Some(cached) = cache.get(CacheType::Refactor, "src/main.rs", file_content)? {
//!     println!("Using cached analysis");
//! } else {
//!     // Perform analysis...
//!     let result = serde_json::json!({"score": 95});
//!     cache.set(CacheSetParams {
//!         cache_type: CacheType::Refactor,
//!         file_path: "src/main.rs",
//!         content: file_content,
//!         provider: "xai",
//!         model: "grok-beta",
//!         result,
//!         tokens_used: Some(150),
//!         prompt_hash: None,
//!         schema_version: None,
//!     })?;
//! }
//! # Ok(())
//! # }
//! ```

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};
use tracing::{debug, info};

/// Directory name for repo-level cache
pub const REPO_CACHE_DIR: &str = ".rustcode";

/// Cache storage strategy
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CacheStrategy {
    /// Centralized cache in ~/.rustcode/cache/repos/<hash>/
    #[default]
    Centralized,
    /// Local cache in <repo>/.rustcode/cache/
    Local,
}

/// Compute stable hash of repository path
fn compute_repo_hash(path: &Path) -> String {
    let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let path_str = canonical.display().to_string();

    // Use SHA256 for consistency with file hashing
    let mut hasher = Sha256::new();
    hasher.update(path_str.as_bytes());
    let result = hasher.finalize();
    format!("{:x}", result)[..8].to_string()
}

/// Cache types for different analysis results
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheType {
    /// General analysis results
    Analysis,
    /// Documentation generation
    Docs,
    /// Refactoring analysis
    Refactor,
    /// TODO/FIXME scans
    Todos,
}

impl CacheType {
    /// Get the subdirectory name for this cache type
    pub fn subdirectory(&self) -> &'static str {
        match self {
            CacheType::Analysis => "analysis",
            CacheType::Docs => "docs",
            CacheType::Refactor => "refactor",
            CacheType::Todos => "todos",
        }
    }
}

/// Cache entry for repository-level caching
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepoCacheEntry {
    /// File path (relative to repo root)
    pub file_path: String,

    /// SHA-256 hash of file content when analyzed
    pub file_hash: String,

    /// Multi-factor cache key (includes file, model, prompt, schema)
    #[serde(default)]
    pub cache_key: String,

    /// Timestamp when analysis was performed (RFC3339)
    pub analyzed_at: String,

    /// LLM provider used
    pub provider: String,

    /// Model used
    pub model: String,

    /// Prompt template hash (first 16 chars of SHA-256)
    #[serde(default)]
    pub prompt_hash: String,

    /// Schema version for this analysis type
    #[serde(default = "default_schema_version")]
    pub schema_version: u32,

    /// Analysis result (JSON)
    pub result: serde_json::Value,

    /// Token count (if available)
    pub tokens_used: Option<usize>,

    /// File size in bytes
    pub file_size: usize,

    /// Cache type
    pub cache_type: String,
}

/// Default schema version
fn default_schema_version() -> u32 {
    1
}

/// Parameters for setting a cache entry
pub struct CacheSetParams<'a> {
    /// Cache type
    pub cache_type: CacheType,
    /// File path (relative to repo root)
    pub file_path: &'a str,
    /// File content
    pub content: &'a str,
    /// LLM provider
    pub provider: &'a str,
    /// Model name
    pub model: &'a str,
    /// Analysis result
    pub result: serde_json::Value,
    /// Token count (if available)
    pub tokens_used: Option<usize>,
    /// Prompt hash (optional, will be computed if not provided)
    pub prompt_hash: Option<&'a str>,
    /// Schema version (optional, defaults to 1)
    pub schema_version: Option<u32>,
}

/// Repository cache manager
pub struct RepoCache {
    /// Cache directory (.rustcode)
    cache_dir: PathBuf,

    /// Whether cache is enabled
    enabled: bool,
}

impl RepoCache {
    /// Create a new repository cache with specified strategy
    ///
    /// This will create the cache structure if it doesn't exist.
    pub fn new_with_strategy(
        repo_root: impl AsRef<Path>,
        strategy: CacheStrategy,
    ) -> anyhow::Result<Self> {
        let repo_path = repo_root.as_ref();

        let cache_dir = match strategy {
            CacheStrategy::Centralized => {
                let repo_hash = compute_repo_hash(repo_path);
                let base = dirs::home_dir()
                    .ok_or_else(|| anyhow::anyhow!("No home directory"))?
                    .join(".rustcode/cache/repos")
                    .join(&repo_hash);

                // Create meta.json with repo info
                fs::create_dir_all(&base)?;
                let meta = serde_json::json!({
                    "path": repo_path.display().to_string(),
                    "hash": repo_hash,
                    "schema_version": 1,
                    "created_at": chrono::Utc::now().to_rfc3339(),
                });
                fs::write(base.join("meta.json"), serde_json::to_string_pretty(&meta)?)?;

                base
            }
            CacheStrategy::Local => repo_path.join(REPO_CACHE_DIR),
        };

        let cache = Self {
            cache_dir,
            enabled: true,
        };

        // Initialize cache structure
        cache.ensure_cache_structure()?;

        Ok(cache)
    }

    /// Create a new repository cache (uses default centralized strategy)
    ///
    /// This will create the cache structure if it doesn't exist.
    pub fn new(repo_root: impl AsRef<Path>) -> anyhow::Result<Self> {
        Self::new_with_strategy(repo_root, CacheStrategy::default())
    }

    /// Create a disabled cache (no-op)
    pub fn disabled() -> Self {
        Self {
            cache_dir: PathBuf::new(),
            enabled: false,
        }
    }

    /// Check if cache is enabled
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Get the cache directory path
    pub fn cache_dir(&self) -> &Path {
        &self.cache_dir
    }

    /// Ensure cache directory structure exists
    fn ensure_cache_structure(&self) -> anyhow::Result<()> {
        if !self.enabled {
            return Ok(());
        }

        // Create main cache directory
        if !self.cache_dir.exists() {
            fs::create_dir_all(&self.cache_dir)?;
            info!("Created repo cache directory: {}", self.cache_dir.display());

            // Create README
            self.create_readme()?;
        }

        // Create subdirectories for each cache type
        for cache_type in &[
            CacheType::Analysis,
            CacheType::Docs,
            CacheType::Refactor,
            CacheType::Todos,
        ] {
            let subdir = self.cache_dir.join("cache").join(cache_type.subdirectory());
            if !subdir.exists() {
                fs::create_dir_all(&subdir)?;
                debug!("Created cache subdirectory: {}", subdir.display());
            }
        }

        Ok(())
    }

    /// Create README for cache directory
    fn create_readme(&self) -> anyhow::Result<()> {
        let readme_path = self.cache_dir.join("README.md");
        if !readme_path.exists() {
            let content = r#"# RustCode Cache Directory

This directory contains cached analysis results from RustCode.

## Structure

- `cache/analysis/` - General code analysis results
- `cache/docs/` - Generated documentation
- `cache/refactor/` - Refactoring suggestions and code smell detection
- `cache/todos/` - TODO/FIXME scan results

## Cache Invalidation

Cache entries are automatically invalidated when file contents change.
Each entry stores a SHA-256 hash of the analyzed file content.

## Managing the Cache

You can safely delete this directory to clear all cached results.
RustCode will regenerate cache entries as needed.

To disable caching, set `cache.enabled = false` in your config.

## Committing to Git

You can optionally commit this directory to version control to share
analysis results with your team. This can save API costs and speed up
analysis for unchanged files.

Add to `.gitignore` if you prefer not to track cache files:
```
.rustcode/cache/
```
"#;
            fs::write(&readme_path, content)?;
            info!("Created cache README: {}", readme_path.display());
        }
        Ok(())
    }

    /// Calculate SHA-256 hash of content
    fn hash_content(&self, content: &str) -> String {
        let mut hasher = Sha256::new();
        hasher.update(content.as_bytes());
        format!("{:x}", hasher.finalize())
    }

    /// Compute multi-factor cache key
    ///
    /// Combines file hash, model ID, prompt hash, and schema version to create
    /// a unique cache key that invalidates when any of these factors change.
    fn compute_cache_key(
        &self,
        file_hash: &str,
        model: &str,
        prompt_hash: &str,
        schema_version: u32,
    ) -> String {
        let combined = format!("{}:{}:{}:{}", file_hash, model, prompt_hash, schema_version);
        let mut hasher = Sha256::new();
        hasher.update(combined.as_bytes());
        format!("{:x}", hasher.finalize())[..32].to_string()
    }

    /// Get cache file path for a file
    fn cache_file_path(&self, cache_type: CacheType, file_path: &str) -> PathBuf {
        // Sanitize file path to create safe cache filename
        let safe_name = file_path.replace(['/', '\\'], "_").replace(['.', ':'], "_");

        self.cache_dir
            .join("cache")
            .join(cache_type.subdirectory())
            .join(format!("{}.json", safe_name))
    }

    /// Get cached analysis result for a file
    /// Get cached analysis result if available and valid
    ///
    /// Returns None if:
    /// - Cache is disabled
    /// - No cache entry exists
    /// - Cache entry exists but content has changed (stale)
    /// - Cache entry exists but model/prompt has changed (stale)
    pub fn get(
        &self,
        cache_type: CacheType,
        file_path: &str,
        current_content: &str,
    ) -> anyhow::Result<Option<RepoCacheEntry>> {
        self.get_with_validation(cache_type, file_path, current_content, None, None)
    }

    /// Get cached analysis with explicit model and prompt validation
    ///
    /// This validates that the cache entry was created with the same model and prompt.
    pub fn get_with_validation(
        &self,
        cache_type: CacheType,
        file_path: &str,
        current_content: &str,
        model: Option<&str>,
        prompt_hash: Option<&str>,
    ) -> anyhow::Result<Option<RepoCacheEntry>> {
        if !self.enabled {
            return Ok(None);
        }

        let cache_file = self.cache_file_path(cache_type, file_path);
        if !cache_file.exists() {
            debug!(
                "Cache MISS (no entry): {} / {}",
                cache_type.subdirectory(),
                file_path
            );
            return Ok(None);
        }

        // Load cache entry
        let content = fs::read_to_string(&cache_file)?;
        let entry: RepoCacheEntry = serde_json::from_str(&content)?;

        // Check if content hash matches
        let current_hash = self.hash_content(current_content);
        if entry.file_hash != current_hash {
            debug!(
                "Cache STALE (content changed): {} / {}",
                cache_type.subdirectory(),
                file_path
            );
            return Ok(None);
        }

        // Validate model if provided
        if let Some(expected_model) = model {
            if entry.model != expected_model {
                debug!(
                    "Cache STALE (model changed): {} / {} (cached: {}, current: {})",
                    cache_type.subdirectory(),
                    file_path,
                    entry.model,
                    expected_model
                );
                return Ok(None);
            }
        }

        // Validate prompt hash if provided
        if let Some(expected_prompt) = prompt_hash {
            if !entry.prompt_hash.is_empty() && entry.prompt_hash != expected_prompt {
                debug!(
                    "Cache STALE (prompt changed): {} / {}",
                    cache_type.subdirectory(),
                    file_path
                );
                return Ok(None);
            }
        }

        // Validate using multi-factor cache key if available
        if !entry.cache_key.is_empty() {
            let current_prompt = prompt_hash
                .map(|s| s.to_string())
                .unwrap_or_else(|| crate::prompt_hashes::get_prompt_hash_for_type(cache_type));
            let current_model = model.unwrap_or(&entry.model);
            let expected_key = self.compute_cache_key(
                &current_hash,
                current_model,
                &current_prompt,
                entry.schema_version,
            );

            if entry.cache_key != expected_key {
                debug!(
                    "Cache STALE (key mismatch): {} / {}",
                    cache_type.subdirectory(),
                    file_path
                );
                return Ok(None);
            }
        }

        debug!("Cache HIT: {} / {}", cache_type.subdirectory(), file_path);
        Ok(Some(entry))
    }

    /// Store analysis result in cache
    pub fn set(&self, params: CacheSetParams) -> anyhow::Result<()> {
        if !self.enabled {
            return Ok(());
        }

        // Compute prompt hash if not provided
        let prompt_hash = params
            .prompt_hash
            .map(|s| s.to_string())
            .unwrap_or_else(|| crate::prompt_hashes::get_prompt_hash_for_type(params.cache_type));

        let schema_version = params.schema_version.unwrap_or(1);

        // Compute multi-factor cache key
        let file_hash = self.hash_content(params.content);
        let cache_key =
            self.compute_cache_key(&file_hash, params.model, &prompt_hash, schema_version);

        let entry = RepoCacheEntry {
            file_path: params.file_path.to_string(),
            file_hash,
            cache_key,
            analyzed_at: chrono::Utc::now().to_rfc3339(),
            provider: params.provider.to_string(),
            model: params.model.to_string(),
            prompt_hash,
            schema_version,
            result: params.result,
            tokens_used: params.tokens_used,
            file_size: params.content.len(),
            cache_type: params.cache_type.subdirectory().to_string(),
        };

        let cache_file = self.cache_file_path(params.cache_type, params.file_path);

        // Ensure parent directory exists
        if let Some(parent) = cache_file.parent() {
            fs::create_dir_all(parent)?;
        }

        // Write entry
        let json = serde_json::to_string_pretty(&entry)?;
        fs::write(&cache_file, json)?;

        debug!(
            "Cached {} analysis for: {}",
            params.cache_type.subdirectory(),
            params.file_path
        );
        Ok(())
    }

    /// Clear all cache entries of a specific type
    pub fn clear_type(&self, cache_type: CacheType) -> anyhow::Result<usize> {
        if !self.enabled {
            return Ok(0);
        }

        let cache_dir = self.cache_dir.join("cache").join(cache_type.subdirectory());
        if !cache_dir.exists() {
            return Ok(0);
        }

        let mut removed = 0;
        for entry in fs::read_dir(&cache_dir)? {
            let entry = entry?;
            if entry.file_type()?.is_file() {
                fs::remove_file(entry.path())?;
                removed += 1;
            }
        }

        info!(
            "Cleared {} cache entries for {}",
            removed,
            cache_type.subdirectory()
        );
        Ok(removed)
    }

    /// Clear all cache entries
    pub fn clear_all(&self) -> anyhow::Result<usize> {
        if !self.enabled {
            return Ok(0);
        }

        let mut total_removed = 0;
        for cache_type in &[
            CacheType::Analysis,
            CacheType::Docs,
            CacheType::Refactor,
            CacheType::Todos,
        ] {
            total_removed += self.clear_type(*cache_type)?;
        }

        info!("Cleared {} total cache entries", total_removed);
        Ok(total_removed)
    }

    /// Get statistics for a cache type
    pub fn stats(&self, cache_type: CacheType) -> anyhow::Result<CacheStats> {
        if !self.enabled {
            return Ok(CacheStats::default());
        }

        let cache_dir = self.cache_dir.join("cache").join(cache_type.subdirectory());
        if !cache_dir.exists() {
            return Ok(CacheStats::default());
        }

        let mut stats = CacheStats {
            cache_type: cache_type.subdirectory().to_string(),
            ..Default::default()
        };

        for entry in fs::read_dir(&cache_dir)? {
            let entry = entry?;
            if entry.file_type()?.is_file() && entry.path().extension().is_some_and(|e| e == "json")
            {
                stats.total_entries += 1;

                // Try to read entry for more stats
                if let Ok(content) = fs::read_to_string(entry.path()) {
                    if let Ok(cache_entry) = serde_json::from_str::<RepoCacheEntry>(&content) {
                        if let Some(tokens) = cache_entry.tokens_used {
                            stats.total_tokens += tokens;
                        }
                        stats.total_file_size += cache_entry.file_size;
                    }
                }
            }
        }

        Ok(stats)
    }

    /// Get combined statistics for all cache types
    pub fn all_stats(&self) -> anyhow::Result<Vec<CacheStats>> {
        let mut all_stats = Vec::new();
        for cache_type in &[
            CacheType::Analysis,
            CacheType::Docs,
            CacheType::Refactor,
            CacheType::Todos,
        ] {
            all_stats.push(self.stats(*cache_type)?);
        }
        Ok(all_stats)
    }

    /// Print cache summary
    pub fn print_summary(&self) -> anyhow::Result<()> {
        println!("\n📦 Repository Cache Summary");
        println!("  Location: {}", self.cache_dir.display());
        println!();

        let all_stats = self.all_stats()?;
        let total_entries: usize = all_stats.iter().map(|s| s.total_entries).sum();
        let total_tokens: usize = all_stats.iter().map(|s| s.total_tokens).sum();

        if total_entries == 0 {
            println!("  No cache entries found");
            return Ok(());
        }

        for stats in all_stats {
            if stats.total_entries > 0 {
                println!("  {} cache:", stats.cache_type);
                println!("    Entries: {}", stats.total_entries);
                println!("    Tokens: {}", stats.total_tokens);
                println!("    Total file size: {} bytes", stats.total_file_size);
            }
        }

        println!();
        println!("  Total entries: {}", total_entries);
        println!("  Total tokens: {}", total_tokens);

        Ok(())
    }

    /// Print detailed summary with cost estimates and budget tracking
    pub fn print_detailed_summary(
        &self,
        budget_config: Option<&crate::token_budget::BudgetConfig>,
    ) -> anyhow::Result<()> {
        use crate::token_budget::TokenPricing;

        println!("\n📦 Repository Cache Summary");
        println!("  Location: {}", self.cache_dir.display());
        println!();

        let all_stats = self.all_stats()?;
        let total_entries: usize = all_stats.iter().map(|s| s.total_entries).sum();
        let total_tokens: usize = all_stats.iter().map(|s| s.total_tokens).sum();

        if total_entries == 0 {
            println!("  No cache entries found");
            return Ok(());
        }

        // Estimate provider/model distribution (we'll use default Grok pricing)
        // In the future, we could read actual provider/model from cache entries
        let pricing = TokenPricing::grok();
        let estimated_cost = pricing.estimate_cost(total_tokens);

        for stats in all_stats {
            if stats.total_entries > 0 {
                println!("  {} cache:", stats.cache_type);
                println!("    Entries: {}", stats.total_entries);
                println!("    Tokens: {}", stats.total_tokens);

                let cache_cost = pricing.estimate_cost(stats.total_tokens);
                println!("    Estimated cost: ${:.4}", cache_cost);
                println!("    Total file size: {} bytes", stats.total_file_size);
            }
        }

        println!();
        println!("  Total entries: {}", total_entries);
        println!("  Total tokens: {}", total_tokens);
        println!("  Total estimated cost: ${:.4}", estimated_cost);

        // Budget tracking
        if let Some(budget) = budget_config {
            println!();
            println!("💰 Budget Status:");
            let status = budget.check_spending(estimated_cost);
            println!("  {} {}", status.emoji(), status.message());
            println!("  Remaining: ${:.2}", budget.remaining(estimated_cost));

            let tokens_left = budget.tokens_remaining(estimated_cost, &pricing);
            println!("  Estimated tokens remaining: ~{}", tokens_left);
        }

        Ok(())
    }
}

/// Cache statistics
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CacheStats {
    /// Cache type
    pub cache_type: String,
    /// Number of entries
    pub total_entries: usize,
    /// Total tokens used
    pub total_tokens: usize,
    /// Total file size cached
    pub total_file_size: usize,
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_repo_cache_creation() {
        let temp = TempDir::new().unwrap();
        // Test with local strategy since centralized would go to home dir
        let cache = RepoCache::new_with_strategy(temp.path(), CacheStrategy::Local).unwrap();
        assert!(cache.is_enabled());
        assert!(cache.cache_dir().exists());
        assert!(cache.cache_dir().join("README.md").exists());
    }

    #[test]
    fn test_cache_structure() {
        let temp = TempDir::new().unwrap();
        let cache = RepoCache::new_with_strategy(temp.path(), CacheStrategy::Local).unwrap();

        // Check all subdirectories exist
        for cache_type in &[
            CacheType::Analysis,
            CacheType::Docs,
            CacheType::Refactor,
            CacheType::Todos,
        ] {
            let subdir = cache
                .cache_dir()
                .join("cache")
                .join(cache_type.subdirectory());
            assert!(subdir.exists(), "Missing: {}", subdir.display());
        }
    }

    #[test]
    fn test_cache_get_set() {
        let temp = TempDir::new().unwrap();
        let cache = RepoCache::new_with_strategy(temp.path(), CacheStrategy::Local).unwrap();

        let file_path = "src/main.rs";
        let content = "fn main() {}";
        let result = serde_json::json!({"score": 95});

        // Should be a miss initially
        assert!(cache
            .get(CacheType::Refactor, file_path, content)
            .unwrap()
            .is_none());

        // Store entry
        cache
            .set(CacheSetParams {
                cache_type: CacheType::Refactor,
                file_path,
                content,
                provider: "xai",
                model: "grok-beta",
                result: result.clone(),
                tokens_used: Some(100),
                prompt_hash: None,
                schema_version: None,
            })
            .unwrap();

        // Should be a hit now
        let cached = cache
            .get(CacheType::Refactor, file_path, content)
            .unwrap()
            .unwrap();
        assert_eq!(cached.result, result);
        assert_eq!(cached.tokens_used, Some(100));
        assert_eq!(cached.provider, "xai");
    }

    #[test]
    fn test_cache_invalidation() {
        let temp = TempDir::new().unwrap();
        let cache = RepoCache::new_with_strategy(temp.path(), CacheStrategy::Local).unwrap();

        let file_path = "src/main.rs";
        let content1 = "fn main() {}";
        let content2 = "fn main() { println!(\"changed\"); }";
        let result = serde_json::json!({"score": 95});

        // Store with content1
        cache
            .set(CacheSetParams {
                cache_type: CacheType::Refactor,
                file_path,
                content: content1,
                provider: "xai",
                model: "grok-beta",
                result: result.clone(),
                tokens_used: Some(100),
                prompt_hash: None,
                schema_version: None,
            })
            .unwrap();

        // Should hit with same content
        assert!(cache
            .get(CacheType::Refactor, file_path, content1)
            .unwrap()
            .is_some());

        // Should miss with different content (stale)
        assert!(cache
            .get(CacheType::Refactor, file_path, content2)
            .unwrap()
            .is_none());
    }

    #[test]
    fn test_clear_cache() {
        let temp = TempDir::new().unwrap();
        let cache = RepoCache::new_with_strategy(temp.path(), CacheStrategy::Local).unwrap();

        // Add some entries
        cache
            .set(CacheSetParams {
                cache_type: CacheType::Refactor,
                file_path: "src/main.rs",
                content: "fn main() {}",
                provider: "xai",
                model: "grok-beta",
                result: serde_json::json!({"score": 95}),
                tokens_used: Some(100),
                prompt_hash: None,
                schema_version: None,
            })
            .unwrap();

        cache
            .set(CacheSetParams {
                cache_type: CacheType::Docs,
                file_path: "src/lib.rs",
                content: "pub fn test() {}",
                provider: "xai",
                model: "grok-beta",
                result: serde_json::json!({"docs": "test"}),
                tokens_used: Some(50),
                prompt_hash: None,
                schema_version: None,
            })
            .unwrap();

        // Clear refactor cache
        let removed = cache.clear_type(CacheType::Refactor).unwrap();
        assert_eq!(removed, 1);

        // Clear all
        let removed = cache.clear_all().unwrap();
        assert_eq!(removed, 1); // Only docs left
    }

    #[test]
    fn test_cache_stats() {
        let temp = TempDir::new().unwrap();
        let cache = RepoCache::new_with_strategy(temp.path(), CacheStrategy::Local).unwrap();

        // Initially empty
        let stats = cache.stats(CacheType::Refactor).unwrap();
        assert_eq!(stats.total_entries, 0);

        // Add entry
        cache
            .set(CacheSetParams {
                cache_type: CacheType::Refactor,
                file_path: "src/main.rs",
                content: "fn main() {}",
                provider: "xai",
                model: "grok-beta",
                result: serde_json::json!({"score": 95}),
                tokens_used: Some(100),
                prompt_hash: None,
                schema_version: None,
            })
            .unwrap();

        // Check stats
        let stats = cache.stats(CacheType::Refactor).unwrap();
        assert_eq!(stats.total_entries, 1);
        assert_eq!(stats.total_tokens, 100);
    }
}
