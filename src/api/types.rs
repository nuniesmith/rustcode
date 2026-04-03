//! API request and response types for RAG endpoints

use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

// ============================================================================
// Shared Error Type
// ============================================================================

/// Standard API error response body.
///
/// Implements [`IntoResponse`] so handlers can return `Result<_, ApiError>`
/// and Axum will serialise the error automatically.
///
/// # Example
///
/// ```rust,ignore
/// async fn my_handler() -> Result<Json<Foo>, ApiError> {
///     let row = db_call().await.map_err(ApiError::internal)?;
///     Ok(Json(row))
/// }
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiError {
    pub error: String,
    pub code: String,
}

impl ApiError {
    pub fn not_found(msg: impl Into<String>) -> Self {
        Self {
            error: msg.into(),
            code: "NOT_FOUND".to_string(),
        }
    }

    pub fn internal(msg: impl Into<String>) -> Self {
        Self {
            error: msg.into(),
            code: "INTERNAL_ERROR".to_string(),
        }
    }

    pub fn bad_request(msg: impl Into<String>) -> Self {
        Self {
            error: msg.into(),
            code: "BAD_REQUEST".to_string(),
        }
    }

    pub fn unauthorized(msg: impl Into<String>) -> Self {
        Self {
            error: msg.into(),
            code: "UNAUTHORIZED".to_string(),
        }
    }

    /// Convenience constructor: map any `Display` error into an internal error.
    pub fn from_error(e: impl std::fmt::Display) -> Self {
        Self::internal(e.to_string())
    }

    fn status_code(&self) -> StatusCode {
        match self.code.as_str() {
            "NOT_FOUND" => StatusCode::NOT_FOUND,
            "BAD_REQUEST" => StatusCode::BAD_REQUEST,
            "UNAUTHORIZED" => StatusCode::UNAUTHORIZED,
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = self.status_code();
        (status, Json(self)).into_response()
    }
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "[{}] {}", self.code, self.error)
    }
}

impl std::error::Error for ApiError {}

// Automatically convert sqlx errors into internal API errors.
impl From<sqlx::Error> for ApiError {
    fn from(e: sqlx::Error) -> Self {
        tracing::error!(error = %e, "Database error in API handler");
        Self::internal(e.to_string())
    }
}

// Automatically convert anyhow errors.
impl From<anyhow::Error> for ApiError {
    fn from(e: anyhow::Error) -> Self {
        Self::internal(e.to_string())
    }
}

// ============================================================================
// Common Types
// ============================================================================

/// Standard API response wrapper
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiResponse<T> {
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<T>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

impl<T> ApiResponse<T> {
    pub fn success(data: T) -> Self {
        Self {
            success: true,
            data: Some(data),
            error: None,
            message: None,
        }
    }

    pub fn success_with_message(data: T, message: String) -> Self {
        Self {
            success: true,
            data: Some(data),
            error: None,
            message: Some(message),
        }
    }
}

impl ApiResponse<()> {
    pub fn error(error: String) -> Self {
        Self {
            success: false,
            data: None,
            error: Some(error),
            message: None,
        }
    }

    pub fn message(message: String) -> Self {
        Self {
            success: true,
            data: None,
            error: None,
            message: Some(message),
        }
    }
}

/// Pagination parameters
#[derive(Debug, Clone, Deserialize)]
pub struct PaginationQuery {
    #[serde(default = "default_page")]
    pub page: u32,
    #[serde(default = "default_limit")]
    pub limit: u32,
}

fn default_page() -> u32 {
    1
}

fn default_limit() -> u32 {
    20
}

/// Paginated response
#[derive(Debug, Clone, Serialize)]
pub struct PaginatedResponse<T> {
    pub items: Vec<T>,
    pub total: u32,
    pub page: u32,
    pub limit: u32,
    pub total_pages: u32,
}

impl<T> PaginatedResponse<T> {
    pub fn new(items: Vec<T>, total: u32, page: u32, limit: u32) -> Self {
        let total_pages = total.div_ceil(limit);
        Self {
            items,
            total,
            page,
            limit,
            total_pages,
        }
    }
}

// ============================================================================
// Document Management
// ============================================================================

/// Request to upload a document
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UploadDocumentRequest {
    pub title: String,
    pub content: String,
    pub doc_type: String,
    #[serde(default)]
    pub tags: Vec<String>,
    pub repo_id: Option<i64>,
    pub source_type: Option<String>,
    pub source_url: Option<String>,
}

/// Response for uploaded document
#[derive(Debug, Clone, Serialize)]
pub struct UploadDocumentResponse {
    pub id: String,
    pub title: String,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub message: String,
}

/// Request to update document metadata
/// Request to update document
#[derive(Debug, Clone, Deserialize)]
pub struct UpdateDocumentRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<String>>,
}

