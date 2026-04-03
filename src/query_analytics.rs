//! Query Analytics Module
//!
//! Provides analytics and insights for search patterns and query behavior.
//! Tracks search trends, popular queries, and user search patterns.
//!
//! # Features
//!
//! - **Query Tracking**: Record all search queries with metadata
//! - **Pattern Analysis**: Identify common search patterns
//! - **Performance Metrics**: Track query performance over time
//! - **Trend Detection**: Identify trending searches
//! - **User Insights**: Analyze search behavior per user
//!
//! # Example
//!
//! ```rust,no_run
//! use rustcode::query_analytics::{QueryAnalytics, AnalyticsConfig};
//!
//! # async fn example() -> anyhow::Result<()> {
//! let config = AnalyticsConfig::default();
//! let analytics = QueryAnalytics::new(config).await?;
//!
//! // Track a search
//! analytics.track_search(
//!     "rust async patterns",
//!     "semantic",
//!     10,
//!     45,
//!     Some("user-123")
//! ).await?;
//!
//! // Get popular queries
//! let popular = analytics.get_popular_queries(10).await?;
//! for query in popular {
//!     println!("{}: {} searches", query.query, query.count);
//! }
//! # Ok(())
//! # }
//! ```

use anyhow::{Context, Result};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

// ============================================================================
// Configuration
// ============================================================================

/// Analytics configuration
#[derive(Debug, Clone)]
pub struct AnalyticsConfig {
    /// Enable analytics tracking
    pub enabled: bool,

    /// Database pool
    pub db_pool: Option<PgPool>,

    /// Retention period in days
    pub retention_days: i64,

    /// Enable in-memory aggregation
    pub enable_memory_cache: bool,

    /// Aggregate interval in seconds
    pub aggregate_interval_secs: u64,
}

impl Default for AnalyticsConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            db_pool: None,
            retention_days: 90,
            enable_memory_cache: true,
            aggregate_interval_secs: 300, // 5 minutes
        }
    }
}

// ============================================================================
// Data Structures
// ============================================================================

/// Search analytics entry
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchAnalytics {
    pub id: i64,
    pub query: String,
    pub search_type: String, // semantic, keyword, hybrid
    pub result_count: i32,
    pub execution_time_ms: i64,
    pub user_id: Option<String>,
    pub filters: Option<String>, // JSON serialized filters
    pub timestamp: DateTime<Utc>,
}

/// Query pattern statistics
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryPattern {
    pub query: String,
    pub count: i64,
    pub avg_execution_time_ms: f64,
    pub avg_results: f64,
    pub first_seen: DateTime<Utc>,
    pub last_seen: DateTime<Utc>,
    pub search_types: Vec<String>,
}

/// Time-based analytics
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimeSeriesPoint {
    pub timestamp: DateTime<Utc>,
    pub query_count: i64,
    pub avg_execution_time_ms: f64,
    pub unique_queries: i64,
}

/// User search behavior
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserSearchBehavior {
    pub user_id: String,
    pub total_searches: i64,
    pub unique_queries: i64,
    pub avg_results: f64,
    pub favorite_search_type: String,
    pub most_searched_terms: Vec<(String, i64)>,
}

/// Analytics statistics
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnalyticsStats {
    pub total_searches: i64,
    pub unique_queries: i64,
    pub unique_users: i64,
    pub avg_execution_time_ms: f64,
    pub avg_results_per_query: f64,
    pub search_type_distribution: HashMap<String, i64>,
    pub period_start: i64,
    pub period_end: i64,
}

// ============================================================================
// Query Analytics
// ============================================================================

pub struct QueryAnalytics {
    config: AnalyticsConfig,
    db_pool: PgPool,
    memory_cache: Arc<RwLock<MemoryCache>>,
}

#[derive(Debug, Default)]
struct MemoryCache {
    _recent_queries: Vec<SearchAnalytics>,
    pattern_counts: HashMap<String, i64>,
    _last_aggregation: Option<DateTime<Utc>>,
}

