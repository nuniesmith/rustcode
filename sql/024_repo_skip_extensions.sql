-- Migration 024: per-repo skip_extensions override
--
-- Backs the per-repo override for the scanner's file-extension skip list.
-- When NULL (default), scanners fall back to the global
-- SCANNER_SKIP_EXTENSIONS env var / `default_skip_extensions()` constant.
-- When set, the per-repo list fully *replaces* the global default for that
-- repo (see RegisteredRepo::effective_skip_extensions in src/repo/sync.rs)
-- — additive semantics were considered and rejected: replace lets a repo
-- opt back in to globally-skipped extensions (e.g. a lockfile-validation
-- repo that needs to analyze .lock files).
--
-- Stored as TEXT[] (Postgres native array) rather than a comma-separated
-- TEXT to keep the value queryable and avoid the parsing footgun where
-- an extension legitimately containing a comma would round-trip wrong.
-- Extension strings are stored without the leading dot to match the
-- existing `default_skip_extensions()` representation in src/config.rs.

ALTER TABLE registered_repos
    ADD COLUMN IF NOT EXISTS skip_extensions TEXT[] NULL;
