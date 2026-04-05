// Audit endpoint — Axum handlers for `GET /api/audit` and `POST /api/audit`
//
// # Routes
//
// | Method | Path              | Description                                      |
// |--------|-------------------|--------------------------------------------------|
// | GET    | `/api/audit`      | List recent audit reports stored in `docs/audit/`|
// | POST   | `/api/audit`      | Trigger a new audit run for a given repo path    |
// | GET    | `/api/audit/:id`  | Fetch a specific audit report by ID              |
//
// # Integration notes
//
// - Wired into `src/server.rs` via `audit_router()`.
// - Delegates to `src/audit/runner.rs` (`AuditRunnerWithGrok`) for the actual work.
// - Uses `src/audit/cache.rs` (`RedisAuditCache`) to skip re-auditing unchanged files.
// - On completion, findings are appended to the target repo's `todo.md`
//   via the `append_to_todo` flag on `AuditRequest`.

use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{error, info, warn};

use crate::audit::cache::RedisAuditCache;
use crate::audit::report::{AuditReport, ReportConfig, ReportFormat};
use crate::audit::runner::{AuditRunner, AuditRunnerConfig};
use crate::audit::types::{AuditRequest, AuditResponse, AuditStatus};
use crate::grok_client::GrokClient;

// ============================================================================
// State
// ============================================================================

// Shared state for the audit sub-router.
//
// Constructed once at startup in `server.rs` and handed to the router via
// `.with_state(Arc::new(audit_state))`.
#[derive(Clone)]
pub struct AuditState {
    // Optional Grok client for LLM-assisted scoring. `None` when `XAI_API_KEY` is unset.
    pub grok: Option<Arc<GrokClient>>,
    // Redis-backed dedup cache. Degrades gracefully to no-op when Redis is unreachable.
    pub cache: Arc<RwLock<RedisAuditCache>>,
    // Directory where audit JSON results are persisted (default: `docs/audit`).
    pub output_dir: std::path::PathBuf,
    // Runner config — applies to every audit triggered via the API.
    pub runner_config: AuditRunnerConfig,
}

impl AuditState {
    // Build from environment variables.
    //
    // | Env var             | Default                 |
    // |---------------------|-------------------------|
    // | `XAI_API_KEY`       | (none) — LLM disabled   |
    // | `REDIS_URL`         | `redis://127.0.0.1:6379`|
    // | `AUDIT_OUTPUT_DIR`  | `docs/audit`            |
    pub async fn from_env(db: crate::db::Database) -> Self {
        let grok = match std::env::var("XAI_API_KEY") {
            Ok(key) if !key.is_empty() => {
                info!("AuditState: GrokClient enabled");
                Some(Arc::new(GrokClient::new(key, db)))
            }
            _ => {
                info!("AuditState: XAI_API_KEY not set — LLM scoring disabled");
                None
            }
        };

        let cache = match RedisAuditCache::from_env().await {
            Ok(c) => {
                info!("AuditState: RedisAuditCache ready");
                c
            }
            Err(e) => {
                warn!(error = %e, "AuditState: could not build RedisAuditCache — using no-op");
                // from_env gracefully falls back to disabled mode; this branch
                // is hit only on unexpected construction errors.
                RedisAuditCache::from_env()
                    .await
                    .unwrap_or_else(|_| unreachable!("AuditCacheConfig::disabled always succeeds"))
            }
        };

        let output_dir = std::path::PathBuf::from(
            std::env::var("AUDIT_OUTPUT_DIR").unwrap_or_else(|_| "docs/audit".to_string()),
        );

        Self {
            grok,
            cache: Arc::new(RwLock::new(cache)),
            output_dir,
            runner_config: AuditRunnerConfig::default(),
        }
    }
}

// ============================================================================
// Router
// ============================================================================

// Build the `/api/audit` sub-router.
//
// Mount this inside `src/server.rs` with:
// ```rust,ignore
// .merge(audit_router(audit_state))
// ```
pub fn audit_router(state: Arc<AuditState>) -> Router {
    Router::new()
        .route("/api/audit", get(handle_audit_get))
        .route("/api/audit", post(handle_audit_post))
        .route("/api/audit/{id}", get(handle_audit_get_by_id))
        .with_state(state)
}