/// Document details response
#[derive(Debug, Clone, Serialize)]
pub struct DocumentResponse {
    pub id: String,
    pub title: String,
    pub content: String,
    pub doc_type: String,
    pub tags: Vec<String>,
    pub repo_id: Option<i64>,
    pub source_type: Option<String>,
    pub source_url: Option<String>,
    pub indexed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub chunk_count: i64,
}

/// List documents query parameters
#[derive(Debug, Clone, Deserialize)]
pub struct ListDocumentsQuery {
    #[serde(default = "default_page")]
    pub page: u32,
    #[serde(default = "default_limit")]
    pub limit: u32,
    pub doc_type: Option<String>,
    pub repo_id: Option<i64>,
    pub indexed_only: Option<bool>,
    pub tag: Option<String>,
}

// ============================================================================
// Search
// ============================================================================

/// Search request
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchRequest {
    pub query: String,
    #[serde(default = "default_search_limit")]
    pub limit: usize,
    #[serde(default)]
    pub search_type: SearchType,
    #[serde(default)]
    pub filters: SearchFiltersRequest,
}

fn default_search_limit() -> usize {
    10
}

/// Type of search to perform
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum SearchType {
    #[default]
    Hybrid,
    Semantic,
    Keyword,
}

/// Search filters
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SearchFiltersRequest {
    pub doc_type: Option<String>,
    pub tags: Option<Vec<String>>,
    pub repo_id: Option<i64>,
    pub source_type: Option<String>,
    pub indexed_only: Option<bool>,
    pub date_from: Option<DateTime<Utc>>,
    pub date_to: Option<DateTime<Utc>>,
}

/// Search result item
#[derive(Debug, Clone, Serialize)]
pub struct SearchResultItem {
    pub document_id: i64,
    pub chunk_id: i64,
    pub title: String,
    pub content: String,
    pub doc_type: String,
    pub score: f32,
    pub tags: Vec<String>,
    pub source_url: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// Search response
#[derive(Debug, Clone, Serialize)]
pub struct SearchResponse {
    pub results: Vec<SearchResultItem>,
    pub total_results: usize,
    pub search_type: SearchType,
    pub query: String,
    pub execution_time_ms: u64,
}

// ============================================================================
// Indexing
// ============================================================================

/// Request to index a document
#[derive(Debug, Clone, Deserialize)]
pub struct IndexDocumentRequest {
    pub document_id: String,
    #[serde(default)]
    pub force_reindex: bool,
}

/// Batch index request
#[derive(Debug, Clone, Deserialize)]
pub struct BatchIndexRequest {
    pub document_ids: Vec<String>,
    #[serde(default)]
    pub force_reindex: bool,
}

/// Index job response
#[derive(Debug, Clone, Serialize)]
pub struct IndexJobResponse {
    pub job_id: String,
    pub document_ids: Vec<String>,
    pub status: String,
    pub queued_at: DateTime<Utc>,
}

/// Index status response
#[derive(Debug, Clone, Serialize)]
pub struct IndexStatusResponse {
    pub job_id: String,
    pub status: IndexJobStatus,
    pub documents_total: usize,
    pub documents_completed: usize,
    pub documents_failed: usize,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum IndexJobStatus {
    Queued,
    Processing,
    Completed,
    Failed,
}

// ============================================================================
// Statistics
// ============================================================================

/// System statistics response
#[derive(Debug, Clone, Serialize)]
pub struct StatsResponse {
    pub documents: DocumentStats,
    pub chunks: ChunkStats,
    pub search: SearchStats,
    pub indexing: IndexingStats,
}

#[derive(Debug, Clone, Serialize)]
pub struct DocumentStats {
    pub total: i64,
    pub indexed: i64,
    pub pending: i64,
    pub by_type: Vec<TypeCount>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChunkStats {
    pub total: i64,
    pub avg_per_document: f64,
    pub avg_size: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct SearchStats {
    pub total_searches: i64,
    pub avg_results: f64,
    pub avg_execution_time_ms: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct IndexingStats {
    pub jobs_queued: i64,
    pub jobs_processing: i64,
    pub jobs_completed: i64,
    pub jobs_failed: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct TypeCount {
    pub doc_type: String,
    pub count: i64,
}

// ============================================================================
// Health & Status
// ============================================================================

#[derive(Debug, Clone, Serialize)]
pub struct HealthResponse {
    pub status: String,
    pub version: String,
    pub uptime_seconds: u64,
    pub services: ServiceHealth,
}

#[derive(Debug, Clone, Serialize)]
pub struct ServiceHealth {
    pub database: bool,
    pub embeddings: bool,
    pub search: bool,
}
