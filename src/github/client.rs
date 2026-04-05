// GitHub API Client
//
// High-performance GitHub client supporting both REST and GraphQL APIs.
// Implements rate limiting, caching, and retry logic for production use.
//
// # Architecture
//
// This client follows the "cost optimization" pattern from the architectural
// research - GitHub API calls are FREE (rate-limited), so we maximize their
// use to avoid expensive LLM calls.
//
// # Features
//
// - Automatic token management and authentication
// - Rate limit tracking and backoff
// - Connection pooling and keep-alive
// - Comprehensive error handling
// - GraphQL support for complex queries
// - REST API for standard operations
//
// # Example
//
// ```rust,no_run
// use rustcode::github::GitHubClient;
//
// #[tokio::main]
// async fn main() -> anyhow::Result<()> {
//     let client = GitHubClient::new("ghp_your_token")?;
//
//     // Fetch user repositories
//     let repos = client.list_user_repos("username").await?;
//
//     // Check rate limit
//     let limits = client.get_rate_limit().await?;
//     println!("Remaining: {}/{}", limits.resources.core.remaining, limits.resources.core.limit);
//
//     Ok(())
// }
// ```

use crate::github::{models::*, GitHubError, Result};
use chrono::{DateTime, Utc};
use reqwest::{
    header::{HeaderMap, HeaderValue, ACCEPT, AUTHORIZATION, USER_AGENT},
    Client, StatusCode,
};
use serde::{Deserialize, Serialize};
use url::form_urlencoded;
use std::time::Duration;
use tracing::{debug, warn};

const GITHUB_API_BASE: &str = "https://api.github.com";
const GITHUB_GRAPHQL_ENDPOINT: &str = "https://api.github.com/graphql";
const DEFAULT_PER_PAGE: u32 = 100;
const MAX_PER_PAGE: u32 = 100;

// ============================================================================
// Client Configuration
// ============================================================================

// GitHub client configuration
#[derive(Debug, Clone)]
pub struct GitHubConfig {
    // Personal Access Token (PAT)
    pub token: String,

    // API base URL (default: https://api.github.com)
    pub base_url: String,

    // GraphQL endpoint
    pub graphql_url: String,

    // Request timeout in seconds
    pub timeout_secs: u64,

    // User agent string
    pub user_agent: String,

    // Enable automatic rate limit handling
    pub auto_rate_limit: bool,

    // Minimum remaining rate limit before warning
    pub rate_limit_warning_threshold: i32,
}

impl Default for GitHubConfig {
    fn default() -> Self {
        Self {
            token: String::new(),
            base_url: GITHUB_API_BASE.to_string(),
            graphql_url: GITHUB_GRAPHQL_ENDPOINT.to_string(),
            timeout_secs: 30,
            user_agent: format!("rustcode/{}", env!("CARGO_PKG_VERSION")),
            auto_rate_limit: true,
            rate_limit_warning_threshold: 100,
        }
    }
}

impl GitHubConfig {
    // Create new config with token
    pub fn new(token: impl Into<String>) -> Self {
        Self {
            token: token.into(),
            ..Default::default()
        }
    }

    // Set custom base URL (for GitHub Enterprise)
    pub fn with_base_url(mut self, url: impl Into<String>) -> Self {
        self.base_url = url.into();
        self
    }

    // Set request timeout
    pub fn with_timeout(mut self, secs: u64) -> Self {
        self.timeout_secs = secs;
        self
    }
}

// ============================================================================
// Rate Limit Info
// ============================================================================

// Rate limit information from response headers
#[derive(Debug, Clone)]
pub struct RateLimitInfo {
    pub limit: i32,
    pub remaining: i32,
    pub reset: DateTime<Utc>,
    pub used: i32,
}

impl RateLimitInfo {
    // Parse rate limit from response headers
    fn from_headers(headers: &HeaderMap) -> Option<Self> {
        let limit = headers
            .get("x-ratelimit-limit")?
            .to_str()
            .ok()?
            .parse()
            .ok()?;

        let remaining = headers
            .get("x-ratelimit-remaining")?
            .to_str()
            .ok()?
            .parse()
            .ok()?;

        let reset_timestamp: i64 = headers
            .get("x-ratelimit-reset")?
            .to_str()
            .ok()?
            .parse()
            .ok()?;

        let used = headers
            .get("x-ratelimit-used")?
            .to_str()
            .ok()?
            .parse()
            .ok()?;

        let reset = DateTime::from_timestamp(reset_timestamp, 0)?;

        Some(Self {
            limit,
            remaining,
            reset,
            used,
        })
    }

    // Check if rate limit is approaching exhaustion
    pub fn is_exhausted(&self, threshold: i32) -> bool {
        self.remaining < threshold
    }
}

// ============================================================================
// GitHub Client
// ============================================================================

