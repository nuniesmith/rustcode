// GitHub Synchronization Engine
//
// Provides bidirectional synchronization between GitHub and the local database.
// This module implements the "cost optimization" pattern by caching GitHub data
// locally and minimizing redundant API calls.
//
// # Architecture
//
// The sync engine follows these principles:
// - **Incremental Sync**: Only fetch data that changed since last sync
// - **Bidirectional**: Support both GitHub → Local and Local → GitHub flows
// - **Cost-Free**: GitHub API is free (rate-limited), maximizing its use
// - **Event-Driven**: React to webhook events for real-time updates
//
// # Example
//
// ```rust,no_run
// use rustcode::github::{GitHubClient, SyncEngine};
// use sqlx::PgPool;
//
// #[tokio::main]
// async fn main() -> anyhow::Result<()> {
//     let client = GitHubClient::new("ghp_token")?;
//     let pool = PgPool::connect(&std::env::var("DATABASE_URL").unwrap_or_else(|_| "postgresql://rustcode:changeme@localhost:5432/rustcode".to_string())).await?;
//
//     let sync = SyncEngine::new(client, pool);
//
//     // Full sync of all repos
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

use crate::github::{Result, client::GitHubClient, models::*};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Row};
use tracing::{debug, error, info, warn};

// ============================================================================
// Sync Configuration
// ============================================================================

// Synchronization options
#[derive(Debug, Clone)]
pub struct SyncOptions {
    // Sync repositories
    pub sync_repos: bool,

    // Sync issues
    pub sync_issues: bool,

    // Sync pull requests
    pub sync_prs: bool,

    // Sync commits (last N per repo)
    pub sync_commits: bool,
    pub commits_limit: u32,

    // Sync labels and milestones
    pub sync_metadata: bool,

    // Force full resync (ignore last_synced timestamps)
    pub force_full: bool,

    // Only sync specific repositories
    pub repo_filter: Option<Vec<String>>,
}

impl Default for SyncOptions {
    fn default() -> Self {
        Self {
            sync_repos: true,
            sync_issues: true,
            sync_prs: true,
            sync_commits: true,
            commits_limit: 100,
            sync_metadata: true,
            force_full: false,
            repo_filter: None,
        }
    }
}

impl SyncOptions {
    // Create minimal sync options (repos only)
    pub fn repos_only() -> Self {
        Self {
            sync_repos: true,
            sync_issues: false,
            sync_prs: false,
            sync_commits: false,
            commits_limit: 0,
            sync_metadata: false,
            force_full: false,
            repo_filter: None,
        }
    }

    // Create full sync options
    pub fn full() -> Self {
        Self::default()
    }

    // Filter to specific repositories
    pub fn with_repos(mut self, repos: Vec<String>) -> Self {
        self.repo_filter = Some(repos);
        self
    }

    // Force full resync
    pub fn force_full(mut self) -> Self {
        self.force_full = true;
        self
    }
}

// ============================================================================
// Sync Results
// ============================================================================

// Result of a synchronization operation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncResult {
    pub started_at: DateTime<Utc>,
    pub completed_at: DateTime<Utc>,
    pub duration_secs: f64,

    pub repos_synced: u32,
    pub issues_synced: u32,
    pub prs_synced: u32,
    pub commits_synced: u32,
    pub labels_synced: u32,
    pub milestones_synced: u32,

    pub items_created: u32,
    pub items_updated: u32,
    pub items_deleted: u32,

    pub errors: Vec<String>,
    pub warnings: Vec<String>,
}

impl SyncResult {
    fn new() -> Self {
        let now = Utc::now();
        Self {
            started_at: now,
            completed_at: now,
            duration_secs: 0.0,
            repos_synced: 0,
            issues_synced: 0,
            prs_synced: 0,
            commits_synced: 0,
            labels_synced: 0,
            milestones_synced: 0,
            items_created: 0,
            items_updated: 0,
            items_deleted: 0,
            errors: Vec::new(),
            warnings: Vec::new(),
        }
    }

    fn finish(&mut self) {
        self.completed_at = Utc::now();
        self.duration_secs =
            (self.completed_at - self.started_at).num_milliseconds() as f64 / 1000.0;
    }

    fn add_error(&mut self, error: String) {
        self.errors.push(error);
    }

