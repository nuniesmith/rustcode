// Database module for Rustassistant
//
// Provides PostgreSQL-based storage for notes, repositories, and tasks.
// Uses sqlx for async database operations.

use serde::{Deserialize, Serialize};
use sqlx::{FromRow, PgPool, postgres::PgPoolOptions};
use thiserror::Error;

// ============================================================================
// Error Types
// ============================================================================

#[derive(Error, Debug)]
pub enum DbError {
    #[error("Database error: {0}")]
    Sqlx(#[from] sqlx::Error),

    #[error("Not found: {0}")]
    NotFound(String),

    #[error("Invalid input: {0}")]
    InvalidInput(String),
}

pub type DbResult<T> = Result<T, DbError>;

// Type alias for convenience
pub type DbPool = PgPool;

// ============================================================================
// Models
// ============================================================================

// A note/thought captured by the user
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct Note {
    pub id: String,
    pub title: String,
    pub content: String,
    pub status: String,
    #[sqlx(default)]
    pub repo_id: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
    // Comma-separated tags — populated from the `notes_with_tags` view when
    // queried via tag-aware helpers; NULL when queried from the bare table.
    #[sqlx(default)]
    pub tags: Option<String>,
}

impl Note {
    // Get status as a string (legacy API)
    pub fn status_str(&self) -> &str {
        &self.status
    }

    // Get formatted created_at timestamp (legacy API)
    pub fn created_at_formatted(&self) -> String {
        chrono::DateTime::from_timestamp(self.created_at, 0)
            .map(|dt| dt.format("%Y-%m-%d %H:%M:%S").to_string())
            .unwrap_or_else(|| "unknown".to_string())
    }

    // Get formatted updated_at timestamp (legacy API)
    pub fn updated_at_formatted(&self) -> String {
        chrono::DateTime::from_timestamp(self.updated_at, 0)
            .map(|dt| dt.format("%Y-%m-%d %H:%M:%S").to_string())
            .unwrap_or_else(|| "unknown".to_string())
    }
}

// A tag for categorizing notes
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct Tag {
    pub name: String,
    pub color: String,
    pub description: Option<String>,
    pub usage_count: i64,
    pub created_at: i64,
    pub updated_at: i64,
}

impl Tag {
    // Get formatted created_at timestamp
    pub fn created_at_formatted(&self) -> String {
        chrono::DateTime::from_timestamp(self.created_at, 0)
            .map(|dt| dt.format("%Y-%m-%d %H:%M:%S").to_string())
            .unwrap_or_else(|| "unknown".to_string())
    }
}

// Note-Tag relationship
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct NoteTag {
    pub note_id: String,
    pub tag: String,
    pub created_at: i64,
}

// A tracked repository
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct Repository {
    pub id: String,
    #[sqlx(rename = "local_path")]
    pub path: String,
    pub name: String,
    #[sqlx(default)]
    pub status: String,
    #[sqlx(rename = "last_scanned_at")]
    pub last_analyzed: Option<i64>,
    #[sqlx(default)]
    pub metadata: Option<String>, // JSON blob
    #[sqlx(rename = "auto_scan")]
    pub auto_scan_enabled: i32,
    #[sqlx(rename = "scan_interval_mins")]
    pub scan_interval_minutes: i32,
    #[sqlx(default)]
    pub last_scan_check: Option<i64>,
    pub last_commit_hash: Option<String>,
    #[sqlx(rename = "url")]
    pub git_url: Option<String>, // GitHub clone URL
    pub created_at: i64,
    pub updated_at: i64,
    // Scan progress tracking
    #[sqlx(default)]
    pub scan_status: Option<String>, // idle/scanning/error
    #[sqlx(default)]
    pub scan_progress: Option<String>,
    #[sqlx(default)]
    pub scan_current_file: Option<String>,
    #[sqlx(default)]
    pub scan_files_total: Option<i32>,
    #[sqlx(default)]
    pub scan_files_processed: Option<i32>,
    #[sqlx(default)]
    pub last_scan_duration_ms: Option<i64>,
    #[sqlx(default)]
    pub last_scan_files_found: Option<i32>,
    #[sqlx(default)]
    pub last_scan_issues_found: Option<i32>,
    #[sqlx(default)]
    pub last_error: Option<String>,
    // Flag set via the API to request a project review re-run
    #[sqlx(default)]
    pub review_requested: Option<bool>,
}

impl Repository {
    // Get formatted created_at timestamp (legacy API)
    pub fn created_at_formatted(&self) -> String {
        chrono::DateTime::from_timestamp(self.created_at, 0)
            .map(|dt| dt.format("%Y-%m-%d %H:%M:%S").to_string())
            .unwrap_or_else(|| "unknown".to_string())
    }

    // Get scan status display string
    pub fn scan_status_display(&self) -> String {
        match self.scan_status.as_deref() {
            Some("scanning") => "🔄 Scanning".to_string(),
            Some("error") => "❌ Error".to_string(),
            _ => "✅ Idle".to_string(),
        }
    }

    // Get progress percentage (0-100)
    pub fn progress_percentage(&self) -> i64 {
        match (self.scan_files_processed, self.scan_files_total) {
            (Some(processed), Some(total)) if total > 0 => {
                ((processed as i64 * 100) / total as i64).min(100)
            }
            _ => 0,
        }
    }

    // Check if auto-scan is enabled
    pub fn is_auto_scan_enabled(&self) -> bool {
        self.auto_scan_enabled != 0
    }
}

// A generated or manual task
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct Task {
    pub id: String,
    // Title — may be NULL for rows created by the legacy (migration-001) schema,
    // where the equivalent column is `content`.
    #[sqlx(default)]
    pub title: String,
    #[sqlx(default)]
    pub description: Option<String>,
    pub priority: i32, // 1=critical, 2=high, 3=medium, 4=low
    pub status: String,
    #[sqlx(default)]
    pub source: String, // "note", "analysis", "manual"
    #[sqlx(default)]
    pub source_id: Option<String>, // ID of note or file that generated this
    #[sqlx(default)]
    pub repo_id: Option<String>,
    #[sqlx(default)]
    pub file_path: Option<String>,
    #[sqlx(default)]
    pub line_number: Option<i32>,
    pub created_at: i64,
    pub updated_at: i64,
}

// Document model for RAG system
#[derive(Debug, Clone)]
pub struct Document {
    pub id: String,
    pub title: String,
    pub content: String,
    pub content_type: String, // markdown, text, code, html
    pub source_type: String,  // manual, url, file, repo
    pub source_url: Option<String>,
    pub doc_type: String, // reference, research, tutorial, architecture, note, snippet
    pub tags: Option<String>, // Comma-separated for backward compat
    pub repo_id: Option<String>,
    pub file_path: Option<String>,
    pub word_count: i64,
    pub char_count: i64,
    pub created_at: i64,
    pub updated_at: i64,
    pub indexed_at: Option<i64>,
    // Whether this document is pinned (surfaced first in listings).
    // Defaults to `false`; set via `POST /api/web/docs/:id/pin`.
    pub pinned: bool,
}

impl Document {
    // Format created_at timestamp
    pub fn created_at_formatted(&self) -> String {
        chrono::DateTime::from_timestamp(self.created_at, 0)
            .map(|dt| dt.format("%Y-%m-%d %H:%M").to_string())
            .unwrap_or_default()
    }

