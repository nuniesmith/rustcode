// `POST /api/v1/memory/prune` — manual trigger for the agent-memory hygiene
// pass. The same pass also runs nightly from `server.rs`; this endpoint lets
// operators kick it off on demand (e.g. after a noisy consolidation run).
//
// Request body is optional. When present it overrides individual fields of
// `PruneConfig`; absent fields fall back to defaults. Auth: same bearer-token
// gate as the rest of `/api/*` via the standard middleware.
//
// Body shape (all fields optional):
//
// ```json
// {
//   "decay_age_days": 30,
//   "decay_to": 0.1,
//   "delete_importance_below": 0.1,
//   "delete_age_days": 90,
//   "dedupe_similarity": 0.95,
//   "dedupe_enabled": true
// }
// ```
//
// Response is the `PruneReport`:
//
// ```json
// {"decayed": 12, "deleted": 3, "merged": 5}
// ```

use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
};
use serde::{Deserialize, Serialize};
use tracing::{info, warn};
use uuid::Uuid;

use crate::api::repos::RepoAppState;
use crate::memory::{MemoryEntry, MemoryKind, PruneConfig, PruneReport};

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
pub struct PruneRequest {
    pub decay_age_days: Option<i64>,
    pub decay_to: Option<f32>,
    pub delete_importance_below: Option<f32>,
    pub delete_age_days: Option<i64>,
    pub dedupe_similarity: Option<f32>,
    pub dedupe_enabled: Option<bool>,
}

impl PruneRequest {
    /// Apply overrides on top of `PruneConfig::default()`.
    fn into_config(self) -> PruneConfig {
        let d = PruneConfig::default();
        PruneConfig {
            decay_age_days: self.decay_age_days.unwrap_or(d.decay_age_days),
            decay_to: self.decay_to.unwrap_or(d.decay_to),
            delete_importance_below: self
                .delete_importance_below
                .unwrap_or(d.delete_importance_below),
            delete_age_days: self.delete_age_days.unwrap_or(d.delete_age_days),
            dedupe_similarity: self.dedupe_similarity.unwrap_or(d.dedupe_similarity),
            dedupe_enabled: self.dedupe_enabled.unwrap_or(d.dedupe_enabled),
        }
    }
}

/// `POST /api/v1/memory/prune` — runs the three-phase pass and returns
/// the row counts. Returns 503 when memory isn't configured (no
/// `ANTHROPIC_API_KEY` at boot or `RC_MEMORY_INJECTION=false`).
pub async fn handle_prune(
    State(state): State<RepoAppState>,
    body: Option<Json<PruneRequest>>,
) -> impl IntoResponse {
    let Some(memory) = state.agent_memory.as_ref() else {
        warn!("memory prune requested but AgentMemory is not configured");
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({
                "error": "agent memory is not configured (set ANTHROPIC_API_KEY and leave RC_MEMORY_INJECTION on)",
            })),
        )
            .into_response();
    };

    let config = body.map(|Json(req)| req.into_config()).unwrap_or_default();
    info!(?config, "memory prune: starting");

    match memory.prune(&config).await {
        Ok(report) => {
            info!(
                decayed = report.decayed,
                deleted = report.deleted,
                merged = report.merged,
                "memory prune: complete"
            );
            (StatusCode::OK, Json(report_to_json(report))).into_response()
        }
        Err(e) => {
            warn!(error = %e, "memory prune: failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({
                    "error": format!("prune failed: {}", e),
                })),
            )
                .into_response()
        }
    }
}

fn report_to_json(report: PruneReport) -> serde_json::Value {
    serde_json::json!({
        "decayed": report.decayed,
        "deleted": report.deleted,
        "merged": report.merged,
    })
}

// ---------------------------------------------------------------------------
// GET /api/v1/memory  —  list stored memory entries
// ---------------------------------------------------------------------------

