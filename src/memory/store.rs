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

use std::collections::HashSet;
use std::sync::Arc;

use anyhow::{Context, Result};
use chrono::Utc;
use sqlx::PgPool;
use tracing::{debug, info, warn};
use uuid::Uuid;

use super::types::{MemoryEntry, MemoryKind, MemorySearchHit, NewMemory, cosine_similarity};
use rag::EmbeddingGenerator;

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

// Manual Debug impl — `EmbeddingGenerator` (from `rag`) doesn't derive Debug
// because it wraps an `Arc<RwLock<Option<TextEmbedding>>>`. Printing its
// address isn't useful; show a placeholder so anything that holds
// `AgentMemory` (e.g. `AgentPipeline`) can still derive Debug.
impl std::fmt::Debug for AgentMemory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentMemory")
            .field("pool", &self.pool)
            .field("embedder", &"<EmbeddingGenerator>")
            .finish()
    }
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
        let importance = new.importance.unwrap_or(DEFAULT_IMPORTANCE).clamp(0.0, 1.0);

        let embedding = self
            .embedder
            .embed(&new.content)
            .await
            .context("failed to embed memory content")?
            .vector;
        let embedding_json = serde_json::to_string(&embedding).context("serialize embedding")?;

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
    pub async fn list(&self, project_scope: Option<&str>, limit: i64) -> Result<Vec<MemoryEntry>> {
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

        Ok(rows
            .into_iter()
            .filter_map(AgentMemoryRow::into_entry)
            .collect())
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

    /// Three-phase memory hygiene pass:
    ///
    /// 1. **Decay** — entries with `access_count == 0` and
    ///    `created_at < NOW() - decay_age_days` have their importance
    ///    lowered to `decay_to` (no-op when the current value is already
    ///    that low). Pure SQL.
    /// 2. **Delete** — entries with `importance < delete_importance_below`
    ///    and `created_at < NOW() - delete_age_days` are removed. Pure SQL.
    /// 3. **Dedupe** (optional, when `dedupe_enabled`) — for each project
    ///    scope independently, load the surviving rows and find pairs with
    ///    cosine similarity > `dedupe_similarity`. Within each pair keep
    ///    the higher-importance entry; delete the other. O(n²) per
    ///    project, so we cap candidates at `MAX_CANDIDATES`.
    ///
    /// Returns a `PruneReport` summarizing what happened. Failures inside
    /// individual phases are returned as `Err`; partial progress already
    /// committed to the database is not rolled back.
    pub async fn prune(&self, config: &PruneConfig) -> Result<PruneReport> {
        let decayed = self.decay_phase(config).await?;
        let deleted = self.delete_phase(config).await?;
        let merged = if config.dedupe_enabled {
            self.dedupe_phase(config).await?
        } else {
            0
        };
        info!(decayed, deleted, merged, "agent memory: prune complete");
        Ok(PruneReport {
            decayed,
            deleted,
            merged,
        })
    }

    async fn decay_phase(&self, config: &PruneConfig) -> Result<u64> {
        let result = sqlx::query(
            r#"
            UPDATE agent_memory
            SET importance = $1
            WHERE access_count = 0
              AND importance > $1
              AND created_at < NOW() - ($2 || ' days')::INTERVAL
            "#,
        )
        .bind(config.decay_to)
        .bind(config.decay_age_days.to_string())
        .execute(&self.pool)
        .await
        .context("prune: decay phase")?;
        Ok(result.rows_affected())
    }

    async fn delete_phase(&self, config: &PruneConfig) -> Result<u64> {
        let result = sqlx::query(
            r#"
            DELETE FROM agent_memory
            WHERE importance < $1
              AND created_at < NOW() - ($2 || ' days')::INTERVAL
            "#,
        )
        .bind(config.delete_importance_below)
        .bind(config.delete_age_days.to_string())
        .execute(&self.pool)
        .await
        .context("prune: delete phase")?;
        Ok(result.rows_affected())
    }

    async fn dedupe_phase(&self, config: &PruneConfig) -> Result<u64> {
        // Find the distinct project scopes (including NULL → represented
        // as `Option::None`) so we can dedupe each scope independently.
        // Memories in different projects should never merge.
        let scopes: Vec<Option<String>> =
            sqlx::query_scalar("SELECT DISTINCT project FROM agent_memory")
                .fetch_all(&self.pool)
                .await
                .context("prune: list project scopes")?;

        let mut total_merged: u64 = 0;
        for scope in scopes {
            let rows = self.fetch_candidates(scope.as_deref()).await?;
            // Restrict to entries in THIS scope (fetch_candidates returns
            // globals too for non-None scope, but those belong to
            // `None`'s dedupe pass — skip here to avoid double-counting).
            let same_scope: Vec<MemoryEntry> =
                rows.into_iter().filter(|e| e.project == scope).collect();

            let to_delete = find_duplicates(&same_scope, config.dedupe_similarity);
            if to_delete.is_empty() {
                continue;
            }
            let ids: Vec<Uuid> = to_delete.into_iter().collect();
            let deleted = sqlx::query("DELETE FROM agent_memory WHERE id = ANY($1)")
                .bind(&ids)
                .execute(&self.pool)
                .await
                .context("prune: dedupe delete")?;
            debug!(
                project = ?scope,
                merged = deleted.rows_affected(),
                "prune: deduped project scope"
            );
            total_merged = total_merged.saturating_add(deleted.rows_affected());
        }
        Ok(total_merged)
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

        Ok(rows
            .into_iter()
            .filter_map(AgentMemoryRow::into_entry)
            .collect())
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

/// Tunable thresholds for `AgentMemory::prune`.
///
/// Defaults match the values called out in `TODO.md` under MEM-D:
/// 30-day decay window, 90-day delete window, importance floor of 0.1,
/// cosine-similarity dedupe threshold of 0.95.
#[derive(Debug, Clone)]
pub struct PruneConfig {
    /// Entries with `access_count == 0` older than this many days are
    /// demoted to `decay_to` importance.
    pub decay_age_days: i64,
    /// Importance value applied to decayed entries.
    pub decay_to: f32,
    /// Entries with `importance` strictly below this AND older than
    /// `delete_age_days` are deleted.
    pub delete_importance_below: f32,
    pub delete_age_days: i64,
    /// Cosine similarity threshold for dedupe. Pairs at or above this
    /// value (within the same project scope) collapse to the
    /// higher-importance entry.
    pub dedupe_similarity: f32,
    /// When false, dedupe is skipped (decay + delete still run).
    /// Defaults to true — the expensive pass is bounded by
    /// `MAX_CANDIDATES` per project scope.
    pub dedupe_enabled: bool,
}

impl Default for PruneConfig {
    fn default() -> Self {
        Self {
            decay_age_days: 30,
            decay_to: 0.1,
            delete_importance_below: 0.1,
            delete_age_days: 90,
            dedupe_similarity: 0.95,
            dedupe_enabled: true,
        }
    }
}

/// Summary of what a single `prune` invocation did. All counts are
/// row-level totals across every project scope touched.
#[derive(Debug, Clone, Copy, Default, serde::Serialize, serde::Deserialize)]
pub struct PruneReport {
    pub decayed: u64,
    pub deleted: u64,
    pub merged: u64,
}

impl PruneReport {
    #[must_use]
    pub fn total_changes(&self) -> u64 {
        self.decayed + self.deleted + self.merged
    }
}

/// Identify near-duplicate entries within a single project scope.
///
/// For each pair `(a, b)` with `cosine(a.embedding, b.embedding) >= threshold`,
/// the lower-importance entry is queued for deletion (ties broken by older
/// `last_accessed`). A single entry can only be flagged once even if it has
/// multiple near-duplicates — the survivor accumulates the matches.
///
/// Returns the set of IDs to delete. O(n²) in `entries.len()` — keep the
/// candidate count bounded by `MAX_CANDIDATES`.
fn find_duplicates(entries: &[MemoryEntry], threshold: f32) -> HashSet<Uuid> {
    let mut to_delete: HashSet<Uuid> = HashSet::new();
    for (i, a) in entries.iter().enumerate() {
        if to_delete.contains(&a.id) {
            continue;
        }
        for b in entries.iter().skip(i + 1) {
            if to_delete.contains(&b.id) {
                continue;
            }
            let sim = crate::memory::cosine_similarity(&a.embedding, &b.embedding);
            if sim < threshold {
                continue;
            }
            // Keep the higher-importance entry; if tied, keep the most
            // recently accessed one.
            let loser = if a.importance > b.importance {
                b.id
            } else if b.importance > a.importance {
                a.id
            } else if a.last_accessed >= b.last_accessed {
                b.id
            } else {
                a.id
            };
            to_delete.insert(loser);
        }
    }
    to_delete
}

#[cfg(test)]
mod prune_tests {
    use super::*;
    use crate::memory::types::{MemoryEntry, MemoryKind};
    use chrono::{Duration, Utc};

    fn entry(importance: f32, embedding: Vec<f32>, age_secs: i64) -> MemoryEntry {
        let now = Utc::now();
        MemoryEntry {
            id: Uuid::new_v4(),
            project: None,
            kind: MemoryKind::Observation,
            content: "x".to_string(),
            embedding,
            importance,
            created_at: now,
            last_accessed: now - Duration::seconds(age_secs),
            access_count: 0,
        }
    }

    #[test]
    fn duplicates_collapse_to_higher_importance() {
        let lower = entry(0.3, vec![1.0, 0.0, 0.0], 0);
        let higher = entry(0.7, vec![1.0, 0.0, 0.0], 0);
        let lower_id = lower.id;
        let higher_id = higher.id;
        let to_delete = find_duplicates(&[lower, higher], 0.95);
        assert!(to_delete.contains(&lower_id));
        assert!(!to_delete.contains(&higher_id));
    }

    #[test]
    fn dissimilar_entries_are_not_flagged() {
        let a = entry(0.5, vec![1.0, 0.0], 0);
        let b = entry(0.5, vec![0.0, 1.0], 0);
        let to_delete = find_duplicates(&[a, b], 0.95);
        assert!(to_delete.is_empty());
    }

    #[test]
    fn tie_broken_by_last_accessed() {
        // Same importance — older entry should be the one deleted.
        let older = entry(0.5, vec![1.0, 0.0, 0.0], 1000);
        let newer = entry(0.5, vec![1.0, 0.0, 0.0], 0);
        let older_id = older.id;
        let newer_id = newer.id;
        let to_delete = find_duplicates(&[older, newer], 0.95);
        assert!(to_delete.contains(&older_id));
        assert!(!to_delete.contains(&newer_id));
    }

    #[test]
    fn already_flagged_entries_dont_chain() {
        // Three near-identical entries with descending importance.
        // The top one should survive; the other two should be flagged.
        let a = entry(0.9, vec![1.0, 0.0, 0.0], 0);
        let b = entry(0.5, vec![1.0, 0.0, 0.0], 0);
        let c = entry(0.3, vec![1.0, 0.0, 0.0], 0);
        let a_id = a.id;
        let to_delete = find_duplicates(&[a, b, c], 0.95);
        assert_eq!(to_delete.len(), 2);
        assert!(!to_delete.contains(&a_id));
    }

    #[test]
    fn empty_input_yields_empty_set() {
        let to_delete = find_duplicates(&[], 0.95);
        assert!(to_delete.is_empty());
    }

    #[test]
    fn single_entry_yields_empty_set() {
        let only = entry(0.5, vec![1.0, 0.0], 0);
        let to_delete = find_duplicates(&[only], 0.95);
        assert!(to_delete.is_empty());
    }

    #[test]
    fn default_config_matches_todo_spec() {
        let c = PruneConfig::default();
        assert_eq!(c.decay_age_days, 30);
        assert!((c.decay_to - 0.1).abs() < 1e-6);
        assert!((c.delete_importance_below - 0.1).abs() < 1e-6);
        assert_eq!(c.delete_age_days, 90);
        assert!((c.dedupe_similarity - 0.95).abs() < 1e-6);
        assert!(c.dedupe_enabled);
    }

    #[test]
    fn prune_report_totals_count_all_phases() {
        let r = PruneReport {
            decayed: 5,
            deleted: 2,
            merged: 3,
        };
        assert_eq!(r.total_changes(), 10);
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
