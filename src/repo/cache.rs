// # Postgres-backed Repository Cache
//
// Provides a robust, queryable cache for LLM analysis results, backed by the
// shared Postgres pool (the `cache_entries` / `cache_stats` tables defined in
// `sql/025_cache_tables.sql`).
//
// ## Features
//
// - Postgres storage with indices for fast queries
// - Compressed JSON storage using zstd (stored as BYTEA)
// - Multi-factor cache keys (file hash + model + prompt + schema)
// - Token usage tracking and cost estimation
// - Advanced queries (by repo, model, prompt, date range)
// - Cache eviction policies (LRU, size-based, cost-aware)
//
// Rows are scoped by `repo_path`. Construct with [`RepoCacheSql::new_for_repo`]
// to scope repo-wide queries (`stats`, `get_all_entries`, `clear_*`) to a single
// repository; [`RepoCacheSql::new`] leaves the cache unscoped (operates across
// every repo in the table).
//
// ## Usage
//
// ```rust,no_run
// use rustcode::RepoCacheSql;
// use rustcode::repo::cache::CacheSetParams;
// use rustcode::CacheType;
//
// # async fn run(pool: sqlx::PgPool) -> anyhow::Result<()> {
// let cache = RepoCacheSql::new_for_repo(pool, "/path/to/repo").await?;
//
// // Check cache
// let content = "fn main() {}";
// if let Some(_entry) = cache
//     .get(CacheType::Refactor, "src/main.rs", content, "xai", "grok-beta", None, None)
//     .await?
// {
//     println!("Cache hit!");
//     return Ok(());
// }
//
// // Store result
// let result = serde_json::json!({"score": 95});
// cache
//     .set(CacheSetParams {
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
//     })
//     .await?;
//
// // Get statistics
// let stats = cache.stats().await?;
// println!("Total tokens: {}", stats.total_tokens);
// # Ok(())
// # }
// ```

use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use std::path::Path;
use tracing::{debug, info};

// Re-export CacheType from the file-based cache (shared enum).
pub use crate::repo::file_cache::CacheType;

// Parameters for setting cache entries
#[derive(Debug)]
pub struct CacheSetParams<'a> {
    pub cache_type: crate::repo::file_cache::CacheType,
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
    pub result_json: String, // Decompressed JSON text
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

// Postgres-backed repository cache.
//
// `repo_scope`, when set, restricts repo-wide queries (`stats`,
// `get_all_entries`, `clear_all`, `clear_type`) to a single `repo_path`. The
// content-addressed `get`/`set` path is global (cache keys already encode the
// file content, model, prompt, and schema).
pub struct RepoCacheSql {
    pub pool: PgPool,
    repo_scope: Option<String>,
}

// Column tuple for a full `cache_entries` row.
type CacheRow = (
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
    DateTime<Utc>,
    DateTime<Utc>,
    i64,
);

