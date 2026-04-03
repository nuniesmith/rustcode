-- Migration: 018_chunks_integer_to_bigint.sql
-- Alter dynamically-created chunk/savings tables that were originally
-- created with INTEGER columns by ChunkStore::initialize_schema().
-- These tables are NOT created by the numbered migrations, so this
-- migration performs idempotent ALTER COLUMN statements to bring the
-- column types in line with the i64 Rust types used in the codebase.

-- ============================================================================
-- code_chunks
-- ============================================================================

ALTER TABLE IF EXISTS code_chunks
    ALTER COLUMN word_count       TYPE BIGINT USING word_count::BIGINT,
    ALTER COLUMN complexity_score TYPE BIGINT USING complexity_score::BIGINT,
    ALTER COLUMN issue_count      TYPE BIGINT USING issue_count::BIGINT;

-- ============================================================================
-- chunk_locations
-- ============================================================================

ALTER TABLE IF EXISTS chunk_locations
    ALTER COLUMN start_line TYPE BIGINT USING start_line::BIGINT,
    ALTER COLUMN end_line   TYPE BIGINT USING end_line::BIGINT;

-- ============================================================================
-- scan_savings
-- ============================================================================

ALTER TABLE IF EXISTS scan_savings
    ALTER COLUMN static_issue_count TYPE BIGINT USING static_issue_count::BIGINT;

-- ============================================================================
-- llm_costs  (created by CostTracker::initialize_schema)
-- ============================================================================

ALTER TABLE IF EXISTS llm_costs
    ALTER COLUMN input_tokens  TYPE BIGINT USING input_tokens::BIGINT,
    ALTER COLUMN output_tokens TYPE BIGINT USING output_tokens::BIGINT,
    ALTER COLUMN cached_tokens TYPE BIGINT USING COALESCE(cached_tokens, 0)::BIGINT;

-- ============================================================================
-- search_analytics  (created by QueryAnalytics::init_tables)
-- ============================================================================

ALTER TABLE IF EXISTS search_analytics
    ALTER COLUMN result_count      TYPE BIGINT USING result_count::BIGINT,
    ALTER COLUMN execution_time_ms TYPE BIGINT USING execution_time_ms::BIGINT;
