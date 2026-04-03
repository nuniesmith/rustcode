-- Migration 015: registered_repos table
-- Rewritten for PostgreSQL
-- Backs the in-memory RepoSyncService.repos HashMap with persistent storage.
-- Repos registered via POST /api/v1/repos survive server restarts.
-- Changes from SQLite version:
--   - strftime('%s', 'now') → EXTRACT(EPOCH FROM NOW())::BIGINT
--   - BOOLEAN column uses native TRUE/FALSE literals
--   - Partial unique index on active=TRUE (Postgres supports WHERE clauses on unique indexes)
--   - Trigger uses PL/pgSQL function (set_updated_at defined in migration 001)
--   - CREATE TRIGGER IF NOT EXISTS not supported — use DROP + CREATE

CREATE TABLE IF NOT EXISTS registered_repos (
    id          TEXT    PRIMARY KEY,
    name        TEXT    NOT NULL,
    local_path  TEXT    NOT NULL,
    remote_url  TEXT,
    branch      TEXT    NOT NULL DEFAULT 'main',
    last_synced BIGINT,
    active      BOOLEAN NOT NULL DEFAULT TRUE,
    created_at  BIGINT  NOT NULL DEFAULT EXTRACT(EPOCH FROM NOW())::BIGINT,
    updated_at  BIGINT  NOT NULL DEFAULT EXTRACT(EPOCH FROM NOW())::BIGINT
);

-- Fast lookup by local path — only enforce uniqueness among active repos
CREATE UNIQUE INDEX IF NOT EXISTS idx_registered_repos_local_path
    ON registered_repos (local_path)
    WHERE active = TRUE;

-- Filter active repos quickly (common query in list_repos)
CREATE INDEX IF NOT EXISTS idx_registered_repos_active
    ON registered_repos (active);

-- Trigger: keep updated_at current on every UPDATE
DROP TRIGGER IF EXISTS trg_registered_repos_updated_at ON registered_repos;
CREATE TRIGGER trg_registered_repos_updated_at
BEFORE UPDATE ON registered_repos
FOR EACH ROW EXECUTE FUNCTION set_updated_at();
