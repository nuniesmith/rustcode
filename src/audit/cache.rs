// Audit cache — Redis-backed deduplication for audit results
//
// Skips re-auditing files whose content hash hasn't changed since the last
// run. Uses the same Redis pool as the LLM response cache (`allkeys-lru`,
// 256 MB) so there is no extra infrastructure required.
//
// # Key scheme
//
// ```text
// audit:file:<sha256_of_content>          → AuditFileCacheEntry (JSON, TTL 7 days)
// audit:repo:<repo_id>:run:<timestamp>    → AuditRunSummary     (JSON, TTL 30 days)
// audit:repo:<repo_id>:latest             → run timestamp string (no TTL)
// ```
//
// # TODO(scaffolder): implement
//
// The structs and trait are fully defined. Implement the Redis I/O in the
// `RedisAuditCache` methods — everything is stubbed with `todo!()`.

use redis::AsyncCommands;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::Duration;

use crate::error::{AuditError, Result};

// ============================================================================
// Configuration
// ============================================================================

// Configuration for the audit result cache
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditCacheConfig {
    // Redis connection URL (e.g. `redis://127.0.0.1:6379`)
    pub redis_url: String,
    // TTL for per-file cache entries
    pub file_entry_ttl: Duration,
    // TTL for per-run summary entries
    pub run_summary_ttl: Duration,
    // Key prefix (allows namespacing in a shared Redis instance)
    pub key_prefix: String,
    // Whether caching is enabled at all (set to `false` to force re-audit)
    pub enabled: bool,
}

impl Default for AuditCacheConfig {
    fn default() -> Self {
        Self {
            redis_url: "redis://127.0.0.1:6379".to_string(),
            file_entry_ttl: Duration::from_secs(60 * 60 * 24 * 7), // 7 days
            run_summary_ttl: Duration::from_secs(60 * 60 * 24 * 30), // 30 days
            key_prefix: "audit".to_string(),
            enabled: true,
        }
    }
}

impl AuditCacheConfig {
    // Create a disabled cache config (useful in tests / CI dry-runs)
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            ..Default::default()
        }
    }

    // Build a file-level cache key from a content hash
    pub fn file_key(&self, sha256: &str) -> String {
        format!("{}:file:{}", self.key_prefix, sha256)
    }

    // Build a run-level cache key
    pub fn run_key(&self, repo_id: &str, timestamp: &str) -> String {
        format!("{}:repo:{}:run:{}", self.key_prefix, repo_id, timestamp)
    }

    // Build the "latest run" pointer key for a repo
    pub fn latest_key(&self, repo_id: &str) -> String {
        format!("{}:repo:{}:latest", self.key_prefix, repo_id)
    }
}

// ============================================================================
// Cache entry types
// ============================================================================

// The severity level of a single audit finding (mirrors `AuditSeverity`)
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CachedSeverity {
    Info,
    Low,
    Medium,
    High,
    Critical,
}

// A cached audit result for a single source file
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditFileCacheEntry {
    // SHA-256 of the file content at the time of the audit
    pub content_sha256: String,
    // Relative path to the file within the repo
    pub file_path: String,
    // When this entry was cached
    pub cached_at: DateTime<Utc>,
    // Overall quality score (0–100)
    pub quality_score: f32,
    // Security score (0–100)
    pub security_score: f32,
    // Number of findings by severity
    pub finding_counts: HashMap<String, usize>,
    // Serialised finding summaries (short strings, not full `AuditFinding` objects)
    pub finding_summaries: Vec<String>,
    // Model that produced this result
    pub model: String,
    // Token cost in USD
    pub cost_usd: f64,
}

impl AuditFileCacheEntry {
    // Whether this cached result has any high/critical findings
    pub fn has_critical_findings(&self) -> bool {
        self.finding_counts
            .iter()
            .filter(|(k, _)| k.as_str() == "critical" || k.as_str() == "high")
            .any(|(_, &count)| count > 0)
    }

    // Total finding count across all severities
    pub fn total_findings(&self) -> usize {
        self.finding_counts.values().sum()
    }
}

