//! Error types for the audit service

use std::io;
use std::path::PathBuf;
use thiserror::Error;

/// Result type alias for audit operations
pub type Result<T> = std::result::Result<T, AuditError>;

/// Main error type for audit operations
#[derive(Error, Debug)]
pub enum AuditError {
    /// Git-related errors
    #[error("Git error: {0}")]
    Git(#[from] git2::Error),

    /// I/O errors
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),

    /// HTTP/Network errors
    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),

    /// JSON serialization errors
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    /// Configuration errors
    #[error("Configuration error: {0}")]
    Config(String),

    /// LLM API errors
    #[error("LLM API error: {0}")]
    LlmApi(String),

    /// File not found
    #[error("File not found: {0}")]
    FileNotFound(PathBuf),

    /// Invalid file path
    #[error("Invalid file path: {0}")]
    InvalidPath(PathBuf),

    /// Repository not found
    #[error("Repository not found: {0}")]
    RepositoryNotFound(String),

    /// Invalid repository
    #[error("Invalid repository: {0}")]
    InvalidRepository(String),

    /// Parse error
    #[error("Parse error in {file}: {message}")]
    Parse { file: PathBuf, message: String },

    /// Tag validation error
    #[error("Invalid tag: {0}")]
    InvalidTag(String),

    /// Task generation error
    #[error("Task generation failed: {0}")]
    TaskGeneration(String),

    /// Rate limit exceeded
    #[error("Rate limit exceeded for LLM API")]
    RateLimitExceeded,

    /// Invalid API key
    #[error("Invalid or missing API key for {service}")]
    InvalidApiKey { service: String },

    /// Timeout error
    #[error("Operation timed out: {0}")]
    Timeout(String),

    /// Generic error with context
    #[error("{context}: {source}")]
    WithContext {
        context: String,
        source: Box<AuditError>,
    },

    /// Generic error
    #[error("{0}")]
    Other(String),
}

impl AuditError {
    /// Add context to an error
    pub fn context(self, context: impl Into<String>) -> Self {
        AuditError::WithContext {
            context: context.into(),
            source: Box::new(self),
        }
    }

    /// Create a config error
    pub fn config(msg: impl Into<String>) -> Self {
        AuditError::Config(msg.into())
    }

    /// Create an LLM API error
    pub fn llm_api(msg: impl Into<String>) -> Self {
        AuditError::LlmApi(msg.into())
    }

    /// Create a generic error
    pub fn other(msg: impl Into<String>) -> Self {
        AuditError::Other(msg.into())
    }
}

/// Extension trait for adding context to Results
pub trait ResultExt<T> {
    /// Add context to an error result
    fn context(self, context: impl Into<String>) -> Result<T>;
}

impl<T> ResultExt<T> for Result<T> {
    fn context(self, context: impl Into<String>) -> Result<T> {
        self.map_err(|e| e.context(context))
    }
}
