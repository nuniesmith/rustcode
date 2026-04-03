-- Migration: 021_audit_runs.sql
-- Full audit runs — persists per-repo audit jobs with live progress + final JSON report

CREATE TABLE IF NOT EXISTS audit_runs (
    id TEXT PRIMARY KEY NOT NULL,
    repo_id TEXT,                          -- FK to repositories.id (nullable for path-only runs)
    repo_path TEXT NOT NULL,
    repo_name TEXT NOT NULL,

    -- Lifecycle
    status TEXT NOT NULL DEFAULT 'pending',   -- pending | running | completed | failed
    started_at BIGINT,
    completed_at BIGINT,
    created_at BIGINT NOT NULL DEFAULT EXTRACT(EPOCH FROM NOW())::BIGINT,

    -- Live progress (updated as each file is processed)
    files_total INTEGER NOT NULL DEFAULT 0,
    files_done INTEGER NOT NULL DEFAULT 0,
    current_file TEXT,

    -- Counts (updated incrementally and finalized at completion)
    findings_critical INTEGER NOT NULL DEFAULT 0,
    findings_high INTEGER NOT NULL DEFAULT 0,
    findings_medium INTEGER NOT NULL DEFAULT 0,
    findings_low INTEGER NOT NULL DEFAULT 0,
    findings_info INTEGER NOT NULL DEFAULT 0,

    -- Final report (set on completion)
    report_markdown TEXT,
    report_json TEXT,

    -- Error tracking
    error_message TEXT,

    -- Cost tracking
    estimated_cost_usd REAL NOT NULL DEFAULT 0.0,

    -- Audit config snapshot
    config_json TEXT
);

CREATE INDEX IF NOT EXISTS idx_audit_runs_repo_id   ON audit_runs(repo_id);
CREATE INDEX IF NOT EXISTS idx_audit_runs_status    ON audit_runs(status);
CREATE INDEX IF NOT EXISTS idx_audit_runs_created   ON audit_runs(created_at DESC);
CREATE INDEX IF NOT EXISTS idx_audit_runs_repo_path ON audit_runs(repo_path);
