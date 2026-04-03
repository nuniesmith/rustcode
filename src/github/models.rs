//! GitHub Domain Models
//!
//! Type-safe representations of GitHub entities with full serde support.
//! These models are designed to work with both GraphQL and REST APIs.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

// Helper module for deserializing Unix timestamps
mod serde_helpers {
    use chrono::{DateTime, Utc};
    use serde::{Deserialize, Deserializer};

    pub fn deserialize_timestamp<'de, D>(deserializer: D) -> Result<DateTime<Utc>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let timestamp = i64::deserialize(deserializer)?;
        DateTime::from_timestamp(timestamp, 0)
            .ok_or_else(|| serde::de::Error::custom("invalid timestamp"))
    }
}

// ============================================================================
// User & Organization
// ============================================================================

/// GitHub user or organization
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct User {
    pub id: i64,
    pub login: String,
    pub name: Option<String>,
    pub email: Option<String>,
    pub avatar_url: String,
    pub html_url: String,
    #[serde(rename = "type")]
    pub user_type: UserType,
    pub bio: Option<String>,
    pub company: Option<String>,
    pub location: Option<String>,
    pub blog: Option<String>,
    pub twitter_username: Option<String>,
    pub public_repos: Option<i32>,
    pub followers: Option<i32>,
    pub following: Option<i32>,
    pub created_at: Option<DateTime<Utc>>,
    pub updated_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "PascalCase")]
pub enum UserType {
    User,
    Organization,
    Bot,
}

// ============================================================================
// Repository
// ============================================================================

