// GitHub API client and domain models.
//
// Pure HTTP client side of `rustcode`'s GitHub integration: no database,
// no queue, no application config. The DB-coupled pieces (`sync`,
// `background_sync`, `search`, `webhook`) live in the `rustcode` crate
// under `src/github/` and consume the types defined here.
//
// # Quick start
//
// ```rust,no_run
// use github_client::{GitHubClient, GitHubError};
//
// #[tokio::main]
// async fn main() -> Result<(), GitHubError> {
//     let client = GitHubClient::new("ghp_your_token")?;
//     let limits = client.get_rate_limit().await?;
//     println!("Remaining: {}/{}", limits.resources.core.remaining, limits.resources.core.limit);
//     Ok(())
// }
// ```

pub mod client;
mod error;
pub mod models;

pub use client::{GitHubClient, GitHubConfig, RateLimitInfo};
pub use error::{GitHubError, Result};
pub use models::{
    Commit, CommitStatus, Issue, IssueState, Label, PrState, PullRequest, Repository,
    RepositoryVisibility, User,
};
