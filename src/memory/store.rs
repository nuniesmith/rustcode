// SQLite-style persistent agent memory, backed by Postgres.
//
// `AgentMemory` is a thin wrapper around a `PgPool` plus a shared
// `EmbeddingGenerator`. Recording an entry generates the embedding inline;
// searching loads candidate rows for the requested project scope and
// re-ranks them in Rust by `cosine(query, embedding) * importance`. We
// deliberately don't reach for pgvector here — at the volumes this layer
// will see (a few thousand entries per project) the bandwidth cost of
// shipping vectors over the wire is well under the LLM call overhead, and
// keeping the schema in plain Postgres means no extra extension to
// install in dev / CI.
//
// The TODO description originally specced this on SQLite; we use Postgres
// because the rest of the project already relies on a `PgPool` from
// `AppState`, and embeddings sit alongside `document_embeddings` in the
// same schema.

use std::sync::Arc;

use anyhow::{Context, Result};
use chrono::Utc;
use sqlx::PgPool;
use tracing::{debug, warn};
use uuid::Uuid;

use crate::embeddings::EmbeddingGenerator;
use super::types::{
    MemoryEntry, MemoryKind, MemorySearchHit, NewMemory, cosine_similarity,
};

/// Default ranking weight when a caller omits `importance` on a new entry.
pub const DEFAULT_IMPORTANCE: f32 = 0.5;

/// Format a slice of `MemorySearchHit`s as a `[Memory]` block suitable
/// for prepending to a user / system prompt. Returns the empty string
/// when `hits` is empty so callers can unconditionally prepend the
/// output without checking.
///
/// Example output:
///
/// ```text
/// [Memory]
/// - (Decision) Prefer sqlx over diesel for async-friendly DB access.
/// - (Pattern) All Axum handlers use State<Arc<AppState>>; never clone the pool directly.
/// - (Preference) User wants streaming responses for all long-running operations.
/// ```
#[must_use]
pub fn format_memories_for_prompt(hits: &[crate::memory::types::MemorySearchHit]) -> String {
    if hits.is_empty() {
        return String::new();
    }
    let mut out = String::from("[Memory]\n");
    for hit in hits {
        let kind = match hit.entry.kind {
            crate::memory::types::MemoryKind::Observation => "Observation",
            crate::memory::types::MemoryKind::Decision => "Decision",
            crate::memory::types::MemoryKind::Preference => "Preference",
            crate::memory::types::MemoryKind::Pattern => "Pattern",
            crate::memory::types::MemoryKind::TaskOutcome => "TaskOutcome",
        };
        out.push_str(&format!("- ({}) {}\n", kind, hit.entry.content.trim()));
    }
    out
}

/// Maximum number of candidate rows we'll pull into Rust for cosine
/// ranking on a single search call. The query asks for `top_k` results;
/// we scan up to `MAX_CANDIDATES` rows and pick the best `top_k`.
pub const MAX_CANDIDATES: i64 = 4096;

/// Persistent agent memory store. Cloning is cheap (`Arc`-shared pool +
/// embedder) so callers can hand instances to spawned tasks freely.
#[derive(Clone)]
pub struct AgentMemory {
    pool: PgPool,
    embedder: Arc<EmbeddingGenerator>,
}

impl AgentMemory {
    #[must_use]
    pub fn new(pool: PgPool, embedder: Arc<EmbeddingGenerator>) -> Self {
        Self { pool, embedder }
    }

    /// Embed `new.content` and insert a fresh row. Returns the full
    /// `MemoryEntry` as written to the database (so callers see the
    /// assigned id + timestamps).
    pub async fn record(&self, new: NewMemory) -> Result<MemoryEntry> {
        if new.content.trim().is_empty() {
            anyhow::bail!("memory content must not be empty");
        }
        let importance = new
            .importance
            .unwrap_or(DEFAULT_IMPORTANCE)
            .clamp(0.0, 1.0);

        let embedding = self
            .embedder
            .embed(&new.content)
            .await
            .context("failed to embed memory content")?
            .vector;
        let embedding_json =
            serde_json::to_string(&embedding).context("serialize embedding")?;

        let id = Uuid::new_v4();
        let now = Utc::now();

        sqlx::query(
            r#"
            INSERT INTO agent_memory
                (id, project, kind, content, embedding, importance, created_at, last_accessed, access_count)
            VALUES
                ($1, $2, $3, $4, $5, $6, $7, $7, 0)
            "#,
        )
        .bind(id)
        .bind(&new.project)
        .bind(new.kind.as_db_str())
        .bind(&new.content)
        .bind(&embedding_json)
        .bind(importance)
        .bind(now)
        .execute(&self.pool)
        .await
        .context("insert agent_memory")?;

        Ok(MemoryEntry {
            id,
            project: new.project,
            kind: new.kind,
            content: new.content,
            embedding,
            importance,
            created_at: now,
            last_accessed: now,
            access_count: 0,
        })
    }

