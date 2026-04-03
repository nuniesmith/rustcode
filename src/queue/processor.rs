//! Queue Processor (Legacy)
//!
//! Handles moving items through processing stages:
//! Inbox → PendingAnalysis → Analyzing → PendingTagging → Ready
//!
//! **DEPRECATED:** The primary task system is now the `tasks` table
//! (managed by `db::core::create_task` / `db::core::list_tasks`).
//! The auto-scanner writes project review tasks directly to `tasks`,
//! and the API dashboard endpoint reads from `tasks`.
//!
//! This module still operates on the `queue_items` table and is used
//! for capturing notes, thoughts, and TODO comments via `capture_thought`,
//! `capture_note`, and `capture_todo`. Consider migrating these to write
//! to the `tasks` table as well, then retiring `queue_items` entirely.

use crate::db::core::create_task;
use crate::db::queue::{QueueItem, QueuePriority, QueueSource, QueueStage};
use crate::tag_schema::{CodeStatus, TagCategory};
use anyhow::Result;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use std::time::Duration;
use tokio::time::sleep;
use tracing::{error, info, warn};

// ============================================================================
// Queue Operations
// ============================================================================

/// Add raw content to the queue for processing
pub async fn enqueue(
    pool: &PgPool,
    content: &str,
    source: QueueSource,
    priority: QueuePriority,
    repo_id: Option<&str>,
    file_path: Option<&str>,
    line_number: Option<i32>,
) -> Result<QueueItem> {
    let id = uuid::Uuid::new_v4().to_string();
    let now = Utc::now().timestamp();
    let content_hash = format!("{:x}", md5::compute(content.as_bytes()));

    // Check for duplicate
    let existing: Option<(String,)> = sqlx::query_as(
        "SELECT id FROM queue_items WHERE content_hash = $1 AND stage != 'archived'",
    )
    .bind(&content_hash)
    .fetch_optional(pool)
    .await?;

    if let Some((existing_id,)) = existing {
        warn!(
            "Duplicate content detected, returning existing item: {}",
            existing_id
        );
        return get_queue_item(pool, &existing_id).await;
    }

    sqlx::query(
        r#"
        INSERT INTO queue_items
        (id, content, stage, source, priority, repo_id, file_path, line_number,
         content_hash, retry_count, created_at, updated_at)
        VALUES ($1, $2, 'inbox', $3, $4, $5, $6, $7, $8, 0, $9, $10)
    "#,
    )
    .bind(&id)
    .bind(content)
    .bind(format!("{:?}", source).to_lowercase())
    .bind(priority as i32)
    .bind(repo_id)
    .bind(file_path)
    .bind(line_number)
    .bind(&content_hash)
    .bind(now)
    .bind(now)
    .execute(pool)
    .await?;

    info!("Enqueued item {} from {:?}", id, source);
    get_queue_item(pool, &id).await
}

/// Get a queue item by ID
pub async fn get_queue_item(pool: &PgPool, id: &str) -> Result<QueueItem> {
    sqlx::query_as::<_, QueueItem>("SELECT * FROM queue_items WHERE id = $1")
        .bind(id)
        .fetch_one(pool)
        .await
        .map_err(Into::into)
}

/// Move item to next stage
pub async fn advance_stage(pool: &PgPool, id: &str) -> Result<QueueStage> {
    let item = get_queue_item(pool, id).await?;
    let current = parse_stage(&item.stage);

    let next = match current {
        QueueStage::Inbox => QueueStage::PendingAnalysis,
        QueueStage::PendingAnalysis => QueueStage::Analyzing,
        QueueStage::Analyzing => QueueStage::PendingTagging,
        QueueStage::PendingTagging => QueueStage::Ready,
        QueueStage::Ready => QueueStage::Ready, // Already done
        QueueStage::Failed => QueueStage::PendingAnalysis, // Retry
        QueueStage::Archived => QueueStage::Archived,
    };

    let now = Utc::now().timestamp();
    let processed_at = if next == QueueStage::Ready {
        Some(now)
    } else {
        None
    };

    sqlx::query("UPDATE queue_items SET stage = $1, updated_at = $2, processed_at = COALESCE($3, processed_at) WHERE id = $4")
        .bind(format!("{:?}", next).to_lowercase())
        .bind(now)
        .bind(processed_at)
        .bind(id)
        .execute(pool)
        .await?;

    info!("Item {} moved from {:?} to {:?}", id, current, next);
    Ok(next)
}