/// GitHub repository
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Repository {
    pub id: i64,
    pub node_id: String,
    pub name: String,
    pub full_name: String,
    pub owner: User,
    pub description: Option<String>,
    pub html_url: String,
    pub clone_url: String,
    pub ssh_url: String,
    pub homepage: Option<String>,
    pub language: Option<String>,
    pub languages_url: String,

    // Visibility
    pub private: bool,
    pub visibility: RepositoryVisibility,
    pub fork: bool,
    pub archived: bool,
    pub disabled: bool,

    // Stats
    pub stargazers_count: i32,
    pub watchers_count: i32,
    pub forks_count: i32,
    pub open_issues_count: i32,
    pub size: i64, // KB

    // Topics & Tags
    pub topics: Vec<String>,
    pub has_issues: bool,
    pub has_projects: bool,
    pub has_wiki: bool,
    pub has_pages: bool,
    pub has_downloads: bool,

    // Branches
    pub default_branch: String,

    // Timestamps
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub pushed_at: Option<DateTime<Utc>>,

    // License
    pub license: Option<License>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum RepositoryVisibility {
    Public,
    Private,
    Internal,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct License {
    pub key: String,
    pub name: String,
    pub spdx_id: Option<String>,
    pub url: Option<String>,
}

// ============================================================================
// Issues
// ============================================================================

/// GitHub issue
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Issue {
    pub id: i64,
    pub node_id: String,
    pub number: i32,
    pub title: String,
    pub body: Option<String>,
    pub body_text: Option<String>, // Rendered text without markdown
    pub body_html: Option<String>, // Rendered HTML

    pub user: User,
    pub state: IssueState,
    pub state_reason: Option<IssueStateReason>,

    pub labels: Vec<Label>,
    pub assignees: Vec<User>,
    pub milestone: Option<Milestone>,

    pub comments: i32,
    pub locked: bool,
    pub active_lock_reason: Option<String>,

    // URLs
    pub html_url: String,
    pub repository_url: String,
    pub comments_url: String,

    // Timestamps
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub closed_at: Option<DateTime<Utc>>,

    // PR relationship
    pub pull_request: Option<IssuePullRequestLink>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum IssueState {
    Open,
    Closed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum IssueStateReason {
    Completed,
    NotPlanned,
    Reopened,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IssuePullRequestLink {
    pub url: String,
    pub html_url: String,
    pub diff_url: String,
    pub patch_url: String,
}

// ============================================================================
// Pull Requests
// ============================================================================

/// GitHub pull request
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PullRequest {
    pub id: i64,
    pub node_id: String,
    pub number: i32,
    pub title: String,
    pub body: Option<String>,
    pub body_text: Option<String>,
    pub body_html: Option<String>,

    pub user: User,
    pub state: PrState,
    pub draft: bool,
    pub merged: bool,
    pub mergeable: Option<bool>,
    pub mergeable_state: Option<String>,
    pub merged_by: Option<User>,

    // Branch info
    pub head: PrBranch,
    pub base: PrBranch,

    // Review state
    pub requested_reviewers: Vec<User>,
    pub requested_teams: Vec<Team>,
    pub labels: Vec<Label>,
    pub milestone: Option<Milestone>,

    // Stats
    pub commits: i32,
    pub additions: i32,
    pub deletions: i32,
    pub changed_files: i32,
    pub comments: i32,
    pub review_comments: i32,

    // URLs
    pub html_url: String,
    pub diff_url: String,
    pub patch_url: String,
    pub issue_url: String,
    pub commits_url: String,
    pub review_comments_url: String,
    pub statuses_url: String,

    // Timestamps
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub closed_at: Option<DateTime<Utc>>,
    pub merged_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum PrState {
    Open,
    Closed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrBranch {
    pub label: String,
    pub r#ref: String,
    pub sha: String,
    pub user: User,
    pub repo: Option<Repository>,
}

// ============================================================================
// Commits
// ============================================================================

/// GitHub commit
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Commit {
    pub sha: String,
    pub node_id: String,
    pub commit: CommitDetails,
    pub author: Option<User>,
    pub committer: Option<User>,
    pub parents: Vec<CommitReference>,
    pub html_url: String,
    pub comments_url: String,
    pub stats: Option<CommitStats>,
    pub files: Option<Vec<CommitFile>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommitDetails {
    pub author: GitUser,
    pub committer: GitUser,
    pub message: String,
    pub comment_count: i32,
    pub tree: TreeReference,
    pub verification: Option<Verification>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitUser {
    pub name: String,
    pub email: String,
    pub date: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommitReference {
    pub sha: String,
    pub url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TreeReference {
    pub sha: String,
    pub url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommitStats {
    pub additions: i32,
    pub deletions: i32,
    pub total: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommitFile {
    pub filename: String,
    pub status: FileStatus,
    pub additions: i32,
    pub deletions: i32,
    pub changes: i32,
    pub blob_url: String,
    pub raw_url: String,
    pub patch: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum FileStatus {
    Added,
    Modified,
    Removed,
    Renamed,
    Copied,
    Changed,
    Unchanged,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Verification {
    pub verified: bool,
    pub reason: String,
    pub signature: Option<String>,
    pub payload: Option<String>,
}

// ============================================================================
// Commit Status (CI/CD)
// ============================================================================

/// Commit status (CI/CD checks)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommitStatus {
    pub id: i64,
    pub state: CommitState,
    pub description: Option<String>,
    pub target_url: Option<String>,
    pub context: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub creator: User,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum CommitState {
    Pending,
    Success,
    Failure,
    Error,
}

// ============================================================================
// Labels, Milestones, Teams
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Label {
    pub id: i64,
    pub node_id: String,
    pub url: String,
    pub name: String,
    pub description: Option<String>,
    pub color: String,
    pub default: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Milestone {
    pub id: i64,
    pub node_id: String,
    pub number: i32,
    pub title: String,
    pub description: Option<String>,
    pub state: MilestoneState,
    pub open_issues: i32,
    pub closed_issues: i32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub due_on: Option<DateTime<Utc>>,
    pub closed_at: Option<DateTime<Utc>>,
    pub creator: User,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum MilestoneState {
    Open,
    Closed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Team {
    pub id: i64,
    pub node_id: String,
    pub name: String,
    pub slug: String,
    pub description: Option<String>,
    pub privacy: String,
    pub permission: String,
    pub url: String,
    pub html_url: String,
    pub members_url: String,
    pub repositories_url: String,
}

// ============================================================================
// Search Results
// ============================================================================

/// Generic search response wrapper
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResponse<T> {
    pub total_count: i32,
    pub incomplete_results: bool,
    pub items: Vec<T>,
}

// ============================================================================
// Activity Events
// ============================================================================

/// GitHub event (for activity tracking)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    pub id: String,
    #[serde(rename = "type")]
    pub event_type: EventType,
    pub actor: User,
    pub repo: EventRepository,
    pub payload: serde_json::Value, // Polymorphic based on event_type
    pub public: bool,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventRepository {
    pub id: i64,
    pub name: String,
    pub url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum EventType {
    PushEvent,
    PullRequestEvent,
    IssuesEvent,
    IssueCommentEvent,
    WatchEvent,
    ForkEvent,
    CreateEvent,
    DeleteEvent,
    ReleaseEvent,
    PublicEvent,
    MemberEvent,
    CommitCommentEvent,
    GollumEvent, // Wiki
}

// ============================================================================
// Rate Limiting
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RateLimit {
    pub limit: i32,
    pub remaining: i32,
    #[serde(deserialize_with = "serde_helpers::deserialize_timestamp")]
    pub reset: DateTime<Utc>,
    pub used: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resource: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RateLimitResponse {
    pub resources: RateLimitResources,
    pub rate: RateLimit,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RateLimitResources {
    pub core: RateLimit,
    pub search: RateLimit,
    pub graphql: RateLimit,
    pub integration_manifest: Option<RateLimit>,
    pub code_scanning_upload: Option<RateLimit>,
}

// ============================================================================
// Helper Functions
// ============================================================================

impl Repository {
    /// Check if repository is a fork
    pub fn is_fork(&self) -> bool {
        self.fork
    }

    /// Check if repository is archived
    pub fn is_archived(&self) -> bool {
        self.archived
    }

    /// Check if repository is active (not archived, not disabled)
    pub fn is_active(&self) -> bool {
        !self.archived && !self.disabled
    }

    /// Get primary language
    pub fn primary_language(&self) -> Option<&str> {
        self.language.as_deref()
    }
}

impl Issue {
    /// Check if issue is open
    pub fn is_open(&self) -> bool {
        self.state == IssueState::Open
    }

    /// Check if issue is a pull request
    pub fn is_pull_request(&self) -> bool {
        self.pull_request.is_some()
    }

    /// Check if issue is assigned
    pub fn is_assigned(&self) -> bool {
        !self.assignees.is_empty()
    }
}

impl PullRequest {
    /// Check if PR is open
    pub fn is_open(&self) -> bool {
        self.state == PrState::Open
    }

    /// Check if PR is draft
    pub fn is_draft(&self) -> bool {
        self.draft
    }

    /// Check if PR is merged
    pub fn is_merged(&self) -> bool {
        self.merged
    }

    /// Check if PR needs review
    pub fn needs_review(&self) -> bool {
        self.is_open() && !self.is_draft() && !self.requested_reviewers.is_empty()
    }
}

impl Commit {
    /// Get commit message (first line)
    pub fn message_summary(&self) -> &str {
        self.commit.message.lines().next().unwrap_or("")
    }

    /// Get commit author name
    pub fn author_name(&self) -> &str {
        &self.commit.author.name
    }

    /// Check if commit is verified (signed)
    pub fn is_verified(&self) -> bool {
        self.commit
            .verification
            .as_ref()
            .map(|v| v.verified)
            .unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_issue_is_pull_request() {
        let mut issue = create_test_issue();
        assert!(!issue.is_pull_request());

        issue.pull_request = Some(IssuePullRequestLink {
            url: "https://api.github.com/repos/test/test/pulls/1".to_string(),
            html_url: "https://github.com/test/test/pull/1".to_string(),
            diff_url: "https://github.com/test/test/pull/1.diff".to_string(),
            patch_url: "https://github.com/test/test/pull/1.patch".to_string(),
        });
        assert!(issue.is_pull_request());
    }

    #[test]
    fn test_repository_is_active() {
        let mut repo = create_test_repository();
        assert!(repo.is_active());

        repo.archived = true;
        assert!(!repo.is_active());

        repo.archived = false;
        repo.disabled = true;
        assert!(!repo.is_active());
    }

    #[test]
    fn test_pr_needs_review() {
        let mut pr = create_test_pull_request();
        pr.requested_reviewers = vec![create_test_user()];

        assert!(pr.needs_review());

        pr.draft = true;
        assert!(!pr.needs_review());

        pr.draft = false;
        pr.state = PrState::Closed;
        assert!(!pr.needs_review());
    }

    // Test helpers
    fn create_test_user() -> User {
        User {
            id: 1,
            login: "testuser".to_string(),
            name: Some("Test User".to_string()),
            email: Some("test@example.com".to_string()),
            avatar_url: "https://avatars.githubusercontent.com/u/1".to_string(),
            html_url: "https://github.com/testuser".to_string(),
            user_type: UserType::User,
            bio: None,
            company: None,
            location: None,
            blog: None,
            twitter_username: None,
            public_repos: Some(10),
            followers: Some(5),
            following: Some(3),
            created_at: Some(Utc::now()),
            updated_at: Some(Utc::now()),
        }
    }

    fn create_test_repository() -> Repository {
        Repository {
            id: 1,
            node_id: "MDEwOlJlcG9zaXRvcnkx".to_string(),
            name: "test-repo".to_string(),
            full_name: "testuser/test-repo".to_string(),
            owner: create_test_user(),
            description: Some("Test repository".to_string()),
            html_url: "https://github.com/testuser/test-repo".to_string(),
            clone_url: "https://github.com/testuser/test-repo.git".to_string(),
            ssh_url: "git@github.com:testuser/test-repo.git".to_string(),
            homepage: None,
            language: Some("Rust".to_string()),
            languages_url: "https://api.github.com/repos/testuser/test-repo/languages".to_string(),
            private: false,
            visibility: RepositoryVisibility::Public,
            fork: false,
            archived: false,
            disabled: false,
            stargazers_count: 100,
            watchers_count: 50,
            forks_count: 10,
            open_issues_count: 5,
            size: 1024,
            topics: vec!["rust".to_string(), "cli".to_string()],
            has_issues: true,
            has_projects: true,
            has_wiki: true,
            has_pages: false,
            has_downloads: true,
            default_branch: "main".to_string(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            pushed_at: Some(Utc::now()),
            license: None,
        }
    }

    fn create_test_issue() -> Issue {
        Issue {
            id: 1,
            node_id: "MDU6SXNzdWUx".to_string(),
            number: 1,
            title: "Test issue".to_string(),
            body: Some("This is a test issue".to_string()),
            body_text: None,
            body_html: None,
            user: create_test_user(),
            state: IssueState::Open,
            state_reason: None,
            labels: vec![],
            assignees: vec![],
            milestone: None,
            comments: 0,
            locked: false,
            active_lock_reason: None,
            html_url: "https://github.com/testuser/test-repo/issues/1".to_string(),
            repository_url: "https://api.github.com/repos/testuser/test-repo".to_string(),
            comments_url: "https://api.github.com/repos/testuser/test-repo/issues/1/comments"
                .to_string(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            closed_at: None,
            pull_request: None,
        }
    }

    fn create_test_pull_request() -> PullRequest {
        PullRequest {
            id: 1,
            node_id: "MDExOlB1bGxSZXF1ZXN0MQ==".to_string(),
            number: 1,
            title: "Test PR".to_string(),
            body: Some("Test pull request".to_string()),
            body_text: None,
            body_html: None,
            user: create_test_user(),
            state: PrState::Open,
            draft: false,
            merged: false,
            mergeable: Some(true),
            mergeable_state: Some("clean".to_string()),
            merged_by: None,
            head: create_test_branch("feature"),
            base: create_test_branch("main"),
            requested_reviewers: vec![],
            requested_teams: vec![],
            labels: vec![],
            milestone: None,
            commits: 1,
            additions: 10,
            deletions: 5,
            changed_files: 2,
            comments: 0,
            review_comments: 0,
            html_url: "https://github.com/testuser/test-repo/pull/1".to_string(),
            diff_url: "https://github.com/testuser/test-repo/pull/1.diff".to_string(),
            patch_url: "https://github.com/testuser/test-repo/pull/1.patch".to_string(),
            issue_url: "https://api.github.com/repos/testuser/test-repo/issues/1".to_string(),
            commits_url: "https://api.github.com/repos/testuser/test-repo/pulls/1/commits"
                .to_string(),
            review_comments_url: "https://api.github.com/repos/testuser/test-repo/pulls/1/comments"
                .to_string(),
            statuses_url: "https://api.github.com/repos/testuser/test-repo/statuses/abc123"
                .to_string(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            closed_at: None,
            merged_at: None,
        }
    }

    fn create_test_branch(name: &str) -> PrBranch {
        PrBranch {
            label: format!("testuser:{}", name),
            r#ref: name.to_string(),
            sha: "abc123def456".to_string(),
            user: create_test_user(),
            repo: None,
        }
    }
}
