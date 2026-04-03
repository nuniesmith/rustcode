-- Migration: 007_ideas.sql
-- Rewritten for PostgreSQL
-- Changes from SQLite version:
--   - strftime('%s', 'now') → EXTRACT(EPOCH FROM NOW())::BIGINT
--   - CREATE VIEW IF NOT EXISTS → CREATE OR REPLACE VIEW
--   - SQLite trigger syntax → PL/pgSQL functions + CREATE TRIGGER
--   - INSERT OR IGNORE → INSERT ... ON CONFLICT DO NOTHING
--   - FTS5 virtual table removed — documents full-text search handled by
--     the tsvector column + GIN index added in migration 006

-- ============================================================================
-- Ideas Table
-- ============================================================================

CREATE TABLE IF NOT EXISTS ideas (
    id             TEXT    PRIMARY KEY,
    content        TEXT    NOT NULL,
    tags           TEXT,
    project        TEXT,
    repo_id        TEXT,
    priority       INTEGER NOT NULL DEFAULT 3,
    status         TEXT    NOT NULL DEFAULT 'inbox',
    category       TEXT,
    linked_doc_id  TEXT,
    linked_task_id TEXT,
    created_at     BIGINT  NOT NULL DEFAULT EXTRACT(EPOCH FROM NOW())::BIGINT,
    updated_at     BIGINT  NOT NULL DEFAULT EXTRACT(EPOCH FROM NOW())::BIGINT,
    FOREIGN KEY (repo_id)       REFERENCES repositories(id) ON DELETE SET NULL,
    FOREIGN KEY (linked_doc_id) REFERENCES documents(id)    ON DELETE SET NULL
);

-- ============================================================================
-- Indexes
-- ============================================================================

CREATE INDEX IF NOT EXISTS idx_ideas_status   ON ideas(status);
CREATE INDEX IF NOT EXISTS idx_ideas_priority ON ideas(priority);
CREATE INDEX IF NOT EXISTS idx_ideas_category ON ideas(category);
CREATE INDEX IF NOT EXISTS idx_ideas_project  ON ideas(project);
CREATE INDEX IF NOT EXISTS idx_ideas_created  ON ideas(created_at DESC);
CREATE INDEX IF NOT EXISTS idx_ideas_repo     ON ideas(repo_id) WHERE repo_id IS NOT NULL;

-- ============================================================================
-- Trigger: auto-update ideas.updated_at
-- ============================================================================

DROP TRIGGER IF EXISTS update_idea_timestamp ON ideas;
CREATE TRIGGER update_idea_timestamp
BEFORE UPDATE ON ideas
FOR EACH ROW EXECUTE FUNCTION set_updated_at();

-- ============================================================================
-- Views
-- ============================================================================

CREATE OR REPLACE VIEW active_ideas AS
SELECT
    id,
    content,
    tags,
    project,
    priority,
    status,
    category,
    created_at,
    updated_at,
    CASE priority
        WHEN 1 THEN 'urgent'
        WHEN 2 THEN 'high'
        WHEN 3 THEN 'normal'
        WHEN 4 THEN 'low'
        WHEN 5 THEN 'someday'
        ELSE 'unknown'
    END AS priority_label
FROM ideas
WHERE status IN ('inbox', 'active', 'in_progress')
ORDER BY priority ASC, created_at DESC;

CREATE OR REPLACE VIEW ideas_by_category AS
SELECT
    COALESCE(category, 'uncategorized')                AS category,
    COUNT(*)                                           AS count,
    COUNT(*) FILTER (WHERE status = 'inbox')           AS inbox_count,
    COUNT(*) FILTER (WHERE status = 'active')          AS active_count,
    COUNT(*) FILTER (WHERE status = 'done')            AS done_count
FROM ideas
GROUP BY category
ORDER BY count DESC;

CREATE OR REPLACE VIEW recent_ideas_activity AS
SELECT
    i.id,
    i.content,
    i.status,
    i.category,
    i.priority,
    i.tags,
    i.project,
    r.name                                                             AS repo_name,
    i.created_at,
    i.updated_at,
    TO_CHAR(TO_TIMESTAMP(i.created_at), 'YYYY-MM-DD HH24:MI:SS')     AS created_at_formatted,
    TO_CHAR(TO_TIMESTAMP(i.updated_at), 'YYYY-MM-DD HH24:MI:SS')     AS updated_at_formatted
FROM ideas i
LEFT JOIN repositories r ON i.repo_id = r.id
ORDER BY i.updated_at DESC
LIMIT 100;

-- ============================================================================
-- Sample Data
-- ============================================================================

INSERT INTO ideas (id, content, tags, priority, status, category)
VALUES (
    'welcome-idea',
    'Welcome to the Ideas system! Use this to capture quick thoughts, feature requests, bugs, and todos.',
    'welcome,getting-started',
    3,
    'inbox',
    'random'
) ON CONFLICT (id) DO NOTHING;

-- ============================================================================
-- Migration Complete
-- ============================================================================
