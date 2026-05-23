// Document Embedding Module
//
// This module provides embedding generation for the RAG system using fastembed.
// It handles model initialization, caching, and batch embedding generation.
//
// # Features
//
// - **Multiple models**: Support for various embedding models
// - **Batch processing**: Efficient batch embedding generation
// - **Model caching**: Lazy initialization and reuse
// - **Error handling**: Comprehensive error types
//
// # Example
//
// ```rust,no_run
// use rustcode::embeddings::{EmbeddingGenerator, EmbeddingConfig};
//
// # async fn example() -> anyhow::Result<()> {
// let config = EmbeddingConfig::default();
// let generator = EmbeddingGenerator::new(config)?;
//
// let texts = vec!["Hello world", "Rust is awesome"];
// let embeddings = generator.embed_batch(&texts).await?;
//
// println!("Generated {} embeddings", embeddings.len());
// # Ok(())
// # }
// ```

use anyhow::{Context, Result};
use fastembed::{EmbeddingModel, InitOptions, TextEmbedding};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::RwLock;

// ============================================================================
// Configuration
// ============================================================================

// Configuration for the embedding generator
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbeddingConfig {
    // The embedding model to use
    pub model_name: EmbeddingModelType,

    // Maximum batch size for embedding generation
    pub batch_size: usize,

    // Whether to show download progress
    pub show_download_progress: bool,

    // Cache directory for models (None = use default)
    pub cache_dir: Option<String>,
}

impl Default for EmbeddingConfig {
    fn default() -> Self {
        Self {
            model_name: EmbeddingModelType::BGESmallENV15,
            batch_size: 32,
            show_download_progress: true,
            cache_dir: None,
        }
    }
}

// Supported embedding models
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum EmbeddingModelType {
    // BGE Small EN v1.5 - Fast, 384 dimensions
    BGESmallENV15,
    // BGE Base EN v1.5 - Balanced, 768 dimensions
    BGEBaseENV15,
    // All-MiniLM-L6-v2 - Very fast, 384 dimensions
    AllMiniLML6V2,
}

impl EmbeddingModelType {
    // Get the fastembed model enum
    pub fn to_fastembed_model(&self) -> EmbeddingModel {
        match self {
            Self::BGESmallENV15 => EmbeddingModel::BGESmallENV15,
            Self::BGEBaseENV15 => EmbeddingModel::BGEBaseENV15,
            Self::AllMiniLML6V2 => EmbeddingModel::AllMiniLML6V2,
        }
    }

    // Get the embedding dimension for this model
    pub fn dimension(&self) -> usize {
        match self {
            Self::BGESmallENV15 => 384,
            Self::BGEBaseENV15 => 768,
            Self::AllMiniLML6V2 => 384,
        }
    }

    // Get a human-readable name for this model
    pub fn name(&self) -> &'static str {
        match self {
            Self::BGESmallENV15 => "BGE-small-en-v1.5",
            Self::BGEBaseENV15 => "BGE-base-en-v1.5",
            Self::AllMiniLML6V2 => "all-MiniLM-L6-v2",
        }
    }
}

// ============================================================================
// Embedding Data Types
// ============================================================================

// A single embedding vector
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Embedding {
    // The embedding vector
    pub vector: Vec<f32>,

    // The model used to generate this embedding
    pub model: String,

    // The dimension of the embedding
    pub dimension: usize,
}

impl Embedding {
    // Create a new embedding
    pub fn new(vector: Vec<f32>, model: String, dimension: usize) -> Self {
        Self {
            vector,
            model,
            dimension,
        }
    }

