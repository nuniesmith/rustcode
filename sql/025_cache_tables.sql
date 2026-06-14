-- sql/025_cache_tables.sql
-- CLEANUP-H: finish the SQLite -> Postgres cache migration.
--
-- The repository analysis cache (`RepoCacheSql`, src/repo/cache.rs) and the
-- LLM response cache (`ResponseCache`, src/cache/responses.rs) were the last
-- two SQLite islands in an otherwise Postgres-only build. Both are now backed
-- by the shared Postgres pool; these tables replace the per-repo `cache.db`
-- files (`~/.cache/rustcode/repos/<hash>/cache.db`) and the global
-- `data/rustcode_cache.db` file respectively.
--
-- Changes from the old SQLite schemas:
--   - BIGSERIAL instead of INTEGER PRIMARY KEY AUTOINCREMENT
--   - BYTEA instead of BLOB for the compressed (zstd) JSON result
--   - TIMESTAMPTZ ... DEFAULT NOW() instead of TEXT ... DEFAULT (datetime('now'))
--   - OCTET_LENGTH(result_blob) replaces SQLite's LENGTH() for byte sizing
--   - cache_entries is a single shared table; rows are scoped by repo_path
--     (the previous design used one SQLite file per repo).


-- ---------------------------------------------------------------------------
-- Repository analysis cache (per-file LLM analysis results, zstd-compressed)
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS cache_entries (
    id              BIGSERIAL    PRIMARY KEY,
    cache_type      TEXT         NOT NULL,
    repo_path       TEXT         NOT NULL,
    file_path       TEXT         NOT NULL,
    file_hash       TEXT         NOT NULL,
    cache_key       TEXT         NOT NULL UNIQUE,
    provider        TEXT         NOT NULL,
    model           TEXT         NOT NULL,
    prompt_hash     TEXT         NOT NULL,
    schema_version  INTEGER      NOT NULL DEFAULT 1,
    result_blob     BYTEA        NOT NULL,
    tokens_used     BIGINT,
    file_size       BIGINT       NOT NULL,
    created_at      TIMESTAMPTZ  NOT NULL DEFAULT NOW(),
    last_accessed   TIMESTAMPTZ  NOT NULL DEFAULT NOW(),
    access_count    BIGINT       NOT NULL DEFAULT 0
);

CREATE INDEX IF NOT EXISTS idx_cache_entries_cache_key     ON cache_entries(cache_key);
CREATE INDEX IF NOT EXISTS idx_cache_entries_cache_type    ON cache_entries(cache_type);
CREATE INDEX IF NOT EXISTS idx_cache_entries_repo_path     ON cache_entries(repo_path);
CREATE INDEX IF NOT EXISTS idx_cache_entries_model         ON cache_entries(model);
CREATE INDEX IF NOT EXISTS idx_cache_entries_created_at    ON cache_entries(created_at);
CREATE INDEX IF NOT EXISTS idx_cache_entries_last_accessed ON cache_entries(last_accessed);

-- Single-row hit/miss counter for the analysis cache (mirrors the old
-- SQLite `cache_stats` table; id is pinned to 1).
CREATE TABLE IF NOT EXISTS cache_stats (
    id            INTEGER      PRIMARY KEY,
    cache_hits    BIGINT       NOT NULL DEFAULT 0,
    cache_misses  BIGINT       NOT NULL DEFAULT 0,
    last_updated  TIMESTAMPTZ  NOT NULL DEFAULT NOW()
);

INSERT INTO cache_stats (id, cache_hits, cache_misses)
VALUES (1, 0, 0)
ON CONFLICT (id) DO NOTHING;


-- ---------------------------------------------------------------------------
-- LLM response cache (content-addressed prompt -> response, with TTL)
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS response_cache (
    id             BIGSERIAL    PRIMARY KEY,
    content_hash   TEXT         NOT NULL UNIQUE,
    operation      TEXT         NOT NULL,
    response       TEXT         NOT NULL,
    created_at     TIMESTAMPTZ  NOT NULL DEFAULT NOW(),
    expires_at     TIMESTAMPTZ  NOT NULL,
    hit_count      BIGINT       NOT NULL DEFAULT 0,
    last_accessed  TIMESTAMPTZ  NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_response_cache_hash      ON response_cache(content_hash);
CREATE INDEX IF NOT EXISTS idx_response_cache_expires   ON response_cache(expires_at);
CREATE INDEX IF NOT EXISTS idx_response_cache_operation ON response_cache(operation);

COMMENT ON TABLE cache_entries IS
    'Per-file LLM analysis results (zstd-compressed JSON). Replaces the per-repo SQLite cache.db; rows scoped by repo_path. Populated by RepoCacheSql (src/repo/cache.rs).';
COMMENT ON TABLE response_cache IS
    'Content-addressed LLM response cache with TTL. Replaces data/rustcode_cache.db. Populated by ResponseCache (src/cache/responses.rs).';
