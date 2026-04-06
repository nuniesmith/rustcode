// # Code Chunks Database Module
//
// SQLite schema and CRUD operations for persisting code chunk metadata
// and the content-addressable dedup index. This enables cross-repo
// deduplication by storing chunk hashes, locations, and embeddings.
//
// ## Tables
//
// - `code_chunks`: Individual code chunks with metadata (hash, entity type, complexity, etc.)
// - `chunk_locations`: Where each chunk appears (repo, file, lines) — many-to-one with chunks
// - `scan_savings`: Records of files skipped or downgraded by static analysis (for cost reporting)
//
// ## Usage
//
// ```rust,no_run
// use rustcode::db::chunks::{ChunkStore, ChunkRecord, ChunkLocationRecord};
//
// # async fn example() -> anyhow::Result<()> {
// # let pool = rustcode::db::init_db(&std::env::var("DATABASE_URL").unwrap_or_else(|_| "postgresql://rustcode:changeme@localhost:5432/rustcode_test".to_string())).await?;
// let store = ChunkStore::new(pool).await?;
//
// // Insert a chunk
// let record = ChunkRecord {
//     content_hash: "abc123".into(),
//     entity_type: "function".into(),
//     entity_name: "process_data".into(),
//     language: "rust".into(),
//     word_count: 150,
//     complexity_score: 12,
//     is_public: true,
//     has_tests: false,
//     is_test_code: false,
//     issue_count: 0,
//     embedding: None,
// };
// store.upsert_chunk(&record).await?;
//
// // Link a location
// let loc = ChunkLocationRecord {
//     content_hash: "abc123".into(),
//     repo_id: "rustcode".into(),
//     file_path: "src/lib.rs".into(),
//     start_line: 10,
//     end_line: 45,
//     entity_name: "process_data".into(),
// };
// store.upsert_location(&loc).await?;
// # Ok(())
// # }
// ```

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};

use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use tracing::{debug, info};

// ---------------------------------------------------------------------------
// Record types
// ---------------------------------------------------------------------------

// A code chunk record for the `code_chunks` table
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChunkRecord {
    // SHA-256 content hash (primary key / dedup key)
    pub content_hash: String,

    // Entity type: function, struct, enum, trait, impl_block, class, module, etc.
    pub entity_type: String,

    // Name of the entity (function name, struct name, etc.)
    pub entity_name: String,

    // Source language (rust, kotlin, python, go, typescript, etc.)
    pub language: String,

    // Word count of the chunk content
    pub word_count: i64,

    // Complexity score (0-100)
    pub complexity_score: i64,

    // Whether the entity is public
    pub is_public: bool,

    // Whether the chunk has associated tests
    pub has_tests: bool,

    // Whether the chunk itself is test code
    pub is_test_code: bool,

    // Number of issues found in this chunk
    pub issue_count: i64,

    // Serialized embedding vector (JSON array of f32), if computed
    pub embedding: Option<String>,
}

// A stored chunk with database timestamps
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredChunk {
    // All fields from ChunkRecord
    pub content_hash: String,
    pub entity_type: String,
    pub entity_name: String,
    pub language: String,
    pub word_count: i64,
    pub complexity_score: i64,
    pub is_public: bool,
    pub has_tests: bool,
    pub is_test_code: bool,
    pub issue_count: i64,
    pub embedding: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub last_analyzed: Option<DateTime<Utc>>,
}

// A location where a chunk appears
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChunkLocationRecord {
    // Content hash (foreign key to code_chunks)
    pub content_hash: String,

    // Repository identifier (name or path)
    pub repo_id: String,

    // File path relative to repo root
    pub file_path: String,

    // Start line in the file (1-based)
    pub start_line: i64,

    // End line in the file (1-based, inclusive)
    pub end_line: i64,

    // Entity name at this location
    pub entity_name: String,
}

// A stored location with database timestamp
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredLocation {
    pub id: i64,
    pub content_hash: String,
    pub repo_id: String,
    pub file_path: String,
    pub start_line: i64,
    pub end_line: i64,
    pub entity_name: String,
    pub created_at: DateTime<Utc>,
}

// Record of a static analysis savings decision
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScanSavingsRecord {
    // Repository identifier
    pub repo_id: String,

    // File path relative to repo root
    pub file_path: String,

    // The recommendation that was applied (SKIP, MINIMAL, STANDARD, DEEP_DIVE)
    pub recommendation: String,

    // Reason for skip (if recommendation was SKIP)
    pub skip_reason: Option<String>,

    // Number of static issues found (without LLM)
    pub static_issue_count: i64,

    // Estimated LLM value score (0.0-1.0)
    pub estimated_llm_value: f64,

    // Estimated cost saved in USD (0.0 if not skipped)
    pub estimated_cost_saved_usd: f64,

    // Whether an LLM call was actually made
    pub llm_called: bool,

    // Actual cost if LLM was called
    pub actual_cost_usd: f64,

    // Scan session identifier (groups savings from one scan run)
    pub scan_session_id: Option<String>,
}

// A stored savings record with database timestamp
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredSavingsRecord {
    pub id: i64,
    pub repo_id: String,
    pub file_path: String,
    pub recommendation: String,
    pub skip_reason: Option<String>,
    pub static_issue_count: i64,
    pub estimated_llm_value: f64,
    pub estimated_cost_saved_usd: f64,
    pub llm_called: bool,
    pub actual_cost_usd: f64,
    pub scan_session_id: Option<String>,
    pub created_at: DateTime<Utc>,
}

// Cross-repo duplicate info
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CrossRepoDuplicate {
    pub content_hash: String,
    pub entity_type: String,
    pub entity_name: String,
    pub language: String,
    pub complexity_score: i64,
    pub location_count: i64,
    pub repos: Vec<String>,
    pub locations: Vec<StoredLocation>,
}

// Savings summary for a scan session or time period
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SavingsSummary {
    pub total_files: i64,
    pub files_skipped: i64,
    pub files_minimal: i64,
    pub files_standard: i64,
    pub files_deep_dive: i64,
    pub total_estimated_savings_usd: f64,
    pub total_actual_cost_usd: f64,
    pub total_static_issues: i64,
    pub llm_calls_avoided: i64,
    pub savings_percent: f64,
}

// Chunk dedup statistics
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DedupStats {
    pub total_chunks: i64,
    pub total_locations: i64,
    pub unique_chunks: i64,
    pub duplicated_chunks: i64,
    pub cross_repo_duplicates: i64,
    pub chunks_with_embeddings: i64,
    pub avg_complexity: f64,
    pub by_language: Vec<(String, i64)>,
    pub by_entity_type: Vec<(String, i64)>,
}

// ---------------------------------------------------------------------------
// ChunkStore
// ---------------------------------------------------------------------------

// Persistent store for code chunks and dedup index
pub struct ChunkStore {
    pool: PgPool,
}

impl ChunkStore {
    // Create a new ChunkStore, initializing the schema if needed
    pub async fn new(pool: PgPool) -> Result<Self> {
        let store = Self { pool };
        store.initialize_schema().await?;
        Ok(store)
    }

    // Initialize all required tables and indexes
    async fn initialize_schema(&self) -> Result<()> {
        // Acquire a session-level advisory lock so that concurrent test threads
        // don't race on `CREATE TABLE IF NOT EXISTS` + `BIGSERIAL` sequence
        // creation or `CREATE INDEX IF NOT EXISTS`, which triggers
        // `pg_type_typname_nsp_index` / `pg_class_relname_nsp_index` unique-
        // constraint violations inside Postgres.
        sqlx::query("SELECT pg_advisory_lock(7483923)")
            .execute(&self.pool)
            .await
            .context("Failed to acquire chunk_store init lock")?;

        let result = self.initialize_schema_inner().await;

        let _ = sqlx::query("SELECT pg_advisory_unlock(7483923)")
            .execute(&self.pool)
            .await;

        result
    }

