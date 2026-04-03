-- Migration: 019_multi_tenant_tables.sql
--
-- Creates the multi-tenancy tables used by TenantManager.  These tables are
-- also created at runtime by TenantManager::init_tables_inner(), but because
-- CREATE TABLE IF NOT EXISTS is a no-op on an existing table the Rust code
-- alone cannot fix column-type mismatches on a pre-existing database.
--
-- By putting the authoritative DDL here we guarantee:
--   1. A fresh CI database gets the correct BIGINT types from the start.
--   2. A pre-existing database (local dev, staging) gets its INTEGER counter
--      columns promoted to BIGINT via the idempotent ALTER blocks below.
--
-- All counter columns are BIGINT (INT8) to match the i64 Rust fields in
-- TenantUsage and TenantQuota.  Using INTEGER (INT4) causes sqlx to panic
-- with "INT8 is not compatible with INT4" when decoding query results.

-- ============================================================================
-- organizations
-- ============================================================================

CREATE TABLE IF NOT EXISTS organizations (
    id                    TEXT PRIMARY KEY,
    name                  TEXT NOT NULL,
    slug                  TEXT UNIQUE NOT NULL,
    max_documents         BIGINT NOT NULL DEFAULT 10000,
    max_storage_mb        BIGINT NOT NULL DEFAULT 10240,
    max_searches_per_day  BIGINT NOT NULL DEFAULT 100000,
    max_api_keys          BIGINT NOT NULL DEFAULT 10,
    max_webhooks          BIGINT NOT NULL DEFAULT 5,
    enabled               BOOLEAN NOT NULL DEFAULT TRUE,
    created_at            TIMESTAMPTZ DEFAULT NOW(),
    custom_domain         TEXT
);

-- Promote any INTEGER columns that may exist from an older schema.
DO $$
BEGIN
    -- max_documents
    IF EXISTS (
        SELECT 1 FROM information_schema.columns
        WHERE table_schema = 'public' AND table_name = 'organizations'
          AND column_name = 'max_documents' AND data_type = 'integer'
    ) THEN
        ALTER TABLE organizations ALTER COLUMN max_documents TYPE BIGINT;
    END IF;

    -- max_storage_mb
    IF EXISTS (
        SELECT 1 FROM information_schema.columns
        WHERE table_schema = 'public' AND table_name = 'organizations'
          AND column_name = 'max_storage_mb' AND data_type = 'integer'
    ) THEN
        ALTER TABLE organizations ALTER COLUMN max_storage_mb TYPE BIGINT;
    END IF;

    -- max_searches_per_day
    IF EXISTS (
        SELECT 1 FROM information_schema.columns
        WHERE table_schema = 'public' AND table_name = 'organizations'
          AND column_name = 'max_searches_per_day' AND data_type = 'integer'
    ) THEN
        ALTER TABLE organizations ALTER COLUMN max_searches_per_day TYPE BIGINT;
    END IF;

    -- max_api_keys
    IF EXISTS (
        SELECT 1 FROM information_schema.columns
        WHERE table_schema = 'public' AND table_name = 'organizations'
          AND column_name = 'max_api_keys' AND data_type = 'integer'
    ) THEN
        ALTER TABLE organizations ALTER COLUMN max_api_keys TYPE BIGINT;
    END IF;

    -- max_webhooks
    IF EXISTS (
        SELECT 1 FROM information_schema.columns
        WHERE table_schema = 'public' AND table_name = 'organizations'
          AND column_name = 'max_webhooks' AND data_type = 'integer'
    ) THEN
        ALTER TABLE organizations ALTER COLUMN max_webhooks TYPE BIGINT;
    END IF;
END
$$;

-- ============================================================================
-- tenant_usage
-- ============================================================================

CREATE TABLE IF NOT EXISTS tenant_usage (
    tenant_id       TEXT PRIMARY KEY,
    document_count  BIGINT NOT NULL DEFAULT 0,
    storage_mb      BIGINT NOT NULL DEFAULT 0,
    searches_today  BIGINT NOT NULL DEFAULT 0,
    api_key_count   BIGINT NOT NULL DEFAULT 0,
    webhook_count   BIGINT NOT NULL DEFAULT 0,
    last_updated    TIMESTAMPTZ DEFAULT NOW(),
    FOREIGN KEY (tenant_id) REFERENCES organizations(id) ON DELETE CASCADE
);