    // Format updated_at timestamp
    pub fn updated_at_formatted(&self) -> String {
        chrono::DateTime::from_timestamp(self.updated_at, 0)
            .map(|dt| dt.format("%Y-%m-%d %H:%M").to_string())
            .unwrap_or_default()
    }

    // Get indexing status
    pub fn index_status(&self) -> &str {
        match self.indexed_at {
            None => "not_indexed",
            Some(indexed) if self.updated_at > indexed => "needs_reindex",
            Some(_) => "indexed",
        }
    }

    // Parse tags into Vec
    pub fn tag_list(&self) -> Vec<String> {
        self.tags
            .as_ref()
            .map(|t| t.split(',').map(|s| s.trim().to_string()).collect())
            .unwrap_or_default()
    }

    // Return a pin icon string suitable for HTML display.
    pub fn pin_icon(&self) -> &'static str {
        if self.pinned { "📌 " } else { "" }
    }
}

// Document chunk for embeddings
#[derive(Debug, Clone)]
pub struct DocumentChunk {
    pub id: String,
    pub document_id: String,
    pub chunk_index: i64,
    pub content: String,
    pub char_start: i64,
    pub char_end: i64,
    pub word_count: i64,
    pub heading: Option<String>,
    pub created_at: i64,
}

// Document embedding
#[derive(Debug, Clone)]
pub struct DocumentEmbedding {
    pub id: String,
    pub chunk_id: String,
    pub embedding: String, // JSON array of floats
    pub model: String,
    pub dimension: i64,
    pub created_at: i64,
}

impl DocumentEmbedding {
    // Parse embedding from JSON string
    pub fn parse_embedding(&self) -> Result<Vec<f32>, serde_json::Error> {
        serde_json::from_str(&self.embedding)
    }

    // Create from vector
    pub fn from_vector(chunk_id: String, vector: &[f32], model: String) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            chunk_id,
            embedding: serde_json::to_string(vector).unwrap_or_default(),
            model,
            dimension: vector.len() as i64,
            created_at: chrono::Utc::now().timestamp(),
        }
    }
}

// Document tag
#[derive(Debug, Clone)]
pub struct DocumentTag {
    pub document_id: String,
    pub tag: String,
    pub created_at: i64,
}

// Database initialization
// ============================================================================

// Initialize the database connection pool and create tables
pub async fn init_db(database_url: &str) -> DbResult<PgPool> {
    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(database_url)
        .await?;

    // Run all migrations from the ./migrations directory.
    // This replaces the old inline create_tables() + create_queue_tables() calls.
    sqlx::migrate!("./sql")
        .run(&pool)
        .await
        .map_err(|e| DbError::Sqlx(e.into()))?;

    Ok(pool)
}

// ============================================================================
// Note Operations
// ============================================================================

// Create a new note with optional tags and repo linking
pub async fn create_note_with_tags(
    pool: &PgPool,
    content: &str,
    tags: &[&str],
    _project: Option<&str>,
    repo_id: Option<&str>,
) -> DbResult<Note> {
    let id = uuid::Uuid::new_v4().to_string();
    let now = chrono::Utc::now().timestamp();
    // Use content as the title (first 120 chars), store full text in content column.
    let title: String = content.chars().take(120).collect();

    // Insert the note — the `notes` table has no `tags` or `project` column;
    // tags are stored in `note_tags` (many-to-many).
    sqlx::query(
        r#"
        INSERT INTO notes (id, title, content, status, repo_id, created_at, updated_at)
        VALUES ($1, $2, $3, 'active', $4, $5, $6)
        "#,
    )
    .bind(&id)
    .bind(&title)
    .bind(content)
    .bind(repo_id)
    .bind(now)
    .bind(now)
    .execute(pool)
    .await?;

    // Add tags to note_tags table
    if !tags.is_empty() {
        set_note_tags(pool, &id, tags).await?;
    }

    Ok(Note {
        id,
        title,
        content: content.to_string(),
        tags: if tags.is_empty() {
            None
        } else {
            Some(tags.join(","))
        },
        status: "active".to_string(),
        repo_id: repo_id.map(|s| s.to_string()),
        created_at: now,
        updated_at: now,
    })
}

// Create a new note (legacy API - backward compatible)
pub async fn create_note(
    pool: &PgPool,
    content: &str,
    tags: Option<&str>,
    project: Option<&str>,
) -> DbResult<Note> {
    let tag_vec: Vec<&str> = tags
        .map(|t| t.split(',').map(|s| s.trim()).collect())
        .unwrap_or_default();

    create_note_with_tags(pool, content, &tag_vec, project, None).await
}

// Get a note by ID
pub async fn get_note(pool: &PgPool, id: &str) -> DbResult<Note> {
    sqlx::query_as::<_, Note>("SELECT * FROM notes WHERE id = $1")
        .bind(id)
        .fetch_optional(pool)
        .await?
        .ok_or_else(|| DbError::NotFound(format!("Note not found: {}", id)))
}

