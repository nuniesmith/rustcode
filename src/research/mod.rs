// Research System
//
// Spawn multiple parallel research workers to investigate topics,
// aggregate findings, and produce comprehensive reports.

pub mod aggregator;
pub mod worker;

use serde::{Deserialize, Serialize};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

// ============================================================================
// Research Request Model
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct ResearchRequest {
    pub id: String,
    pub topic: String,
    pub description: Option<String>,

    // Type of research: "general", "code", "idea", "comparison"
    pub research_type: String,

    // Scope: how deep to go (stored as string: "quick", "standard", "deep")
    pub depth: String,

    // Related repository (for code research)
    pub repo_context: Option<String>,

    // Related files to consider
    pub file_context: Option<String>,

    // Status of the research
    pub status: String,

    // Number of parallel workers to spawn
    pub worker_count: i32,

    // Final aggregated report
    pub report: Option<String>,

    // Total tokens used across all workers
    pub total_tokens: i64,

    pub created_at: i64,
    pub completed_at: Option<i64>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ResearchDepth {
    // Quick overview, 1-2 workers
    Quick,
    #[default]
    // Standard depth, 3-4 workers
    Standard,
    // Deep dive, 5+ workers
    Deep,
}

impl ResearchDepth {
    pub fn worker_count(&self) -> i32 {
        match self {
            ResearchDepth::Quick => 2,
            ResearchDepth::Standard => 4,
            ResearchDepth::Deep => 6,
        }
    }
}

impl ResearchRequest {
    pub fn new(topic: impl Into<String>, research_type: impl Into<String>) -> Self {
        let depth = ResearchDepth::default();
        let worker_count = depth.worker_count();
        Self {
            id: Uuid::new_v4().to_string(),
            topic: topic.into(),
            description: None,
            research_type: research_type.into(),
            depth: format!("{:?}", depth).to_lowercase(),
            repo_context: None,
            file_context: None,
            status: "pending".to_string(),
            worker_count,
            report: None,
            total_tokens: 0,
            created_at: chrono::Utc::now().timestamp(),
            completed_at: None,
        }
    }

    pub fn with_depth(mut self, depth: ResearchDepth) -> Self {
        self.worker_count = depth.worker_count();
        self.depth = format!("{:?}", depth).to_lowercase();
        self
    }

    // Get the depth as an enum
    pub fn depth_enum(&self) -> ResearchDepth {
        match self.depth.as_str() {
            "quick" => ResearchDepth::Quick,
            "deep" => ResearchDepth::Deep,
            _ => ResearchDepth::Standard,
        }
    }

    pub fn with_context(mut self, repo: Option<String>, files: Option<String>) -> Self {
        self.repo_context = repo;
        self.file_context = files;
        self
    }

    pub fn with_description(mut self, desc: impl Into<String>) -> Self {
        self.description = Some(desc.into());
        self
    }
}

// ============================================================================
// Research Worker Result
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct WorkerResult {
    pub id: String,
    pub research_id: String,
    pub worker_index: i32,

    // The subtopic this worker investigated
    pub subtopic: String,

    // Sources consulted (could be RAG results, web, docs)
    pub sources: Option<String>, // JSON array

    // The worker's findings
    pub findings: String,

    // Key points extracted
    pub key_points: Option<String>, // JSON array

    // Confidence score (1-10)
    pub confidence: i32,

    // Tokens used by this worker
    pub tokens_used: i64,

    pub status: String,
    pub error: Option<String>,
    pub created_at: i64,
    pub completed_at: Option<i64>,
}

impl WorkerResult {
    pub fn new(research_id: &str, worker_index: i32, subtopic: impl Into<String>) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            research_id: research_id.to_string(),
            worker_index,
            subtopic: subtopic.into(),
            sources: None,
            findings: String::new(),
            key_points: None,
            confidence: 0,
            tokens_used: 0,
            status: "pending".to_string(),
            error: None,
            created_at: chrono::Utc::now().timestamp(),
            completed_at: None,
        }
    }
}

// ============================================================================
// Database Operations
// ============================================================================