impl QueryAnalytics {
    /// Create new query analytics instance
    pub async fn new(config: AnalyticsConfig) -> Result<Self> {
        let db_pool = config
            .db_pool
            .clone()
            .context("Database pool required for analytics")?;

        // Initialize tables
        Self::init_tables(&db_pool).await?;

        let memory_cache = Arc::new(RwLock::new(MemoryCache::default()));

        let analytics = Self {
            config,
            db_pool,
            memory_cache,
        };

        // Start background cleanup task
        if analytics.config.enabled {
            analytics.start_cleanup_task();
        }

        Ok(analytics)
    }

    /// Initialize analytics tables
    async fn init_tables(pool: &PgPool) -> Result<()> {
        // Acquire a session-level advisory lock so that concurrent test threads
        // don't race on `CREATE TABLE IF NOT EXISTS` + `BIGSERIAL` sequence
        // creation, which would trigger a `pg_type_typname_nsp_index` unique-
        // constraint violation inside Postgres.
        sqlx::query("SELECT pg_advisory_lock(7483920)")
            .execute(pool)
            .await
            .context("Failed to acquire analytics init lock")?;

        let result = Self::init_tables_inner(pool).await;

        // Always release the advisory lock, even if init failed.
        let _ = sqlx::query("SELECT pg_advisory_unlock(7483920)")
            .execute(pool)
            .await;

        result
    }

    async fn init_tables_inner(pool: &PgPool) -> Result<()> {
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS search_analytics (
                id BIGSERIAL PRIMARY KEY,
                query TEXT NOT NULL,
                search_type TEXT NOT NULL,
                result_count INTEGER NOT NULL,
                execution_time_ms INTEGER NOT NULL,
                user_id TEXT,
                filters TEXT,
                timestamp TIMESTAMPTZ DEFAULT NOW(),
                created_at TIMESTAMPTZ DEFAULT NOW()
            )
            "#,
        )
        .execute(pool)
        .await
        .context("Failed to create search_analytics table")?;

        // Create indexes
        sqlx::query(
            r#"
            CREATE INDEX IF NOT EXISTS idx_search_analytics_query
            ON search_analytics(query)
            "#,
        )
        .execute(pool)
        .await?;

        sqlx::query(
            r#"
            CREATE INDEX IF NOT EXISTS idx_search_analytics_timestamp
            ON search_analytics(timestamp)
            "#,
        )
        .execute(pool)
        .await?;

        sqlx::query(
            r#"
            CREATE INDEX IF NOT EXISTS idx_search_analytics_user
            ON search_analytics(user_id)
            "#,
        )
        .execute(pool)
        .await?;

