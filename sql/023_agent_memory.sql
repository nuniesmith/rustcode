-- sql/023_agent_memory.sql
-- Agent memory store
--
-- Persistent cross-session knowledge: observations, decisions, preferences,
-- patterns, and task outcomes. Populated by `AgentMemory::record(...)`
-- (e.g. from session consolidation in MEM-C) and queried by
-- `AgentMemory::search(query, project, top_k)` to ground subsequent
-- pipeline runs.
--
-- Embeddings are stored as JSON TEXT (one `[f32, ...]` array per row),
-- matching the convention already used by `document_embeddings`. Cosine
-- ranking happens in Rust against the query vector — the row count we
-- expect (a few thousand per project) is small enough that this is
-- cheaper than a pgvector dependency.
--
-- Rows are scoped by `project`: `NULL` = global, anything else =
-- project-specific. Search defaults to "global + matching project".

CREATE TABLE IF NOT EXISTS agent_memory (
    id              UUID         PRIMARY KEY,
    project         TEXT,
    kind            TEXT         NOT NULL,
    content         TEXT         NOT NULL,
    embedding       TEXT         NOT NULL,
    importance      REAL         NOT NULL DEFAULT 0.5,
    created_at      TIMESTAMPTZ  NOT NULL DEFAULT NOW(),
    last_accessed   TIMESTAMPTZ  NOT NULL DEFAULT NOW(),
    access_count    INTEGER      NOT NULL DEFAULT 0
);

CREATE INDEX IF NOT EXISTS idx_agent_memory_project    ON agent_memory(project);
CREATE INDEX IF NOT EXISTS idx_agent_memory_kind       ON agent_memory(kind);
CREATE INDEX IF NOT EXISTS idx_agent_memory_importance ON agent_memory(importance DESC);
CREATE INDEX IF NOT EXISTS idx_agent_memory_created    ON agent_memory(created_at DESC);

COMMENT ON TABLE agent_memory IS
    'Persistent agent memory entries (observations, decisions, preferences, patterns, task outcomes). Created by MEM-A; consumed by MEM-B for prompt injection.';
COMMENT ON COLUMN agent_memory.project IS
    'Project scope: NULL = global, otherwise a project identifier (typically owner/repo).';
COMMENT ON COLUMN agent_memory.kind IS
    'One of: observation, decision, preference, pattern, task_outcome.';
COMMENT ON COLUMN agent_memory.embedding IS
    'JSON-encoded f32 vector. Dimension matches the embedder configured at runtime.';
COMMENT ON COLUMN agent_memory.importance IS
    'Retrieval-ranking weight in [0.0, 1.0]. MEM-D adjusts this based on access patterns.';
