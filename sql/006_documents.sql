-- Migration: 006_documents.sql
-- Rewritten for PostgreSQL
-- Changes from SQLite version:
--   - INTEGER PRIMARY KEY AUTOINCREMENT → BIGSERIAL PRIMARY KEY (for llm_usage)
--   - strftime('%s', 'now') → EXTRACT(EPOCH FROM NOW())::BIGINT
--   - CREATE VIEW IF NOT EXISTS → CREATE OR REPLACE VIEW
--   - SQLite trigger syntax → PL/pgSQL functions + CREATE TRIGGER
--   - INSERT OR IGNORE → INSERT ... ON CONFLICT DO NOTHING
--   - GROUP_CONCAT → STRING_AGG
--   - datetime(..., 'unixepoch') → TO_CHAR(TO_TIMESTAMP(...), ...)
--   - Added tsvector column for full-text search on documents

-- ============================================================================
-- Documents Table
-- ============================================================================

CREATE TABLE IF NOT EXISTS documents (
    id           TEXT    PRIMARY KEY,
    title        TEXT    NOT NULL,
    content      TEXT    NOT NULL,
    content_type TEXT    DEFAULT 'markdown'
                         CHECK(content_type IN ('markdown', 'text', 'code', 'html')),
    source_type  TEXT    DEFAULT 'manual'
                         CHECK(source_type IN ('manual', 'url', 'file', 'repo')),
    source_url   TEXT,
    doc_type     TEXT    DEFAULT 'reference'
                         CHECK(doc_type IN ('reference', 'research', 'tutorial', 'architecture', 'note', 'snippet')),
    tags         TEXT,
    repo_id      TEXT,
    file_path    TEXT,
    word_count   INTEGER DEFAULT 0,
    char_count   INTEGER DEFAULT 0,
    created_at   BIGINT  NOT NULL DEFAULT EXTRACT(EPOCH FROM NOW())::BIGINT,
    updated_at   BIGINT  NOT NULL DEFAULT EXTRACT(EPOCH FROM NOW())::BIGINT,
    indexed_at   BIGINT,

    -- Full-text search vector (auto-maintained by trigger)
    search_vector tsvector,

    FOREIGN KEY (repo_id) REFERENCES repositories(id) ON DELETE SET NULL
);

-- ============================================================================
-- Document Chunks Table
-- ============================================================================

CREATE TABLE IF NOT EXISTS document_chunks (
    id          TEXT    PRIMARY KEY,
    document_id TEXT    NOT NULL,
    chunk_index INTEGER NOT NULL,
    content     TEXT    NOT NULL,
    char_start  INTEGER NOT NULL,
    char_end    INTEGER NOT NULL,
    word_count  INTEGER DEFAULT 0,
    heading     TEXT,
    created_at  BIGINT  NOT NULL DEFAULT EXTRACT(EPOCH FROM NOW())::BIGINT,
    FOREIGN KEY (document_id) REFERENCES documents(id) ON DELETE CASCADE,
    UNIQUE(document_id, chunk_index)
);

-- ============================================================================
-- Document Embeddings Table
-- ============================================================================

CREATE TABLE IF NOT EXISTS document_embeddings (
    id         TEXT    PRIMARY KEY,
    chunk_id   TEXT    NOT NULL UNIQUE,
    embedding  TEXT    NOT NULL,
    model      TEXT    NOT NULL DEFAULT 'mxbai-embed-large-v1',
    dimension  INTEGER NOT NULL DEFAULT 1024,
    created_at BIGINT  NOT NULL DEFAULT EXTRACT(EPOCH FROM NOW())::BIGINT,
    FOREIGN KEY (chunk_id) REFERENCES document_chunks(id) ON DELETE CASCADE
);

-- ============================================================================
-- Document Tags Junction Table
-- ============================================================================

CREATE TABLE IF NOT EXISTS document_tags (
    document_id TEXT   NOT NULL,
    tag         TEXT   NOT NULL,
    created_at  BIGINT NOT NULL DEFAULT EXTRACT(EPOCH FROM NOW())::BIGINT,
    PRIMARY KEY (document_id, tag),
    FOREIGN KEY (document_id) REFERENCES documents(id) ON DELETE CASCADE,
    FOREIGN KEY (tag)         REFERENCES tags(name)    ON DELETE CASCADE
);

-- ============================================================================
-- Indexes
-- ============================================================================