    // Serialize the embedding vector to bytes (for database storage)
    pub fn to_bytes(&self) -> Vec<u8> {
        // Convert f32 vector to byte vector
        let mut bytes = Vec::with_capacity(self.vector.len() * 4);
        for value in &self.vector {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
        bytes
    }

    // Deserialize an embedding vector from bytes
    pub fn from_bytes(bytes: &[u8], model: String, dimension: usize) -> Result<Self> {
        if !bytes.len().is_multiple_of(4) {
            anyhow::bail!("Invalid embedding bytes: length must be multiple of 4");
        }

        let mut vector = Vec::with_capacity(bytes.len() / 4);
        for chunk in bytes.chunks_exact(4) {
            let value = f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
            vector.push(value);
        }

        if vector.len() != dimension {
            anyhow::bail!(
                "Embedding dimension mismatch: expected {}, got {}",
                dimension,
                vector.len()
            );
        }

        Ok(Self {
            vector,
            model,
            dimension,
        })
    }

    // Calculate cosine similarity with another embedding
    pub fn cosine_similarity(&self, other: &Embedding) -> Result<f32> {
        if self.vector.len() != other.vector.len() {
            anyhow::bail!("Cannot compare embeddings with different dimensions");
        }

        let dot_product: f32 = self
            .vector
            .iter()
            .zip(other.vector.iter())
            .map(|(a, b)| a * b)
            .sum();

        let norm_a: f32 = self.vector.iter().map(|x| x * x).sum::<f32>().sqrt();
        let norm_b: f32 = other.vector.iter().map(|x| x * x).sum::<f32>().sqrt();

        if norm_a == 0.0 || norm_b == 0.0 {
            return Ok(0.0);
        }

        Ok(dot_product / (norm_a * norm_b))
    }
}

// ============================================================================
// Embedding Generator
// ============================================================================

// Main embedding generator that wraps the fastembed model
pub struct EmbeddingGenerator {
    config: EmbeddingConfig,
    model: Arc<RwLock<Option<TextEmbedding>>>,
}

impl EmbeddingGenerator {
    // Create a new embedding generator with the given configuration
    pub fn new(config: EmbeddingConfig) -> Result<Self> {
        Ok(Self {
            config,
            model: Arc::new(RwLock::new(None)),
        })
    }

    // Initialize the embedding model (lazy loading)
    async fn ensure_model_loaded(&self) -> Result<()> {
        // Check if model is already loaded
        {
            let model_guard = self.model.read().await;
            if model_guard.is_some() {
                return Ok(());
            }
        }

        // Load the model
        let mut model_guard = self.model.write().await;

        // Double-check in case another task loaded it while we were waiting
        if model_guard.is_some() {
            return Ok(());
        }

        let mut init_options = InitOptions::new(self.config.model_name.to_fastembed_model());

        if let Some(cache_dir) = &self.config.cache_dir {
            init_options = init_options.with_cache_dir(cache_dir.into());
        }

        init_options = init_options.with_show_download_progress(self.config.show_download_progress);

        // Initialize the model (this may download it if not cached)
        let embedding_model =
            TextEmbedding::try_new(init_options).context("Failed to initialize embedding model")?;

        *model_guard = Some(embedding_model);

        tracing::info!(
            "Loaded embedding model: {} ({}D)",
            self.config.model_name.name(),
            self.config.model_name.dimension()
        );

        Ok(())
    }

    // Generate embeddings for a batch of texts
    pub async fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Embedding>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }

        // Ensure model is loaded
        self.ensure_model_loaded().await?;

        let mut model_guard = self.model.write().await;
        let model = model_guard.as_mut().context("Model not initialized")?;

        // Convert texts to owned strings for fastembed
        let text_strings: Vec<String> = texts.iter().map(|s| s.to_string()).collect();

        // Generate embeddings
        let embedding_vectors = model
            .embed(text_strings, Some(self.config.batch_size))
            .context("Failed to generate embeddings")?;

        // Convert to our Embedding type
        let model_name = self.config.model_name.name().to_string();
        let dimension = self.config.model_name.dimension();

        let embeddings = embedding_vectors
            .into_iter()
            .map(|vec| Embedding::new(vec, model_name.clone(), dimension))
            .collect();

