//! Rate limiting middleware using token bucket algorithm

use axum::{
    extract::{Request, State},
    http::StatusCode,
    middleware::Next,
    response::{IntoResponse, Response},
};
use chrono::{DateTime, Utc};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;

// ============================================================================
// Configuration
// ============================================================================

/// Rate limit configuration
#[derive(Debug, Clone)]
pub struct RateLimitConfig {
    /// Maximum requests per window
    pub max_requests: u32,
    /// Time window in seconds
    pub window_seconds: u64,
    /// Whether to enable rate limiting
    pub enabled: bool,
}

impl Default for RateLimitConfig {
    fn default() -> Self {
        Self {
            max_requests: 100,
            window_seconds: 60,
            enabled: true,
        }
    }
}

impl RateLimitConfig {
    pub fn new(max_requests: u32, window_seconds: u64) -> Self {
        Self {
            max_requests,
            window_seconds,
            enabled: true,
        }
    }

    pub fn permissive() -> Self {
        Self {
            max_requests: 1000,
            window_seconds: 60,
            enabled: true,
        }
    }

    pub fn strict() -> Self {
        Self {
            max_requests: 20,
            window_seconds: 60,
            enabled: true,
        }
    }

    pub fn disabled() -> Self {
        Self {
            max_requests: 0,
            window_seconds: 0,
            enabled: false,
        }
    }
}

// ============================================================================
// Token Bucket
// ============================================================================

/// Token bucket for rate limiting
#[derive(Debug, Clone)]
struct TokenBucket {
    tokens: f64,
    last_refill: DateTime<Utc>,
    capacity: f64,
    refill_rate: f64, // tokens per second
}

impl TokenBucket {
    fn new(capacity: u32, refill_rate: f64) -> Self {
        Self {
            tokens: capacity as f64,
            last_refill: Utc::now(),
            capacity: capacity as f64,
            refill_rate,
        }
    }

    /// Refill tokens based on elapsed time
    fn refill(&mut self) {
        let now = Utc::now();
        let elapsed = (now - self.last_refill).num_milliseconds() as f64 / 1000.0;

        if elapsed > 0.0 {
            let new_tokens = elapsed * self.refill_rate;
            self.tokens = (self.tokens + new_tokens).min(self.capacity);
            self.last_refill = now;
        }
    }

    /// Try to consume a token
    fn try_consume(&mut self) -> bool {
        self.refill();

        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }

    /// Get remaining tokens
    fn remaining(&mut self) -> u32 {
        self.refill();
        self.tokens.floor() as u32
    }

    /// Get time until next token (in seconds)
    fn time_until_next_token(&self) -> f64 {
        if self.tokens >= 1.0 {
            0.0
        } else {
            (1.0 - self.tokens) / self.refill_rate
        }
    }
}

// ============================================================================
// Rate Limiter
// ============================================================================

/// Rate limiter state
pub struct RateLimiter {
    config: RateLimitConfig,
    buckets: Arc<Mutex<HashMap<String, TokenBucket>>>,
}