// Summary of a complete audit run stored in Redis
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditRunSummary {
    // Unique run identifier (timestamp-based)
    pub run_id: String,
    // The repository that was audited
    pub repo_id: String,
    // When the run started
    pub started_at: DateTime<Utc>,
    // When the run completed
    pub completed_at: Option<DateTime<Utc>>,
    // Total files audited in this run
    pub files_audited: usize,
    // Files skipped due to cache hits
    pub files_from_cache: usize,
    // Files that failed to audit
    pub files_failed: usize,
    // Aggregate quality score across all files
    pub avg_quality_score: f32,
    // Aggregate security score across all files
    pub avg_security_score: f32,
    // Total LLM cost for this run in USD
    pub total_cost_usd: f64,
    // High/critical finding count across all files
    pub critical_findings: usize,
    // Status of the run
    pub status: RunStatus,
}

// Status of an audit run
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RunStatus {
    Running,
    Completed,
    Failed,
    Cancelled,
}

impl std::fmt::Display for RunStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RunStatus::Running => write!(f, "running"),
            RunStatus::Completed => write!(f, "completed"),
            RunStatus::Failed => write!(f, "failed"),
            RunStatus::Cancelled => write!(f, "cancelled"),
        }
    }
}

// ============================================================================
// AuditCache trait
// ============================================================================

// Interface for audit result caching backends
#[async_trait::async_trait]
pub trait AuditCache: Send + Sync {
    // Check whether a file with the given SHA-256 has a cached audit result.
    // Returns `None` if not cached or if caching is disabled.
    async fn get_file_result(&self, sha256: &str) -> Result<Option<AuditFileCacheEntry>>;

    // Store an audit result for a file
    async fn set_file_result(&self, entry: &AuditFileCacheEntry) -> Result<()>;

    // Delete a cached file result (e.g. after a content update)
    async fn invalidate_file(&self, sha256: &str) -> Result<()>;

    // Retrieve a run summary by run ID
    async fn get_run_summary(&self, repo_id: &str, run_id: &str)
    -> Result<Option<AuditRunSummary>>;

    // Store a run summary
    async fn set_run_summary(&self, summary: &AuditRunSummary) -> Result<()>;

    // Get the latest run summary for a repo (follows the `latest` pointer)
    async fn get_latest_run(&self, repo_id: &str) -> Result<Option<AuditRunSummary>>;

    // Return cache statistics (hits, misses, key count)
    async fn stats(&self) -> Result<AuditCacheStats>;

    // Flush all audit cache keys (use with care — respects the key prefix)
    async fn flush(&self) -> Result<usize>;
}

// ============================================================================
// Cache statistics
// ============================================================================

// Statistics about the audit cache
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AuditCacheStats {
    // Total number of file-level cache keys present
    pub file_keys: usize,
    // Total number of run-level cache keys present
    pub run_keys: usize,
    // Approximate memory used by audit keys (bytes), if available
    pub memory_bytes: Option<u64>,
    // Hit rate since the server started (0.0–1.0), if tracked
    pub hit_rate: Option<f64>,
}

// ============================================================================
// Redis implementation (stub — TODO: implement)
// ============================================================================

// Redis-backed audit cache
pub struct RedisAuditCache {
    config: AuditCacheConfig,
    pool: Option<deadpool_redis::Pool>,
}

impl RedisAuditCache {
    // Create a new Redis-backed audit cache.
    //
    // If `config.enabled` is `false` or the Redis pool cannot be created,
    // the cache operates in no-op mode (all reads miss, all writes are dropped).
    pub async fn new(config: AuditCacheConfig) -> Result<Self> {
        let pool = if config.enabled {
            match deadpool_redis::Config::from_url(&config.redis_url)
                .create_pool(Some(deadpool_redis::Runtime::Tokio1))
            {
                Ok(p) => Some(p),
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        url = %config.redis_url,
                        "RedisAuditCache: failed to create pool — running in no-op mode"
                    );
                    None
                }
            }
        } else {
            None
        };
        Ok(Self { config, pool })
    }

    // Build from environment — reads `REDIS_URL` or falls back to default
    pub async fn from_env() -> Result<Self> {
        let redis_url =
            std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://127.0.0.1:6379".to_string());
        Self::new(AuditCacheConfig {
            redis_url,
            ..Default::default()
        })
        .await
    }

    // Serialise a value to JSON bytes for Redis storage
    fn encode<T: Serialize>(value: &T) -> Result<Vec<u8>> {
        serde_json::to_vec(value)
            .map_err(|e| AuditError::other(format!("Cache serialisation error: {}", e)))
    }

    // Deserialise JSON bytes from Redis storage
    fn decode<T: for<'de> Deserialize<'de>>(bytes: &[u8]) -> Result<T> {
        serde_json::from_slice(bytes)
            .map_err(|e| AuditError::other(format!("Cache deserialisation error: {}", e)))
    }
}

