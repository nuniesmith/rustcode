// # SQLite-based Repository Cache
//
// Provides a robust, queryable cache for LLM analysis results using SQLite.
//
// ## Features
//
// - SQLite storage with indices for fast queries
// - Compressed JSON storage using zstd
// - Multi-factor cache keys (file hash + model + prompt + schema)
// - Token usage tracking and cost estimation
// - Advanced queries (by repo, model, prompt, date range)
// - Cache eviction policies (LRU, size-based, cost-aware)
// - Migration from JSON file-based cache
//
// ## Usage
//
// ```rust,no_run
// use rustcode::repo_cache_sql::{RepoCacheSql, CacheSetParams};
// use rustcode::repo_cache::CacheType;
//
// #[tokio::main]
// async fn main() -> anyhow::Result<()> {
//     let cache = RepoCacheSql::new_for_repo("/path/to/repo").await?;
//
//     // Check cache
//     let content = "fn main() {}";
//     if let Some(entry) = cache.get(
//         CacheType::Refactor,
//         "src/main.rs",
//         content,
//         "xai",
//         "grok-beta",
//         None,
//         None
//     ).await? {
//         println!("Cache hit!");
//         return Ok(());
//     }
//
//     // Store result
//     let result = serde_json::json!({"score": 95});
//     cache.set(CacheSetParams {
//         cache_type: CacheType::Refactor,
//         repo_path: "/path/to/repo",
//         file_path: "src/main.rs",
//         content,
//         provider: "xai",
//         model: "grok-beta",
//         result,
//         tokens_used: Some(150),
//         prompt_hash: None,
//         schema_version: None,
//     }).await?;
//
//     // Get statistics
//     let stats = cache.stats().await?;
//     println!("Total tokens: {}", stats.total_tokens);
//
//     Ok(())
// }
// ```

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::SqlitePool;
use std::path::{Path, PathBuf};
use tracing::{debug, info};

// Re-export CacheType from repo_cache
pub use crate::repo_cache::CacheType;

// Parameters for setting cache entries
#[derive(Debug)]
pub struct CacheSetParams<'a> {
    pub cache_type: crate::repo_cache::CacheType,
    pub repo_path: &'a str,
    pub file_path: &'a str,
    pub content: &'a str,
    pub provider: &'a str,
    pub model: &'a str,
    pub result: serde_json::Value,
    pub tokens_used: Option<usize>,
    pub prompt_hash: Option<&'a str>,
    pub schema_version: Option<i32>,
}

// Cache entry stored in database
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheEntry {
    pub id: i64,
    pub cache_type: String,
    pub repo_path: String,
    pub file_path: String,
    pub file_hash: String,
    pub cache_key: String,
    pub provider: String,
    pub model: String,
    pub prompt_hash: String,
    pub schema_version: i32,
    pub result_json: String, // Compressed
    pub tokens_used: Option<i64>,
    pub file_size: i64,
    pub created_at: DateTime<Utc>,
    pub last_accessed: DateTime<Utc>,
    pub access_count: i64,
}

// Cache statistics
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheStats {
    pub total_entries: i64,
    pub total_tokens: i64,
    pub total_file_size: i64,
    pub total_result_size: i64,
    pub estimated_cost: f64,
    pub cache_hits: i64,
    pub cache_misses: i64,
    pub hit_rate: f64,
    pub by_type: Vec<CacheTypeStats>,
    pub by_model: Vec<ModelStats>,
}

// Statistics per cache type
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheTypeStats {
    pub cache_type: String,
    pub entries: i64,
    pub tokens: i64,
    pub cost: f64,
}

// Statistics per model
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelStats {
    pub model: String,
    pub entries: i64,
    pub tokens: i64,
    pub cost: f64,
}

// Eviction policy for cache cleanup
#[derive(Debug, Clone, Copy)]
pub enum EvictionPolicy {
    // Least Recently Used
    LRU,
    // Oldest entries first
    OldestFirst,
    // Largest entries first (by result size)
    LargestFirst,
    // Most expensive to recreate (highest token count)
    MostExpensive,
}

// SQLite-based repository cache
pub struct RepoCacheSql {
    pub pool: SqlitePool,
}