// List notes with optional filtering
pub async fn list_notes(
    pool: &PgPool,
    limit: i64,
    status: Option<&str>,
    _project: Option<&str>,
    tag: Option<&str>,
) -> DbResult<Vec<Note>> {
    // When filtering by tag use the notes_with_tags view (has aggregated tags
    // column); otherwise query the bare notes table which is cheaper.
    let mut param_idx: u32 = 1;

    let (base_table, tag_join) = if tag.is_some() {
        ("notes_with_tags", true)
    } else {
        ("notes", false)
    };

    let mut query = format!("SELECT * FROM {} WHERE 1=1", base_table);

    if status.is_some() {
        query.push_str(&format!(" AND status = ${}", param_idx));
        param_idx += 1;
    }
    if tag_join {
        query.push_str(&format!(" AND tags LIKE ${}", param_idx));
        param_idx += 1;
    }

    query.push_str(&format!(" ORDER BY created_at DESC LIMIT ${}", param_idx));

    let mut q = sqlx::query_as::<_, Note>(&query);

    if let Some(s) = status {
        q = q.bind(s);
    }
    if let Some(t) = tag {
        q = q.bind(format!("%{}%", t));
    }
    q = q.bind(limit);

    Ok(q.fetch_all(pool).await?)
}

// Search notes by content or title
pub async fn search_notes(pool: &PgPool, query: &str, limit: i64) -> DbResult<Vec<Note>> {
    let search_pattern = format!("%{}%", query);

    Ok(sqlx::query_as::<_, Note>(
        r#"
        SELECT * FROM notes
        WHERE content ILIKE $1 OR title ILIKE $2
        ORDER BY created_at DESC
        LIMIT $3
        "#,
    )
    .bind(&search_pattern)
    .bind(&search_pattern)
    .bind(limit)
    .fetch_all(pool)
    .await?)
}

// Update note status
pub async fn update_note_status(pool: &PgPool, id: &str, status: &str) -> DbResult<()> {
    let now = chrono::Utc::now().timestamp();

    let result = sqlx::query("UPDATE notes SET status = $1, updated_at = $2 WHERE id = $3")
        .bind(status)
        .bind(now)
        .bind(id)
        .execute(pool)
        .await?;

    if result.rows_affected() == 0 {
        return Err(DbError::NotFound(format!("Note not found: {}", id)));
    }

    Ok(())
}

// Delete a note
pub async fn delete_note(pool: &PgPool, id: &str) -> DbResult<()> {
    let result = sqlx::query("DELETE FROM notes WHERE id = $1")
        .bind(id)
        .execute(pool)
        .await?;

    if result.rows_affected() == 0 {
        return Err(DbError::NotFound(format!("Note not found: {}", id)));
    }

    Ok(())
}

// ============================================================================
// Tag Operations
// ============================================================================

// Get all tags ordered by usage count
pub async fn list_tags(pool: &PgPool) -> DbResult<Vec<Tag>> {
    Ok(
        sqlx::query_as::<_, Tag>("SELECT * FROM tags ORDER BY usage_count DESC, name ASC")
            .fetch_all(pool)
            .await?,
    )
}

// Get a specific tag by name
pub async fn get_tag(pool: &PgPool, name: &str) -> DbResult<Option<Tag>> {
    Ok(
        sqlx::query_as::<_, Tag>("SELECT * FROM tags WHERE name = $1")
            .bind(name)
            .fetch_optional(pool)
            .await?,
    )
}

// Create or update a tag
pub async fn upsert_tag(
    pool: &PgPool,
    name: &str,
    color: Option<&str>,
    description: Option<&str>,
) -> DbResult<Tag> {
    let now = chrono::Utc::now().timestamp();

    sqlx::query(
        r#"
        INSERT INTO tags (name, color, description, created_at, updated_at)
        VALUES ($1, $2, $3, $4, $5)
        ON CONFLICT(name) DO UPDATE SET
            color = COALESCE($6, color),
            description = COALESCE($7, description),
            updated_at = $8
        "#,
    )
    .bind(name)
    .bind(color.unwrap_or("#3b82f6"))
    .bind(description)
    .bind(now)
    .bind(now)
    .bind(color)
    .bind(description)
    .bind(now)
    .execute(pool)
    .await?;

    get_tag(pool, name)
        .await?
        .ok_or_else(|| DbError::NotFound(format!("Tag not found after upsert: {}", name)))
}

// Delete a tag (will cascade delete note_tags relationships)
pub async fn delete_tag(pool: &PgPool, name: &str) -> DbResult<()> {
    let result = sqlx::query("DELETE FROM tags WHERE name = $1")
        .bind(name)
        .execute(pool)
        .await?;

    if result.rows_affected() == 0 {
        return Err(DbError::NotFound(format!("Tag not found: {}", name)));
    }

    Ok(())
}

// Add a tag to a note
pub async fn add_tag_to_note(pool: &PgPool, note_id: &str, tag: &str) -> DbResult<()> {
    let now = chrono::Utc::now().timestamp();

    sqlx::query(
        r#"
        INSERT INTO note_tags (note_id, tag, created_at)
        VALUES ($1, $2, $3)
        ON CONFLICT DO NOTHING
        "#,
    )
    .bind(note_id)
    .bind(tag)
    .bind(now)
    .execute(pool)
    .await?;

    Ok(())
}

// Remove a tag from a note
pub async fn remove_tag_from_note(pool: &PgPool, note_id: &str, tag: &str) -> DbResult<()> {
    sqlx::query("DELETE FROM note_tags WHERE note_id = $1 AND tag = $2")
        .bind(note_id)
        .bind(tag)
        .execute(pool)
        .await?;

    Ok(())
}

// Get all tags for a note
pub async fn get_note_tags(pool: &PgPool, note_id: &str) -> DbResult<Vec<String>> {
    let tags = sqlx::query_scalar::<_, String>("SELECT tag FROM note_tags WHERE note_id = $1")
        .bind(note_id)
        .fetch_all(pool)
        .await?;

    Ok(tags)
}

// Set tags for a note (replaces existing tags)
pub async fn set_note_tags(pool: &PgPool, note_id: &str, tags: &[&str]) -> DbResult<()> {
    // Start a transaction
    let mut tx = pool.begin().await?;

    // Remove existing tags
    sqlx::query("DELETE FROM note_tags WHERE note_id = $1")
        .bind(note_id)
        .execute(&mut *tx)
        .await?;

    // Add new tags
    let now = chrono::Utc::now().timestamp();
    for tag in tags {
        sqlx::query(
            r#"
            INSERT INTO note_tags (note_id, tag, created_at)
            VALUES ($1, $2, $3)
            ON CONFLICT DO NOTHING
            "#,
        )
        .bind(note_id)
        .bind(tag)
        .bind(now)
        .execute(&mut *tx)
        .await?;
    }

    tx.commit().await?;
    Ok(())
}

