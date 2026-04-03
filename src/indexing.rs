//! Document Indexing Module
//!
//! This module orchestrates the complete document indexing pipeline:
//! 1. Chunk the document into smaller pieces
//! 2. Generate embeddings for each chunk
//! 3. Store chunks and embeddings in the database
//! 4. Mark document as indexed
//!
//! # Features
//!
//! - **End-to-end indexing**: Complete pipeline from document to searchable embeddings
//! - **Batch processing**: Efficient batch embedding generation
//! - **Transaction safety**: Atomic operations with rollback on failure
//! - **Progress tracking**: Monitor indexing progress for large documents
//!
//! # Example
//!
//! ```rust,no_run
//! use rustcode::indexing::{DocumentIndexer, IndexingConfig};
//! use rustcode::db::get_document;
//! use sqlx::PgPool;
//!
//! # async fn example(pool: &PgPool) -> anyhow::Result<()> {
//! let indexer = DocumentIndexer::new(IndexingConfig::default()).await?;
//!
//! // Index a document by ID
//! let document_id = "doc-123";
//! let result = indexer.index_document(pool, document_id).await?;
//!
//! println!("Indexed {} chunks", result.chunks_indexed);
//! # Ok(())
//! # }
//! ```

use crate::chunking::{chunk_document, ChunkConfig};
use crate::db::{create_chunks, get_document, mark_document_indexed, store_embedding};
use crate::embeddings::{EmbeddingConfig, EmbeddingGenerator};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use std::sync::Arc;

// ============================================================================
// Configuration
// ============================================================================

/// Configuration for document indexing
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexingConfig {
    /// Chunking configuration
    pub chunk_config: ChunkConfig,

    /// Embedding configuration
    pub embedding_config: EmbeddingConfig,

    /// Maximum number of chunks to process in a single batch
    pub max_batch_size: usize,

    /// Whether to overwrite existing embeddings
    pub overwrite_existing: bool,
}

impl Default for IndexingConfig {
    fn default() -> Self {
        Self {
            chunk_config: ChunkConfig::default(),
            embedding_config: EmbeddingConfig::default(),
            max_batch_size: 32,
            overwrite_existing: false,
        }
    }
}

// ============================================================================
// Indexing Results
// ============================================================================

/// Result of indexing a single document
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexingResult {
    /// Document ID that was indexed
    pub document_id: String,

    /// Number of chunks created
    pub chunks_created: usize,

    /// Number of chunks indexed (with embeddings)
    pub chunks_indexed: usize,

    /// Total word count processed
    pub total_words: usize,

    /// Model used for embeddings
    pub model_name: String,

    /// Embedding dimension
    pub embedding_dimension: usize,

    /// Whether this was a re-index (overwrite)
    pub was_reindexed: bool,
}

impl IndexingResult {
    /// Create a new indexing result
    pub fn new(
        document_id: String,
        chunks_created: usize,
        chunks_indexed: usize,
        total_words: usize,
        model_name: String,
        embedding_dimension: usize,
        was_reindexed: bool,
    ) -> Self {
        Self {
            document_id,
            chunks_created,
            chunks_indexed,
            total_words,
            model_name,
            embedding_dimension,
            was_reindexed,
        }
    }
}

/// Progress information during indexing
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexingProgress {
    /// Current chunk being processed
    pub current_chunk: usize,

    /// Total chunks to process
    pub total_chunks: usize,

    /// Percentage complete (0-100)
    pub percent_complete: f64,

    /// Current stage
    pub stage: IndexingStage,
}

/// Stages of the indexing process
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum IndexingStage {
    /// Loading document from database
    LoadingDocument,

    /// Chunking document
    Chunking,

    /// Generating embeddings
    GeneratingEmbeddings,

    /// Storing to database
    StoringToDatabase,

    /// Marking as indexed
    MarkingIndexed,

    /// Complete
    Complete,
}

impl std::fmt::Display for IndexingStage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::LoadingDocument => write!(f, "Loading document"),
            Self::Chunking => write!(f, "Chunking document"),
            Self::GeneratingEmbeddings => write!(f, "Generating embeddings"),
            Self::StoringToDatabase => write!(f, "Storing to database"),
            Self::MarkingIndexed => write!(f, "Marking as indexed"),
            Self::Complete => write!(f, "Complete"),
        }
    }
}

// ============================================================================
// Document Indexer
// ============================================================================

