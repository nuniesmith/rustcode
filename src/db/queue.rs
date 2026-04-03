//! Task Queue and Analysis Pipeline Database Schema (Legacy)
//!
//! Handles staged processing of content from raw input through
//! LLM analysis to tagged, searchable knowledge.
//!
//! **DEPRECATED:** The primary task system is now the `tasks` table
//! (managed by `db::core::create_task` / `db::core::list_tasks`).
//! The auto-scanner writes project review tasks directly to `tasks`,
//! and the web UI dashboard + `/queue` page read from `tasks`.
//!
//! This module defines the `queue_items` table schema and types used
//! by the legacy staged pipeline in `queue/processor.rs`. It is still
//! active for note/thought/TODO capture but should eventually be
//! consolidated into the `tasks` table.

use serde::{Deserialize, Serialize};
use sqlx::{FromRow, PgPool};

// ============================================================================
// Configuration
// ============================================================================

/// GitHub username for repo scanning
pub const GITHUB_USERNAME: &str = "nuniesmith";

// ============================================================================
// Queue Item Stages
// ============================================================================

/// Processing stages for queue items
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type)]
#[sqlx(type_name = "TEXT", rename_all = "snake_case")]
#[derive(Default)]
pub enum QueueStage {
    /// Just captured, no processing yet
    #[default]
    Inbox,
    /// Waiting for LLM analysis
    PendingAnalysis,
    /// Currently being analyzed
    Analyzing,
    /// Analysis complete, waiting for tagging
    PendingTagging,
    /// Fully processed and ready for use
    Ready,
    /// Processing failed (with retry count)
    Failed,
    /// Archived/inactive
    Archived,
}

/// Source type for queue items
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type)]
#[sqlx(type_name = "TEXT", rename_all = "snake_case")]
pub enum QueueSource {
    /// Manual note input
    Note,
    /// TODO comment from code
    TodoComment,
    /// File from repository
    RepoFile,
    /// Random thought/idea capture
    RawThought,
    /// Research snippet
    Research,
    /// External document (PDF, etc)
    Document,
}

/// Priority levels for processing
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type)]
#[sqlx(type_name = "INTEGER")]
#[derive(Default)]
pub enum QueuePriority {
    Critical = 1,
    High = 2,
    #[default]
    Normal = 3,
    Low = 4,
    Background = 5,
}

// ============================================================================
// Queue Item Model
// ============================================================================

/// A single item in the processing queue
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct QueueItem {
    pub id: String,

    /// The raw content to process
    pub content: String,

    /// Current processing stage
    pub stage: String, // QueueStage as string for SQLite

    /// Where this item came from
    pub source: String, // QueueSource as string

    /// Processing priority
    pub priority: i32,

    /// Associated repository (if from code)
    pub repo_id: Option<String>,

    /// File path within repo (if applicable)
    pub file_path: Option<String>,

    /// Line number (for TODOs)
    pub line_number: Option<i32>,

    /// LLM-generated analysis (JSON blob)
    pub analysis: Option<String>,

    /// LLM-generated tags (comma-separated)
    pub tags: Option<String>,

    /// LLM-assigned category
    pub category: Option<String>,

    /// Quality/importance score (1-10)
    pub score: Option<i32>,

    /// Number of processing attempts
    pub retry_count: i32,

    /// Last error message if failed
    pub last_error: Option<String>,

    /// Content hash for deduplication
    pub content_hash: String,

    pub created_at: i64,
    pub updated_at: i64,
    pub processed_at: Option<i64>,
}

// ============================================================================
// File Analysis Cache (Per-Repo)
// ============================================================================

/// Cached analysis for a single file
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct FileAnalysis {
    pub id: String,

    /// Repository this file belongs to
    pub repo_id: String,

    /// Full path within repo
    pub file_path: String,

    /// File extension
    pub extension: Option<String>,

    /// Content hash (to detect changes)
    pub content_hash: String,

    /// File size in bytes
    pub size_bytes: i64,

    /// Number of lines
    pub line_count: i32,

    /// LLM-generated summary
    pub summary: Option<String>,

    /// LLM-assigned purpose/role
    pub purpose: Option<String>,

    /// Detected language
    pub language: Option<String>,

    /// Complexity score (1-10)
    pub complexity_score: Option<i32>,

    /// Quality score (1-10)
    pub quality_score: Option<i32>,

    /// Security concerns found
    pub security_notes: Option<String>,

    /// Suggested improvements (JSON array)
    pub improvements: Option<String>,

    /// Dependencies detected (JSON array)
    pub dependencies: Option<String>,

    /// Exports/public API (JSON array)
    pub exports: Option<String>,

    /// Tags for categorization
    pub tags: Option<String>,

    /// Whether this file needs attention
    pub needs_attention: bool,

    /// Last analyzed timestamp
    pub analyzed_at: Option<i64>,

    pub created_at: i64,
    pub updated_at: i64,
}

// ============================================================================
// Repository Cache Metadata
// ============================================================================

