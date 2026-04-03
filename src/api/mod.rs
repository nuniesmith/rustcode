//! API module for RAG system
//!
//! Provides REST API endpoints for:
//! - Document management (upload, read, update, delete)
//! - Semantic search (hybrid, semantic-only, keyword)
//! - Background indexing with job queue
//! - Authentication and rate limiting
//! - System statistics and health checks

pub mod admin;
pub mod auth;
pub mod handlers;
pub mod jobs;
pub mod proxy;
pub mod proxy_client;
pub mod rate_limit;
pub mod repos;
pub mod types;

use axum::{
    Router, middleware,
    routing::{delete, get, post, put},
};
use std::sync::Arc;

use crate::embeddings::{EmbeddingConfig, EmbeddingGenerator};
use crate::indexing::IndexingConfig;
use sqlx::PgPool;

pub use auth::{AuthConfig, AuthResult, generate_api_key, hash_api_key};
pub use handlers::ApiState;
pub use jobs::{JobQueue, JobQueueConfig, JobStatus};
pub use proxy::{ProxyState, proxy_router};
pub use proxy_client::{
    ChatMessage, ChatReply, ChatRequestBuilder, ProxyClient, ProxyClientConfig,
};
pub use rate_limit::{RateLimitConfig, RateLimiter};
pub use types::*;

// ============================================================================
// Router Setup
// ============================================================================

/// Create the API router with all endpoints
pub async fn create_api_router(
    db_pool: PgPool,
    auth_config: AuthConfig,
    rate_limit_config: RateLimitConfig,
    indexing_config: IndexingConfig,
    job_queue_config: JobQueueConfig,
) -> Router {
    // Initialize embedding generator
    let embedding_generator = Arc::new(tokio::sync::Mutex::new(
        EmbeddingGenerator::new(EmbeddingConfig::default()).unwrap(),
    ));

    // Create API state
    let api_state = Arc::new(
        ApiState::new(
            db_pool,
            embedding_generator,
            indexing_config,
            job_queue_config,
        )
        .await,
    );

    // Create rate limiter
    let rate_limiter = Arc::new(RateLimiter::new(rate_limit_config));

    // Create auth config
    let auth_config = Arc::new(auth_config);

    // Build router
    let router = Router::new()
        // Health & Stats
        .route("/health", get(handlers::health_check))
        .route("/stats", get(handlers::get_stats))
        // Documents
        .route("/documents", post(handlers::upload_document))
        .route("/documents", get(handlers::list_documents))
        .route("/documents/{id}", get(handlers::get_document))
        .route("/documents/{id}", put(handlers::update_document))
        .route("/documents/{id}", delete(handlers::delete_document))
        // Search
        .route("/search", post(handlers::search_documents))
        // Indexing
        .route("/index", post(handlers::index_document))
        .route("/index/batch", post(handlers::batch_index_documents))
        .route("/index/jobs", get(handlers::list_index_jobs))
        .route("/index/jobs/{job_id}", get(handlers::get_index_job_status))
        .route(
            "/index/jobs/{job_id}/cancel",
            post(handlers::cancel_index_job),
        )
        .merge(admin::admin_router())
        .with_state(api_state);

    // Apply middleware (rate limiting, then auth)
    router
        .layer(middleware::from_fn_with_state(
            rate_limiter,
            rate_limit::rate_limit_middleware,
        ))
        .layer(middleware::from_fn_with_state(
            auth_config,
            auth::auth_middleware,
        ))
}

/// Create API router with default configuration
pub async fn create_default_api_router(db_pool: PgPool) -> Router {
    create_api_router(
        db_pool,
        AuthConfig::default(),
        RateLimitConfig::default(),
        IndexingConfig::default(),
        JobQueueConfig::default(),
    )
    .await
}

// ============================================================================
// Configuration Helpers
// ============================================================================

/// API configuration builder
#[derive(Default)]
pub struct ApiConfig {
    pub auth: AuthConfig,
    pub rate_limit: RateLimitConfig,
    pub indexing: IndexingConfig,
    pub job_queue: JobQueueConfig,
}

impl ApiConfig {
    /// Create production configuration
    pub fn production() -> Self {
        Self {
            auth: AuthConfig {
                api_keys: Vec::new(),
                require_auth: true,
                allow_anonymous_read: false,
            },
            rate_limit: RateLimitConfig::strict(),
            indexing: IndexingConfig::default(),
            job_queue: JobQueueConfig::default(),
        }
    }

    /// Create development configuration
    pub fn development() -> Self {
        Self {
            auth: AuthConfig::default(),
            rate_limit: RateLimitConfig::permissive(),
            indexing: IndexingConfig::default(),
            job_queue: JobQueueConfig::default(),
        }
    }

    /// Add API key
    pub fn with_api_key(mut self, key: String) -> Self {
        self.auth.add_key(&key);
        self
    }

    /// Set rate limit
    pub fn with_rate_limit(mut self, max_requests: u32, window_seconds: u64) -> Self {
        self.rate_limit = RateLimitConfig::new(max_requests, window_seconds);
        self
    }

    /// Enable anonymous read access
    pub fn allow_anonymous_read(mut self) -> Self {
        self.auth.allow_anonymous_read = true;
        self
    }

    /// Build router with this configuration
    pub async fn build_router(self, db_pool: PgPool) -> Router {
        create_api_router(
            db_pool,
            self.auth,
            self.rate_limit,
            self.indexing,
            self.job_queue,
        )
        .await
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_api_config_builder() {
        let config = ApiConfig::development()
            .with_api_key("test_key".to_string())
            .allow_anonymous_read();

        assert!(config.auth.require_auth);
        assert!(config.auth.allow_anonymous_read);
    }

    #[test]
    fn test_production_config() {
        let config = ApiConfig::production();
        assert!(config.auth.require_auth);
        assert!(!config.auth.allow_anonymous_read);
    }
}