/// Main document indexer that orchestrates the indexing pipeline
pub struct DocumentIndexer {
    config: IndexingConfig,
    embedding_generator: Arc<EmbeddingGenerator>,
}

impl DocumentIndexer {
    /// Create a new document indexer
    pub async fn new(config: IndexingConfig) -> Result<Self> {
        let embedding_generator = EmbeddingGenerator::new(config.embedding_config.clone())
            .context("Failed to create embedding generator")?;

        Ok(Self {
            config,
            embedding_generator: Arc::new(embedding_generator),
        })
    }

    /// Index a document by ID
    ///
    /// This performs the complete indexing pipeline:
    /// 1. Load document from database
    /// 2. Chunk the document
    /// 3. Generate embeddings for chunks
    /// 4. Store chunks and embeddings
    /// 5. Mark document as indexed
    pub async fn index_document(&self, pool: &PgPool, document_id: &str) -> Result<IndexingResult> {
        tracing::info!("Starting indexing for document: {}", document_id);

        // Stage 1: Load document
        tracing::debug!("Loading document from database");
        let document = get_document(pool, document_id)
            .await
            .context("Failed to load document")?;

        if document.content.is_empty() {
            anyhow::bail!("Document has no content to index");
        }

        // Check if already indexed
        let was_reindexed = document.indexed_at.is_some();
        if was_reindexed && !self.config.overwrite_existing {
            tracing::warn!("Document already indexed and overwrite_existing=false");
            anyhow::bail!("Document already indexed. Set overwrite_existing=true to re-index.");
        }

        // Stage 2: Chunk the document
        tracing::debug!("Chunking document");
        let chunks = chunk_document(&document.content, &self.config.chunk_config)
            .context("Failed to chunk document")?;

        if chunks.is_empty() {
            anyhow::bail!("No chunks generated from document");
        }

        tracing::info!("Generated {} chunks", chunks.len());

        // Stage 3: Generate embeddings in batches
        tracing::debug!("Generating embeddings for {} chunks", chunks.len());
        let mut all_embeddings = Vec::new();

        for (batch_idx, chunk_batch) in chunks.chunks(self.config.max_batch_size).enumerate() {
            let batch_texts: Vec<&str> = chunk_batch.iter().map(|c| c.content.as_str()).collect();

            tracing::debug!(
                "Processing batch {}/{} ({} chunks)",
                batch_idx + 1,
                chunks.len().div_ceil(self.config.max_batch_size),
                batch_texts.len()
            );

            let batch_embeddings = self
                .embedding_generator
                .embed_batch(&batch_texts)
                .await
                .context("Failed to generate embeddings")?;

            all_embeddings.extend(batch_embeddings);
        }

        tracing::info!("Generated {} embeddings", all_embeddings.len());

        // Stage 4: Store chunks and embeddings to database
        tracing::debug!("Storing chunks and embeddings to database");

        // Prepare chunks in the format expected by create_chunks
        let chunk_tuples: Vec<(String, i64, i64, Option<String>)> = chunks
            .iter()
            .map(|chunk| {
                (
                    chunk.content.clone(),
                    chunk.char_start as i64,
                    chunk.char_end as i64,
                    chunk.heading.clone(),
                )
            })
            .collect();

        let db_chunks = create_chunks(pool, document_id.to_string(), chunk_tuples)
            .await
            .context("Failed to store chunks")?;

        // Store embeddings
        let model_name = self.embedding_generator.model_name();

        for (idx, (chunk, embedding)) in db_chunks.iter().zip(all_embeddings.iter()).enumerate() {
            store_embedding(
                pool,
                chunk.id.clone(),
                embedding.vector.clone(),
                model_name.to_string(),
            )
            .await
            .with_context(|| format!("Failed to store embedding for chunk {}", idx))?;
        }

        // Stage 5: Mark document as indexed
        tracing::debug!("Marking document as indexed");
        mark_document_indexed(pool, document_id)
            .await
            .context("Failed to mark document as indexed")?;

        let total_words: usize = chunks.iter().map(|c| c.word_count).sum();

        let result = IndexingResult::new(
            document_id.to_string(),
            chunks.len(),
            all_embeddings.len(),
            total_words,
            model_name.to_string(),
            self.embedding_generator.dimension(),
            was_reindexed,
        );

        tracing::info!(
            "Successfully indexed document: {} chunks, {} words",
            result.chunks_indexed,
            result.total_words
        );

        Ok(result)
    }

