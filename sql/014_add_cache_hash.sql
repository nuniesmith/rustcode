-- Migration: 014_add_cache_hash.sql
-- Rewritten for PostgreSQL
-- Adds cache_hash column to repositories table.
-- Changes from SQLite version:
--   - ADD COLUMN IF NOT EXISTS (safe to re-run in Postgres)

ALTER TABLE repositories ADD COLUMN IF NOT EXISTS cache_hash TEXT;

CREATE INDEX IF NOT EXISTS idx_repositories_cache_hash ON repositories(cache_hash)
    WHERE cache_hash IS NOT NULL;
