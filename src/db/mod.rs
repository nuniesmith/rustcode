// Database module
//
// Provides database operations for notes, repositories, tasks, and queue system.

pub mod chunks;
pub mod config;
pub mod core;
pub mod documents;
pub mod queue;
pub mod scan_events;

// Re-export chunk store types and functions.
// NOTE: chunk_to_record, chunk_to_location, chunks_to_records live in the
// root crate's src/code_chunker.rs — they depend on CodeChunk which is a
// root-crate type and cannot be re-exported from here without a circular dep.
pub use chunks::{
    ChunkLocationRecord, ChunkRecord, ChunkStore, CrossRepoDuplicate, DedupStats, SavingsSummary,
    ScanSavingsRecord, StoredChunk, StoredLocation, StoredSavingsRecord,
    estimate_llm_cost_for_file,
};

// Re-export configuration types and functions
pub use config::{
    DatabaseConfig, DatabaseHealth, backup_database, ensure_data_dir, get_backup_path,
    get_data_dir, health_check, init_pool, print_env_help,
};

// Convenience type alias — consumers can use `db::PgPool` instead of `sqlx::PgPool`
pub use sqlx::PgPool;

// Re-export core database types and functions
pub use core::*;

// Re-export queue types and functions
pub use queue::{
    FileAnalysis, GITHUB_USERNAME, QueueItem, QueuePriority, QueueSource, QueueStage, RepoCache,
    create_queue_tables,
};

// Re-export document types and functions
pub use documents::{
    Idea,
    Tag,
    count_documents,
    count_documents_by_type,
    // Ideas functions
    count_ideas,
    create_chunks,
    create_document,
    create_idea,
    delete_document,
    delete_document_chunks,
    delete_document_embeddings,
    delete_idea,
    get_all_embeddings,
    get_document,
    get_document_chunks,
    get_document_embeddings,
    get_document_tags,
    get_unindexed_documents,
    list_documents,
    list_ideas,
    // Tags functions
    list_tags,
    mark_document_indexed,
    // FTS5 search
    search_documents,
    search_documents_by_tags,
    search_documents_by_title,
    search_tags,
    set_document_pinned,
    store_embedding,
    update_document,
    update_idea_status,
};
