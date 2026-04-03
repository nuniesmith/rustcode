-- Migration: 011_scan_progress_enhanced.sql
-- Rewritten for PostgreSQL
-- Adds enhanced scan progress tracking columns for real-time UI updates.
-- Changes from SQLite version:
--   - All timestamp/epoch columns → BIGINT
--   - REAL → DOUBLE PRECISION for cost accumulation
--   - ADD COLUMN IF NOT EXISTS (Postgres 9.6+ supports this)

-- Timestamp when the current scan started (unix epoch seconds), used for ETA calculation
ALTER TABLE repositories ADD COLUMN IF NOT EXISTS scan_started_at BIGINT;

-- Accumulated cost in USD for the current scan run
ALTER TABLE repositories ADD COLUMN IF NOT EXISTS scan_cost_accumulated DOUBLE PRECISION DEFAULT 0.0;

-- Number of cache hits during the current scan
ALTER TABLE repositories ADD COLUMN IF NOT EXISTS scan_cache_hits INTEGER DEFAULT 0;

-- Number of API calls (non-cached analyses) during the current scan
ALTER TABLE repositories ADD COLUMN IF NOT EXISTS scan_api_calls INTEGER DEFAULT 0;
