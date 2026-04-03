//! # Response Cache Module
//!
//! Caches LLM API responses to avoid redundant calls and reduce costs.
//!
//! ## Features
//!
//! - Content-based caching using SHA-256 hashes
//! - TTL-based cache invalidation
//! - SQLite storage for persistence
//! - Cache statistics and metrics
//! - Automatic cleanup of expired entries
//!
//! ## Usage
//!
//! ```rust,no_run
//! use rustcode::response_cache::ResponseCache;
//!
//! #[tokio::main]
//! async fn main() -> anyhow::Result<()> {
//!     let cache = ResponseCache::new("cache.db").await?;
//!
//!     // Check cache before API call
//!     let prompt = "analyze this code...";
//!     if let Some(cached) = cache.get(prompt, "file_scoring").await? {
//!         println!("Cache hit! Using cached response.");
//!         return Ok(());
//!     }
//!
//!     // Make API call and cache result (example only)
//!     let response = "API response here";
//!     cache.set(prompt, "file_scoring", response, None).await?;
//!
//!     Ok(())
//! }
//! ```

use anyhow::{Context, Result};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Default cache TTL in hours (24 hours)
const DEFAULT_TTL_HOURS: i64 = 24;

/// Response cache for LLM API calls
pub struct ResponseCache {
    pool: sqlx::SqlitePool,
}

/// Cached response entry
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

