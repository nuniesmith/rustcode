// GitHub Search Module
//
// Unified search interface across repositories, issues, PRs, and commits.
// Implements intelligent query routing to minimize LLM costs by answering
// queries directly from the local GitHub cache.
//
// # Architecture
//
// This module follows the "cost optimization" pattern - search operations
// query the local SQLite database (FREE) instead of calling expensive LLMs.
// Only when semantic understanding is required do we escalate to LLM APIs.
//
// # Example
//
// ```rust,no_run
// use rustcode::github::{GitHubClient, SyncEngine, search::*};
// use sqlx::PgPool;
//
// #[tokio::main]
// async fn main() -> anyhow::Result<()> {
//     let pool = PgPool::connect(&std::env::var("DATABASE_URL").unwrap_or_else(|_| "postgresql://rustcode:changeme@localhost:5432/rustcode".to_string())).await?;
//     let searcher = GitHubSearcher::new(pool);
//
//     // Search across all GitHub data
//     let query = SearchQuery::new("authentication bug")
//         .with_type(SearchType::Issues)
//         .only_open();
//
//     let results = searcher.search(query).await?;
//     println!("Found {} results", results.len());
//
//     Ok(())
// }
// ```

use crate::github::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Row};
use tracing::{debug, info};

// ============================================================================
// Search Types
// ============================================================================

// Type of search to perform
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SearchType {
    // Search repositories
    Repositories,

    // Search issues
    Issues,

    // Search pull requests
    PullRequests,

    // Search commits
    Commits,

    // Search everything
    All,
}

impl std::fmt::Display for SearchType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SearchType::Repositories => write!(f, "repositories"),
            SearchType::Issues => write!(f, "issues"),
            SearchType::PullRequests => write!(f, "pull_requests"),
            SearchType::Commits => write!(f, "commits"),
            SearchType::All => write!(f, "all"),
        }
    }
}

// ============================================================================
// Search Query
// ============================================================================

// Search query builder
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchQuery {
    // Search text
    pub text: String,

    // Type of search
    pub search_type: SearchType,

    // Filter by repository
    pub repository: Option<String>,

    // Filter by state (open/closed)
    pub state: Option<String>,

    // Filter by language
    pub language: Option<String>,

    // Filter by author/user
    pub author: Option<String>,

    // Filter by label
    pub labels: Vec<String>,

    // Date range filters
    pub created_after: Option<DateTime<Utc>>,
    pub created_before: Option<DateTime<Utc>>,
    pub updated_after: Option<DateTime<Utc>>,
    pub updated_before: Option<DateTime<Utc>>,

    // Limit results
    pub limit: Option<i32>,

    // Offset for pagination
    pub offset: Option<i32>,

    // Sort by field
    pub sort_by: Option<SortField>,

    // Sort order
    pub sort_order: SortOrder,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SortField {
    Created,
    Updated,
    Stars,
    Relevance,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SortOrder {
    Asc,
    Desc,
}

impl SearchQuery {
    // Create new search query
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            search_type: SearchType::All,
            repository: None,
            state: None,
            language: None,
            author: None,
            labels: Vec::new(),
            created_after: None,
            created_before: None,
            updated_after: None,
            updated_before: None,
            limit: Some(100),
            offset: None,
            sort_by: Some(SortField::Relevance),
            sort_order: SortOrder::Desc,
        }
    }

    // Set search type
    pub fn with_type(mut self, search_type: SearchType) -> Self {
        self.search_type = search_type;
        self
    }

    // Filter by repository
    pub fn in_repo(mut self, repo: impl Into<String>) -> Self {
        self.repository = Some(repo.into());
        self
    }

    // Only open items
    pub fn only_open(mut self) -> Self {
        self.state = Some("open".to_string());
        self
    }

    // Only closed items
    pub fn only_closed(mut self) -> Self {
        self.state = Some("closed".to_string());
        self
    }

    // Filter by language
    pub fn with_language(mut self, lang: impl Into<String>) -> Self {
        self.language = Some(lang.into());
        self
    }

    // Filter by author
    pub fn by_author(mut self, author: impl Into<String>) -> Self {
        self.author = Some(author.into());
        self
    }

    // Add label filter
    pub fn with_label(mut self, label: impl Into<String>) -> Self {
        self.labels.push(label.into());
        self
    }

    // Set result limit
    pub fn limit(mut self, limit: i32) -> Self {
        self.limit = Some(limit);
        self
    }

    // Set pagination offset
    pub fn offset(mut self, offset: i32) -> Self {
        self.offset = Some(offset);
        self
    }

    // Sort by field
    pub fn sort_by(mut self, field: SortField, order: SortOrder) -> Self {
        self.sort_by = Some(field);
        self.sort_order = order;
        self
    }
}