#[async_trait::async_trait]
impl AuditCache for RedisAuditCache {
    async fn get_file_result(&self, sha256: &str) -> Result<Option<AuditFileCacheEntry>> {
        let pool = match self.pool.as_ref() {
            Some(p) => p,
            None => return Ok(None),
        };
        let key = self.config.file_key(sha256);
        let mut conn = pool
            .get()
            .await
            .map_err(|e| AuditError::other(format!("Redis pool error: {}", e)))?;
        let bytes: Option<Vec<u8>> = redis::cmd("GET")
            .arg(&key)
            .query_async(&mut conn)
            .await
            .map_err(|e| AuditError::other(format!("Redis GET error: {}", e)))?;
        match bytes {
            Some(b) => Ok(Some(Self::decode(&b)?)),
            None => Ok(None),
        }
    }

    async fn set_file_result(&self, entry: &AuditFileCacheEntry) -> Result<()> {
        let pool = match self.pool.as_ref() {
            Some(p) => p,
            None => return Ok(()),
        };
        let key = self.config.file_key(&entry.content_sha256);
        let bytes = Self::encode(entry)?;
        let ttl_secs = self.config.file_entry_ttl.as_secs();
        let mut conn = pool
            .get()
            .await
            .map_err(|e| AuditError::other(format!("Redis pool error: {}", e)))?;
        redis::cmd("SETEX")
            .arg(&key)
            .arg(ttl_secs)
            .arg(&bytes)
            .query_async::<()>(&mut conn)
            .await
            .map_err(|e| AuditError::other(format!("Redis SETEX error: {}", e)))?;
        Ok(())
    }

    async fn invalidate_file(&self, sha256: &str) -> Result<()> {
        let pool = match self.pool.as_ref() {
            Some(p) => p,
            None => return Ok(()),
        };
        let key = self.config.file_key(sha256);
        let mut conn = pool
            .get()
            .await
            .map_err(|e| AuditError::other(format!("Redis pool error: {}", e)))?;
        redis::cmd("DEL")
            .arg(&key)
            .query_async::<()>(&mut conn)
            .await
            .map_err(|e| AuditError::other(format!("Redis DEL error: {}", e)))?;
        Ok(())
    }

    async fn get_run_summary(
        &self,
        repo_id: &str,
        run_id: &str,
    ) -> Result<Option<AuditRunSummary>> {
        let pool = match self.pool.as_ref() {
            Some(p) => p,
            None => return Ok(None),
        };
        let key = self.config.run_key(repo_id, run_id);
        let mut conn = pool
            .get()
            .await
            .map_err(|e| AuditError::other(format!("Redis pool error: {}", e)))?;
        let bytes: Option<Vec<u8>> = redis::cmd("GET")
            .arg(&key)
            .query_async(&mut conn)
            .await
            .map_err(|e| AuditError::other(format!("Redis GET error: {}", e)))?;
        match bytes {
            Some(b) => Ok(Some(Self::decode(&b)?)),
            None => Ok(None),
        }
    }

    async fn set_run_summary(&self, summary: &AuditRunSummary) -> Result<()> {
        let pool = match self.pool.as_ref() {
            Some(p) => p,
            None => return Ok(()),
        };
        let key = self.config.run_key(&summary.repo_id, &summary.run_id);
        let latest_key = self.config.latest_key(&summary.repo_id);
        let bytes = Self::encode(summary)?;
        let ttl_secs = self.config.run_summary_ttl.as_secs();
        let run_id = summary.run_id.clone();
        let mut conn = pool
            .get()
            .await
            .map_err(|e| AuditError::other(format!("Redis pool error: {}", e)))?;
        // Store the full summary under the run key with TTL
        redis::cmd("SETEX")
            .arg(&key)
            .arg(ttl_secs)
            .arg(&bytes)
            .query_async::<()>(&mut conn)
            .await
            .map_err(|e| AuditError::other(format!("Redis SETEX error: {}", e)))?;
        // Update the latest-run pointer (no TTL — always points to newest run)
        redis::cmd("SET")
            .arg(&latest_key)
            .arg(&run_id)
            .query_async::<()>(&mut conn)
            .await
            .map_err(|e| AuditError::other(format!("Redis SET error: {}", e)))?;
        Ok(())
    }

