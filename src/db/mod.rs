//! Database module
//!
//! Provides database operations for notes, repositories, tasks, and queue system.

pub mod chunks;
pub mod config;
pub mod core;
pub mod documents;
pub mod queue;
pub mod scan_events;

// Re-export chunk store types and functions
pub use chunks::{
    chunk_to_location, chunk_to_record, chunks_to_records, estimate_llm_cost_for_file,
    ChunkLocationRecord, ChunkRecord, ChunkStore, CrossRepoDuplicate, DedupStats, SavingsSummary,
    ScanSavingsRecord, StoredChunk, StoredLocation, StoredSavingsRecord,
};

// Re-export configuration types and functions
pub use config::{
    backup_database, ensure_data_dir, get_backup_path, get_data_dir, health_check, init_pool,
    print_env_help, DatabaseConfig, DatabaseHealth,
};

// Convenience type alias — consumers can use `db::PgPool` instead of `sqlx::PgPool`
pub use sqlx::PgPool;

// Re-export core database types and functions
pub use core::*;

// Re-export queue types and functions
pub use queue::{
    create_queue_tables, FileAnalysis, QueueItem, QueuePriority, QueueSource, QueueStage,
    RepoCache, GITHUB_USERNAME,
};

// Re-export document types and functions
pub use documents::{
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
    Idea,
    Tag,
};
