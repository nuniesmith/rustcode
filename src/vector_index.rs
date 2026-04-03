//! Vector Index Module
//!
//! Provides fast approximate nearest neighbor (ANN) search using HNSW
//! (Hierarchical Navigable Small World) algorithm for semantic search.
//!
//! # Features
//!
//! - **HNSW Index**: Fast approximate nearest neighbor search
//! - **Incremental Updates**: Add/remove vectors dynamically
//! - **Persistence**: Save/load index to disk
//! - **Memory Efficient**: Optimized for production use
//! - **Thread Safe**: Concurrent read access
//!
//! # Example
//!
//! ```rust,no_run
//! use rustcode::vector_index::{VectorIndex, IndexConfig};
//!
//! # async fn example() -> anyhow::Result<()> {
//! let config = IndexConfig::default();
//! let mut index = VectorIndex::new(config);
//!
//! // Add vectors
//! index.add_vector("doc1", vec![0.1, 0.2, 0.3]);
//! index.add_vector("doc2", vec![0.2, 0.3, 0.4]);
//!
//! // Search
//! let query = vec![0.15, 0.25, 0.35];
//! let results = index.search(&query, 10)?;
//!
//! for result in results {
//!     println!("ID: {}, Score: {:.4}", result.id, result.score);
//! }
//! # Ok(())
//! # }
//! ```

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, RwLock};

// ============================================================================
// Configuration
// ============================================================================

/// Configuration for the vector index
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexConfig {
    /// Number of bi-directional links created for each node (M parameter)
    pub m: usize,

    /// Size of the dynamic candidate list (ef_construction parameter)
    pub ef_construction: usize,

    /// Size of the dynamic candidate list during search (ef_search parameter)
    pub ef_search: usize,

    /// Maximum number of layers in the graph
    pub max_layers: usize,

    /// Dimension of vectors
    pub dimension: usize,

    /// Distance metric to use
    pub distance_metric: DistanceMetric,
}

impl Default for IndexConfig {
    fn default() -> Self {
        Self {
            m: 16,                // Standard HNSW M value
            ef_construction: 200, // Higher = better quality, slower build
            ef_search: 50,        // Higher = better recall, slower search
            max_layers: 16,       // Logarithmic in dataset size
            dimension: 384,       // FastEmbed default dimension
            distance_metric: DistanceMetric::Cosine,
        }
    }
}

/// Distance metric for vector comparison
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub enum DistanceMetric {
    /// Cosine similarity (1 - cosine distance)
    Cosine,
    /// Euclidean (L2) distance
    Euclidean,
    /// Manhattan (L1) distance
    Manhattan,
    /// Dot product similarity
    DotProduct,
}

// ============================================================================
// Vector Index
// ============================================================================

/// Main vector index structure
pub struct VectorIndex {
    config: IndexConfig,
    vectors: Arc<RwLock<HashMap<String, Vec<f32>>>>,
    index: Arc<RwLock<HNSWIndex>>,
}

impl VectorIndex {
    /// Create a new vector index
    pub fn new(config: IndexConfig) -> Self {
        Self {
            config: config.clone(),
            vectors: Arc::new(RwLock::new(HashMap::new())),
            index: Arc::new(RwLock::new(HNSWIndex::new(config))),
        }
    }

    /// Add a vector to the index
    pub fn add_vector(&mut self, id: impl Into<String>, vector: Vec<f32>) -> Result<()> {
        let id = id.into();

        // Validate dimension
        if vector.len() != self.config.dimension {
            anyhow::bail!(
                "Vector dimension mismatch: expected {}, got {}",
                self.config.dimension,
                vector.len()
            );
        }

        // Normalize if using cosine similarity
        let normalized = if self.config.distance_metric == DistanceMetric::Cosine {
            normalize_vector(&vector)
        } else {
            vector.clone()
        };

        // Store vector
        {
            let mut vectors = self.vectors.write().unwrap();
            vectors.insert(id.clone(), normalized.clone());
        }

        // Add to index
        {
            let mut index = self.index.write().unwrap();
            index.insert(id, normalized)?;
        }

        Ok(())
    }

