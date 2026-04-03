//! Admin API Module
//!
//! Provides administrative endpoints for the dashboard.
//! Includes metrics, analytics, webhook management, API key management, and system health.

use crate::api::ApiResponse;
use crate::api::handlers::ApiState;
use anyhow::Result;
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{delete, get, post},
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use std::sync::Arc;

// ============================================================================
// Admin Router
// ============================================================================

/// Create admin router — wired into `create_api_router` under `/admin`
pub fn admin_router() -> Router<Arc<ApiState>> {
    Router::new()
        .route("/admin/stats", get(admin_stats))
        .route("/admin/health", get(health_check))
        .route("/admin/api-keys", get(list_api_keys).post(create_api_key))
        .route("/admin/api-keys/{id}", delete(revoke_api_key))
        .route("/admin/jobs", get(list_jobs))
        .route("/admin/jobs/{id}/retry", post(retry_job))
}

// ============================================================================
// Request / Response Types
// ============================================================================

#[derive(Debug, Serialize)]
pub struct AdminStats {
    pub total_documents: i64,
    pub indexed_documents: i64,
    pub total_chunks: i64,
    pub active_jobs: i64,
    pub pending_jobs: i64,
    pub failed_jobs: i64,
    pub queue_depth: usize,
    pub uptime_secs: u64,
}

#[derive(Debug, Serialize)]
pub struct HealthStatus {
    pub status: String,
    pub database: bool,
    pub timestamp: DateTime<Utc>,
    pub version: &'static str,
}

#[derive(Debug, Deserialize)]
pub struct CreateApiKeyRequest {
    pub name: String,
    pub description: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ApiKeyResponse {
    pub id: String,
    pub name: String,
    pub prefix: String,
    /// Only populated on creation — never returned again after that.
    pub key: Option<String>,
    pub created_at: DateTime<Utc>,
    pub last_used: Option<DateTime<Utc>>,
    pub request_count: i64,
}

#[derive(Debug, Serialize)]
pub struct JobResponse {
    pub id: String,
    pub document_id: String,
    pub status: String,
    pub progress: f64,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    pub duration_ms: Option<i64>,
    pub error: Option<String>,
    pub chunks_processed: i64,
}

#[derive(Debug, Deserialize)]
pub struct JobsQuery {
    /// Filter by status: pending | processing | completed | failed
    pub status: Option<String>,
    pub limit: Option<i64>,
}

// ============================================================================
// Helpers
// ============================================================================

/// Map a sqlx error to a 500 status code, logging the detail.
fn db_err(e: sqlx::Error) -> StatusCode {
    tracing::error!(error = %e, "Admin handler DB error");
    StatusCode::INTERNAL_SERVER_ERROR
}

// ============================================================================
// Handlers
// ============================================================================

/// GET /admin/stats
async fn admin_stats(State(state): State<Arc<ApiState>>) -> Result<impl IntoResponse, StatusCode> {
    // Document counts — these tables are created by migration 006_documents.sql.
    // If they don't exist yet (pre-migration dev environment) we return 0 safely.
    let total_documents = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM documents")
        .fetch_one(&state.db_pool)
        .await
        .unwrap_or(0);

    let indexed_documents =
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM documents WHERE indexed_at IS NOT NULL")
            .fetch_one(&state.db_pool)
            .await
            .unwrap_or(0);

    let total_chunks = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM document_chunks")
        .fetch_one(&state.db_pool)
        .await
        .unwrap_or(0);

    // Job counts — these tables are created by migration 006_documents.sql.
    let active_jobs = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM indexing_jobs WHERE status = 'processing'",
    )
    .fetch_one(&state.db_pool)
    .await
    .unwrap_or(0);

    let pending_jobs =
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM indexing_jobs WHERE status = 'pending'")
            .fetch_one(&state.db_pool)
            .await
            .unwrap_or(0);

    let failed_jobs =
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM indexing_jobs WHERE status = 'failed'")
            .fetch_one(&state.db_pool)
            .await
            .unwrap_or(0);

    // In-process job queue depth (does not touch DB).
    let queue_depth = state.job_queue.pending_count();

    // Server uptime
    let uptime_secs = state.start_time.elapsed().map(|d| d.as_secs()).unwrap_or(0);

    Ok(Json(ApiResponse::success(AdminStats {
        total_documents,
        indexed_documents,
        total_chunks,
        active_jobs,
        pending_jobs,
        failed_jobs,
        queue_depth,
        uptime_secs,
    })))
}

/// GET /admin/health
async fn health_check(State(state): State<Arc<ApiState>>) -> Result<impl IntoResponse, StatusCode> {
    let db_ok = sqlx::query("SELECT 1")
        .execute(&state.db_pool)
        .await
        .is_ok();

    let status = if db_ok { "healthy" } else { "degraded" };

    Ok(Json(ApiResponse::success(HealthStatus {
        status: status.to_string(),
        database: db_ok,
        timestamp: Utc::now(),
        version: env!("CARGO_PKG_VERSION"),
    })))
}

