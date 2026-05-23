// RC-EXTRACT-A slice 1 (2026-05-23): semantic-indexing primitives extracted
// from the rustcode crate. This first slice hosts the two dependency-leaf
// modules — chunking (text/code splitting) and the in-memory vector index
// (cosine-similarity nearest-neighbor lookup).
//
// Slice 2+ will move `embeddings`, `indexing`, and `search` here once
// the `fastembed` / ort-sys dep landed in this crate and a `Storage` trait
// is introduced so the indexing pipeline doesn't reach back into
// `rustcode::db`.

pub mod chunking;
pub mod code_chunker;
pub mod embeddings;
pub mod file_language;
pub mod vector_index;

pub use chunking::{ChunkConfig, ChunkData, chunk_document};
pub use embeddings::{
    Embedding, EmbeddingConfig, EmbeddingGenerator, EmbeddingModelType, EmbeddingStats,
};
pub use file_language::FileLanguage;
pub use vector_index::{
    DistanceMetric, IndexConfig, SearchResult, VectorIndex,
};