// Search notes by tags (AND logic - note must have all specified tags)
pub async fn search_notes_by_tags(pool: &PgPool, tags: &[&str], limit: i64) -> DbResult<Vec<Note>> {
    if tags.is_empty() {
        return list_notes(pool, limit, None, None, None).await;
    }

    // Build query with multiple tag filters
    let tag_count = tags.len();
    let query = format!(
        r#"
        SELECT DISTINCT n.*
        FROM notes n
        INNER JOIN note_tags nt ON n.id = nt.note_id
        WHERE nt.tag IN ({})
        GROUP BY n.id
        HAVING COUNT(DISTINCT nt.tag) = $1
        ORDER BY n.created_at DESC
        LIMIT $2
        "#,
        (1..=tag_count)
            .map(|i| format!("${}", i))
            .collect::<Vec<_>>()
            .join(",")
    );

    // Tags are bound as $1..$N, then HAVING count = $(N+1), LIMIT = $(N+2)
    let having_idx = tag_count + 1;
    let limit_idx = tag_count + 2;

    // Patch in the correct positional indexes for HAVING and LIMIT
    let query = query
        .replace(
            "HAVING COUNT(DISTINCT nt.tag) = $1",
            &format!("HAVING COUNT(DISTINCT nt.tag) = ${}", having_idx),
        )
        .replace("LIMIT $2", &format!("LIMIT ${}", limit_idx));

    let mut q = sqlx::query_as::<_, Note>(&query);
    for tag in tags {
        q = q.bind(*tag);
    }
    q = q.bind(tag_count as i64).bind(limit);

    Ok(q.fetch_all(pool).await?)
}

// Update note with repo_id
pub async fn update_note_repo(pool: &PgPool, note_id: &str, repo_id: Option<&str>) -> DbResult<()> {
    let now = chrono::Utc::now().timestamp();

    let result = sqlx::query("UPDATE notes SET repo_id = $1, updated_at = $2 WHERE id = $3")
        .bind(repo_id)
        .bind(now)
        .bind(note_id)
        .execute(pool)
        .await?;

    if result.rows_affected() == 0 {
        return Err(DbError::NotFound(format!("Note not found: {}", note_id)));
    }

    Ok(())
}

// Get notes for a repository
pub async fn get_repo_notes(pool: &PgPool, repo_id: &str, limit: i64) -> DbResult<Vec<Note>> {
    Ok(sqlx::query_as::<_, Note>(
        r#"
            SELECT * FROM notes
            WHERE repo_id = $1
            ORDER BY created_at DESC
            LIMIT $2
            "#,
    )
    .bind(repo_id)
    .bind(limit)
    .fetch_all(pool)
    .await?)
}

// Count notes for a repository
pub async fn count_repo_notes(pool: &PgPool, repo_id: &str) -> DbResult<i64> {
    let count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM notes WHERE repo_id = $1")
        .bind(repo_id)
        .fetch_one(pool)
        .await?;

    Ok(count.0)
}

// ============================================================================
// Repository Operations
// ============================================================================

// Add a repository to track
pub async fn add_repository(
    pool: &PgPool,
    path: &str,
    name: &str,
    git_url: Option<&str>,
) -> DbResult<Repository> {
    let id = uuid::Uuid::new_v4().to_string();
    let now = chrono::Utc::now().timestamp();

    sqlx::query(
        r#"
        INSERT INTO repositories (id, local_path, name, url, created_at, updated_at)
        VALUES ($1, $2, $3, $4, $5, $6)
        "#,
    )
    .bind(&id)
    .bind(path)
    .bind(name)
    .bind(git_url)
    .bind(now)
    .bind(now)
    .execute(pool)
    .await?;

    Ok(Repository {
        id,
        path: path.to_string(),
        name: name.to_string(),
        status: "active".to_string(),
        last_analyzed: None,
        metadata: None,
        auto_scan_enabled: 0,
        scan_interval_minutes: 60,
        last_scan_check: None,
        last_commit_hash: None,
        git_url: git_url.map(|s| s.to_string()),
        created_at: now,
        updated_at: now,
        // Scan progress tracking defaults
        scan_status: Some("idle".to_string()),
        scan_progress: None,
        scan_current_file: None,
        scan_files_total: Some(0i32),
        scan_files_processed: Some(0i32),
        last_scan_duration_ms: None,
        last_scan_files_found: Some(0i32),
        last_scan_issues_found: Some(0i32),
        last_error: None,
        review_requested: None,
    })
}

// Get a repository by ID
pub async fn get_repository(pool: &PgPool, id: &str) -> DbResult<Repository> {
    sqlx::query_as::<_, Repository>("SELECT * FROM repositories WHERE id = $1")
        .bind(id)
        .fetch_optional(pool)
        .await?
        .ok_or_else(|| DbError::NotFound(format!("Repository not found: {}", id)))
}

// Get a repository by path
pub async fn get_repository_by_path(pool: &PgPool, path: &str) -> DbResult<Option<Repository>> {
    Ok(
        sqlx::query_as::<_, Repository>("SELECT * FROM repositories WHERE local_path = $1")
            .bind(path)
            .fetch_optional(pool)
            .await?,
    )
}

// List all repositories
pub async fn list_repositories(pool: &PgPool) -> DbResult<Vec<Repository>> {
    Ok(
        sqlx::query_as::<_, Repository>("SELECT * FROM repositories ORDER BY name ASC")
            .fetch_all(pool)
            .await?,
    )
}

// Update repository analysis timestamp and metadata
pub async fn update_repository_analysis(
    pool: &PgPool,
    id: &str,
    _metadata: Option<&str>,
) -> DbResult<()> {
    let now = chrono::Utc::now().timestamp();

    let result =
        sqlx::query("UPDATE repositories SET last_scanned_at = $1, updated_at = $2 WHERE id = $3")
            .bind(now)
            .bind(now)
            .bind(id)
            .execute(pool)
            .await?;

    if result.rows_affected() == 0 {
        return Err(DbError::NotFound(format!("Repository not found: {}", id)));
    }

    Ok(())
}

// Remove a repository
pub async fn remove_repository(pool: &PgPool, id: &str) -> DbResult<()> {
    let result = sqlx::query("DELETE FROM repositories WHERE id = $1")
        .bind(id)
        .execute(pool)
        .await?;

    if result.rows_affected() == 0 {
        return Err(DbError::NotFound(format!("Repository not found: {}", id)));
    }

    Ok(())
}

// ============================================================================
// Scan Progress Operations
// ============================================================================