    /// Embed `query`, fetch candidate rows, and return the top-`top_k`
    /// hits ranked by `similarity * importance`.
    ///
    /// `project_scope` selects which rows are considered:
    /// - `None`: include both global (`project IS NULL`) entries and any
    ///   per-project rows (i.e. everything).
    /// - `Some("foo/bar")`: only global entries and entries scoped to
    ///   `"foo/bar"`. Other projects' memories are not surfaced.
    ///
    /// Returns an empty `Vec` when the table is empty or `top_k == 0`.
    pub async fn search(
        &self,
        query: &str,
        project_scope: Option<&str>,
        top_k: usize,
    ) -> Result<Vec<MemorySearchHit>> {
        if top_k == 0 {
            return Ok(Vec::new());
        }
        if query.trim().is_empty() {
            return Ok(Vec::new());
        }

        let query_vec = self
            .embedder
            .embed(query)
            .await
            .context("failed to embed search query")?
            .vector;

        let rows = self.fetch_candidates(project_scope).await?;
        debug!(
            candidates = rows.len(),
            top_k, project = ?project_scope, "agent memory: ranking candidates"
        );

        let mut hits: Vec<MemorySearchHit> = rows
            .into_iter()
            .filter_map(|row| {
                let similarity = cosine_similarity(&query_vec, &row.embedding);
                if similarity.is_nan() {
                    return None;
                }
                let score = similarity * row.importance;
                Some(MemorySearchHit {
                    entry: row,
                    similarity,
                    score,
                })
            })
            .collect();

        hits.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        hits.truncate(top_k);

        // Bump access counters for the rows we're returning. Best-effort —
        // if the UPDATE fails the search still returns a useful result.
        if !hits.is_empty() {
            let ids: Vec<Uuid> = hits.iter().map(|h| h.entry.id).collect();
            if let Err(e) = self.touch_many(&ids).await {
                warn!(error = %e, "agent memory: failed to touch access counters");
            }
        }

        Ok(hits)
    }

    /// Return the first `limit` entries sorted by `created_at DESC`,
    /// optionally filtered by project. Used by the future memory dashboard
    /// (MEM-E) and useful for `agent_memory --list` debugging.
    pub async fn list(
        &self,
        project_scope: Option<&str>,
        limit: i64,
    ) -> Result<Vec<MemoryEntry>> {
        let rows = match project_scope {
            Some(p) => {
                sqlx::query_as::<_, AgentMemoryRow>(
                    r#"
                    SELECT id, project, kind, content, embedding, importance,
                           created_at, last_accessed, access_count
                    FROM agent_memory
                    WHERE project = $1 OR project IS NULL
                    ORDER BY created_at DESC
                    LIMIT $2
                    "#,
                )
                .bind(p)
                .bind(limit)
                .fetch_all(&self.pool)
                .await
            }
            None => {
                sqlx::query_as::<_, AgentMemoryRow>(
                    r#"
                    SELECT id, project, kind, content, embedding, importance,
                           created_at, last_accessed, access_count
                    FROM agent_memory
                    ORDER BY created_at DESC
                    LIMIT $1
                    "#,
                )
                .bind(limit)
                .fetch_all(&self.pool)
                .await
            }
        }
        .context("list agent_memory rows")?;

        Ok(rows.into_iter().filter_map(AgentMemoryRow::into_entry).collect())
    }

    /// Count entries in scope. Used by callers that want to surface a
    /// "remembering N things about this project" hint.
    pub async fn count(&self, project_scope: Option<&str>) -> Result<i64> {
        let count: i64 = match project_scope {
            Some(p) => {
                sqlx::query_scalar(
                    "SELECT COUNT(*) FROM agent_memory WHERE project = $1 OR project IS NULL",
                )
                .bind(p)
                .fetch_one(&self.pool)
                .await?
            }
            None => {
                sqlx::query_scalar("SELECT COUNT(*) FROM agent_memory")
                    .fetch_one(&self.pool)
                    .await?
            }
        };
        Ok(count)
    }

    /// Delete a single entry by id. Returns `true` when a row was deleted,
    /// `false` when the id was unknown.
    pub async fn delete(&self, id: Uuid) -> Result<bool> {
        let rows = sqlx::query("DELETE FROM agent_memory WHERE id = $1")
            .bind(id)
            .execute(&self.pool)
            .await
            .context("delete agent_memory")?;
        Ok(rows.rows_affected() > 0)
    }

    /// Manually bump access tracking for an entry (e.g. when the caller
    /// surfaced it through a non-search path and still wants it to count
    /// against MEM-D's pruning heuristic).
    pub async fn touch(&self, id: Uuid) -> Result<()> {
        self.touch_many(std::slice::from_ref(&id)).await
    }

    async fn touch_many(&self, ids: &[Uuid]) -> Result<()> {
        if ids.is_empty() {
            return Ok(());
        }
        sqlx::query(
            r#"
            UPDATE agent_memory
            SET access_count = access_count + 1,
                last_accessed = NOW()
            WHERE id = ANY($1)
            "#,
        )
        .bind(ids)
        .execute(&self.pool)
        .await
        .context("touch agent_memory")?;
        Ok(())
    }