impl RateLimiter {
    pub fn new(config: RateLimitConfig) -> Self {
        Self {
            config,
            buckets: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Check if request is allowed for the given identifier
    pub async fn check_rate_limit(&self, identifier: &str) -> RateLimitResult {
        if !self.config.enabled {
            return RateLimitResult::Allowed {
                remaining: u32::MAX,
                reset_after: 0,
            };
        }

        let mut buckets = self.buckets.lock().await;

        // Get or create bucket
        let bucket = buckets.entry(identifier.to_string()).or_insert_with(|| {
            let refill_rate = self.config.max_requests as f64 / self.config.window_seconds as f64;
            TokenBucket::new(self.config.max_requests, refill_rate)
        });

        // Try to consume a token
        if bucket.try_consume() {
            let remaining = bucket.remaining();
            RateLimitResult::Allowed {
                remaining,
                reset_after: 0,
            }
        } else {
            let retry_after = bucket.time_until_next_token().ceil() as u64;
            RateLimitResult::RateLimited { retry_after }
        }
    }

    /// Clean up old buckets
    pub async fn cleanup(&self) {
        let mut buckets = self.buckets.lock().await;
        let cutoff = Utc::now() - chrono::Duration::seconds(self.config.window_seconds as i64 * 2);

        buckets.retain(|_, bucket| bucket.last_refill > cutoff);
    }

    /// Get stats
    pub async fn get_stats(&self) -> RateLimitStats {
        let buckets = self.buckets.lock().await;
        RateLimitStats {
            total_clients: buckets.len(),
            config: self.config.clone(),
        }
    }
}

/// Rate limit check result
#[derive(Debug, Clone)]
pub enum RateLimitResult {
    Allowed { remaining: u32, reset_after: u64 },
    RateLimited { retry_after: u64 },
}

/// Rate limiter stats
#[derive(Debug, Clone)]
pub struct RateLimitStats {
    pub total_clients: usize,
    pub config: RateLimitConfig,
}

// ============================================================================
// Middleware
// ============================================================================

/// Rate limiting middleware
pub async fn rate_limit_middleware(
    State(limiter): State<Arc<RateLimiter>>,
    request: Request,
    next: Next,
) -> Response {
    // Extract identifier (IP address or API key)
    let identifier = extract_identifier(&request);

    // Check rate limit
    match limiter.check_rate_limit(&identifier).await {
        RateLimitResult::Allowed { remaining, .. } => {
            let mut response = next.run(request).await;

            // Add rate limit headers
            let headers = response.headers_mut();
            headers.insert(
                "X-RateLimit-Limit",
                limiter.config.max_requests.to_string().parse().unwrap(),
            );
            headers.insert(
                "X-RateLimit-Remaining",
                remaining.to_string().parse().unwrap(),
            );
            headers.insert(
                "X-RateLimit-Window",
                limiter.config.window_seconds.to_string().parse().unwrap(),
            );

            response
        }
        RateLimitResult::RateLimited { retry_after } => {
            let mut response = (
                StatusCode::TOO_MANY_REQUESTS,
                format!("Rate limit exceeded. Retry after {} seconds", retry_after),
            )
                .into_response();

            // Add retry-after header
            let headers = response.headers_mut();
            headers.insert("Retry-After", retry_after.to_string().parse().unwrap());
            headers.insert(
                "X-RateLimit-Limit",
                limiter.config.max_requests.to_string().parse().unwrap(),
            );
            headers.insert("X-RateLimit-Remaining", "0".parse().unwrap());

            response
        }
    }
}

/// Extract identifier from request
fn extract_identifier(request: &Request) -> String {
    // Try to get API key from headers first
    if let Some(api_key) = request
        .headers()
        .get("X-API-Key")
        .or_else(|| request.headers().get("Authorization"))
        .and_then(|v| v.to_str().ok())
    {
        return format!("key:{}", api_key);
    }

    // Fall back to IP address
    if let Some(forwarded) = request
        .headers()
        .get("X-Forwarded-For")
        .and_then(|v| v.to_str().ok())
    {
        if let Some(ip) = forwarded.split(',').next() {
            return format!("ip:{}", ip.trim());
        }
    }

    if let Some(real_ip) = request
        .headers()
        .get("X-Real-IP")
        .and_then(|v| v.to_str().ok())
    {
        return format!("ip:{}", real_ip);
    }

    // Default identifier
    "unknown".to_string()
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_token_bucket_consume() {
        let mut bucket = TokenBucket::new(10, 1.0);

        // Should be able to consume initial tokens
        assert!(bucket.try_consume());
        assert_eq!(bucket.remaining(), 9);

        // Consume all tokens
        for _ in 0..9 {
            assert!(bucket.try_consume());
        }

        // Should fail when empty
        assert!(!bucket.try_consume());
        assert_eq!(bucket.remaining(), 0);
    }

    #[test]
    fn test_token_bucket_refill() {
        use std::thread;
        use std::time::Duration;

        let mut bucket = TokenBucket::new(5, 2.0); // 2 tokens per second

        // Consume all tokens
        for _ in 0..5 {
            assert!(bucket.try_consume());
        }
        assert_eq!(bucket.remaining(), 0);

        // Wait for refill (simulate 1 second)
        thread::sleep(Duration::from_secs(1));

        // Should have refilled approximately 2 tokens
        let remaining = bucket.remaining();
        assert!((1..=3).contains(&remaining)); // Allow some variance
    }

    #[tokio::test]
    async fn test_rate_limiter() {
        let config = RateLimitConfig::new(5, 60);
        let limiter = RateLimiter::new(config);

        // Should allow initial requests
        for i in 0..5 {
            match limiter.check_rate_limit("test_client").await {
                RateLimitResult::Allowed { remaining, .. } => {
                    assert_eq!(remaining, 4 - i);
                }
                RateLimitResult::RateLimited { .. } => panic!("Should not be rate limited"),
            }
        }

        // 6th request should be rate limited
        match limiter.check_rate_limit("test_client").await {
            RateLimitResult::Allowed { .. } => panic!("Should be rate limited"),
            RateLimitResult::RateLimited { retry_after } => {
                assert!(retry_after > 0);
            }
        }
    }

    #[tokio::test]
    async fn test_rate_limiter_disabled() {
        let config = RateLimitConfig::disabled();
        let limiter = RateLimiter::new(config);

        // Should always allow when disabled
        for _ in 0..100 {
            match limiter.check_rate_limit("test_client").await {
                RateLimitResult::Allowed { remaining, .. } => {
                    assert_eq!(remaining, u32::MAX);
                }
                RateLimitResult::RateLimited { .. } => {
                    panic!("Should not rate limit when disabled")
                }
            }
        }
    }

    #[tokio::test]
    async fn test_rate_limiter_per_client() {
        let config = RateLimitConfig::new(3, 60);
        let limiter = RateLimiter::new(config);

        // Client 1 consumes their limit
        for _ in 0..3 {
            limiter.check_rate_limit("client1").await;
        }

        // Client 1 should be rate limited
        match limiter.check_rate_limit("client1").await {
            RateLimitResult::RateLimited { .. } => {}
            _ => panic!("Client 1 should be rate limited"),
        }

        // Client 2 should still be allowed
        match limiter.check_rate_limit("client2").await {
            RateLimitResult::Allowed { .. } => {}
            _ => panic!("Client 2 should not be rate limited"),
        }
    }

    #[tokio::test]
    async fn test_cleanup() {
        let config = RateLimitConfig::new(5, 1);
        let limiter = RateLimiter::new(config);

        // Add some clients
        limiter.check_rate_limit("client1").await;
        limiter.check_rate_limit("client2").await;

        let stats = limiter.get_stats().await;
        assert_eq!(stats.total_clients, 2);

        // Cleanup should not remove recent buckets
        limiter.cleanup().await;
        let stats = limiter.get_stats().await;
        assert_eq!(stats.total_clients, 2);
    }
}