/// GET /admin/api-keys
async fn list_api_keys(
    State(state): State<Arc<ApiState>>,
) -> Result<impl IntoResponse, StatusCode> {
    // api_keys table may not exist yet — return empty list gracefully.
    let rows = sqlx::query_as::<_, (String, String, String, String, Option<String>, i64)>(
        "SELECT id, name, key_prefix, created_at, last_used, request_count \
         FROM api_keys \
         ORDER BY created_at DESC",
    )
    .fetch_all(&state.db_pool)
    .await
    .unwrap_or_default();

    let responses: Vec<ApiKeyResponse> = rows
        .into_iter()
        .map(
            |(id, name, prefix, created_at, last_used, request_count)| ApiKeyResponse {
                id,
                name,
                prefix,
                key: None,
                created_at: created_at.parse().unwrap_or(Utc::now()),
                last_used: last_used.and_then(|d| d.parse().ok()),
                request_count,
            },
        )
        .collect();

    Ok(Json(ApiResponse::success(responses)))
}

/// POST /admin/api-keys
async fn create_api_key(
    State(state): State<Arc<ApiState>>,
    Json(req): Json<CreateApiKeyRequest>,
) -> Result<impl IntoResponse, StatusCode> {
    let raw_key = crate::api::generate_api_key();
    let key_hash = crate::api::hash_api_key(&raw_key);
    let prefix = raw_key[..8].to_string();
    let id = uuid::Uuid::new_v4().to_string();
    let created_at = Utc::now();

    sqlx::query(
        "INSERT INTO api_keys \
         (id, name, description, key_hash, key_prefix, created_at, request_count) \
         VALUES ($1, $2, $3, $4, $5, $6, 0)",
    )
    .bind(&id)
    .bind(&req.name)
    .bind(&req.description)
    .bind(&key_hash)
    .bind(&prefix)
    .bind(created_at.to_rfc3339())
    .execute(&state.db_pool)
    .await
    .map_err(db_err)?;

    Ok(Json(ApiResponse::success(ApiKeyResponse {
        id,
        name: req.name,
        prefix,
        key: Some(raw_key),
        created_at,
        last_used: None,
        request_count: 0,
    })))
}

/// DELETE /admin/api-keys/:id
async fn revoke_api_key(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, StatusCode> {
    let rows_affected = sqlx::query("DELETE FROM api_keys WHERE id = $1")
        .bind(&id)
        .execute(&state.db_pool)
        .await
        .map_err(db_err)?
        .rows_affected();

    if rows_affected == 0 {
        return Err(StatusCode::NOT_FOUND);
    }

    Ok(Json(ApiResponse::<()>::message(
        "API key revoked".to_string(),
    )))
}

/// GET /admin/jobs
async fn list_jobs(
    State(state): State<Arc<ApiState>>,
    Query(params): Query<JobsQuery>,
) -> Result<impl IntoResponse, StatusCode> {
    let limit = params.limit.unwrap_or(50).min(200);

    // Build query dynamically based on optional status filter.
    type JobRow = (
        String,
        String,
        String,
        f64,
        Option<String>,
        Option<String>,
        Option<String>,
        i64,
    );
    let rows: Vec<JobRow> = if let Some(ref status) = params.status {
        sqlx::query_as(
            "SELECT id, document_id, status, progress, \
             started_at, completed_at, error, chunks_processed \
             FROM indexing_jobs \
             WHERE status = $1 \
             ORDER BY created_at DESC \
             LIMIT $2",
        )
        .bind(status)
        .bind(limit)
        .fetch_all(&state.db_pool)
        .await
        .unwrap_or_default()
    } else {
        sqlx::query_as(
            "SELECT id, document_id, status, progress, \
             started_at, completed_at, error, chunks_processed \
             FROM indexing_jobs \
             ORDER BY created_at DESC \
             LIMIT $1",
        )
        .bind(limit)
        .fetch_all(&state.db_pool)
        .await
        .unwrap_or_default()
    };

    let responses: Vec<JobResponse> = rows
        .into_iter()
        .map(
            |(id, document_id, status, progress, started, completed, error, chunks)| {
                let started_at: Option<DateTime<Utc>> = started.and_then(|s| s.parse().ok());
                let completed_at: Option<DateTime<Utc>> = completed.and_then(|c| c.parse().ok());
                let duration_ms = match (started_at, completed_at) {
                    (Some(s), Some(c)) => Some((c - s).num_milliseconds()),
                    _ => None,
                };
                JobResponse {
                    id,
                    document_id,
                    status,
                    progress,
                    started_at,
                    completed_at,
                    duration_ms,
                    error,
                    chunks_processed: chunks,
                }
            },
        )
        .collect();

    Ok(Json(ApiResponse::success(responses)))
}

/// POST /admin/jobs/:id/retry
async fn retry_job(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, StatusCode> {
    let rows_affected =
        sqlx::query("UPDATE indexing_jobs SET status = 'pending', error = NULL WHERE id = $1")
            .bind(&id)
            .execute(&state.db_pool)
            .await
            .map_err(db_err)?
            .rows_affected();

    if rows_affected == 0 {
        return Err(StatusCode::NOT_FOUND);
    }

    Ok(Json(ApiResponse::<()>::message(
        "Job re-queued".to_string(),
    )))
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn admin_router_builds() {
        // Just ensure the router construction doesn't panic.
        let _r = admin_router();
    }
}
