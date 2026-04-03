//! Research Worker
//!
//! Handles parallel research execution. Each worker investigates
//! a subtopic and reports findings back for aggregation.

use super::{ResearchRequest, WorkerResult, save_worker_result};
use crate::db::get_all_embeddings;
use crate::embeddings::{EmbeddingConfig, EmbeddingGenerator};
use crate::llm::GrokClient;
use crate::vector_index::{IndexConfig, VectorIndex};
use anyhow::Result;
use futures::future::join_all;
use sqlx::PgPool;
use std::sync::Arc;
use std::sync::OnceLock;
use tokio::sync::{Mutex, Semaphore};
use tracing::{error, info, warn};

// ============================================================================
// Worker Configuration
// ============================================================================

#[derive(Debug, Clone)]
pub struct WorkerConfig {
    /// Maximum concurrent workers
    pub max_concurrent: usize,
    /// Timeout per worker (seconds)
    pub timeout_secs: u64,
    /// Max tokens per worker request
    pub max_tokens: usize,
    /// Retry failed workers
    pub retry_failed: bool,
}

impl Default for WorkerConfig {
    fn default() -> Self {
        Self {
            max_concurrent: 4,
            timeout_secs: 120,
            max_tokens: 4096,
            retry_failed: true,
        }
    }
}

// ============================================================================
// Research Orchestrator
// ============================================================================

pub struct ResearchOrchestrator {
    pool: PgPool,
    llm: Arc<GrokClient>,
    config: WorkerConfig,
    semaphore: Arc<Semaphore>,
}

impl ResearchOrchestrator {
    pub fn new(pool: PgPool, llm: GrokClient, config: WorkerConfig) -> Self {
        let semaphore = Arc::new(Semaphore::new(config.max_concurrent));
        Self {
            pool,
            llm: Arc::new(llm),
            config,
            semaphore,
        }
    }

    /// Execute a research request with parallel workers
    pub async fn execute(&self, request: &ResearchRequest) -> Result<Vec<WorkerResult>> {
        info!(
            "Starting research: {} with {} workers",
            request.topic, request.worker_count
        );

        // Step 1: Generate subtopics using LLM
        let subtopics = self.generate_subtopics(request).await?;
        info!("Generated {} subtopics", subtopics.len());

        // Step 2: Spawn workers for each subtopic
        let mut handles = Vec::new();

        for (index, subtopic) in subtopics.into_iter().enumerate() {
            let pool = self.pool.clone();
            let llm = self.llm.clone();
            let semaphore = self.semaphore.clone();
            let research_id = request.id.clone();
            let topic = request.topic.clone();
            let context = request.repo_context.clone();
            let config = self.config.clone();

            let handle = tokio::spawn(async move {
                // Acquire semaphore to limit concurrency
                let _permit = semaphore.acquire().await.unwrap();

                let mut result = WorkerResult::new(&research_id, index as i32, &subtopic);

                match Self::run_worker(&llm, &topic, &subtopic, context.as_deref(), &config).await {
                    Ok((findings, sources, tokens)) => {
                        result.findings = findings;
                        result.sources = Some(serde_json::to_string(&sources).unwrap_or_default());
                        result.tokens_used = tokens as i64;
                        result.status = "completed".to_string();
                        result.confidence = Self::calculate_confidence(&result);
                        result.completed_at = Some(chrono::Utc::now().timestamp());
                    }
                    Err(e) => {
                        error!("Worker {} failed: {}", index, e);
                        result.status = "failed".to_string();
                        result.error = Some(e.to_string());
                    }
                }

                // Save result to database
                if let Err(e) = save_worker_result(&pool, &result).await {
                    error!("Failed to save worker result: {}", e);
                }

                result
            });

            handles.push(handle);
        }

        // Step 3: Collect all results
        let results: Vec<WorkerResult> = join_all(handles)
            .await
            .into_iter()
            .filter_map(|r| r.ok())
            .collect();

        info!(
            "Research complete: {}/{} workers succeeded",
            results.iter().filter(|r| r.status == "completed").count(),
            results.len()
        );

        Ok(results)
    }

