// # Response Cache Module
//
// Caches LLM API responses to avoid redundant calls and reduce costs.
//
// ## Features
//
// - Content-based caching using SHA-256 hashes
// - TTL-based cache invalidation
// - Postgres storage for persistence (shared `response_cache` table)
// - Cache statistics and metrics
// - Automatic cleanup of expired entries
//
// ## Usage
//
// ```rust,no_run
// use rustcode::response_cache::ResponseCache;
//
// # async fn run(pool: sqlx::PgPool) -> anyhow::Result<()> {
// let cache = ResponseCache::new(pool).await?;
//
// // Check cache before API call
// let prompt = "analyze this code...";
// if let Some(cached) = cache.get(prompt, "file_scoring").await? {
//     println!("Cache hit! Using cached response.");
//     return Ok(());
// }
//
// // Make API call and cache result (example only)
// let response = "API response here";
// cache.set(prompt, "file_scoring", response, None).await?;
// # Ok(())
// # }
// ```
//
// The schema lives in `sql/025_cache_tables.sql` and is applied by the
// standard `sqlx::migrate!("./sql")` run at pool init.

use anyhow::{Context, Result};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::PgPool;

// Default cache TTL in hours (24 hours)
const DEFAULT_TTL_HOURS: i64 = 24;

// Response cache for LLM API calls, backed by the shared Postgres pool.
pub struct ResponseCache {
    pool: PgPool,
}

// Cached response entry
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedResponse {
    pub id: i64,
    pub content_hash: String,
    pub operation: String,
    pub response: String,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub hit_count: i64,
    pub last_accessed: DateTime<Utc>,
}

// Cache statistics
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheStats {
    pub total_entries: i64,
    pub total_hits: i64,
    pub total_size_bytes: i64,
    pub hit_rate: f64,
    pub oldest_entry: Option<DateTime<Utc>>,
    pub newest_entry: Option<DateTime<Utc>>,
}

impl ResponseCache {
    // Create a new response cache backed by the shared Postgres pool.
    //
    // The `response_cache` table is created by the standard migration run
    // (`sql/025_cache_tables.sql`); this constructor does not run DDL.
    pub async fn new(pool: PgPool) -> Result<Self> {
        Ok(Self { pool })
    }

    // Generate content hash from prompt and operation
    fn generate_hash(prompt: &str, operation: &str) -> String {
        let mut hasher = Sha256::new();
        hasher.update(prompt.as_bytes());
        hasher.update(operation.as_bytes());
        format!("{:x}", hasher.finalize())
    }

    // Get cached response if available and not expired
    pub async fn get(&self, prompt: &str, operation: &str) -> Result<Option<String>> {
        let hash = Self::generate_hash(prompt, operation);

        let result = sqlx::query_as::<_, (i64, String, DateTime<Utc>)>(
            r#"
            SELECT id, response, expires_at
            FROM response_cache
            WHERE content_hash = $1
            AND expires_at > NOW()
            "#,
        )
        .bind(&hash)
        .fetch_optional(&self.pool)
        .await
        .context("Failed to query cache")?;

        if let Some((id, response, expires_at)) = result {
            // Update hit count and last accessed
            sqlx::query(
                r#"
                UPDATE response_cache
                SET hit_count = hit_count + 1,
                    last_accessed = NOW()
                WHERE id = $1
                "#,
            )
            .bind(id)
            .execute(&self.pool)
            .await
            .context("Failed to update cache hit count")?;

            // Log cache hit
            tracing::debug!("Cache HIT: {} (expires: {})", operation, expires_at);

            Ok(Some(response))
        } else {
            tracing::debug!("Cache MISS: {}", operation);
            Ok(None)
        }
    }

    // Store response in cache
    pub async fn set(
        &self,
        prompt: &str,
        operation: &str,
        response: &str,
        ttl_hours: Option<i64>,
    ) -> Result<()> {
        let hash = Self::generate_hash(prompt, operation);
        let ttl = ttl_hours.unwrap_or(DEFAULT_TTL_HOURS);
        let expires_at = Utc::now() + Duration::hours(ttl);

        sqlx::query(
            r#"
            INSERT INTO response_cache
            (content_hash, operation, response, expires_at)
            VALUES ($1, $2, $3, $4)
            ON CONFLICT (content_hash) DO UPDATE SET
                operation = EXCLUDED.operation,
                response = EXCLUDED.response,
                expires_at = EXCLUDED.expires_at
            "#,
        )
        .bind(&hash)
        .bind(operation)
        .bind(response)
        .bind(expires_at)
        .execute(&self.pool)
        .await
        .context("Failed to cache response")?;

        tracing::info!("Cached response for {} (TTL: {}h)", operation, ttl);

        Ok(())
    }

