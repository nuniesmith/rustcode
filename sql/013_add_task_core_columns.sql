-- Migration: 013_add_task_core_columns.sql
-- Rewritten for PostgreSQL
-- Adds columns expected by db/core.rs Task struct and auto_scanner create_task().
-- The tasks table was originally created by 001_simplified_tasks.sql with a
-- different column set (content, source_type, source_repo, source_file, etc.).
-- The consolidated code path uses title/description/source/repo_id/file_path
-- instead, so we add them here as nullable to coexist with the legacy columns.
-- Changes from SQLite version:
--   - ADD COLUMN IF NOT EXISTS (safe to re-run in Postgres)
--   - REFERENCES repositories(id) is a proper FK constraint

ALTER TABLE tasks ADD COLUMN IF NOT EXISTS title       TEXT;
ALTER TABLE tasks ADD COLUMN IF NOT EXISTS description TEXT;
ALTER TABLE tasks ADD COLUMN IF NOT EXISTS source      TEXT DEFAULT 'manual';
ALTER TABLE tasks ADD COLUMN IF NOT EXISTS source_id   TEXT;
ALTER TABLE tasks ADD COLUMN IF NOT EXISTS repo_id     TEXT REFERENCES repositories(id);
ALTER TABLE tasks ADD COLUMN IF NOT EXISTS file_path   TEXT;
ALTER TABLE tasks ADD COLUMN IF NOT EXISTS line_number INTEGER;