    fn add_warning(&mut self, warning: String) {
        self.warnings.push(warning);
    }
}

// ============================================================================
// Sync Engine
// ============================================================================

// GitHub synchronization engine
pub struct SyncEngine {
    client: GitHubClient,
    pool: PgPool,
}

impl SyncEngine {
    // Create new sync engine
    pub fn new(client: GitHubClient, pool: PgPool) -> Self {
        Self { client, pool }
    }

    // Initialize database schema for GitHub data
    pub async fn initialize_schema(&self) -> Result<()> {
        info!("Initializing GitHub sync schema");

        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS github_repositories (
                id INTEGER PRIMARY KEY,
                node_id TEXT NOT NULL,
                name TEXT NOT NULL,
                full_name TEXT NOT NULL UNIQUE,
                owner_login TEXT NOT NULL,
                description TEXT,
                html_url TEXT NOT NULL,
                clone_url TEXT NOT NULL,
                ssh_url TEXT NOT NULL,
                language TEXT,
                private INTEGER NOT NULL,
                fork INTEGER NOT NULL,
                archived INTEGER NOT NULL,
                stargazers_count INTEGER NOT NULL DEFAULT 0,
                watchers_count INTEGER NOT NULL DEFAULT 0,
                forks_count INTEGER NOT NULL DEFAULT 0,
                open_issues_count INTEGER NOT NULL DEFAULT 0,
                topics TEXT,
                default_branch TEXT NOT NULL,
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL,
                pushed_at INTEGER,
                last_synced_at INTEGER NOT NULL,
                sync_enabled INTEGER NOT NULL DEFAULT 1
            )
            "#,
        )
        .execute(&self.pool)
        .await?;

        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS github_issues (
                id INTEGER PRIMARY KEY,
                node_id TEXT NOT NULL,
                repo_id INTEGER NOT NULL,
                number INTEGER NOT NULL,
                title TEXT NOT NULL,
                body TEXT,
                state TEXT NOT NULL,
                user_login TEXT NOT NULL,
                labels TEXT,
                assignees TEXT,
                milestone_id INTEGER,
                comments INTEGER NOT NULL DEFAULT 0,
                locked INTEGER NOT NULL DEFAULT 0,
                html_url TEXT NOT NULL,
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL,
                closed_at INTEGER,
                is_pull_request INTEGER NOT NULL DEFAULT 0,
                last_synced_at INTEGER NOT NULL,
                FOREIGN KEY (repo_id) REFERENCES github_repositories(id),
                UNIQUE(repo_id, number)
            )
            "#,
        )
        .execute(&self.pool)
        .await?;

        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS github_pull_requests (
                id INTEGER PRIMARY KEY,
                node_id TEXT NOT NULL,
                repo_id INTEGER NOT NULL,
                number INTEGER NOT NULL,
                title TEXT NOT NULL,
                body TEXT,
                state TEXT NOT NULL,
                draft INTEGER NOT NULL DEFAULT 0,
                merged INTEGER NOT NULL DEFAULT 0,
                user_login TEXT NOT NULL,
                head_ref TEXT NOT NULL,
                head_sha TEXT NOT NULL,
                base_ref TEXT NOT NULL,
                base_sha TEXT NOT NULL,
                labels TEXT,
                commits INTEGER NOT NULL DEFAULT 0,
                additions INTEGER NOT NULL DEFAULT 0,
                deletions INTEGER NOT NULL DEFAULT 0,
                changed_files INTEGER NOT NULL DEFAULT 0,
                html_url TEXT NOT NULL,
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL,
                closed_at INTEGER,
                merged_at INTEGER,
                last_synced_at INTEGER NOT NULL,
                FOREIGN KEY (repo_id) REFERENCES github_repositories(id),
                UNIQUE(repo_id, number)
            )
            "#,
        )
        .execute(&self.pool)
        .await?;

        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS github_commits (
                sha TEXT PRIMARY KEY,
                node_id TEXT NOT NULL,
                repo_id INTEGER NOT NULL,
                author_name TEXT NOT NULL,
                author_email TEXT NOT NULL,
                author_date INTEGER NOT NULL,
                committer_name TEXT NOT NULL,
                committer_email TEXT NOT NULL,
                message TEXT NOT NULL,
                additions INTEGER,
                deletions INTEGER,
                total_changes INTEGER,
                html_url TEXT NOT NULL,
                created_at INTEGER NOT NULL,
                last_synced_at INTEGER NOT NULL,
                FOREIGN KEY (repo_id) REFERENCES github_repositories(id)
            )
            "#,
        )
        .execute(&self.pool)
        .await?;

        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS github_labels (
                id INTEGER PRIMARY KEY,
                repo_id INTEGER NOT NULL,
                name TEXT NOT NULL,
                color TEXT NOT NULL,
                description TEXT,
                FOREIGN KEY (repo_id) REFERENCES github_repositories(id),
                UNIQUE(repo_id, name)
            )
            "#,
        )
        .execute(&self.pool)
        .await?;

        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS github_milestones (
                id INTEGER PRIMARY KEY,
                repo_id INTEGER NOT NULL,
                number INTEGER NOT NULL,
                title TEXT NOT NULL,
                description TEXT,
                state TEXT NOT NULL,
                open_issues INTEGER NOT NULL DEFAULT 0,
                closed_issues INTEGER NOT NULL DEFAULT 0,
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL,
                due_on INTEGER,
                closed_at INTEGER,
                FOREIGN KEY (repo_id) REFERENCES github_repositories(id),
                UNIQUE(repo_id, number)
            )
            "#,
        )
        .execute(&self.pool)
        .await?;

        // Create indexes for performance
        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_github_repos_owner ON github_repositories(owner_login)",
        )
        .execute(&self.pool)
        .await?;

        sqlx::query("CREATE INDEX IF NOT EXISTS idx_github_issues_repo ON github_issues(repo_id)")
            .execute(&self.pool)
            .await?;

        sqlx::query("CREATE INDEX IF NOT EXISTS idx_github_issues_state ON github_issues(state)")
            .execute(&self.pool)
            .await?;

        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_github_prs_repo ON github_pull_requests(repo_id)",
        )
        .execute(&self.pool)
        .await?;

        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_github_prs_state ON github_pull_requests(state)",
        )
        .execute(&self.pool)
        .await?;

        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_github_commits_repo ON github_commits(repo_id)",
        )
        .execute(&self.pool)
        .await?;

        info!("GitHub sync schema initialized successfully");
        Ok(())
    }

    // Sync all repositories for authenticated user
    pub async fn sync_all_repos(&self) -> Result<SyncResult> {
        self.sync_with_options(SyncOptions::default()).await
    }

    // Sync with custom options
    pub async fn sync_with_options(&self, options: SyncOptions) -> Result<SyncResult> {
        let mut result = SyncResult::new();
        info!("Starting GitHub sync with options: {:?}", options);

        // Step 1: Sync repositories
        if options.sync_repos {
            match self.sync_repositories(&options, &mut result).await {
                Ok(_) => info!("Repository sync completed"),
                Err(e) => {
                    error!("Repository sync failed: {}", e);
                    result.add_error(format!("Repo sync error: {}", e));
                }
            }
        }

        // Step 2: Get list of repos to sync issues/PRs for
        let repos = self.get_synced_repos(&options).await?;
        info!("Found {} repositories to sync", repos.len());

        // Step 3: Sync issues
        if options.sync_issues {
            for (owner, repo_name, repo_id) in &repos {
                match self
                    .sync_issues(owner, repo_name, *repo_id, &mut result)
                    .await
                {
                    Ok(_) => debug!("Synced issues for {}/{}", owner, repo_name),
                    Err(e) => {
                        warn!("Failed to sync issues for {}/{}: {}", owner, repo_name, e);
                        result.add_warning(format!(
                            "Issues sync failed for {}/{}: {}",
                            owner, repo_name, e
                        ));
                    }
                }
            }
        }

        // Step 4: Sync pull requests
        if options.sync_prs {
            for (owner, repo_name, repo_id) in &repos {
                match self
                    .sync_pull_requests(owner, repo_name, *repo_id, &mut result)
                    .await
                {
                    Ok(_) => debug!("Synced PRs for {}/{}", owner, repo_name),
                    Err(e) => {
                        warn!("Failed to sync PRs for {}/{}: {}", owner, repo_name, e);
                        result.add_warning(format!(
                            "PR sync failed for {}/{}: {}",
                            owner, repo_name, e
                        ));
                    }
                }
            }
        }

        // Step 5: Sync commits
        if options.sync_commits {
            for (owner, repo_name, repo_id) in &repos {
                match self
                    .sync_commits(
                        owner,
                        repo_name,
                        *repo_id,
                        options.commits_limit,
                        &mut result,
                    )
                    .await
                {
                    Ok(_) => debug!("Synced commits for {}/{}", owner, repo_name),
                    Err(e) => {
                        warn!("Failed to sync commits for {}/{}: {}", owner, repo_name, e);
                        result.add_warning(format!(
                            "Commit sync failed for {}/{}: {}",
                            owner, repo_name, e
                        ));
                    }
                }
            }
        }

        result.finish();
        info!(
            "GitHub sync completed in {:.2}s: {} repos, {} issues, {} PRs, {} commits",
            result.duration_secs,
            result.repos_synced,
            result.issues_synced,
            result.prs_synced,
            result.commits_synced
        );

        Ok(result)
    }

    // Sync repositories
    async fn sync_repositories(
        &self,
        options: &SyncOptions,
        result: &mut SyncResult,
    ) -> Result<()> {
        info!("Syncing repositories from GitHub");

        // If specific repos are specified, fetch them directly instead of listing all
        if let Some(ref filter) = options.repo_filter {
            info!("Fetching {} specific repositories", filter.len());

            for full_name in filter {
                let parts: Vec<&str> = full_name.split('/').collect();
                if parts.len() != 2 {
                    warn!("Invalid repo format: {}, expected owner/repo", full_name);
                    result.add_warning(format!("Invalid repo format: {}", full_name));
                    continue;
                }

                let (owner, repo_name) = (parts[0], parts[1]);

                match self.client.get_repo(owner, repo_name).await {
                    Ok(repo) => {
                        // Skip archived repos unless force_full
                        if repo.archived && !options.force_full {
                            debug!("Skipping archived repo: {}", repo.full_name);
                            continue;
                        }

                        match self.upsert_repository(&repo).await {
                            Ok(created) => {
                                result.repos_synced += 1;
                                if created {
                                    result.items_created += 1;
                                } else {
                                    result.items_updated += 1;
                                }
                            }
                            Err(e) => {
                                error!("Failed to upsert repository {}: {}", repo.full_name, e);
                                result
                                    .add_error(format!("Failed to save {}: {}", repo.full_name, e));
                            }
                        }
                    }
                    Err(e) => {
                        error!("Failed to fetch repository {}: {}", full_name, e);
                        result.add_error(format!("Failed to fetch {}: {}", full_name, e));
                    }
                }
            }
        } else {
            // No filter - list all repos for authenticated user
            let repos = self.client.list_my_repos().await?;
            info!("Fetched {} repositories from GitHub", repos.len());

            for repo in repos {
                // Skip archived repos unless force_full
                if repo.archived && !options.force_full {
                    debug!("Skipping archived repo: {}", repo.full_name);
                    continue;
                }

                match self.upsert_repository(&repo).await {
                    Ok(created) => {
                        result.repos_synced += 1;
                        if created {
                            result.items_created += 1;
                        } else {
                            result.items_updated += 1;
                        }
                    }
                    Err(e) => {
                        error!("Failed to upsert repository {}: {}", repo.full_name, e);
                        result.add_error(format!("Failed to save {}: {}", repo.full_name, e));
                    }
                }
            }
        }

        Ok(())
    }

    // Upsert a repository into the database
    async fn upsert_repository(&self, repo: &Repository) -> Result<bool> {
        let topics_json = serde_json::to_string(&repo.topics)?;
        let now = Utc::now().timestamp();

        let existing = sqlx::query("SELECT id FROM github_repositories WHERE id = $1")
            .bind(repo.id)
            .fetch_optional(&self.pool)
            .await?;

        let is_new = existing.is_none();

        sqlx::query(
            r#"
            INSERT INTO github_repositories (
                id, node_id, name, full_name, owner_login, owner_id, description,
                html_url, clone_url, ssh_url, language, private, fork, archived,
                stargazers_count, watchers_count, forks_count, open_issues_count,
                topics, default_branch, created_at, updated_at, pushed_at, last_synced_at
            ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18, $19, $20, $21, $22, $23, $24)
            ON CONFLICT(id) DO UPDATE SET
                name = excluded.name,
                description = excluded.description,
                language = excluded.language,
                stargazers_count = excluded.stargazers_count,
                watchers_count = excluded.watchers_count,
                forks_count = excluded.forks_count,
                open_issues_count = excluded.open_issues_count,
                topics = excluded.topics,
                updated_at = excluded.updated_at,
                pushed_at = excluded.pushed_at,
                last_synced_at = excluded.last_synced_at,
                archived = excluded.archived
            "#,
        )
        .bind(repo.id)
        .bind(&repo.node_id)
        .bind(&repo.name)
        .bind(&repo.full_name)
        .bind(&repo.owner.login)
        .bind(repo.owner.id)
        .bind(&repo.description)
        .bind(&repo.html_url)
        .bind(&repo.clone_url)
        .bind(&repo.ssh_url)
        .bind(&repo.language)
        .bind(repo.private as i32)
        .bind(repo.fork as i32)
        .bind(repo.archived as i32)
        .bind(repo.stargazers_count)
        .bind(repo.watchers_count)
        .bind(repo.forks_count)
        .bind(repo.open_issues_count)
        .bind(topics_json)
        .bind(&repo.default_branch)
        .bind(repo.created_at.timestamp())
        .bind(repo.updated_at.timestamp())
        .bind(repo.pushed_at.map(|t| t.timestamp()))
        .bind(now)
        .execute(&self.pool)
        .await?;

        Ok(is_new)
    }

    // Get list of repositories to sync
    async fn get_synced_repos(&self, options: &SyncOptions) -> Result<Vec<(String, String, i64)>> {
        let mut query = String::from(
            "SELECT owner_login, name, id FROM github_repositories WHERE sync_enabled = 1",
        );

        if let Some(ref filter) = options.repo_filter {
            let placeholders = filter.iter().map(|_| "?").collect::<Vec<_>>().join(",");
            query.push_str(&format!(" AND full_name IN ({})", placeholders));
        }

        debug!("Query for synced repos: {}", query);
        debug!("Repo filter: {:?}", options.repo_filter);

        let mut query_builder = sqlx::query(&query);

        if let Some(ref filter) = options.repo_filter {
            for repo in filter {
                query_builder = query_builder.bind(repo);
            }
        }

        let rows = query_builder.fetch_all(&self.pool).await?;

        let repos: Vec<(String, String, i64)> = rows
            .into_iter()
            .map(|row| {
                let owner: String = row.get(0);
                let name: String = row.get(1);
                let id: i64 = row.get(2);
                (owner, name, id)
            })
            .collect();

        info!("Found {} repositories to sync: {:?}", repos.len(), repos);

        Ok(repos)
    }

    // Sync issues for a repository
    async fn sync_issues(
        &self,
        owner: &str,
        repo: &str,
        repo_id: i64,
        result: &mut SyncResult,
    ) -> Result<()> {
        let issues = self.client.list_issues(owner, repo, Some("all")).await?;

        for issue in issues {
            match self.upsert_issue(&issue, repo_id).await {
                Ok(_) => {
                    result.issues_synced += 1;
                }
                Err(e) => {
                    error!("Failed to upsert issue #{}: {}", issue.number, e);
                }
            }
        }

        Ok(())
    }

    // Upsert an issue into the database
    async fn upsert_issue(&self, issue: &Issue, repo_id: i64) -> Result<()> {
        let labels_json =
            serde_json::to_string(&issue.labels.iter().map(|l| &l.name).collect::<Vec<_>>())?;
        let assignees_json =
            serde_json::to_string(&issue.assignees.iter().map(|a| &a.login).collect::<Vec<_>>())?;
        let now = Utc::now().timestamp();

        sqlx::query(
            r#"
            INSERT INTO github_issues (
                id, node_id, repo_id, number, title, body, state, user_login,
                labels, assignees, milestone_id, comments, locked, html_url,
                created_at, updated_at, closed_at, is_pull_request, last_synced_at
            ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18, $19)
            ON CONFLICT(repo_id, number) DO UPDATE SET
                title = excluded.title,
                body = excluded.body,
                state = excluded.state,
                labels = excluded.labels,
                assignees = excluded.assignees,
                comments = excluded.comments,
                updated_at = excluded.updated_at,
                closed_at = excluded.closed_at,
                last_synced_at = excluded.last_synced_at
            "#,
        )
        .bind(issue.id)
        .bind(&issue.node_id)
        .bind(repo_id)
        .bind(issue.number)
        .bind(&issue.title)
        .bind(&issue.body)
        .bind(format!("{:?}", issue.state).to_lowercase())
        .bind(&issue.user.login)
        .bind(labels_json)
        .bind(assignees_json)
        .bind(issue.milestone.as_ref().map(|m| m.id))
        .bind(issue.comments)
        .bind(issue.locked as i32)
        .bind(&issue.html_url)
        .bind(issue.created_at.timestamp())
        .bind(issue.updated_at.timestamp())
        .bind(issue.closed_at.map(|t| t.timestamp()))
        .bind(issue.pull_request.is_some() as i32)
        .bind(now)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    // Sync pull requests for a repository
    async fn sync_pull_requests(
        &self,
        owner: &str,
        repo: &str,
        repo_id: i64,
        result: &mut SyncResult,
    ) -> Result<()> {
        let prs = self
            .client
            .list_pull_requests(owner, repo, Some("all"))
            .await?;

        for pr in prs {
            match self.upsert_pull_request(&pr, repo_id).await {
                Ok(_) => {
                    result.prs_synced += 1;
                }
                Err(e) => {
                    error!("Failed to upsert PR #{}: {}", pr.number, e);
                }
            }
        }

        Ok(())
    }

    // Upsert a pull request into the database
    async fn upsert_pull_request(&self, pr: &PullRequest, repo_id: i64) -> Result<()> {
        let labels_json =
            serde_json::to_string(&pr.labels.iter().map(|l| &l.name).collect::<Vec<_>>())?;
        let now = Utc::now().timestamp();

        sqlx::query(
            r#"
            INSERT INTO github_pull_requests (
                id, node_id, repo_id, number, title, body, state, draft, merged,
                user_login, head_ref, head_sha, base_ref, base_sha, labels,
                commits, additions, deletions, changed_files, html_url,
                created_at, updated_at, closed_at, merged_at, last_synced_at
            ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18, $19, $20, $21, $22, $23, $24, $25)
            ON CONFLICT(repo_id, number) DO UPDATE SET
                title = excluded.title,
                body = excluded.body,
                state = excluded.state,
                draft = excluded.draft,
                merged = excluded.merged,
                labels = excluded.labels,
                commits = excluded.commits,
                additions = excluded.additions,
                deletions = excluded.deletions,
                changed_files = excluded.changed_files,
                updated_at = excluded.updated_at,
                closed_at = excluded.closed_at,
                merged_at = excluded.merged_at,
                last_synced_at = excluded.last_synced_at
            "#,
        )
        .bind(pr.id)
        .bind(&pr.node_id)
        .bind(repo_id)
        .bind(pr.number)
        .bind(&pr.title)
        .bind(&pr.body)
        .bind(format!("{:?}", pr.state).to_lowercase())
        .bind(pr.draft as i32)
        .bind(pr.merged as i32)
        .bind(&pr.user.login)
        .bind(&pr.head.r#ref)
        .bind(&pr.head.sha)
        .bind(&pr.base.r#ref)
        .bind(&pr.base.sha)
        .bind(labels_json)
        .bind(pr.commits)
        .bind(pr.additions)
        .bind(pr.deletions)
        .bind(pr.changed_files)
        .bind(&pr.html_url)
        .bind(pr.created_at.timestamp())
        .bind(pr.updated_at.timestamp())
        .bind(pr.closed_at.map(|t| t.timestamp()))
        .bind(pr.merged_at.map(|t| t.timestamp()))
        .bind(now)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    // Sync commits for a repository
    async fn sync_commits(
        &self,
        owner: &str,
        repo: &str,
        repo_id: i64,
        limit: u32,
        result: &mut SyncResult,
    ) -> Result<()> {
        let commits = self.client.list_commits(owner, repo, Some(limit)).await?;

        for commit in commits {
            match self.upsert_commit(&commit, repo_id).await {
                Ok(_) => {
                    result.commits_synced += 1;
                }
                Err(e) => {
                    error!("Failed to upsert commit {}: {}", commit.sha, e);
                }
            }
        }

        Ok(())
    }

    // Upsert a commit into the database
    async fn upsert_commit(&self, commit: &Commit, repo_id: i64) -> Result<()> {
        let now = Utc::now().timestamp();

        sqlx::query(
            r#"
            INSERT INTO github_commits (
                sha, node_id, repo_id, author_name, author_email, author_date,
                committer_name, committer_email, committer_date, message, additions, deletions,
                total_changes, html_url, created_at, last_synced_at
            ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16)
            ON CONFLICT(sha) DO UPDATE SET
                last_synced_at = excluded.last_synced_at
            "#,
        )
        .bind(&commit.sha)
        .bind(&commit.node_id)
        .bind(repo_id)
        .bind(&commit.commit.author.name)
        .bind(&commit.commit.author.email)
        .bind(commit.commit.author.date.timestamp())
        .bind(&commit.commit.committer.name)
        .bind(&commit.commit.committer.email)
        .bind(commit.commit.committer.date.timestamp())
        .bind(&commit.commit.message)
        .bind(commit.stats.as_ref().map(|s| s.additions))
        .bind(commit.stats.as_ref().map(|s| s.deletions))
        .bind(commit.stats.as_ref().map(|s| s.total))
        .bind(&commit.html_url)
        .bind(now)
        .bind(now)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    // Incremental sync (only updates since last sync)
    pub async fn sync_incremental(&self) -> Result<SyncResult> {
        info!("Starting incremental GitHub sync");

        // For now, just do a full sync
        // In production, you would check last_synced_at timestamps
        // and only fetch items updated since then
        self.sync_all_repos().await
    }

    // Get open issues across all synced repositories
    pub async fn get_open_issues(&self) -> Result<Vec<(String, i32, String)>> {
        let rows = sqlx::query(
            r#"
            SELECT r.full_name, i.number, i.title
            FROM github_issues i
            JOIN github_repositories r ON i.repo_id = r.id
            WHERE i.state = 'open' AND i.is_pull_request = 0
            ORDER BY i.updated_at DESC
            "#,
        )
        .fetch_all(&self.pool)
        .await?;

        let issues = rows
            .into_iter()
            .map(|row| {
                let repo: String = row.get(0);
                let number: i32 = row.get(1);
                let title: String = row.get(2);
                (repo, number, title)
            })
            .collect();

        Ok(issues)
    }

    // Get open PRs needing review
    pub async fn get_prs_needing_review(&self) -> Result<Vec<(String, i32, String)>> {
        let rows = sqlx::query(
            r#"
            SELECT r.full_name, p.number, p.title
            FROM github_pull_requests p
            JOIN github_repositories r ON p.repo_id = r.id
            WHERE p.state = 'open' AND p.draft = 0
            ORDER BY p.updated_at DESC
            "#,
        )
        .fetch_all(&self.pool)
        .await?;

        let prs = rows
            .into_iter()
            .map(|row| {
                let repo: String = row.get(0);
                let number: i32 = row.get(1);
                let title: String = row.get(2);
                (repo, number, title)
            })
            .collect();

        Ok(prs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sync_options_default() {
        let opts = SyncOptions::default();
        assert!(opts.sync_repos);
        assert!(opts.sync_issues);
        assert!(opts.sync_prs);
        assert_eq!(opts.commits_limit, 100);
    }

    #[test]
    fn test_sync_options_repos_only() {
        let opts = SyncOptions::repos_only();
        assert!(opts.sync_repos);
        assert!(!opts.sync_issues);
        assert!(!opts.sync_prs);
        assert!(!opts.sync_commits);
    }

    #[test]
    fn test_sync_options_builder() {
        let opts = SyncOptions::default()
            .with_repos(vec!["owner/repo".to_string()])
            .force_full();

        assert!(opts.force_full);
        assert_eq!(opts.repo_filter, Some(vec!["owner/repo".to_string()]));
    }

    #[test]
    fn test_sync_result_duration() {
        let mut result = SyncResult::new();
        std::thread::sleep(std::time::Duration::from_millis(100));
        result.finish();

        assert!(result.duration_secs >= 0.1);
        assert!(result.duration_secs < 1.0);
    }
}