// Start a scan - set status to 'scanning' and initialize progress tracking
pub async fn start_scan(pool: &PgPool, repo_id: &str, total_files: i64) -> DbResult<()> {
    let now = chrono::Utc::now().timestamp();

    let result = sqlx::query(
        r#"
        UPDATE repositories
        SET scan_status = 'scanning',
            scan_files_total = $1,
            scan_files_processed = 0,
            scan_progress = 'Starting scan...',
            scan_current_file = NULL,
            last_scanned_at = $2,
            last_error = NULL,
            updated_at = $3
        WHERE id = $4
        "#,
    )
    .bind(total_files)
    .bind(now)
    .bind(now)
    .bind(repo_id)
    .execute(pool)
    .await?;

    if result.rows_affected() == 0 {
        return Err(DbError::NotFound(format!(
            "Repository not found: {}",
            repo_id
        )));
    }

    // Log scan started event
    log_scan_event(
        pool,
        repo_id,
        "scan_started",
        &format!("Scan started with {} files to process", total_files),
        None,
    )
    .await?;

    Ok(())
}

// Update scan progress during scanning
pub async fn update_scan_progress(
    pool: &PgPool,
    repo_id: &str,
    files_processed: i64,
    current_file: Option<&str>,
) -> DbResult<()> {
    let now = chrono::Utc::now().timestamp();
    let progress_msg = if let Some(file) = current_file {
        format!("Processing file {}", file)
    } else {
        format!("Processed {} files", files_processed)
    };

    let result = sqlx::query(
        r#"
        UPDATE repositories
        SET scan_files_processed = $1,
            scan_current_file = $2,
            scan_progress = $3,
            updated_at = $4
        WHERE id = $5
        "#,
    )
    .bind(files_processed)
    .bind(current_file)
    .bind(&progress_msg)
    .bind(now)
    .bind(repo_id)
    .execute(pool)
    .await?;

    if result.rows_affected() == 0 {
        return Err(DbError::NotFound(format!(
            "Repository not found: {}",
            repo_id
        )));
    }

    Ok(())
}

// Complete a scan - set status to 'idle' and record metrics
pub async fn complete_scan(
    pool: &PgPool,
    repo_id: &str,
    duration_ms: i64,
    files_found: i64,
    issues_found: i64,
) -> DbResult<()> {
    let now = chrono::Utc::now().timestamp();

    let result = sqlx::query(
        r#"
        UPDATE repositories
        SET scan_status = 'idle',
            scan_progress = 'Scan complete',
            scan_current_file = NULL,
            last_scan_duration_ms = $1,
            last_scan_files_found = $2,
            last_scan_issues_found = $3,
            last_scanned_at = $4,
            updated_at = $5
        WHERE id = $6
        "#,
    )
    .bind(duration_ms)
    .bind(files_found)
    .bind(issues_found)
    .bind(now)
    .bind(now)
    .bind(repo_id)
    .execute(pool)
    .await?;

    if result.rows_affected() == 0 {
        return Err(DbError::NotFound(format!(
            "Repository not found: {}",
            repo_id
        )));
    }

    // Log scan completed event
    log_scan_event(
        pool,
        repo_id,
        "scan_completed",
        &format!(
            "Scan completed in {}ms: {} files, {} issues",
            duration_ms, files_found, issues_found
        ),
        Some(
            &serde_json::json!({
                "duration_ms": duration_ms,
                "files_found": files_found,
                "issues_found": issues_found
            })
            .to_string(),
        ),
    )
    .await?;

    Ok(())
}

// Mark a scan as failed with error
pub async fn fail_scan(pool: &PgPool, repo_id: &str, error_message: &str) -> DbResult<()> {
    let now = chrono::Utc::now().timestamp();

    let result = sqlx::query(
        r#"
        UPDATE repositories
        SET scan_status = 'error',
            scan_progress = 'Scan failed',
            scan_current_file = NULL,
            last_error = $1,
            updated_at = $2
        WHERE id = $3
        "#,
    )
    .bind(error_message)
    .bind(now)
    .bind(repo_id)
    .execute(pool)
    .await?;

    if result.rows_affected() == 0 {
        return Err(DbError::NotFound(format!(
            "Repository not found: {}",
            repo_id
        )));
    }

    // Log scan error event
    log_scan_event(pool, repo_id, "scan_error", error_message, None).await?;

    Ok(())
}

// ============================================================================
// Scan Event Logging
// ============================================================================

// Log a scan event to the scan_events table
pub async fn log_scan_event(
    pool: &PgPool,
    repo_id: &str,
    event_type: &str,
    message: &str,
    metadata: Option<&str>,
) -> DbResult<()> {
    let now = chrono::Utc::now().timestamp();

    sqlx::query(
        r#"
        INSERT INTO scan_events (repo_id, event_type, message, metadata, created_at)
        VALUES ($1, $2, $3, $4, $5)
        "#,
    )
    .bind(repo_id)
    .bind(event_type)
    .bind(message)
    .bind(metadata)
    .bind(now)
    .execute(pool)
    .await?;

    Ok(())
}

// Get recent scan events for a repository
pub async fn get_scan_events(
    pool: &PgPool,
    repo_id: Option<&str>,
    limit: i64,
) -> DbResult<Vec<ScanEvent>> {
    let events = if let Some(rid) = repo_id {
        sqlx::query_as::<_, ScanEvent>(
            r#"
            SELECT * FROM scan_events
            WHERE repo_id = $1
            ORDER BY created_at DESC
            LIMIT $2
            "#,
        )
        .bind(rid)
        .bind(limit)
        .fetch_all(pool)
        .await?
    } else {
        sqlx::query_as::<_, ScanEvent>(
            r#"
            SELECT * FROM scan_events
            ORDER BY created_at DESC
            LIMIT $1
            "#,
        )
        .bind(limit)
        .fetch_all(pool)
        .await?
    };

    Ok(events)
}

// Scan event model
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct ScanEvent {
    pub id: i64,
    pub repo_id: String,
    pub event_type: String,
    pub message: String,
    pub metadata: Option<String>,
    pub created_at: i64,
}

impl ScanEvent {
    // Get formatted created_at timestamp
    pub fn created_at_formatted(&self) -> String {
        chrono::DateTime::from_timestamp(self.created_at, 0)
            .map(|dt| dt.format("%Y-%m-%d %H:%M:%S").to_string())
            .unwrap_or_else(|| "unknown".to_string())
    }

