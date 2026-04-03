//! Semantic Search Module
//!
//! This module provides semantic search capabilities using vector embeddings.
//! It supports similarity-based retrieval, filtering, ranking, and hybrid search.
//!
//! # Features
//!
//! - **Vector similarity search**: Find semantically similar documents
//! - **Top-k retrieval**: Get the most relevant results
//! - **Filtering**: Filter by document type, tags, repository, dates
//! - **Hybrid search**: Combine semantic and keyword search
//! - **Relevance scoring**: Rank results by relevance
//!
//! # Example
//!
//! ```rust,no_run
//! use rustcode::search::{SemanticSearcher, SearchQuery, SearchConfig};
//! use sqlx::PgPool;
//!
//! # async fn example(pool: &PgPool) -> anyhow::Result<()> {
//! let searcher = SemanticSearcher::new(SearchConfig::default()).await?;
//!
//! let query = SearchQuery {
//!     text: "How do I implement async functions in Rust?".to_string(),
//!     top_k: 10,
//!     filters: Default::default(),
//! };
//!
//! let results = searcher.search(pool, &query).await?;
//!
//! for result in results {
//!     println!("Document: {} (score: {:.4})", result.document_id, result.score);
//! }
//! # Ok(())
//! # }
//! ```

use crate::embeddings::{Embedding, EmbeddingGenerator};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Row};
use std::collections::HashMap;

// ============================================================================
// Configuration
// ============================================================================

/// Configuration for semantic search
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchConfig {
    /// Default number of results to return
    pub default_top_k: usize,

    /// Maximum number of results allowed
    pub max_top_k: usize,

    /// Minimum similarity score threshold (0.0 - 1.0)
    pub min_similarity: f32,

    /// Whether to use hybrid search by default
    pub use_hybrid_search: bool,

    /// Weight for semantic search in hybrid mode (0.0 - 1.0)
    pub semantic_weight: f32,

    /// Weight for keyword search in hybrid mode (0.0 - 1.0)
    pub keyword_weight: f32,
}

impl Default for SearchConfig {
    fn default() -> Self {
        Self {
            default_top_k: 10,
            max_top_k: 100,
            min_similarity: 0.0,
            use_hybrid_search: false,
            semantic_weight: 0.7,
            keyword_weight: 0.3,
        }
    }
}

// ============================================================================
// Search Query
// ============================================================================

/// A search query with filters
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchQuery {
    /// The search text
    pub text: String,

    /// Number of results to return
    #[serde(default)]
    pub top_k: usize,

    /// Search filters
    #[serde(default)]
    pub filters: SearchFilters,
}

/// Filters for search queries
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchFilters {
    /// Filter by document type
    pub doc_type: Option<String>,

    /// Filter by tags (any of these tags)
    pub tags: Option<Vec<String>>,

    /// Filter by repository ID
    pub repo_id: Option<i64>,

    /// Filter by source type (e.g., "file", "manual", "web")
    pub source_type: Option<String>,

    /// Filter by minimum creation date (unix timestamp)
    pub created_after: Option<i64>,

    /// Filter by maximum creation date (unix timestamp)
    pub created_before: Option<i64>,

    /// Only search indexed documents
    #[serde(default = "default_indexed_only")]
    pub indexed_only: bool,
}

fn default_indexed_only() -> bool {
    true
}

impl Default for SearchFilters {
    fn default() -> Self {
        Self {
            doc_type: None,
            tags: None,
            repo_id: None,
            source_type: None,
            created_after: None,
            created_before: None,
            indexed_only: true,
        }
    }
}

// ============================================================================
// Search Results
// ============================================================================

/// A single search result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResult {
    /// Document ID
    pub document_id: String,

    /// Chunk ID that matched
    pub chunk_id: String,

    /// Chunk index within the document
    pub chunk_index: i64,

    /// The chunk content
    pub content: String,

    /// Similarity score (0.0 - 1.0)
    pub score: f32,

    /// Document title
    pub title: Option<String>,

    /// Document type
    pub doc_type: Option<String>,

    /// Document tags
    pub tags: Option<Vec<String>>,

    /// Heading context for this chunk
    pub heading: Option<String>,

    /// Character position in original document
    pub char_start: i64,

    /// Character end position in original document
    pub char_end: i64,

    /// Metadata about the match
    pub metadata: SearchResultMetadata,
}

/// Metadata about a search result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResultMetadata {
    /// Model used for embedding
    pub model: String,

    /// Embedding dimension
    pub dimension: i64,

    /// Whether this was a semantic match
    pub semantic_match: bool,

    /// Whether this was a keyword match
    pub keyword_match: bool,

    /// Semantic similarity score (if semantic match)
    pub semantic_score: Option<f32>,

    /// Keyword score (if keyword match)
    pub keyword_score: Option<f32>,
}