/// Mark item as failed
pub async fn mark_failed(pool: &PgPool, id: &str, error: &str) -> Result<()> {
    let now = Utc::now().timestamp();

    sqlx::query(
        "UPDATE queue_items SET stage = 'failed', last_error = $1, retry_count = retry_count + 1, updated_at = $2 WHERE id = $3"
    )
    .bind(error)
    .bind(now)
    .bind(id)
    .execute(pool)
    .await?;

    error!("Item {} failed: {}", id, error);
    Ok(())
}

/// Update item with analysis results
pub async fn update_analysis(pool: &PgPool, id: &str, analysis: &AnalysisResult) -> Result<()> {
    let now = Utc::now().timestamp();
    let analysis_json = serde_json::to_string(analysis)?;
    let tags = analysis.tags.join(",");

    sqlx::query(r#"
        UPDATE queue_items
        SET analysis = $1, tags = $2, category = $3, score = $4, stage = 'pending_tagging', updated_at = $5
        WHERE id = $6
    "#)
    .bind(&analysis_json)
    .bind(&tags)
    .bind(&analysis.category)
    .bind(analysis.score)
    .bind(now)
    .bind(id)
    .execute(pool)
    .await?;

    Ok(())
}

/// Get next items to process for a given stage
pub async fn get_pending_items(
    pool: &PgPool,
    stage: QueueStage,
    limit: i32,
) -> Result<Vec<QueueItem>> {
    let stage_str = format!("{:?}", stage).to_lowercase();

    sqlx::query_as::<_, QueueItem>(
        "SELECT * FROM queue_items WHERE stage = $1 ORDER BY priority ASC, created_at ASC LIMIT $2",
    )
    .bind(&stage_str)
    .bind(limit)
    .fetch_all(pool)
    .await
    .map_err(Into::into)
}

/// Get items that failed but can be retried
pub async fn get_retriable_items(pool: &PgPool, max_retries: i32) -> Result<Vec<QueueItem>> {
    sqlx::query_as::<_, QueueItem>(
        "SELECT * FROM queue_items WHERE stage = 'failed' AND retry_count < $1 ORDER BY priority ASC"
    )
    .bind(max_retries)
    .fetch_all(pool)
    .await
    .map_err(Into::into)
}

/// Get queue statistics
pub async fn get_queue_stats(pool: &PgPool) -> Result<QueueStats> {
    let counts: Vec<(String, i64)> =
        sqlx::query_as("SELECT stage, COUNT(*) as count FROM queue_items GROUP BY stage")
            .fetch_all(pool)
            .await?;

    let mut stats = QueueStats::default();
    for (stage, count) in counts {
        match stage.as_str() {
            "inbox" => stats.inbox = count,
            "pending_analysis" => stats.pending_analysis = count,
            "analyzing" => stats.analyzing = count,
            "pending_tagging" => stats.pending_tagging = count,
            "ready" => stats.ready = count,
            "failed" => stats.failed = count,
            "archived" => stats.archived = count,
            _ => {}
        }
    }

    Ok(stats)
}

// ============================================================================
// Analysis Result Types
// ============================================================================

/// LLM analysis output
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnalysisResult {
    /// Short summary of the content
    pub summary: String,

    /// Suggested tags
    pub tags: Vec<String>,

    /// Category (docs, code, idea, task, research, etc)
    pub category: String,

    /// Importance/quality score (1-10)
    pub score: i32,

    /// Actionable items extracted
    pub action_items: Vec<String>,

    /// Related concepts/topics
    pub related_topics: Vec<String>,

    /// Suggested project association
    pub suggested_project: Option<String>,
}

/// Queue statistics
#[derive(Debug, Default, Serialize)]
pub struct QueueStats {
    pub inbox: i64,
    pub pending_analysis: i64,
    pub analyzing: i64,
    pub pending_tagging: i64,
    pub ready: i64,
    pub failed: i64,
    pub archived: i64,
}

impl QueueStats {
    pub fn total_pending(&self) -> i64 {
        self.inbox + self.pending_analysis + self.analyzing + self.pending_tagging
    }
}

// ============================================================================
// Queue Processor (Background Worker)
// ============================================================================

/// Background processor configuration
pub struct ProcessorConfig {
    /// How many items to process per batch
    pub batch_size: i32,

    /// Delay between batches (ms)
    pub batch_delay_ms: u64,

    /// Maximum retries before giving up
    pub max_retries: i32,

    /// Delay before retrying failed items (seconds)
    pub retry_delay_secs: u64,
}

impl Default for ProcessorConfig {
    fn default() -> Self {
        Self {
            batch_size: 10,
            batch_delay_ms: 1000,
            max_retries: 3,
            retry_delay_secs: 300, // 5 minutes
        }
    }
}

/// The background queue processor
pub struct QueueProcessor {
    pool: PgPool,
    config: ProcessorConfig,
    llm_client: Box<dyn LlmAnalyzer + Send + Sync>,
}

/// Trait for LLM analysis (implement with your Grok client)
#[async_trait::async_trait]
pub trait LlmAnalyzer {
    async fn analyze_content(&self, content: &str, source: &str) -> Result<AnalysisResult>;
    async fn analyze_file(
        &self,
        content: &str,
        file_path: &str,
        language: &str,
    ) -> Result<FileAnalysisResult>;
}

/// File-specific analysis result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileAnalysisResult {
    pub summary: String,
    pub purpose: String,
    pub language: String,
    pub complexity_score: i32,
    pub quality_score: i32,
    pub security_notes: Vec<String>,
    pub improvements: Vec<String>,
    pub dependencies: Vec<String>,
    pub exports: Vec<String>,
    pub tags: Vec<String>,
    pub needs_attention: bool,
    pub tokens_used: Option<usize>,
}