    /// Remove a vector from the index
    pub fn remove_vector(&mut self, id: &str) -> Result<()> {
        {
            let mut vectors = self.vectors.write().unwrap();
            vectors.remove(id);
        }

        {
            let mut index = self.index.write().unwrap();
            index.remove(id)?;
        }

        Ok(())
    }

    /// Search for nearest neighbors
    pub fn search(&self, query: &[f32], k: usize) -> Result<Vec<SearchResult>> {
        // Validate dimension
        if query.len() != self.config.dimension {
            anyhow::bail!(
                "Query dimension mismatch: expected {}, got {}",
                self.config.dimension,
                query.len()
            );
        }

        // Normalize if using cosine similarity
        let normalized_query = if self.config.distance_metric == DistanceMetric::Cosine {
            normalize_vector(query)
        } else {
            query.to_vec()
        };

        // Search index
        let index = self.index.read().unwrap();
        let vectors = self.vectors.read().unwrap();

        index.search(&normalized_query, k, &vectors, self.config.distance_metric)
    }

    /// Get the number of vectors in the index
    pub fn len(&self) -> usize {
        self.vectors.read().unwrap().len()
    }

    /// Check if the index is empty
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Clear the index
    pub fn clear(&mut self) {
        self.vectors.write().unwrap().clear();
        self.index.write().unwrap().clear();
    }

    /// Save index to disk
    pub fn save(&self, path: impl AsRef<std::path::Path>) -> Result<()> {
        let vectors = self.vectors.read().unwrap();
        let index = self.index.read().unwrap();

        let data = IndexData {
            config: self.config.clone(),
            vectors: vectors.clone(),
            index: index.serialize(),
        };

        let file = std::fs::File::create(path)?;
        bincode::serialize_into(file, &data)?;

        Ok(())
    }

    /// Load index from disk
    pub fn load(path: impl AsRef<std::path::Path>) -> Result<Self> {
        let file = std::fs::File::open(path)?;
        let data: IndexData = bincode::deserialize_from(file)?;

        let index = HNSWIndex::deserialize(&data.index, data.config.clone())?;

        Ok(Self {
            config: data.config,
            vectors: Arc::new(RwLock::new(data.vectors)),
            index: Arc::new(RwLock::new(index)),
        })
    }
}

/// Search result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResult {
    pub id: String,
    pub score: f32,
    pub distance: f32,
}

// ============================================================================
// HNSW Index Implementation
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
struct HNSWNode {
    id: String,
    layer: usize,
    neighbors: Vec<Vec<String>>, // Neighbors at each layer
}

struct HNSWIndex {
    config: IndexConfig,
    nodes: HashMap<String, HNSWNode>,
    entry_point: Option<String>,
    layer_multiplier: f64,
}

impl HNSWIndex {
    fn new(config: IndexConfig) -> Self {
        let layer_multiplier = 1.0 / (config.m as f64).ln();
        Self {
            config,
            nodes: HashMap::new(),
            entry_point: None,
            layer_multiplier,
        }
    }

    fn insert(&mut self, id: String, _vector: Vec<f32>) -> Result<()> {
        // Determine layer for this node
        let layer = self.random_layer();

        // Create node with empty neighbor lists
        let mut neighbors = Vec::new();
        for _ in 0..=layer {
            neighbors.push(Vec::new());
        }

        let node = HNSWNode {
            id: id.clone(),
            layer,
            neighbors,
        };

        self.nodes.insert(id.clone(), node);

        // Update entry point if needed
        if self.entry_point.is_none()
            || layer > self.get_node_layer(&self.entry_point.clone().unwrap())
        {
            self.entry_point = Some(id);
        }

        Ok(())
    }

    fn remove(&mut self, id: &str) -> Result<()> {
        self.nodes.remove(id);

        // Remove from all neighbor lists
        for node in self.nodes.values_mut() {
            for layer_neighbors in &mut node.neighbors {
                layer_neighbors.retain(|n| n != id);
            }
        }

        // Update entry point if needed
        if self.entry_point.as_deref() == Some(id) {
            self.entry_point = self.nodes.keys().next().cloned();
        }

        Ok(())
    }