        Ok(())
    }

    /// Track a search query
    pub async fn track_search(
        &self,
        query: &str,
        search_type: &str,
        result_count: i32,
        execution_time_ms: i64,
        user_id: Option<&str>,
    ) -> Result<i64> {
        if !self.config.enabled {
            return Ok(0);
        }

        let id = sqlx::query_scalar::<_, i64>(
            r#"
            INSERT INTO search_analytics (query, search_type, result_count, execution_time_ms, user_id, timestamp)
            VALUES ($1, $2, $3, $4, $5, $6)
            RETURNING id
            "#,
        )
        .bind(query)
        .bind(search_type)
        .bind(result_count)
        .bind(execution_time_ms)
        .bind(user_id)
        .bind(Utc::now())
        .fetch_one(&self.db_pool)
        .await
        .context("Failed to track search")?;

        // Update memory cache
        if self.config.enable_memory_cache {
            let mut cache = self.memory_cache.write().await;
            cache
                .pattern_counts
                .entry(query.to_string())
                .and_modify(|c| *c += 1)
                .or_insert(1);
        }

        Ok(id)
    }

    /// Track search with filters
    pub async fn track_search_with_filters(
        &self,
        query: &str,
        search_type: &str,
        result_count: i32,
        execution_time_ms: i64,
        user_id: Option<&str>,
        filters: &HashMap<String, String>,
    ) -> Result<i64> {
        if !self.config.enabled {
            return Ok(0);
        }

        let filters_json = serde_json::to_string(filters)?;

        let id = sqlx::query_scalar::<_, i64>(
            r#"
            INSERT INTO search_analytics (query, search_type, result_count, execution_time_ms, user_id, filters, timestamp)
            VALUES ($1, $2, $3, $4, $5, $6, $7)
            RETURNING id
            "#,
        )
        .bind(query)
        .bind(search_type)
        .bind(result_count)
        .bind(execution_time_ms)
        .bind(user_id)
        .bind(filters_json)
        .bind(Utc::now())
        .fetch_one(&self.db_pool)
        .await
        .context("Failed to track search with filters")?;

        Ok(id)
    }

    /// Get popular queries
    pub async fn get_popular_queries(&self, limit: i64) -> Result<Vec<QueryPattern>> {
        let patterns = sqlx::query_as::<_, (String, i64, f64, f64, DateTime<Utc>, DateTime<Utc>)>(
            r#"
            SELECT
                query,
                COUNT(*) as count,
                AVG(execution_time_ms)::DOUBLE PRECISION as avg_time,
                AVG(result_count)::DOUBLE PRECISION as avg_results,
                MIN(timestamp) as first_seen,
                MAX(timestamp) as last_seen
            FROM search_analytics
            WHERE timestamp > NOW() - INTERVAL '30 days'
            GROUP BY query
            ORDER BY count DESC
            LIMIT $1
            "#,
        )
        .bind(limit)
        .fetch_all(&self.db_pool)
        .await
        .context("Failed to fetch popular queries")?;

        let mut results = Vec::new();
        for (query, count, avg_time, avg_results, first_seen, last_seen) in patterns {
            // Get search types for this query
            let search_types = sqlx::query_scalar::<_, String>(
                r#"
                SELECT DISTINCT search_type
                FROM search_analytics
                WHERE query = $1
                "#,
            )
            .bind(&query)
            .fetch_all(&self.db_pool)
            .await?;

            results.push(QueryPattern {
                query,
                count,
                avg_execution_time_ms: avg_time,
                avg_results,
                first_seen,
                last_seen,
                search_types,
            });
        }

        Ok(results)
    }

    /// Get trending queries (increasing in popularity)
    pub async fn get_trending_queries(&self, limit: i64) -> Result<Vec<QueryPattern>> {
        let patterns =
            sqlx::query_as::<_, (String, i64, i64, f64, f64, DateTime<Utc>, DateTime<Utc>)>(
                r#"
            WITH recent AS (
                SELECT query, COUNT(*) as recent_count
                FROM search_analytics
                WHERE timestamp > NOW() - INTERVAL '7 days'
                GROUP BY query
            ),
            older AS (
                SELECT query, COUNT(*) as older_count
                FROM search_analytics
                WHERE timestamp BETWEEN NOW() - INTERVAL '14 days' AND NOW() - INTERVAL '7 days'
                GROUP BY query
            )
            SELECT
                sa.query,
                COUNT(*) as total_count,
                COALESCE(recent.recent_count, 0) - COALESCE(older.older_count, 0) as trend,
                AVG(sa.execution_time_ms)::DOUBLE PRECISION as avg_time,
                AVG(sa.result_count)::DOUBLE PRECISION as avg_results,
                MIN(sa.timestamp) as first_seen,
                MAX(sa.timestamp) as last_seen
            FROM search_analytics sa
            LEFT JOIN recent ON sa.query = recent.query
            LEFT JOIN older ON sa.query = older.query
            WHERE sa.timestamp > NOW() - INTERVAL '30 days'
            GROUP BY sa.query
            HAVING COALESCE(recent.recent_count, 0) - COALESCE(older.older_count, 0) > 0
            ORDER BY trend DESC
            LIMIT $1
            "#,
            )
            .bind(limit)
            .fetch_all(&self.db_pool)
            .await
            .context("Failed to fetch trending queries")?;

        let mut results = Vec::new();
        for (query, _total, _trend, avg_time, avg_results, first_seen, last_seen) in patterns {
            let search_types = sqlx::query_scalar::<_, String>(
                r#"
                SELECT DISTINCT search_type
                FROM search_analytics
                WHERE query = $1
                "#,
            )
            .bind(&query)
            .fetch_all(&self.db_pool)
            .await?;

            results.push(QueryPattern {
                query: query.clone(),
                count: _total,
                avg_execution_time_ms: avg_time,
                avg_results,
                first_seen,
                last_seen,
                search_types,
            });
        }

        Ok(results)
    }

    /// Get user search behavior
    pub async fn get_user_behavior(&self, user_id: &str) -> Result<Option<UserSearchBehavior>> {
        let row = sqlx::query_as::<_, (i64, i64, f64)>(
            r#"
            SELECT
                COUNT(*) as total_searches,
                COUNT(DISTINCT query) as unique_queries,
                AVG(result_count)::DOUBLE PRECISION as avg_results
            FROM search_analytics
            WHERE user_id = $1
            "#,
        )
        .bind(user_id)
        .fetch_optional(&self.db_pool)
        .await?;

        if row.is_none() {
            return Ok(None);
        }

        let (total_searches, unique_queries, avg_results) = row.unwrap();

        // Get favorite search type
        let favorite_search_type = sqlx::query_scalar::<_, String>(
            r#"
            SELECT search_type
            FROM search_analytics
            WHERE user_id = $1
            GROUP BY search_type
            ORDER BY COUNT(*) DESC
            LIMIT 1
            "#,
        )
        .bind(user_id)
        .fetch_one(&self.db_pool)
        .await?;

        // Get most searched terms
        let most_searched = sqlx::query_as::<_, (String, i64)>(
            r#"
            SELECT query, COUNT(*) as count
            FROM search_analytics
            WHERE user_id = $1
            GROUP BY query
            ORDER BY count DESC
            LIMIT 10
            "#,
        )
        .bind(user_id)
        .fetch_all(&self.db_pool)
        .await?;

        Ok(Some(UserSearchBehavior {
            user_id: user_id.to_string(),
            total_searches,
            unique_queries,
            avg_results,
            favorite_search_type,
            most_searched_terms: most_searched,
        }))
    }

    /// Get time series data
    pub async fn get_time_series(
        &self,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
        interval_hours: i64,
    ) -> Result<Vec<TimeSeriesPoint>> {
        let points = sqlx::query_as::<_, (String, i64, f64, i64)>(
            r#"
            SELECT
                TO_CHAR(
                    DATE_TRUNC('hour', timestamp) +
                    (FLOOR(EXTRACT(HOUR FROM timestamp) / $1::float) * $1::float || ' hours')::INTERVAL,
                    'YYYY-MM-DD"T"HH24:MI:SS+00:00'
                ) as bucket,
                COUNT(*) as query_count,
                AVG(execution_time_ms) as avg_time,
                COUNT(DISTINCT query) as unique_queries
            FROM search_analytics
            WHERE timestamp BETWEEN TO_TIMESTAMP($3) AND TO_TIMESTAMP($4)
            GROUP BY bucket
            ORDER BY bucket
            "#,
        )
        .bind(interval_hours)
        .bind(interval_hours)
        .bind(start.timestamp())
        .bind(end.timestamp())
        .fetch_all(&self.db_pool)
        .await
        .context("Failed to fetch time series")?;

        let mut results = Vec::new();
        for (bucket, count, avg_time, unique) in points {
            results.push(TimeSeriesPoint {
                timestamp: DateTime::parse_from_rfc3339(&bucket)?.with_timezone(&Utc),
                query_count: count,
                avg_execution_time_ms: avg_time,
                unique_queries: unique,
            });
        }

        Ok(results)
    }

    /// Get overall analytics statistics
    pub async fn get_stats(&self, days: i64) -> Result<AnalyticsStats> {
        let start = Utc::now() - Duration::days(days);
        let end = Utc::now();

        let row = sqlx::query_as::<_, (i64, i64, i64, f64, f64)>(
            r#"
            SELECT
                COUNT(*) as total_searches,
                COUNT(DISTINCT query) as unique_queries,
                COUNT(DISTINCT user_id) as unique_users,
                AVG(execution_time_ms)::DOUBLE PRECISION as avg_time,
                AVG(result_count)::DOUBLE PRECISION as avg_results
            FROM search_analytics
            WHERE timestamp BETWEEN $1 AND $2
            "#,
        )
        .bind(start)
        .bind(end)
        .fetch_one(&self.db_pool)
        .await?;

        let (total, unique_q, unique_u, avg_time, avg_results) = row;

        // Get search type distribution
        let types = sqlx::query_as::<_, (String, i64)>(
            r#"
            SELECT search_type, COUNT(*) as count
            FROM search_analytics
            WHERE timestamp BETWEEN $1 AND $2
            GROUP BY search_type
            "#,
        )
        .bind(start)
        .bind(end)
        .fetch_all(&self.db_pool)
        .await?;

        let mut distribution = HashMap::new();
        for (search_type, count) in types {
            distribution.insert(search_type, count);
        }

        Ok(AnalyticsStats {
            total_searches: total,
            unique_queries: unique_q,
            unique_users: unique_u,
            avg_execution_time_ms: avg_time,
            avg_results_per_query: avg_results,
            search_type_distribution: distribution,
            period_start: start.timestamp(),
            period_end: end.timestamp(),
        })
    }

    /// Cleanup old analytics data
    pub async fn cleanup_old_data(&self) -> Result<u64> {
        let cutoff = Utc::now() - Duration::days(self.config.retention_days);

        let result = sqlx::query(
            r#"
            DELETE FROM search_analytics
            WHERE timestamp < $1
            "#,
        )
        .bind(cutoff)
        .execute(&self.db_pool)
        .await?;

        Ok(result.rows_affected())
    }

    /// Start background cleanup task
    fn start_cleanup_task(&self) {
        let db_pool = self.db_pool.clone();
        let retention_days = self.config.retention_days;

        tokio::spawn(async move {
            let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(86400)); // Daily
            loop {
                interval.tick().await;

                let cutoff = Utc::now() - Duration::days(retention_days);
                let _ = sqlx::query(
                    r#"
                    DELETE FROM search_analytics
                    WHERE timestamp < $1
                    "#,
                )
                .bind(cutoff)
                .execute(&db_pool)
                .await;
            }
        });
    }

    /// Export analytics data for reporting
    pub async fn export_data(
        &self,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    ) -> Result<Vec<SearchAnalytics>> {
        let records = sqlx::query_as::<_, (i64, String, String, i32, i64, Option<String>, Option<String>, DateTime<Utc>)>(
            r#"
            SELECT id, query, search_type, result_count, execution_time_ms, user_id, filters, timestamp
            FROM search_analytics
            WHERE timestamp BETWEEN $1 AND $2
            ORDER BY timestamp
            "#,
        )
        .bind(start)
        .bind(end)
        .fetch_all(&self.db_pool)
        .await?;

        let mut results = Vec::new();
        for (
            id,
            query,
            search_type,
            result_count,
            execution_time_ms,
            user_id,
            filters,
            timestamp,
        ) in records
        {
            results.push(SearchAnalytics {
                id,
                query,
                search_type,
                result_count,
                execution_time_ms,
                user_id,
                filters,
                timestamp,
            });
        }

        Ok(results)
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    async fn setup_test_db() -> PgPool {
        let pool = PgPool::connect(&std::env::var("DATABASE_URL").unwrap_or_else(|_| {
            "postgresql://rustcode:changeme@localhost:5432/rustcode_test".to_string()
        }))
        .await
        .unwrap();
        QueryAnalytics::init_tables(&pool).await.unwrap();
        pool
    }

    #[tokio::test]
    async fn test_track_search() {
        let pool = setup_test_db().await;
        let config = AnalyticsConfig {
            enabled: true,
            db_pool: Some(pool),
            ..Default::default()
        };

        let analytics = QueryAnalytics::new(config).await.unwrap();

        let id = analytics
            .track_search("rust async", "semantic", 10, 45, Some("user-1"))
            .await
            .unwrap();

        assert!(id > 0);
    }

    #[tokio::test]
    async fn test_popular_queries() {
        let pool = setup_test_db().await;
        let config = AnalyticsConfig {
            enabled: true,
            db_pool: Some(pool),
            ..Default::default()
        };

        let analytics = QueryAnalytics::new(config).await.unwrap();

        // Use unique query strings per test run so that rows inserted by
        // other parallel tests (e.g. test_track_search) don't inflate our
        // counts or push our expected top entry off the leaderboard.
        use std::time::{SystemTime, UNIX_EPOCH};
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .subsec_nanos();
        let top_query = format!("rust async unique {}", nanos);
        let other_query = format!("python flask unique {}", nanos);

        analytics
            .track_search(&top_query, "semantic", 10, 45, Some("user-1"))
            .await
            .unwrap();
        analytics
            .track_search(&top_query, "semantic", 12, 50, Some("user-2"))
            .await
            .unwrap();
        analytics
            .track_search(&other_query, "keyword", 8, 30, Some("user-1"))
            .await
            .unwrap();

        let popular = analytics.get_popular_queries(10).await.unwrap();
        assert!(!popular.is_empty());

        // Find our top_query entry — it must exist and have count >= 2.
        let entry = popular
            .iter()
            .find(|p| p.query == top_query)
            .expect("top unique query should appear in popular queries");
        assert!(
            entry.count >= 2,
            "expected count >= 2 for '{}', got {}",
            top_query,
            entry.count
        );

        // other_query should appear too, with count >= 1.
        let other = popular.iter().find(|p| p.query == other_query);
        assert!(other.is_some(), "secondary unique query should also appear");
    }

    #[tokio::test]
    async fn test_analytics_stats() {
        use std::time::{SystemTime, UNIX_EPOCH};
        let pool = setup_test_db().await;
        let config = AnalyticsConfig {
            enabled: true,
            db_pool: Some(pool),
            ..Default::default()
        };

        let analytics = QueryAnalytics::new(config).await.unwrap();

        // Use a unique user_id so we can verify this test's row was inserted
        // even when other parallel tests have also written to search_analytics.
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .subsec_nanos();
        let unique_user = format!("stats-test-user-{}", nanos);

        analytics
            .track_search("test query", "semantic", 5, 40, Some(&unique_user))
            .await
            .unwrap();

        // get_stats counts ALL rows in the window, so other parallel tests may
        // have added rows too — use >= 1 for global counters.
        let stats = analytics.get_stats(30).await.unwrap();
        assert!(
            stats.total_searches >= 1,
            "expected at least 1 total search, got {}",
            stats.total_searches
        );
        assert!(
            stats.unique_queries >= 1,
            "expected at least 1 unique query, got {}",
            stats.unique_queries
        );

        // Confirm our specific unique user appears in the per-user behaviour,
        // which gives an exact row count for just this test's inserts.
        let behavior = analytics
            .get_user_behavior(&unique_user)
            .await
            .unwrap()
            .expect("user behaviour should exist after tracking");
        assert_eq!(behavior.total_searches, 1);
        assert_eq!(behavior.unique_queries, 1);
    }
}