    // Clear expired cache entries
    pub async fn clear_expired(&self) -> Result<u64> {
        let result = sqlx::query(
            r#"
            DELETE FROM response_cache
            WHERE expires_at < NOW()
            "#,
        )
        .execute(&self.pool)
        .await
        .context("Failed to clear expired cache entries")?;

        let rows_deleted = result.rows_affected();
        if rows_deleted > 0 {
            tracing::info!("Cleared {} expired cache entries", rows_deleted);
        }

        Ok(rows_deleted)
    }

    // Clear all cache entries
    pub async fn clear_all(&self) -> Result<u64> {
        let result = sqlx::query("DELETE FROM response_cache")
            .execute(&self.pool)
            .await
            .context("Failed to clear cache")?;

        let rows_deleted = result.rows_affected();
        tracing::info!("Cleared all cache entries ({})", rows_deleted);

        Ok(rows_deleted)
    }

    // Clear cache entries for a specific operation
    pub async fn clear_operation(&self, operation: &str) -> Result<u64> {
        let result = sqlx::query(
            r#"
            DELETE FROM response_cache
            WHERE operation = $1
            "#,
        )
        .bind(operation)
        .execute(&self.pool)
        .await
        .context("Failed to clear operation cache")?;

        let rows_deleted = result.rows_affected();
        tracing::info!("Cleared {} cache entries for {}", rows_deleted, operation);

        Ok(rows_deleted)
    }

    // Get cache statistics
    pub async fn get_stats(&self) -> Result<CacheStats> {
        let (total_entries,) = sqlx::query_as::<_, (i64,)>(
            "SELECT COUNT(*) FROM response_cache WHERE expires_at > NOW()",
        )
        .fetch_one(&self.pool)
        .await
        .context("Failed to count cache entries")?;

        let (total_hits,) =
            sqlx::query_as::<_, (i64,)>("SELECT COALESCE(SUM(hit_count), 0) FROM response_cache")
                .fetch_one(&self.pool)
                .await
                .context("Failed to sum hit counts")?;

        let (total_size_bytes,) = sqlx::query_as::<_, (i64,)>(
            "SELECT COALESCE(SUM(OCTET_LENGTH(response)), 0)::BIGINT FROM response_cache",
        )
        .fetch_one(&self.pool)
        .await
        .context("Failed to calculate cache size")?;

        // Calculate hit rate (hits per entry)
        let hit_rate = if total_entries > 0 {
            total_hits as f64 / total_entries as f64
        } else {
            0.0
        };

        let oldest = sqlx::query_as::<_, (DateTime<Utc>,)>(
            "SELECT created_at FROM response_cache ORDER BY created_at ASC LIMIT 1",
        )
        .fetch_optional(&self.pool)
        .await
        .context("Failed to get oldest entry")?
        .map(|(dt,)| dt);

        let newest = sqlx::query_as::<_, (DateTime<Utc>,)>(
            "SELECT created_at FROM response_cache ORDER BY created_at DESC LIMIT 1",
        )
        .fetch_optional(&self.pool)
        .await
        .context("Failed to get newest entry")?
        .map(|(dt,)| dt);

        Ok(CacheStats {
            total_entries,
            total_hits,
            total_size_bytes,
            hit_rate,
            oldest_entry: oldest,
            newest_entry: newest,
        })
    }

    // Get cache entries by operation
    pub async fn get_entries_by_operation(&self, operation: &str) -> Result<Vec<CachedResponse>> {
        let entries = sqlx::query_as::<
            _,
            (
                i64,
                String,
                String,
                String,
                DateTime<Utc>,
                DateTime<Utc>,
                i64,
                DateTime<Utc>,
            ),
        >(
            r#"
            SELECT id, content_hash, operation, response, created_at, expires_at, hit_count, last_accessed
            FROM response_cache
            WHERE operation = $1
            AND expires_at > NOW()
            ORDER BY created_at DESC
            "#,
        )
        .bind(operation)
        .fetch_all(&self.pool)
        .await
        .context("Failed to fetch cache entries")?;

        Ok(entries
            .into_iter()
            .map(
                |(
                    id,
                    content_hash,
                    operation,
                    response,
                    created_at,
                    expires_at,
                    hit_count,
                    last_accessed,
                )| CachedResponse {
                    id,
                    content_hash,
                    operation,
                    response,
                    created_at,
                    expires_at,
                    hit_count,
                    last_accessed,
                },
            )
            .collect())
    }