    async fn initialize_schema_inner(&self) -> Result<()> {
        // Code chunks table — content-addressable by hash
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS code_chunks (
                content_hash TEXT PRIMARY KEY,
                entity_type TEXT NOT NULL,
                entity_name TEXT NOT NULL,
                language TEXT NOT NULL,
                word_count BIGINT NOT NULL DEFAULT 0,
                complexity_score BIGINT NOT NULL DEFAULT 0,
                is_public BOOLEAN NOT NULL DEFAULT FALSE,
                has_tests BOOLEAN NOT NULL DEFAULT FALSE,
                is_test_code BOOLEAN NOT NULL DEFAULT FALSE,
                issue_count BIGINT NOT NULL DEFAULT 0,
                embedding TEXT,
                created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                last_analyzed TIMESTAMPTZ
            )
            "#,
        )
        .execute(&self.pool)
        .await
        .context("Failed to create code_chunks table")?;

        // Chunk locations table — where each chunk appears
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS chunk_locations (
                id BIGSERIAL PRIMARY KEY,
                content_hash TEXT NOT NULL,
                repo_id TEXT NOT NULL,
                file_path TEXT NOT NULL,
                start_line BIGINT NOT NULL,
                end_line BIGINT NOT NULL,
                entity_name TEXT NOT NULL DEFAULT '',
                created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                FOREIGN KEY (content_hash) REFERENCES code_chunks(content_hash)
                    ON DELETE CASCADE,
                UNIQUE(content_hash, repo_id, file_path, start_line)
            )
            "#,
        )
        .execute(&self.pool)
        .await
        .context("Failed to create chunk_locations table")?;

        // Scan savings table — tracks static analysis decisions for cost reporting
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS scan_savings (
                id BIGSERIAL PRIMARY KEY,
                repo_id TEXT NOT NULL,
                file_path TEXT NOT NULL,
                recommendation TEXT NOT NULL,
                skip_reason TEXT,
                static_issue_count BIGINT NOT NULL DEFAULT 0,
                estimated_llm_value DOUBLE PRECISION NOT NULL DEFAULT 0.0,
                estimated_cost_saved_usd DOUBLE PRECISION NOT NULL DEFAULT 0.0,
                llm_called BOOLEAN NOT NULL DEFAULT FALSE,
                actual_cost_usd DOUBLE PRECISION NOT NULL DEFAULT 0.0,
                scan_session_id TEXT,
                created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
            )
            "#,
        )
        .execute(&self.pool)
        .await
        .context("Failed to create scan_savings table")?;

        // Indexes for efficient lookups
        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_chunk_loc_hash ON chunk_locations(content_hash)",
        )
        .execute(&self.pool)
        .await
        .context("Failed to create chunk location hash index")?;

        sqlx::query("CREATE INDEX IF NOT EXISTS idx_chunk_loc_repo ON chunk_locations(repo_id)")
            .execute(&self.pool)
            .await
            .context("Failed to create chunk location repo index")?;

        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_chunk_loc_file ON chunk_locations(repo_id, file_path)",
        )
        .execute(&self.pool)
        .await
        .context("Failed to create chunk location file index")?;

        sqlx::query("CREATE INDEX IF NOT EXISTS idx_chunks_language ON code_chunks(language)")
            .execute(&self.pool)
            .await
            .context("Failed to create chunks language index")?;

        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_chunks_entity_type ON code_chunks(entity_type)",
        )
        .execute(&self.pool)
        .await
        .context("Failed to create chunks entity_type index")?;

        sqlx::query("CREATE INDEX IF NOT EXISTS idx_savings_repo ON scan_savings(repo_id)")
            .execute(&self.pool)
            .await
            .context("Failed to create savings repo index")?;

        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_savings_session ON scan_savings(scan_session_id)",
        )
        .execute(&self.pool)
        .await
        .context("Failed to create savings session index")?;

        sqlx::query("CREATE INDEX IF NOT EXISTS idx_savings_created ON scan_savings(created_at)")
            .execute(&self.pool)
            .await
            .context("Failed to create savings created_at index")?;

        debug!("ChunkStore schema initialized");
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Chunk CRUD
    // -----------------------------------------------------------------------

    // Insert or update a code chunk. If the hash already exists, updates metadata.
    pub async fn upsert_chunk(&self, record: &ChunkRecord) -> Result<()> {
        sqlx::query(
            r#"
            INSERT INTO code_chunks (
                content_hash, entity_type, entity_name, language,
                word_count, complexity_score, is_public, has_tests,
                is_test_code, issue_count, embedding, last_analyzed
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, NOW())
            ON CONFLICT(content_hash) DO UPDATE SET
                entity_type = excluded.entity_type,
                entity_name = excluded.entity_name,
                language = excluded.language,
                word_count = excluded.word_count,
                complexity_score = excluded.complexity_score,
                is_public = excluded.is_public,
                has_tests = excluded.has_tests,
                is_test_code = excluded.is_test_code,
                issue_count = excluded.issue_count,
                embedding = COALESCE(excluded.embedding, code_chunks.embedding),
                updated_at = NOW(),
                last_analyzed = NOW()
            "#,
        )
        .bind(&record.content_hash)
        .bind(&record.entity_type)
        .bind(&record.entity_name)
        .bind(&record.language)
        .bind(record.word_count)
        .bind(record.complexity_score)
        .bind(record.is_public)
        .bind(record.has_tests)
        .bind(record.is_test_code)
        .bind(record.issue_count)
        .bind(&record.embedding)
        .execute(&self.pool)
        .await
        .context("Failed to upsert code chunk")?;

        Ok(())
    }

    // Get a chunk by content hash
    pub async fn get_chunk(&self, content_hash: &str) -> Result<Option<StoredChunk>> {
        let row = sqlx::query_as::<
            _,
            (
                String,
                String,
                String,
                String,
                i64,
                i64,
                bool,
                bool,
                bool,
                i64,
                Option<String>,
                DateTime<Utc>,
                DateTime<Utc>,
                Option<DateTime<Utc>>,
            ),
        >(
            r#"
            SELECT content_hash, entity_type, entity_name, language,
                   word_count, complexity_score, is_public, has_tests,
                   is_test_code, issue_count, embedding, created_at,
                   updated_at, last_analyzed
            FROM code_chunks
            WHERE content_hash = $1
            "#,
        )
        .bind(content_hash)
        .fetch_optional(&self.pool)
        .await
        .context("Failed to get chunk")?;

        Ok(row.map(|r| StoredChunk {
            content_hash: r.0,
            entity_type: r.1,
            entity_name: r.2,
            language: r.3,
            word_count: r.4,
            complexity_score: r.5,
            is_public: r.6,
            has_tests: r.7,
            is_test_code: r.8,
            issue_count: r.9,
            embedding: r.10,
            created_at: r.11,
            updated_at: r.12,
            last_analyzed: r.13,
        }))
    }

    // Check if a content hash already exists in the store
    pub async fn contains(&self, content_hash: &str) -> Result<bool> {
        let row =
            sqlx::query_as::<_, (i64,)>("SELECT COUNT(*) FROM code_chunks WHERE content_hash = $1")
                .bind(content_hash)
                .fetch_one(&self.pool)
                .await
                .context("Failed to check chunk existence")?;

        Ok(row.0 > 0)
    }

    // Delete a chunk and all its locations
    pub async fn delete_chunk(&self, content_hash: &str) -> Result<bool> {
        let result = sqlx::query("DELETE FROM code_chunks WHERE content_hash = $1")
            .bind(content_hash)
            .execute(&self.pool)
            .await
            .context("Failed to delete chunk")?;

        Ok(result.rows_affected() > 0)
    }

    // Batch insert/update chunks (uses a transaction for efficiency)
    pub async fn upsert_chunks_batch(&self, records: &[ChunkRecord]) -> Result<usize> {
        if records.is_empty() {
            return Ok(0);
        }

        let mut tx = self
            .pool
            .begin()
            .await
            .context("Failed to begin transaction")?;
        let mut count = 0;

        for record in records {
            sqlx::query(
                r#"
                INSERT INTO code_chunks (
                    content_hash, entity_type, entity_name, language,
                    word_count, complexity_score, is_public, has_tests,
                    is_test_code, issue_count, embedding, last_analyzed
                )
                VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, NOW())
                ON CONFLICT(content_hash) DO UPDATE SET
                    entity_type = excluded.entity_type,
                    entity_name = excluded.entity_name,
                    word_count = excluded.word_count,
                    complexity_score = excluded.complexity_score,
                    is_public = excluded.is_public,
                    has_tests = excluded.has_tests,
                    is_test_code = excluded.is_test_code,
                    issue_count = excluded.issue_count,
                    embedding = COALESCE(excluded.embedding, code_chunks.embedding),
                    updated_at = NOW(),
                    last_analyzed = NOW()
                "#,
            )
            .bind(&record.content_hash)
            .bind(&record.entity_type)
            .bind(&record.entity_name)
            .bind(&record.language)
            .bind(record.word_count)
            .bind(record.complexity_score)
            .bind(record.is_public)
            .bind(record.has_tests)
            .bind(record.is_test_code)
            .bind(record.issue_count)
            .bind(&record.embedding)
            .execute(&mut *tx)
            .await
            .context("Failed to upsert chunk in batch")?;

            count += 1;
        }

        tx.commit().await.context("Failed to commit chunk batch")?;

        debug!("Upserted {} chunks in batch", count);
        Ok(count)
    }

    // Update the embedding for a chunk
    pub async fn update_embedding(&self, content_hash: &str, embedding_json: &str) -> Result<bool> {
        let result = sqlx::query(
            r#"
            UPDATE code_chunks
            SET embedding = $1, updated_at = NOW()
            WHERE content_hash = $2
            "#,
        )
        .bind(embedding_json)
        .bind(content_hash)
        .execute(&self.pool)
        .await
        .context("Failed to update embedding")?;

        Ok(result.rows_affected() > 0)
    }

    // Get all chunks that don't have embeddings yet
    pub async fn get_chunks_without_embeddings(&self, limit: i64) -> Result<Vec<StoredChunk>> {
        let rows = sqlx::query_as::<
            _,
            (
                String,
                String,
                String,
                String,
                i64,
                i64,
                bool,
                bool,
                bool,
                i64,
                Option<String>,
                DateTime<Utc>,
                DateTime<Utc>,
                Option<DateTime<Utc>>,
            ),
        >(
            r#"
            SELECT content_hash, entity_type, entity_name, language,
                   word_count, complexity_score, is_public, has_tests,
                   is_test_code, issue_count, embedding, created_at,
                   updated_at, last_analyzed
            FROM code_chunks
            WHERE embedding IS NULL
            ORDER BY complexity_score DESC
            LIMIT $1
            "#,
        )
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .context("Failed to get chunks without embeddings")?;

        Ok(rows
            .into_iter()
            .map(|r| StoredChunk {
                content_hash: r.0,
                entity_type: r.1,
                entity_name: r.2,
                language: r.3,
                word_count: r.4,
                complexity_score: r.5,
                is_public: r.6,
                has_tests: r.7,
                is_test_code: r.8,
                issue_count: r.9,
                embedding: r.10,
                created_at: r.11,
                updated_at: r.12,
                last_analyzed: r.13,
            })
            .collect())
    }

    // -----------------------------------------------------------------------
    // Location CRUD
    // -----------------------------------------------------------------------

    // Insert or update a chunk location
    pub async fn upsert_location(&self, loc: &ChunkLocationRecord) -> Result<()> {
        sqlx::query(
            r#"
            INSERT INTO chunk_locations (
                content_hash, repo_id, file_path, start_line, end_line, entity_name
            )
            VALUES ($1, $2, $3, $4, $5, $6)
            ON CONFLICT(content_hash, repo_id, file_path, start_line) DO UPDATE SET
                end_line = excluded.end_line,
                entity_name = excluded.entity_name
            "#,
        )
        .bind(&loc.content_hash)
        .bind(&loc.repo_id)
        .bind(&loc.file_path)
        .bind(loc.start_line)
        .bind(loc.end_line)
        .bind(&loc.entity_name)
        .execute(&self.pool)
        .await
        .context("Failed to upsert chunk location")?;

        Ok(())
    }

    // Batch insert locations (uses a transaction)
    pub async fn upsert_locations_batch(&self, locations: &[ChunkLocationRecord]) -> Result<usize> {
        if locations.is_empty() {
            return Ok(0);
        }

        let mut tx = self
            .pool
            .begin()
            .await
            .context("Failed to begin transaction")?;
        let mut count = 0;

        for loc in locations {
            sqlx::query(
                r#"
                INSERT INTO chunk_locations (
                    content_hash, repo_id, file_path, start_line, end_line, entity_name
                )
                VALUES ($1, $2, $3, $4, $5, $6)
                ON CONFLICT(content_hash, repo_id, file_path, start_line) DO UPDATE SET
                    end_line = excluded.end_line,
                    entity_name = excluded.entity_name
                "#,
            )
            .bind(&loc.content_hash)
            .bind(&loc.repo_id)
            .bind(&loc.file_path)
            .bind(loc.start_line)
            .bind(loc.end_line)
            .bind(&loc.entity_name)
            .execute(&mut *tx)
            .await
            .context("Failed to upsert location in batch")?;

            count += 1;
        }

        tx.commit()
            .await
            .context("Failed to commit location batch")?;

        debug!("Upserted {} locations in batch", count);
        Ok(count)
    }

    // Get all locations for a content hash
    pub async fn get_locations(&self, content_hash: &str) -> Result<Vec<StoredLocation>> {
        let rows = sqlx::query_as::<_, (i64, String, String, String, i64, i64, String, DateTime<Utc>)>(
            r#"
            SELECT id, content_hash, repo_id, file_path, start_line, end_line, entity_name, created_at
            FROM chunk_locations
            WHERE content_hash = $1
            ORDER BY repo_id, file_path
            "#,
        )
        .bind(content_hash)
        .fetch_all(&self.pool)
        .await
        .context("Failed to get chunk locations")?;

        Ok(rows
            .into_iter()
            .map(|r| StoredLocation {
                id: r.0,
                content_hash: r.1,
                repo_id: r.2,
                file_path: r.3,
                start_line: r.4,
                end_line: r.5,
                entity_name: r.6,
                created_at: r.7,
            })
            .collect())
    }

    // Get all locations for a specific file in a repo
    pub async fn get_file_locations(
        &self,
        repo_id: &str,
        file_path: &str,
    ) -> Result<Vec<StoredLocation>> {
        let rows = sqlx::query_as::<_, (i64, String, String, String, i64, i64, String, DateTime<Utc>)>(
            r#"
            SELECT id, content_hash, repo_id, file_path, start_line, end_line, entity_name, created_at
            FROM chunk_locations
            WHERE repo_id = $1 AND file_path = $2
            ORDER BY start_line
            "#,
        )
        .bind(repo_id)
        .bind(file_path)
        .fetch_all(&self.pool)
        .await
        .context("Failed to get file locations")?;

        Ok(rows
            .into_iter()
            .map(|r| StoredLocation {
                id: r.0,
                content_hash: r.1,
                repo_id: r.2,
                file_path: r.3,
                start_line: r.4,
                end_line: r.5,
                entity_name: r.6,
                created_at: r.7,
            })
            .collect())
    }

    // Remove all locations for a specific file (used before re-chunking)
    pub async fn clear_file_locations(&self, repo_id: &str, file_path: &str) -> Result<u64> {
        let result =
            sqlx::query("DELETE FROM chunk_locations WHERE repo_id = $1 AND file_path = $2")
                .bind(repo_id)
                .bind(file_path)
                .execute(&self.pool)
                .await
                .context("Failed to clear file locations")?;

        Ok(result.rows_affected())
    }

    // Remove all locations for a repo (used before full re-scan)
    pub async fn clear_repo_locations(&self, repo_id: &str) -> Result<u64> {
        let result = sqlx::query("DELETE FROM chunk_locations WHERE repo_id = $1")
            .bind(repo_id)
            .execute(&self.pool)
            .await
            .context("Failed to clear repo locations")?;

        Ok(result.rows_affected())
    }

    // -----------------------------------------------------------------------
    // Cross-repo dedup queries
    // -----------------------------------------------------------------------

    // Find chunks that appear in more than one repository
    pub async fn find_cross_repo_duplicates(
        &self,
        min_complexity: i64,
    ) -> Result<Vec<CrossRepoDuplicate>> {
        // Find hashes that appear in multiple repos
        let rows = sqlx::query_as::<_, (String, i64)>(
            r#"
            SELECT cl.content_hash, COUNT(DISTINCT cl.repo_id) as repo_count
            FROM chunk_locations cl
            JOIN code_chunks cc ON cc.content_hash = cl.content_hash
            WHERE cc.complexity_score >= $1
            GROUP BY cl.content_hash
            HAVING COUNT(DISTINCT cl.repo_id) > 1
            ORDER BY COUNT(DISTINCT cl.repo_id) DESC
            LIMIT 100
            "#,
        )
        .bind(min_complexity)
        .fetch_all(&self.pool)
        .await
        .context("Failed to find cross-repo duplicates")?;

        let mut duplicates = Vec::new();

        for (hash, _count) in rows {
            // Get chunk metadata
            if let Some(chunk) = self.get_chunk(&hash).await? {
                // Get all locations
                let locations = self.get_locations(&hash).await?;
                let repos: Vec<String> = locations
                    .iter()
                    .map(|l| l.repo_id.clone())
                    .collect::<std::collections::HashSet<_>>()
                    .into_iter()
                    .collect();

                duplicates.push(CrossRepoDuplicate {
                    content_hash: hash,
                    entity_type: chunk.entity_type,
                    entity_name: chunk.entity_name,
                    language: chunk.language,
                    complexity_score: chunk.complexity_score,
                    location_count: locations.len() as i64,
                    repos,
                    locations,
                });
            }
        }

        Ok(duplicates)
    }

    // Check if a content hash already exists and was analyzed (for skip-on-dedup)
    pub async fn is_already_analyzed(&self, content_hash: &str) -> Result<bool> {
        let row = sqlx::query_as::<_, (i64,)>(
            r#"
            SELECT COUNT(*) FROM code_chunks
            WHERE content_hash = $1 AND last_analyzed IS NOT NULL
            "#,
        )
        .bind(content_hash)
        .fetch_one(&self.pool)
        .await
        .context("Failed to check analysis status")?;

        Ok(row.0 > 0)
    }

    // -----------------------------------------------------------------------
    // Scan savings CRUD
    // -----------------------------------------------------------------------

    // Record a static analysis savings decision
    pub async fn record_savings(&self, record: &ScanSavingsRecord) -> Result<i64> {
        let row: (i64,) = sqlx::query_as(
            r#"
            INSERT INTO scan_savings (
                repo_id, file_path, recommendation, skip_reason,
                static_issue_count, estimated_llm_value,
                estimated_cost_saved_usd, llm_called, actual_cost_usd,
                scan_session_id
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
            RETURNING id
            "#,
        )
        .bind(&record.repo_id)
        .bind(&record.file_path)
        .bind(&record.recommendation)
        .bind(&record.skip_reason)
        .bind(record.static_issue_count)
        .bind(record.estimated_llm_value)
        .bind(record.estimated_cost_saved_usd)
        .bind(record.llm_called)
        .bind(record.actual_cost_usd)
        .bind(&record.scan_session_id)
        .fetch_one(&self.pool)
        .await
        .context("Failed to record scan savings")?;
        let id = row.0;

        Ok(id)
    }

    // Batch record savings (uses a transaction)
    pub async fn record_savings_batch(&self, records: &[ScanSavingsRecord]) -> Result<usize> {
        if records.is_empty() {
            return Ok(0);
        }

        let mut tx = self
            .pool
            .begin()
            .await
            .context("Failed to begin transaction")?;
        let mut count = 0;

        for record in records {
            sqlx::query(
                r#"
                INSERT INTO scan_savings (
                    repo_id, file_path, recommendation, skip_reason,
                    static_issue_count, estimated_llm_value,
                    estimated_cost_saved_usd, llm_called, actual_cost_usd,
                    scan_session_id
                )
                VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
                "#,
            )
            .bind(&record.repo_id)
            .bind(&record.file_path)
            .bind(&record.recommendation)
            .bind(&record.skip_reason)
            .bind(record.static_issue_count)
            .bind(record.estimated_llm_value)
            .bind(record.estimated_cost_saved_usd)
            .bind(record.llm_called)
            .bind(record.actual_cost_usd)
            .bind(&record.scan_session_id)
            .execute(&mut *tx)
            .await
            .context("Failed to record savings in batch")?;

            count += 1;
        }

        tx.commit()
            .await
            .context("Failed to commit savings batch")?;

        debug!("Recorded {} savings entries in batch", count);
        Ok(count)
    }

    // Get savings summary for a scan session
    pub async fn get_session_savings(&self, scan_session_id: &str) -> Result<SavingsSummary> {
        self.get_savings_summary_where("scan_session_id = $1", scan_session_id)
            .await
    }

    // Get savings summary for a repository (all time)
    pub async fn get_repo_savings(&self, repo_id: &str) -> Result<SavingsSummary> {
        self.get_savings_summary_where("repo_id = $1", repo_id)
            .await
    }

    // Get savings summary for today
    pub async fn get_daily_savings(&self) -> Result<SavingsSummary> {
        self.get_savings_summary_where("created_at >= CURRENT_DATE", "")
            .await
    }

    // Internal helper for savings summaries
    async fn get_savings_summary_where(
        &self,
        where_clause: &str,
        bind_value: &str,
    ) -> Result<SavingsSummary> {
        let query = format!(
            r#"
            SELECT
                COUNT(*)::BIGINT as total_files,
                COALESCE(SUM(CASE WHEN recommendation = 'SKIP' THEN 1 ELSE 0 END), 0)::BIGINT as files_skipped,
                COALESCE(SUM(CASE WHEN recommendation = 'MINIMAL' THEN 1 ELSE 0 END), 0)::BIGINT as files_minimal,
                COALESCE(SUM(CASE WHEN recommendation = 'STANDARD' THEN 1 ELSE 0 END), 0)::BIGINT as files_standard,
                COALESCE(SUM(CASE WHEN recommendation = 'DEEP_DIVE' THEN 1 ELSE 0 END), 0)::BIGINT as files_deep_dive,
                COALESCE(SUM(estimated_cost_saved_usd), 0.0)::DOUBLE PRECISION as total_estimated_savings,
                COALESCE(SUM(actual_cost_usd), 0.0)::DOUBLE PRECISION as total_actual_cost,
                COALESCE(SUM(static_issue_count), 0)::BIGINT as total_static_issues,
                COALESCE(SUM(CASE WHEN llm_called = FALSE THEN 1 ELSE 0 END), 0)::BIGINT as llm_calls_avoided
            FROM scan_savings
            WHERE {}
            "#,
            where_clause
        );

        let row = if bind_value.is_empty() {
            sqlx::query_as::<_, (i64, i64, i64, i64, i64, f64, f64, i64, i64)>(&query)
                .fetch_one(&self.pool)
                .await
                .context("Failed to get savings summary")?
        } else {
            sqlx::query_as::<_, (i64, i64, i64, i64, i64, f64, f64, i64, i64)>(&query)
                .bind(bind_value)
                .fetch_one(&self.pool)
                .await
                .context("Failed to get savings summary")?
        };

        let total_possible_cost = row.5 + row.6; // savings + actual
        let savings_percent = if total_possible_cost > 0.0 {
            (row.5 / total_possible_cost) * 100.0
        } else {
            0.0
        };

        Ok(SavingsSummary {
            total_files: row.0,
            files_skipped: row.1,
            files_minimal: row.2,
            files_standard: row.3,
            files_deep_dive: row.4,
            total_estimated_savings_usd: row.5,
            total_actual_cost_usd: row.6,
            total_static_issues: row.7,
            llm_calls_avoided: row.8,
            savings_percent,
        })
    }

    // -----------------------------------------------------------------------
    // Statistics
    // -----------------------------------------------------------------------

    // Get overall dedup statistics
    pub async fn get_dedup_stats(&self) -> Result<DedupStats> {
        // Basic counts
        let counts = sqlx::query_as::<_, (i64, i64, i64)>(
            r#"
            SELECT
                (SELECT COUNT(*) FROM code_chunks) as total_chunks,
                (SELECT COUNT(*) FROM chunk_locations) as total_locations,
                (SELECT COUNT(*) FROM code_chunks WHERE embedding IS NOT NULL) as with_embeddings
            "#,
        )
        .fetch_one(&self.pool)
        .await
        .context("Failed to get dedup counts")?;

        // Duplicated chunks (appear in >1 location)
        let dup_count = sqlx::query_as::<_, (i64,)>(
            r#"
            SELECT COUNT(*) FROM (
                SELECT content_hash
                FROM chunk_locations
                GROUP BY content_hash
                HAVING COUNT(*) > 1
            )
            "#,
        )
        .fetch_one(&self.pool)
        .await
        .context("Failed to get duplicate count")?;

        // Cross-repo duplicates
        let cross_repo = sqlx::query_as::<_, (i64,)>(
            r#"
            SELECT COUNT(*) FROM (
                SELECT content_hash
                FROM chunk_locations
                GROUP BY content_hash
                HAVING COUNT(DISTINCT repo_id) > 1
            )
            "#,
        )
        .fetch_one(&self.pool)
        .await
        .context("Failed to get cross-repo count")?;

        // Average complexity
        let avg_complexity = sqlx::query_as::<_, (f64,)>(
            "SELECT COALESCE(AVG(complexity_score)::DOUBLE PRECISION, 0.0) FROM code_chunks",
        )
        .fetch_one(&self.pool)
        .await
        .context("Failed to get avg complexity")?;

        // By language
        let by_language = sqlx::query_as::<_, (String, i64)>(
            r#"
            SELECT language, COUNT(*) as cnt
            FROM code_chunks
            GROUP BY language
            ORDER BY cnt DESC
            "#,
        )
        .fetch_all(&self.pool)
        .await
        .context("Failed to get language breakdown")?;

        // By entity type
        let by_entity_type = sqlx::query_as::<_, (String, i64)>(
            r#"
            SELECT entity_type, COUNT(*) as cnt
            FROM code_chunks
            GROUP BY entity_type
            ORDER BY cnt DESC
            "#,
        )
        .fetch_all(&self.pool)
        .await
        .context("Failed to get entity type breakdown")?;

        Ok(DedupStats {
            total_chunks: counts.0,
            total_locations: counts.1,
            unique_chunks: counts.0 - dup_count.0,
            duplicated_chunks: dup_count.0,
            cross_repo_duplicates: cross_repo.0,
            chunks_with_embeddings: counts.2,
            avg_complexity: avg_complexity.0,
            by_language,
            by_entity_type,
        })
    }

    // Get total chunk count
    pub async fn chunk_count(&self) -> Result<i64> {
        let row = sqlx::query_as::<_, (i64,)>("SELECT COUNT(*) FROM code_chunks")
            .fetch_one(&self.pool)
            .await
            .context("Failed to count chunks")?;

        Ok(row.0)
    }

    // Get total location count
    pub async fn location_count(&self) -> Result<i64> {
        let row = sqlx::query_as::<_, (i64,)>("SELECT COUNT(*) FROM chunk_locations")
            .fetch_one(&self.pool)
            .await
            .context("Failed to count locations")?;

        Ok(row.0)
    }

    // -----------------------------------------------------------------------
    // Cleanup
    // -----------------------------------------------------------------------

    // Remove orphaned chunks (chunks with no locations)
    pub async fn cleanup_orphaned_chunks(&self) -> Result<u64> {
        let result = sqlx::query(
            r#"
            DELETE FROM code_chunks
            WHERE content_hash NOT IN (
                SELECT DISTINCT content_hash FROM chunk_locations
            )
            "#,
        )
        .execute(&self.pool)
        .await
        .context("Failed to cleanup orphaned chunks")?;

        let deleted = result.rows_affected();
        if deleted > 0 {
            info!("Cleaned up {} orphaned chunks", deleted);
        }

        Ok(deleted)
    }

    // Clear old savings records
    pub async fn clear_old_savings(&self, days: i64) -> Result<u64> {
        let result = sqlx::query(
            r#"
            DELETE FROM scan_savings
            WHERE created_at < NOW() - ($1 || ' days')::INTERVAL
            "#,
        )
        .bind(format!("-{}", days))
        .execute(&self.pool)
        .await
        .context("Failed to clear old savings")?;

        let deleted = result.rows_affected();
        if deleted > 0 {
            info!(
                "Cleared {} savings records older than {} days",
                deleted, days
            );
        }

        Ok(deleted)
    }

    // Get the pool reference (for advanced queries)
    pub fn pool(&self) -> &PgPool {
        &self.pool
    }
}

