// Caching Layer Module
//
// Provides multi-tier caching for API responses and search results.
// Supports in-memory LRU cache and optional Redis backend.
//
// # Features
//
// - **In-Memory Cache**: Fast LRU cache for hot data
// - **Redis Support**: Optional distributed caching
// - **TTL Support**: Time-based expiration
// - **Cache Invalidation**: Manual and automatic invalidation
// - **Statistics**: Cache hit/miss tracking
//
// # Example
//
// ```rust,no_run
// use rustcode::cache_layer::{CacheLayer, CacheConfig};
//
// # async fn example() -> anyhow::Result<()> {
// let config = CacheConfig::default();
// let cache = CacheLayer::new(config).await?;
//
// // Set value (must be Serialize; use a String, not a &str literal)
// cache.set("key", &"value".to_string(), Some(3600)).await?;
//
// // Get value
// if let Some(value) = cache.get::<String>("key").await? {
//     println!("Cached value: {}", value);
// }
//
// // Get stats (stats() is async)
// let stats = cache.stats().await;
// println!("Hits: {}, Misses: {}", stats.hits, stats.misses);
// # Ok(())
// # }
// ```

use anyhow::{Context, Result};
use redis::AsyncCommands;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::collections::HashMap;
use std::hash::Hash;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::RwLock;

type RedisPool = deadpool_redis::Pool;

// ============================================================================
// Configuration
// ============================================================================

// Cache configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheConfig {
    // Maximum number of items in memory cache
    pub max_memory_items: usize,

    // Default TTL in seconds (None = no expiration)
    pub default_ttl: Option<u64>,

    // Enable Redis backend
    pub enable_redis: bool,

    // Redis connection URL
    pub redis_url: Option<String>,

    // Redis key prefix
    pub redis_prefix: String,

    // Enable cache statistics
    pub enable_stats: bool,
}

impl Default for CacheConfig {
    fn default() -> Self {
        Self {
            max_memory_items: 1000,
            default_ttl: Some(3600), // 1 hour
            enable_redis: false,
            redis_url: None,
            redis_prefix: "rustcode:".to_string(),
            enable_stats: true,
        }
    }
}

impl CacheConfig {
    // Create configuration for development (memory-only)
    pub fn development() -> Self {
        Self {
            max_memory_items: 500,
            default_ttl: Some(300), // 5 minutes
            enable_redis: false,
            redis_url: None,
            redis_prefix: "dev:".to_string(),
            enable_stats: true,
        }
    }

    // Create configuration for production (with Redis)
    pub fn production(redis_url: String) -> Self {
        Self {
            max_memory_items: 5000,
            default_ttl: Some(3600),
            enable_redis: true,
            redis_url: Some(redis_url),
            redis_prefix: "prod:".to_string(),
            enable_stats: true,
        }
    }
}

// ============================================================================
// Cache Entry
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CacheEntry<T> {
    value: T,
    created_at: u64,
    expires_at: Option<u64>,
    access_count: u64,
    last_accessed: u64,
}

impl<T> CacheEntry<T> {
    fn new(value: T, ttl: Option<u64>) -> Self {
        let now = now_timestamp();
        Self {
            value,
            created_at: now,
            expires_at: ttl.map(|t| now + t),
            access_count: 0,
            last_accessed: now,
        }
    }

    fn is_expired(&self) -> bool {
        if let Some(expires_at) = self.expires_at {
            now_timestamp() >= expires_at
        } else {
            false
        }
    }

    fn access(&mut self) -> &T {
        self.access_count += 1;
        self.last_accessed = now_timestamp();
        &self.value
    }
}

// ============================================================================
// LRU Cache
// ============================================================================

struct LRUCache<K, V> {
    capacity: usize,
    map: HashMap<K, CacheEntry<V>>,
    access_order: Vec<K>,
}

impl<K: Clone + Eq + Hash, V> LRUCache<K, V> {
    fn new(capacity: usize) -> Self {
        Self {
            capacity,
            map: HashMap::new(),
            access_order: Vec::new(),
        }
    }