// Main GitHub API client
#[derive(Clone)]
pub struct GitHubClient {
    config: GitHubConfig,
    client: Client,
    last_rate_limit: std::sync::Arc<tokio::sync::RwLock<Option<RateLimitInfo>>>,
}

impl GitHubClient {
    // Create new GitHub client with token
    pub fn new(token: impl Into<String>) -> Result<Self> {
        let config = GitHubConfig::new(token);
        Self::with_config(config)
    }

    // Create client with custom configuration
    pub fn with_config(config: GitHubConfig) -> Result<Self> {
        if config.token.is_empty() {
            return Err(GitHubError::ConfigError(
                "GitHub token is required".to_string(),
            ));
        }

        // Build HTTP client with optimizations
        let mut headers = HeaderMap::new();
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {}", config.token))
                .map_err(|e| GitHubError::ConfigError(format!("Invalid token: {}", e)))?,
        );
        headers.insert(
            ACCEPT,
            HeaderValue::from_static("application/vnd.github+json"),
        );
        headers.insert(
            USER_AGENT,
            HeaderValue::from_str(&config.user_agent)
                .map_err(|e| GitHubError::ConfigError(format!("Invalid user agent: {}", e)))?,
        );
        headers.insert(
            "X-GitHub-Api-Version",
            HeaderValue::from_static("2022-11-28"),
        );

        let client = Client::builder()
            .timeout(Duration::from_secs(config.timeout_secs))
            .default_headers(headers)
            .pool_max_idle_per_host(10)
            .pool_idle_timeout(Duration::from_secs(90))
            .tcp_keepalive(Duration::from_secs(60))
            .build()
            .map_err(|e| GitHubError::ConfigError(format!("Failed to build HTTP client: {}", e)))?;

