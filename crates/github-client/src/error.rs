use thiserror::Error;

#[derive(Error, Debug)]
pub enum GitHubError {
    #[error("GitHub API error: {0}")]
    ApiError(String),

    #[error("Authentication failed: {0}")]
    AuthError(String),

    #[error("Rate limit exceeded. Resets at: {reset_at}")]
    RateLimitExceeded {
        reset_at: chrono::DateTime<chrono::Utc>,
    },

    #[error("Resource not found: {resource_type} with id {id}")]
    NotFound { resource_type: String, id: String },

    #[error("Invalid configuration: {0}")]
    ConfigError(String),

    #[error("Network error: {0}")]
    NetworkError(#[from] reqwest::Error),

    #[error("Database error: {0}")]
    DatabaseError(#[from] sqlx::Error),

    #[error("Serialization error: {0}")]
    SerializationError(#[from] serde_json::Error),

    #[error("Invalid GitHub URL: {0}")]
    InvalidUrl(String),

    #[error("Webhook verification failed")]
    WebhookVerificationFailed,
}

pub type Result<T> = std::result::Result<T, GitHubError>;