// ============================================================================
// Handlers
// ============================================================================

// `GET /api/audit`
//
// Returns a list of recent audit reports from `docs/audit/`, ordered by
// `created_at` descending (newest first).
pub async fn handle_audit_get(State(state): State<Arc<AuditState>>) -> impl IntoResponse {
    let dir = &state.output_dir;

    // Ensure the directory exists before listing it.
    if let Err(e) = tokio::fs::create_dir_all(dir).await {
        error!(dir = %dir.display(), error = %e, "Failed to create audit output dir");
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": format!("Storage error: {}", e) })),
        )
            .into_response();
    }

    let mut entries = match tokio::fs::read_dir(dir).await {
        Ok(e) => e,
        Err(e) => {
            error!(error = %e, "Failed to read audit output dir");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": format!("Failed to list audit dir: {}", e) })),
            )
                .into_response();
        }
    };

    let mut reports: Vec<AuditReportSummary> = Vec::new();

    loop {
        match entries.next_entry().await {
            Ok(Some(entry)) => {
                let path = entry.path();
                // Only read *.json files
                if path.extension().and_then(|e| e.to_str()) != Some("json") {
                    continue;
                }

                match tokio::fs::read_to_string(&path).await {
                    Ok(content) => match serde_json::from_str::<AuditResponse>(&content) {
                        Ok(resp) => {
                            reports.push(AuditReportSummary {
                                id: resp.id.clone(),
                                repo: resp.request.repo.clone(),
                                created_at: resp
                                    .completed_at
                                    .unwrap_or(resp.requested_at)
                                    .to_rfc3339(),
                                status: resp.status,
                                findings_count: resp.findings.len(),
                                report_path: format!("docs/audit/{}.md", resp.id),
                            });
                        }
                        Err(e) => {
                            warn!(
                                path = %path.display(),
                                error = %e,
                                "Failed to deserialise audit JSON — skipping"
                            );
                        }
                    },
                    Err(e) => {
                        warn!(path = %path.display(), error = %e, "Failed to read audit file — skipping");
                    }
                }
            }
            Ok(None) => break,
            Err(e) => {
                warn!(error = %e, "Error iterating audit dir entry — skipping");
            }
        }
    }

    // Newest first
    reports.sort_by(|a, b| b.created_at.cmp(&a.created_at));

    let total = reports.len();
    (
        StatusCode::OK,
        Json(serde_json::to_value(AuditListResponse { reports, total }).unwrap_or_default()),
    )
        .into_response()
}