impl QueueProcessor {
    pub fn new(
        pool: PgPool,
        config: ProcessorConfig,
        llm_client: Box<dyn LlmAnalyzer + Send + Sync>,
    ) -> Self {
        Self {
            pool,
            config,
            llm_client,
        }
    }

    /// Run the processor loop
    pub async fn run(&self) -> Result<()> {
        info!("Queue processor started");

        loop {
            // Process inbox items (move to pending_analysis)
            self.process_inbox().await?;

            // Process pending analysis items
            self.process_analysis().await?;

            // Process pending tagging items
            self.process_tagging().await?;

            // Retry failed items
            self.retry_failed().await?;

            // Brief pause between cycles
            sleep(Duration::from_millis(self.config.batch_delay_ms)).await;
        }
    }

    /// Move inbox items to pending_analysis
    async fn process_inbox(&self) -> Result<()> {
        let items =
            get_pending_items(&self.pool, QueueStage::Inbox, self.config.batch_size).await?;

        for item in items {
            // Simple validation - if content is too short, skip
            if item.content.trim().len() < 5 {
                mark_failed(&self.pool, &item.id, "Content too short").await?;
                continue;
            }

            advance_stage(&self.pool, &item.id).await?;
        }

        Ok(())
    }

    /// Run LLM analysis on pending items
    async fn process_analysis(&self) -> Result<()> {
        let items = get_pending_items(
            &self.pool,
            QueueStage::PendingAnalysis,
            self.config.batch_size,
        )
        .await?;

        for item in items {
            // Mark as analyzing
            advance_stage(&self.pool, &item.id).await?;

            // Run LLM analysis
            match self
                .llm_client
                .analyze_content(&item.content, &item.source)
                .await
            {
                Ok(analysis) => {
                    update_analysis(&self.pool, &item.id, &analysis).await?;
                    info!(
                        "Analyzed item {}: category={}, score={}",
                        item.id, analysis.category, analysis.score
                    );
                }
                Err(e) => {
                    mark_failed(&self.pool, &item.id, &e.to_string()).await?;
                }
            }
        }

        Ok(())
    }