impl RepoCacheSql {
    // Compute the cache hash for a repository path.
    //
    // This is the stable 8-character hex hash stored in
    // `repositories.cache_hash`; it does not touch the database.
    pub fn compute_repo_hash(repo_path: impl AsRef<Path>) -> String {
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

    // Canonicalize a repo path into the string stored in `cache_entries.repo_path`.
    fn canonical_repo_path(repo_path: impl AsRef<Path>) -> String {
        let repo_path = repo_path.as_ref();
        repo_path
            .canonicalize()
            .unwrap_or_else(|_| repo_path.to_path_buf())
            .to_string_lossy()
            .to_string()
    }

    // Create an unscoped cache over the shared Postgres pool.
    //
    // Repo-wide queries operate across every repository in the table. Use
    // [`Self::new_for_repo`] to scope them to a single repository.
    pub async fn new(pool: PgPool) -> Result<Self> {
        Ok(Self {
            pool,
            repo_scope: None,
        })
    }

    // Create a cache scoped to a single repository.
    //
    // Repo-wide queries (`stats`, `get_all_entries`, `clear_*`) are restricted
    // to this repository's `repo_path`.
    pub async fn new_for_repo(pool: PgPool, repo_path: impl AsRef<Path>) -> Result<Self> {
        let scope = Self::canonical_repo_path(repo_path);
        info!("Using Postgres cache scoped to repo {}", scope);
        Ok(Self {
            pool,
            repo_scope: Some(scope),
        })
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
        cache_type: crate::repo::file_cache::CacheType,
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
                SET last_accessed = NOW(), access_count = access_count + 1
                WHERE cache_key = $1
                "#,
            )
            .bind(&cache_key)
            .execute(&self.pool)
            .await?;

            // Update hit count
            sqlx::query(
                r#"
                UPDATE cache_stats SET cache_hits = cache_hits + 1, last_updated = NOW()
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
                UPDATE cache_stats SET cache_misses = cache_misses + 1, last_updated = NOW()
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

    // Clear all entries of a specific type (within the repo scope, if set)
    pub async fn clear_type(&self, cache_type: crate::repo::file_cache::CacheType) -> Result<u64> {
        let result = if let Some(scope) = &self.repo_scope {
            sqlx::query(
                r#"
                DELETE FROM cache_entries WHERE cache_type = $1 AND repo_path = $2
                "#,
            )
            .bind(cache_type.subdirectory())
            .bind(scope)
            .execute(&self.pool)
            .await?
        } else {
            sqlx::query(
                r#"
                DELETE FROM cache_entries WHERE cache_type = $1
                "#,
            )
            .bind(cache_type.subdirectory())
            .execute(&self.pool)
            .await?
        };

        Ok(result.rows_affected())
    }

    // Clear all cache entries (within the repo scope, if set)
    pub async fn clear_all(&self) -> Result<u64> {
        let result = if let Some(scope) = &self.repo_scope {
            sqlx::query("DELETE FROM cache_entries WHERE repo_path = $1")
                .bind(scope)
                .execute(&self.pool)
                .await?
        } else {
            let r = sqlx::query("DELETE FROM cache_entries")
                .execute(&self.pool)
                .await?;
            // Reset global hit/miss counters only on a full clear.
            sqlx::query(
                r#"
                UPDATE cache_stats SET cache_hits = 0, cache_misses = 0, last_updated = NOW()
                WHERE id = 1
                "#,
            )
            .execute(&self.pool)
            .await?;
            r
        };

        Ok(result.rows_affected())
    }

    // Get cache statistics (within the repo scope, if set)
    pub async fn stats(&self) -> Result<CacheStats> {
        use crate::llm::usage::budget::TokenPricing;

        // Overall stats. SUM(bigint) returns NUMERIC in Postgres, so cast back
        // to BIGINT; an empty table yields NULL, hence the Option wrappers.
        let (total_entries, total_tokens, total_file_size, total_result_size) =
            if let Some(scope) = &self.repo_scope {
                sqlx::query_as::<_, (i64, Option<i64>, Option<i64>, Option<i64>)>(
                    r#"
                SELECT
                    COUNT(*),
                    SUM(tokens_used)::BIGINT,
                    SUM(file_size)::BIGINT,
                    SUM(OCTET_LENGTH(result_blob))::BIGINT
                FROM cache_entries
                WHERE repo_path = $1
                "#,
                )
                .bind(scope)
                .fetch_one(&self.pool)
                .await?
            } else {
                sqlx::query_as::<_, (i64, Option<i64>, Option<i64>, Option<i64>)>(
                    r#"
                SELECT
                    COUNT(*),
                    SUM(tokens_used)::BIGINT,
                    SUM(file_size)::BIGINT,
                    SUM(OCTET_LENGTH(result_blob))::BIGINT
                FROM cache_entries
                "#,
                )
                .fetch_one(&self.pool)
                .await?
            };

        let total_tokens = total_tokens.unwrap_or(0);
        let total_file_size = total_file_size.unwrap_or(0);
        let total_result_size = total_result_size.unwrap_or(0);

        // Hit/miss stats (global counter)
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
        let by_type_rows = if let Some(scope) = &self.repo_scope {
            sqlx::query_as::<_, (String, i64, Option<i64>)>(
                r#"
                SELECT cache_type, COUNT(*), SUM(tokens_used)::BIGINT
                FROM cache_entries
                WHERE repo_path = $1
                GROUP BY cache_type
                "#,
            )
            .bind(scope)
            .fetch_all(&self.pool)
            .await?
        } else {
            sqlx::query_as::<_, (String, i64, Option<i64>)>(
                r#"
                SELECT cache_type, COUNT(*), SUM(tokens_used)::BIGINT
                FROM cache_entries
                GROUP BY cache_type
                "#,
            )
            .fetch_all(&self.pool)
            .await?
        };

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
        let by_model_rows = if let Some(scope) = &self.repo_scope {
            sqlx::query_as::<_, (String, i64, Option<i64>)>(
                r#"
                SELECT model, COUNT(*), SUM(tokens_used)::BIGINT
                FROM cache_entries
                WHERE repo_path = $1
                GROUP BY model
                "#,
            )
            .bind(scope)
            .fetch_all(&self.pool)
            .await?
        } else {
            sqlx::query_as::<_, (String, i64, Option<i64>)>(
                r#"
                SELECT model, COUNT(*), SUM(tokens_used)::BIGINT
                FROM cache_entries
                GROUP BY model
                "#,
            )
            .fetch_all(&self.pool)
            .await?
        };

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

    // Evict entries based on policy until target size is reached.
    //
    // Operates within the repo scope, if set. `target_size` is the desired
    // total compressed result size in bytes.
    pub async fn evict(&self, policy: EvictionPolicy, target_size: i64) -> Result<u64> {
        let (current_size,): (Option<i64>,) = if let Some(scope) = &self.repo_scope {
            sqlx::query_as(
                "SELECT SUM(OCTET_LENGTH(result_blob))::BIGINT FROM cache_entries WHERE repo_path = $1",
            )
            .bind(scope)
            .fetch_one(&self.pool)
            .await?
        } else {
            sqlx::query_as("SELECT SUM(OCTET_LENGTH(result_blob))::BIGINT FROM cache_entries")
                .fetch_one(&self.pool)
                .await?
        };

        let current_size = current_size.unwrap_or(0);
        if current_size <= target_size {
            return Ok(0);
        }

        let order_clause = match policy {
            EvictionPolicy::LRU => "ORDER BY last_accessed ASC",
            EvictionPolicy::OldestFirst => "ORDER BY created_at ASC",
            EvictionPolicy::LargestFirst => "ORDER BY OCTET_LENGTH(result_blob) DESC",
            EvictionPolicy::MostExpensive => "ORDER BY tokens_used DESC NULLS LAST",
        };

        // Get (id, size) for eviction candidates, scoped if needed.
        let where_scope = if self.repo_scope.is_some() {
            "WHERE repo_path = $1"
        } else {
            ""
        };
        let query = format!(
            r#"
            SELECT id, OCTET_LENGTH(result_blob)::BIGINT
            FROM cache_entries
            {where_scope}
            {order_clause}
            "#
        );

        let candidates: Vec<(i64, i64)> = if let Some(scope) = &self.repo_scope {
            sqlx::query_as(&query)
                .bind(scope)
                .fetch_all(&self.pool)
                .await?
        } else {
            sqlx::query_as(&query).fetch_all(&self.pool).await?
        };

        let mut deleted = 0;
        let mut size_freed = 0;

        for (id, size) in candidates {
            sqlx::query("DELETE FROM cache_entries WHERE id = $1")
                .bind(id)
                .execute(&self.pool)
                .await?;

            size_freed += size;
            deleted += 1;

            if current_size - size_freed <= target_size {
                break;
            }
        }

        info!("Evicted {} entries, freed {} bytes", deleted, size_freed);
        Ok(deleted)
    }

    // Map a full row tuple into a `CacheEntry`.
    fn row_to_entry(row: CacheRow) -> CacheEntry {
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
            created_at: row.13,
            last_accessed: row.14,
            access_count: row.15,
        }
    }

    // Get entries for a specific repository
    pub async fn entries_for_repo(&self, repo_path: &str) -> Result<Vec<CacheEntry>> {
        let rows = sqlx::query_as::<_, CacheRow>(
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

        Ok(rows.into_iter().map(Self::row_to_entry).collect())
    }

    // Get all cache entries (within the repo scope, if set) for project-wide
    // review. Returns entries ordered by file_path for deterministic iteration.
    pub async fn get_all_entries(&self) -> Result<Vec<CacheEntry>> {
        let rows = if let Some(scope) = &self.repo_scope {
            sqlx::query_as::<_, CacheRow>(
                r#"
                SELECT
                    id, cache_type, repo_path, file_path, file_hash, cache_key,
                    provider, model, prompt_hash, schema_version, result_blob,
                    tokens_used, file_size, created_at, last_accessed, access_count
                FROM cache_entries
                WHERE repo_path = $1
                ORDER BY file_path ASC
                "#,
            )
            .bind(scope)
            .fetch_all(&self.pool)
            .await?
        } else {
            sqlx::query_as::<_, CacheRow>(
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
            .await?
        };

        Ok(rows.into_iter().map(Self::row_to_entry).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::postgres::PgPoolOptions;

    // Connect to the test Postgres instance and scope to a unique synthetic
    // repo path so the shared `cache_entries` table stays isolated per test.
    // Returns None when DATABASE_URL is unset (plain `cargo test` is a no-op).
    async fn test_cache() -> Option<RepoCacheSql> {
        let url = std::env::var("DATABASE_URL").ok()?;
        let pool = PgPoolOptions::new()
            .max_connections(2)
            .connect(&url)
            .await
            .ok()?;
        sqlx::migrate!("./sql").run(&pool).await.ok()?;
        let scope = format!("/test/repo/{}", uuid::Uuid::new_v4());
        let cache = RepoCacheSql {
            pool,
            repo_scope: Some(scope),
        };
        // Start from a clean slate for this synthetic repo.
        cache.clear_all().await.ok()?;
        Some(cache)
    }

    fn set_params<'a>(
        repo_path: &'a str,
        file_path: &'a str,
        content: &'a str,
        result: serde_json::Value,
        tokens: Option<usize>,
    ) -> CacheSetParams<'a> {
        CacheSetParams {
            cache_type: crate::repo::file_cache::CacheType::Refactor,
            repo_path,
            file_path,
            content,
            provider: "xai",
            model: "grok-beta",
            result,
            tokens_used: tokens,
            prompt_hash: None,
            schema_version: None,
        }
    }

    #[tokio::test]
    async fn test_cache_creation() {
        let Some(cache) = test_cache().await else {
            eprintln!("DATABASE_URL not set; skipping test_cache_creation");
            return;
        };
        // A scoped, freshly-cleared cache reports zero entries.
        let stats = cache.stats().await.unwrap();
        assert_eq!(stats.total_entries, 0);
    }

    #[tokio::test]
    async fn test_cache_get_set() {
        let Some(cache) = test_cache().await else {
            eprintln!("DATABASE_URL not set; skipping test_cache_get_set");
            return;
        };
        let repo = cache.repo_scope.clone().unwrap();

        let result = serde_json::json!({"score": 95});
        cache
            .set(set_params(
                &repo,
                "src/main.rs",
                "fn main() {}",
                result.clone(),
                Some(100),
            ))
            .await
            .unwrap();

        let cached = cache
            .get(
                crate::repo::file_cache::CacheType::Refactor,
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
    async fn test_cache_invalidation() {
        let Some(cache) = test_cache().await else {
            eprintln!("DATABASE_URL not set; skipping test_cache_invalidation");
            return;
        };
        let repo = cache.repo_scope.clone().unwrap();

        cache
            .set(set_params(
                &repo,
                "src/main.rs",
                "fn main() {}",
                serde_json::json!({"score": 95}),
                Some(100),
            ))
            .await
            .unwrap();

        // Different content should miss
        let cached = cache
            .get(
                crate::repo::file_cache::CacheType::Refactor,
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
    async fn test_cache_stats() {
        let Some(cache) = test_cache().await else {
            eprintln!("DATABASE_URL not set; skipping test_cache_stats");
            return;
        };
        let repo = cache.repo_scope.clone().unwrap();

        cache
            .set(set_params(
                &repo,
                "src/main.rs",
                "fn main() {}",
                serde_json::json!({"score": 95}),
                Some(100),
            ))
            .await
            .unwrap();

        let stats = cache.stats().await.unwrap();
        assert_eq!(stats.total_entries, 1);
        assert_eq!(stats.total_tokens, 100);
    }

    #[tokio::test]
    async fn test_clear_cache() {
        let Some(cache) = test_cache().await else {
            eprintln!("DATABASE_URL not set; skipping test_clear_cache");
            return;
        };
        let repo = cache.repo_scope.clone().unwrap();

        cache
            .set(set_params(
                &repo,
                "src/main.rs",
                "fn main() {}",
                serde_json::json!({"score": 95}),
                Some(100),
            ))
            .await
            .unwrap();

        let deleted = cache
            .clear_type(crate::repo::file_cache::CacheType::Refactor)
            .await
            .unwrap();
        assert_eq!(deleted, 1);

        let stats = cache.stats().await.unwrap();
        assert_eq!(stats.total_entries, 0);
    }

    #[tokio::test]
    async fn test_eviction() {
        let Some(cache) = test_cache().await else {
            eprintln!("DATABASE_URL not set; skipping test_eviction");
            return;
        };
        let repo = cache.repo_scope.clone().unwrap();

        // Add multiple entries
        for i in 0..10 {
            cache
                .set(set_params(
                    &repo,
                    &format!("src/file{}.rs", i),
                    &format!("fn file{}() {{}}", i),
                    serde_json::json!({"score": i}),
                    Some(100 * i),
                ))
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