// ============================================================================
// Semantic Searcher
// ============================================================================

/// Main semantic search engine
pub struct SemanticSearcher {
    config: SearchConfig,
    embedding_generator: EmbeddingGenerator,
}

impl SemanticSearcher {
    /// Create a new semantic searcher
    pub async fn new(config: SearchConfig) -> Result<Self> {
        let embedding_config = crate::embeddings::EmbeddingConfig::default();
        let embedding_generator = EmbeddingGenerator::new(embedding_config)
            .context("Failed to create embedding generator")?;

        Ok(Self {
            config,
            embedding_generator,
        })
    }

    /// Perform semantic search
    pub async fn search(&self, pool: &PgPool, query: &SearchQuery) -> Result<Vec<SearchResult>> {
        // Determine top_k
        let top_k = if query.top_k == 0 {
            self.config.default_top_k
        } else {
            query.top_k.min(self.config.max_top_k)
        };

        if self.config.use_hybrid_search {
            self.hybrid_search(pool, query, top_k).await
        } else {
            self.semantic_search_only(pool, query, top_k).await
        }
    }

    /// Perform semantic-only search
    async fn semantic_search_only(
        &self,
        pool: &PgPool,
        query: &SearchQuery,
        top_k: usize,
    ) -> Result<Vec<SearchResult>> {
        // Generate query embedding
        tracing::debug!("Generating embedding for query: {}", query.text);
        let query_embedding = self
            .embedding_generator
            .embed(&query.text)
            .await
            .context("Failed to generate query embedding")?;

        // Retrieve all candidate embeddings from database
        tracing::debug!("Retrieving candidate embeddings from database");
        let candidates = self.get_candidate_embeddings(pool, &query.filters).await?;

        if candidates.is_empty() {
            tracing::warn!("No candidate embeddings found");
            return Ok(Vec::new());
        }

        // Calculate similarities
        tracing::debug!(
            "Calculating similarities for {} candidates",
            candidates.len()
        );
        let mut scored_results: Vec<(CandidateEmbedding, f32)> = candidates
            .into_iter()
            .filter_map(
                |candidate| match self.calculate_similarity(&query_embedding, &candidate) {
                    Ok(score) => {
                        if score >= self.config.min_similarity {
                            Some((candidate, score))
                        } else {
                            None
                        }
                    }
                    Err(e) => {
                        tracing::error!("Failed to calculate similarity: {}", e);
                        None
                    }
                },
            )
            .collect();

        // Sort by score descending
        scored_results.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

        // Take top k
        scored_results.truncate(top_k);

        // Convert to search results
        let results = scored_results
            .into_iter()
            .map(|(candidate, score)| SearchResult {
                document_id: candidate.document_id,
                chunk_id: candidate.chunk_id,
                chunk_index: candidate.chunk_index,
                content: candidate.content,
                score,
                title: candidate.title,
                doc_type: candidate.doc_type,
                tags: candidate.tags,
                heading: candidate.heading,
                char_start: candidate.char_start,
                char_end: candidate.char_end,
                metadata: SearchResultMetadata {
                    model: candidate.model,
                    dimension: candidate.dimension,
                    semantic_match: true,
                    keyword_match: false,
                    semantic_score: Some(score),
                    keyword_score: None,
                },
            })
            .collect();

        Ok(results)
    }

    /// Perform hybrid search (semantic + keyword)
    async fn hybrid_search(
        &self,
        pool: &PgPool,
        query: &SearchQuery,
        top_k: usize,
    ) -> Result<Vec<SearchResult>> {
        // Get semantic results
        let semantic_results = self.semantic_search_only(pool, query, top_k * 2).await?;

        // Get keyword results
        let keyword_results = self.keyword_search(pool, query, top_k * 2).await?;

        // Merge using Reciprocal Rank Fusion
        let merged = self.merge_results(semantic_results, keyword_results, top_k);

        Ok(merged)
    }