    async fn get_latest_run(&self, repo_id: &str) -> Result<Option<AuditRunSummary>> {
        let pool = match self.pool.as_ref() {
            Some(p) => p,
            None => return Ok(None),
        };
        let latest_key = self.config.latest_key(repo_id);
        let mut conn = pool
            .get()
            .await
            .map_err(|e| AuditError::other(format!("Redis pool error: {}", e)))?;
        // Step 1: resolve the latest run_id pointer
        let run_id: Option<String> = redis::cmd("GET")
            .arg(&latest_key)
            .query_async(&mut conn)
            .await
            .map_err(|e| AuditError::other(format!("Redis GET error: {}", e)))?;
        let run_id = match run_id {
            Some(id) => id,
            None => return Ok(None),
        };
        // Step 2: fetch the run summary by run_id
        drop(conn); // release connection back to pool before recursive call
        self.get_run_summary(repo_id, &run_id).await
    }

    async fn stats(&self) -> Result<AuditCacheStats> {
        let pool = match self.pool.as_ref() {
            Some(p) => p,
            None => return Ok(AuditCacheStats::default()),
        };
        let mut conn = pool
            .get()
            .await
            .map_err(|e| AuditError::other(format!("Redis pool error: {}", e)))?;

        // Count file-level cache keys
        let file_pattern = format!("{}:file:*", self.config.key_prefix);
        let file_keys: Vec<String> = scan_all_keys(&mut conn, &file_pattern).await?;

        // Count run-level cache keys
        let run_pattern = format!("{}:repo:*:run:*", self.config.key_prefix);
        let run_keys: Vec<String> = scan_all_keys(&mut conn, &run_pattern).await?;

        // Get memory usage from Redis INFO
        let info: String = redis::cmd("INFO")
            .arg("memory")
            .query_async(&mut conn)
            .await
            .unwrap_or_default();
        let memory_bytes = parse_used_memory(&info);

        Ok(AuditCacheStats {
            file_keys: file_keys.len(),
            run_keys: run_keys.len(),
            memory_bytes: Some(memory_bytes),
            hit_rate: None, // Redis doesn't expose per-keyspace hit rates easily
        })
    }

    async fn flush(&self) -> Result<usize> {
        let pool = match self.pool.as_ref() {
            Some(p) => p,
            None => return Ok(0),
        };
        let mut conn = pool
            .get()
            .await
            .map_err(|e| AuditError::other(format!("Redis pool error: {}", e)))?;

        let pattern = format!("{}:*", self.config.key_prefix);
        let keys: Vec<String> = scan_all_keys(&mut conn, &pattern).await?;

        if keys.is_empty() {
            return Ok(0);
        }

        let count = keys.len();
        redis::cmd("DEL")
            .arg(&keys)
            .query_async::<()>(&mut conn)
            .await
            .map_err(|e| AuditError::other(format!("Redis DEL error: {}", e)))?;

        Ok(count)
    }
}

// ============================================================================
// Helpers
// ============================================================================

// Collect all keys matching `pattern` using SCAN (cursor-based, safe on large keyspaces).
async fn scan_all_keys(
    conn: &mut deadpool_redis::Connection,
    pattern: &str,
) -> Result<Vec<String>> {
    let mut cursor: u64 = 0;
    let mut all_keys = Vec::new();
    loop {
        let (next_cursor, batch): (u64, Vec<String>) = redis::cmd("SCAN")
            .arg(cursor)
            .arg("MATCH")
            .arg(pattern)
            .arg("COUNT")
            .arg(200u64)
            .query_async(conn)
            .await
            .map_err(|e| AuditError::other(format!("Redis SCAN error: {}", e)))?;
        all_keys.extend(batch);
        cursor = next_cursor;
        if cursor == 0 {
            break;
        }
    }
    Ok(all_keys)
}

// Parse `used_memory:` bytes from Redis `INFO memory` output.
fn parse_used_memory(info: &str) -> u64 {
    for line in info.lines() {
        if let Some(rest) = line.strip_prefix("used_memory:") {
            if let Ok(n) = rest.trim().parse::<u64>() {
                return n;
            }
        }
    }
    0
}