    // Get a human-readable relative time (e.g., "2 minutes ago")
    pub fn created_at_relative(&self) -> String {
        let now = chrono::Utc::now().timestamp();
        let diff = now - self.created_at;

        if diff < 60 {
            "just now".to_string()
        } else if diff < 3600 {
            format!("{} minutes ago", diff / 60)
        } else if diff < 86400 {
            format!("{} hours ago", diff / 3600)
        } else {
            format!("{} days ago", diff / 86400)
        }
    }
}

// ============================================================================
// Task Operations
// ============================================================================

// Create a new task
#[allow(clippy::too_many_arguments)]
pub async fn create_task(
    pool: &PgPool,
    title: &str,
    description: Option<&str>,
    priority: i32,
    source: &str,
    source_id: Option<&str>,
    repo_id: Option<&str>,
    file_path: Option<&str>,
    line_number: Option<i32>,
) -> DbResult<Task> {
    let id = format!(
        "TASK-{}",
        &uuid::Uuid::new_v4().to_string()[..8].to_uppercase()
    );
    let now = chrono::Utc::now().timestamp();

    // `content` is NOT NULL in the schema (migration 001); `title` was added
    // later (migration 013). We store `title` in both columns so both old and
    // new query paths work without a schema change.
    sqlx::query(
        r#"
        INSERT INTO tasks (id, content, title, description, priority, status, source, source_id,
                          repo_id, file_path, line_number, created_at, updated_at)
        VALUES ($1, $2, $3, $4, $5, 'pending', $6, $7, $8, $9, $10, $11, $12)
        "#,
    )
    .bind(&id)
    .bind(title) // content (NOT NULL legacy column)
    .bind(title) // title  (migration-013 column)
    .bind(description)
    .bind(priority)
    .bind(source)
    .bind(source_id)
    .bind(repo_id)
    .bind(file_path)
    .bind(line_number)
    .bind(now)
    .bind(now)
    .execute(pool)
    .await?;

    Ok(Task {
        id,
        title: title.to_string(),
        description: description.map(|s| s.to_string()),
        priority,
        status: "pending".to_string(),
        source: source.to_string(),
        source_id: source_id.map(|s| s.to_string()),
        repo_id: repo_id.map(|s| s.to_string()),
        file_path: file_path.map(|s| s.to_string()),
        line_number,
        created_at: now,
        updated_at: now,
    })
}

// List tasks with optional filtering
pub async fn list_tasks(
    pool: &PgPool,
    limit: i64,
    status: Option<&str>,
    priority: Option<i32>,
    repo_id: Option<&str>,
) -> DbResult<Vec<Task>> {
    let mut query = String::from(
        "SELECT id, \
                COALESCE(title, 'Untitled') as title, \
                description, \
                priority, \
                status, \
                COALESCE(source, 'manual') as source, \
                source_id, \
                repo_id, \
                file_path, \
                line_number, \
                created_at, \
                updated_at \
         FROM tasks WHERE 1=1",
    );

    let mut param_idx: u32 = 1;

    if status.is_some() {
        query.push_str(&format!(" AND status = ${}", param_idx));
        param_idx += 1;
    }
    if priority.is_some() {
        query.push_str(&format!(" AND priority <= ${}", param_idx));
        param_idx += 1;
    }
    if repo_id.is_some() {
        query.push_str(&format!(" AND repo_id = ${}", param_idx));
        param_idx += 1;
    }

    query.push_str(&format!(
        " ORDER BY priority ASC, created_at DESC LIMIT ${}",
        param_idx
    ));

    let mut q = sqlx::query_as::<_, Task>(&query);

    if let Some(s) = status {
        q = q.bind(s);
    }
    if let Some(p) = priority {
        q = q.bind(p);
    }
    if let Some(r) = repo_id {
        q = q.bind(r);
    }
    q = q.bind(limit);

    Ok(q.fetch_all(pool).await?)
}

// Update task status
pub async fn update_task_status(pool: &PgPool, id: &str, status: &str) -> DbResult<()> {
    let now = chrono::Utc::now().timestamp();

    let result = sqlx::query("UPDATE tasks SET status = $1, updated_at = $2 WHERE id = $3")
        .bind(status)
        .bind(now)
        .bind(id)
        .execute(pool)
        .await?;

    if result.rows_affected() == 0 {
        return Err(DbError::NotFound(format!("Task not found: {}", id)));
    }

    Ok(())
}

// Get the next recommended task (highest priority pending task)
pub async fn get_next_task(pool: &PgPool) -> DbResult<Option<Task>> {
    Ok(sqlx::query_as::<_, Task>(
        r#"
        SELECT id,
               COALESCE(title, 'Untitled') as title,
               description,
               priority,
               status,
               COALESCE(source, 'manual') as source,
               source_id,
               repo_id,
               file_path,
               line_number,
               created_at,
               updated_at
        FROM tasks
        WHERE status = 'pending'
        ORDER BY priority ASC, created_at ASC
        LIMIT 1
        "#,
    )
    .fetch_optional(pool)
    .await?)
}

// ============================================================================
// Statistics
// ============================================================================

// Get database statistics
#[derive(Debug, Serialize)]
pub struct DbStats {
    pub total_notes: i64,
    pub inbox_notes: i64,
    pub total_repos: i64,
    pub total_tasks: i64,
    pub pending_tasks: i64,
}