    /// Finalize tagging, refine tags via schema, link to projects, write to
    /// `tasks` table, then advance the item to `Ready`.
    ///
    /// # What this does
    /// 1. Parse LLM-produced tags from `item.tags` (comma-separated).
    /// 2. Normalise / validate tags against `TagSchema` (canonical aliases,
    ///    deduplication, sort).
    /// 3. Infer a [`CodeStatus`] from the item source and LLM score.
    /// 4. Derive a task priority from the tag category + status.
    /// 5. Attempt to resolve the queue item's `repo_id` (or fall back to a
    ///    `registered_repos` lookup by `file_path`) so the new task is linked
    ///    to the correct repo.
    /// 6. Write a new row to the `tasks` table via `db::core::create_task`.
    /// 7. Advance the queue item to `Ready`.
    async fn process_tagging(&self) -> Result<()> {
        let items = get_pending_items(
            &self.pool,
            QueueStage::PendingTagging,
            self.config.batch_size,
        )
        .await?;

        for item in items {
            // ── 1. Parse raw tags from LLM analysis ──────────────────────
            let raw_tags: Vec<String> = item
                .tags
                .as_deref()
                .unwrap_or("")
                .split(',')
                .map(|t| t.trim().to_lowercase())
                .filter(|t| !t.is_empty())
                .collect();

            // ── 2. Normalise and validate tags ────────────────────────────
            let refined_tags = refine_tags(&raw_tags);

            // ── 3. Infer code status ──────────────────────────────────────
            let status = infer_status_from_item(&item);

            // ── 4. Infer tag category from the LLM-assigned category field
            //       or fall back to the first tag, or Organisation as default.
            let category = item
                .category
                .as_deref()
                .and_then(TagCategory::from_str)
                .unwrap_or_else(|| {
                    refined_tags
                        .first()
                        .and_then(|t| TagCategory::from_str(t))
                        .unwrap_or(TagCategory::Organization)
                });

            // ── 5. Derive numeric priority ────────────────────────────────
            let priority = derive_priority(category, status, item.score);

            // ── 6. Resolve repo_id: prefer explicit field, then path lookup.
            let resolved_repo_id = if item.repo_id.is_some() {
                item.repo_id.clone()
            } else {
                resolve_repo_from_path(&self.pool, item.file_path.as_deref()).await
            };

            // ── 7. Build a human-readable title ───────────────────────────
            let title = build_task_title(&item, &refined_tags);

            // ── 8. Build description from analysis JSON (best-effort) ─────
            let description: Option<String> = item.analysis.as_deref().and_then(|json| {
                serde_json::from_str::<serde_json::Value>(json)
                    .ok()
                    .and_then(|v| {
                        v.get("summary")
                            .and_then(|s| s.as_str())
                            .map(|s| s.to_string())
                    })
            });

            // ── 9. Write to tasks table ───────────────────────────────────
            match create_task(
                &self.pool,
                &title,
                description.as_deref(),
                priority,
                "queue_processor",      // source
                Some(item.id.as_str()), // source_id — back-link to queue_items
                resolved_repo_id.as_deref(),
                item.file_path.as_deref(),
                item.line_number,
            )
            .await
            {
                Ok(task) => {
                    info!(
                        queue_id = %item.id,
                        task_id  = %task.id,
                        priority,
                        tags     = %refined_tags.join(","),
                        repo_id  = ?resolved_repo_id,
                        "Tagged queue item — task created"
                    );
                }
                Err(e) => {
                    // Non-fatal: log and continue so the item still advances
                    // to Ready. A missing tasks row is preferable to a stuck
                    // queue item that blocks the whole pipeline.
                    warn!(
                        queue_id = %item.id,
                        error    = %e,
                        "Failed to create task for queue item — advancing anyway"
                    );
                }
            }

            // ── 10. Advance queue item to Ready ───────────────────────────
            advance_stage(&self.pool, &item.id).await?;
            info!(queue_id = %item.id, "Item is now ready");
        }

        Ok(())
    }

    /// Retry failed items
    async fn retry_failed(&self) -> Result<()> {
        let items = get_retriable_items(&self.pool, self.config.max_retries).await?;

        for item in items {
            info!(
                "Retrying failed item {} (attempt {})",
                item.id,
                item.retry_count + 1
            );
            advance_stage(&self.pool, &item.id).await?; // Moves back to pending_analysis
        }

        Ok(())
    }
}

// ============================================================================
// Helper Functions
// ============================================================================

// ============================================================================
// Tag Refinement Helpers
// ============================================================================

/// Normalise a raw tag list: validate against `TagSchema`, deduplicate, sort.
///
/// Unknown tags are kept as-is (lowercased) so we don't silently drop
/// user-supplied context. Known schema tags are normalised to their canonical
/// form (e.g. "tech-debt" → "technical-debt").
fn refine_tags(raw: &[String]) -> Vec<String> {
    use crate::tag_schema::validate_tag;

    let mut out: Vec<String> = raw
        .iter()
        .map(|t| {
            // Normalise common aliases to canonical schema values
            match t.as_str() {
                "tech-debt" | "debt" => "technical-debt".to_string(),
                "perf" => "performance".to_string(),
                "sec" | "security" => "security".to_string(),
                "docs" | "documentation" => "documentation".to_string(),
                "tests" | "testing" => "testing".to_string(),
                "config" | "configuration" => "configuration".to_string(),
                "exp" | "experimental" => "experimental".to_string(),
                other => other.to_string(),
            }
        })
        .collect();

    // Validate and log unknown tags (keep them — they carry intent).
    for tag in &out {
        let v = validate_tag(tag);
        if !v.is_valid {
            tracing::debug!(tag = %tag, "Tag did not pass schema validation — keeping as freeform");
        }
    }

    out.sort();
    out.dedup();
    out
}