/// Query parameters for `GET /api/v1/memory`.
///
/// All fields are optional. `project` narrows to entries scoped to that
/// project (plus globals — same scoping semantics as `AgentMemory::search`).
/// `kind` filters in Rust after fetch (the store doesn't index by kind for
/// dashboard listing). `limit` is clamped to `[1, 500]`.
#[derive(Debug, Deserialize, Default)]
pub struct ListQuery {
    #[serde(default)]
    pub project: Option<String>,
    #[serde(default)]
    pub kind: Option<MemoryKind>,
    #[serde(default)]
    pub limit: Option<i64>,
}

/// Compact projection of `MemoryEntry` for dashboard consumers — the
/// embedding vector is stripped (large + not useful outside ranking).
#[derive(Debug, Serialize)]
pub struct MemoryEntryView {
    pub id: Uuid,
    pub project: Option<String>,
    pub kind: MemoryKind,
    pub content: String,
    pub importance: f32,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub last_accessed: chrono::DateTime<chrono::Utc>,
    pub access_count: u32,
}

impl From<MemoryEntry> for MemoryEntryView {
    fn from(e: MemoryEntry) -> Self {
        Self {
            id: e.id,
            project: e.project,
            kind: e.kind,
            content: e.content,
            importance: e.importance,
            created_at: e.created_at,
            last_accessed: e.last_accessed,
            access_count: e.access_count,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct ListResponse {
    pub total: i64,
    pub entries: Vec<MemoryEntryView>,
}

const DEFAULT_LIST_LIMIT: i64 = 50;
const MAX_LIST_LIMIT: i64 = 500;

pub async fn handle_list(
    State(state): State<RepoAppState>,
    Query(q): Query<ListQuery>,
) -> impl IntoResponse {
    let Some(memory) = state.agent_memory.as_ref() else {
        return memory_unavailable_response();
    };

    let limit = q
        .limit
        .unwrap_or(DEFAULT_LIST_LIMIT)
        .clamp(1, MAX_LIST_LIMIT);

    let scope = q.project.as_deref();
    let total = match memory.count(scope).await {
        Ok(n) => n,
        Err(e) => {
            warn!(error = %e, "memory list: count failed");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": format!("count failed: {}", e)})),
            )
                .into_response();
        }
    };

    let entries = match memory.list(scope, limit).await {
        Ok(rows) => rows,
        Err(e) => {
            warn!(error = %e, "memory list: list failed");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": format!("list failed: {}", e)})),
            )
                .into_response();
        }
    };

    // Kind filtering happens in Rust — the table is indexed by
    // `(project, created_at)` for listing, not by kind.
    let entries: Vec<MemoryEntryView> = entries
        .into_iter()
        .filter(|e| q.kind.map_or(true, |want| e.kind == want))
        .map(MemoryEntryView::from)
        .collect();

    (
        StatusCode::OK,
        Json(ListResponse { total, entries }),
    )
        .into_response()
}

// ---------------------------------------------------------------------------
// DELETE /api/v1/memory/:id  —  remove a single entry
// ---------------------------------------------------------------------------

pub async fn handle_delete(
    State(state): State<RepoAppState>,
    Path(id): Path<Uuid>,
) -> impl IntoResponse {
    let Some(memory) = state.agent_memory.as_ref() else {
        return memory_unavailable_response();
    };

    match memory.delete(id).await {
        Ok(true) => {
            info!(memory_id = %id, "memory entry deleted");
            (
                StatusCode::OK,
                Json(serde_json::json!({"deleted": true, "id": id})),
            )
                .into_response()
        }
        Ok(false) => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"deleted": false, "id": id})),
        )
            .into_response(),
        Err(e) => {
            warn!(error = %e, memory_id = %id, "memory delete failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": format!("delete failed: {}", e)})),
            )
                .into_response()
        }
    }
}

