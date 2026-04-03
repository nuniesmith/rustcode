//! Cache module for storing and retrieving LLM audit results
//!
//! This module provides intelligent caching of LLM analysis results to:
//! - Avoid re-analyzing unchanged files
//! - Track file changes via content hashing
//! - Store analysis results with timestamps
//! - Enable incremental audits
//! - Track API usage and costs

use crate::error::{AuditError, Result};
use serde::{Deserialize, Serialize};
use std::cell::RefCell;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use tracing::{debug, info, warn};

/// Directory name for audit cache
pub const CACHE_DIR: &str = ".audit-cache";

/// Cache entry for a single file's LLM analysis
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheEntry {
    /// File path (relative to project root)
    pub file_path: String,

    /// SHA-256 hash of file content when analyzed
    pub content_hash: String,

    /// Timestamp when analysis was performed
    pub analyzed_at: String,

    /// LLM provider used
    pub provider: String,

    /// Model used
    pub model: String,

    /// Analysis result (JSON)
    pub analysis: serde_json::Value,

    /// Token count (if available)
    pub tokens_used: Option<usize>,

    /// File size in bytes
    pub file_size: usize,
}

/// Statistics about cache usage
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheStats {
    /// Total files in cache
    pub total_entries: usize,

    /// Total API calls saved by cache hits
    pub cache_hits: usize,

    /// Total API calls made (cache misses)
    pub cache_misses: usize,

    /// Total tokens used (lifetime)
    pub total_tokens: usize,

    /// Last updated timestamp
    pub last_updated: String,

    /// Estimated cost savings (in USD)
    pub estimated_savings: f64,

    /// Total files analyzed (lifetime)
    pub total_files_analyzed: usize,
}

impl Default for CacheStats {
    fn default() -> Self {
        Self {
            total_entries: 0,
            cache_hits: 0,
            cache_misses: 0,
            total_tokens: 0,
            last_updated: chrono::Utc::now().to_rfc3339(),
            estimated_savings: 0.0,
            total_files_analyzed: 0,
        }
    }
}

/// Cache manager for LLM audit results
pub struct AuditCache {
    /// Cache directory path
    cache_dir: PathBuf,

    /// In-memory cache of entries (using RefCell for interior mutability)
    entries: RefCell<HashMap<String, CacheEntry>>,

    /// Cache statistics (using RefCell for interior mutability)
    stats: RefCell<CacheStats>,

    /// Whether cache is enabled
    enabled: bool,
}

impl AuditCache {
    /// Create a new cache manager with config
    pub fn new(project_root: &Path, config: &crate::llm_config::CacheConfig) -> Result<Self> {
        let cache_dir = project_root.join(CACHE_DIR);

        // Create cache directory if it doesn't exist
        if !cache_dir.exists() {
            fs::create_dir_all(&cache_dir)
                .map_err(|e| AuditError::other(format!("Failed to create cache dir: {}", e)))?;
            info!("Created audit cache directory: {}", cache_dir.display());
        }

        let mut cache = Self {
            cache_dir,
            entries: RefCell::new(HashMap::new()),
            stats: RefCell::new(CacheStats::default()),
            enabled: config.enabled,
        };

        // Load existing entries and stats
        cache.load()?;

        Ok(cache)
    }

    /// Create a disabled cache (no-op)
    pub fn disabled() -> Self {
        Self {
            cache_dir: PathBuf::from("."),
            entries: RefCell::new(HashMap::new()),
            stats: RefCell::new(CacheStats::default()),
            enabled: false,
        }
    }

    /// Check if cache is enabled
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Get cache directory path
    pub fn cache_dir(&self) -> &Path {
        &self.cache_dir
    }

    /// Load cache from disk
    fn load(&mut self) -> Result<()> {
        if !self.enabled {
            return Ok(());
        }

        // Load stats
        let stats_file = self.cache_dir.join("stats.json");
        if stats_file.exists() {
            let content = fs::read_to_string(&stats_file)
                .map_err(|e| AuditError::other(format!("Failed to read stats: {}", e)))?;
            let loaded_stats = serde_json::from_str(&content).unwrap_or_else(|e| {
                warn!("Failed to parse cache stats: {}", e);
                CacheStats::default()
            });
            debug!("Loaded cache stats: {} entries", loaded_stats.total_entries);
            *self.stats.borrow_mut() = loaded_stats;
        }

        // Load entries
        let entries_file = self.cache_dir.join("entries.json");
        if entries_file.exists() {
            let content = fs::read_to_string(&entries_file)
                .map_err(|e| AuditError::other(format!("Failed to read entries: {}", e)))?;
            let loaded_entries = serde_json::from_str(&content).unwrap_or_else(|e| {
                warn!("Failed to parse cache entries: {}", e);
                HashMap::new()
            });
            debug!("Loaded {} cache entries", loaded_entries.len());
            *self.entries.borrow_mut() = loaded_entries;
        }

        Ok(())
    }