/// Infer a [`CodeStatus`] from the queue item's source type and LLM score.
fn infer_status_from_item(item: &QueueItem) -> CodeStatus {
    match item.source.as_str() {
        "todo_comment" => CodeStatus::NeedsReview,
        "repo_file" => match item.score {
            Some(s) if s >= 8 => CodeStatus::Stable,
            Some(s) if s >= 5 => CodeStatus::Active,
            Some(_) => CodeStatus::NeedsReview,
            None => CodeStatus::Unknown,
        },
        "raw_thought" | "note" => CodeStatus::New,
        "research" | "document" => CodeStatus::Active,
        _ => CodeStatus::Unknown,
    }
}

/// Derive a numeric task priority (1 = Critical, 2 = High, 3 = Medium, 4 = Low)
/// from the combined tag category and status.
fn derive_priority(category: TagCategory, status: CodeStatus, score: Option<i32>) -> i32 {
    use crate::tag_schema::Priority;
    let schema_priority = Priority::from_status_and_category(status, category);
    let base = match schema_priority {
        Priority::Critical => 1,
        Priority::High => 2,
        Priority::Medium => 3,
        Priority::Low => 4,
    };

    // Boost priority if the LLM gave a high importance score (8–10)
    if let Some(s) = score {
        if s >= 9 && base > 1 {
            return base - 1;
        }
    }
    base
}

/// Try to find a `registered_repos` row whose `local_path` is a prefix of
/// the given file path. Returns `None` when no match is found or when the
/// path argument is `None`.
async fn resolve_repo_from_path(pool: &PgPool, file_path: Option<&str>) -> Option<String> {
    let path = file_path?;

    // Walk up the path segments looking for a registered repo root.
    // We query with a LIKE pattern so we don't need to know the exact depth.
    let row: Option<(String,)> = sqlx::query_as(
        "SELECT id FROM registered_repos \
         WHERE active = TRUE AND $1 LIKE (local_path || '%') \
         ORDER BY length(local_path) DESC \
         LIMIT 1",
    )
    .bind(path)
    .fetch_optional(pool)
    .await
    .ok()
    .flatten();

    row.map(|(id,)| id)
}

/// Build a short task title from the queue item's content and refined tags.
fn build_task_title(item: &QueueItem, tags: &[String]) -> String {
    // Use the first 120 chars of content as the title, stripping newlines.
    let snippet: String = item
        .content
        .lines()
        .next()
        .unwrap_or(&item.content)
        .chars()
        .take(120)
        .collect();

    if tags.is_empty() {
        snippet
    } else {
        format!("[{}] {}", tags[0], snippet)
    }
}

fn parse_stage(s: &str) -> QueueStage {
    match s {
        "inbox" => QueueStage::Inbox,
        "pending_analysis" => QueueStage::PendingAnalysis,
        "analyzing" => QueueStage::Analyzing,
        "pending_tagging" => QueueStage::PendingTagging,
        "ready" => QueueStage::Ready,
        "failed" => QueueStage::Failed,
        "archived" => QueueStage::Archived,
        _ => QueueStage::Inbox,
    }
}

// ============================================================================
// Quick Capture Functions
// ============================================================================

/// Quick capture for random thoughts
pub async fn capture_thought(pool: &PgPool, text: &str) -> Result<QueueItem> {
    enqueue(
        pool,
        text,
        QueueSource::RawThought,
        QueuePriority::Normal,
        None,
        None,
        None,
    )
    .await
}

/// Quick capture for notes
pub async fn capture_note(pool: &PgPool, text: &str, project: Option<&str>) -> Result<QueueItem> {
    // If project specified, try to find matching repo
    let repo_id = if let Some(p) = project {
        sqlx::query_as::<_, (String,)>("SELECT id FROM repositories WHERE name = $1")
            .bind(p)
            .fetch_optional(pool)
            .await?
            .map(|(id,)| id)
    } else {
        None
    };

    enqueue(
        pool,
        text,
        QueueSource::Note,
        QueuePriority::Normal,
        repo_id.as_deref(),
        None,
        None,
    )
    .await
}

/// Capture a TODO found in code
pub async fn capture_todo(
    pool: &PgPool,
    content: &str,
    repo_id: &str,
    file_path: &str,
    line_number: i32,
) -> Result<QueueItem> {
    enqueue(
        pool,
        content,
        QueueSource::TodoComment,
        QueuePriority::High, // TODOs get higher priority
        Some(repo_id),
        Some(file_path),
        Some(line_number),
    )
    .await
}
