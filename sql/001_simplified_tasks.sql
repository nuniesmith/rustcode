-- Migration: 001_simplified_tasks.sql
-- Rewritten for PostgreSQL
-- Consolidates QueueItem, FileAnalysis, and TodoItem into a single Task table

-- ============================================================================
-- Core Task Table
-- ============================================================================

CREATE TABLE IF NOT EXISTS tasks (
    id TEXT PRIMARY KEY NOT NULL,

    -- Content
    content TEXT NOT NULL,
    context TEXT,
    llm_suggestion TEXT,

    -- Source tracking
    source_type TEXT NOT NULL DEFAULT 'manual',
    source_repo TEXT,
    source_file TEXT,
    source_line INTEGER,
    content_hash TEXT,

    -- Status & Priority
    status TEXT NOT NULL DEFAULT 'pending',
    priority INTEGER NOT NULL DEFAULT 5,
    category TEXT,

    -- Grouping
    group_id TEXT,
    group_reason TEXT,

    -- Processing metadata
    retry_count INTEGER NOT NULL DEFAULT 0,
    last_error TEXT,
    tokens_used INTEGER,

    -- Timestamps
    created_at BIGINT NOT NULL DEFAULT EXTRACT(EPOCH FROM NOW())::BIGINT,
    updated_at BIGINT NOT NULL DEFAULT EXTRACT(EPOCH FROM NOW())::BIGINT,
    processed_at BIGINT,
    completed_at BIGINT
);

-- ============================================================================
-- Indexes for common queries
-- ============================================================================

CREATE INDEX IF NOT EXISTS idx_tasks_status ON tasks(status);
CREATE INDEX IF NOT EXISTS idx_tasks_priority ON tasks(priority DESC);
CREATE INDEX IF NOT EXISTS idx_tasks_source_repo ON tasks(source_repo);
CREATE INDEX IF NOT EXISTS idx_tasks_source_file ON tasks(source_file);
CREATE INDEX IF NOT EXISTS idx_tasks_group_id ON tasks(group_id);
CREATE INDEX IF NOT EXISTS idx_tasks_content_hash ON tasks(content_hash);
CREATE INDEX IF NOT EXISTS idx_tasks_category ON tasks(category);

-- Composite index for queue queries (pending tasks by priority)
CREATE INDEX IF NOT EXISTS idx_tasks_queue ON tasks(status, priority DESC, created_at);

-- ============================================================================
-- Repository Tracking
-- ============================================================================

CREATE TABLE IF NOT EXISTS repositories (
    id TEXT PRIMARY KEY NOT NULL,
    name TEXT NOT NULL UNIQUE,
    url TEXT,
    local_path TEXT,

    auto_scan INTEGER NOT NULL DEFAULT 1,
    scan_interval_mins INTEGER NOT NULL DEFAULT 60,
    last_scanned_at BIGINT,
    last_commit_hash TEXT,

    created_at BIGINT NOT NULL DEFAULT EXTRACT(EPOCH FROM NOW())::BIGINT,
    updated_at BIGINT NOT NULL DEFAULT EXTRACT(EPOCH FROM NOW())::BIGINT
);

CREATE INDEX IF NOT EXISTS idx_repos_auto_scan ON repositories(auto_scan, last_scanned_at);

-- ============================================================================
-- Task Groups (for batch IDE handoff)
-- ============================================================================

CREATE TABLE IF NOT EXISTS task_groups (
    id TEXT PRIMARY KEY NOT NULL,
    name TEXT NOT NULL,
    description TEXT,

    combined_priority INTEGER NOT NULL DEFAULT 5,
    task_count INTEGER NOT NULL DEFAULT 0,

    status TEXT NOT NULL DEFAULT 'pending',

    created_at BIGINT NOT NULL DEFAULT EXTRACT(EPOCH FROM NOW())::BIGINT,
    exported_at BIGINT
);

-- ============================================================================
-- Processing Stats (for cost tracking)
-- ============================================================================

CREATE TABLE IF NOT EXISTS llm_usage (
    id BIGSERIAL PRIMARY KEY,
    task_id TEXT,
    operation TEXT NOT NULL,
    tokens_input INTEGER NOT NULL DEFAULT 0,
    tokens_output INTEGER NOT NULL DEFAULT 0,
    cost_usd REAL,
    model TEXT DEFAULT 'grok-4.1',
    created_at BIGINT NOT NULL DEFAULT EXTRACT(EPOCH FROM NOW())::BIGINT,

    FOREIGN KEY (task_id) REFERENCES tasks(id)
);

CREATE INDEX IF NOT EXISTS idx_llm_usage_created ON llm_usage(created_at);

-- ============================================================================
-- Views for common queries
-- ============================================================================

CREATE OR REPLACE VIEW v_task_queue AS
SELECT
    t.*,
    g.name AS group_name,
    g.task_count AS group_size
FROM tasks t
LEFT JOIN task_groups g ON t.group_id = g.id
WHERE t.status IN ('pending', 'review', 'ready')
ORDER BY t.priority DESC, t.created_at ASC;

CREATE OR REPLACE VIEW v_daily_stats AS
SELECT
    TO_CHAR(TO_TIMESTAMP(created_at), 'YYYY-MM-DD') AS day,
    COUNT(*) AS tasks_created,
    SUM(CASE WHEN status = 'done' THEN 1 ELSE 0 END) AS tasks_completed,
    SUM(tokens_used) AS total_tokens
FROM tasks
GROUP BY TO_CHAR(TO_TIMESTAMP(created_at), 'YYYY-MM-DD')
ORDER BY day DESC;

-- ============================================================================
-- Triggers for updated_at
-- ============================================================================

CREATE OR REPLACE FUNCTION set_updated_at()
RETURNS TRIGGER AS $$
BEGIN
    NEW.updated_at = EXTRACT(EPOCH FROM NOW())::BIGINT;
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS tasks_updated_at ON tasks;
CREATE TRIGGER tasks_updated_at
BEFORE UPDATE ON tasks
FOR EACH ROW EXECUTE FUNCTION set_updated_at();

DROP TRIGGER IF EXISTS repos_updated_at ON repositories;
CREATE TRIGGER repos_updated_at
BEFORE UPDATE ON repositories
FOR EACH ROW EXECUTE FUNCTION set_updated_at();