    // Get most frequently accessed cache entries
    pub async fn get_hot_entries(&self, limit: i64) -> Result<Vec<CachedResponse>> {
        let entries = sqlx::query_as::<
            _,
            (
                i64,
                String,
                String,
                String,
                DateTime<Utc>,
                DateTime<Utc>,
                i64,
                DateTime<Utc>,
            ),
        >(
            r#"
            SELECT id, content_hash, operation, response, created_at, expires_at, hit_count, last_accessed
            FROM response_cache
            WHERE expires_at > NOW()
            ORDER BY hit_count DESC
            LIMIT $1
            "#,
        )
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .context("Failed to fetch hot entries")?;

        Ok(entries
            .into_iter()
            .map(
                |(
                    id,
                    content_hash,
                    operation,
                    response,
                    created_at,
                    expires_at,
                    hit_count,
                    last_accessed,
                )| CachedResponse {
                    id,
                    content_hash,
                    operation,
                    response,
                    created_at,
                    expires_at,
                    hit_count,
                    last_accessed,
                },
            )
            .collect())
    }

    // Estimate cost savings from cache
    pub async fn calculate_savings(&self, cost_per_query: f64) -> Result<f64> {
        let stats = self.get_stats().await?;
        Ok(stats.total_hits as f64 * cost_per_query)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::postgres::PgPoolOptions;

    // Connect to the test Postgres instance. Skipped (returns None) when
    // DATABASE_URL is unset so a plain `cargo test` without a DB is a no-op.
    async fn test_pool() -> Option<PgPool> {
        let url = std::env::var("DATABASE_URL").ok()?;
        let pool = PgPoolOptions::new()
            .max_connections(2)
            .connect(&url)
            .await
            .ok()?;
        sqlx::migrate!("./sql").run(&pool).await.ok()?;
        Some(pool)
    }

    #[tokio::test]
    async fn test_cache_operations() -> Result<()> {
        let Some(pool) = test_pool().await else {
            eprintln!("DATABASE_URL not set; skipping test_cache_operations");
            return Ok(());
        };
        let cache = ResponseCache::new(pool).await?;

        // Unique operation namespace so the shared table stays isolated.
        let op = format!("test_op_{}", uuid::Uuid::new_v4());
        cache.clear_operation(&op).await?;

        // Test cache miss
        let result = cache.get("test prompt", &op).await?;
        assert!(result.is_none());

        // Test cache set
        cache
            .set("test prompt", &op, "test response", Some(1))
            .await?;

        // Test cache hit
        let result = cache.get("test prompt", &op).await?;
        assert_eq!(result, Some("test response".to_string()));

        cache.clear_operation(&op).await?;
        Ok(())
    }

    #[tokio::test]
    async fn test_cache_stats() -> Result<()> {
        let Some(pool) = test_pool().await else {
            eprintln!("DATABASE_URL not set; skipping test_cache_stats");
            return Ok(());
        };
        let cache = ResponseCache::new(pool).await?;

        let op = format!("test_op_{}", uuid::Uuid::new_v4());
        cache.clear_operation(&op).await?;

        cache.set("prompt1", &op, "response1", Some(1)).await?;
        cache.set("prompt2", &op, "response2", Some(1)).await?;

        // Generate some hits
        cache.get("prompt1", &op).await?;
        cache.get("prompt1", &op).await?;

        // Scope assertions to this operation to avoid cross-test interference.
        let entries = cache.get_entries_by_operation(&op).await?;
        assert_eq!(entries.len(), 2);
        let hits: i64 = entries.iter().map(|e| e.hit_count).sum();
        assert_eq!(hits, 2);

        cache.clear_operation(&op).await?;
        Ok(())
    }

    #[test]
    fn test_hash_generation() {
        let hash1 = ResponseCache::generate_hash("test", "op1");
        let hash2 = ResponseCache::generate_hash("test", "op1");
        let hash3 = ResponseCache::generate_hash("test", "op2");

        assert_eq!(hash1, hash2);
        assert_ne!(hash1, hash3);
    }
}