/// Per-repository cache metadata
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct RepoCache {
    pub id: String,

    /// Repository ID (foreign key)
    pub repo_id: String,

    /// Directory tree (JSON structure)
    pub dir_tree: Option<String>,

    /// Total files in repo
    pub total_files: i32,

    /// Files analyzed so far
    pub analyzed_files: i32,

    /// Total TODOs found
    pub total_todos: i32,

    /// Open/active TODOs
    pub active_todos: i32,

    /// Overall repo health score (1-10)
    pub health_score: Option<i32>,

    /// Primary languages (JSON array)
    pub languages: Option<String>,

    /// Key patterns detected
    pub patterns: Option<String>,

    /// Standardization issues found
    pub standardization_issues: Option<String>,

    /// Last full scan timestamp
    pub last_scan_at: Option<i64>,

    /// Last tree update timestamp
    pub tree_updated_at: Option<i64>,

    pub created_at: i64,
    pub updated_at: i64,
}

// ============================================================================
// Table Creation
// ============================================================================

pub async fn create_queue_tables(pool: &PgPool) -> Result<(), sqlx::Error> {
    // Queue items table
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS queue_items (
            id TEXT PRIMARY KEY,
            content TEXT NOT NULL,
            stage TEXT NOT NULL DEFAULT 'inbox',
            source TEXT NOT NULL DEFAULT 'note',
            priority INTEGER NOT NULL DEFAULT 3,
            repo_id TEXT,
            file_path TEXT,
            line_number INTEGER,
            analysis TEXT,
            tags TEXT,
            category TEXT,
            score INTEGER,
            retry_count INTEGER NOT NULL DEFAULT 0,
            last_error TEXT,
            content_hash TEXT NOT NULL,
            created_at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL,
            processed_at INTEGER,
            FOREIGN KEY (repo_id) REFERENCES repositories(id)
        )
    "#,
    )
    .execute(pool)
    .await?;

    // File analysis cache
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS file_analysis (
            id TEXT PRIMARY KEY,
            repo_id TEXT NOT NULL,
            file_path TEXT NOT NULL,
            extension TEXT,
            content_hash TEXT NOT NULL,
            size_bytes INTEGER NOT NULL,
            line_count INTEGER NOT NULL,
            summary TEXT,
            purpose TEXT,
            language TEXT,
            complexity_score INTEGER,
            quality_score INTEGER,
            security_notes TEXT,
            improvements TEXT,
            dependencies TEXT,
            exports TEXT,
            tags TEXT,
            needs_attention INTEGER NOT NULL DEFAULT 0,
            analyzed_at INTEGER,
            created_at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL,
            UNIQUE(repo_id, file_path),
            FOREIGN KEY (repo_id) REFERENCES repositories(id)
        )
    "#,
    )
    .execute(pool)
    .await?;

    // Note: todo_items table was removed in migration 017_drop_todo_items.sql.
    // TODOs are now stored in the `tasks` table (source = 'github_scanner' or
    // 'queue_processor'). No DDL emitted here.

    // Repository cache
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS repo_cache (
            id TEXT PRIMARY KEY,
            repo_id TEXT NOT NULL UNIQUE,
            dir_tree TEXT,
            total_files INTEGER NOT NULL DEFAULT 0,
            analyzed_files INTEGER NOT NULL DEFAULT 0,
            total_todos INTEGER NOT NULL DEFAULT 0,
            active_todos INTEGER NOT NULL DEFAULT 0,
            health_score INTEGER,
            languages TEXT,
            patterns TEXT,
            standardization_issues TEXT,
            last_scan_at INTEGER,
            tree_updated_at INTEGER,
            created_at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL,
            FOREIGN KEY (repo_id) REFERENCES repositories(id)
        )
    "#,
    )
    .execute(pool)
    .await?;

    // Indexes for queue processing
    sqlx::query("CREATE INDEX IF NOT EXISTS idx_queue_stage ON queue_items(stage)")
        .execute(pool)
        .await?;
    sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_queue_priority ON queue_items(priority, created_at)",
    )
    .execute(pool)
    .await?;
    sqlx::query("CREATE INDEX IF NOT EXISTS idx_queue_source ON queue_items(source)")
        .execute(pool)
        .await?;
    sqlx::query("CREATE INDEX IF NOT EXISTS idx_queue_hash ON queue_items(content_hash)")
        .execute(pool)
        .await?;

    // Indexes for file analysis
    sqlx::query("CREATE INDEX IF NOT EXISTS idx_file_repo ON file_analysis(repo_id)")
        .execute(pool)
        .await?;
    sqlx::query("CREATE INDEX IF NOT EXISTS idx_file_hash ON file_analysis(content_hash)")
        .execute(pool)
        .await?;
    sqlx::query("CREATE INDEX IF NOT EXISTS idx_file_attention ON file_analysis(needs_attention)")
        .execute(pool)
        .await?;

    // Note: idx_todo_* indexes were dropped along with todo_items in migration 017.

    Ok(())
}
