//! Document database operations for RAG system
//!
//! Provides CRUD operations for documents, chunks, and embeddings.
//! All queries use Postgres syntax ($1, $2, ... placeholders).

use sqlx::{PgPool, Row};
use uuid::Uuid;

use super::{DbError, DbResult, Document, DocumentChunk, DocumentEmbedding};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;

// ============================================================================
// Document CRUD Operations
// ============================================================================

/// Create a new document
#[allow(clippy::too_many_arguments)]
pub async fn create_document(
    pool: &PgPool,
    title: String,
    content: String,
    content_type: String,
    source_type: String,
    doc_type: String,
    repo_id: Option<String>,
    tags: Option<Vec<String>>,
) -> DbResult<Document> {
    let id = Uuid::new_v4().to_string();
    let now = chrono::Utc::now().timestamp();

    let word_count = content.split_whitespace().count() as i64;
    let char_count = content.chars().count() as i64;

    let tags_str = tags.as_ref().map(|t| t.join(","));

    sqlx::query(
        "INSERT INTO documents
        (id, title, content, content_type, source_type, doc_type, tags, repo_id, word_count, char_count, created_at, updated_at)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)",
    )
    .bind(&id)
    .bind(&title)
    .bind(&content)
    .bind(&content_type)
    .bind(&source_type)
    .bind(&doc_type)
    .bind(&tags_str)
    .bind(&repo_id)
    .bind(word_count)
    .bind(char_count)
    .bind(now)
    .bind(now)
    .execute(pool)
    .await
    .map_err(DbError::Sqlx)?;

    // If tags provided, insert into document_tags
    if let Some(tag_list) = tags {
        for tag in tag_list {
            // Ensure tag exists in tags table
            let _ = super::upsert_tag(pool, &tag, None, None).await;

            // Link to document
            let _ = sqlx::query(
                "INSERT INTO document_tags (document_id, tag, created_at)
                 VALUES ($1, $2, $3)
                 ON CONFLICT DO NOTHING",
            )
            .bind(&id)
            .bind(&tag)
            .bind(now)
            .execute(pool)
            .await;
        }
    }

    get_document(pool, &id).await
}

/// Get a document by ID
pub async fn get_document(pool: &PgPool, id: &str) -> DbResult<Document> {
    let row = sqlx::query(
        "SELECT id, title, content, content_type, source_type, source_url, doc_type, tags,
                repo_id, file_path, word_count, char_count, created_at, updated_at, indexed_at,
                COALESCE(pinned, FALSE) AS pinned
         FROM documents WHERE id = $1",
    )
    .bind(id)
    .fetch_one(pool)
    .await
    .map_err(|e| match e {
        sqlx::Error::RowNotFound => DbError::NotFound(format!("Document {} not found", id)),
        e => DbError::Sqlx(e),
    })?;

    Ok(Document {
        id: row.get::<Option<String>, _>("id").unwrap_or_default(),
        title: row.get("title"),
        content: row.get("content"),
        content_type: row
            .get::<Option<String>, _>("content_type")
            .unwrap_or_else(|| "markdown".to_string()),
        source_type: row
            .get::<Option<String>, _>("source_type")
            .unwrap_or_else(|| "manual".to_string()),
        source_url: row.get("source_url"),
        doc_type: row
            .get::<Option<String>, _>("doc_type")
            .unwrap_or_else(|| "reference".to_string()),
        tags: row.get("tags"),
        repo_id: row.get("repo_id"),
        file_path: row.get("file_path"),
        word_count: row.get::<Option<i64>, _>("word_count").unwrap_or(0),
        char_count: row.get::<Option<i64>, _>("char_count").unwrap_or(0),
        created_at: row.get("created_at"),
        updated_at: row.get("updated_at"),
        indexed_at: row.get("indexed_at"),
        pinned: row.get::<Option<bool>, _>("pinned").unwrap_or(false),
    })
}