pub async fn create_research_tables(pool: &PgPool) -> anyhow::Result<()> {
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS research_requests (
            id TEXT PRIMARY KEY NOT NULL,
            topic TEXT NOT NULL,
            description TEXT,
            research_type TEXT NOT NULL DEFAULT 'general',
            depth TEXT NOT NULL DEFAULT 'standard',
            repo_context TEXT,
            file_context TEXT,
            status TEXT NOT NULL DEFAULT 'pending',
            worker_count INTEGER NOT NULL DEFAULT 4,
            report TEXT,
            total_tokens INTEGER NOT NULL DEFAULT 0,
            created_at INTEGER NOT NULL DEFAULT extract(epoch from now())::bigint,
            completed_at INTEGER
        )
    "#,
    )
    .execute(pool)
    .await?;

    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS worker_results (
            id TEXT PRIMARY KEY NOT NULL,
            research_id TEXT NOT NULL,
            worker_index INTEGER NOT NULL,
            subtopic TEXT NOT NULL,
            sources TEXT,
            findings TEXT NOT NULL DEFAULT '',
            key_points TEXT,
            confidence INTEGER NOT NULL DEFAULT 0,
            tokens_used INTEGER NOT NULL DEFAULT 0,
            status TEXT NOT NULL DEFAULT 'pending',
            error TEXT,
            created_at INTEGER NOT NULL DEFAULT extract(epoch from now())::bigint,
            completed_at INTEGER,
            FOREIGN KEY (research_id) REFERENCES research_requests(id)
        )
    "#,
    )
    .execute(pool)
    .await?;

    sqlx::query("CREATE INDEX IF NOT EXISTS idx_worker_research ON worker_results(research_id)")
        .execute(pool)
        .await?;

    Ok(())
}

pub async fn save_research_request(pool: &PgPool, req: &ResearchRequest) -> anyhow::Result<()> {
    sqlx::query(
        r#"
        INSERT INTO research_requests (
            id, topic, description, research_type, depth, repo_context, file_context,
            status, worker_count, report, total_tokens, created_at, completed_at
        ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)
    "#,
    )
    .bind(&req.id)
    .bind(&req.topic)
    .bind(&req.description)
    .bind(&req.research_type)
    .bind(&req.depth)
    .bind(&req.repo_context)
    .bind(&req.file_context)
    .bind(&req.status)
    .bind(req.worker_count)
    .bind(&req.report)
    .bind(req.total_tokens)
    .bind(req.created_at)
    .bind(req.completed_at)
    .execute(pool)
    .await?;

    Ok(())
}

pub async fn save_worker_result(pool: &PgPool, result: &WorkerResult) -> anyhow::Result<()> {
    sqlx::query(
        r#"
        INSERT INTO worker_results (
            id, research_id, worker_index, subtopic, sources, findings, key_points,
            confidence, tokens_used, status, error, created_at, completed_at
        ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)
        ON CONFLICT (id) DO UPDATE SET
            research_id = EXCLUDED.research_id,
            worker_index = EXCLUDED.worker_index,
            subtopic = EXCLUDED.subtopic,
            sources = EXCLUDED.sources,
            findings = EXCLUDED.findings,
            key_points = EXCLUDED.key_points,
            confidence = EXCLUDED.confidence,
            tokens_used = EXCLUDED.tokens_used,
            status = EXCLUDED.status,
            error = EXCLUDED.error,
            created_at = EXCLUDED.created_at,
            completed_at = EXCLUDED.completed_at
    "#,
    )
    .bind(&result.id)
    .bind(&result.research_id)
    .bind(result.worker_index)
    .bind(&result.subtopic)
    .bind(&result.sources)
    .bind(&result.findings)
    .bind(&result.key_points)
    .bind(result.confidence)
    .bind(result.tokens_used)
    .bind(&result.status)
    .bind(&result.error)
    .bind(result.created_at)
    .bind(result.completed_at)
    .execute(pool)
    .await?;

    Ok(())
}

pub async fn get_research_with_results(
    pool: &PgPool,
    research_id: &str,
) -> anyhow::Result<(ResearchRequest, Vec<WorkerResult>)> {
    let request =
        sqlx::query_as::<_, ResearchRequest>("SELECT * FROM research_requests WHERE id = $1")
            .bind(research_id)
            .fetch_one(pool)
            .await?;

    let results = sqlx::query_as::<_, WorkerResult>(
        "SELECT * FROM worker_results WHERE research_id = $1 ORDER BY worker_index",
    )
    .bind(research_id)
    .fetch_all(pool)
    .await?;

    Ok((request, results))
}

pub async fn list_research(pool: &PgPool, limit: i32) -> anyhow::Result<Vec<ResearchRequest>> {
    let requests = sqlx::query_as::<_, ResearchRequest>(
        "SELECT * FROM research_requests ORDER BY created_at DESC LIMIT $1",
    )
    .bind(limit)
    .fetch_all(pool)
    .await?;

    Ok(requests)
}
