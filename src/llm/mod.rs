//! LLM module
//!
//! Provides LLM integration for code analysis and content processing.

pub mod compat;
pub mod grok;
pub mod simple_client;

// Re-export main types
pub use grok::{
    GrokAnalyzer, ProjectPhase, ProjectPlan, StandardizationIssue, StandardizationReport,
    TodoAnalysis,
};

// Re-export compatibility types
pub use compat::{FileAuditResult, LlmAnalysisResult, LlmClient};

// Re-export simple client for research system
pub use simple_client::GrokClient;