        Ok(Self {
            config,
            client,
            last_rate_limit: std::sync::Arc::new(tokio::sync::RwLock::new(None)),
        })
    }

    // Get current rate limit info (cached)
    pub async fn get_cached_rate_limit(&self) -> Option<RateLimitInfo> {
        self.last_rate_limit.read().await.clone()
    }

    // Update rate limit from headers
    async fn update_rate_limit(&self, headers: &HeaderMap) {
        if let Some(rate_limit) = RateLimitInfo::from_headers(headers) {
            if self.config.auto_rate_limit
                && rate_limit.is_exhausted(self.config.rate_limit_warning_threshold)
            {
                warn!(
                    "GitHub API rate limit approaching: {}/{}",
                    rate_limit.remaining, rate_limit.limit
                );
            }
            *self.last_rate_limit.write().await = Some(rate_limit);
        }
    }

    // Make authenticated GET request
    async fn get<T: for<'de> Deserialize<'de>>(&self, path: &str) -> Result<T> {
        let url = format!("{}{}", self.config.base_url, path);
        debug!("GET {}", url);

        let response = self.client.get(&url).send().await?;

        // Update rate limit tracking
        self.update_rate_limit(response.headers()).await;

        let status = response.status();
        if !status.is_success() {
            return Err(self.handle_error_response(status, response).await);
        }

        let data = response.json().await?;
        Ok(data)
    }

    // Make authenticated GET request with pagination
    async fn get_paginated<T: for<'de> Deserialize<'de>>(
        &self,
        path: &str,
        per_page: Option<u32>,
    ) -> Result<Vec<T>> {
        let per_page = per_page.unwrap_or(DEFAULT_PER_PAGE).min(MAX_PER_PAGE);
        let mut all_items = Vec::new();
        let mut page = 1;

        loop {
            let url = format!(
                "{}{}?per_page={}&page={}",
                self.config.base_url, path, per_page, page
            );
            debug!("GET {} (page {})", path, page);

            let response = self.client.get(&url).send().await?;
            self.update_rate_limit(response.headers()).await;

            let status = response.status();
            if !status.is_success() {
                return Err(self.handle_error_response(status, response).await);
            }

            let items: Vec<T> = response.json().await?;
            if items.is_empty() {
                break;
            }

            all_items.extend(items);
            page += 1;
        }

        Ok(all_items)
    }

    // Make authenticated POST request
    async fn post<T: for<'de> Deserialize<'de>, B: Serialize>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<T> {
        let url = format!("{}{}", self.config.base_url, path);
        debug!("POST {}", url);

        let response = self.client.post(&url).json(body).send().await?;
        self.update_rate_limit(response.headers()).await;

        let status = response.status();
        if !status.is_success() {
            return Err(self.handle_error_response(status, response).await);
        }

        let data = response.json().await?;
        Ok(data)
    }

    // Make GraphQL query
    #[allow(dead_code)]
    async fn graphql<T: for<'de> Deserialize<'de>>(
        &self,
        query: &str,
        variables: Option<serde_json::Value>,
    ) -> Result<T> {
        #[derive(Serialize)]
        struct GraphQLRequest<'a> {
            query: &'a str,
            #[serde(skip_serializing_if = "Option::is_none")]
            variables: Option<serde_json::Value>,
        }

        #[derive(Deserialize)]
        struct GraphQLResponse<T> {
            data: Option<T>,
            errors: Option<Vec<GraphQLError>>,
        }

        #[derive(Deserialize)]
        struct GraphQLError {
            message: String,
        }

        let request = GraphQLRequest { query, variables };

        debug!("GraphQL query: {}", query);

        let response = self
            .client
            .post(&self.config.graphql_url)
            .json(&request)
            .send()
            .await?;

        self.update_rate_limit(response.headers()).await;

        let result: GraphQLResponse<T> = response.json().await?;

        if let Some(errors) = result.errors {
            let error_msg = errors
                .iter()
                .map(|e| e.message.clone())
                .collect::<Vec<_>>()
                .join(", ");
            return Err(GitHubError::ApiError(format!(
                "GraphQL errors: {}",
                error_msg
            )));
        }

        result
            .data
            .ok_or_else(|| GitHubError::ApiError("No data in GraphQL response".to_string()))
    }

    // Handle error response
    async fn handle_error_response(
        &self,
        status: StatusCode,
        response: reqwest::Response,
    ) -> GitHubError {
        match status {
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => {
                GitHubError::AuthError("Invalid or expired GitHub token".to_string())
            }
            StatusCode::NOT_FOUND => {
                let body = response.text().await.unwrap_or_default();
                GitHubError::NotFound {
                    resource_type: "resource".to_string(),
                    id: body,
                }
            }
            StatusCode::TOO_MANY_REQUESTS => {
                if let Some(reset) = response.headers().get("x-ratelimit-reset") {
                    if let Ok(timestamp) = reset.to_str().unwrap_or("").parse::<i64>() {
                        if let Some(reset_at) = DateTime::from_timestamp(timestamp, 0) {
                            return GitHubError::RateLimitExceeded { reset_at };
                        }
                    }
                }
                GitHubError::ApiError("Rate limit exceeded".to_string())
            }
            _ => {
                let body = response.text().await.unwrap_or_default();
                GitHubError::ApiError(format!("HTTP {}: {}", status, body))
            }
        }
    }

    // ========================================================================
    // Repository Operations
    // ========================================================================

    // Get authenticated user's repositories
    pub async fn list_user_repos(&self, username: &str) -> Result<Vec<Repository>> {
        self.get_paginated(&format!("/users/{}/repos", username), None)
            .await
    }

    // Get repositories for authenticated user
    pub async fn list_my_repos(&self) -> Result<Vec<Repository>> {
        self.get_paginated("/user/repos", None).await
    }

    // Get a specific repository
    pub async fn get_repo(&self, owner: &str, repo: &str) -> Result<Repository> {
        self.get(&format!("/repos/{}/{}", owner, repo)).await
    }

    // List repository languages
    pub async fn get_repo_languages(
        &self,
        owner: &str,
        repo: &str,
    ) -> Result<std::collections::HashMap<String, i64>> {
        self.get(&format!("/repos/{}/{}/languages", owner, repo))
            .await
    }

    // ========================================================================
    // Issue Operations
    // ========================================================================

    // List issues for a repository
    pub async fn list_issues(
        &self,
        owner: &str,
        repo: &str,
        state: Option<&str>,
    ) -> Result<Vec<Issue>> {
        let state_param = state.unwrap_or("open");
        let per_page = DEFAULT_PER_PAGE.min(MAX_PER_PAGE);
        let mut all_items = Vec::new();
        let mut page = 1;

        loop {
            let url = format!(
                "{}/repos/{}/{}/issues?state={}&per_page={}&page={}",
                self.config.base_url, owner, repo, state_param, per_page, page
            );
            debug!(
                "GET /repos/{}/{}/issues (page {}, state={})",
                owner, repo, page, state_param
            );

            let response = self.client.get(&url).send().await?;
            self.update_rate_limit(response.headers()).await;

            let status = response.status();
            if !status.is_success() {
                return Err(self.handle_error_response(status, response).await);
            }

            let items: Vec<Issue> = response.json().await?;
            if items.is_empty() {
                break;
            }

            all_items.extend(items);
            page += 1;
        }

        Ok(all_items)
    }

    // Get a specific issue
    pub async fn get_issue(&self, owner: &str, repo: &str, number: i32) -> Result<Issue> {
        self.get(&format!("/repos/{}/{}/issues/{}", owner, repo, number))
            .await
    }

    // Create a new issue
    pub async fn create_issue(
        &self,
        owner: &str,
        repo: &str,
        title: &str,
        body: Option<&str>,
        labels: Option<Vec<String>>,
    ) -> Result<Issue> {
        #[derive(Serialize)]
        struct CreateIssueRequest<'a> {
            title: &'a str,
            #[serde(skip_serializing_if = "Option::is_none")]
            body: Option<&'a str>,
            #[serde(skip_serializing_if = "Option::is_none")]
            labels: Option<Vec<String>>,
        }

        let request = CreateIssueRequest {
            title,
            body,
            labels,
        };
        self.post(&format!("/repos/{}/{}/issues", owner, repo), &request)
            .await
    }

    // ========================================================================
    // Pull Request Operations
    // ========================================================================

    // List pull requests for a repository
    pub async fn list_pull_requests(
        &self,
        owner: &str,
        repo: &str,
        state: Option<&str>,
    ) -> Result<Vec<PullRequest>> {
        let state_param = state.unwrap_or("open");
        let per_page = DEFAULT_PER_PAGE.min(MAX_PER_PAGE);
        let mut all_items = Vec::new();
        let mut page = 1;

        loop {
            let url = format!(
                "{}/repos/{}/{}/pulls?state={}&per_page={}&page={}",
                self.config.base_url, owner, repo, state_param, per_page, page
            );
            debug!(
                "GET /repos/{}/{}/pulls (page {}, state={})",
                owner, repo, page, state_param
            );

            let response = self.client.get(&url).send().await?;
            self.update_rate_limit(response.headers()).await;

            let status = response.status();
            if !status.is_success() {
                return Err(self.handle_error_response(status, response).await);
            }

            let items: Vec<PullRequest> = response.json().await?;
            if items.is_empty() {
                break;
            }

            all_items.extend(items);
            page += 1;
        }

        Ok(all_items)
    }

    // Get a specific pull request
    pub async fn get_pull_request(
        &self,
        owner: &str,
        repo: &str,
        number: i32,
    ) -> Result<PullRequest> {
        self.get(&format!("/repos/{}/{}/pulls/{}", owner, repo, number))
            .await
    }

    // ========================================================================
    // Commit Operations
    // ========================================================================

    // List commits for a repository
    pub async fn list_commits(
        &self,
        owner: &str,
        repo: &str,
        per_page: Option<u32>,
    ) -> Result<Vec<Commit>> {
        self.get_paginated(&format!("/repos/{}/{}/commits", owner, repo), per_page)
            .await
    }

    // Get a specific commit
    pub async fn get_commit(&self, owner: &str, repo: &str, sha: &str) -> Result<Commit> {
        self.get(&format!("/repos/{}/{}/commits/{}", owner, repo, sha))
            .await
    }

    // ========================================================================
    // Rate Limit Operations
    // ========================================================================

    // Get current rate limit status
    pub async fn get_rate_limit(&self) -> Result<RateLimitResponse> {
        self.get("/rate_limit").await
    }

    // ========================================================================
    // Search Operations
    // ========================================================================

    // Search repositories
    pub async fn search_repositories(&self, query: &str) -> Result<SearchResponse<Repository>> {
        self.get(&format!(
            "/search/repositories?q={}",
            urlencoding::encode(query)
        ))
        .await
    }

    // Search issues
    pub async fn search_issues(&self, query: &str) -> Result<SearchResponse<Issue>> {
        self.get(&format!("/search/issues?q={}", urlencoding::encode(query)))
            .await
    }

    // ========================================================================
    // User Operations
    // ========================================================================

    // Get authenticated user
    pub async fn get_authenticated_user(&self) -> Result<User> {
        self.get("/user").await
    }

    // Get a specific user
    pub async fn get_user(&self, username: &str) -> Result<User> {
        self.get(&format!("/users/{}", username)).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_default() {
        let config = GitHubConfig::default();
        assert_eq!(config.base_url, GITHUB_API_BASE);
        assert_eq!(config.timeout_secs, 30);
        assert!(config.auto_rate_limit);
    }

    #[test]
    fn test_config_builder() {
        let config = GitHubConfig::new("test_token")
            .with_base_url("https://github.enterprise.com")
            .with_timeout(60);

        assert_eq!(config.token, "test_token");
        assert_eq!(config.base_url, "https://github.enterprise.com");
        assert_eq!(config.timeout_secs, 60);
    }

    #[test]
    fn test_rate_limit_exhausted() {
        let rate_limit = RateLimitInfo {
            limit: 5000,
            remaining: 50,
            reset: Utc::now(),
            used: 4950,
        };

        assert!(rate_limit.is_exhausted(100));
        assert!(!rate_limit.is_exhausted(10));
    }

    #[tokio::test]
    async fn test_client_creation_fails_without_token() {
        let config = GitHubConfig::default();
        let result = GitHubClient::with_config(config);
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_client_creation_success() {
        let result = GitHubClient::new("ghp_test_token");
        assert!(result.is_ok());
    }
}