    fn get(&mut self, key: &K) -> Option<&V> {
        // Check expiry first without holding a mutable borrow
        let is_expired = self.map.get(key).map(|e| e.is_expired()).unwrap_or(false);

        if is_expired {
            self.map.remove(key);
            self.access_order.retain(|k| k != key);
            return None;
        }

        if let Some(entry) = self.map.get_mut(key) {
            // Update access order
            self.access_order.retain(|k| k != key);
            self.access_order.push(key.clone());

            Some(entry.access())
        } else {
            None
        }
    }

    fn set(&mut self, key: K, value: V, ttl: Option<u64>) {
        // Remove if exists
        if self.map.contains_key(&key) {
            self.access_order.retain(|k| k != &key);
        }

        // Evict if at capacity
        while self.map.len() >= self.capacity {
            if let Some(oldest) = self.access_order.first().cloned() {
                self.map.remove(&oldest);
                self.access_order.remove(0);
            } else {
                break;
            }
        }

        // Insert new entry
        self.map.insert(key.clone(), CacheEntry::new(value, ttl));
        self.access_order.push(key);
    }

    fn remove(&mut self, key: &K) -> bool {
        if self.map.remove(key).is_some() {
            self.access_order.retain(|k| k != key);
            true
        } else {
            false
        }
    }

    fn clear(&mut self) {
        self.map.clear();
        self.access_order.clear();
    }

    fn len(&self) -> usize {
        self.map.len()
    }

    fn cleanup_expired(&mut self) {
        let expired_keys: Vec<_> = self
            .map
            .iter()
            .filter(|(_, entry)| entry.is_expired())
            .map(|(k, _)| k.clone())
            .collect();

        for key in expired_keys {
            self.remove(&key);
        }
    }
}

// ============================================================================
// Cache Statistics
// ============================================================================

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CacheStats {
    pub hits: u64,
    pub misses: u64,
    pub sets: u64,
    pub evictions: u64,
    pub memory_items: usize,
    pub redis_items: usize,
}

impl CacheStats {
    pub fn hit_rate(&self) -> f64 {
        let total = self.hits + self.misses;
        if total == 0 {
            0.0
        } else {
            self.hits as f64 / total as f64
        }
    }

    pub fn total_requests(&self) -> u64 {
        self.hits + self.misses
    }
}

// ============================================================================
// Cache Layer
// ============================================================================

// Multi-tier caching layer
pub struct CacheLayer {
    config: CacheConfig,
    memory_cache: Arc<RwLock<LRUCache<String, Vec<u8>>>>,
    stats: Arc<RwLock<CacheStats>>,
    redis_pool: Option<RedisPool>,
}

impl CacheLayer {
    // Create a new cache layer
    pub async fn new(config: CacheConfig) -> Result<Self> {
        let memory_cache = Arc::new(RwLock::new(LRUCache::new(config.max_memory_items)));
        let stats = Arc::new(RwLock::new(CacheStats::default()));

        // Initialize Redis pool if enabled
        let redis_pool = if config.enable_redis {
            if let Some(ref redis_url) = config.redis_url {
                let cfg = deadpool_redis::Config::from_url(redis_url);
                let pool = cfg
                    .create_pool(Some(deadpool_redis::Runtime::Tokio1))
                    .context("Failed to create Redis pool")?;
                Some(pool)
            } else {
                None
            }
        } else {
            None
        };

        let cache = Self {
            config,
            memory_cache,
            stats,
            redis_pool,
        };

        // Start background cleanup task
        cache.start_cleanup_task();

        Ok(cache)
    }

    // Get a value from cache
    pub async fn get<T: DeserializeOwned + serde::Serialize>(
        &self,
        key: &str,
    ) -> Result<Option<T>> {
        // Try memory cache first
        let mut memory = self.memory_cache.write().await;
        if let Some(bytes) = memory.get(&key.to_string()) {
            if self.config.enable_stats {
                let mut stats = self.stats.write().await;
                stats.hits += 1;
            }

            let value: T =
                bincode::deserialize(bytes).context("Failed to deserialize cached value")?;
            return Ok(Some(value));
        }
        drop(memory);

        // Record miss
        if self.config.enable_stats {
            let mut stats = self.stats.write().await;
            stats.misses += 1;
        }

        // Try Redis if enabled
        if self.config.enable_redis {
            if let Some(value) = self.get_from_redis::<T>(key).await? {
                // Store in memory cache for faster access
                let bytes = bincode::serialize(&value).context("Failed to serialize value")?;
                let mut memory = self.memory_cache.write().await;
                memory.set(key.to_string(), bytes, self.config.default_ttl);
                return Ok(Some(value));
            }
        }

        Ok(None)
    }