// `POST /api/audit`
//
// Triggers a new audit run. The runner is spawned in a background task; the
// endpoint returns **202 Accepted** immediately with the `audit_id` so callers
// can poll `GET /api/audit/:id`.
pub async fn handle_audit_post(
    State(state): State<Arc<AuditState>>,
    Json(req): Json<AuditRequest>,
) -> impl IntoResponse {
    info!(repo = %req.repo, mode = %req.mode, "POST /api/audit — triggering run");

    // Validate the repo path exists before we accept the job.
    let repo_path = std::path::PathBuf::from(&req.repo);
    if !repo_path.exists() {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(serde_json::json!({
                "error": format!("Repo path does not exist: {}", req.repo)
            })),
        )
            .into_response();
    }

    // Pre-generate a run ID so we can return it to the caller immediately.
    let run_id = uuid::Uuid::new_v4().to_string();
    let output_dir = state.output_dir.clone();

    info!(run_id = %run_id, "Accepted audit job");

    // Clone everything the background task needs.
    let grok = state.grok.clone();
    let runner_config = state.runner_config.clone();
    let run_id_bg = run_id.clone();

    tokio::spawn(async move {
        let repo_path = std::path::PathBuf::from(&req.repo);
        let result = if let Some(grok_client) = grok {
            let runner = AuditRunner::with_grok(runner_config, grok_client);
            runner.run(req).await
        } else {
            // No LLM key — fall back to static-analysis-only path.
            AuditRunner::with_defaults()
                .run_static_only(&repo_path)
                .await
        };

        match result {
            Ok(mut response) => {
                // Override the ID with the one we pre-allocated so the caller can
                // poll by the ID we returned in the 202.
                response.id = run_id_bg.clone();

                // Persist JSON result
                if let Err(e) = persist_audit_result(&output_dir, &response).await {
                    error!(run_id = %run_id_bg, error = %e, "Failed to persist audit result");
                }

                // Persist Markdown report alongside
                let report = AuditReport::with_config(
                    response.clone(),
                    ReportConfig {
                        format: ReportFormat::Markdown,
                        ..ReportConfig::default()
                    },
                );
                let md_path = output_dir.join(format!("{}.md", run_id_bg));
                if let Err(e) = report.save_to(&md_path) {
                    warn!(run_id = %run_id_bg, error = %e, "Failed to write Markdown report");
                }

                info!(
                    run_id = %run_id_bg,
                    findings = response.findings.len(),
                    status = %response.status,
                    "Audit background task complete"
                );
            }
            Err(e) => {
                error!(run_id = %run_id_bg, error = %e, "Audit background task failed");
                // Write a minimal failure record so GET /api/audit/:id returns something
                // useful rather than a 404.
                let failure = serde_json::json!({
                    "id": run_id_bg,
                    "status": "failed",
                    "error": e.to_string(),
                });
                let path = output_dir.join(format!("{}.json", run_id_bg));
                if let Ok(json) = serde_json::to_string_pretty(&failure) {
                    let _ = tokio::fs::write(path, json).await;
                }
            }
        }
    });

    (
        StatusCode::ACCEPTED,
        Json(
            serde_json::to_value(AuditJobAccepted {
                audit_id: run_id,
                status: AuditStatus::Running,
                message: "Audit started. Poll GET /api/audit/<audit_id> for status.".to_string(),
            })
            .unwrap_or_default(),
        ),
    )
        .into_response()
}

// `GET /api/audit/:id`
//
// Returns the full `AuditResponse` JSON for the given run ID, or 404.
pub async fn handle_audit_get_by_id(
    State(state): State<Arc<AuditState>>,
    Path(audit_id): Path<String>,
) -> impl IntoResponse {
    // Sanitise: only allow alphanumeric + dash + underscore to prevent path traversal.
    if !audit_id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "Invalid audit ID format" })),
        )
            .into_response();
    }

    let json_path = state.output_dir.join(format!("{}.json", audit_id));

    match tokio::fs::read_to_string(&json_path).await {
        Ok(content) => match serde_json::from_str::<serde_json::Value>(&content) {
            Ok(value) => (StatusCode::OK, Json(value)).into_response(),
            Err(e) => {
                error!(id = %audit_id, error = %e, "Failed to deserialise stored audit JSON");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({ "error": "Stored audit report is malformed" })),
                )
                    .into_response()
            }
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({
                "error": format!("Audit report '{}' not found", audit_id)
            })),
        )
            .into_response(),
        Err(e) => {
            error!(id = %audit_id, error = %e, "I/O error reading audit report");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": format!("I/O error: {}", e) })),
            )
                .into_response()
        }
    }
}

// ============================================================================
// Helpers
// ============================================================================

// Write `AuditResponse` to `<output_dir>/<id>.json`.
async fn persist_audit_result(
    output_dir: &std::path::Path,
    response: &AuditResponse,
) -> Result<(), std::io::Error> {
    tokio::fs::create_dir_all(output_dir).await?;
    let path = output_dir.join(format!("{}.json", response.id));
    let json = serde_json::to_string_pretty(response).map_err(std::io::Error::other)?;
    tokio::fs::write(path, json).await
}

// ============================================================================
// Response types
// ============================================================================

// Summary entry for a single audit report (used in list response)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditReportSummary {
    // Unique audit ID (UUID v4)
    pub id: String,
    // Repository that was audited
    pub repo: String,
    // ISO-8601 creation timestamp
    pub created_at: String,
    // Current status
    pub status: AuditStatus,
    // Total number of findings
    pub findings_count: usize,
    // Relative path to the Markdown report
    pub report_path: String,
}