/// Update a document's title, content, and doc_type
pub async fn update_document(
    pool: &PgPool,
    id: &str,
    title: Option<String>,
    content: Option<String>,
    doc_type: Option<String>,
    tags: Option<String>,
) -> DbResult<Document> {
    let now = chrono::Utc::now().timestamp();

    // Fetch current values to fill in unchanged fields
    let current = get_document(pool, id).await?;

    let new_title = title.unwrap_or(current.title);
    let new_content = content.unwrap_or(current.content);
    let new_doc_type = doc_type.unwrap_or(current.doc_type);
    let new_tags = tags.or(current.tags);

    let word_count = new_content.split_whitespace().count() as i64;
    let char_count = new_content.chars().count() as i64;

    sqlx::query(
        "UPDATE documents
         SET title = $1, content = $2, doc_type = $3, tags = $4,
             word_count = $5, char_count = $6, updated_at = $7
         WHERE id = $8",
    )
    .bind(&new_title)
    .bind(&new_content)
    .bind(&new_doc_type)
    .bind(&new_tags)
    .bind(word_count)
    .bind(char_count)
    .bind(now)
    .bind(id)
    .execute(pool)
    .await
    .map_err(DbError::Sqlx)?;

    get_document(pool, id).await
}

/// Delete a document by ID
pub async fn delete_document(pool: &PgPool, id: &str) -> DbResult<()> {
    sqlx::query("DELETE FROM documents WHERE id = $1")
        .bind(id)
        .execute(pool)
        .await
        .map_err(DbError::Sqlx)?;
    Ok(())
}

/// Delete all chunks for a document
pub async fn delete_document_chunks(pool: &PgPool, document_id: &str) -> DbResult<()> {
    sqlx::query("DELETE FROM document_chunks WHERE document_id = $1")
        .bind(document_id)
        .execute(pool)
        .await
        .map_err(DbError::Sqlx)?;
    Ok(())
}

/// Delete all embeddings for a document (via its chunks)
pub async fn delete_document_embeddings(pool: &PgPool, document_id: &str) -> DbResult<()> {
    sqlx::query(
        "DELETE FROM document_embeddings
         WHERE chunk_id IN (
             SELECT id FROM document_chunks WHERE document_id = $1
         )",
    )
    .bind(document_id)
    .execute(pool)
    .await
    .map_err(DbError::Sqlx)?;
    Ok(())
}

/// List documents with optional filters
pub async fn list_documents(
    pool: &PgPool,
    doc_type: Option<String>,
    repo_id: Option<String>,
    limit: Option<i64>,
    offset: Option<i64>,
) -> DbResult<Vec<Document>> {
    let limit = limit.unwrap_or(50);
    let offset = offset.unwrap_or(0);

    // Build a dynamic query. sqlx doesn't support truly dynamic queries with macros,
    // so we use query() and bind manually.
    let mut conditions: Vec<String> = Vec::new();
    let mut param_idx = 1usize;

    if doc_type.is_some() {
        conditions.push(format!("doc_type = ${}", param_idx));
        param_idx += 1;
    }
    if repo_id.is_some() {
        conditions.push(format!("repo_id = ${}", param_idx));
        param_idx += 1;
    }

    let where_clause = if conditions.is_empty() {
        String::new()
    } else {
        format!("WHERE {}", conditions.join(" AND "))
    };

    let sql = format!(
        "SELECT id, title, content, content_type, source_type, source_url, doc_type, tags,
                repo_id, file_path, word_count, char_count, created_at, updated_at, indexed_at,
                COALESCE(pinned, FALSE) AS pinned
         FROM documents
         {}
         ORDER BY pinned DESC, created_at DESC
         LIMIT ${} OFFSET ${}",
        where_clause,
        param_idx,
        param_idx + 1
    );

    let mut q = sqlx::query(&sql);

    if let Some(ref dt) = doc_type {
        q = q.bind(dt);
    }
    if let Some(ref rid) = repo_id {
        q = q.bind(rid);
    }
    q = q.bind(limit).bind(offset);

    let rows = q.fetch_all(pool).await.map_err(DbError::Sqlx)?;

    Ok(rows
        .into_iter()
        .map(|row| Document {
            id: row.get::<Option<String>, _>("id").unwrap_or_default(),
            title: row.get("title"),
            content: row.get("content"),
            content_type: row
                .get::<Option<String>, _>("content_type")
                .unwrap_or_else(|| "markdown".to_string()),
            source_type: row
                .get::<Option<String>, _>("source_type")
                .unwrap_or_else(|| "manual".to_string()),
            source_url: row.get("source_url"),
            doc_type: row
                .get::<Option<String>, _>("doc_type")
                .unwrap_or_else(|| "reference".to_string()),
            tags: row.get("tags"),
            repo_id: row.get("repo_id"),
            file_path: row.get("file_path"),
            word_count: row.get::<Option<i64>, _>("word_count").unwrap_or(0),
            char_count: row.get::<Option<i64>, _>("char_count").unwrap_or(0),
            created_at: row.get("created_at"),
            updated_at: row.get("updated_at"),
            indexed_at: row.get("indexed_at"),
            pinned: row.get::<Option<bool>, _>("pinned").unwrap_or(false),
        })
        .collect())
}

