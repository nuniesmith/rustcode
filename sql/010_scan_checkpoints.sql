-- Migration 010: Scan checkpoints for resumable scanning
-- Rewritten for PostgreSQL
-- Changes from SQLite version:
--   - REAL → DOUBLE PRECISION for cumulative_cost
--   - INTEGER for updated_at → BIGINT (Unix epoch seconds)

CREATE TABLE IF NOT EXISTS scan_checkpoints (
    repo_id                TEXT             NOT NULL,
    last_completed_index   INTEGER          NOT NULL,
    last_completed_file    TEXT             NOT NULL,
    files_analyzed         INTEGER          NOT NULL DEFAULT 0,
    files_cached           INTEGER          NOT NULL DEFAULT 0,
    cumulative_cost        DOUBLE PRECISION NOT NULL DEFAULT 0.0,
    total_files            INTEGER          NOT NULL,
    updated_at             BIGINT           NOT NULL,
    PRIMARY KEY (repo_id)
);