    /// Perform keyword-based search
    async fn keyword_search(
        &self,
        pool: &PgPool,
        query: &SearchQuery,
        top_k: usize,
    ) -> Result<Vec<SearchResult>> {
        let filter_clause = self.build_filter_clause(&query.filters);

        let sql = format!(
            "SELECT
                c.id as chunk_id,
                c.document_id,
                c.chunk_index,
                c.content,
                c.heading,
                c.char_start,
                c.char_end,
                d.title,
                d.doc_type,
                d.tags
             FROM document_chunks c
             JOIN documents d ON c.document_id = d.id
             WHERE c.content LIKE ?
             {}
             ORDER BY c.chunk_index ASC
             LIMIT ?",
            filter_clause
        );

        let search_pattern = format!("%{}%", query.text);

        let rows = sqlx::query(&sql)
            .bind(&search_pattern)
            .bind(top_k as i64)
            .fetch_all(pool)
            .await
            .context("Failed to execute keyword search")?;

        let results = rows
            .into_iter()
            .enumerate()
            .map(|(idx, row)| {
                let tags_str: Option<String> = row.try_get("tags").ok();
                let tags = tags_str.and_then(|s| serde_json::from_str(&s).ok());

                // Simple scoring based on rank
                let score = 1.0 / (idx as f32 + 1.0);

                SearchResult {
                    document_id: row.get("document_id"),
                    chunk_id: row.get("chunk_id"),
                    chunk_index: row.get("chunk_index"),
                    content: row.get("content"),
                    score,
                    title: row.try_get("title").ok(),
                    doc_type: row.try_get("doc_type").ok(),
                    tags,
                    heading: row.try_get("heading").ok(),
                    char_start: row.get("char_start"),
                    char_end: row.get("char_end"),
                    metadata: SearchResultMetadata {
                        model: "keyword".to_string(),
                        dimension: 0,
                        semantic_match: false,
                        keyword_match: true,
                        semantic_score: None,
                        keyword_score: Some(score),
                    },
                }
            })
            .collect();

        Ok(results)
    }

    /// Merge semantic and keyword results using Reciprocal Rank Fusion
    fn merge_results(
        &self,
        semantic_results: Vec<SearchResult>,
        keyword_results: Vec<SearchResult>,
        top_k: usize,
    ) -> Vec<SearchResult> {
        let mut scores: HashMap<String, (f32, SearchResult)> = HashMap::new();
        let k = 60.0; // RRF constant

        // Add semantic results
        for (rank, result) in semantic_results.into_iter().enumerate() {
            let rrf_score = self.config.semantic_weight / (k + rank as f32 + 1.0);
            scores.insert(result.chunk_id.clone(), (rrf_score, result));
        }

        // Add keyword results
        for (rank, result) in keyword_results.into_iter().enumerate() {
            let rrf_score = self.config.keyword_weight / (k + rank as f32 + 1.0);

            scores
                .entry(result.chunk_id.clone())
                .and_modify(|(score, existing)| {
                    *score += rrf_score;
                    existing.metadata.keyword_match = true;
                    existing.metadata.keyword_score = Some(result.score);
                })
                .or_insert((rrf_score, result));
        }

        // Sort by combined score
        let mut merged: Vec<(f32, SearchResult)> = scores
            .into_iter()
            .map(|(_, (score, mut result))| {
                result.score = score;
                (score, result)
            })
            .collect();

        merged.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

        // Take top k
        merged.truncate(top_k);

        merged.into_iter().map(|(_, result)| result).collect()
    }

    /// Get candidate embeddings from database with filters
    async fn get_candidate_embeddings(
        &self,
        pool: &PgPool,
        filters: &SearchFilters,
    ) -> Result<Vec<CandidateEmbedding>> {
        let filter_clause = self.build_filter_clause(filters);

        let sql = format!(
            "SELECT
                e.id as embedding_id,
                e.chunk_id,
                e.embedding,
                e.model,
                e.dimension,
                c.document_id,
                c.chunk_index,
                c.content,
                c.heading,
                c.char_start,
                c.char_end,
                d.title,
                d.doc_type,
                d.tags
             FROM document_embeddings e
             JOIN document_chunks c ON e.chunk_id = c.id
             JOIN documents d ON c.document_id = d.id
             {}",
            filter_clause
        );

        let rows = sqlx::query(&sql)
            .fetch_all(pool)
            .await
            .context("Failed to fetch candidate embeddings")?;

        let candidates = rows
            .into_iter()
            .filter_map(|row| {
                let embedding_bytes: Vec<u8> = match row.try_get("embedding") {
                    Ok(bytes) => bytes,
                    Err(e) => {
                        tracing::error!("Failed to get embedding bytes: {}", e);
                        return None;
                    }
                };

                let model: String = row.get("model");
                let dimension: i64 = row.get("dimension");

                let embedding = match Embedding::from_bytes(
                    &embedding_bytes,
                    model.clone(),
                    dimension as usize,
                ) {
                    Ok(emb) => emb,
                    Err(e) => {
                        tracing::error!("Failed to deserialize embedding: {}", e);
                        return None;
                    }
                };

                let tags_str: Option<String> = row.try_get("tags").ok();
                let tags = tags_str.and_then(|s| serde_json::from_str(&s).ok());

                Some(CandidateEmbedding {
                    chunk_id: row.get("chunk_id"),
                    document_id: row.get("document_id"),
                    chunk_index: row.get("chunk_index"),
                    content: row.get("content"),
                    embedding,
                    model,
                    dimension,
                    title: row.try_get("title").ok(),
                    doc_type: row.try_get("doc_type").ok(),
                    tags,
                    heading: row.try_get("heading").ok(),
                    char_start: row.get("char_start"),
                    char_end: row.get("char_end"),
                })
            })
            .collect();

        Ok(candidates)
    }