    // Set a value in cache
    pub async fn set<T: Serialize>(
        &self,
        key: &str,
        value: &T,
        ttl_seconds: Option<u64>,
    ) -> Result<()> {
        let bytes = bincode::serialize(value).context("Failed to serialize value")?;

        let ttl = ttl_seconds.or(self.config.default_ttl);

        // Set in memory cache
        {
            let mut memory = self.memory_cache.write().await;
            memory.set(key.to_string(), bytes.clone(), ttl);
        }

        // Update stats
        if self.config.enable_stats {
            let mut stats = self.stats.write().await;
            stats.sets += 1;
            stats.memory_items = self.memory_cache.read().await.len();
        }

        // Set in Redis if enabled
        if self.config.enable_redis {
            self.set_in_redis(key, &bytes, ttl).await?;
            if self.config.enable_stats {
                let mut stats = self.stats.write().await;
                stats.redis_items += 1;
            }
        }

        Ok(())
    }

    // Delete a value from cache
    pub async fn delete(&self, key: &str) -> Result<bool> {
        let mut memory = self.memory_cache.write().await;
        let removed = memory.remove(&key.to_string());

        // Delete from Redis if enabled
        if self.config.enable_redis {
            self.delete_from_redis(key).await?;
        }

        Ok(removed)
    }

    // Clear all cached values
    pub async fn clear(&self) -> Result<()> {
        let mut memory = self.memory_cache.write().await;
        memory.clear();

        // Clear Redis if enabled
        if self.config.enable_redis {
            self.clear_redis().await?;
        }

        Ok(())
    }

    // Get cache statistics
    pub async fn stats(&self) -> CacheStats {
        let mut stats = self.stats.read().await.clone();
        stats.memory_items = self.memory_cache.read().await.len();
        stats
    }

    // Reset statistics
    pub async fn reset_stats(&self) {
        let mut stats = self.stats.write().await;
        *stats = CacheStats::default();
    }