    /// Generate subtopics for parallel research
    async fn generate_subtopics(&self, request: &ResearchRequest) -> Result<Vec<String>> {
        let prompt = format!(
            r#"Break down this research topic into {count} distinct subtopics that can be researched in parallel.

Topic: {topic}
{description}
Type: {research_type}
{context}

Return ONLY a JSON array of subtopic strings, no explanation.
Example: ["subtopic 1", "subtopic 2", "subtopic 3"]

Subtopics should be:
- Distinct and non-overlapping
- Specific enough to research independently
- Together, cover the main topic comprehensively"#,
            count = request.worker_count,
            topic = request.topic,
            description = request.description.as_deref().unwrap_or(""),
            research_type = request.research_type,
            context = request
                .repo_context
                .as_ref()
                .map(|c| format!("Code context: {}", c))
                .unwrap_or_default(),
        );

        let response = self.llm.generate(&prompt, 1024).await?;

        // Parse JSON array from response
        let subtopics: Vec<String> = serde_json::from_str(&response)
            .or_else(|_| {
                // Try to extract JSON from response
                let start = response.find('[').unwrap_or(0);
                let end = response.rfind(']').map(|i| i + 1).unwrap_or(response.len());
                serde_json::from_str(&response[start..end])
            })
            .unwrap_or_else(|_| {
                // Fallback: split by newlines
                response
                    .lines()
                    .filter(|l| !l.trim().is_empty())
                    .map(|l| {
                        l.trim()
                            .trim_matches(|c| c == '"' || c == '-' || c == '*')
                            .to_string()
                    })
                    .take(request.worker_count as usize)
                    .collect()
            });

        Ok(subtopics)
    }

    /// Run a single worker to research a subtopic
    async fn run_worker(
        llm: &GrokClient,
        main_topic: &str,
        subtopic: &str,
        context: Option<&str>,
        config: &WorkerConfig,
    ) -> Result<(String, Vec<String>, usize)> {
        let prompt = format!(
            r#"Research the following subtopic in depth.

Main Topic: {main_topic}
Subtopic to Research: {subtopic}
{context}

Provide:
1. A comprehensive analysis of this subtopic
2. Key findings and insights
3. Relevant examples or evidence
4. How this relates to the main topic

Be thorough but focused on this specific subtopic."#,
            main_topic = main_topic,
            subtopic = subtopic,
            context = context
                .map(|c| format!("Context:\n{}", c))
                .unwrap_or_default(),
        );

        let response = llm.generate(&prompt, config.max_tokens).await?;
        let tokens = response.len() / 4; // Rough estimate

        // For now, sources are empty (would come from RAG)
        let sources: Vec<String> = vec![];

        Ok((response, sources, tokens))
    }

    /// Calculate confidence score based on result quality
    fn calculate_confidence(result: &WorkerResult) -> i32 {
        let mut score = 5; // Base score

        // Longer findings = more thorough
        if result.findings.len() > 1000 {
            score += 2;
        }
        if result.findings.len() > 2000 {
            score += 1;
        }

        // Has sources
        if result
            .sources
            .as_ref()
            .map(|s| s.len() > 10)
            .unwrap_or(false)
        {
            score += 2;
        }

        score.min(10)
    }
}

// ============================================================================
// RAG Integration — VectorIndex + fastembed
// ============================================================================

pub struct RagContext {
    pub query: String,
    pub results: Vec<RagResult>,
}

pub struct RagResult {
    pub content: String,
    pub source: String,
    pub score: f32,
}

// ---------------------------------------------------------------------------
// Global in-process vector index.
// Built lazily on first RAG query; refreshed via `refresh_rag_index`.
// Wrapped in a Mutex so we can rebuild it without restarting the server.
// ---------------------------------------------------------------------------

struct RagIndex {
    index: VectorIndex,
    /// chunk_id → (content snippet, source label)
    metadata: std::collections::HashMap<String, (String, String)>,
}

static RAG_INDEX: OnceLock<Arc<Mutex<Option<RagIndex>>>> = OnceLock::new();

fn rag_index_cell() -> &'static Arc<Mutex<Option<RagIndex>>> {
    RAG_INDEX.get_or_init(|| Arc::new(Mutex::new(None)))
}