fn memory_unavailable_response() -> axum::response::Response {
    warn!("memory endpoint hit but AgentMemory is not configured");
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(serde_json::json!({
            "error": "agent memory is not configured (set ANTHROPIC_API_KEY and leave RC_MEMORY_INJECTION on)",
        })),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_body_uses_defaults() {
        let req = PruneRequest::default();
        let cfg = req.into_config();
        let default = PruneConfig::default();
        assert_eq!(cfg.decay_age_days, default.decay_age_days);
        assert!((cfg.decay_to - default.decay_to).abs() < 1e-6);
        assert_eq!(cfg.delete_age_days, default.delete_age_days);
        assert_eq!(cfg.dedupe_enabled, default.dedupe_enabled);
    }

    #[test]
    fn body_overrides_individual_fields() {
        let body = r#"{
            "decay_age_days": 7,
            "dedupe_enabled": false
        }"#;
        let req: PruneRequest = serde_json::from_str(body).expect("parse");
        let cfg = req.into_config();
        assert_eq!(cfg.decay_age_days, 7);
        assert!(!cfg.dedupe_enabled);
        // Unspecified fields keep defaults.
        let default = PruneConfig::default();
        assert_eq!(cfg.delete_age_days, default.delete_age_days);
    }

    #[test]
    fn report_serializes_with_expected_keys() {
        let json = report_to_json(PruneReport {
            decayed: 1,
            deleted: 2,
            merged: 3,
        });
        assert_eq!(json["decayed"], 1);
        assert_eq!(json["deleted"], 2);
        assert_eq!(json["merged"], 3);
    }

    #[test]
    fn entry_view_strips_embedding() {
        // Build a MemoryEntry with a non-trivial embedding and confirm the
        // view's serialized JSON has no `embedding` key.
        use chrono::Utc;
        let now = Utc::now();
        let entry = MemoryEntry {
            id: Uuid::new_v4(),
            project: Some("owner/repo".to_string()),
            kind: MemoryKind::Decision,
            content: "use sqlx".to_string(),
            embedding: vec![0.1, 0.2, 0.3, 0.4],
            importance: 0.7,
            created_at: now,
            last_accessed: now,
            access_count: 3,
        };
        let view: MemoryEntryView = entry.into();
        let json = serde_json::to_value(&view).expect("serialize");
        assert!(json.get("embedding").is_none());
        assert_eq!(json["content"], "use sqlx");
        assert_eq!(json["access_count"], 3);
        assert_eq!(json["project"], "owner/repo");
    }

    #[test]
    fn list_query_clamps_oversize_limit() {
        // Mirrors what `handle_list` does — the limit value is clamped
        // before reaching `AgentMemory::list`. We can test the bound
        // directly here.
        let raw = 10_000_i64;
        let clamped = raw.clamp(1, MAX_LIST_LIMIT);
        assert_eq!(clamped, MAX_LIST_LIMIT);
    }

    #[test]
    fn list_query_clamps_negative_limit() {
        let raw = -42_i64;
        let clamped = raw.clamp(1, MAX_LIST_LIMIT);
        assert_eq!(clamped, 1);
    }

    #[test]
    fn list_query_accepts_kind_via_serde_json() {
        // Axum's Query<T> deserializes from `?kind=task_outcome` through
        // serde with the same snake_case rules MemoryKind uses elsewhere.
        // We use a JSON object here as a stand-in for the urlencoded body
        // (same field names + serde shape).
        let body = r#"{
            "project": "owner/repo",
            "kind": "task_outcome",
            "limit": 10
        }"#;
        let q: ListQuery = serde_json::from_str(body).expect("decode");
        assert_eq!(q.project.as_deref(), Some("owner/repo"));
        assert_eq!(q.kind, Some(MemoryKind::TaskOutcome));
        assert_eq!(q.limit, Some(10));
    }

    #[test]
    fn list_query_defaults_when_empty() {
        let q: ListQuery = serde_json::from_str("{}").expect("decode");
        assert!(q.project.is_none());
        assert!(q.kind.is_none());
        assert!(q.limit.is_none());
    }
}
}