    // Start background cleanup task
    fn start_cleanup_task(&self) {
        let memory_cache = self.memory_cache.clone();

        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(60));
            loop {
                interval.tick().await;
                let mut cache = memory_cache.write().await;
                cache.cleanup_expired();
            }
        });
    }

    // Get or set with a function
    pub async fn get_or_set<T, F, Fut>(
        &self,
        key: &str,
        f: F,
        ttl_seconds: Option<u64>,
    ) -> Result<T>
    where
        T: Serialize + DeserializeOwned,
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<T>>,
    {
        // Try to get from cache
        if let Some(value) = self.get(key).await? {
            return Ok(value);
        }

        // Compute value
        let value = f().await?;

        // Store in cache
        self.set(key, &value, ttl_seconds).await?;

        Ok(value)
    }

    // Invalidate cache by pattern (prefix match)
    pub async fn invalidate_pattern(&self, pattern: &str) -> Result<usize> {
        let mut memory = self.memory_cache.write().await;
        let keys_to_remove: Vec<_> = memory
            .map
            .keys()
            .filter(|k| k.starts_with(pattern))
            .cloned()
            .collect();

        let count = keys_to_remove.len();
        for key in keys_to_remove {
            memory.remove(&key);
        }

        // Also invalidate in Redis
        if self.config.enable_redis {
            let redis_count = self.invalidate_redis_pattern(pattern).await?;
            return Ok(count + redis_count);
        }

        Ok(count)
    }

    // ========================================================================
    // Redis Operations
    // ========================================================================

    // Get value from Redis
    async fn get_from_redis<T: DeserializeOwned>(&self, key: &str) -> Result<Option<T>> {
        let pool = match &self.redis_pool {
            Some(pool) => pool,
            None => return Ok(None),
        };

        let mut conn = pool.get().await.context("Failed to get Redis connection")?;
        let full_key = self.redis_key(key);

        let bytes: Option<Vec<u8>> = conn
            .get(&full_key)
            .await
            .context("Failed to get value from Redis")?;

        match bytes {
            Some(b) => {
                let value: T =
                    bincode::deserialize(&b).context("Failed to deserialize Redis value")?;
                Ok(Some(value))
            }
            None => Ok(None),
        }
    }

    // Set value in Redis
    async fn set_in_redis(&self, key: &str, bytes: &[u8], ttl: Option<u64>) -> Result<()> {
        let pool = match &self.redis_pool {
            Some(pool) => pool,
            None => return Ok(()),
        };

        let mut conn = pool.get().await.context("Failed to get Redis connection")?;
        let full_key = self.redis_key(key);

        if let Some(seconds) = ttl {
            conn.set_ex::<_, _, ()>(&full_key, bytes, seconds)
                .await
                .context("Failed to set value in Redis with TTL")?;
        } else {
            conn.set::<_, _, ()>(&full_key, bytes)
                .await
                .context("Failed to set value in Redis")?;
        }

        Ok(())
    }

    // Delete value from Redis
    async fn delete_from_redis(&self, key: &str) -> Result<bool> {
        let pool = match &self.redis_pool {
            Some(pool) => pool,
            None => return Ok(false),
        };

        let mut conn = pool.get().await.context("Failed to get Redis connection")?;
        let full_key = self.redis_key(key);

        let removed: bool = conn
            .del(&full_key)
            .await
            .context("Failed to delete value from Redis")?;

        Ok(removed)
    }

    // Clear all Redis keys with our prefix
    async fn clear_redis(&self) -> Result<()> {
        let pool = match &self.redis_pool {
            Some(pool) => pool,
            None => return Ok(()),
        };

        let mut conn = pool.get().await.context("Failed to get Redis connection")?;
        let pattern = format!("{}*", self.config.redis_prefix);

        // Use SCAN to find all keys with our prefix
        let mut cursor = 0;
        loop {
            let (next_cursor, keys): (u64, Vec<String>) = redis::cmd("SCAN")
                .arg(cursor)
                .arg("MATCH")
                .arg(&pattern)
                .arg("COUNT")
                .arg(100)
                .query_async(&mut conn)
                .await
                .context("Failed to scan Redis keys")?;

            if !keys.is_empty() {
                conn.del::<_, ()>(&keys)
                    .await
                    .context("Failed to delete Redis keys")?;
            }

            if next_cursor == 0 {
                break;
            }
            cursor = next_cursor;
        }

        Ok(())
    }

    // Invalidate Redis keys by pattern
    async fn invalidate_redis_pattern(&self, pattern: &str) -> Result<usize> {
        let pool = match &self.redis_pool {
            Some(pool) => pool,
            None => return Ok(0),
        };

        let mut conn = pool.get().await.context("Failed to get Redis connection")?;
        let search_pattern = format!("{}{}", self.config.redis_prefix, pattern);
        let mut total_deleted = 0;
        let mut cursor = 0;

        loop {
            let (next_cursor, keys): (u64, Vec<String>) = redis::cmd("SCAN")
                .arg(cursor)
                .arg("MATCH")
                .arg(format!("{}*", search_pattern))
                .arg("COUNT")
                .arg(100)
                .query_async(&mut conn)
                .await
                .context("Failed to scan Redis keys")?;

            if !keys.is_empty() {
                total_deleted += keys.len();
                conn.del::<_, ()>(&keys)
                    .await
                    .context("Failed to delete Redis keys")?;
            }

            if next_cursor == 0 {
                break;
            }
            cursor = next_cursor;
        }

        Ok(total_deleted)
    }

    // Generate full Redis key with prefix
    fn redis_key(&self, key: &str) -> String {
        format!("{}{}", self.config.redis_prefix, key)
    }
}

