//! Queue module
//!
//! Provides staged processing pipeline for content from raw input to tagged, searchable knowledge.

pub mod processor;

// Re-export main types
pub use processor::{
    advance_stage, capture_note, capture_thought, capture_todo, enqueue, get_pending_items,
    get_queue_item, get_queue_stats, get_retriable_items, mark_failed, update_analysis,
    AnalysisResult, FileAnalysisResult, LlmAnalyzer, ProcessorConfig, QueueProcessor, QueueStats,
};