pub async fn get_stats(pool: &PgPool) -> DbResult<DbStats> {
    let total_notes: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM notes")
        .fetch_one(pool)
        .await?;

    let inbox_notes: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM notes WHERE status = 'inbox'")
        .fetch_one(pool)
        .await?;

    let total_repos: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM repositories")
        .fetch_one(pool)
        .await?;

    let total_tasks: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM tasks")
        .fetch_one(pool)
        .await?;

    let pending_tasks: (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM tasks WHERE status = 'pending'")
            .fetch_one(pool)
            .await?;

    Ok(DbStats {
        total_notes: total_notes.0,
        inbox_notes: inbox_notes.0,
        total_repos: total_repos.0,
        total_tasks: total_tasks.0,
        pending_tasks: pending_tasks.0,
    })
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    async fn setup_test_db() -> PgPool {
        let url = std::env::var("DATABASE_URL").unwrap_or_else(|_| {
            "postgresql://rustcode:changeme@localhost:5432/rustcode_test".to_string()
        });
        init_db(&url).await.unwrap()
    }

    // Ensure a tag exists in the `tags` table so FK inserts into `note_tags` succeed.
    async fn ensure_tag(pool: &PgPool, name: &str) {
        let now = chrono::Utc::now().timestamp();
        sqlx::query(
            "INSERT INTO tags (name, created_at, updated_at) VALUES ($1, $2, $3) ON CONFLICT DO NOTHING",
        )
        .bind(name)
        .bind(now)
        .bind(now)
        .execute(pool)
        .await
        .unwrap();
    }

    // Generate a short unique suffix so parallel tests don't collide on UNIQUE columns.
    fn uid() -> String {
        uuid::Uuid::new_v4().to_string()[..8].to_string()
    }

    #[tokio::test]
    async fn test_create_and_get_note() {
        let pool = setup_test_db().await;

        // Pre-create the tags that the FK constraint requires.
        ensure_tag(&pool, "testtag1").await;
        ensure_tag(&pool, "testtag2").await;

        let content = format!("Test note content {}", uid());
        let note = create_note(&pool, &content, Some("testtag1,testtag2"), None)
            .await
            .unwrap();

        assert_eq!(note.content, content);
        // tags are populated into the returned Note by create_note_with_tags
        assert_eq!(note.tags, Some("testtag1,testtag2".to_string()));
        assert_eq!(note.status, "active");

        let fetched = get_note(&pool, &note.id).await.unwrap();
        assert_eq!(fetched.id, note.id);
        assert_eq!(fetched.content, note.content);
    }

    #[tokio::test]
    async fn test_list_notes() {
        let pool = setup_test_db().await;

        // Use a rare tag name so the tag-filter assertion isn't confused by
        // notes inserted by other concurrently running tests.
        let unique_tag = format!("listtag-{}", uid());
        ensure_tag(&pool, &unique_tag).await;

        let s = uid();
        create_note(&pool, &format!("List note A {}", s), None, None)
            .await
            .unwrap();
        create_note(
            &pool,
            &format!("List note B {}", s),
            Some(&unique_tag),
            None,
        )
        .await
        .unwrap();
        create_note(&pool, &format!("List note C {}", s), None, None)
            .await
            .unwrap();

        // We inserted ≥3 notes; the shared DB may have more from other tests.
        let all_notes = list_notes(&pool, 100, None, None, None).await.unwrap();
        assert!(all_notes.len() >= 3);

        // Filter by the unique tag: exactly one note should match.
        let tagged = list_notes(&pool, 100, None, None, Some(&unique_tag))
            .await
            .unwrap();
        assert_eq!(tagged.len(), 1);
    }

    #[tokio::test]
    async fn test_search_notes() {
        let pool = setup_test_db().await;

        // Use a unique search token so the ILIKE filter is unambiguous.
        let token = format!("UniqueRustToken{}", uid());
        let content_rust = format!("{} programming tips", token);
        let content_py = format!("PythonBasics{}", uid());

        create_note(&pool, &content_rust, None, None).await.unwrap();
        create_note(&pool, &content_py, None, None).await.unwrap();

        let results = search_notes(&pool, &token, 10).await.unwrap();
        assert_eq!(results.len(), 1);
        assert!(results[0].content.contains(&token));
    }

    #[tokio::test]
    async fn test_repository_crud() {
        let pool = setup_test_db().await;

        // Use unique path + name so the UNIQUE(name) constraint is not violated
        // when tests run in parallel or are re-run without truncating the DB.
        let id_suffix = uid();
        let name = format!("test-repo-{}", id_suffix);
        let path = format!("/tmp/test-repo-{}", id_suffix);

        let repo = add_repository(&pool, &path, &name, None).await.unwrap();

        assert_eq!(repo.name, name);
        assert_eq!(repo.path, path);

        // After inserting, at least this repo exists.
        let repos = list_repositories(&pool).await.unwrap();
        assert!(repos.iter().any(|r| r.id == repo.id));

        remove_repository(&pool, &repo.id).await.unwrap();

        let repos_after = list_repositories(&pool).await.unwrap();
        assert!(!repos_after.iter().any(|r| r.id == repo.id));
    }

    #[tokio::test]
    async fn test_task_creation_and_next() {
        let pool = setup_test_db().await;

        // Use a unique title prefix so get_next_task result is unambiguous
        // even if other tests have inserted tasks with priority 1.
        let pfx = format!("CriticalUnique-{}", uid());

        create_task(
            &pool,
            &format!("{}-low", pfx),
            None,
            4,
            "manual",
            None,
            None,
            None,
            None,
        )
        .await
        .unwrap();
        create_task(
            &pool,
            &format!("{}-high", pfx),
            None,
            2,
            "manual",
            None,
            None,
            None,
            None,
        )
        .await
        .unwrap();
        let critical = create_task(
            &pool,
            &format!("{}-critical", pfx),
            None,
            1,
            "manual",
            None,
            None,
            None,
            None,
        )
        .await
        .unwrap();

        // get_next_task returns the globally highest-priority pending task.
        // Because priority 1 is the highest and we just inserted one,
        // the returned task must have priority 1.
        let next = get_next_task(&pool).await.unwrap().unwrap();
        assert_eq!(next.priority, 1);
        // Our critical task must be present in the DB.
        let all = list_tasks(&pool, 200, Some("pending"), Some(1), None)
            .await
            .unwrap();
        assert!(all.iter().any(|t| t.id == critical.id));
    }

    #[tokio::test]
    async fn test_stats() {
        let pool = setup_test_db().await;

        let s = uid();
        create_note(&pool, &format!("Stats note 1 {}", s), None, None)
            .await
            .unwrap();
        create_note(&pool, &format!("Stats note 2 {}", s), None, None)
            .await
            .unwrap();

        let repo_name = format!("stats-repo-{}", s);
        let repo_path = format!("/tmp/stats-repo-{}", s);
        add_repository(&pool, &repo_path, &repo_name, None)
            .await
            .unwrap();

        create_task(
            &pool,
            &format!("Stats task {}", s),
            None,
            2,
            "manual",
            None,
            None,
            None,
            None,
        )
        .await
        .unwrap();

        let stats = get_stats(&pool).await.unwrap();
        assert!(stats.total_notes >= 2);
        assert!(stats.total_repos >= 1);
        assert!(stats.total_tasks >= 1);
        assert!(stats.pending_tasks >= 1);
    }
}

// ============================================================================
// Backward Compatibility Layer
// ============================================================================
// This Database struct provides compatibility with existing code that uses
// the old struct-based API. New code should use the function-based API above.

// Backward-compatible Database wrapper
#[derive(Clone)]
pub struct Database {
    pub pool: PgPool,
}

impl Database {
    // Create a new database connection (legacy API)
    pub async fn new(database_url: &str) -> DbResult<Self> {
        let pool = init_db(database_url).await?;
        Ok(Self { pool })
    }

    // Create a Database from an existing PgPool
    pub fn from_pool(pool: PgPool) -> Self {
        Self { pool }
    }

    // Get a reference to the pool
    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    // Create a note (legacy API)
    pub async fn create_note(&self, content: &str, status: NoteStatus) -> DbResult<String> {
        let note = create_note(&self.pool, content, None, None).await?;
        if status.as_str() != "inbox" {
            update_note_status(&self.pool, &note.id, status.as_str()).await?;
        }
        Ok(note.id)
    }

    // Get a note by ID (legacy API)
    pub async fn get_note(&self, id: &str) -> DbResult<Note> {
        get_note(&self.pool, id).await
    }

    // List notes (legacy API)
    pub async fn list_notes(
        &self,
        status: Option<NoteStatus>,
        limit: Option<i64>,
        _offset: Option<i64>,
    ) -> DbResult<Vec<Note>> {
        let limit = limit.unwrap_or(50);
        let status_str = status.map(|s| s.as_str());
        list_notes(&self.pool, limit, status_str, None, None).await
    }

    // Add a repository (legacy API)
    pub async fn add_repository(
        &self,
        name: &str,
        path: &str,
        remote_url: Option<String>,
        _default_branch: Option<String>,
    ) -> DbResult<String> {
        let repo = add_repository(&self.pool, path, name, remote_url.as_deref()).await?;
        Ok(repo.id)
    }

    // Get a repository by ID (legacy API)
    pub async fn get_repository(&self, id: &str) -> DbResult<Repository> {
        get_repository(&self.pool, id).await
    }

    // List repositories (legacy API)
    pub async fn list_repositories(&self) -> DbResult<Vec<Repository>> {
        list_repositories(&self.pool).await
    }

    // Record LLM cost (legacy API - now a no-op, consider removing calls)
    pub async fn record_llm_cost(
        &self,
        _model: &str,
        _operation: &str,
        _prompt_tokens: i64,
        _completion_tokens: i64,
        _estimated_cost_usd: f64,
        _repository_id: Option<i64>,
    ) -> DbResult<()> {
        // Legacy API - no longer storing LLM costs in new schema
        // Keep as no-op for compatibility
        Ok(())
    }

    // Get total LLM cost (legacy API - returns 0.0)
    pub async fn get_total_llm_cost(&self) -> DbResult<f64> {
        Ok(0.0)
    }

    // Get LLM cost by period (legacy API - returns 0.0)
    pub async fn get_llm_cost_by_period(&self, _hours: i64) -> DbResult<f64> {
        Ok(0.0)
    }

    // Get cache hit rate from llm_costs table (last 30 days)
    //
    // Returns the percentage of queries that were cache hits (0-100).
    // Returns 0 if no data or if the llm_costs table doesn't exist.
    pub async fn get_cache_hit_rate(&self) -> DbResult<i64> {
        // Query cache hit stats from llm_costs table (created by CostTracker)
        let result = sqlx::query_as::<_, (i64, i64)>(
            r#"
            SELECT
                COUNT(*) as total,
                COALESCE(SUM(CASE WHEN cache_hit = TRUE THEN 1 ELSE 0 END), 0) as hits
            FROM llm_costs
            WHERE timestamp >= datetime('now', '-30 days')
            "#,
        )
        .fetch_optional(&self.pool)
        .await;

        match result {
            Ok(Some((total, hits))) if total > 0 => {
                Ok(((hits as f64 / total as f64) * 100.0) as i64)
            }
            _ => Ok(0), // No data or table doesn't exist
        }
    }

    // Get cost by model (legacy API - returns empty map)
    pub async fn get_cost_by_model(&self) -> DbResult<std::collections::HashMap<String, f64>> {
        Ok(std::collections::HashMap::new())
    }

    // Count notes (legacy API)
    pub async fn count_notes(&self) -> DbResult<i64> {
        let stats = get_stats(&self.pool).await?;
        Ok(stats.total_notes)
    }

    // Count repositories (legacy API)
    pub async fn count_repositories(&self) -> DbResult<i64> {
        let stats = get_stats(&self.pool).await?;
        Ok(stats.total_repos)
    }

    // Get recent LLM operations (legacy API - returns empty vec)
    pub async fn get_recent_llm_operations(&self, _limit: i64) -> DbResult<Vec<LlmCost>> {
        Ok(Vec::new())
    }

    // Get stats (legacy API)
    pub async fn get_stats(&self) -> DbResult<DatabaseStats> {
        let stats = get_stats(&self.pool).await?;
        Ok(DatabaseStats {
            total_notes: stats.total_notes,
            inbox_notes: stats.inbox_notes,
            total_tags: 0, // Not tracked in new schema
            total_repositories: stats.total_repos,
        })
    }
}

// Legacy note status enum
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoteStatus {
    Inbox,
    Active,
    Processed,
    Archived,
}

impl NoteStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            NoteStatus::Inbox => "inbox",
            NoteStatus::Active => "active",
            NoteStatus::Processed => "processed",
            NoteStatus::Archived => "archived",
        }
    }
}

