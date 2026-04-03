-- Migration: 002_github_integration.sql
-- Rewritten for PostgreSQL
-- FTS5 virtual tables replaced with tsvector columns + GIN indexes
-- AUTOINCREMENT replaced with BIGSERIAL
-- strftime replaced with EXTRACT(EPOCH FROM NOW())::BIGINT
-- INSERT OR IGNORE replaced with INSERT ... ON CONFLICT DO NOTHING

-- ============================================================================
-- Repositories Table
-- ============================================================================
CREATE TABLE IF NOT EXISTS github_repositories (
    id BIGINT PRIMARY KEY,
    node_id TEXT NOT NULL,
    name TEXT NOT NULL,
    full_name TEXT NOT NULL UNIQUE,
    owner_login TEXT NOT NULL,
    owner_id BIGINT NOT NULL,
    description TEXT,
    html_url TEXT NOT NULL,
    clone_url TEXT NOT NULL,
    ssh_url TEXT NOT NULL,
    homepage TEXT,
    language TEXT,

    -- Visibility
    private INTEGER NOT NULL DEFAULT 0,
    fork INTEGER NOT NULL DEFAULT 0,
    archived INTEGER NOT NULL DEFAULT 0,
    disabled INTEGER NOT NULL DEFAULT 0,

    -- Statistics
    stargazers_count INTEGER NOT NULL DEFAULT 0,
    watchers_count INTEGER NOT NULL DEFAULT 0,
    forks_count INTEGER NOT NULL DEFAULT 0,
    open_issues_count INTEGER NOT NULL DEFAULT 0,
    size INTEGER NOT NULL DEFAULT 0,

    -- Features
    topics TEXT,
    has_issues INTEGER NOT NULL DEFAULT 1,
    has_projects INTEGER NOT NULL DEFAULT 1,
    has_wiki INTEGER NOT NULL DEFAULT 1,
    has_pages INTEGER NOT NULL DEFAULT 0,
    has_downloads INTEGER NOT NULL DEFAULT 1,

    -- Branches
    default_branch TEXT NOT NULL DEFAULT 'main',

    -- Timestamps (Unix epoch)
    created_at BIGINT NOT NULL,
    updated_at BIGINT NOT NULL,
    pushed_at BIGINT,

    -- Sync metadata
    last_synced_at BIGINT NOT NULL,
    sync_enabled INTEGER NOT NULL DEFAULT 1
);

CREATE INDEX IF NOT EXISTS idx_github_repos_owner ON github_repositories(owner_login);
CREATE INDEX IF NOT EXISTS idx_github_repos_language ON github_repositories(language);
CREATE INDEX IF NOT EXISTS idx_github_repos_archived ON github_repositories(archived);
CREATE INDEX IF NOT EXISTS idx_github_repos_sync ON github_repositories(sync_enabled);
CREATE INDEX IF NOT EXISTS idx_github_repos_full_name ON github_repositories(full_name);

-- ============================================================================
-- Issues Table
-- ============================================================================
CREATE TABLE IF NOT EXISTS github_issues (
    id BIGINT PRIMARY KEY,
    node_id TEXT NOT NULL,
    repo_id BIGINT NOT NULL,
    number INTEGER NOT NULL,
    title TEXT NOT NULL,
    body TEXT,
    body_text TEXT,

    -- User info
    user_login TEXT NOT NULL,
    user_id BIGINT NOT NULL,

    -- State
    state TEXT NOT NULL CHECK(state IN ('open', 'closed')),
    state_reason TEXT,
    locked INTEGER NOT NULL DEFAULT 0,

    -- Metadata
    labels TEXT,
    assignees TEXT,
    milestone_id BIGINT,
    comments INTEGER NOT NULL DEFAULT 0,

    -- URLs
    html_url TEXT NOT NULL,

    -- Timestamps
    created_at BIGINT NOT NULL,
    updated_at BIGINT NOT NULL,
    closed_at BIGINT,

    -- PR relationship
    is_pull_request INTEGER NOT NULL DEFAULT 0,

    -- Sync metadata
    last_synced_at BIGINT NOT NULL,

    -- Full-text search vector
    search_vector tsvector GENERATED ALWAYS AS (
        to_tsvector('english', COALESCE(title, '') || ' ' || COALESCE(body_text, '') || ' ' || COALESCE(body, ''))
    ) STORED,

    FOREIGN KEY (repo_id) REFERENCES github_repositories(id) ON DELETE CASCADE,
    UNIQUE(repo_id, number)
);