// ---------------------------------------------------------------------------
// Conversion helpers: CodeChunker types → DB records
//
// NOTE: chunk_to_record, chunk_to_location, and chunks_to_records have been
// moved to src/code_chunker.rs in the root crate.  They reference CodeChunk
// (a root-crate type) and cannot live here now that rustcode-db is a
// standalone crate — doing so would create a circular dependency.
// ---------------------------------------------------------------------------

// Estimated cost of an LLM call for a file of the given character count
// Used to estimate savings when a file is skipped.
// Based on Grok 4.1 Fast pricing: $0.20/M input, $0.50/M output
// Assumes ~4 chars per token, ~30% output ratio
pub fn estimate_llm_cost_for_file(char_count: usize) -> f64 {
    let input_tokens = char_count as f64 / 4.0;
    let output_tokens = input_tokens * 0.3;
    let input_cost = (input_tokens / 1_000_000.0) * 0.20;
    let output_cost = (output_tokens / 1_000_000.0) * 0.50;
    input_cost + output_cost
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    async fn create_test_pool() -> PgPool {
        PgPool::connect(&std::env::var("DATABASE_URL").unwrap_or_else(|_| {
            "postgresql://rustcode:changeme@localhost:5432/rustcode_test".to_string()
        }))
        .await
        .expect("Failed to create test pool")
    }

    // Generate a short unique suffix to avoid hash/name collisions across test runs.
    fn uid() -> String {
        uuid::Uuid::new_v4().to_string()[..8].to_string()
    }

    #[tokio::test]
    async fn test_schema_initialization() {
        let pool = create_test_pool().await;
        let store = ChunkStore::new(pool).await;
        assert!(store.is_ok(), "Schema init should succeed");
    }

    #[tokio::test]
    async fn test_upsert_and_get_chunk() {
        let pool = create_test_pool().await;
        let store = ChunkStore::new(pool).await.unwrap();
        let h = format!("upsert-{}", uid());

        let record = ChunkRecord {
            content_hash: h.clone(),
            entity_type: "function".into(),
            entity_name: "process_data".into(),
            language: "rust".into(),
            word_count: 150,
            complexity_score: 12,
            is_public: true,
            has_tests: false,
            is_test_code: false,
            issue_count: 2,
            embedding: None,
        };

        store.upsert_chunk(&record).await.unwrap();

        let stored = store.get_chunk(&h).await.unwrap();
        assert!(stored.is_some());
        let stored = stored.unwrap();
        assert_eq!(stored.entity_name, "process_data");
        assert_eq!(stored.complexity_score, 12);
        assert!(stored.is_public);
        assert_eq!(stored.issue_count, 2);
    }

    #[tokio::test]
    async fn test_upsert_preserves_embedding() {
        let pool = create_test_pool().await;
        let store = ChunkStore::new(pool).await.unwrap();
        let h = format!("preserve-emb-{}", uid());

        // First insert with embedding
        let record = ChunkRecord {
            content_hash: h.clone(),
            entity_type: "function".into(),
            entity_name: "foo".into(),
            language: "rust".into(),
            word_count: 50,
            complexity_score: 5,
            is_public: false,
            has_tests: false,
            is_test_code: false,
            issue_count: 0,
            embedding: Some("[0.1, 0.2, 0.3]".into()),
        };
        store.upsert_chunk(&record).await.unwrap();

        // Second upsert WITHOUT embedding — should preserve the original
        let record2 = ChunkRecord {
            content_hash: h.clone(),
            entity_type: "function".into(),
            entity_name: "foo_updated".into(),
            language: "rust".into(),
            word_count: 60,
            complexity_score: 7,
            is_public: true,
            has_tests: false,
            is_test_code: false,
            issue_count: 1,
            embedding: None,
        };
        store.upsert_chunk(&record2).await.unwrap();

        let stored = store.get_chunk(&h).await.unwrap().unwrap();
        assert_eq!(stored.entity_name, "foo_updated");
        assert_eq!(stored.complexity_score, 7);
        assert!(stored.is_public);
        // Embedding should be preserved
        assert_eq!(stored.embedding, Some("[0.1, 0.2, 0.3]".into()));
    }

    #[tokio::test]
    async fn test_contains() {
        let pool = create_test_pool().await;
        let store = ChunkStore::new(pool).await.unwrap();
        let h = format!("contains-{}", uid());
        let nonexistent = format!("nonexistent-{}", uid());

        assert!(!store.contains(&nonexistent).await.unwrap());

        let record = ChunkRecord {
            content_hash: h.clone(),
            entity_type: "struct".into(),
            entity_name: "Foo".into(),
            language: "rust".into(),
            word_count: 30,
            complexity_score: 2,
            is_public: true,
            has_tests: false,
            is_test_code: false,
            issue_count: 0,
            embedding: None,
        };
        store.upsert_chunk(&record).await.unwrap();

        assert!(store.contains(&h).await.unwrap());
    }

    #[tokio::test]
    async fn test_locations() {
        let pool = create_test_pool().await;
        let store = ChunkStore::new(pool).await.unwrap();
        let h = format!("hash-loc-{}", uid());
        let repo_a = format!("repo-a-{}", uid());
        let repo_b = format!("repo-b-{}", uid());

        // Insert chunk first
        let chunk = ChunkRecord {
            content_hash: h.clone(),
            entity_type: "function".into(),
            entity_name: "bar".into(),
            language: "rust".into(),
            word_count: 100,
            complexity_score: 10,
            is_public: false,
            has_tests: false,
            is_test_code: false,
            issue_count: 0,
            embedding: None,
        };
        store.upsert_chunk(&chunk).await.unwrap();

        // Add locations in two repos
        let loc1 = ChunkLocationRecord {
            content_hash: h.clone(),
            repo_id: repo_a.clone(),
            file_path: "src/lib.rs".into(),
            start_line: 10,
            end_line: 30,
            entity_name: "bar".into(),
        };
        let loc2 = ChunkLocationRecord {
            content_hash: h.clone(),
            repo_id: repo_b.clone(),
            file_path: "src/utils.rs".into(),
            start_line: 5,
            end_line: 25,
            entity_name: "bar".into(),
        };

        store.upsert_location(&loc1).await.unwrap();
        store.upsert_location(&loc2).await.unwrap();

        let locs = store.get_locations(&h).await.unwrap();
        assert_eq!(locs.len(), 2);

        let file_locs = store
            .get_file_locations(&repo_a, "src/lib.rs")
            .await
            .unwrap();
        assert_eq!(file_locs.len(), 1);
        assert_eq!(file_locs[0].start_line, 10);
    }

    #[tokio::test]
    async fn test_cross_repo_duplicates() {
        let pool = create_test_pool().await;
        let store = ChunkStore::new(pool).await.unwrap();
        let h = format!("dup-hash-{}", uid());
        let rx = format!("repo-x-{}", uid());
        let ry = format!("repo-y-{}", uid());

        // Insert a chunk
        let chunk = ChunkRecord {
            content_hash: h.clone(),
            entity_type: "function".into(),
            entity_name: "shared_util".into(),
            language: "rust".into(),
            word_count: 200,
            complexity_score: 15,
            is_public: true,
            has_tests: false,
            is_test_code: false,
            issue_count: 0,
            embedding: None,
        };
        store.upsert_chunk(&chunk).await.unwrap();

        // Add in two different repos
        store
            .upsert_location(&ChunkLocationRecord {
                content_hash: h.clone(),
                repo_id: rx.clone(),
                file_path: "src/utils.rs".into(),
                start_line: 1,
                end_line: 20,
                entity_name: "shared_util".into(),
            })
            .await
            .unwrap();

        store
            .upsert_location(&ChunkLocationRecord {
                content_hash: h.clone(),
                repo_id: ry.clone(),
                file_path: "src/helpers.rs".into(),
                start_line: 1,
                end_line: 20,
                entity_name: "shared_util".into(),
            })
            .await
            .unwrap();

        // find_cross_repo_duplicates returns ALL cross-repo dups in DB;
        // assert our specific hash is among them.
        let dups = store.find_cross_repo_duplicates(0).await.unwrap();
        let our_dup = dups.iter().find(|d| d.content_hash == h);
        assert!(our_dup.is_some(), "Expected our dup hash to appear");
        let our_dup = our_dup.unwrap();
        assert_eq!(our_dup.repos.len(), 2);
        assert_eq!(our_dup.location_count, 2);
    }

    #[tokio::test]
    async fn test_savings_recording() {
        let pool = create_test_pool().await;
        let store = ChunkStore::new(pool).await.unwrap();
        let session = format!("session-rec-{}", uid());

        let savings = ScanSavingsRecord {
            repo_id: "test_repo".into(),
            file_path: "src/generated.rs".into(),
            recommendation: "SKIP".into(),
            skip_reason: Some("generated code".into()),
            static_issue_count: 0,
            estimated_llm_value: 0.0,
            estimated_cost_saved_usd: 0.005,
            llm_called: false,
            actual_cost_usd: 0.0,
            scan_session_id: Some(session.clone()),
        };
        let id = store.record_savings(&savings).await.unwrap();
        assert!(id > 0);

        let savings2 = ScanSavingsRecord {
            repo_id: "test_repo".into(),
            file_path: "src/main.rs".into(),
            recommendation: "STANDARD".into(),
            skip_reason: None,
            static_issue_count: 3,
            estimated_llm_value: 0.6,
            estimated_cost_saved_usd: 0.0,
            llm_called: true,
            actual_cost_usd: 0.01,
            scan_session_id: Some(session.clone()),
        };
        store.record_savings(&savings2).await.unwrap();

        let summary = store.get_session_savings(&session).await.unwrap();
        assert_eq!(summary.total_files, 2);
        assert_eq!(summary.files_skipped, 1);
        assert_eq!(summary.files_standard, 1);
        assert_eq!(summary.llm_calls_avoided, 1);
        assert!(summary.total_estimated_savings_usd > 0.0);
    }

    #[tokio::test]
    async fn test_batch_operations() {
        let pool = create_test_pool().await;
        let store = ChunkStore::new(pool).await.unwrap();
        let pfx = uid();
        let repo = format!("batch-repo-{}", pfx);

        let chunks: Vec<ChunkRecord> = (0..5)
            .map(|i| ChunkRecord {
                content_hash: format!("batch-{}-hash-{}", pfx, i),
                entity_type: "function".into(),
                entity_name: format!("func_{}", i),
                language: "rust".into(),
                word_count: 50 + i * 10,
                complexity_score: i,
                is_public: i % 2 == 0,
                has_tests: false,
                is_test_code: false,
                issue_count: 0,
                embedding: None,
            })
            .collect();

        let count = store.upsert_chunks_batch(&chunks).await.unwrap();
        assert_eq!(count, 5);

        // chunk_count() is global; assert we inserted at least 5
        let total = store.chunk_count().await.unwrap();
        assert!(total >= 5);

        let locations: Vec<ChunkLocationRecord> = (0..5)
            .map(|i| ChunkLocationRecord {
                content_hash: format!("batch-{}-hash-{}", pfx, i),
                repo_id: repo.clone(),
                file_path: format!("src/file_{}.rs", i),
                start_line: 1,
                end_line: 20,
                entity_name: format!("func_{}", i),
            })
            .collect();

        let loc_count = store.upsert_locations_batch(&locations).await.unwrap();
        assert_eq!(loc_count, 5);

        // location_count() is global; assert we inserted at least 5
        let total_locs = store.location_count().await.unwrap();
        assert!(total_locs >= 5);
    }

    #[tokio::test]
    async fn test_dedup_stats() {
        let pool = create_test_pool().await;
        let store = ChunkStore::new(pool).await.unwrap();
        let pfx = uid();

        // Insert chunks
        for i in 0..3 {
            store
                .upsert_chunk(&ChunkRecord {
                    content_hash: format!("stats-{}-hash-{}", pfx, i),
                    entity_type: if i == 0 { "function" } else { "struct" }.into(),
                    entity_name: format!("item_{}", i),
                    language: if i < 2 { "rust" } else { "python" }.into(),
                    word_count: 100,
                    complexity_score: 10 * (i + 1),
                    is_public: true,
                    has_tests: false,
                    is_test_code: false,
                    issue_count: 0,
                    embedding: if i == 0 { Some("[0.1]".into()) } else { None },
                })
                .await
                .unwrap();
        }

        // get_dedup_stats() is global; assert our inserts are reflected
        let stats = store.get_dedup_stats().await.unwrap();
        assert!(stats.total_chunks >= 3);
        assert!(stats.chunks_with_embeddings >= 1);
        assert!(stats.avg_complexity > 0.0);
        assert!(!stats.by_language.is_empty());
        assert!(!stats.by_entity_type.is_empty());
    }

    #[tokio::test]
    async fn test_cleanup_orphaned_chunks() {
        let pool = create_test_pool().await;
        let store = ChunkStore::new(pool).await.unwrap();
        let pfx = uid();
        let orphan_h = format!("orphan-{}", pfx);
        let linked_h = format!("linked-{}", pfx);
        let repo = format!("repo-cleanup-{}", pfx);

        // Insert chunk without any location
        store
            .upsert_chunk(&ChunkRecord {
                content_hash: orphan_h.clone(),
                entity_type: "function".into(),
                entity_name: "orphaned_fn".into(),
                language: "rust".into(),
                word_count: 50,
                complexity_score: 5,
                is_public: false,
                has_tests: false,
                is_test_code: false,
                issue_count: 0,
                embedding: None,
            })
            .await
            .unwrap();

        // Insert chunk with a location
        store
            .upsert_chunk(&ChunkRecord {
                content_hash: linked_h.clone(),
                entity_type: "function".into(),
                entity_name: "linked_fn".into(),
                language: "rust".into(),
                word_count: 50,
                complexity_score: 5,
                is_public: false,
                has_tests: false,
                is_test_code: false,
                issue_count: 0,
                embedding: None,
            })
            .await
            .unwrap();

        store
            .upsert_location(&ChunkLocationRecord {
                content_hash: linked_h.clone(),
                repo_id: repo.clone(),
                file_path: "src/lib.rs".into(),
                start_line: 1,
                end_line: 10,
                entity_name: "linked_fn".into(),
            })
            .await
            .unwrap();

        // cleanup_orphaned_chunks removes ALL orphans in the DB; our orphan
        // must be gone and our linked chunk must still be present.
        store.cleanup_orphaned_chunks().await.unwrap();

        assert!(!store.contains(&orphan_h).await.unwrap());
        assert!(store.contains(&linked_h).await.unwrap());
    }

    #[tokio::test]
    async fn test_delete_chunk_cascades() {
        let pool = create_test_pool().await;
        let store = ChunkStore::new(pool).await.unwrap();
        let h = format!("cascade-{}", uid());
        let repo = format!("repo-cascade-{}", uid());

        store
            .upsert_chunk(&ChunkRecord {
                content_hash: h.clone(),
                entity_type: "function".into(),
                entity_name: "fn_cascade".into(),
                language: "rust".into(),
                word_count: 50,
                complexity_score: 5,
                is_public: false,
                has_tests: false,
                is_test_code: false,
                issue_count: 0,
                embedding: None,
            })
            .await
            .unwrap();

        store
            .upsert_location(&ChunkLocationRecord {
                content_hash: h.clone(),
                repo_id: repo.clone(),
                file_path: "src/lib.rs".into(),
                start_line: 1,
                end_line: 10,
                entity_name: "fn_cascade".into(),
            })
            .await
            .unwrap();

        let deleted = store.delete_chunk(&h).await.unwrap();
        assert!(deleted);

        // Location should also be deleted (CASCADE)
        let locs = store.get_locations(&h).await.unwrap();
        assert!(locs.is_empty());
    }

    #[tokio::test]
    async fn test_clear_file_locations() {
        let pool = create_test_pool().await;
        let store = ChunkStore::new(pool).await.unwrap();
        let h = format!("file-clear-{}", uid());
        let repo = format!("repo-clear-{}", uid());

        store
            .upsert_chunk(&ChunkRecord {
                content_hash: h.clone(),
                entity_type: "function".into(),
                entity_name: "fn_clear".into(),
                language: "rust".into(),
                word_count: 50,
                complexity_score: 5,
                is_public: false,
                has_tests: false,
                is_test_code: false,
                issue_count: 0,
                embedding: None,
            })
            .await
            .unwrap();

        store
            .upsert_location(&ChunkLocationRecord {
                content_hash: h.clone(),
                repo_id: repo.clone(),
                file_path: "src/clear_me.rs".into(),
                start_line: 1,
                end_line: 10,
                entity_name: "fn_clear".into(),
            })
            .await
            .unwrap();

        store
            .upsert_location(&ChunkLocationRecord {
                content_hash: h.clone(),
                repo_id: repo.clone(),
                file_path: "src/keep_me.rs".into(),
                start_line: 1,
                end_line: 10,
                entity_name: "fn_clear".into(),
            })
            .await
            .unwrap();

        let cleared = store
            .clear_file_locations(&repo, "src/clear_me.rs")
            .await
            .unwrap();
        assert_eq!(cleared, 1);

        let remaining = store.get_locations(&h).await.unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].file_path, "src/keep_me.rs");
    }

    #[tokio::test]
    async fn test_update_embedding() {
        let pool = create_test_pool().await;
        let store = ChunkStore::new(pool).await.unwrap();
        let h = format!("embed-test-{}", uid());

        store
            .upsert_chunk(&ChunkRecord {
                content_hash: h.clone(),
                entity_type: "function".into(),
                entity_name: "fn_embed".into(),
                language: "rust".into(),
                word_count: 50,
                complexity_score: 5,
                is_public: false,
                has_tests: false,
                is_test_code: false,
                issue_count: 0,
                embedding: None,
            })
            .await
            .unwrap();

        let updated = store
            .update_embedding(&h, "[0.1, 0.2, 0.3, 0.4]")
            .await
            .unwrap();
        assert!(updated);

        let chunk = store.get_chunk(&h).await.unwrap().unwrap();
        assert_eq!(chunk.embedding, Some("[0.1, 0.2, 0.3, 0.4]".into()));
    }

    #[tokio::test]
    async fn test_chunks_without_embeddings() {
        let pool = create_test_pool().await;
        let store = ChunkStore::new(pool).await.unwrap();
        let pfx = uid();
        let with_h = format!("with-emb-{}", pfx);
        let without_h = format!("without-emb-{}", pfx);

        // Insert one with embedding, one without
        store
            .upsert_chunk(&ChunkRecord {
                content_hash: with_h.clone(),
                entity_type: "function".into(),
                entity_name: "fn_with".into(),
                language: "rust".into(),
                word_count: 50,
                // Use a very high complexity so this chunk sorts to the front
                // of the `ORDER BY complexity_score DESC` query.
                complexity_score: 9999,
                is_public: false,
                has_tests: false,
                is_test_code: false,
                issue_count: 0,
                embedding: Some("[0.1]".into()),
            })
            .await
            .unwrap();

        store
            .upsert_chunk(&ChunkRecord {
                content_hash: without_h.clone(),
                entity_type: "function".into(),
                entity_name: "fn_without".into(),
                language: "rust".into(),
                word_count: 50,
                complexity_score: 9998,
                is_public: false,
                has_tests: false,
                is_test_code: false,
                issue_count: 0,
                embedding: None,
            })
            .await
            .unwrap();

        // get_chunks_without_embeddings returns top-N by complexity DESC.
        // Fetch a larger batch so that chunks inserted by other parallel tests
        // with equal or higher complexity scores don't push our entry out of
        // the result set.
        let without = store.get_chunks_without_embeddings(100).await.unwrap();
        assert!(
            !without.is_empty(),
            "Expected at least one chunk without embeddings"
        );
        assert!(
            without.iter().any(|c| c.content_hash == without_h),
            "Expected to find '{}' in chunks-without-embeddings result",
            without_h
        );
        assert!(
            without.iter().all(|c| c.content_hash != with_h),
            "Chunk with embedding '{}' must NOT appear in the no-embedding list",
            with_h
        );
    }

    #[tokio::test]
    async fn test_is_already_analyzed() {
        let pool = create_test_pool().await;
        let store = ChunkStore::new(pool).await.unwrap();
        let new_h = format!("new-hash-{}", uid());
        let analyzed_h = format!("analyzed-hash-{}", uid());

        // Not yet inserted
        assert!(!store.is_already_analyzed(&new_h).await.unwrap());

        // Insert with last_analyzed set (upsert_chunk sets it via NOW())
        store
            .upsert_chunk(&ChunkRecord {
                content_hash: analyzed_h.clone(),
                entity_type: "function".into(),
                entity_name: "fn_analyzed".into(),
                language: "rust".into(),
                word_count: 50,
                complexity_score: 5,
                is_public: false,
                has_tests: false,
                is_test_code: false,
                issue_count: 0,
                embedding: None,
            })
            .await
            .unwrap();

        assert!(store.is_already_analyzed(&analyzed_h).await.unwrap());
    }

    #[tokio::test]
    async fn test_estimate_llm_cost() {
        // A 10K char file
        let cost = estimate_llm_cost_for_file(10_000);
        // ~2500 input tokens, ~750 output tokens
        // input: 2500/1M * 0.20 = 0.0005
        // output: 750/1M * 0.50 = 0.000375
        // total ≈ 0.000875
        assert!(cost > 0.0005, "Cost too low: {}", cost);
        assert!(cost < 0.005, "Cost too high: {}", cost);
    }

    #[tokio::test]
    async fn test_savings_batch() {
        let pool = create_test_pool().await;
        let store = ChunkStore::new(pool).await.unwrap();
        let session = format!("batch-session-{}", uid());

        let records: Vec<ScanSavingsRecord> = (0..3)
            .map(|i| ScanSavingsRecord {
                repo_id: "batch_repo".into(),
                file_path: format!("src/file_{}.rs", i),
                recommendation: if i == 0 { "SKIP" } else { "STANDARD" }.into(),
                skip_reason: if i == 0 { Some("trivial".into()) } else { None },
                static_issue_count: i,
                estimated_llm_value: i as f64 * 0.3,
                estimated_cost_saved_usd: if i == 0 { 0.005 } else { 0.0 },
                llm_called: i != 0,
                actual_cost_usd: if i != 0 { 0.01 } else { 0.0 },
                scan_session_id: Some(session.clone()),
            })
            .collect();

        let count = store.record_savings_batch(&records).await.unwrap();
        assert_eq!(count, 3);

        let summary = store.get_session_savings(&session).await.unwrap();
        assert_eq!(summary.total_files, 3);
        assert_eq!(summary.files_skipped, 1);
        assert_eq!(summary.llm_calls_avoided, 1);
    }
}