// Legacy stats struct
#[derive(Debug, Clone)]
pub struct DatabaseStats {
    pub total_notes: i64,
    pub inbox_notes: i64,
    pub total_tags: i64,
    pub total_repositories: i64,
}

// Legacy LlmCost struct (kept for compatibility)
#[derive(Debug, Clone)]
pub struct LlmCost {
    pub id: String,
    pub model: String,
    pub operation: String,
    pub prompt_tokens: i32,
    pub completion_tokens: i32,
    pub total_tokens: i32,
    pub estimated_cost_usd: f64,
    pub repository_id: Option<String>,
    pub created_at: i64,
}

impl LlmCost {
    // Get formatted created_at timestamp (legacy API)
    pub fn created_at_formatted(&self) -> String {
        chrono::DateTime::from_timestamp(self.created_at, 0)
            .map(|dt| dt.format("%Y-%m-%d %H:%M:%S").to_string())
            .unwrap_or_else(|| "unknown".to_string())
    }
}

// Legacy LlmCostStats struct (kept for compatibility)
#[derive(Debug, Clone)]
pub struct LlmCostStats {
    pub total_cost: f64,
    pub cost_last_24h: f64,
    pub cost_last_7d: f64,
    pub cost_last_30d: f64,
    pub by_model: std::collections::HashMap<String, f64>,
}