/// Count total documents
pub async fn count_documents(pool: &PgPool) -> DbResult<i64> {
    let count = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM documents")
        .fetch_one(pool)
        .await
        .map_err(DbError::Sqlx)?;
    Ok(count)
}

/// Count documents by type
pub async fn count_documents_by_type(pool: &PgPool, doc_type: &str) -> DbResult<i64> {
    let count = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM documents WHERE doc_type = $1")
        .bind(doc_type)
        .fetch_one(pool)
        .await
        .map_err(DbError::Sqlx)?;
    Ok(count)
}

/// Get documents that have not yet been indexed (or need re-indexing)
pub async fn get_unindexed_documents(pool: &PgPool, limit: i64) -> DbResult<Vec<Document>> {
    let rows = sqlx::query(
        "SELECT id, title, content, content_type, source_type, source_url, doc_type, tags,
                repo_id, file_path, word_count, char_count, created_at, updated_at, indexed_at,
                COALESCE(pinned, FALSE) AS pinned
         FROM documents
         WHERE indexed_at IS NULL OR updated_at > indexed_at
         ORDER BY updated_at DESC
         LIMIT $1",
    )
    .bind(limit)
    .fetch_all(pool)
    .await
    .map_err(DbError::Sqlx)?;

    Ok(rows
        .into_iter()
        .map(|row| Document {
            id: row.get::<Option<String>, _>("id").unwrap_or_default(),
            title: row.get("title"),
            content: row.get("content"),
            content_type: row
                .get::<Option<String>, _>("content_type")
                .unwrap_or_else(|| "markdown".to_string()),
            source_type: row
                .get::<Option<String>, _>("source_type")
                .unwrap_or_else(|| "manual".to_string()),
            source_url: row.get("source_url"),
            doc_type: row
                .get::<Option<String>, _>("doc_type")
                .unwrap_or_else(|| "reference".to_string()),
            tags: row.get("tags"),
            repo_id: row.get("repo_id"),
            file_path: row.get("file_path"),
            word_count: row.get::<Option<i64>, _>("word_count").unwrap_or(0),
            char_count: row.get::<Option<i64>, _>("char_count").unwrap_or(0),
            created_at: row.get("created_at"),
            updated_at: row.get("updated_at"),
            indexed_at: row.get("indexed_at"),
            pinned: row.get::<Option<bool>, _>("pinned").unwrap_or(false),
        })
        .collect())
}

/// Toggle the `pinned` state of a document.
///
/// Returns the new pinned value (`true` = now pinned, `false` = now unpinned).
pub async fn set_document_pinned(pool: &PgPool, id: &str, pinned: bool) -> DbResult<bool> {
    let rows_affected =
        sqlx::query("UPDATE documents SET pinned = $1, updated_at = $2 WHERE id = $3")
            .bind(pinned)
            .bind(chrono::Utc::now().timestamp())
            .bind(id)
            .execute(pool)
            .await
            .map_err(DbError::Sqlx)?
            .rows_affected();

    if rows_affected == 0 {
        return Err(DbError::NotFound(format!("Document {} not found", id)));
    }
    Ok(pinned)
}

/// Mark a document as indexed at the current time
pub async fn mark_document_indexed(pool: &PgPool, document_id: &str) -> DbResult<()> {
    let now = chrono::Utc::now().timestamp();
    sqlx::query("UPDATE documents SET indexed_at = $1 WHERE id = $2")
        .bind(now)
        .bind(document_id)
        .execute(pool)
        .await
        .map_err(DbError::Sqlx)?;
    Ok(())
}

/// Get tags for a document
pub async fn get_document_tags(pool: &PgPool, document_id: &str) -> DbResult<Vec<String>> {
    let rows = sqlx::query("SELECT tag FROM document_tags WHERE document_id = $1 ORDER BY tag")
        .bind(document_id)
        .fetch_all(pool)
        .await
        .map_err(DbError::Sqlx)?;

    Ok(rows
        .into_iter()
        .map(|r| r.get::<String, _>("tag"))
        .collect())
}

