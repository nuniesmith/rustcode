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

use axum::{Json, extract::State, http::StatusCode, response::IntoResponse};
use serde::Deserialize;
use tracing::{info, warn};

use crate::api::repos::RepoAppState;
use crate::memory::{PruneConfig, PruneReport};

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
}
