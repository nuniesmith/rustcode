-- Migration: 009_queue_items.sql
-- Rewritten for PostgreSQL
-- Changes from SQLite version:
--   - INTEGER PRIMARY KEY AUTOINCREMENT → BIGSERIAL PRIMARY KEY
--   - strftime('%s', 'now') → EXTRACT(EPOCH FROM NOW())::BIGINT
--   - BOOLEAN stored as 0/1 INTEGER → kept as INTEGER for Rust compat
--   - All timestamp columns → BIGINT (Unix epoch seconds)

-- ============================================================================
-- Queue Items Table
-- ============================================================================

CREATE TABLE IF NOT EXISTS queue_items (
    id             TEXT    PRIMARY KEY,
    content        TEXT    NOT NULL,
    stage          TEXT    NOT NULL DEFAULT 'inbox',
    source         TEXT    NOT NULL DEFAULT 'note',
    priority       INTEGER NOT NULL DEFAULT 3,
    repo_id        TEXT,
    file_path      TEXT,
    line_number    INTEGER,
    analysis       TEXT,
    tags           TEXT,
    category       TEXT,
    score          INTEGER,
    retry_count    INTEGER NOT NULL DEFAULT 0,
    last_error     TEXT,
    content_hash   TEXT    NOT NULL,
    created_at     BIGINT  NOT NULL,
    updated_at     BIGINT  NOT NULL,
    processed_at   BIGINT,
    FOREIGN KEY (repo_id) REFERENCES repositories(id)
);

-- ============================================================================
-- File Analysis Cache
-- ============================================================================

CREATE TABLE IF NOT EXISTS file_analysis (
    id                TEXT    PRIMARY KEY,
    repo_id           TEXT    NOT NULL,
    file_path         TEXT    NOT NULL,
    extension         TEXT,
    content_hash      TEXT    NOT NULL,
    size_bytes        INTEGER NOT NULL,
    line_count        INTEGER NOT NULL,
    summary           TEXT,
    purpose           TEXT,
    language          TEXT,
    complexity_score  INTEGER,
    quality_score     INTEGER,
    security_notes    TEXT,
    improvements      TEXT,
    dependencies      TEXT,
    exports           TEXT,
    tags              TEXT,
    needs_attention   INTEGER NOT NULL DEFAULT 0,
    analyzed_at       BIGINT,
    created_at        BIGINT  NOT NULL,
    updated_at        BIGINT  NOT NULL,
    UNIQUE(repo_id, file_path),
    FOREIGN KEY (repo_id) REFERENCES repositories(id)
);

-- ============================================================================
-- TODO Items
-- ============================================================================

CREATE TABLE IF NOT EXISTS todo_items (
    id              TEXT    PRIMARY KEY,
    repo_id         TEXT    NOT NULL,
    file_path       TEXT    NOT NULL,
    line_number     INTEGER NOT NULL,
    content         TEXT    NOT NULL,
    todo_type       TEXT    NOT NULL,
    priority        INTEGER,
    context         TEXT,
    estimated_effort REAL,
    task_id         TEXT,
    content_hash    TEXT    NOT NULL,
    is_active       INTEGER NOT NULL DEFAULT 1,
    created_at      BIGINT  NOT NULL,
    updated_at      BIGINT  NOT NULL,
    FOREIGN KEY (repo_id)  REFERENCES repositories(id),
    FOREIGN KEY (task_id)  REFERENCES tasks(id)
);

-- ============================================================================
-- Repository Cache Metadata
-- ============================================================================

CREATE TABLE IF NOT EXISTS repo_cache (
    id                       TEXT    PRIMARY KEY,
    repo_id                  TEXT    NOT NULL UNIQUE,
    dir_tree                 TEXT,
    total_files              INTEGER NOT NULL DEFAULT 0,
    analyzed_files           INTEGER NOT NULL DEFAULT 0,
    total_todos              INTEGER NOT NULL DEFAULT 0,
    active_todos             INTEGER NOT NULL DEFAULT 0,
    health_score             INTEGER,
    languages                TEXT,
    patterns                 TEXT,
    standardization_issues   TEXT,
    last_scan_at             BIGINT,
    tree_updated_at          BIGINT,
    created_at               BIGINT  NOT NULL,
    updated_at               BIGINT  NOT NULL,
    FOREIGN KEY (repo_id) REFERENCES repositories(id)
);

-- ============================================================================
-- Indexes
-- ============================================================================

CREATE INDEX IF NOT EXISTS idx_queue_stage    ON queue_items(stage);
CREATE INDEX IF NOT EXISTS idx_queue_priority ON queue_items(priority, created_at);
CREATE INDEX IF NOT EXISTS idx_queue_repo     ON queue_items(repo_id);
CREATE INDEX IF NOT EXISTS idx_queue_hash     ON queue_items(content_hash);

CREATE INDEX IF NOT EXISTS idx_file_analysis_repo       ON file_analysis(repo_id);
CREATE INDEX IF NOT EXISTS idx_file_analysis_path       ON file_analysis(repo_id, file_path);
CREATE INDEX IF NOT EXISTS idx_file_analysis_attention  ON file_analysis(needs_attention)
    WHERE needs_attention = 1;

CREATE INDEX IF NOT EXISTS idx_todo_repo    ON todo_items(repo_id);
CREATE INDEX IF NOT EXISTS idx_todo_file    ON todo_items(repo_id, file_path);
CREATE INDEX IF NOT EXISTS idx_todo_active  ON todo_items(is_active) WHERE is_active = 1;
CREATE INDEX IF NOT EXISTS idx_todo_task    ON todo_items(task_id)   WHERE task_id IS NOT NULL;

-- ============================================================================
-- Migration complete
-- ============================================================================