CREATE INDEX IF NOT EXISTS idx_github_issues_repo ON github_issues(repo_id);
CREATE INDEX IF NOT EXISTS idx_github_issues_state ON github_issues(state);
CREATE INDEX IF NOT EXISTS idx_github_issues_user ON github_issues(user_login);
CREATE INDEX IF NOT EXISTS idx_github_issues_updated ON github_issues(updated_at);
CREATE INDEX IF NOT EXISTS idx_github_issues_pr ON github_issues(is_pull_request);
CREATE INDEX IF NOT EXISTS idx_github_issues_title ON github_issues(title);
CREATE INDEX IF NOT EXISTS idx_github_issues_fts ON github_issues USING GIN(search_vector);

-- ============================================================================
-- Pull Requests Table
-- ============================================================================
CREATE TABLE IF NOT EXISTS github_pull_requests (
    id BIGINT PRIMARY KEY,
    node_id TEXT NOT NULL,
    repo_id BIGINT NOT NULL,
    number INTEGER NOT NULL,
    title TEXT NOT NULL,
    body TEXT,
    body_text TEXT,

    -- User info
    user_login TEXT NOT NULL,
    user_id BIGINT NOT NULL,

    -- State
    state TEXT NOT NULL CHECK(state IN ('open', 'closed')),
    draft INTEGER NOT NULL DEFAULT 0,
    merged INTEGER NOT NULL DEFAULT 0,
    mergeable INTEGER,
    mergeable_state TEXT,

    -- Branch info
    head_ref TEXT NOT NULL,
    head_sha TEXT NOT NULL,
    head_repo_id BIGINT,
    base_ref TEXT NOT NULL,
    base_sha TEXT NOT NULL,

    -- Review info
    requested_reviewers TEXT,
    labels TEXT,
    milestone_id BIGINT,

    -- Statistics
    commits INTEGER NOT NULL DEFAULT 0,
    additions INTEGER NOT NULL DEFAULT 0,
    deletions INTEGER NOT NULL DEFAULT 0,
    changed_files INTEGER NOT NULL DEFAULT 0,
    comments INTEGER NOT NULL DEFAULT 0,
    review_comments INTEGER NOT NULL DEFAULT 0,

    -- URLs
    html_url TEXT NOT NULL,
    diff_url TEXT NOT NULL,
    patch_url TEXT NOT NULL,

    -- Timestamps
    created_at BIGINT NOT NULL,
    updated_at BIGINT NOT NULL,
    closed_at BIGINT,
    merged_at BIGINT,
    merged_by_login TEXT,

    -- Sync metadata
    last_synced_at BIGINT NOT NULL,

    -- Full-text search vector
    search_vector tsvector GENERATED ALWAYS AS (
        to_tsvector('english', COALESCE(title, '') || ' ' || COALESCE(body_text, '') || ' ' || COALESCE(body, ''))
    ) STORED,

    FOREIGN KEY (repo_id) REFERENCES github_repositories(id) ON DELETE CASCADE,
    UNIQUE(repo_id, number)
);

CREATE INDEX IF NOT EXISTS idx_github_prs_repo ON github_pull_requests(repo_id);
CREATE INDEX IF NOT EXISTS idx_github_prs_state ON github_pull_requests(state);
CREATE INDEX IF NOT EXISTS idx_github_prs_user ON github_pull_requests(user_login);
CREATE INDEX IF NOT EXISTS idx_github_prs_draft ON github_pull_requests(draft);
CREATE INDEX IF NOT EXISTS idx_github_prs_merged ON github_pull_requests(merged);
CREATE INDEX IF NOT EXISTS idx_github_prs_updated ON github_pull_requests(updated_at);
CREATE INDEX IF NOT EXISTS idx_github_prs_base_ref ON github_pull_requests(base_ref);
CREATE INDEX IF NOT EXISTS idx_github_prs_fts ON github_pull_requests USING GIN(search_vector);