impl RepoCacheSql {
    // Compute the cache hash for a repository path without opening the database.
    // This is the same hash used to locate the cache DB directory.
    // Returns the 8-character hex hash.
    pub fn compute_repo_hash(repo_path: impl AsRef<Path>) -> String {
        use sha2::{Digest, Sha256};

        let repo_path = repo_path.as_ref();

        // Compute stable hash for repo path
        let canonical_path = repo_path
            .canonicalize()
            .unwrap_or_else(|_| repo_path.to_path_buf());
        let path_str = canonical_path.to_string_lossy();

        let mut hasher = Sha256::new();
        hasher.update(path_str.as_bytes());
        let hash = hasher.finalize();
        format!("{:x}", hash)[..8].to_string()
    }

    // Create a new SQLite cache for a repository using path-based hashing
    pub async fn new_for_repo(repo_path: impl AsRef<Path>) -> Result<Self> {
        let repo_path = repo_path.as_ref();
        let repo_hash = Self::compute_repo_hash(repo_path);

        // Get XDG cache directory
        let cache_dir = if let Some(cache_home) = std::env::var_os("XDG_CACHE_HOME") {
            PathBuf::from(cache_home)
        } else if let Some(home) = dirs::home_dir() {
            home.join(".cache")
        } else {
            anyhow::bail!("Cannot determine cache directory");
        };

        let db_path = cache_dir
            .join("rustcode")
            .join("repos")
            .join(&repo_hash)
            .join("cache.db");

        Self::new(&db_path).await
    }

    // Create a new SQLite cache using a precomputed cache hash.
    // This is useful when the web server doesn't have access to the repo path
    // but has the hash stored in the database.
    pub async fn new_with_hash(cache_hash: &str) -> Result<Self> {
        // Get XDG cache directory
        let cache_dir = if let Some(cache_home) = std::env::var_os("XDG_CACHE_HOME") {
            PathBuf::from(cache_home)
        } else if let Some(home) = dirs::home_dir() {
            home.join(".cache")
        } else {
            anyhow::bail!("Cannot determine cache directory");
        };

        let db_path = cache_dir
            .join("rustcode")
            .join("repos")
            .join(cache_hash)
            .join("cache.db");

        Self::new(&db_path).await
    }

