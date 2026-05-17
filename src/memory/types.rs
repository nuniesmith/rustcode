// Data types for the persistent agent memory layer.
//
// Mirrors the schema in `sql/023_agent_memory.sql`. Embeddings travel as
// `Vec<f32>` in memory and as JSON arrays on disk (matching the existing
// `document_embeddings` convention). See `store.rs` for the I/O surface.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Coarse category for a memory entry. Drives both retrieval ranking
/// (some kinds matter more for some queries) and downstream presentation
/// (the prompt-injection step labels entries by kind).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MemoryKind {
    /// "Project X uses pattern Y." Neutral statement of fact about the codebase.
    Observation,
    /// "We chose approach A over B because…" Architectural choices worth remembering.
    Decision,
    /// "User prefers idiomatic Rust over verbose code." Personalization signal.
    Preference,
    /// Recurring pattern seen across projects (e.g. "Axum handlers use State<Arc<…>>").
    Pattern,
    /// "Task type X worked with strategy Y / failed because Z." Drives future plans.
    TaskOutcome,
}

impl MemoryKind {
    /// Stable string representation used in the database. Keep this in sync
    /// with the `kind` column convention and the `from_str` parser below.
    #[must_use]
    pub const fn as_db_str(&self) -> &'static str {
        match self {
            Self::Observation => "observation",
            Self::Decision => "decision",
            Self::Preference => "preference",
            Self::Pattern => "pattern",
            Self::TaskOutcome => "task_outcome",
        }
    }

    /// Parse from the on-disk string representation. Returns `None` for
    /// unrecognized values — callers should treat that as a row to skip
    /// rather than a fatal error.
    #[must_use]
    pub fn from_db_str(s: &str) -> Option<Self> {
        match s {
            "observation" => Some(Self::Observation),
            "decision" => Some(Self::Decision),
            "preference" => Some(Self::Preference),
            "pattern" => Some(Self::Pattern),
            "task_outcome" => Some(Self::TaskOutcome),
            _ => None,
        }
    }
}

/// One memory record as it exists on disk. Returned by `search`, `list`,
/// and similar query methods. The embedding is intentionally kept here
/// rather than stripped: callers re-ranking client-side need it, and
/// callers that don't care can drop it cheaply.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryEntry {
    pub id: Uuid,
    /// Project scope. `None` = global memory, `Some` = entries that should
    /// only surface when the caller asks about this project.
    pub project: Option<String>,
    pub kind: MemoryKind,
    pub content: String,
    /// Vector representation of `content`. Dimension matches whichever
    /// embedder was configured when the entry was recorded.
    pub embedding: Vec<f32>,
    /// Retrieval-ranking weight in `[0.0, 1.0]`. MEM-D will adjust this
    /// based on access patterns; today everything gets the caller's
    /// initial value (default `0.5`).
    pub importance: f32,
    pub created_at: DateTime<Utc>,
    pub last_accessed: DateTime<Utc>,
    pub access_count: u32,
}

/// Payload for `AgentMemory::record`. The store fills in `id`,
/// `embedding`, timestamps, and `access_count`.
#[derive(Debug, Clone)]
pub struct NewMemory {
    pub project: Option<String>,
    pub kind: MemoryKind,
    pub content: String,
    /// `[0.0, 1.0]`. `None` defaults to `0.5`. Out-of-range values are
    /// clamped by the store.
    pub importance: Option<f32>,
}

impl NewMemory {
    #[must_use]
    pub fn new(kind: MemoryKind, content: impl Into<String>) -> Self {
        Self {
            project: None,
            kind,
            content: content.into(),
            importance: None,
        }
    }

    #[must_use]
    pub fn with_project(mut self, project: impl Into<String>) -> Self {
        self.project = Some(project.into());
        self
    }

    #[must_use]
    pub fn with_importance(mut self, importance: f32) -> Self {
        self.importance = Some(importance);
        self
    }
}

/// One search result. Cosine similarity is in `[-1.0, 1.0]` but embedding
/// models tuned for cosine retrieval keep most relevant pairs near `+1.0`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemorySearchHit {
    pub entry: MemoryEntry,
    pub similarity: f32,
    /// `similarity * importance` — the rank used to order results. Exposed
    /// so callers can see why entry A beat entry B without recomputing.
    pub score: f32,
}

/// Cosine similarity between two equal-length vectors. Returns `0.0`
/// when either vector is zero-length or lengths mismatch.
#[must_use]
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let mut dot: f32 = 0.0;
    let mut norm_a: f32 = 0.0;
    let mut norm_b: f32 = 0.0;
    for (x, y) in a.iter().zip(b.iter()) {
        dot += x * y;
        norm_a += x * x;
        norm_b += y * y;
    }
    if norm_a == 0.0 || norm_b == 0.0 {
        return 0.0;
    }
    dot / (norm_a.sqrt() * norm_b.sqrt())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_kind_round_trips_via_db_str() {
        for kind in [
            MemoryKind::Observation,
            MemoryKind::Decision,
            MemoryKind::Preference,
            MemoryKind::Pattern,
            MemoryKind::TaskOutcome,
        ] {
            let s = kind.as_db_str();
            assert_eq!(MemoryKind::from_db_str(s), Some(kind));
        }
    }

    #[test]
    fn memory_kind_rejects_unknown_strings() {
        assert_eq!(MemoryKind::from_db_str("nonsense"), None);
        assert_eq!(MemoryKind::from_db_str(""), None);
    }

    #[test]
    fn memory_kind_serializes_snake_case() {
        let json = serde_json::to_string(&MemoryKind::TaskOutcome).unwrap();
        assert_eq!(json, "\"task_outcome\"");
    }

    #[test]
    fn cosine_similarity_identical_vectors_is_one() {
        let v = vec![1.0_f32, 2.0, 3.0];
        let sim = cosine_similarity(&v, &v);
        assert!((sim - 1.0).abs() < 1e-6);
    }

    #[test]
    fn cosine_similarity_orthogonal_vectors_is_zero() {
        let a = vec![1.0_f32, 0.0];
        let b = vec![0.0_f32, 1.0];
        assert!(cosine_similarity(&a, &b).abs() < 1e-6);
    }

    #[test]
    fn cosine_similarity_opposite_vectors_is_negative_one() {
        let a = vec![1.0_f32, 2.0];
        let b = vec![-1.0_f32, -2.0];
        let sim = cosine_similarity(&a, &b);
        assert!((sim + 1.0).abs() < 1e-6);
    }

    #[test]
    fn cosine_similarity_mismatched_lengths_returns_zero() {
        let a = vec![1.0_f32, 2.0];
        let b = vec![1.0_f32];
        assert_eq!(cosine_similarity(&a, &b), 0.0);
    }

    #[test]
    fn cosine_similarity_empty_returns_zero() {
        let a: Vec<f32> = Vec::new();
        let b: Vec<f32> = Vec::new();
        assert_eq!(cosine_similarity(&a, &b), 0.0);
    }

    #[test]
    fn cosine_similarity_zero_vector_returns_zero() {
        let a = vec![0.0_f32, 0.0];
        let b = vec![1.0_f32, 1.0];
        assert_eq!(cosine_similarity(&a, &b), 0.0);
    }

    #[test]
    fn new_memory_builder_sets_project_and_importance() {
        let m = NewMemory::new(MemoryKind::Decision, "use sqlx not diesel")
            .with_project("rustcode")
            .with_importance(0.9);
        assert_eq!(m.project.as_deref(), Some("rustcode"));
        assert_eq!(m.importance, Some(0.9));
    }
}
