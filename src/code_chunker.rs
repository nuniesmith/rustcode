// CodeChunker: the bulk logic now lives in `rag::code_chunker` (extracted
// 2026-05-23 as RC-EXTRACT-A slice 4). This file is a thin shim that:
//   1. Re-exports the moved types so historical paths
//      (`crate::code_chunker::CodeChunk`, etc.) keep working for in-tree
//      callers and the `rustcode::lib.rs` re-exports.
//   2. Hosts the three `CodeChunk → ChunkRecord` conversion helpers
//      which can't live in `rag` (they would force `rag` to depend on
//      `rustcode::db`, creating a circular crate dependency).

pub use rag::code_chunker::*;

// ---------------------------------------------------------------------------
// Conversion helpers: CodeChunk → rustcode-db record types
//
// These live here (not in rustcode-db/src/db/chunks.rs) because CodeChunk is
// defined in `rag::code_chunker`. Putting them in rustcode-db would require
// rustcode-db to depend on the root crate — a circular dependency. Equally,
// putting them in `rag` would require `rag` to depend on `rustcode::db` —
// also circular. So they sit here, at the rustcode crate level, where both
// `rag::CodeChunk` and `crate::db::chunks::Chunk*Record` are visible.
// ---------------------------------------------------------------------------

use crate::db::chunks::{ChunkLocationRecord, ChunkRecord};

// Convert a [`CodeChunk`] into a [`ChunkRecord`] for persistence.
pub fn chunk_to_record(chunk: &CodeChunk) -> ChunkRecord {
    ChunkRecord {
        content_hash: chunk.content_hash.clone(),
        entity_type: chunk.entity_type.to_string(),
        entity_name: chunk.entity_name.clone(),
        language: chunk.language.to_string(),
        word_count: chunk.word_count as i64,
        complexity_score: chunk.complexity_score as i64,
        is_public: chunk.is_public,
        has_tests: chunk.has_tests,
        is_test_code: chunk.is_test_code,
        issue_count: chunk.issue_count as i64,
        embedding: if chunk.vector.is_empty() {
            None
        } else {
            Some(serde_json::to_string(&chunk.vector).unwrap_or_default())
        },
    }
}

// Convert a [`CodeChunk`] into a [`ChunkLocationRecord`].
pub fn chunk_to_location(chunk: &CodeChunk) -> ChunkLocationRecord {
    ChunkLocationRecord {
        content_hash: chunk.content_hash.clone(),
        repo_id: chunk.repo_id.clone(),
        file_path: chunk.file_path.clone(),
        start_line: chunk.start_line as i64,
        end_line: chunk.end_line as i64,
        entity_name: chunk.entity_name.clone(),
    }
}

// Convert a batch of [`CodeChunk`]s into paired record + location vecs.
pub fn chunks_to_records(chunks: &[CodeChunk]) -> (Vec<ChunkRecord>, Vec<ChunkLocationRecord>) {
    let mut records = Vec::with_capacity(chunks.len());
    let mut locations = Vec::with_capacity(chunks.len());
    for chunk in chunks {
        records.push(chunk_to_record(chunk));
        locations.push(chunk_to_location(chunk));
    }
    (records, locations)
}
