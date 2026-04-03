//! GitHub Integration Module
//!
//! Provides comprehensive GitHub API integration for rustcode, enabling:
//! - Repository synchronization and tracking
//! - Issue and PR management
//! - Commit history and activity tracking
//! - Webhook support for real-time updates
//! - Cost-free GitHub API usage (vs expensive LLM calls)
//!
//! # Architecture
//!
//! This module follows the "crates vs services" pattern from the architectural
//! research, providing clean abstractions for GitHub operations:
//!
//! - `client`: Low-level GitHub API client (GraphQL + REST)
//! - `models`: Type-safe domain models for GitHub entities
//! - `sync`: Bidirectional synchronization with local database
//! - `webhook`: Event-driven updates from GitHub
//! - `search`: Unified search across repos, issues, PRs
//!
//! # Cost Optimization
//!
//! GitHub API calls are FREE (rate-limited to 5000/hour for authenticated users).
//! This module implements the query router pattern to prefer GitHub API over LLM
//! calls whenever possible, achieving significant cost savings.
//!
//! # Example Usage
//!
//! ```rust,no_run
//! use rustcode::github::{GitHubClient, SyncEngine};
//! use sqlx::PgPool;
//!
//! #[tokio::main]
//! async fn main() -> anyhow::Result<()> {
//!     // Initialize client with PAT
//!     let client = GitHubClient::new("ghp_your_token_here")?;
//!     let pool = PgPool::connect(&std::env::var("DATABASE_URL").unwrap_or_else(|_| "postgresql://rustcode:changeme@localhost:5432/rustcode".to_string())).await?;
//!
//!     // Sync all user repositories
//!     let sync = SyncEngine::new(client, pool);
//!     let result = sync.sync_all_repos().await?;
//!     println!("Synced {} repositories", result.repos_synced);
//!
//!     // Incremental sync (only changes since last sync)
//!     let result = sync.sync_incremental().await?;
//!     println!("Updated {} items", result.items_updated);
//!
//!     Ok(())
//! }
//! ```

pub mod background_sync;
pub mod client;
pub mod models;
pub mod search;
pub mod sync;
pub mod webhook;

// Re-export commonly used types for convenience
pub use background_sync::{
    start_background_sync, start_background_sync_with_config, BackgroundSyncConfig,
    BackgroundSyncManager,
};
pub use client::{GitHubClient, GitHubConfig, RateLimitInfo};
pub use models::{
    Commit, CommitStatus, Issue, IssueState, Label, PrState, PullRequest, Repository,
    RepositoryVisibility, User,
};
pub use search::{GitHubSearcher, SearchQuery, SearchResult, SearchType};
pub use sync::{SyncEngine, SyncOptions, SyncResult};
pub use webhook::{WebhookEvent, WebhookHandler, WebhookPayload};

use thiserror::Error;

/// GitHub integration specific errors
#[derive(Error, Debug)]
pub enum GitHubError {
    #[error("GitHub API error: {0}")]
    ApiError(String),

    #[error("Authentication failed: {0}")]
    AuthError(String),

    #[error("Rate limit exceeded. Resets at: {reset_at}")]
    RateLimitExceeded {
        reset_at: chrono::DateTime<chrono::Utc>,
    },

    #[error("Resource not found: {resource_type} with id {id}")]
    NotFound { resource_type: String, id: String },

    #[error("Invalid configuration: {0}")]
    ConfigError(String),

    #[error("Network error: {0}")]
    NetworkError(#[from] reqwest::Error),

    #[error("Database error: {0}")]
    DatabaseError(#[from] sqlx::Error),

    #[error("Serialization error: {0}")]
    SerializationError(#[from] serde_json::Error),

    #[error("Invalid GitHub URL: {0}")]
    InvalidUrl(String),

    #[error("Webhook verification failed")]
    WebhookVerificationFailed,
}

pub type Result<T> = std::result::Result<T, GitHubError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_module_exports() {
        // Ensure all main types are accessible
        let _: Option<GitHubClient> = None;
        let _: Option<Repository> = None;
        let _: Option<Issue> = None;
        let _: Option<PullRequest> = None;
        let _: Option<SyncEngine> = None;
    }
}
