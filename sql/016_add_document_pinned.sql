-- Migration: 016_add_document_pinned.sql
-- Adds a `pinned` boolean column to the `documents` table.
-- Pinned documents are surfaced first in listings and cannot be auto-archived.

ALTER TABLE documents
    ADD COLUMN IF NOT EXISTS pinned BOOLEAN NOT NULL DEFAULT FALSE;

-- Index so we can efficiently query "all pinned docs" without a full scan.
CREATE INDEX IF NOT EXISTS idx_documents_pinned
    ON documents (pinned)
    WHERE pinned = TRUE;

-- ============================================================================
-- Migration Complete
-- ============================================================================