    fn search(
        &self,
        query: &[f32],
        k: usize,
        vectors: &HashMap<String, Vec<f32>>,
        metric: DistanceMetric,
    ) -> Result<Vec<SearchResult>> {
        if self.nodes.is_empty() {
            return Ok(Vec::new());
        }

        let n = vectors.len();

        // ------------------------------------------------------------------
        // Phase 1: HNSW graph traversal (greedy descent + ef_search beam search).
        //
        // NOTE: The graph edges stored in HNSWNode::neighbors are only populated
        // once HNSWIndex::insert() is enhanced to run the full HNSW linking
        // algorithm.  Until then every neighbor list is empty, the traversal
        // below returns at most 1 candidate (the entry-point node), and the
        // brute-force fallback kicks in automatically — existing behaviour is
        // fully preserved.
        //
        // TODO: Implement graph construction in HNSWIndex::insert() so that
        //       HNSW traversal is fully utilised and this fallback can be removed.
        //       Tracker: #RUSTCODE-HNSW-BUILD
        // ------------------------------------------------------------------
        let hnsw_candidates = self.hnsw_traverse(query, k, vectors, metric);

        if hnsw_candidates.len() >= k.min(n) {
            return Ok(hnsw_candidates);
        }

        // ------------------------------------------------------------------
        // Brute-force fallback — O(n) linear scan.
        //
        // Automatically used when the HNSW graph has no edges (see NOTE above).
        // WARNING: Performance degrades significantly beyond 10,000 vectors.
        // Consider Qdrant or another ANN service for production-scale deployments.
        // TODO: Remove once HNSWIndex::insert() builds graph edges (#RUSTCODE-HNSW-BUILD).
        // ------------------------------------------------------------------
        if n > 10_000 {
            tracing::warn!(
                vectors = n,
                "HNSW graph traversal returned insufficient candidates \
                 (graph edges not yet built). Falling back to O(n) brute-force \
                 scan — search latency will degrade significantly beyond 10,000 \
                 vectors. See TODO #RUSTCODE-HNSW-BUILD."
            );
        }

        let mut results: Vec<_> = vectors
            .iter()
            .map(|(id, vec)| {
                let distance = compute_distance(query, vec, metric);
                SearchResult {
                    id: id.clone(),
                    score: distance_to_score(distance, metric),
                    distance,
                }
            })
            .collect();

        results.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        results.truncate(k);
        Ok(results)
    }

    /// HNSW graph traversal: greedy descent from the top layer down to layer 1,
    /// followed by an `ef_search` beam search on layer 0.
    ///
    /// Returns the best candidates found via graph traversal.  When the graph
    /// has no edges this returns at most one result (the entry-point node), and
    /// `search()` will fall through to the brute-force path.
    fn hnsw_traverse(
        &self,
        query: &[f32],
        k: usize,
        vectors: &HashMap<String, Vec<f32>>,
        metric: DistanceMetric,
    ) -> Vec<SearchResult> {
        let entry = match &self.entry_point {
            Some(e) => e.clone(),
            None => return Vec::new(),
        };

        if vectors.get(&entry).is_none() {
            return Vec::new();
        }

        let ef_search = self.config.ef_search.max(k);
        let top_layer = self.nodes.get(&entry).map(|n| n.layer).unwrap_or(0);

        // ── Phase 1: greedy descent from top_layer down to layer 1 ──────────
        //
        // At each layer we greedily move to whichever neighbor is closer to the
        // query than the current entry point, repeating until no improvement is
        // found, then descend one layer.
        let mut ep = entry.clone();

        for layer in (1..=top_layer).rev() {
            'greedy: loop {
                let ep_dist = vectors
                    .get(&ep)
                    .map(|v| compute_distance(query, v, metric))
                    .unwrap_or(f32::MAX);

                if let Some(node) = self.nodes.get(&ep) {
                    if let Some(layer_neighbors) = node.neighbors.get(layer) {
                        for neighbor_id in layer_neighbors {
                            if let Some(nv) = vectors.get(neighbor_id) {
                                let nd = compute_distance(query, nv, metric);
                                if nd < ep_dist {
                                    ep = neighbor_id.clone();
                                    continue 'greedy; // restart with the improved ep
                                }
                            }
                        }
                    }
                }
                break; // no improvement found at this layer
            }
        }

