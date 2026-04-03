-- Migration: 012_add_review_requested.sql
-- Rewritten for PostgreSQL
-- Adds review_requested column to repositories table.
-- Changes from SQLite version:
--   - ADD COLUMN IF NOT EXISTS (safe to re-run)
--   - BOOLEAN DEFAULT FALSE instead of INTEGER DEFAULT 0

ALTER TABLE repositories ADD COLUMN IF NOT EXISTS review_requested BOOLEAN NOT NULL DEFAULT FALSE;
