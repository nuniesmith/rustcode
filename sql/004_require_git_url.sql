-- Migration: 004_require_git_url.sql
-- Rewritten for PostgreSQL
-- Makes git_url required for repositories and adds validation columns

-- ============================================================================
-- Add default git_url for existing repositories without one
-- ============================================================================

UPDATE repositories
SET url = 'https://github.com/unknown/repo.git'
WHERE url IS NULL OR url = '';

-- ============================================================================
-- Add metadata columns for repository configuration
-- ============================================================================

ALTER TABLE repositories ADD COLUMN IF NOT EXISTS source_type TEXT DEFAULT 'git'
    CHECK(source_type IN ('git', 'local', 'external'));

ALTER TABLE repositories ADD COLUMN IF NOT EXISTS clone_depth INTEGER DEFAULT 1;

ALTER TABLE repositories ADD COLUMN IF NOT EXISTS last_sync_at BIGINT;

-- ============================================================================
-- Indexes
-- ============================================================================

CREATE INDEX IF NOT EXISTS idx_repositories_url
    ON repositories(url)
    WHERE url IS NOT NULL;

CREATE INDEX IF NOT EXISTS idx_repositories_sync
    ON repositories(last_sync_at, auto_scan)
    WHERE auto_scan = 1;

-- ============================================================================
-- Backfill source_type for existing rows
-- ============================================================================

UPDATE repositories
SET source_type = 'local'
WHERE local_path NOT LIKE '/app/repos/%'
  AND local_path NOT LIKE '%/repos/%'
  AND source_type IS NULL;

UPDATE repositories
SET source_type = 'git'
WHERE (
    url LIKE 'https://github.com/%'
 OR url LIKE 'https://gitlab.com/%'
 OR url LIKE 'https://bitbucket.org/%'
)
AND source_type IS NULL;

UPDATE repositories
SET clone_depth = 1
WHERE source_type = 'git';

UPDATE repositories
SET last_sync_at = EXTRACT(EPOCH FROM NOW())::BIGINT
WHERE local_path LIKE '/app/repos/%'
   OR local_path LIKE '%/repos/%';

-- ============================================================================
-- View for repository sync status
-- ============================================================================

CREATE OR REPLACE VIEW repository_sync_status AS
SELECT
    id,
    name,
    url,
    source_type,
    clone_depth,
    local_path,
    CASE
        WHEN last_sync_at IS NULL THEN 'never_synced'
        WHEN (EXTRACT(EPOCH FROM NOW())::BIGINT - last_sync_at) > 86400 THEN 'stale'
        WHEN (EXTRACT(EPOCH FROM NOW())::BIGINT - last_sync_at) > 3600  THEN 'needs_update'
        ELSE 'up_to_date'
    END AS sync_status,
    TO_CHAR(TO_TIMESTAMP(last_sync_at), 'YYYY-MM-DD HH24:MI:SS') AS last_sync_time,
    EXTRACT(EPOCH FROM NOW())::BIGINT - COALESCE(last_sync_at, 0) AS seconds_since_sync
FROM repositories
WHERE source_type = 'git';

-- ============================================================================
-- Migration complete
-- ============================================================================