    /// Save cache to disk
    pub fn save(&self) -> Result<()> {
        if !self.enabled {
            return Ok(());
        }

        // Save stats
        let stats_file = self.cache_dir.join("stats.json");
        let stats_json = serde_json::to_string_pretty(&*self.stats.borrow())
            .map_err(|e| AuditError::other(format!("Failed to serialize stats: {}", e)))?;
        fs::write(&stats_file, stats_json)
            .map_err(|e| AuditError::other(format!("Failed to write stats: {}", e)))?;

        // Save entries
        let entries_file = self.cache_dir.join("entries.json");
        let entries_json = serde_json::to_string_pretty(&*self.entries.borrow())
            .map_err(|e| AuditError::other(format!("Failed to serialize entries: {}", e)))?;
        fs::write(&entries_file, entries_json)
            .map_err(|e| AuditError::other(format!("Failed to write entries: {}", e)))?;

        debug!("Saved cache: {} entries", self.entries.borrow().len());
        Ok(())
    }

    /// Calculate SHA-256 hash of file content
    pub fn hash_content(&self, content: &str) -> String {
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        hasher.update(content.as_bytes());
        format!("{:x}", hasher.finalize())
    }

    /// Get cache entry for a file (takes string key and content for hash check)
    pub fn get(&self, cache_key: &str, content: &str) -> Result<Option<CacheEntry>> {
        if !self.enabled {
            return Ok(None);
        }

        let content_hash = self.hash_content(content);

        if let Some(entry) = self.entries.borrow().get(cache_key) {
            // Check if content has changed
            if entry.content_hash == content_hash {
                debug!("Cache HIT: {}", cache_key);
                return Ok(Some(entry.clone()));
            } else {
                debug!("Cache STALE (content changed): {}", cache_key);
            }
        }

        debug!("Cache MISS: {}", cache_key);
        Ok(None)
    }

    /// Store a new cache entry (simplified API taking CacheEntry)
    pub fn set(&self, cache_key: String, entry: CacheEntry) -> Result<()> {
        if !self.enabled {
            return Ok(());
        }

        // Update stats
        if let Some(tokens) = entry.tokens_used {
            let mut stats = self.stats.borrow_mut();
            stats.total_tokens += tokens;
            // Rough estimate: $0.01 per 1000 tokens (adjust per provider)
            stats.estimated_savings += (stats.cache_hits as f64) * 0.01;
            stats.total_files_analyzed += 1;
            stats.last_updated = chrono::Utc::now().to_rfc3339();
        }

        // Store entry
        self.entries.borrow_mut().insert(cache_key.clone(), entry);
        self.stats.borrow_mut().total_entries = self.entries.borrow().len();

        debug!("Cached analysis for: {}", cache_key);
        Ok(())
    }

    /// Get cache statistics (clone to avoid borrowing issues)
    pub fn stats(&self) -> CacheStats {
        self.stats.borrow().clone()
    }

    /// Get count of cached entries
    pub fn entry_count(&self) -> usize {
        self.entries.borrow().len()
    }

    /// Clear all cache entries
    pub fn clear(&self) -> Result<()> {
        if !self.enabled {
            return Ok(());
        }

        self.entries.borrow_mut().clear();
        *self.stats.borrow_mut() = CacheStats::default();
        self.save()?;

        info!("Cache cleared");
        Ok(())
    }

    /// Prune stale entries (files that no longer exist)
    pub fn prune(&self, project_root: &Path) -> Result<usize> {
        if !self.enabled {
            return Ok(0);
        }

        let mut removed = 0;
        let mut to_remove = Vec::new();

        for (path_key, _entry) in self.entries.borrow().iter() {
            let full_path = project_root.join(path_key);
            if !full_path.exists() {
                to_remove.push(path_key.clone());
                removed += 1;
            }
        }

        for key in to_remove {
            self.entries.borrow_mut().remove(&key);
        }

        if removed > 0 {
            self.stats.borrow_mut().total_entries = self.entries.borrow().len();
            self.save()?;
            info!("Pruned {} stale cache entries", removed);
        }

        Ok(removed)
    }

    /// Get cache hit rate as percentage
    pub fn hit_rate(&self) -> f64 {
        let stats = self.stats.borrow();
        let total = stats.cache_hits + stats.cache_misses;
        if total == 0 {
            0.0
        } else {
            (stats.cache_hits as f64 / total as f64) * 100.0
        }
    }