-- Promote any INTEGER columns that may exist from an older schema.
DO $$
BEGIN
    IF EXISTS (
        SELECT 1 FROM information_schema.columns
        WHERE table_schema = 'public' AND table_name = 'tenant_usage'
          AND column_name = 'document_count' AND data_type = 'integer'
    ) THEN
        ALTER TABLE tenant_usage ALTER COLUMN document_count TYPE BIGINT;
    END IF;

    IF EXISTS (
        SELECT 1 FROM information_schema.columns
        WHERE table_schema = 'public' AND table_name = 'tenant_usage'
          AND column_name = 'storage_mb' AND data_type = 'integer'
    ) THEN
        ALTER TABLE tenant_usage ALTER COLUMN storage_mb TYPE BIGINT;
    END IF;

    IF EXISTS (
        SELECT 1 FROM information_schema.columns
        WHERE table_schema = 'public' AND table_name = 'tenant_usage'
          AND column_name = 'searches_today' AND data_type = 'integer'
    ) THEN
        ALTER TABLE tenant_usage ALTER COLUMN searches_today TYPE BIGINT;
    END IF;

    IF EXISTS (
        SELECT 1 FROM information_schema.columns
        WHERE table_schema = 'public' AND table_name = 'tenant_usage'
          AND column_name = 'api_key_count' AND data_type = 'integer'
    ) THEN
        ALTER TABLE tenant_usage ALTER COLUMN api_key_count TYPE BIGINT;
    END IF;

    IF EXISTS (
        SELECT 1 FROM information_schema.columns
        WHERE table_schema = 'public' AND table_name = 'tenant_usage'
          AND column_name = 'webhook_count' AND data_type = 'integer'
    ) THEN
        ALTER TABLE tenant_usage ALTER COLUMN webhook_count TYPE BIGINT;
    END IF;
END
$$;

-- ============================================================================
-- tenant_usage_history
-- ============================================================================

CREATE TABLE IF NOT EXISTS tenant_usage_history (
    id                   BIGSERIAL PRIMARY KEY,
    tenant_id            TEXT NOT NULL,
    date                 DATE NOT NULL,
    documents_created    BIGINT NOT NULL DEFAULT 0,
    searches_performed   BIGINT NOT NULL DEFAULT 0,
    storage_mb           BIGINT NOT NULL DEFAULT 0,
    FOREIGN KEY (tenant_id) REFERENCES organizations(id) ON DELETE CASCADE,
    UNIQUE(tenant_id, date)
);

-- Promote any INTEGER columns that may exist from an older schema.
DO $$
BEGIN
    IF EXISTS (
        SELECT 1 FROM information_schema.columns
        WHERE table_schema = 'public' AND table_name = 'tenant_usage_history'
          AND column_name = 'documents_created' AND data_type = 'integer'
    ) THEN
        ALTER TABLE tenant_usage_history
            ALTER COLUMN documents_created TYPE BIGINT;
    END IF;

    IF EXISTS (
        SELECT 1 FROM information_schema.columns
        WHERE table_schema = 'public' AND table_name = 'tenant_usage_history'
          AND column_name = 'searches_performed' AND data_type = 'integer'
    ) THEN
        ALTER TABLE tenant_usage_history
            ALTER COLUMN searches_performed TYPE BIGINT;
    END IF;

    IF EXISTS (
        SELECT 1 FROM information_schema.columns
        WHERE table_schema = 'public' AND table_name = 'tenant_usage_history'
          AND column_name = 'storage_mb' AND data_type = 'integer'
    ) THEN
        ALTER TABLE tenant_usage_history
            ALTER COLUMN storage_mb TYPE BIGINT;
    END IF;
END
$$;

-- ============================================================================
-- organizations — also promote quota columns that were INTEGER in early builds
-- ============================================================================
-- (Belt-and-suspenders: the DO block above handles this, but an explicit
--  multi-column ALTER is cleaner for dev databases that have drifted far.)

DO $$
DECLARE
    needs_alter BOOLEAN := FALSE;
BEGIN
    SELECT TRUE INTO needs_alter
    FROM information_schema.columns
    WHERE table_schema = 'public'
      AND table_name   = 'organizations'
      AND column_name  IN ('max_documents','max_storage_mb','max_searches_per_day',
                           'max_api_keys','max_webhooks')
      AND data_type    = 'integer'
    LIMIT 1;

    -- If any column is still INTEGER the single-column ALTERs above already
    -- handled them; this block is just a safety net that re-runs harmlessly.
    IF needs_alter THEN
        RAISE NOTICE 'organizations: some quota columns were still INTEGER — already migrated above';
    END IF;
END
$$;