        Ok(embeddings)
    }

    // Generate a single embedding
    pub async fn embed(&self, text: &str) -> Result<Embedding> {
        let mut embeddings = self.embed_batch(&[text]).await?;
        embeddings.pop().context("Failed to generate embedding")
    }

    // Get the model configuration
    pub fn config(&self) -> &EmbeddingConfig {
        &self.config
    }

    // Get the embedding dimension for the current model
    pub fn dimension(&self) -> usize {
        self.config.model_name.dimension()
    }

    // Get the model name
    pub fn model_name(&self) -> &str {
        self.config.model_name.name()
    }
}

// ============================================================================
// Statistics
// ============================================================================

// Statistics about embedding operations
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EmbeddingStats {
    // Total number of embeddings generated
    pub total_embeddings: usize,

    // Total number of texts processed
    pub total_texts: usize,

    // Average batch size
    pub avg_batch_size: f64,

    // Model name used
    pub model_name: String,

    // Embedding dimension
    pub dimension: usize,
}

impl EmbeddingStats {
    // Create new stats for a model
    pub fn new(model_name: String, dimension: usize) -> Self {
        Self {
            total_embeddings: 0,
            total_texts: 0,
            avg_batch_size: 0.0,
            model_name,
            dimension,
        }
    }

    // Record a batch of embeddings
    pub fn record_batch(&mut self, batch_size: usize) {
        self.total_texts += batch_size;
        self.total_embeddings += batch_size;

        // Update rolling average
        let total_batches = self.total_embeddings as f64 / self.avg_batch_size.max(1.0);
        self.avg_batch_size =
            ((self.avg_batch_size * (total_batches - 1.0)) + batch_size as f64) / total_batches;
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_embedding_serialization() {
        let vector = vec![0.1, 0.2, 0.3, 0.4];
        let embedding = Embedding::new(vector.clone(), "test-model".to_string(), 4);

        // Serialize to bytes
        let bytes = embedding.to_bytes();
        assert_eq!(bytes.len(), 16); // 4 floats * 4 bytes

        // Deserialize back
        let restored = Embedding::from_bytes(&bytes, "test-model".to_string(), 4).unwrap();
        assert_eq!(restored.vector.len(), 4);
        assert_eq!(restored.dimension, 4);

        // Check values are close (floating point comparison)
        for (a, b) in vector.iter().zip(restored.vector.iter()) {
            assert!((a - b).abs() < 0.0001);
        }
    }

    #[test]
    fn test_cosine_similarity() {
        let emb1 = Embedding::new(vec![1.0, 0.0, 0.0], "test".to_string(), 3);
        let emb2 = Embedding::new(vec![1.0, 0.0, 0.0], "test".to_string(), 3);
        let emb3 = Embedding::new(vec![0.0, 1.0, 0.0], "test".to_string(), 3);

        // Identical vectors should have similarity 1.0
        let sim1 = emb1.cosine_similarity(&emb2).unwrap();
        assert!((sim1 - 1.0).abs() < 0.0001);

        // Orthogonal vectors should have similarity 0.0
        let sim2 = emb1.cosine_similarity(&emb3).unwrap();
        assert!(sim2.abs() < 0.0001);
    }

    #[test]
    fn test_model_dimensions() {
        assert_eq!(EmbeddingModelType::BGESmallENV15.dimension(), 384);
        assert_eq!(EmbeddingModelType::BGEBaseENV15.dimension(), 768);
        assert_eq!(EmbeddingModelType::AllMiniLML6V2.dimension(), 384);
    }

    #[test]
    fn test_embedding_config_default() {
        let config = EmbeddingConfig::default();
        assert_eq!(config.model_name, EmbeddingModelType::BGESmallENV15);
        assert_eq!(config.batch_size, 32);
        assert!(config.show_download_progress);
    }

    #[test]
    fn test_embedding_stats() {
        let mut stats = EmbeddingStats::new("test-model".to_string(), 384);
        assert_eq!(stats.total_embeddings, 0);

        stats.record_batch(10);
        assert_eq!(stats.total_embeddings, 10);
        assert_eq!(stats.total_texts, 10);

        stats.record_batch(20);
        assert_eq!(stats.total_embeddings, 30);
        assert_eq!(stats.total_texts, 30);
    }
}