/// Cache statistics
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
    /// Create a new response cache
    pub async fn new(database_path: &str) -> Result<Self> {
        let database_url = format!("sqlite:{}?mode=rwc", database_path);
        let pool = sqlx::SqlitePool::connect(&database_url)
            .await
            .context("Failed to connect to cache database")?;

        let cache = Self { pool };
        cache.initialize_schema().await?;

        Ok(cache)
    }

    /// Initialize the cache schema
    async fn initialize_schema(&self) -> Result<()> {
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS response_cache (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                content_hash TEXT NOT NULL UNIQUE,
                operation TEXT NOT NULL,
                response TEXT NOT NULL,
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                expires_at TEXT NOT NULL,
                hit_count INTEGER NOT NULL DEFAULT 0,
                last_accessed TEXT NOT NULL DEFAULT (datetime('now'))
            )
            "#,
        )
        .execute(&self.pool)
        .await
        .context("Failed to create response_cache table")?;

        // Create indexes
        sqlx::query("CREATE INDEX IF NOT EXISTS idx_cache_hash ON response_cache(content_hash)")
            .execute(&self.pool)
            .await
            .context("Failed to create hash index")?;

        sqlx::query("CREATE INDEX IF NOT EXISTS idx_cache_expires ON response_cache(expires_at)")
            .execute(&self.pool)
            .await
            .context("Failed to create expires index")?;

        sqlx::query("CREATE INDEX IF NOT EXISTS idx_cache_operation ON response_cache(operation)")
            .execute(&self.pool)
            .await
            .context("Failed to create operation index")?;

        Ok(())
    }

    /// Generate content hash from prompt and operation
    fn generate_hash(prompt: &str, operation: &str) -> String {
        let mut hasher = Sha256::new();
        hasher.update(prompt.as_bytes());
        hasher.update(operation.as_bytes());
        format!("{:x}", hasher.finalize())
    }

    /// Get cached response if available and not expired
    pub async fn get(&self, prompt: &str, operation: &str) -> Result<Option<String>> {
        let hash = Self::generate_hash(prompt, operation);

        let result = sqlx::query_as::<_, (i64, String, String)>(
            r#"
            SELECT id, response, expires_at
            FROM response_cache
            WHERE content_hash = $1
            AND expires_at > datetime('now')
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
                    last_accessed = datetime('now')
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

    /// Store response in cache
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
        .bind(expires_at.to_rfc3339())
        .execute(&self.pool)
        .await
        .context("Failed to cache response")?;

        tracing::info!("Cached response for {} (TTL: {}h)", operation, ttl);

        Ok(())
    }

    /// Clear expired cache entries
    pub async fn clear_expired(&self) -> Result<u64> {
        let result = sqlx::query(
            r#"
            DELETE FROM response_cache
            WHERE expires_at < datetime('now')
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

    /// Clear all cache entries
    pub async fn clear_all(&self) -> Result<u64> {
        let result = sqlx::query("DELETE FROM response_cache")
            .execute(&self.pool)
            .await
            .context("Failed to clear cache")?;

        let rows_deleted = result.rows_affected();
        tracing::info!("Cleared all cache entries ({})", rows_deleted);

        Ok(rows_deleted)
    }

    /// Clear cache entries for a specific operation
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

    /// Get cache statistics
    pub async fn get_stats(&self) -> Result<CacheStats> {
        let (total_entries,) = sqlx::query_as::<_, (i64,)>(
            "SELECT COUNT(*) FROM response_cache WHERE expires_at > datetime('now')",
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
            "SELECT COALESCE(SUM(LENGTH(response)), 0) FROM response_cache",
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

        let oldest = sqlx::query_as::<_, (String,)>(
            "SELECT created_at FROM response_cache ORDER BY created_at ASC LIMIT 1",
        )
        .fetch_optional(&self.pool)
        .await
        .context("Failed to get oldest entry")?
        .and_then(|(s,)| DateTime::parse_from_rfc3339(&s).ok())
        .map(|dt| dt.with_timezone(&Utc));

        let newest = sqlx::query_as::<_, (String,)>(
            "SELECT created_at FROM response_cache ORDER BY created_at DESC LIMIT 1",
        )
        .fetch_optional(&self.pool)
        .await
        .context("Failed to get newest entry")?
        .and_then(|(s,)| DateTime::parse_from_rfc3339(&s).ok())
        .map(|dt| dt.with_timezone(&Utc));

        Ok(CacheStats {
            total_entries,
            total_hits,
            total_size_bytes,
            hit_rate,
            oldest_entry: oldest,
            newest_entry: newest,
        })
    }

    /// Get cache entries by operation
    pub async fn get_entries_by_operation(&self, operation: &str) -> Result<Vec<CachedResponse>> {
        let entries = sqlx::query_as::<_, (i64, String, String, String, String, String, i64, String)>(
            r#"
            SELECT id, content_hash, operation, response, created_at, expires_at, hit_count, last_accessed
            FROM response_cache
            WHERE operation = $1
            AND expires_at > datetime('now')
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
                )| {
                    CachedResponse {
                        id,
                        content_hash,
                        operation,
                        response,
                        created_at: DateTime::parse_from_rfc3339(&created_at)
                            .unwrap_or_else(|_| Utc::now().into())
                            .with_timezone(&Utc),
                        expires_at: DateTime::parse_from_rfc3339(&expires_at)
                            .unwrap_or_else(|_| Utc::now().into())
                            .with_timezone(&Utc),
                        hit_count,
                        last_accessed: DateTime::parse_from_rfc3339(&last_accessed)
                            .unwrap_or_else(|_| Utc::now().into())
                            .with_timezone(&Utc),
                    }
                },
            )
            .collect())
    }

    /// Get most frequently accessed cache entries
    pub async fn get_hot_entries(&self, limit: i64) -> Result<Vec<CachedResponse>> {
        let entries = sqlx::query_as::<_, (i64, String, String, String, String, String, i64, String)>(
            r#"
            SELECT id, content_hash, operation, response, created_at, expires_at, hit_count, last_accessed
            FROM response_cache
            WHERE expires_at > datetime('now')
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
                )| {
                    CachedResponse {
                        id,
                        content_hash,
                        operation,
                        response,
                        created_at: DateTime::parse_from_rfc3339(&created_at)
                            .unwrap_or_else(|_| Utc::now().into())
                            .with_timezone(&Utc),
                        expires_at: DateTime::parse_from_rfc3339(&expires_at)
                            .unwrap_or_else(|_| Utc::now().into())
                            .with_timezone(&Utc),
                        hit_count,
                        last_accessed: DateTime::parse_from_rfc3339(&last_accessed)
                            .unwrap_or_else(|_| Utc::now().into())
                            .with_timezone(&Utc),
                    }
                },
            )
            .collect())
    }

    /// Estimate cost savings from cache
    pub async fn calculate_savings(&self, cost_per_query: f64) -> Result<f64> {
        let stats = self.get_stats().await?;
        Ok(stats.total_hits as f64 * cost_per_query)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    #[ignore = "ResponseCache uses SQLite internally; not available in postgres-only build"]
    async fn test_cache_operations() -> Result<()> {
        let cache = ResponseCache::new(":memory:").await?;

        // Test cache miss
        let result = cache.get("test prompt", "test_op").await?;
        assert!(result.is_none());

        // Test cache set
        cache
            .set("test prompt", "test_op", "test response", Some(1))
            .await?;

        // Test cache hit
        let result = cache.get("test prompt", "test_op").await?;
        assert_eq!(result, Some("test response".to_string()));

        Ok(())
    }

    #[tokio::test]
    #[ignore = "ResponseCache uses SQLite internally; not available in postgres-only build"]
    async fn test_cache_stats() -> Result<()> {
        let cache = ResponseCache::new(":memory:").await?;

        cache.set("prompt1", "op1", "response1", Some(1)).await?;
        cache.set("prompt2", "op1", "response2", Some(1)).await?;

        // Generate some hits
        cache.get("prompt1", "op1").await?;
        cache.get("prompt1", "op1").await?;

        let stats = cache.get_stats().await?;
        assert_eq!(stats.total_entries, 2);
        assert_eq!(stats.total_hits, 2);

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