    /// Build SQL filter clause from search filters
    fn build_filter_clause(&self, filters: &SearchFilters) -> String {
        let mut conditions = Vec::new();

        if filters.indexed_only {
            conditions.push("d.indexed_at IS NOT NULL".to_string());
        }

        if let Some(doc_type) = &filters.doc_type {
            conditions.push(format!("d.doc_type = '{}'", doc_type));
        }

        if let Some(source_type) = &filters.source_type {
            conditions.push(format!("d.source_type = '{}'", source_type));
        }

        if let Some(repo_id) = filters.repo_id {
            conditions.push(format!("d.repo_id = {}", repo_id));
        }

        if let Some(created_after) = filters.created_after {
            conditions.push(format!("d.created_at >= {}", created_after));
        }

        if let Some(created_before) = filters.created_before {
            conditions.push(format!("d.created_at <= {}", created_before));
        }

        // Tag filtering requires JSON operations
        if let Some(tags) = &filters.tags {
            if !tags.is_empty() {
                let tag_conditions: Vec<String> = tags
                    .iter()
                    .map(|tag| format!("d.tags LIKE '%\"{}%'", tag))
                    .collect();
                conditions.push(format!("({})", tag_conditions.join(" OR ")));
            }
        }

        if conditions.is_empty() {
            String::new()
        } else {
            format!("WHERE {}", conditions.join(" AND "))
        }
    }

    /// Calculate cosine similarity between query and candidate
    fn calculate_similarity(
        &self,
        query_embedding: &Embedding,
        candidate: &CandidateEmbedding,
    ) -> Result<f32> {
        query_embedding.cosine_similarity(&candidate.embedding)
    }

    /// Get search configuration
    pub fn config(&self) -> &SearchConfig {
        &self.config
    }
}

// ============================================================================
// Internal Types
// ============================================================================

/// A candidate embedding from the database
#[derive(Debug, Clone)]
struct CandidateEmbedding {
    chunk_id: String,
    document_id: String,
    chunk_index: i64,
    content: String,
    embedding: Embedding,
    model: String,
    dimension: i64,
    title: Option<String>,
    doc_type: Option<String>,
    tags: Option<Vec<String>>,
    heading: Option<String>,
    char_start: i64,
    char_end: i64,
}

// ============================================================================
// Statistics
// ============================================================================

/// Statistics about search operations
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SearchStats {
    /// Total searches performed
    pub total_searches: usize,

    /// Total results returned
    pub total_results: usize,

    /// Average results per search
    pub avg_results_per_search: f64,

    /// Average search time (milliseconds)
    pub avg_search_time_ms: f64,

    /// Searches with zero results
    pub zero_result_searches: usize,
}

impl SearchStats {
    /// Create new search stats
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a search operation
    pub fn record_search(&mut self, results_count: usize, time_ms: f64) {
        self.total_searches += 1;
        self.total_results += results_count;

        if results_count == 0 {
            self.zero_result_searches += 1;
        }

        // Update rolling averages
        let n = self.total_searches as f64;
        self.avg_results_per_search =
            ((self.avg_results_per_search * (n - 1.0)) + results_count as f64) / n;
        self.avg_search_time_ms = ((self.avg_search_time_ms * (n - 1.0)) + time_ms) / n;
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_search_config_default() {
        let config = SearchConfig::default();
        assert_eq!(config.default_top_k, 10);
        assert_eq!(config.max_top_k, 100);
        assert_eq!(config.min_similarity, 0.0);
        assert!(!config.use_hybrid_search);
    }

    #[test]
    fn test_search_filters_default() {
        let filters = SearchFilters::default();
        assert!(filters.indexed_only);
        assert!(filters.doc_type.is_none());
        assert!(filters.tags.is_none());
    }

    #[test]
    fn test_search_stats() {
        let mut stats = SearchStats::new();
        assert_eq!(stats.total_searches, 0);

        stats.record_search(10, 50.0);
        assert_eq!(stats.total_searches, 1);
        assert_eq!(stats.total_results, 10);
        assert_eq!(stats.avg_results_per_search, 10.0);
        assert_eq!(stats.avg_search_time_ms, 50.0);

        stats.record_search(0, 30.0);
        assert_eq!(stats.total_searches, 2);
        assert_eq!(stats.zero_result_searches, 1);
        assert_eq!(stats.avg_results_per_search, 5.0);
        assert_eq!(stats.avg_search_time_ms, 40.0);
    }
}