// ============================================================================
// Utility Functions
// ============================================================================

fn now_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

// ============================================================================
// Cache Key Builders
// ============================================================================

// Helper for building cache keys
pub struct CacheKey;

impl CacheKey {
    pub fn search(query: &str, limit: usize, filters_hash: &str) -> String {
        format!("search:{}:{}:{}", query, limit, filters_hash)
    }

    pub fn document(id: i64) -> String {
        format!("doc:{}", id)
    }

    pub fn document_chunks(id: i64) -> String {
        format!("doc:{}:chunks", id)
    }

    pub fn stats() -> String {
        "stats".to_string()
    }

    pub fn job_status(job_id: &str) -> String {
        format!("job:{}", job_id)
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_lru_cache() {
        let mut cache = LRUCache::new(2);

        cache.set("key1".to_string(), "value1".to_string(), None);
        cache.set("key2".to_string(), "value2".to_string(), None);

        assert_eq!(cache.get(&"key1".to_string()), Some(&"value1".to_string()));
        assert_eq!(cache.len(), 2);

        // Should evict key2 (least recently used)
        cache.set("key3".to_string(), "value3".to_string(), None);
        assert_eq!(cache.len(), 2);
        assert!(cache.get(&"key2".to_string()).is_none());
        assert!(cache.get(&"key1".to_string()).is_some());
    }

    #[test]
    fn test_cache_expiration() {
        let mut cache = LRUCache::new(10);

        cache.set("key1".to_string(), "value1".to_string(), Some(1)); // Expires in 1 second

        // Should return None for expired entry
        std::thread::sleep(std::time::Duration::from_secs(2));
        assert!(cache.get(&"key1".to_string()).is_none());
    }

    #[tokio::test]
    async fn test_cache_layer() {
        let config = CacheConfig::default();
        let cache = CacheLayer::new(config).await.unwrap();

        // Set and get
        cache
            .set("test_key", &"test_value", Some(3600))
            .await
            .unwrap();
        let value: Option<String> = cache.get("test_key").await.unwrap();
        assert_eq!(value, Some("test_value".to_string()));

        // Stats
        let stats = cache.stats().await;
        assert_eq!(stats.hits, 1);
        assert_eq!(stats.sets, 1);
    }

    #[tokio::test]
    async fn test_cache_get_or_set() {
        let config = CacheConfig::default();
        let cache = CacheLayer::new(config).await.unwrap();

        let mut call_count = 0;

        // First call should compute
        let value = cache
            .get_or_set(
                "compute_key",
                || async {
                    call_count += 1;
                    Ok::<String, anyhow::Error>("computed".to_string())
                },
                Some(3600),
            )
            .await
            .unwrap();

        assert_eq!(value, "computed");
        assert_eq!(call_count, 1);

        // Second call should use cache
        let value2 = cache
            .get_or_set(
                "compute_key",
                || async {
                    call_count += 1;
                    Ok::<String, anyhow::Error>("computed2".to_string())
                },
                Some(3600),
            )
            .await
            .unwrap();

        assert_eq!(value2, "computed");
        assert_eq!(call_count, 1); // Should not increment
    }

    #[tokio::test]
    async fn test_invalidate_pattern() {
        let config = CacheConfig::default();
        let cache = CacheLayer::new(config).await.unwrap();

        cache
            .set("user:1:profile", &"profile1", None)
            .await
            .unwrap();
        cache
            .set("user:1:settings", &"settings1", None)
            .await
            .unwrap();
        cache
            .set("user:2:profile", &"profile2", None)
            .await
            .unwrap();

        let count = cache.invalidate_pattern("user:1:").await.unwrap();
        assert_eq!(count, 2);

        assert!(
            cache
                .get::<String>("user:1:profile")
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            cache
                .get::<String>("user:2:profile")
                .await
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn test_cache_key_builders() {
        assert_eq!(CacheKey::document(123), "doc:123");
        assert_eq!(CacheKey::document_chunks(456), "doc:456:chunks");
        assert_eq!(CacheKey::job_status("abc"), "job:abc");
    }
}