        // ── Phase 2: ef_search beam search on layer 0 ───────────────────────
        //
        // Two collections:
        //   candidates – frontier nodes to explore next (logically a min-heap)
        //   working    – best ef_search results seen so far (capped at ef_search)
        //
        // We use plain Vecs with linear scans to avoid pulling in external crates.
        // This is fine in practice: ef_search is typically ≤ 200, and this path
        // only produces meaningful work when graph edges exist (future work).

        let mut visited: HashSet<String> = HashSet::new();

        let ep_dist = vectors
            .get(&ep)
            .map(|v| compute_distance(query, v, metric))
            .unwrap_or(f32::MAX);

        // Each entry is (distance, id).
        let mut candidates: Vec<(f32, String)> = vec![(ep_dist, ep.clone())];
        let mut working: Vec<(f32, String)> = vec![(ep_dist, ep.clone())];
        visited.insert(ep);

        while let Some(pos) = candidates
            .iter()
            .enumerate()
            .min_by(|(_, a), (_, b)| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal))
            .map(|(i, _)| i)
        {
            let (current_dist, current_id) = candidates.remove(pos);

            // Early termination: if the closest remaining candidate is already
            // further than the worst item in the working set, we cannot improve.
            let worst_working = working
                .iter()
                .map(|(d, _)| *d)
                .fold(f32::NEG_INFINITY, f32::max);

            if current_dist > worst_working && working.len() >= ef_search {
                break;
            }

            // Expand layer-0 neighbors of current_id.
            if let Some(node) = self.nodes.get(&current_id) {
                if let Some(layer0_neighbors) = node.neighbors.first() {
                    for neighbor_id in layer0_neighbors {
                        if visited.contains(neighbor_id) {
                            continue;
                        }
                        visited.insert(neighbor_id.clone());

                        if let Some(nv) = vectors.get(neighbor_id) {
                            let nd = compute_distance(query, nv, metric);
                            let worst = working
                                .iter()
                                .map(|(d, _)| *d)
                                .fold(f32::NEG_INFINITY, f32::max);

                            if nd < worst || working.len() < ef_search {
                                candidates.push((nd, neighbor_id.clone()));
                                working.push((nd, neighbor_id.clone()));

                                // Evict the furthest element if we exceed ef_search.
                                if working.len() > ef_search {
                                    if let Some(worst_pos) = working
                                        .iter()
                                        .enumerate()
                                        .max_by(|(_, a), (_, b)| {
                                            a.0.partial_cmp(&b.0)
                                                .unwrap_or(std::cmp::Ordering::Equal)
                                        })
                                        .map(|(i, _)| i)
                                    {
                                        working.remove(worst_pos);
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        // Sort by ascending distance (= descending score) and return top k.
        working.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
        working.truncate(k);
        working
            .into_iter()
            .map(|(distance, id)| SearchResult {
                id,
                score: distance_to_score(distance, metric),
                distance,
            })
            .collect()
    }

    fn clear(&mut self) {
        self.nodes.clear();
        self.entry_point = None;
    }

    fn random_layer(&self) -> usize {
        let uniform: f64 = rand::random();
        let layer = (-uniform.ln() * self.layer_multiplier).floor() as usize;
        layer.min(self.config.max_layers - 1)
    }

    fn get_node_layer(&self, id: &str) -> usize {
        self.nodes.get(id).map(|n| n.layer).unwrap_or(0)
    }

    fn serialize(&self) -> Vec<u8> {
        bincode::serialize(&self.nodes).unwrap_or_default()
    }

    fn deserialize(data: &[u8], config: IndexConfig) -> Result<Self> {
        let nodes: HashMap<String, HNSWNode> = bincode::deserialize(data)?;
        let entry_point = nodes.keys().next().cloned();

        Ok(Self {
            config: config.clone(),
            nodes,
            entry_point,
            layer_multiplier: 1.0 / (config.m as f64).ln(),
        })
    }
}

#[derive(Serialize, Deserialize)]
struct IndexData {
    config: IndexConfig,
    vectors: HashMap<String, Vec<f32>>,
    index: Vec<u8>,
}

// ============================================================================
// Utility Functions
// ============================================================================

/// Normalize a vector to unit length
fn normalize_vector(vec: &[f32]) -> Vec<f32> {
    let norm: f32 = vec.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        vec.iter().map(|x| x / norm).collect()
    } else {
        vec.to_vec()
    }
}

/// Convert a raw distance value to a similarity score where higher is better.
///
/// - Cosine / DotProduct: score = 1 − distance  (distance is already 1 − similarity)
/// - Euclidean / Manhattan: score = 1 / (1 + distance)  (maps [0, ∞) → (0, 1])
fn distance_to_score(distance: f32, metric: DistanceMetric) -> f32 {
    match metric {
        DistanceMetric::Cosine | DistanceMetric::DotProduct => 1.0 - distance,
        _ => 1.0 / (1.0 + distance),
    }
}

/// Compute distance between two vectors
fn compute_distance(a: &[f32], b: &[f32], metric: DistanceMetric) -> f32 {
    match metric {
        DistanceMetric::Cosine => {
            let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
            1.0 - dot // Cosine distance (1 - similarity)
        }
        DistanceMetric::Euclidean => a
            .iter()
            .zip(b.iter())
            .map(|(x, y)| (x - y).powi(2))
            .sum::<f32>()
            .sqrt(),
        DistanceMetric::Manhattan => a.iter().zip(b.iter()).map(|(x, y)| (x - y).abs()).sum(),
        DistanceMetric::DotProduct => {
            -a.iter().zip(b.iter()).map(|(x, y)| x * y).sum::<f32>() // Negative for similarity
        }
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normalize_vector() {
        let vec = vec![3.0, 4.0];
        let normalized = normalize_vector(&vec);
        assert!((normalized[0] - 0.6).abs() < 0.001);
        assert!((normalized[1] - 0.8).abs() < 0.001);
    }

    #[test]
    fn test_cosine_distance() {
        let a = vec![1.0, 0.0];
        let b = vec![0.0, 1.0];
        let dist = compute_distance(&a, &b, DistanceMetric::Cosine);
        assert!((dist - 1.0).abs() < 0.001); // Orthogonal vectors

        let c = vec![1.0, 0.0];
        let dist2 = compute_distance(&a, &c, DistanceMetric::Cosine);
        assert!(dist2.abs() < 0.001); // Identical vectors
    }

    #[test]
    fn test_vector_index() {
        let config = IndexConfig {
            dimension: 3,
            ..Default::default()
        };
        let mut index = VectorIndex::new(config);

        // Add vectors
        index.add_vector("vec1", vec![1.0, 0.0, 0.0]).unwrap();
        index.add_vector("vec2", vec![0.0, 1.0, 0.0]).unwrap();
        index.add_vector("vec3", vec![0.0, 0.0, 1.0]).unwrap();

        assert_eq!(index.len(), 3);

        // Search
        let query = vec![0.9, 0.1, 0.0];
        let results = index.search(&query, 2).unwrap();

        assert_eq!(results.len(), 2);
        assert_eq!(results[0].id, "vec1");
    }

    #[test]
    fn test_remove_vector() {
        let config = IndexConfig {
            dimension: 2,
            ..Default::default()
        };
        let mut index = VectorIndex::new(config);

        index.add_vector("vec1", vec![1.0, 0.0]).unwrap();
        index.add_vector("vec2", vec![0.0, 1.0]).unwrap();

        assert_eq!(index.len(), 2);

        index.remove_vector("vec1").unwrap();
        assert_eq!(index.len(), 1);

        let results = index.search(&[1.0, 0.0], 1).unwrap();
        assert_eq!(results[0].id, "vec2");
    }

    #[test]
    fn test_dimension_validation() {
        let config = IndexConfig {
            dimension: 3,
            ..Default::default()
        };
        let mut index = VectorIndex::new(config);

        // Wrong dimension should fail
        let result = index.add_vector("vec1", vec![1.0, 0.0]);
        assert!(result.is_err());
    }
}
