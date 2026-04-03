-- Migration: 020_documents_integer_to_bigint.sql
--
-- Promotes INTEGER (INT4) columns to BIGINT (INT8) in the documents family of
-- tables so they match the i64 Rust struct fields in Document, DocumentChunk,
-- and DocumentEmbedding defined in src/db/core.rs.
--
-- Root cause: migration 006_documents.sql created these columns as INTEGER.
-- Every read path in src/db/documents.rs decodes them as i64 / Option<i64>,
-- which sqlx rejects at runtime with:
--   "mismatched types; Rust type i64 (INT8) is not compatible with SQL type INT4"
--
-- PostgreSQL refuses to ALTER a column type while views reference it, so we
-- must DROP all dependent views first, run the ALTERs, then recreate the views
-- verbatim from migration 006.
--
-- The ALTER statements use USING col::BIGINT for explicit, safe casting.
-- scan_checkpoints columns are also promoted here because auto_scanner.rs
-- decodes them as i64 but migration 010 defined them as INTEGER.

-- ============================================================================
-- Step 1 — Drop all views that reference the columns we are altering.
--           CASCADE drops anything that depends on these views too.
-- ============================================================================

DROP VIEW IF EXISTS document_repo_summary   CASCADE;
DROP VIEW IF EXISTS recent_documents        CASCADE;
DROP VIEW IF EXISTS unindexed_documents     CASCADE;
DROP VIEW IF EXISTS indexed_documents       CASCADE;
DROP VIEW IF EXISTS document_stats          CASCADE;
DROP VIEW IF EXISTS documents_with_tags     CASCADE;

-- ============================================================================
-- Step 2 — Promote documents columns
-- ============================================================================

ALTER TABLE documents
    ALTER COLUMN word_count TYPE BIGINT USING word_count::BIGINT,
    ALTER COLUMN char_count TYPE BIGINT USING char_count::BIGINT;

-- ============================================================================
-- Step 3 — Promote document_chunks columns
-- ============================================================================

ALTER TABLE document_chunks
    ALTER COLUMN chunk_index TYPE BIGINT USING chunk_index::BIGINT,
    ALTER COLUMN char_start  TYPE BIGINT USING char_start::BIGINT,
    ALTER COLUMN char_end    TYPE BIGINT USING char_end::BIGINT,
    ALTER COLUMN word_count  TYPE BIGINT USING word_count::BIGINT;

-- ============================================================================
-- Step 4 — Promote document_embeddings columns
-- ============================================================================

ALTER TABLE document_embeddings
    ALTER COLUMN dimension TYPE BIGINT USING dimension::BIGINT;

-- ============================================================================
-- Step 5 — Promote scan_checkpoints columns
-- ============================================================================

ALTER TABLE IF EXISTS scan_checkpoints
    ALTER COLUMN last_completed_index TYPE BIGINT USING last_completed_index::BIGINT,
    ALTER COLUMN files_analyzed       TYPE BIGINT USING files_analyzed::BIGINT,
    ALTER COLUMN files_cached         TYPE BIGINT USING files_cached::BIGINT,
    ALTER COLUMN total_files          TYPE BIGINT USING total_files::BIGINT;

-- ============================================================================
-- Step 6 — Recreate views verbatim from migration 006_documents.sql
-- ============================================================================

CREATE OR REPLACE VIEW documents_with_tags AS
SELECT
    d.id,
    d.title,
    d.content,
    d.content_type,
    d.source_type,
    d.source_url,
    d.doc_type,
    d.repo_id,
    d.word_count,
    d.char_count,
    d.created_at,
    d.updated_at,
    d.indexed_at,
    STRING_AGG(DISTINCT dt.tag, ',' ORDER BY dt.tag) AS tag_list,
    COUNT(DISTINCT dc.id)                            AS chunk_count,
    COUNT(DISTINCT de.id)                            AS embedding_count
FROM documents d
LEFT JOIN document_tags       dt ON d.id = dt.document_id
LEFT JOIN document_chunks     dc ON d.id = dc.document_id
LEFT JOIN document_embeddings de ON dc.id = de.chunk_id
GROUP BY d.id;

CREATE OR REPLACE VIEW document_stats AS
SELECT
    doc_type,
    COUNT(*)        AS count,
    SUM(word_count) AS total_words,
    AVG(word_count) AS avg_words,
    MAX(word_count) AS max_words,
    MIN(word_count) AS min_words
FROM documents
GROUP BY doc_type;

CREATE OR REPLACE VIEW indexed_documents AS
SELECT
    d.id,
    d.title,
    d.doc_type,
    d.word_count,
    COUNT(DISTINCT dc.id) AS chunk_count,
    COUNT(DISTINCT de.id) AS embedding_count,
    d.updated_at,
    d.indexed_at,
    CASE
        WHEN d.indexed_at IS NULL          THEN 'not_indexed'
        WHEN d.updated_at > d.indexed_at   THEN 'needs_reindex'
        ELSE 'indexed'
    END AS index_status,
    TO_CHAR(TO_TIMESTAMP(d.updated_at), 'YYYY-MM-DD HH24:MI:SS') AS updated_time,
    TO_CHAR(TO_TIMESTAMP(d.indexed_at), 'YYYY-MM-DD HH24:MI:SS') AS indexed_time
FROM documents d
LEFT JOIN document_chunks     dc ON d.id = dc.document_id
LEFT JOIN document_embeddings de ON dc.id = de.chunk_id
GROUP BY d.id;

CREATE OR REPLACE VIEW unindexed_documents AS
SELECT
    id,
    title,
    doc_type,
    word_count,
    updated_at,
    indexed_at
FROM documents
WHERE indexed_at IS NULL OR updated_at > indexed_at
ORDER BY updated_at DESC;

CREATE OR REPLACE VIEW recent_documents AS
SELECT
    d.id,
    d.title,
    d.doc_type,
    d.word_count,
    d.created_at,
    d.updated_at,
    STRING_AGG(DISTINCT dt.tag, ',' ORDER BY dt.tag) AS tags,
    CASE
        WHEN d.created_at = d.updated_at THEN 'created'
        ELSE 'updated'
    END AS activity_type,
    TO_CHAR(TO_TIMESTAMP(d.updated_at), 'YYYY-MM-DD HH24:MI:SS') AS activity_time
FROM documents d
LEFT JOIN document_tags dt ON d.id = dt.document_id
GROUP BY d.id
ORDER BY d.updated_at DESC
LIMIT 50;

CREATE OR REPLACE VIEW document_repo_summary AS
SELECT
    r.id                AS repo_id,
    r.name              AS repo_name,
    COUNT(d.id)         AS document_count,
    SUM(d.word_count)   AS total_words,
    MAX(d.updated_at)   AS last_updated
FROM repositories r
LEFT JOIN documents d ON r.id = d.repo_id
GROUP BY r.id, r.name;

-- ============================================================================
-- Migration complete
-- ============================================================================