    // Create a new SQLite cache
    pub async fn new(database_path: impl AsRef<Path>) -> Result<Self> {
        let path = database_path.as_ref();

        // Ensure parent directory exists
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).with_context(|| {
                format!("Failed to create cache directory: {}", parent.display())
            })?;
        }

        let database_url = format!("sqlite:{}?mode=rwc", path.display());
        let pool = SqlitePool::connect(&database_url)
            .await
            .context("Failed to connect to cache database")?;

        let cache = Self { pool };
        cache.initialize_schema().await?;

        info!("Initialized SQLite cache at {}", path.display());
        Ok(cache)
    }

    // Initialize database schema
    async fn initialize_schema(&self) -> Result<()> {
        // Main cache table
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS cache_entries (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                cache_type TEXT NOT NULL,
                repo_path TEXT NOT NULL,
                file_path TEXT NOT NULL,
                file_hash TEXT NOT NULL,
                cache_key TEXT NOT NULL UNIQUE,
                provider TEXT NOT NULL,
                model TEXT NOT NULL,
                prompt_hash TEXT NOT NULL,
                schema_version INTEGER NOT NULL DEFAULT 1,
                result_blob BLOB NOT NULL,
                tokens_used INTEGER,
                file_size INTEGER NOT NULL,
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                last_accessed TEXT NOT NULL DEFAULT (datetime('now')),
                access_count INTEGER NOT NULL DEFAULT 0
            )
            "#,
        )
        .execute(&self.pool)
        .await?;

        // Indices for fast queries
        sqlx::query(
            r#"
            CREATE INDEX IF NOT EXISTS idx_cache_key ON cache_entries(cache_key);
            CREATE INDEX IF NOT EXISTS idx_cache_type ON cache_entries(cache_type);
            CREATE INDEX IF NOT EXISTS idx_repo_path ON cache_entries(repo_path);
            CREATE INDEX IF NOT EXISTS idx_model ON cache_entries(model);
            CREATE INDEX IF NOT EXISTS idx_created_at ON cache_entries(created_at);
            CREATE INDEX IF NOT EXISTS idx_last_accessed ON cache_entries(last_accessed);
            "#,
        )
        .execute(&self.pool)
        .await?;

        // Cache statistics table
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS cache_stats (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                cache_hits INTEGER NOT NULL DEFAULT 0,
                cache_misses INTEGER NOT NULL DEFAULT 0,
                last_updated TEXT NOT NULL DEFAULT (datetime('now'))
            )
            "#,
        )
        .execute(&self.pool)
        .await?;

        // Initialize stats row if not exists
        sqlx::query(
            r#"
            INSERT INTO cache_stats (id, cache_hits, cache_misses)
            VALUES (1, 0, 0)
            ON CONFLICT (id) DO NOTHING
            "#,
        )
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    // Compute SHA-256 hash of content
    fn hash_content(content: &str) -> String {
        let mut hasher = Sha256::new();
        hasher.update(content.as_bytes());
        format!("{:x}", hasher.finalize())
    }

    // Compute multi-factor cache key
    fn compute_cache_key(
        file_hash: &str,
        model: &str,
        prompt_hash: &str,
        schema_version: i32,
    ) -> String {
        let combined = format!("{}:{}:{}:{}", file_hash, model, prompt_hash, schema_version);
        let mut hasher = Sha256::new();
        hasher.update(combined.as_bytes());
        format!("{:x}", hasher.finalize())
    }

    // Compress JSON data using zstd
    fn compress_json(json: &serde_json::Value) -> Result<Vec<u8>> {
        let json_str = serde_json::to_string(json)?;
        let compressed = zstd::encode_all(json_str.as_bytes(), 3)?;
        Ok(compressed)
    }

    // Decompress JSON data
    fn decompress_json(compressed: &[u8]) -> Result<serde_json::Value> {
        let decompressed = zstd::decode_all(compressed)?;
        let json_str = String::from_utf8(decompressed)?;
        let value = serde_json::from_str(&json_str)?;
        Ok(value)
    }

    // Get cached entry
    #[allow(clippy::too_many_arguments)]
    pub async fn get(
        &self,
        cache_type: crate::repo_cache::CacheType,
        file_path: &str,
        content: &str,
        _provider: &str,
        model: &str,
        prompt_hash: Option<&str>,
        schema_version: Option<i32>,
    ) -> Result<Option<serde_json::Value>> {
        let file_hash = Self::hash_content(content);
        let prompt_hash = prompt_hash
            .map(|s| s.to_string())
            .unwrap_or_else(|| crate::prompt_hashes::get_prompt_hash_for_type(cache_type));
        let schema_version = schema_version.unwrap_or(1);
        let cache_key = Self::compute_cache_key(&file_hash, model, &prompt_hash, schema_version);

        let result = sqlx::query_as::<_, (Vec<u8>,)>(
            r#"
            SELECT result_blob FROM cache_entries WHERE cache_key = $1
            "#,
        )
        .bind(&cache_key)
        .fetch_optional(&self.pool)
        .await?;

        if let Some((blob,)) = result {
            // Update access stats
            sqlx::query(
                r#"
                UPDATE cache_entries
                SET last_accessed = datetime('now'), access_count = access_count + 1
                WHERE cache_key = $1
                "#,
            )
            .bind(&cache_key)
            .execute(&self.pool)
            .await?;

            // Update hit count
            sqlx::query(
                r#"
                UPDATE cache_stats SET cache_hits = cache_hits + 1, last_updated = datetime('now')
                WHERE id = 1
                "#,
            )
            .execute(&self.pool)
            .await?;

            let json = Self::decompress_json(&blob)?;
            debug!("Cache hit for {}", file_path);
            Ok(Some(json))
        } else {
            // Update miss count
            sqlx::query(
                r#"
                UPDATE cache_stats SET cache_misses = cache_misses + 1, last_updated = datetime('now')
                WHERE id = 1
                "#,
            )
            .execute(&self.pool)
            .await?;

            debug!("Cache miss for {}", file_path);
            Ok(None)
        }
    }

    // Set cache entry
    pub async fn set(&self, params: CacheSetParams<'_>) -> Result<()> {
        let file_hash = Self::hash_content(params.content);
        let prompt_hash = params
            .prompt_hash
            .map(|s| s.to_string())
            .unwrap_or_else(|| crate::prompt_hashes::get_prompt_hash_for_type(params.cache_type));
        let schema_version = params.schema_version.unwrap_or(1);
        let cache_key =
            Self::compute_cache_key(&file_hash, params.model, &prompt_hash, schema_version);

        let result_blob = Self::compress_json(&params.result)?;

        sqlx::query(
            r#"
            INSERT INTO cache_entries
            (cache_type, repo_path, file_path, file_hash, cache_key, provider, model,
             prompt_hash, schema_version, result_blob, tokens_used, file_size)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)
            ON CONFLICT (cache_key) DO UPDATE SET
                cache_type = EXCLUDED.cache_type,
                repo_path = EXCLUDED.repo_path,
                file_path = EXCLUDED.file_path,
                file_hash = EXCLUDED.file_hash,
                provider = EXCLUDED.provider,
                model = EXCLUDED.model,
                prompt_hash = EXCLUDED.prompt_hash,
                schema_version = EXCLUDED.schema_version,
                result_blob = EXCLUDED.result_blob,
                tokens_used = EXCLUDED.tokens_used,
                file_size = EXCLUDED.file_size
            "#,
        )
        .bind(params.cache_type.subdirectory())
        .bind(params.repo_path)
        .bind(params.file_path)
        .bind(&file_hash)
        .bind(&cache_key)
        .bind(params.provider)
        .bind(params.model)
        .bind(&prompt_hash)
        .bind(schema_version)
        .bind(&result_blob)
        .bind(params.tokens_used.map(|t| t as i64))
        .bind(params.content.len() as i64)
        .execute(&self.pool)
        .await?;

        debug!(
            "Cached {} result for {}",
            params.cache_type.subdirectory(),
            params.file_path
        );
        Ok(())
    }

    // Set cache entry with pre-computed cache key (for migration)
    #[allow(clippy::too_many_arguments)]
    pub async fn set_with_cache_key(
        &self,
        cache_type: crate::repo_cache::CacheType,
        repo_path: &str,
        file_path: &str,
        file_hash: &str,
        cache_key: &str,
        provider: &str,
        model: &str,
        prompt_hash: &str,
        schema_version: i32,
        result: serde_json::Value,
        tokens_used: Option<usize>,
        file_size: usize,
    ) -> Result<()> {
        let result_blob = Self::compress_json(&result)?;

        sqlx::query(
            r#"
            INSERT INTO cache_entries
            (cache_type, repo_path, file_path, file_hash, cache_key, provider, model,
             prompt_hash, schema_version, result_blob, tokens_used, file_size)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)
            ON CONFLICT (cache_key) DO UPDATE SET
                cache_type = EXCLUDED.cache_type,
                repo_path = EXCLUDED.repo_path,
                file_path = EXCLUDED.file_path,
                file_hash = EXCLUDED.file_hash,
                provider = EXCLUDED.provider,
                model = EXCLUDED.model,
                prompt_hash = EXCLUDED.prompt_hash,
                schema_version = EXCLUDED.schema_version,
                result_blob = EXCLUDED.result_blob,
                tokens_used = EXCLUDED.tokens_used,
                file_size = EXCLUDED.file_size
            "#,
        )
        .bind(cache_type.subdirectory())
        .bind(repo_path)
        .bind(file_path)
        .bind(file_hash)
        .bind(cache_key)
        .bind(provider)
        .bind(model)
        .bind(prompt_hash)
        .bind(schema_version)
        .bind(&result_blob)
        .bind(tokens_used.map(|t| t as i64))
        .bind(file_size as i64)
        .execute(&self.pool)
        .await?;

        debug!(
            "Migrated {} result for {}",
            cache_type.subdirectory(),
            file_path
        );
        Ok(())
    }

    // Clear all entries of a specific type
    pub async fn clear_type(&self, cache_type: crate::repo_cache::CacheType) -> Result<u64> {
        let result = sqlx::query(
            r#"
            DELETE FROM cache_entries WHERE cache_type = $1
            "#,
        )
        .bind(cache_type.subdirectory())
        .execute(&self.pool)
        .await?;

        Ok(result.rows_affected())
    }

    // Clear all cache entries
    pub async fn clear_all(&self) -> Result<u64> {
        let result = sqlx::query("DELETE FROM cache_entries")
            .execute(&self.pool)
            .await?;

        // Reset stats
        sqlx::query(
            r#"
            UPDATE cache_stats SET cache_hits = 0, cache_misses = 0, last_updated = datetime('now')
            WHERE id = 1
            "#,
        )
        .execute(&self.pool)
        .await?;

        Ok(result.rows_affected())
    }

    // Get cache statistics
    pub async fn stats(&self) -> Result<CacheStats> {
        use crate::token_budget::TokenPricing;

        // Overall stats
        let (total_entries, total_tokens, total_file_size, total_result_size) =
            sqlx::query_as::<_, (i64, Option<i64>, i64, i64)>(
                r#"
            SELECT
                COUNT(*),
                SUM(tokens_used),
                SUM(file_size),
                SUM(LENGTH(result_blob))
            FROM cache_entries
            "#,
            )
            .fetch_one(&self.pool)
            .await?;

        let total_tokens = total_tokens.unwrap_or(0);

        // Hit/miss stats
        let (cache_hits, cache_misses) = sqlx::query_as::<_, (i64, i64)>(
            r#"
            SELECT cache_hits, cache_misses FROM cache_stats WHERE id = 1
            "#,
        )
        .fetch_one(&self.pool)
        .await?;

        let hit_rate = if cache_hits + cache_misses > 0 {
            cache_hits as f64 / (cache_hits + cache_misses) as f64
        } else {
            0.0
        };

        // Estimate cost (using Grok pricing as default)
        let pricing = TokenPricing::grok();
        let estimated_cost = pricing.estimate_cost(total_tokens as usize);

        // Stats by type
        let by_type_rows = sqlx::query_as::<_, (String, i64, Option<i64>)>(
            r#"
            SELECT cache_type, COUNT(*), SUM(tokens_used)
            FROM cache_entries
            GROUP BY cache_type
            "#,
        )
        .fetch_all(&self.pool)
        .await?;

        let by_type = by_type_rows
            .into_iter()
            .map(|(cache_type, entries, tokens)| {
                let tokens = tokens.unwrap_or(0);
                let cost = pricing.estimate_cost(tokens as usize);
                CacheTypeStats {
                    cache_type,
                    entries,
                    tokens,
                    cost,
                }
            })
            .collect();

        // Stats by model
        let by_model_rows = sqlx::query_as::<_, (String, i64, Option<i64>)>(
            r#"
            SELECT model, COUNT(*), SUM(tokens_used)
            FROM cache_entries
            GROUP BY model
            "#,
        )
        .fetch_all(&self.pool)
        .await?;

        let by_model = by_model_rows
            .into_iter()
            .map(|(model, entries, tokens)| {
                let tokens = tokens.unwrap_or(0);
                let cost = pricing.estimate_cost(tokens as usize);
                ModelStats {
                    model,
                    entries,
                    tokens,
                    cost,
                }
            })
            .collect();

        Ok(CacheStats {
            total_entries,
            total_tokens,
            total_file_size,
            total_result_size,
            estimated_cost,
            cache_hits,
            cache_misses,
            hit_rate,
            by_type,
            by_model,
        })
    }

    // Evict entries based on policy until target size is reached
    pub async fn evict(&self, policy: EvictionPolicy, target_size: i64) -> Result<u64> {
        let current_size: (i64,) = sqlx::query_as(
            r#"
            SELECT SUM(LENGTH(result_blob)) FROM cache_entries
            "#,
        )
        .fetch_one(&self.pool)
        .await?;

        let current_size = current_size.0;
        if current_size <= target_size {
            return Ok(0);
        }

        let _to_delete = current_size - target_size;
        let order_clause = match policy {
            EvictionPolicy::LRU => "ORDER BY last_accessed ASC",
            EvictionPolicy::OldestFirst => "ORDER BY created_at ASC",
            EvictionPolicy::LargestFirst => "ORDER BY LENGTH(result_blob) DESC",
            EvictionPolicy::MostExpensive => "ORDER BY tokens_used DESC",
        };

        // Get IDs to delete
        let ids: Vec<(i64,)> = sqlx::query_as(&format!(
            r#"
            SELECT id FROM cache_entries
            {}
            LIMIT (SELECT COUNT(*) FROM cache_entries) / 2
            "#,
            order_clause
        ))
        .fetch_all(&self.pool)
        .await?;

        let mut deleted = 0;
        let mut size_freed = 0;

        for (id,) in ids {
            let size: (i64,) = sqlx::query_as(
                r#"
                SELECT LENGTH(result_blob) FROM cache_entries WHERE id = $1
                "#,
            )
            .bind(id)
            .fetch_one(&self.pool)
            .await?;

            sqlx::query("DELETE FROM cache_entries WHERE id = $1")
                .bind(id)
                .execute(&self.pool)
                .await?;

            size_freed += size.0;
            deleted += 1;

            if current_size - size_freed <= target_size {
                break;
            }
        }

        info!("Evicted {} entries, freed {} bytes", deleted, size_freed);
        Ok(deleted)
    }

    // Get entries for a specific repository
    pub async fn entries_for_repo(&self, repo_path: &str) -> Result<Vec<CacheEntry>> {
        let rows = sqlx::query_as::<
            _,
            (
                i64,
                String,
                String,
                String,
                String,
                String,
                String,
                String,
                String,
                i32,
                Vec<u8>,
                Option<i64>,
                i64,
                String,
                String,
                i64,
            ),
        >(
            r#"
            SELECT
                id, cache_type, repo_path, file_path, file_hash, cache_key,
                provider, model, prompt_hash, schema_version, result_blob,
                tokens_used, file_size, created_at, last_accessed, access_count
            FROM cache_entries
            WHERE repo_path = $1
            ORDER BY created_at DESC
            "#,
        )
        .bind(repo_path)
        .fetch_all(&self.pool)
        .await?;

        let entries = rows
            .into_iter()
            .map(|row| {
                let result_json = Self::decompress_json(&row.10)
                    .map(|v| serde_json::to_string(&v).unwrap_or_default())
                    .unwrap_or_default();

                CacheEntry {
                    id: row.0,
                    cache_type: row.1,
                    repo_path: row.2,
                    file_path: row.3,
                    file_hash: row.4,
                    cache_key: row.5,
                    provider: row.6,
                    model: row.7,
                    prompt_hash: row.8,
                    schema_version: row.9,
                    result_json,
                    tokens_used: row.11,
                    file_size: row.12,
                    created_at: row.13.parse().unwrap_or_else(|_| Utc::now()),
                    last_accessed: row.14.parse().unwrap_or_else(|_| Utc::now()),
                    access_count: row.15,
                }
            })
            .collect();

        Ok(entries)
    }

    // Get all cache entries (across all repos) for project-wide review.
    // Returns entries ordered by file_path for deterministic iteration.
    pub async fn get_all_entries(&self) -> Result<Vec<CacheEntry>> {
        let rows = sqlx::query_as::<
            _,
            (
                i64,
                String,
                String,
                String,
                String,
                String,
                String,
                String,
                String,
                i32,
                Vec<u8>,
                Option<i64>,
                i64,
                String,
                String,
                i64,
            ),
        >(
            r#"
            SELECT
                id, cache_type, repo_path, file_path, file_hash, cache_key,
                provider, model, prompt_hash, schema_version, result_blob,
                tokens_used, file_size, created_at, last_accessed, access_count
            FROM cache_entries
            ORDER BY file_path ASC
            "#,
        )
        .fetch_all(&self.pool)
        .await?;

        let entries = rows
            .into_iter()
            .map(|row| {
                let result_json = Self::decompress_json(&row.10)
                    .map(|v| serde_json::to_string(&v).unwrap_or_default())
                    .unwrap_or_default();

                CacheEntry {
                    id: row.0,
                    cache_type: row.1,
                    repo_path: row.2,
                    file_path: row.3,
                    file_hash: row.4,
                    cache_key: row.5,
                    provider: row.6,
                    model: row.7,
                    prompt_hash: row.8,
                    schema_version: row.9,
                    result_json,
                    tokens_used: row.11,
                    file_size: row.12,
                    created_at: row.13.parse().unwrap_or_else(|_| Utc::now()),
                    last_accessed: row.14.parse().unwrap_or_else(|_| Utc::now()),
                    access_count: row.15,
                }
            })
            .collect();

        Ok(entries)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    #[ignore = "RepoCacheSql uses SQLite internally; not available in postgres-only build"]
    async fn test_cache_creation() {
        let cache = RepoCacheSql::new(":memory:").await;
        assert!(cache.is_ok());
    }

    #[tokio::test]
    #[ignore = "RepoCacheSql uses SQLite internally; not available in postgres-only build"]
    async fn test_cache_get_set() {
        let cache = RepoCacheSql::new(":memory:").await.unwrap();

        let result = serde_json::json!({"score": 95});
        cache
            .set(CacheSetParams {
                cache_type: crate::repo_cache::CacheType::Refactor,
                repo_path: "/test/repo",
                file_path: "src/main.rs",
                content: "fn main() {}",
                provider: "xai",
                model: "grok-beta",
                result: result.clone(),
                tokens_used: Some(100),
                prompt_hash: None,
                schema_version: None,
            })
            .await
            .unwrap();

        let cached = cache
            .get(
                crate::repo_cache::CacheType::Refactor,
                "src/main.rs",
                "fn main() {}",
                "xai",
                "grok-beta",
                None,
                None,
            )
            .await
            .unwrap();

        assert!(cached.is_some());
        assert_eq!(cached.unwrap(), result);
    }

    #[tokio::test]
    #[ignore = "RepoCacheSql uses SQLite internally; not available in postgres-only build"]
    async fn test_cache_invalidation() {
        let cache = RepoCacheSql::new(":memory:").await.unwrap();

        let result = serde_json::json!({"score": 95});
        cache
            .set(CacheSetParams {
                cache_type: crate::repo_cache::CacheType::Refactor,
                repo_path: "/test/repo",
                file_path: "src/main.rs",
                content: "fn main() {}",
                provider: "xai",
                model: "grok-beta",
                result: result.clone(),
                tokens_used: Some(100),
                prompt_hash: None,
                schema_version: None,
            })
            .await
            .unwrap();

        // Different content should miss
        let cached = cache
            .get(
                crate::repo_cache::CacheType::Refactor,
                "src/main.rs",
                "fn main() { println!(\"Hello\"); }",
                "xai",
                "grok-beta",
                None,
                None,
            )
            .await
            .unwrap();

        assert!(cached.is_none());
    }

    #[tokio::test]
    #[ignore = "RepoCacheSql uses SQLite internally; not available in postgres-only build"]
    async fn test_cache_stats() {
        let cache = RepoCacheSql::new(":memory:").await.unwrap();

        cache
            .set(CacheSetParams {
                cache_type: crate::repo_cache::CacheType::Refactor,
                repo_path: "/test/repo",
                file_path: "src/main.rs",
                content: "fn main() {}",
                provider: "xai",
                model: "grok-beta",
                result: serde_json::json!({"score": 95}),
                tokens_used: Some(100),
                prompt_hash: None,
                schema_version: None,
            })
            .await
            .unwrap();

        let stats = cache.stats().await.unwrap();
        assert_eq!(stats.total_entries, 1);
        assert_eq!(stats.total_tokens, 100);
    }

    #[tokio::test]
    #[ignore = "RepoCacheSql uses SQLite internally; not available in postgres-only build"]
    async fn test_clear_cache() {
        let cache = RepoCacheSql::new(":memory:").await.unwrap();

        cache
            .set(CacheSetParams {
                cache_type: crate::repo_cache::CacheType::Refactor,
                repo_path: "/test/repo",
                file_path: "src/main.rs",
                content: "fn main() {}",
                provider: "xai",
                model: "grok-beta",
                result: serde_json::json!({"score": 95}),
                tokens_used: Some(100),
                prompt_hash: None,
                schema_version: None,
            })
            .await
            .unwrap();

        let deleted = cache
            .clear_type(crate::repo_cache::CacheType::Refactor)
            .await
            .unwrap();
        assert_eq!(deleted, 1);

        let stats = cache.stats().await.unwrap();
        assert_eq!(stats.total_entries, 0);
    }

    #[tokio::test]
    #[ignore = "RepoCacheSql uses SQLite internally; not available in postgres-only build"]
    async fn test_eviction() {
        let cache = RepoCacheSql::new(":memory:").await.unwrap();

        // Add multiple entries
        for i in 0..10 {
            cache
                .set(CacheSetParams {
                    cache_type: crate::repo_cache::CacheType::Refactor,
                    repo_path: "/test/repo",
                    file_path: &format!("src/file{}.rs", i),
                    content: &format!("fn file{}() {{}}", i),
                    provider: "xai",
                    model: "grok-beta",
                    result: serde_json::json!({"score": i}),
                    tokens_used: Some(100 * i),
                    prompt_hash: None,
                    schema_version: None,
                })
                .await
                .unwrap();
        }

        let stats_before = cache.stats().await.unwrap();
        assert_eq!(stats_before.total_entries, 10);

        // Evict to small size
        let deleted = cache.evict(EvictionPolicy::LRU, 100).await.unwrap();
        assert!(deleted > 0);

        let stats_after = cache.stats().await.unwrap();
        assert!(stats_after.total_entries < stats_before.total_entries);
    }
}