/// Search documents by title (case-insensitive substring)
pub async fn search_documents_by_title(
    pool: &PgPool,
    query: &str,
    limit: Option<i64>,
) -> DbResult<Vec<Document>> {
    let limit = limit.unwrap_or(50);
    let pattern = format!("%{}%", query);

    let rows = sqlx::query(
        "SELECT id, title, content, content_type, source_type, source_url, doc_type, tags,
                repo_id, file_path, word_count, char_count, created_at, updated_at, indexed_at,
                COALESCE(pinned, FALSE) AS pinned
         FROM documents
         WHERE title ILIKE $1
         ORDER BY created_at DESC
         LIMIT $2",
    )
    .bind(&pattern)
    .bind(limit)
    .fetch_all(pool)
    .await
    .map_err(DbError::Sqlx)?;

    Ok(rows
        .into_iter()
        .map(|row| Document {
            id: row.get::<Option<String>, _>("id").unwrap_or_default(),
            title: row.get("title"),
            content: row.get("content"),
            content_type: row
                .get::<Option<String>, _>("content_type")
                .unwrap_or_else(|| "markdown".to_string()),
            source_type: row
                .get::<Option<String>, _>("source_type")
                .unwrap_or_else(|| "manual".to_string()),
            source_url: row.get("source_url"),
            doc_type: row
                .get::<Option<String>, _>("doc_type")
                .unwrap_or_else(|| "reference".to_string()),
            tags: row.get("tags"),
            repo_id: row.get("repo_id"),
            file_path: row.get("file_path"),
            word_count: row.get::<Option<i64>, _>("word_count").unwrap_or(0),
            char_count: row.get::<Option<i64>, _>("char_count").unwrap_or(0),
            created_at: row.get("created_at"),
            updated_at: row.get("updated_at"),
            indexed_at: row.get("indexed_at"),
            pinned: row.get::<Option<bool>, _>("pinned").unwrap_or(false),
        })
        .collect())
}

/// Search documents by tag
pub async fn search_documents_by_tags(
    pool: &PgPool,
    tag: &str,
    limit: Option<i64>,
) -> DbResult<Vec<Document>> {
    let limit = limit.unwrap_or(50);

    let rows = sqlx::query(
        "SELECT DISTINCT d.id, d.title, d.content, d.content_type, d.source_type, d.source_url,
                d.doc_type, d.tags, d.repo_id, d.file_path, d.word_count, d.char_count,
                d.created_at, d.updated_at, d.indexed_at,
                COALESCE(d.pinned, FALSE) AS pinned
         FROM documents d
         JOIN document_tags dt ON d.id = dt.document_id
         WHERE dt.tag = $1
         ORDER BY d.created_at DESC
         LIMIT $2",
    )
    .bind(tag)
    .bind(limit)
    .fetch_all(pool)
    .await
    .map_err(DbError::Sqlx)?;

    Ok(rows
        .into_iter()
        .map(|row| Document {
            id: row.get::<Option<String>, _>("id").unwrap_or_default(),
            title: row.get("title"),
            content: row.get("content"),
            content_type: row
                .get::<Option<String>, _>("content_type")
                .unwrap_or_else(|| "markdown".to_string()),
            source_type: row
                .get::<Option<String>, _>("source_type")
                .unwrap_or_else(|| "manual".to_string()),
            source_url: row.get("source_url"),
            doc_type: row
                .get::<Option<String>, _>("doc_type")
                .unwrap_or_else(|| "reference".to_string()),
            tags: row.get("tags"),
            repo_id: row.get("repo_id"),
            file_path: row.get("file_path"),
            word_count: row.get::<Option<i64>, _>("word_count").unwrap_or(0),
            char_count: row.get::<Option<i64>, _>("char_count").unwrap_or(0),
            created_at: row.get("created_at"),
            updated_at: row.get("updated_at"),
            indexed_at: row.get("indexed_at"),
            pinned: row.get::<Option<bool>, _>("pinned").unwrap_or(false),
        })
        .collect())
}

// ============================================================================
// Document Chunk Operations
// ============================================================================