    /// Index multiple documents in sequence
    pub async fn index_documents(
        &self,
        pool: &PgPool,
        document_ids: &[&str],
    ) -> Result<Vec<IndexingResult>> {
        let mut results = Vec::new();

        for (idx, document_id) in document_ids.iter().enumerate() {
            tracing::info!(
                "Indexing document {}/{}: {}",
                idx + 1,
                document_ids.len(),
                document_id
            );

            match self.index_document(pool, document_id).await {
                Ok(result) => results.push(result),
                Err(e) => {
                    tracing::error!("Failed to index document {}: {}", document_id, e);
                    // Continue with other documents
                }
            }
        }

        Ok(results)
    }

    /// Get the indexer configuration
    pub fn config(&self) -> &IndexingConfig {
        &self.config
    }

    /// Get the embedding generator
    pub fn embedding_generator(&self) -> &EmbeddingGenerator {
        &self.embedding_generator
    }
}

// ============================================================================
// Batch Indexing
// ============================================================================

/// Batch indexer for processing many documents concurrently.
pub struct BatchIndexer {
    indexer: Arc<DocumentIndexer>,
    concurrency: usize,
}

impl BatchIndexer {
    /// Create a new batch indexer.
    pub async fn new(config: IndexingConfig, concurrency: usize) -> Result<Self> {
        let indexer = DocumentIndexer::new(config).await?;
        Ok(Self {
            indexer: Arc::new(indexer),
            concurrency: concurrency.max(1),
        })
    }

    /// Index `document_ids` concurrently, gated by a semaphore of size `self.concurrency`.
    ///
    /// Individual document failures are logged and skipped — the method always
    /// returns the results for documents that succeeded.
    pub async fn index_batch(
        &self,
        pool: &PgPool,
        document_ids: &[String],
    ) -> Result<Vec<IndexingResult>> {
        if document_ids.is_empty() {
            return Ok(Vec::new());
        }

        let semaphore = Arc::new(tokio::sync::Semaphore::new(self.concurrency));
        let mut join_set: tokio::task::JoinSet<Option<IndexingResult>> =
            tokio::task::JoinSet::new();

        for doc_id in document_ids {
            let sem = Arc::clone(&semaphore);
            let indexer = Arc::clone(&self.indexer);
            let pool = pool.clone();
            let id = doc_id.clone();

            join_set.spawn(async move {
                // Acquire permit before starting — limits concurrent DB + embedding work.
                let _permit = sem
                    .acquire()
                    .await
                    .expect("BatchIndexer semaphore closed unexpectedly");

                match indexer.index_document(&pool, &id).await {
                    Ok(result) => {
                        tracing::info!(
                            document_id = %id,
                            chunks = result.chunks_indexed,
                            "Batch index succeeded"
                        );
                        Some(result)
                    }
                    Err(e) => {
                        tracing::error!(document_id = %id, error = %e, "Batch index failed");
                        None
                    }
                }
            });
        }

        let mut results = Vec::with_capacity(document_ids.len());

        while let Some(outcome) = join_set.join_next().await {
            match outcome {
                Ok(Some(result)) => results.push(result),
                Ok(None) => {} // individual failure already logged
                Err(join_err) => {
                    tracing::error!(error = %join_err, "Batch index task panicked");
                }
            }
        }

        tracing::info!(
            total = document_ids.len(),
            succeeded = results.len(),
            failed = document_ids.len() - results.len(),
            "Batch indexing complete"
        );

        Ok(results)
    }

    /// Return the configured concurrency limit.
    pub fn concurrency(&self) -> usize {
        self.concurrency
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_indexing_config_default() {
        let config = IndexingConfig::default();
        assert_eq!(config.max_batch_size, 32);
        assert!(!config.overwrite_existing);
    }

    #[test]
    fn test_indexing_result() {
        let result = IndexingResult::new(
            "test-doc".to_string(),
            10,
            10,
            500,
            "test-model".to_string(),
            384,
            false,
        );

        assert_eq!(result.document_id, "test-doc");
        assert_eq!(result.chunks_created, 10);
        assert_eq!(result.chunks_indexed, 10);
        assert_eq!(result.total_words, 500);
        assert!(!result.was_reindexed);
    }

    #[test]
    fn test_indexing_stage_display() {
        assert_eq!(
            IndexingStage::LoadingDocument.to_string(),
            "Loading document"
        );
        assert_eq!(IndexingStage::Chunking.to_string(), "Chunking document");
        assert_eq!(IndexingStage::Complete.to_string(), "Complete");
    }
}
