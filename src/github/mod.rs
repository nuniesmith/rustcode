// GitHub Integration Module
//
// Provides comprehensive GitHub API integration for rustcode, enabling:
// - Repository synchronization and tracking
// - Issue and PR management
// - Commit history and activity tracking
// - Webhook support for real-time updates
// - Cost-free GitHub API usage (vs expensive LLM calls)
//
// # Architecture
//
// The HTTP client side (`client`, `models`, `GitHubError`) lives in the
// standalone `github-client` crate at `crates/github-client/` and is
// re-exported here for backwards compatibility. The DB-coupled pieces
// (`sync`, `background_sync`, `search`, `webhook`) stay in this binary
// crate since they tie into the application's `PgPool` and queue.
//
// # Cost Optimization
//
// GitHub API calls are FREE (rate-limited to 5000/hour for authenticated users).
// This module implements the query router pattern to prefer GitHub API over LLM
// calls whenever possible, achieving significant cost savings.
//
// # Example Usage
//
// ```rust,no_run
// use rustcode::github::{GitHubClient, SyncEngine};
// use sqlx::PgPool;
//
// #[tokio::main]
// async fn main() -> anyhow::Result<()> {
//     // Initialize client with PAT
//     let client = GitHubClient::new("ghp_your_token_here")?;
//     let pool = PgPool::connect(&std::env::var("DATABASE_URL").unwrap_or_else(|_| "postgresql://rustcode:changeme@localhost:5432/rustcode".to_string())).await?;
//
//     // Sync all user repositories
//     let sync = SyncEngine::new(client, pool);
//     let result = sync.sync_all_repos().await?;
//     println!("Synced {} repositories", result.repos_synced);
//
//     // Incremental sync (only changes since last sync)
//     let result = sync.sync_incremental().await?;
//     println!("Updated {} items", result.items_updated);
//
//     Ok(())
// }
// ```

pub mod background_sync;
pub mod search;
pub mod sync;
pub mod webhook;

// Re-export the HTTP client surface from the standalone crate. Module
// paths (`crate::github::client`, `crate::github::models`) keep resolving
// for the in-tree consumers that use the full path.
pub use github_client::{
    Commit, CommitStatus, GitHubClient, GitHubConfig, GitHubError, Issue, IssueState, Label,
    PrState, PullRequest, RateLimitInfo, Repository, RepositoryVisibility, Result, User,
};
pub use github_client::{client, models};

pub use background_sync::{
    BackgroundSyncConfig, BackgroundSyncManager, start_background_sync,
    start_background_sync_with_config,
};
pub use search::{GitHubSearcher, SearchQuery, SearchResult, SearchType};
pub use sync::{SyncEngine, SyncOptions, SyncResult};
pub use webhook::{WebhookEvent, WebhookHandler, WebhookPayload};

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