/// (Re-)build the in-process HNSW index from all embeddings stored in Postgres.
///
/// Call this:
/// - Once at server startup (after `init_db`)
/// - After every `DocumentIndexer::index_document` completes
/// - After every `RepoSyncService::sync` that upserts new chunk embeddings
pub async fn refresh_rag_index(pool: &PgPool) -> Result<usize> {
    let embeddings = get_all_embeddings(pool).await?;
    if embeddings.is_empty() {
        info!("RAG index: no embeddings in DB yet — index will be empty");
        let mut guard = rag_index_cell().lock().await;
        *guard = Some(RagIndex {
            index: VectorIndex::new(IndexConfig::default()),
            metadata: std::collections::HashMap::new(),
        });
        return Ok(0);
    }

    // Infer dimension from first entry (all rows should agree).
    let dimension = embeddings[0].dimension as usize;
    let config = IndexConfig {
        dimension,
        ..IndexConfig::default()
    };
    let mut index = VectorIndex::new(config);
    let mut metadata = std::collections::HashMap::new();

    let mut loaded = 0usize;
    for emb in &embeddings {
        // Parse the JSON-serialised float array stored in the DB.
        let vector: Vec<f32> = match serde_json::from_str(&emb.embedding) {
            Ok(v) => v,
            Err(e) => {
                warn!(chunk_id = %emb.chunk_id, error = %e, "Skipping malformed embedding");
                continue;
            }
        };
        if vector.is_empty() || vector.len() != dimension {
            warn!(
                chunk_id = %emb.chunk_id,
                got = vector.len(),
                expected = dimension,
                "Skipping embedding with wrong dimension"
            );
            continue;
        }

        if let Err(e) = index.add_vector(emb.chunk_id.clone(), vector) {
            warn!(chunk_id = %emb.chunk_id, error = %e, "Failed to insert into HNSW index");
            continue;
        }

        // Store a short source label for display; content is fetched separately if needed.
        metadata.insert(
            emb.chunk_id.clone(),
            (String::new(), format!("chunk:{}", emb.chunk_id)),
        );
        loaded += 1;
    }

    info!(loaded, total = embeddings.len(), "RAG index rebuilt");

    let mut guard = rag_index_cell().lock().await;
    *guard = Some(RagIndex { index, metadata });

    Ok(loaded)
}

/// Search the RAG index for chunks semantically close to `query`.
///
/// Falls back gracefully when the index is empty or the embedding model
/// has not initialised yet (returns an empty vec rather than an error).
pub async fn search_rag_context(
    pool: &PgPool,
    query: &str,
    limit: usize,
) -> Result<Vec<RagResult>> {
    // Build the index on first call if it hasn't been populated yet.
    {
        let guard = rag_index_cell().lock().await;
        if guard.is_none() {
            drop(guard);
            if let Err(e) = refresh_rag_index(pool).await {
                warn!(error = %e, "RAG index build failed — returning empty results");
                return Ok(vec![]);
            }
        }
    }

    // Embed the query with the same model used at index time.
    let generator = match EmbeddingGenerator::new(EmbeddingConfig::default()) {
        Ok(g) => g,
        Err(e) => {
            warn!(error = %e, "Could not initialise embedding model for RAG query");
            return Ok(vec![]);
        }
    };

    let query_embedding = match generator.embed(query).await {
        Ok(e) => e,
        Err(e) => {
            warn!(error = %e, "Failed to embed RAG query");
            return Ok(vec![]);
        }
    };

    // Search the HNSW index.
    let search_results = {
        let guard = rag_index_cell().lock().await;
        match guard.as_ref() {
            Some(rag) => {
                if rag.index.is_empty() {
                    return Ok(vec![]);
                }
                rag.index.search(&query_embedding.vector, limit)?
            }
            None => return Ok(vec![]),
        }
    };

    // Fetch actual chunk content from DB for the top hits.
    let mut results = Vec::with_capacity(search_results.len());
    for hit in search_results {
        // Pull chunk content from DB.
        let row = sqlx::query(
            "SELECT dc.content, d.title, d.source_url
             FROM document_chunks dc
             JOIN documents d ON d.id = dc.document_id
             WHERE dc.id = $1",
        )
        .bind(&hit.id)
        .fetch_optional(pool)
        .await;

        let (content, source) = match row {
            Ok(Some(r)) => {
                use sqlx::Row;
                let content: String = r.get("content");
                let title: String = r.get("title");
                let url: Option<String> = r.get("source_url");
                let source = url.unwrap_or(title);
                (content, source)
            }
            _ => {
                // Fall back to the stub metadata we stored at index time.
                let guard = rag_index_cell().lock().await;
                let (_, src) = guard
                    .as_ref()
                    .and_then(|rag| rag.metadata.get(&hit.id))
                    .cloned()
                    .unwrap_or_default();
                (format!("[chunk {}]", hit.id), src)
            }
        };

        results.push(RagResult {
            content,
            source,
            score: hit.score,
        });
    }

    info!(
        query_len = query.len(),
        returned = results.len(),
        "RAG search complete"
    );

    Ok(results)
}

/// Enhance a prompt with RAG context snippets prepended.
pub fn enhance_prompt_with_rag(prompt: &str, rag_results: &[RagResult]) -> String {
    if rag_results.is_empty() {
        return prompt.to_string();
    }

    let context: String = rag_results
        .iter()
        .enumerate()
        .map(|(i, r)| {
            format!(
                "[{}] (score {:.2}) {}\nSource: {}",
                i + 1,
                r.score,
                r.content,
                r.source
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n");

    format!(
        "Relevant context from knowledge base:\n\n{}\n\n---\n\n{}",
        context, prompt
    )
}