// Response body for `GET /api/audit`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditListResponse {
    pub reports: Vec<AuditReportSummary>,
    pub total: usize,
}

// Response body for `POST /api/audit` (202 Accepted)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditJobAccepted {
    // Unique audit run ID for polling
    pub audit_id: String,
    // Current status (will be `running`)
    pub status: AuditStatus,
    // Human-readable message with polling instructions
    pub message: String,
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    async fn make_state() -> Arc<AuditState> {
        let cache = RedisAuditCache::from_env()
            .await
            .unwrap_or_else(|_| panic!("cache build failed"));

        Arc::new(AuditState {
            grok: None,
            cache: Arc::new(RwLock::new(cache)),
            output_dir: std::path::PathBuf::from(format!(
                "/tmp/rustcode-audit-test-{}",
                uuid::Uuid::new_v4()
            )),
            runner_config: AuditRunnerConfig::default(),
        })
    }

    #[tokio::test]
    async fn test_handle_audit_get_empty_dir_returns_200() {
        let state = make_state().await;
        let app = audit_router(state);

        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/audit")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_handle_audit_get_by_id_nonexistent_returns_404() {
        let state = make_state().await;
        let app = audit_router(state);

        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/audit/nonexistent-run-id")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_handle_audit_get_by_id_path_traversal_rejected() {
        let state = make_state().await;
        let app = audit_router(state);

        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/audit/..%2F..%2Fetc%2Fpasswd")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        // Should be 400 Bad Request — path traversal characters are rejected
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn test_handle_audit_post_invalid_repo_returns_422() {
        let state = make_state().await;
        let app = audit_router(state);

        let req_body = serde_json::to_string(&AuditRequest {
            repo: "/tmp/this-path-definitely-does-not-exist-rustcode".to_string(),
            ..AuditRequest::default()
        })
        .unwrap();

        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/audit")
                    .header("content-type", "application/json")
                    .body(Body::from(req_body))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[tokio::test]
    async fn test_handle_audit_post_valid_repo_returns_202() {
        let state = make_state().await;
        let app = audit_router(state);

        // Use /tmp — guaranteed to exist on Linux/macOS CI
        let req_body = serde_json::to_string(&AuditRequest {
            repo: "/tmp".to_string(),
            ..AuditRequest::default()
        })
        .unwrap();

        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/audit")
                    .header("content-type", "application/json")
                    .body(Body::from(req_body))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::ACCEPTED);
    }

    #[test]
    fn test_audit_list_response_serialises() {
        let resp = AuditListResponse {
            reports: vec![AuditReportSummary {
                id: "20240101-abc123".to_string(),
                repo: "nuniesmith/rustcode".to_string(),
                created_at: "2024-01-01T12:00:00Z".to_string(),
                status: AuditStatus::Completed,
                findings_count: 5,
                report_path: "docs/audit/20240101-abc123.md".to_string(),
            }],
            total: 1,
        };

        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("nuniesmith/rustcode"));
        assert!(json.contains("20240101-abc123"));
        assert!(json.contains("\"total\":1"));
    }

    #[test]
    fn test_audit_job_accepted_serialises() {
        let resp = AuditJobAccepted {
            audit_id: "run-001".to_string(),
            status: AuditStatus::Running,
            message: "Audit started".to_string(),
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("run-001"));
        assert!(json.contains("running"));
    }

    #[test]
    fn test_audit_report_summary_deserialises() {
        let json = r#"{
            "id": "abc",
            "repo": "org/repo",
            "created_at": "2024-01-01T00:00:00Z",
            "status": "completed",
            "findings_count": 3,
            "report_path": "docs/audit/abc.md"
        }"#;

        let summary: AuditReportSummary = serde_json::from_str(json).unwrap();
        assert_eq!(summary.id, "abc");
        assert_eq!(summary.findings_count, 3);
    }
}
