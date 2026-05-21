// LLM module
//
// Provides LLM integration for code analysis and content processing.

pub mod compat;
pub mod config;
pub mod grok;
pub mod grok_client;
pub mod grok_reasoning;
pub mod ollama;
pub mod router;
pub mod simple_client;
pub mod usage;

// Re-export main types
pub use grok::{
    GrokAnalyzer, ProjectPhase, ProjectPlan, StandardizationIssue, StandardizationReport,
    TodoAnalysis,
};

// Re-export compatibility types
pub use compat::{FileAuditResult, LlmAnalysisResult, LlmClient};

// Re-export simple client for research system
pub use simple_client::GrokClient;