// ============================================================================
// Search Results
// ============================================================================

// Search result item
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum SearchResult {
    Repository(RepositoryResult),
    Issue(IssueResult),
    PullRequest(PullRequestResult),
    Commit(CommitResult),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepositoryResult {
    pub id: i64,
    pub name: String,
    pub full_name: String,
    pub description: Option<String>,
    pub language: Option<String>,
    pub html_url: String,
    pub stars: i32,
    pub forks: i32,
    pub open_issues: i32,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IssueResult {
    pub id: i64,
    pub repo_full_name: String,
    pub number: i32,
    pub title: String,
    pub body: Option<String>,
    pub state: String,
    pub user_login: String,
    pub labels: Vec<String>,
    pub html_url: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PullRequestResult {
    pub id: i64,
    pub repo_full_name: String,
    pub number: i32,
    pub title: String,
    pub body: Option<String>,
    pub state: String,
    pub draft: bool,
    pub merged: bool,
    pub user_login: String,
    pub labels: Vec<String>,
    pub html_url: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommitResult {
    pub sha: String,
    pub repo_full_name: String,
    pub author_name: String,
    pub message: String,
    pub additions: Option<i32>,
    pub deletions: Option<i32>,
    pub html_url: String,
    pub author_date: DateTime<Utc>,
}

// ============================================================================
// GitHub Searcher
// ============================================================================

// GitHub search engine
pub struct GitHubSearcher {
    pool: PgPool,
}

impl GitHubSearcher {
    // Create new searcher
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    // Execute search query
    pub async fn search(&self, query: SearchQuery) -> Result<Vec<SearchResult>> {
        info!("Executing search: {:?}", query);

        match query.search_type {
            SearchType::Repositories => self.search_repositories(&query).await,
            SearchType::Issues => self.search_issues(&query).await,
            SearchType::PullRequests => self.search_pull_requests(&query).await,
            SearchType::Commits => self.search_commits(&query).await,
            SearchType::All => self.search_all(&query).await,
        }
    }

    // Search repositories
    async fn search_repositories(&self, query: &SearchQuery) -> Result<Vec<SearchResult>> {
        let mut sql = String::from(
            r#"
            SELECT id, name, full_name, description, language, html_url,
                   stargazers_count, forks_count, open_issues_count, updated_at
            FROM github_repositories
            WHERE 1=1
            "#,
        );

        // Build Postgres $N-style bindings using String params
        let mut string_params: Vec<String> = Vec::new();
        let mut param_idx: u32 = 1;

        // Text search — uses iLIKE for case-insensitive matching in Postgres
        if !query.text.is_empty() {
            let pattern = format!("%{}%", query.text);
            sql.push_str(&format!(
                " AND (name ILIKE ${} OR description ILIKE ${} OR full_name ILIKE ${})",
                param_idx,
                param_idx + 1,
                param_idx + 2
            ));
            string_params.push(pattern.clone());
            string_params.push(pattern.clone());
            string_params.push(pattern);
            param_idx += 3;
        }

        // Language filter
        if let Some(ref lang) = query.language {
            sql.push_str(&format!(" AND language = ${}", param_idx));
            string_params.push(lang.clone());
            param_idx += 1;
        }

        // Sorting
        match query.sort_by {
            Some(SortField::Stars) => sql.push_str(" ORDER BY stargazers_count"),
            Some(SortField::Updated) => sql.push_str(" ORDER BY updated_at"),
            Some(SortField::Created) => sql.push_str(" ORDER BY created_at"),
            _ => sql.push_str(" ORDER BY stargazers_count"),
        }

        match query.sort_order {
            SortOrder::Asc => sql.push_str(" ASC"),
            SortOrder::Desc => sql.push_str(" DESC"),
        }

        // Limit
        if let Some(limit) = query.limit {
            sql.push_str(&format!(" LIMIT {}", limit));
        }

        let _ = param_idx; // suppress unused warning
        debug!("Repository search SQL: {}", sql);

        let mut q = sqlx::query(&sql);
        for p in &string_params {
            q = q.bind(p.as_str());
        }
        let rows = q.fetch_all(&self.pool).await?;

        let results = rows
            .into_iter()
            .map(|row| {
                let updated_timestamp: i64 = row.get(9);
                SearchResult::Repository(RepositoryResult {
                    id: row.get(0),
                    name: row.get(1),
                    full_name: row.get(2),
                    description: row.get(3),
                    language: row.get(4),
                    html_url: row.get(5),
                    stars: row.get(6),
                    forks: row.get(7),
                    open_issues: row.get(8),
                    updated_at: DateTime::from_timestamp(updated_timestamp, 0)
                        .unwrap_or_else(Utc::now),
                })
            })
            .collect();

        Ok(results)
    }

    // Search issues
    async fn search_issues(&self, query: &SearchQuery) -> Result<Vec<SearchResult>> {
        let mut sql = String::from(
            r#"
            SELECT i.id, r.full_name, i.number, i.title, i.body, i.state,
                   i.user_login, i.labels, i.html_url, i.created_at, i.updated_at
            FROM github_issues i
            JOIN github_repositories r ON i.repo_id = r.id
            WHERE i.is_pull_request = 0
            "#,
        );

        let mut string_params: Vec<String> = Vec::new();
        let mut param_idx: u32 = 1;

        // Text search
        if !query.text.is_empty() {
            let pattern = format!("%{}%", query.text);
            sql.push_str(&format!(
                " AND (i.title ILIKE ${} OR i.body ILIKE ${})",
                param_idx,
                param_idx + 1
            ));
            string_params.push(pattern.clone());
            string_params.push(pattern);
            param_idx += 2;
        }

        // State filter
        if let Some(ref state) = query.state {
            sql.push_str(&format!(" AND i.state = '{}'", state));
        }

        // Repository filter
        if let Some(ref repo) = query.repository {
            sql.push_str(&format!(" AND r.full_name = '{}'", repo));
        }

        // Author filter
        if let Some(ref author) = query.author {
            sql.push_str(&format!(" AND i.user_login = '{}'", author));
        }

        // Sorting
        sql.push_str(" ORDER BY i.updated_at DESC");

        // Limit
        if let Some(limit) = query.limit {
            sql.push_str(&format!(" LIMIT {}", limit));
        }

        debug!("Issue search SQL: {}", sql);

        let _ = param_idx; // suppress unused warning
        debug!("Issue search SQL: {}", sql);

        let mut q = sqlx::query(&sql);
        for p in &string_params {
            q = q.bind(p.as_str());
        }
        let rows = q.fetch_all(&self.pool).await?;

        let results = rows
            .into_iter()
            .map(|row| {
                let labels_json: String = row.get(7);
                let labels: Vec<String> = serde_json::from_str(&labels_json).unwrap_or_default();
                let created_timestamp: i64 = row.get(9);
                let updated_timestamp: i64 = row.get(10);

                SearchResult::Issue(IssueResult {
                    id: row.get(0),
                    repo_full_name: row.get(1),
                    number: row.get(2),
                    title: row.get(3),
                    body: row.get(4),
                    state: row.get(5),
                    user_login: row.get(6),
                    labels,
                    html_url: row.get(8),
                    created_at: DateTime::from_timestamp(created_timestamp, 0)
                        .unwrap_or_else(Utc::now),
                    updated_at: DateTime::from_timestamp(updated_timestamp, 0)
                        .unwrap_or_else(Utc::now),
                })
            })
            .collect();

        Ok(results)
    }

    // Search pull requests
    async fn search_pull_requests(&self, query: &SearchQuery) -> Result<Vec<SearchResult>> {
        let mut sql = String::from(
            r#"
            SELECT p.id, r.full_name, p.number, p.title, p.body, p.state,
                   p.draft, p.merged, p.user_login, p.labels, p.html_url,
                   p.created_at, p.updated_at
            FROM github_pull_requests p
            JOIN github_repositories r ON p.repo_id = r.id
            WHERE 1=1
            "#,
        );

        // Text search
        if !query.text.is_empty() {
            sql.push_str(" AND (p.title LIKE ? OR p.body LIKE ?)");
        }

        // State filter
        if let Some(ref state) = query.state {
            sql.push_str(&format!(" AND p.state = '{}'", state));
        }

        // Repository filter
        if let Some(ref repo) = query.repository {
            sql.push_str(&format!(" AND r.full_name = '{}'", repo));
        }

        // Author filter
        if let Some(ref author) = query.author {
            sql.push_str(&format!(" AND p.user_login = '{}'", author));
        }

        // Sorting
        sql.push_str(" ORDER BY p.updated_at DESC");

        // Limit
        if let Some(limit) = query.limit {
            sql.push_str(&format!(" LIMIT {}", limit));
        }

        let rows = if !query.text.is_empty() {
            let pattern = format!("%{}%", query.text);
            sqlx::query(&sql)
                .bind(&pattern)
                .bind(&pattern)
                .fetch_all(&self.pool)
                .await?
        } else {
            sqlx::query(&sql).fetch_all(&self.pool).await?
        };

        let results = rows
            .into_iter()
            .map(|row| {
                let labels_json: String = row.get(9);
                let labels: Vec<String> = serde_json::from_str(&labels_json).unwrap_or_default();
                let created_timestamp: i64 = row.get(11);
                let updated_timestamp: i64 = row.get(12);

                SearchResult::PullRequest(PullRequestResult {
                    id: row.get(0),
                    repo_full_name: row.get(1),
                    number: row.get(2),
                    title: row.get(3),
                    body: row.get(4),
                    state: row.get(5),
                    draft: row.get::<i32, _>(6) != 0,
                    merged: row.get::<i32, _>(7) != 0,
                    user_login: row.get(8),
                    labels,
                    html_url: row.get(10),
                    created_at: DateTime::from_timestamp(created_timestamp, 0)
                        .unwrap_or_else(Utc::now),
                    updated_at: DateTime::from_timestamp(updated_timestamp, 0)
                        .unwrap_or_else(Utc::now),
                })
            })
            .collect();

        Ok(results)
    }

    // Search commits
    async fn search_commits(&self, query: &SearchQuery) -> Result<Vec<SearchResult>> {
        let mut sql = String::from(
            r#"
            SELECT c.sha, r.full_name, c.author_name, c.message,
                   c.additions, c.deletions, c.html_url, c.author_date
            FROM github_commits c
            JOIN github_repositories r ON c.repo_id = r.id
            WHERE 1=1
            "#,
        );

        // Text search
        if !query.text.is_empty() {
            sql.push_str(" AND (c.message LIKE ? OR c.author_name LIKE ?)");
        }

        // Repository filter
        if let Some(ref repo) = query.repository {
            sql.push_str(&format!(" AND r.full_name = '{}'", repo));
        }

        // Author filter
        if let Some(ref author) = query.author {
            sql.push_str(&format!(" AND c.author_name LIKE '%{}%'", author));
        }

        // Sorting
        sql.push_str(" ORDER BY c.author_date DESC");

        // Limit
        if let Some(limit) = query.limit {
            sql.push_str(&format!(" LIMIT {}", limit));
        }

        debug!("Commit search SQL: {}", sql);

        let rows = if !query.text.is_empty() {
            let pattern = format!("%{}%", query.text);
            sqlx::query(&sql)
                .bind(&pattern)
                .bind(&pattern)
                .fetch_all(&self.pool)
                .await?
        } else {
            sqlx::query(&sql).fetch_all(&self.pool).await?
        };

        let results = rows
            .into_iter()
            .map(|row| {
                let author_timestamp: i64 = row.get(7);

                SearchResult::Commit(CommitResult {
                    sha: row.get(0),
                    repo_full_name: row.get(1),
                    author_name: row.get(2),
                    message: row.get(3),
                    additions: row.get(4),
                    deletions: row.get(5),
                    html_url: row.get(6),
                    author_date: DateTime::from_timestamp(author_timestamp, 0)
                        .unwrap_or_else(Utc::now),
                })
            })
            .collect();

        Ok(results)
    }

    // Search all types
    async fn search_all(&self, query: &SearchQuery) -> Result<Vec<SearchResult>> {
        let mut results = Vec::new();

        // Search each type with limited results
        let limited_query = query.clone();

        if let Ok(repos) = self.search_repositories(&limited_query).await {
            results.extend(repos);
        }

        if let Ok(issues) = self.search_issues(&limited_query).await {
            results.extend(issues);
        }

        if let Ok(prs) = self.search_pull_requests(&limited_query).await {
            results.extend(prs);
        }

        if let Ok(commits) = self.search_commits(&limited_query).await {
            results.extend(commits);
        }

        // Apply global limit
        if let Some(limit) = query.limit {
            results.truncate(limit as usize);
        }

        Ok(results)
    }

    // Get statistics about synced GitHub data
    pub async fn get_stats(&self) -> Result<GitHubStats> {
        let repos = sqlx::query("SELECT COUNT(*) FROM github_repositories")
            .fetch_one(&self.pool)
            .await?
            .get::<i32, _>(0);

        let issues = sqlx::query("SELECT COUNT(*) FROM github_issues WHERE is_pull_request = 0")
            .fetch_one(&self.pool)
            .await?
            .get::<i32, _>(0);

        let prs = sqlx::query("SELECT COUNT(*) FROM github_pull_requests")
            .fetch_one(&self.pool)
            .await?
            .get::<i32, _>(0);

        let commits = sqlx::query("SELECT COUNT(*) FROM github_commits")
            .fetch_one(&self.pool)
            .await?
            .get::<i32, _>(0);

        let open_issues = sqlx::query(
            "SELECT COUNT(*) FROM github_issues WHERE state = 'open' AND is_pull_request = 0",
        )
        .fetch_one(&self.pool)
        .await?
        .get::<i32, _>(0);

        let open_prs =
            sqlx::query("SELECT COUNT(*) FROM github_pull_requests WHERE state = 'open'")
                .fetch_one(&self.pool)
                .await?
                .get::<i32, _>(0);

        Ok(GitHubStats {
            total_repos: repos,
            total_issues: issues,
            total_prs: prs,
            total_commits: commits,
            open_issues,
            open_prs,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitHubStats {
    pub total_repos: i32,
    pub total_issues: i32,
    pub total_prs: i32,
    pub total_commits: i32,
    pub open_issues: i32,
    pub open_prs: i32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_search_query_builder() {
        let query = SearchQuery::new("test")
            .with_type(SearchType::Issues)
            .in_repo("owner/repo")
            .only_open()
            .limit(50);

        assert_eq!(query.text, "test");
        assert_eq!(query.search_type, SearchType::Issues);
        assert_eq!(query.repository, Some("owner/repo".to_string()));
        assert_eq!(query.state, Some("open".to_string()));
        assert_eq!(query.limit, Some(50));
    }

    #[test]
    fn test_search_type_display() {
        assert_eq!(SearchType::Repositories.to_string(), "repositories");
        assert_eq!(SearchType::Issues.to_string(), "issues");
        assert_eq!(SearchType::PullRequests.to_string(), "pull_requests");
        assert_eq!(SearchType::All.to_string(), "all");
    }
}
