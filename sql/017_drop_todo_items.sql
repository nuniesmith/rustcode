-- Migration: 017_drop_todo_items.sql
-- Drops the legacy `todo_items` table.
--
-- Background:
--   `todo_items` was an early staging table for TODO comments extracted from
--   code. The canonical task store is now the `tasks` table, managed by
--   `db::core::create_task` / `db::core::list_tasks`. The queue processor's
--   `process_tagging` step already writes directly to `tasks` (with a
--   back-link via `source_id`), so `todo_items` is no longer written to and
--   can be safely dropped.
--
-- Safe because:
--   • No application code reads from `todo_items` after batch-010.
--   • `queue_items.id` is preserved as `tasks.source_id` so the link is kept.
--   • The `tasks` table has its own FK to `repositories(id)`.

DROP TABLE IF EXISTS todo_items;

-- Remove the now-redundant index (already dropped with the table, but kept
-- here as an explicit no-op guard in case the index was created outside the
-- table DDL in a future schema variant).
DROP INDEX IF EXISTS idx_todo_repo;
DROP INDEX IF EXISTS idx_todo_active;
DROP INDEX IF EXISTS idx_todo_priority;

-- ============================================================================
-- Migration Complete
-- ============================================================================