    async fn fetch_candidates(&self, project_scope: Option<&str>) -> Result<Vec<MemoryEntry>> {
        // Order by importance DESC so when we hit `MAX_CANDIDATES` we keep
        // the most important rows. For project-scoped searches we union
        // global and project-specific entries.
        let rows = match project_scope {
            Some(p) => {
                sqlx::query_as::<_, AgentMemoryRow>(
                    r#"
                    SELECT id, project, kind, content, embedding, importance,
                           created_at, last_accessed, access_count
                    FROM agent_memory
                    WHERE project = $1 OR project IS NULL
                    ORDER BY importance DESC, last_accessed DESC
                    LIMIT $2
                    "#,
                )
                .bind(p)
                .bind(MAX_CANDIDATES)
                .fetch_all(&self.pool)
                .await
            }
            None => {
                sqlx::query_as::<_, AgentMemoryRow>(
                    r#"
                    SELECT id, project, kind, content, embedding, importance,
                           created_at, last_accessed, access_count
                    FROM agent_memory
                    ORDER BY importance DESC, last_accessed DESC
                    LIMIT $1
                    "#,
                )
                .bind(MAX_CANDIDATES)
                .fetch_all(&self.pool)
                .await
            }
        }
        .context("fetch agent_memory candidates")?;

        Ok(rows.into_iter().filter_map(AgentMemoryRow::into_entry).collect())
    }
}

#[cfg(test)]
mod format_tests {
    use super::*;
    use crate::memory::types::{MemoryEntry, MemoryKind, MemorySearchHit};
    use chrono::Utc;
    use uuid::Uuid;

    fn hit(kind: MemoryKind, content: &str) -> MemorySearchHit {
        let now = Utc::now();
        MemorySearchHit {
            entry: MemoryEntry {
                id: Uuid::new_v4(),
                project: None,
                kind,
                content: content.to_string(),
                embedding: vec![0.0; 4],
                importance: 0.5,
                created_at: now,
                last_accessed: now,
                access_count: 0,
            },
            similarity: 0.9,
            score: 0.45,
        }
    }

    #[test]
    fn empty_hits_yields_empty_string() {
        assert!(format_memories_for_prompt(&[]).is_empty());
    }

    #[test]
    fn formats_kinds_with_capitalized_labels() {
        let hits = vec![
            hit(MemoryKind::Decision, "prefer sqlx"),
            hit(MemoryKind::Pattern, "axum handlers share state via Arc"),
            hit(MemoryKind::Preference, "no emojis in source"),
        ];
        let out = format_memories_for_prompt(&hits);
        assert!(out.starts_with("[Memory]\n"));
        assert!(out.contains("- (Decision) prefer sqlx"));
        assert!(out.contains("- (Pattern) axum handlers share state via Arc"));
        assert!(out.contains("- (Preference) no emojis in source"));
    }

    #[test]
    fn trims_whitespace_from_content() {
        let hits = vec![hit(MemoryKind::Observation, "  needs trimming  \n")];
        let out = format_memories_for_prompt(&hits);
        assert!(out.contains("- (Observation) needs trimming\n"));
        assert!(!out.contains("  needs trimming"));
    }

    #[test]
    fn includes_task_outcome_label() {
        let hits = vec![hit(MemoryKind::TaskOutcome, "tests pass with strategy X")];
        let out = format_memories_for_prompt(&hits);
        assert!(out.contains("- (TaskOutcome) tests pass with strategy X"));
    }
}

// Row shape mapped 1:1 onto the `agent_memory` table. Conversion to
// `MemoryEntry` parses the JSON embedding and the `kind` discriminator;
// rows with a broken kind or unparsable embedding are silently dropped
// from query results (and logged via `warn!`).
#[derive(Debug, sqlx::FromRow)]
struct AgentMemoryRow {
    id: Uuid,
    project: Option<String>,
    kind: String,
    content: String,
    embedding: String,
    importance: f32,
    created_at: chrono::DateTime<Utc>,
    last_accessed: chrono::DateTime<Utc>,
    access_count: i32,
}

impl AgentMemoryRow {
    fn into_entry(self) -> Option<MemoryEntry> {
        let Some(kind) = MemoryKind::from_db_str(&self.kind) else {
            warn!(id = %self.id, kind = %self.kind, "agent memory: skipping row with unknown kind");
            return None;
        };
        let embedding: Vec<f32> = match serde_json::from_str(&self.embedding) {
            Ok(v) => v,
            Err(e) => {
                warn!(id = %self.id, error = %e, "agent memory: skipping row with unparsable embedding");
                return None;
            }
        };
        Some(MemoryEntry {
            id: self.id,
            project: self.project,
            kind,
            content: self.content,
            embedding,
            importance: self.importance,
            created_at: self.created_at,
            last_accessed: self.last_accessed,
            access_count: self.access_count.max(0) as u32,
        })
    }
}
