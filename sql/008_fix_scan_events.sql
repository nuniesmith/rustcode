-- Migration: 008_fix_scan_events.sql
-- Rewritten for PostgreSQL
-- Rebuilds scan_events with the correct schema (details, level columns, nullable repo_id)
-- and recreates the dependent views.
-- The original SQLite version used RENAME + recreate; PostgreSQL supports ALTER TABLE
-- ADD COLUMN IF NOT EXISTS directly, so we take the simpler path when the column is missing.

-- ============================================================================
-- Step 1: Ensure scan_events has the correct schema
-- (Migration 003 already creates scan_events with the full schema in the
--  Postgres rewrite, so this migration is a no-op if 003 ran first.
--  The ADD COLUMN IF NOT EXISTS guards make it safe either way.)
-- ============================================================================

ALTER TABLE scan_events ADD COLUMN IF NOT EXISTS details  TEXT;
ALTER TABLE scan_events ADD COLUMN IF NOT EXISTS level    TEXT NOT NULL DEFAULT 'info';

-- Make repo_id nullable if it isn't already (Postgres allows this)
ALTER TABLE scan_events ALTER COLUMN repo_id DROP NOT NULL;

-- Ensure the level index exists
CREATE INDEX IF NOT EXISTS idx_scan_events_level ON scan_events(level, created_at DESC);

-- ============================================================================
-- Step 2: Drop and recreate views that reference scan_events
-- ============================================================================

-- recent_scan_activity
DROP VIEW IF EXISTS recent_scan_activity;
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

-- repository_health (fixed: use scan_interval_mins column, not alias)
DROP VIEW IF EXISTS repository_health;
CREATE OR REPLACE VIEW repository_health AS
SELECT
    id,
    name,
    scan_status,
    auto_scan,
    scan_interval_mins                                                        AS scan_interval_minutes,
    last_scan_duration_ms,
    last_scan_files_found,
    last_scan_issues_found,
    CASE
        WHEN last_error      IS NOT NULL THEN 'unhealthy'
        WHEN scan_status      = 'error'  THEN 'unhealthy'
        WHEN last_scanned_at  IS NULL    THEN 'never_scanned'
        WHEN (EXTRACT(EPOCH FROM NOW())::BIGINT - last_scanned_at)
             > (scan_interval_mins * 60 * 2)                       THEN 'stale'
        ELSE 'healthy'
    END AS health_status,
    TO_CHAR(TO_TIMESTAMP(last_scanned_at), 'YYYY-MM-DD HH24:MI:SS') AS last_scan
FROM repositories;

-- active_scans
DROP VIEW IF EXISTS active_scans;
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

-- ============================================================================
-- Migration complete
-- ============================================================================