/// Create document chunks
pub async fn create_chunks(
    pool: &PgPool,
    document_id: String,
    chunks: Vec<(String, i64, i64, Option<String>)>, // (content, char_start, char_end, heading)
) -> DbResult<Vec<DocumentChunk>> {
    let now = chrono::Utc::now().timestamp();
    let mut created_chunks = Vec::new();

    for (index, (content, char_start, char_end, heading)) in chunks.into_iter().enumerate() {
        let id = Uuid::new_v4().to_string();
        let word_count = content.split_whitespace().count() as i64;

        sqlx::query(
            "INSERT INTO document_chunks
             (id, document_id, chunk_index, content, char_start, char_end, word_count, heading, created_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
        )
        .bind(&id)
        .bind(&document_id)
        .bind(index as i64)
        .bind(&content)
        .bind(char_start)
        .bind(char_end)
        .bind(word_count)
        .bind(&heading)
        .bind(now)
        .execute(pool)
        .await
        .map_err(DbError::Sqlx)?;

        created_chunks.push(DocumentChunk {
            id,
            document_id: document_id.clone(),
            chunk_index: index as i64,
            content,
            char_start,
            char_end,
            word_count,
            heading,
            created_at: now,
        });
    }

    Ok(created_chunks)
}

/// Get chunks for a document
pub async fn get_document_chunks(pool: &PgPool, document_id: &str) -> DbResult<Vec<DocumentChunk>> {
    let rows = sqlx::query(
        "SELECT id, document_id, chunk_index, content, char_start, char_end, word_count, heading, created_at
         FROM document_chunks
         WHERE document_id = $1
         ORDER BY chunk_index ASC",
    )
    .bind(document_id)
    .fetch_all(pool)
    .await
    .map_err(DbError::Sqlx)?;

    Ok(rows
        .into_iter()
        .map(|row| DocumentChunk {
            id: row.get("id"),
            document_id: row.get("document_id"),
            chunk_index: row.get("chunk_index"),
            content: row.get("content"),
            char_start: row.get("char_start"),
            char_end: row.get("char_end"),
            word_count: row.get::<Option<i64>, _>("word_count").unwrap_or(0),
            heading: row.get("heading"),
            created_at: row.get("created_at"),
        })
        .collect())
}

// ============================================================================
// Embedding Operations
// ============================================================================

/// Store an embedding for a chunk
pub async fn store_embedding(
    pool: &PgPool,
    chunk_id: String,
    vector: Vec<f32>,
    model: String,
) -> DbResult<DocumentEmbedding> {
    let id = Uuid::new_v4().to_string();
    let now = chrono::Utc::now().timestamp();
    let dimension = vector.len() as i64;
    let embedding_json = serde_json::to_string(&vector)
        .map_err(|e| DbError::InvalidInput(format!("Failed to serialize embedding: {}", e)))?;

    sqlx::query(
        "INSERT INTO document_embeddings (id, chunk_id, embedding, model, dimension, created_at)
         VALUES ($1, $2, $3, $4, $5, $6)
         ON CONFLICT (chunk_id) DO UPDATE
         SET embedding = EXCLUDED.embedding, model = EXCLUDED.model,
             dimension = EXCLUDED.dimension, created_at = EXCLUDED.created_at",
    )
    .bind(&id)
    .bind(&chunk_id)
    .bind(&embedding_json)
    .bind(&model)
    .bind(dimension)
    .bind(now)
    .execute(pool)
    .await
    .map_err(DbError::Sqlx)?;

    Ok(DocumentEmbedding {
        id,
        chunk_id,
        embedding: embedding_json,
        model,
        dimension,
        created_at: now,
    })
}

/// Get embeddings for a document's chunks
pub async fn get_document_embeddings(
    pool: &PgPool,
    document_id: &str,
) -> DbResult<Vec<DocumentEmbedding>> {
    let rows = sqlx::query(
        "SELECT de.id, de.chunk_id, de.embedding, de.model, de.dimension, de.created_at
         FROM document_embeddings de
         JOIN document_chunks dc ON de.chunk_id = dc.id
         WHERE dc.document_id = $1
         ORDER BY dc.chunk_index ASC",
    )
    .bind(document_id)
    .fetch_all(pool)
    .await
    .map_err(DbError::Sqlx)?;

    Ok(rows
        .into_iter()
        .map(|row| DocumentEmbedding {
            id: row.get("id"),
            chunk_id: row.get("chunk_id"),
            embedding: row.get("embedding"),
            model: row.get("model"),
            dimension: row.get::<Option<i64>, _>("dimension").unwrap_or(0),
            created_at: row.get("created_at"),
        })
        .collect())
}

/// Get all embeddings (for building the HNSW index on startup)
pub async fn get_all_embeddings(pool: &PgPool) -> DbResult<Vec<DocumentEmbedding>> {
    let rows = sqlx::query(
        "SELECT id, chunk_id, embedding, model, dimension, created_at
         FROM document_embeddings
         ORDER BY created_at ASC",
    )
    .fetch_all(pool)
    .await
    .map_err(DbError::Sqlx)?;

    Ok(rows
        .into_iter()
        .map(|row| DocumentEmbedding {
            id: row.get("id"),
            chunk_id: row.get("chunk_id"),
            embedding: row.get("embedding"),
            model: row.get("model"),
            dimension: row.get::<Option<i64>, _>("dimension").unwrap_or(0),
            created_at: row.get("created_at"),
        })
        .collect())
}

// ============================================================================
// Ideas — Quick thought capture with tagging
// ============================================================================

/// Idea model matching the database schema
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct Idea {
    pub id: String,
    pub content: String,
    pub tags: Option<String>,
    pub project: Option<String>,
    pub repo_id: Option<String>,
    pub priority: i64,
    pub status: String,
    pub category: Option<String>,
    pub linked_doc_id: Option<String>,
    pub linked_task_id: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

/// Create a new idea
#[allow(clippy::too_many_arguments)]
pub async fn create_idea(
    pool: &PgPool,
    content: &str,
    tags: Option<&str>,
    project: Option<&str>,
    repo_id: Option<&str>,
    priority: i64,
    status: &str,
    category: Option<&str>,
) -> DbResult<String> {
    let id = Uuid::new_v4().to_string();
    let now = chrono::Utc::now().timestamp();

    sqlx::query(
        "INSERT INTO ideas (id, content, tags, project, repo_id, priority, status, category, created_at, updated_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
    )
    .bind(&id)
    .bind(content)
    .bind(tags)
    .bind(project)
    .bind(repo_id)
    .bind(priority)
    .bind(status)
    .bind(category)
    .bind(now)
    .bind(now)
    .execute(pool)
    .await
    .map_err(DbError::Sqlx)?;

    Ok(id)
}

/// List ideas with optional filters
pub async fn list_ideas(
    pool: &PgPool,
    limit: i64,
    status: Option<&str>,
    category: Option<&str>,
    tag: Option<&str>,
    project: Option<&str>,
) -> DbResult<Vec<Idea>> {
    // Build dynamic WHERE clause with numbered Postgres placeholders
    let mut conditions: Vec<String> = vec!["1=1".to_string()];
    let mut param_idx = 1usize;

    if status.is_some() {
        param_idx += 1;
        conditions.push(format!("status = ${}", param_idx));
    }
    if category.is_some() {
        param_idx += 1;
        conditions.push(format!("category = ${}", param_idx));
    }
    if tag.is_some() {
        param_idx += 1;
        conditions.push(format!("tags LIKE '%' || ${} || '%'", param_idx));
    }
    if project.is_some() {
        param_idx += 1;
        conditions.push(format!("project = ${}", param_idx));
    }
    // limit is always the last param
    param_idx += 1;
    let limit_param = param_idx;

    let sql = format!(
        "SELECT id, content, tags, project, repo_id, priority, status, category,
                linked_doc_id, linked_task_id, created_at, updated_at
         FROM ideas
         WHERE {}
         ORDER BY created_at DESC
         LIMIT ${}",
        conditions[1..].join(" AND "),
        limit_param
    );

    let mut q = sqlx::query_as::<_, Idea>(&sql);

    if let Some(s) = status {
        q = q.bind(s);
    }
    if let Some(c) = category {
        q = q.bind(c);
    }
    if let Some(t) = tag {
        q = q.bind(t);
    }
    if let Some(p) = project {
        q = q.bind(p);
    }
    q = q.bind(limit);

    q.fetch_all(pool).await.map_err(DbError::Sqlx)
}

/// Update idea status
pub async fn update_idea_status(pool: &PgPool, id: &str, status: &str) -> DbResult<()> {
    let now = chrono::Utc::now().timestamp();

    sqlx::query("UPDATE ideas SET status = $1, updated_at = $2 WHERE id = $3")
        .bind(status)
        .bind(now)
        .bind(id)
        .execute(pool)
        .await
        .map_err(DbError::Sqlx)?;

    Ok(())
}

/// Delete an idea
pub async fn delete_idea(pool: &PgPool, id: &str) -> DbResult<()> {
    sqlx::query("DELETE FROM ideas WHERE id = $1")
        .bind(id)
        .execute(pool)
        .await
        .map_err(DbError::Sqlx)?;
    Ok(())
}

/// Count total ideas
pub async fn count_ideas(pool: &PgPool) -> DbResult<i64> {
    let count = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM ideas")
        .fetch_one(pool)
        .await
        .map_err(DbError::Sqlx)?;
    Ok(count)
}

// ============================================================================
// Tags — Tag registry and search
// ============================================================================

/// Tag model for tag registry
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct Tag {
    pub name: String,
    pub color: String,
    pub description: Option<String>,
    pub usage_count: i64,
    pub created_at: i64,
    pub updated_at: i64,
}

/// List tags ordered by usage count
pub async fn list_tags(pool: &PgPool, limit: i64) -> DbResult<Vec<Tag>> {
    sqlx::query_as::<_, Tag>(
        "SELECT name, color, description, usage_count, created_at, updated_at
         FROM tags
         ORDER BY usage_count DESC
         LIMIT $1",
    )
    .bind(limit)
    .fetch_all(pool)
    .await
    .map_err(DbError::Sqlx)
}

/// Search tags by name
pub async fn search_tags(pool: &PgPool, query: &str) -> DbResult<Vec<Tag>> {
    let pattern = format!("%{}%", query);
    sqlx::query_as::<_, Tag>(
        "SELECT name, color, description, usage_count, created_at, updated_at
         FROM tags
         WHERE name ILIKE $1
         ORDER BY usage_count DESC
         LIMIT 50",
    )
    .bind(&pattern)
    .fetch_all(pool)
    .await
    .map_err(DbError::Sqlx)
}

// ============================================================================
// Document Full-Text Search
// ============================================================================

/// Search documents using Postgres full-text search (tsvector/tsquery)
/// Falls back to ILIKE if the FTS index is not available.
pub async fn search_documents(pool: &PgPool, query: &str) -> DbResult<Vec<Document>> {
    let pattern = format!("%{}%", query);

    // Use ILIKE on title + content — works without a dedicated FTS index.
    // Replace with `to_tsvector('english', content) @@ plainto_tsquery($1)` once
    // GIN index is created in migrations.
    let rows = sqlx::query(
        "SELECT id, title, content, content_type, source_type, source_url, doc_type, tags,
                repo_id, file_path, word_count, char_count, created_at, updated_at, indexed_at,
                COALESCE(pinned, FALSE) AS pinned
         FROM documents
         WHERE title ILIKE $1 OR content ILIKE $1
         ORDER BY pinned DESC, created_at DESC
         LIMIT 50",
    )
    .bind(&pattern)
    .fetch_all(pool)
    .await
    .map_err(DbError::Sqlx)?;

    Ok(rows
        .into_iter()
        .map(|row| Document {
            id: row.get::<Option<String>, _>("id").unwrap_or_default(),
            title: row.get("title"),
            content: row.get("content"),
            content_type: row
                .get::<Option<String>, _>("content_type")
                .unwrap_or_else(|| "markdown".to_string()),
            source_type: row
                .get::<Option<String>, _>("source_type")
                .unwrap_or_else(|| "manual".to_string()),
            source_url: row.get("source_url"),
            doc_type: row
                .get::<Option<String>, _>("doc_type")
                .unwrap_or_else(|| "reference".to_string()),
            tags: row.get("tags"),
            repo_id: row.get("repo_id"),
            file_path: row.get("file_path"),
            word_count: row.get::<Option<i64>, _>("word_count").unwrap_or(0),
            pinned: row.get::<Option<bool>, _>("pinned").unwrap_or(false),
            char_count: row.get::<Option<i64>, _>("char_count").unwrap_or(0),
            created_at: row.get("created_at"),
            updated_at: row.get("updated_at"),
            indexed_at: row.get("indexed_at"),
        })
        .collect())
}