    /// Print cache summary
    pub fn print_summary(&self) {
        let stats = self.stats.borrow();
        println!("\n📦 Audit Cache Summary");
        println!("  Entries: {}", stats.total_entries);
        println!("  Cache Hits: {}", stats.cache_hits);
        println!("  Cache Misses: {}", stats.cache_misses);
        println!("  Hit Rate: {:.1}%", self.hit_rate());
        println!("  Total Tokens Used: {}", stats.total_tokens);
        println!("  Estimated Savings: ${:.2}", stats.estimated_savings);
        println!(
            "  Files Analyzed (lifetime): {}",
            stats.total_files_analyzed
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_cache_creation() {
        let temp = TempDir::new().unwrap();
        let config = crate::llm_config::CacheConfig::default();
        let cache = AuditCache::new(temp.path(), &config).unwrap();
        assert!(cache.is_enabled());
        assert_eq!(cache.entry_count(), 0);
    }

    #[test]
    fn test_disabled_cache() {
        let cache = AuditCache::disabled();
        assert!(!cache.is_enabled());
    }

    #[test]
    fn test_hash_content() {
        let temp = TempDir::new().unwrap();
        let config = crate::llm_config::CacheConfig::default();
        let cache = AuditCache::new(temp.path(), &config).unwrap();

        let content1 = "fn main() {}";
        let content2 = "fn main() {}";
        let content3 = "fn main() { }"; // Different

        let hash1 = cache.hash_content(content1);
        let hash2 = cache.hash_content(content2);
        let hash3 = cache.hash_content(content3);

        assert_eq!(hash1, hash2);
        assert_ne!(hash1, hash3);
    }

    #[test]
    fn test_cache_get_set() {
        let temp = TempDir::new().unwrap();
        let config = crate::llm_config::CacheConfig::default();
        let cache = AuditCache::new(temp.path(), &config).unwrap();

        let file_path = Path::new("test.rs");
        let content = "fn test() {}";
        let analysis = serde_json::json!({"score": 85});
        let cache_key = file_path.to_string_lossy().to_string();

        // Should be a miss initially
        assert!(cache.get(&cache_key, content).unwrap().is_none());

        // Store entry
        let entry = CacheEntry {
            file_path: cache_key.clone(),
            content_hash: cache.hash_content(content),
            analyzed_at: chrono::Utc::now().to_rfc3339(),
            provider: "xai".to_string(),
            model: "grok-4".to_string(),
            analysis: analysis.clone(),
            tokens_used: Some(100),
            file_size: content.len(),
        };
        cache.set(cache_key.clone(), entry).unwrap();

        // Should be a hit now
        let cached_entry = cache.get(&cache_key, content).unwrap().unwrap();
        assert_eq!(cached_entry.analysis, analysis);
        assert_eq!(cached_entry.tokens_used, Some(100));
    }

    #[test]
    fn test_cache_invalidation() {
        let temp = TempDir::new().unwrap();
        let config = crate::llm_config::CacheConfig::default();
        let cache = AuditCache::new(temp.path(), &config).unwrap();

        let file_path = Path::new("test.rs");
        let content1 = "fn test() {}";
        let content2 = "fn test() { println!(\"changed\"); }";
        let analysis = serde_json::json!({"score": 85});
        let cache_key = file_path.to_string_lossy().to_string();

        // Store with content1
        let entry = CacheEntry {
            file_path: cache_key.clone(),
            content_hash: cache.hash_content(content1),
            analyzed_at: chrono::Utc::now().to_rfc3339(),
            provider: "xai".to_string(),
            model: "grok-4".to_string(),
            analysis,
            tokens_used: Some(100),
            file_size: content1.len(),
        };
        cache.set(cache_key.clone(), entry).unwrap();

        // Should hit with same content
        assert!(cache.get(&cache_key, content1).unwrap().is_some());

        // Should miss with different content
        assert!(cache.get(&cache_key, content2).unwrap().is_none());
    }

    #[test]
    fn test_cache_persistence() {
        let temp = TempDir::new().unwrap();
        let config = crate::llm_config::CacheConfig::default();
        let file_path = Path::new("test.rs");
        let content = "fn test() {}";
        let analysis = serde_json::json!({"score": 85});
        let cache_key = file_path.to_string_lossy().to_string();

        // Create cache and add entry
        {
            let cache = AuditCache::new(temp.path(), &config).unwrap();
            let entry = CacheEntry {
                file_path: cache_key.clone(),
                content_hash: cache.hash_content(content),
                analyzed_at: chrono::Utc::now().to_rfc3339(),
                provider: "xai".to_string(),
                model: "grok-4".to_string(),
                analysis: analysis.clone(),
                tokens_used: Some(100),
                file_size: content.len(),
            };
            cache.set(cache_key.clone(), entry).unwrap();
            cache.save().unwrap();
        }

        // Load cache again
        {
            let cache = AuditCache::new(temp.path(), &config).unwrap();
            let entry = cache.get(&cache_key, content).unwrap().unwrap();
            assert_eq!(entry.analysis, analysis);
        }
    }
}
