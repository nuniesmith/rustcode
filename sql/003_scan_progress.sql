-- Migration: 003_scan_progress.sql
-- Rewritten for PostgreSQL
-- Adds scan progress tracking and observability fields to repositories table

-- ============================================================================
-- Add scan progress and status tracking columns to repositories table
-- ============================================================================

ALTER TABLE repositories ADD COLUMN IF NOT EXISTS scan_status TEXT DEFAULT 'idle'
    CHECK(scan_status IN ('idle', 'scanning', 'error'));

ALTER TABLE repositories ADD COLUMN IF NOT EXISTS scan_progress TEXT DEFAULT NULL;
ALTER TABLE repositories ADD COLUMN IF NOT EXISTS scan_current_file TEXT DEFAULT NULL;
ALTER TABLE repositories ADD COLUMN IF NOT EXISTS scan_files_total INTEGER DEFAULT 0;
ALTER TABLE repositories ADD COLUMN IF NOT EXISTS scan_files_processed INTEGER DEFAULT 0;

ALTER TABLE repositories ADD COLUMN IF NOT EXISTS last_scan_duration_ms BIGINT DEFAULT NULL;
ALTER TABLE repositories ADD COLUMN IF NOT EXISTS last_scan_files_found INTEGER DEFAULT 0;
ALTER TABLE repositories ADD COLUMN IF NOT EXISTS last_scan_issues_found INTEGER DEFAULT 0;
ALTER TABLE repositories ADD COLUMN IF NOT EXISTS last_error TEXT DEFAULT NULL;

-- ============================================================================
-- Indexes for performance
-- ============================================================================

CREATE INDEX IF NOT EXISTS idx_repositories_scan_status
    ON repositories(scan_status)
    WHERE scan_status != 'idle';

CREATE INDEX IF NOT EXISTS idx_repositories_auto_scan
    ON repositories(auto_scan, last_scanned_at)
    WHERE auto_scan = 1;

-- ============================================================================
-- scan_events table
-- ============================================================================

CREATE TABLE IF NOT EXISTS scan_events (
    id          BIGSERIAL PRIMARY KEY,
    repo_id     TEXT,
    event_type  TEXT NOT NULL,
    message     TEXT NOT NULL,
    details     TEXT,
    metadata    TEXT,
    level       TEXT NOT NULL DEFAULT 'info',
    created_at  BIGINT NOT NULL DEFAULT EXTRACT(EPOCH FROM NOW())::BIGINT,

    FOREIGN KEY (repo_id) REFERENCES repositories(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_scan_events_created  ON scan_events(created_at DESC);
CREATE INDEX IF NOT EXISTS idx_scan_events_repo     ON scan_events(repo_id, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_scan_events_type     ON scan_events(event_type, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_scan_events_level    ON scan_events(level, created_at DESC);

-- ============================================================================
-- Backfill existing repositories with defaults
-- ============================================================================

UPDATE repositories
SET scan_status = 'idle'
WHERE scan_status IS NULL;

UPDATE repositories
SET last_scan_files_found  = 0,
    last_scan_issues_found = 0
WHERE last_scanned_at IS NOT NULL
  AND (last_scan_files_found IS NULL OR last_scan_issues_found IS NULL);

-- ============================================================================
-- Views
-- ============================================================================

CREATE OR REPLACE VIEW active_scans AS
SELECT
    id,
    name,
    scan_status,
    scan_progress,
    scan_current_file,
    scan_files_processed,
    scan_files_total,
    CASE
        WHEN scan_files_total > 0
            THEN CAST((scan_files_processed * 100.0 / scan_files_total) AS INTEGER)
        ELSE 0
    END AS progress_percentage,
    TO_CHAR(TO_TIMESTAMP(last_scanned_at), 'YYYY-MM-DD HH24:MI:SS') AS scan_started_at
FROM repositories
WHERE scan_status = 'scanning';

CREATE OR REPLACE VIEW recent_scan_activity AS
SELECT
    r.id,
    r.name,
    e.event_type,
    e.message,
    e.details,
    e.metadata,
    e.level,
    TO_CHAR(TO_TIMESTAMP(e.created_at), 'YYYY-MM-DD HH24:MI:SS') AS event_time
FROM scan_events e
LEFT JOIN repositories r ON e.repo_id = r.id
ORDER BY e.created_at DESC
LIMIT 50;

CREATE OR REPLACE VIEW repository_health AS
SELECT
    id,
    name,
    scan_status,
    auto_scan,
    scan_interval_mins AS scan_interval_minutes,
    last_scan_duration_ms,
    last_scan_files_found,
    last_scan_issues_found,
    CASE
        WHEN last_error     IS NOT NULL  THEN 'unhealthy'
        WHEN scan_status     = 'error'   THEN 'unhealthy'
        WHEN last_scanned_at IS NULL     THEN 'never_scanned'
        WHEN (EXTRACT(EPOCH FROM NOW())::BIGINT - last_scanned_at)
             > (scan_interval_mins * 60 * 2)   THEN 'stale'
        ELSE 'healthy'
    END AS health_status,
    TO_CHAR(TO_TIMESTAMP(last_scanned_at), 'YYYY-MM-DD HH24:MI:SS') AS last_scan
FROM repositories;

-- ============================================================================
-- Migration complete
-- ============================================================================
