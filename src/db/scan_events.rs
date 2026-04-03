// src/db/scan_events.rs
//! Scan events - activity feed for scanner operations and system events.
//! Provides real-time observability into what the scanner is doing.

use serde::{Deserialize, Serialize};
use sqlx::{FromRow, PgPool};

// ============================================================================
// Models
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct ScanEvent {
    pub id: i64,
    pub repo_id: Option<String>,
    pub event_type: String,
    pub message: String,
    pub details: Option<String>,
    pub level: String,
    pub created_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScanProgress {
    pub current_file: Option<String>,
    pub files_scanned: i32,
    pub total_files: i32,
    pub phase: String, // "cloning", "tree_walk", "scanning", "analyzing", "complete"
    pub percent: f32,
}

impl ScanProgress {
    pub fn new(phase: &str) -> Self {
        Self {
            current_file: None,
            files_scanned: 0,
            total_files: 0,
            phase: phase.to_string(),
            percent: 0.0,
        }
    }

    pub fn with_progress(mut self, done: i32, total: i32) -> Self {
        self.files_scanned = done;
        self.total_files = total;
        self.percent = if total > 0 {
            (done as f32 / total as f32) * 100.0
        } else {
            0.0
        };
        self
    }
}

// ============================================================================
// Scan Event CRUD
// ============================================================================

/// Log a scan event
pub async fn log_scan_event(
    pool: &PgPool,
    repo_id: Option<&str>,
    event_type: &str,
    message: &str,
    details: Option<&str>,
    level: &str,
) -> Result<i64, sqlx::Error> {
    let now = chrono::Utc::now().timestamp();

    let row: (i64,) = sqlx::query_as(
        r#"
        INSERT INTO scan_events (repo_id, event_type, message, details, level, created_at)
        VALUES ($1, $2, $3, $4, $5, $6)
        RETURNING id
        "#,
    )
    .bind(repo_id)
    .bind(event_type)
    .bind(message)
    .bind(details)
    .bind(level)
    .bind(now)
    .fetch_one(pool)
    .await?;

    Ok(row.0)
}

/// Convenience: log info event
pub async fn log_info(
    pool: &PgPool,
    repo_id: Option<&str>,
    event_type: &str,
    message: &str,
) -> Result<i64, sqlx::Error> {
    log_scan_event(pool, repo_id, event_type, message, None, "info").await
}

/// Convenience: log error event
pub async fn log_error(
    pool: &PgPool,
    repo_id: Option<&str>,
    event_type: &str,
    message: &str,
    error_detail: &str,
) -> Result<i64, sqlx::Error> {
    log_scan_event(
        pool,
        repo_id,
        event_type,
        message,
        Some(error_detail),
        "error",
    )
    .await
}

/// Get recent scan events (for activity feed)
pub async fn get_recent_events(
    pool: &PgPool,
    limit: i64,
    level_filter: Option<&str>,
) -> Result<Vec<ScanEvent>, sqlx::Error> {
    if let Some(level) = level_filter {
        sqlx::query_as::<_, ScanEvent>(
            r#"
            SELECT id, repo_id, event_type, message, details, level, created_at
            FROM scan_events
            WHERE level = $1
            ORDER BY created_at DESC
            LIMIT $2
            "#,
        )
        .bind(level)
        .bind(limit)
        .fetch_all(pool)
        .await
    } else {
        sqlx::query_as::<_, ScanEvent>(
            r#"
            SELECT id, repo_id, event_type, message, details, level, created_at
            FROM scan_events
            ORDER BY created_at DESC
            LIMIT $1
            "#,
        )
        .bind(limit)
        .fetch_all(pool)
        .await
    }
}

/// Get events for a specific repo
pub async fn get_repo_events(
    pool: &PgPool,
    repo_id: &str,
    limit: i64,
) -> Result<Vec<ScanEvent>, sqlx::Error> {
    sqlx::query_as::<_, ScanEvent>(
        r#"
        SELECT id, repo_id, event_type, message, details, level, created_at
        FROM scan_events
        WHERE repo_id = $1
        ORDER BY created_at DESC
        LIMIT $2
        "#,
    )
    .bind(repo_id)
    .bind(limit)
    .fetch_all(pool)
    .await
}

/// Prune old events (keep last N days)
pub async fn prune_events(pool: &PgPool, keep_days: i64) -> Result<u64, sqlx::Error> {
    let cutoff = chrono::Utc::now().timestamp() - (keep_days * 86400);
    let result = sqlx::query("DELETE FROM scan_events WHERE created_at < $1")
        .bind(cutoff)
        .execute(pool)
        .await?;
    Ok(result.rows_affected())
}

// ============================================================================
// Scan Progress Updates (on repositories table)
// ============================================================================

/// Update scan status for a repository
pub async fn update_scan_status(
    pool: &PgPool,
    repo_id: &str,
    status: &str,
    progress: Option<&ScanProgress>,
) -> Result<(), sqlx::Error> {
    let now = chrono::Utc::now().timestamp();
    let progress_json = progress.map(|p| serde_json::to_string(p).unwrap_or_default());

    sqlx::query(
        r#"
        UPDATE repositories
        SET scan_status = $1,
            scan_progress = $2,
            updated_at = $3
        WHERE id = $4
        "#,
    )
    .bind(status)
    .bind(progress_json)
    .bind(now)
    .bind(repo_id)
    .execute(pool)
    .await?;

    Ok(())
}

/// Mark scan as started
pub async fn mark_scan_started(
    pool: &PgPool,
    repo_id: &str,
    total_files: i32,
) -> Result<(), sqlx::Error> {
    let progress = ScanProgress::new("scanning").with_progress(0, total_files);
    update_scan_status(pool, repo_id, "scanning", Some(&progress)).await?;
    log_info(
        pool,
        Some(repo_id),
        "scan_start",
        &format!("Started scanning ({} files)", total_files),
    )
    .await?;
    Ok(())
}

/// Update scan file progress
pub async fn update_scan_file_progress(
    pool: &PgPool,
    repo_id: &str,
    current_file: &str,
    files_done: i32,
    total_files: i32,
) -> Result<(), sqlx::Error> {
    let now = chrono::Utc::now().timestamp();
    let progress = ScanProgress {
        current_file: Some(current_file.to_string()),
        files_scanned: files_done,
        total_files,
        phase: "scanning".to_string(),
        percent: if total_files > 0 {
            (files_done as f32 / total_files as f32) * 100.0
        } else {
            0.0
        },
    };
    let progress_json = serde_json::to_string(&progress).unwrap_or_default();

    sqlx::query(
        r#"
        UPDATE repositories
        SET scan_status = 'scanning',
            scan_progress = $1,
            scan_files_processed = $2,
            scan_files_total = $3,
            updated_at = $4
        WHERE id = $5
        "#,
    )
    .bind(&progress_json)
    .bind(files_done)
    .bind(total_files)
    .bind(now)
    .bind(repo_id)
    .execute(pool)
    .await?;

    Ok(())
}

/// Mark scan as complete
pub async fn mark_scan_complete(
    pool: &PgPool,
    repo_id: &str,
    files_scanned: i32,
    issues_found: i32,
    duration_ms: i64,
) -> Result<(), sqlx::Error> {
    let now = chrono::Utc::now().timestamp();

    sqlx::query(
        r#"
        UPDATE repositories
        SET scan_status = 'idle',
            scan_progress = NULL,
            scan_files_processed = $1,
            scan_files_total = $2,
            last_scan_issues_found = $3,
            last_scan_duration_ms = $4,
            last_scanned_at = $5,
            last_error = NULL,
            updated_at = $6
        WHERE id = $7
        "#,
    )
    .bind(files_scanned)
    .bind(issues_found)
    .bind(duration_ms)
    .bind(now)
    .bind(now)
    .bind(repo_id)
    .execute(pool)
    .await?;

    log_info(
        pool,
        Some(repo_id),
        "scan_complete",
        &format!(
            "Scan complete: {} files, {} issues, {:.1}s",
            files_scanned,
            issues_found,
            duration_ms as f64 / 1000.0
        ),
    )
    .await?;

    Ok(())
}

/// Mark scan as errored
pub async fn mark_scan_error(pool: &PgPool, repo_id: &str, error: &str) -> Result<(), sqlx::Error> {
    let now = chrono::Utc::now().timestamp();

    sqlx::query(
        r#"
        UPDATE repositories
        SET scan_status = 'error',
            scan_progress = NULL,
            last_error = $1,
            updated_at = $2
        WHERE id = $3
        "#,
    )
    .bind(error)
    .bind(now)
    .bind(repo_id)
    .execute(pool)
    .await?;

    log_error(pool, Some(repo_id), "scan_error", "Scan failed", error).await?;

    Ok(())
}