-- ============================================================================
-- Commits Table
-- ============================================================================
CREATE TABLE IF NOT EXISTS github_commits (
    sha TEXT PRIMARY KEY,
    node_id TEXT NOT NULL,
    repo_id BIGINT NOT NULL,

    -- Author (Git signature)
    author_name TEXT NOT NULL,
    author_email TEXT NOT NULL,
    author_date BIGINT NOT NULL,

    -- Committer (Git signature)
    committer_name TEXT NOT NULL,
    committer_email TEXT NOT NULL,
    committer_date BIGINT NOT NULL,

    -- GitHub user
    author_github_login TEXT,
    committer_github_login TEXT,

    -- Message
    message TEXT NOT NULL,
    comment_count INTEGER NOT NULL DEFAULT 0,

    -- Statistics
    additions INTEGER,
    deletions INTEGER,
    total_changes INTEGER,

    -- Verification
    verified INTEGER NOT NULL DEFAULT 0,

    -- URLs
    html_url TEXT NOT NULL,

    -- Sync metadata
    created_at BIGINT NOT NULL,
    last_synced_at BIGINT NOT NULL,

    -- Full-text search vector
    search_vector tsvector GENERATED ALWAYS AS (
        to_tsvector('english', COALESCE(message, '') || ' ' || COALESCE(author_name, ''))
    ) STORED,

    FOREIGN KEY (repo_id) REFERENCES github_repositories(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_github_commits_repo ON github_commits(repo_id);
CREATE INDEX IF NOT EXISTS idx_github_commits_author ON github_commits(author_name);
CREATE INDEX IF NOT EXISTS idx_github_commits_date ON github_commits(author_date);
CREATE INDEX IF NOT EXISTS idx_github_commits_verified ON github_commits(verified);
CREATE INDEX IF NOT EXISTS idx_github_commits_fts ON github_commits USING GIN(search_vector);

-- ============================================================================
-- Labels Table
-- ============================================================================
CREATE TABLE IF NOT EXISTS github_labels (
    id BIGINT PRIMARY KEY,
    node_id TEXT NOT NULL,
    repo_id BIGINT NOT NULL,
    name TEXT NOT NULL,
    color TEXT NOT NULL,
    description TEXT,
    is_default INTEGER NOT NULL DEFAULT 0,

    FOREIGN KEY (repo_id) REFERENCES github_repositories(id) ON DELETE CASCADE,
    UNIQUE(repo_id, name)
);

CREATE INDEX IF NOT EXISTS idx_github_labels_repo ON github_labels(repo_id);
CREATE INDEX IF NOT EXISTS idx_github_labels_name ON github_labels(name);

-- ============================================================================
-- Milestones Table
-- ============================================================================
CREATE TABLE IF NOT EXISTS github_milestones (
    id BIGINT PRIMARY KEY,
    node_id TEXT NOT NULL,
    repo_id BIGINT NOT NULL,
    number INTEGER NOT NULL,
    title TEXT NOT NULL,
    description TEXT,
    state TEXT NOT NULL CHECK(state IN ('open', 'closed')),
    open_issues INTEGER NOT NULL DEFAULT 0,
    closed_issues INTEGER NOT NULL DEFAULT 0,
    created_at BIGINT NOT NULL,
    updated_at BIGINT NOT NULL,
    due_on BIGINT,
    closed_at BIGINT,
    creator_login TEXT,

    FOREIGN KEY (repo_id) REFERENCES github_repositories(id) ON DELETE CASCADE,
    UNIQUE(repo_id, number)
);

CREATE INDEX IF NOT EXISTS idx_github_milestones_repo ON github_milestones(repo_id);
CREATE INDEX IF NOT EXISTS idx_github_milestones_state ON github_milestones(state);

-- ============================================================================
-- Sync History Table
-- ============================================================================
CREATE TABLE IF NOT EXISTS github_sync_history (
    id BIGSERIAL PRIMARY KEY,
    started_at BIGINT NOT NULL,
    completed_at BIGINT NOT NULL,
    duration_secs REAL NOT NULL,
    repos_synced INTEGER NOT NULL DEFAULT 0,
    issues_synced INTEGER NOT NULL DEFAULT 0,
    prs_synced INTEGER NOT NULL DEFAULT 0,
    commits_synced INTEGER NOT NULL DEFAULT 0,
    items_created INTEGER NOT NULL DEFAULT 0,
    items_updated INTEGER NOT NULL DEFAULT 0,
    errors_count INTEGER NOT NULL DEFAULT 0,
    errors TEXT,
    warnings TEXT
);

CREATE INDEX IF NOT EXISTS idx_github_sync_history_completed ON github_sync_history(completed_at);

-- ============================================================================
-- Webhook Events Table
-- ============================================================================
CREATE TABLE IF NOT EXISTS github_webhook_events (
    id BIGSERIAL PRIMARY KEY,
    delivery_id TEXT NOT NULL UNIQUE,
    event_type TEXT NOT NULL,
    action TEXT,
    repo_id BIGINT,
    payload TEXT NOT NULL,
    processed INTEGER NOT NULL DEFAULT 0,
    processed_at BIGINT,
    error TEXT,
    received_at BIGINT NOT NULL,

    FOREIGN KEY (repo_id) REFERENCES github_repositories(id) ON DELETE SET NULL
);

CREATE INDEX IF NOT EXISTS idx_github_webhooks_type ON github_webhook_events(event_type);
CREATE INDEX IF NOT EXISTS idx_github_webhooks_processed ON github_webhook_events(processed);
CREATE INDEX IF NOT EXISTS idx_github_webhooks_received ON github_webhook_events(received_at);

-- ============================================================================
-- Views for Common Queries
-- ============================================================================

CREATE OR REPLACE VIEW github_active_repos AS
SELECT * FROM github_repositories
WHERE archived = 0 AND sync_enabled = 1;

CREATE OR REPLACE VIEW github_open_issues AS
SELECT i.*, r.full_name AS repo_full_name
FROM github_issues i
JOIN github_repositories r ON i.repo_id = r.id
WHERE i.state = 'open' AND i.is_pull_request = 0;

CREATE OR REPLACE VIEW github_open_prs AS
SELECT p.*, r.full_name AS repo_full_name
FROM github_pull_requests p
JOIN github_repositories r ON p.repo_id = r.id
WHERE p.state = 'open';

CREATE OR REPLACE VIEW github_prs_needing_review AS
SELECT p.*, r.full_name AS repo_full_name
FROM github_pull_requests p
JOIN github_repositories r ON p.repo_id = r.id
WHERE p.state = 'open' AND p.draft = 0
ORDER BY p.updated_at DESC;

CREATE OR REPLACE VIEW github_recent_commits AS
SELECT c.*, r.full_name AS repo_full_name
FROM github_commits c
JOIN github_repositories r ON c.repo_id = r.id
WHERE c.author_date > (EXTRACT(EPOCH FROM NOW())::BIGINT - 604800)
ORDER BY c.author_date DESC;

-- ============================================================================
-- GitHub Config Table
-- ============================================================================
CREATE TABLE IF NOT EXISTS github_config (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL,
    updated_at BIGINT NOT NULL
);

INSERT INTO github_config (key, value, updated_at) VALUES
    ('api_version', '2022-11-28', EXTRACT(EPOCH FROM NOW())::BIGINT),
    ('sync_interval_seconds', '3600', EXTRACT(EPOCH FROM NOW())::BIGINT),
    ('default_commits_limit', '100', EXTRACT(EPOCH FROM NOW())::BIGINT),
    ('auto_sync_enabled', '1', EXTRACT(EPOCH FROM NOW())::BIGINT)
ON CONFLICT (key) DO NOTHING;

-- ============================================================================
-- Sync Metadata Table
-- ============================================================================
CREATE TABLE IF NOT EXISTS github_sync_metadata (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

-- ============================================================================
-- Migration Complete
-- ============================================================================
-- GitHub integration schema created successfully (PostgreSQL)
-- Tables: 11 + config + metadata
-- Indexes: standard B-tree + GIN for full-text search (tsvector)
-- Views: 5
-- ============================================================================
