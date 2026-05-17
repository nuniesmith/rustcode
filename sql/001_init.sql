-- ============================================================================
-- RustCode PostgreSQL Initialization Script
-- ============================================================================
-- Creates the rustcode database on the shared FKS PostgreSQL instance
-- and grants the shared fks_user full access.
--
-- This script runs automatically on first container start (empty data volume).
-- RustCode's sqlx migrations handle all schema creation at app startup.
--
-- Location: src/sql/rustcode/001_init.sql
-- ============================================================================

-- Create the rustcode database owned by the shared fks_user.
-- The \gexec trick lets us conditionally create only if it doesn't exist.
SELECT 'CREATE DATABASE rustcode
    WITH
    OWNER     = fks_user
    ENCODING  = ''UTF8''
    LC_COLLATE = ''C''
    LC_CTYPE   = ''C''
    TEMPLATE  = template0'
WHERE NOT EXISTS (
    SELECT FROM pg_database WHERE datname = 'rustcode'
)\gexec

-- Connect to the new database and grant privileges.
\connect rustcode

-- Grant all privileges on the database itself.
GRANT ALL PRIVILEGES ON DATABASE rustcode TO fks_user;

-- Ensure fks_user owns and can use the public schema.
ALTER SCHEMA public OWNER TO fks_user;
GRANT ALL ON SCHEMA public TO fks_user;

-- Future tables/sequences created by sqlx migrations will be owned by fks_user
-- (since migrations run as fks_user via DATABASE_URL). Set default privileges
-- so any objects created by other roles are also accessible to fks_user.
ALTER DEFAULT PRIVILEGES IN SCHEMA public
    GRANT ALL ON TABLES TO fks_user;

ALTER DEFAULT PRIVILEGES IN SCHEMA public
    GRANT ALL ON SEQUENCES TO fks_user;

ALTER DEFAULT PRIVILEGES IN SCHEMA public
    GRANT EXECUTE ON FUNCTIONS TO fks_user;