// ============================================================================
// No-op cache (for testing / offline mode)
// ============================================================================

// A no-op cache that always returns `None` and discards all writes.
// Used in tests and when Redis is unavailable.
pub struct NoopAuditCache;

#[async_trait::async_trait]
impl AuditCache for NoopAuditCache {
    async fn get_file_result(&self, _sha256: &str) -> Result<Option<AuditFileCacheEntry>> {
        Ok(None)
    }

    async fn set_file_result(&self, _entry: &AuditFileCacheEntry) -> Result<()> {
        Ok(())
    }

    async fn invalidate_file(&self, _sha256: &str) -> Result<()> {
        Ok(())
    }

    async fn get_run_summary(
        &self,
        _repo_id: &str,
        _run_id: &str,
    ) -> Result<Option<AuditRunSummary>> {
        Ok(None)
    }

    async fn set_run_summary(&self, _summary: &AuditRunSummary) -> Result<()> {
        Ok(())
    }

    async fn get_latest_run(&self, _repo_id: &str) -> Result<Option<AuditRunSummary>> {
        Ok(None)
    }

    async fn stats(&self) -> Result<AuditCacheStats> {
        Ok(AuditCacheStats::default())
    }

    async fn flush(&self) -> Result<usize> {
        Ok(0)
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // -------------------------------------------------------------------------
    // AuditCacheConfig key generation
    // -------------------------------------------------------------------------

    #[test]
    fn test_file_key_format() {
        let cfg = AuditCacheConfig::default();
        let key = cfg.file_key("abc123");
        assert_eq!(key, "audit:file:abc123");
    }

    #[test]
    fn test_run_key_format() {
        let cfg = AuditCacheConfig::default();
        let key = cfg.run_key("repo42", "2024-01-01T00:00:00Z");
        assert_eq!(key, "audit:repo:repo42:run:2024-01-01T00:00:00Z");
    }

    #[test]
    fn test_latest_key_format() {
        let cfg = AuditCacheConfig::default();
        let key = cfg.latest_key("myrepo");
        assert_eq!(key, "audit:repo:myrepo:latest");
    }

    #[test]
    fn test_custom_prefix() {
        let cfg = AuditCacheConfig {
            key_prefix: "ci".to_string(),
            ..Default::default()
        };
        assert_eq!(cfg.file_key("deadbeef"), "ci:file:deadbeef");
        assert_eq!(cfg.latest_key("r1"), "ci:repo:r1:latest");
    }

    // -------------------------------------------------------------------------
    // AuditFileCacheEntry helpers
    // -------------------------------------------------------------------------

    fn make_entry(high: usize, critical: usize) -> AuditFileCacheEntry {
        let mut counts = HashMap::new();
        counts.insert("high".to_string(), high);
        counts.insert("critical".to_string(), critical);
        counts.insert("low".to_string(), 2);

        AuditFileCacheEntry {
            content_sha256: "abc123".to_string(),
            file_path: "src/lib.rs".to_string(),
            cached_at: Utc::now(),
            quality_score: 75.0,
            security_score: 80.0,
            finding_counts: counts,
            finding_summaries: vec!["Use after free".to_string()],
            model: "grok-4-turbo".to_string(),
            cost_usd: 0.0012,
        }
    }

    #[test]
    fn test_has_critical_findings_true() {
        let entry = make_entry(1, 0);
        assert!(entry.has_critical_findings());
    }

    #[test]
    fn test_has_critical_findings_false() {
        let mut entry = make_entry(0, 0);
        *entry.finding_counts.get_mut("low").unwrap() = 3;
        assert!(!entry.has_critical_findings());
    }

    #[test]
    fn test_total_findings() {
        let entry = make_entry(1, 0);
        // high:1, critical:0, low:2
        assert_eq!(entry.total_findings(), 3);
    }

    // -------------------------------------------------------------------------
    // RunStatus display
    // -------------------------------------------------------------------------

    #[test]
    fn test_run_status_display() {
        assert_eq!(RunStatus::Running.to_string(), "running");
        assert_eq!(RunStatus::Completed.to_string(), "completed");
        assert_eq!(RunStatus::Failed.to_string(), "failed");
        assert_eq!(RunStatus::Cancelled.to_string(), "cancelled");
    }

    // -------------------------------------------------------------------------
    // Serialisation round-trips
    // -------------------------------------------------------------------------

    #[test]
    fn test_file_entry_serde_round_trip() {
        let entry = make_entry(2, 1);
        let json = serde_json::to_string_pretty(&entry).unwrap();
        let back: AuditFileCacheEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(back.content_sha256, "abc123");
        assert_eq!(back.quality_score, 75.0);
        assert_eq!(*back.finding_counts.get("high").unwrap(), 2);
    }

    #[test]
    fn test_run_summary_serde_round_trip() {
        let summary = AuditRunSummary {
            run_id: "run-001".to_string(),
            repo_id: "rustcode".to_string(),
            started_at: Utc::now(),
            completed_at: Some(Utc::now()),
            files_audited: 42,
            files_from_cache: 38,
            files_failed: 0,
            avg_quality_score: 78.5,
            avg_security_score: 82.0,
            total_cost_usd: 0.045,
            critical_findings: 3,
            status: RunStatus::Completed,
        };

        let json = serde_json::to_string_pretty(&summary).unwrap();
        let back: AuditRunSummary = serde_json::from_str(&json).unwrap();
        assert_eq!(back.run_id, "run-001");
        assert_eq!(back.files_audited, 42);
        assert_eq!(back.files_from_cache, 38);
        assert_eq!(back.status, RunStatus::Completed);
    }

    #[test]
    fn test_cache_stats_default() {
        let stats = AuditCacheStats::default();
        assert_eq!(stats.file_keys, 0);
        assert_eq!(stats.run_keys, 0);
        assert!(stats.memory_bytes.is_none());
        assert!(stats.hit_rate.is_none());
    }

    // -------------------------------------------------------------------------
    // NoopAuditCache
    // -------------------------------------------------------------------------

    #[tokio::test]
    async fn test_noop_cache_always_misses() {
        let cache = NoopAuditCache;
        let result = cache.get_file_result("doesnotmatter").await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_noop_cache_set_is_ok() {
        let cache = NoopAuditCache;
        let entry = make_entry(0, 0);
        cache.set_file_result(&entry).await.unwrap();
        // Still misses after set — that's the noop contract
        let result = cache.get_file_result("abc123").await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_noop_cache_flush_returns_zero() {
        let cache = NoopAuditCache;
        let count = cache.flush().await.unwrap();
        assert_eq!(count, 0);
    }

    #[tokio::test]
    async fn test_noop_cache_stats() {
        let cache = NoopAuditCache;
        let stats = cache.stats().await.unwrap();
        assert_eq!(stats.file_keys, 0);
    }

    // -------------------------------------------------------------------------
    // RedisAuditCache disabled mode (no real Redis needed)
    // -------------------------------------------------------------------------

    #[tokio::test]
    async fn test_redis_cache_disabled_always_misses() {
        let cache = RedisAuditCache::new(AuditCacheConfig::disabled())
            .await
            .unwrap();
        let result = cache.get_file_result("abc123").await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_redis_cache_disabled_set_is_ok() {
        let cache = RedisAuditCache::new(AuditCacheConfig::disabled())
            .await
            .unwrap();
        let entry = make_entry(0, 0);
        // Should not error even though Redis isn't available
        cache.set_file_result(&entry).await.unwrap();
    }

    #[tokio::test]
    async fn test_redis_cache_disabled_flush_returns_zero() {
        let cache = RedisAuditCache::new(AuditCacheConfig::disabled())
            .await
            .unwrap();
        let count = cache.flush().await.unwrap();
        assert_eq!(count, 0);
    }

    // -------------------------------------------------------------------------
    // encode / decode helpers
    // -------------------------------------------------------------------------

    #[test]
    fn test_encode_decode_round_trip() {
        let entry = make_entry(1, 1);
        let bytes = RedisAuditCache::encode(&entry).unwrap();
        let back: AuditFileCacheEntry = RedisAuditCache::decode(&bytes).unwrap();
        assert_eq!(back.content_sha256, entry.content_sha256);
        assert_eq!(back.total_findings(), entry.total_findings());
    }

    #[test]
    fn test_decode_bad_bytes_returns_error() {
        let bad = b"this is not valid json!!!";
        let result = RedisAuditCache::decode::<AuditFileCacheEntry>(bad);
        assert!(result.is_err());
    }
}