CREATE INDEX IF NOT EXISTS idx_documents_repo_id  ON documents(repo_id)    WHERE repo_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_documents_doc_type ON documents(doc_type);
CREATE INDEX IF NOT EXISTS idx_documents_created  ON documents(created_at DESC);
CREATE INDEX IF NOT EXISTS idx_documents_updated  ON documents(updated_at DESC);
CREATE INDEX IF NOT EXISTS idx_documents_indexed  ON documents(indexed_at) WHERE indexed_at IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_documents_fts      ON documents USING GIN(search_vector);

CREATE INDEX IF NOT EXISTS idx_document_chunks_doc_id      ON document_chunks(document_id);
CREATE INDEX IF NOT EXISTS idx_document_chunks_chunk_index ON document_chunks(document_id, chunk_index);

CREATE INDEX IF NOT EXISTS idx_document_embeddings_chunk_id ON document_embeddings(chunk_id);
CREATE INDEX IF NOT EXISTS idx_document_embeddings_model    ON document_embeddings(model);

CREATE INDEX IF NOT EXISTS idx_document_tags_tag ON document_tags(tag);
CREATE INDEX IF NOT EXISTS idx_document_tags_doc ON document_tags(document_id);

-- ============================================================================
-- Trigger: keep search_vector current on insert/update
-- ============================================================================

CREATE OR REPLACE FUNCTION documents_search_vector_fn()
RETURNS TRIGGER AS $$
BEGIN
    NEW.search_vector :=
        setweight(to_tsvector('english', COALESCE(NEW.title, '')), 'A') ||
        setweight(to_tsvector('english', COALESCE(NEW.tags,  '')), 'B') ||
        setweight(to_tsvector('english', COALESCE(NEW.content, '')), 'C');
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS documents_search_vector_update ON documents;
CREATE TRIGGER documents_search_vector_update
BEFORE INSERT OR UPDATE OF title, tags, content ON documents
FOR EACH ROW EXECUTE FUNCTION documents_search_vector_fn();

-- ============================================================================
-- Trigger: auto-update documents.updated_at
-- ============================================================================

DROP TRIGGER IF EXISTS update_document_timestamp ON documents;
CREATE TRIGGER update_document_timestamp
BEFORE UPDATE ON documents
FOR EACH ROW EXECUTE FUNCTION set_updated_at();

-- ============================================================================
-- Triggers: keep document_tags → tags.usage_count in sync
-- ============================================================================

CREATE OR REPLACE FUNCTION doc_tag_insert_fn()
RETURNS TRIGGER AS $$
BEGIN
    UPDATE tags
    SET usage_count = usage_count + 1
    WHERE name = NEW.tag;
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE OR REPLACE FUNCTION doc_tag_delete_fn()
RETURNS TRIGGER AS $$
BEGIN
    UPDATE tags
    SET usage_count = GREATEST(0, usage_count - 1)
    WHERE name = OLD.tag;
    RETURN OLD;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS update_tag_count_on_doc_tag_insert ON document_tags;
CREATE TRIGGER update_tag_count_on_doc_tag_insert
AFTER INSERT ON document_tags
FOR EACH ROW EXECUTE FUNCTION doc_tag_insert_fn();

DROP TRIGGER IF EXISTS update_tag_count_on_doc_tag_delete ON document_tags;
CREATE TRIGGER update_tag_count_on_doc_tag_delete
AFTER DELETE ON document_tags
FOR EACH ROW EXECUTE FUNCTION doc_tag_delete_fn();

-- ============================================================================
-- Views
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
-- Initial Data — Welcome document
-- ============================================================================

INSERT INTO documents (
    id,
    title,
    content,
    content_type,
    source_type,
    doc_type,
    word_count,
    char_count
) VALUES (
    'welcome-doc',
    'Welcome to RustCode RAG System',
    E'# Welcome to RustCode RAG\n\nThis is your knowledge base system powered by semantic search and vector embeddings.\n\n## Features\n\n- **Document Storage**: Store markdown documents, code snippets, research notes\n- **Semantic Search**: Find relevant content using natural language queries\n- **Context Retrieval**: Automatically retrieve relevant context for LLM queries\n- **Tag Organization**: Organize documents with tags for easy filtering\n\n## Getting Started\n\n1. Upload documents via the API\n2. Documents are automatically chunked and indexed\n3. Use the search endpoint to find relevant content\n4. Context is automatically stuffed into LLM prompts\n\nHappy searching! \U0001F980',
    'markdown',
    'manual',
    'tutorial',
    120,
    800
) ON CONFLICT (id) DO NOTHING;

-- ============================================================================
-- Migration Complete
-- ============================================================================
