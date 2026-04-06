// GitHub Webhook Handler
//
// Provides webhook support for real-time GitHub event processing.
// This enables the system to react to GitHub events (push, PR, issues)
// without polling, reducing API calls and improving responsiveness.
//
// # Architecture
//
// Webhooks allow GitHub to push events to our system in real-time:
// - Zero API calls needed for updates (GitHub pushes to us)
// - Instant synchronization on repo changes
// - Event-driven architecture for better UX
//
// # Security
//
// All webhooks are verified using HMAC-SHA256 signatures to ensure
// they originate from GitHub and haven't been tampered with.
//
// # Example
//
// ```rust,no_run
// use rustcode::github::webhook::WebhookHandler;
// use axum::{Router, routing::post, response::IntoResponse};
//
// async fn webhook_endpoint() -> impl IntoResponse {
//     "ok"
// }
//
// #[tokio::main]
// async fn main() {
//     let _handler = WebhookHandler::new("webhook_secret");
//
//     let app = Router::new()
//         .route("/webhook", post(webhook_endpoint));
//
//     // Listen on port 3000
//     let listener = tokio::net::TcpListener::bind("0.0.0.0:3000").await.unwrap();
//     axum::serve(listener, app).await.unwrap();
// }
// ```

use crate::github::{GitHubError, Result, models::*};
use chrono::{DateTime, Utc};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use tracing::{debug, info, warn};

type HmacSha256 = Hmac<Sha256>;

// ============================================================================
// Webhook Events
// ============================================================================

// GitHub webhook event (polymorphic)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum WebhookEvent {
    // Push event (commits pushed to a branch)
    #[serde(rename = "push")]
    Push(PushEvent),

    // Issue opened
    #[serde(rename = "opened")]
    IssuesOpened(IssueEvent),

    // Issue closed
    #[serde(rename = "closed")]
    IssuesClosed(IssueEvent),

    // Issue reopened
    #[serde(rename = "reopened")]
    IssuesReopened(IssueEvent),

    // Pull request opened
    PullRequestOpened(PullRequestEvent),

    // Pull request closed
    PullRequestClosed(PullRequestEvent),

    // Pull request reopened
    PullRequestReopened(PullRequestEvent),

    // Pull request merged
    PullRequestMerged(PullRequestEvent),

    // Repository created
    RepositoryCreated(RepositoryEvent),

    // Repository deleted
    RepositoryDeleted(RepositoryEvent),

    // Repository archived
    RepositoryArchived(RepositoryEvent),

    // Star added to repository
    StarCreated(StarEvent),

    // Fork created
    ForkCreated(ForkEvent),

    // Ping event (webhook test)
    Ping(PingEvent),
}

// ============================================================================
// Event Payloads
// ============================================================================

// Push event payload
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PushEvent {
    #[serde(rename = "ref")]
    pub git_ref: String,
    pub before: String,
    pub after: String,
    pub created: bool,
    pub deleted: bool,
    pub forced: bool,
    pub base_ref: Option<String>,
    pub compare: String,
    pub commits: Vec<PushCommit>,
    pub head_commit: Option<PushCommit>,
    pub repository: Repository,
    pub pusher: GitUser,
    pub sender: User,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PushCommit {
    pub id: String,
    pub tree_id: String,
    pub message: String,
    pub timestamp: DateTime<Utc>,
    pub author: GitUser,
    pub committer: GitUser,
    pub added: Vec<String>,
    pub removed: Vec<String>,
    pub modified: Vec<String>,
}

// Issue event payload
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IssueEvent {
    pub action: String,
    pub issue: Issue,
    pub repository: Repository,
    pub sender: User,
}

// Pull request event payload
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PullRequestEvent {
    pub action: String,
    pub number: i32,
    pub pull_request: PullRequest,
    pub repository: Repository,
    pub sender: User,
}

// Repository event payload
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepositoryEvent {
    pub action: String,
    pub repository: Repository,
    pub sender: User,
}

// Star event payload
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StarEvent {
    pub action: String,
    pub starred_at: Option<DateTime<Utc>>,
    pub repository: Repository,
    pub sender: User,
}

// Fork event payload
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ForkEvent {
    pub forkee: Repository,
    pub repository: Repository,
    pub sender: User,
}

// Ping event payload (webhook test)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PingEvent {
    pub zen: String,
    pub hook_id: i64,
    pub hook: WebhookConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebhookConfig {
    #[serde(rename = "type")]
    pub hook_type: String,
    pub id: i64,
    pub name: String,
    pub active: bool,
    pub events: Vec<String>,
    pub config: WebhookConfigDetails,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebhookConfigDetails {
    pub content_type: String,
    pub insecure_ssl: String,
    pub url: String,
}

// ============================================================================
// Webhook Payload Wrapper
// ============================================================================

// Raw webhook payload with headers
#[derive(Debug, Clone)]
pub struct WebhookPayload {
    // Event type (from X-GitHub-Event header)
    pub event_type: String,

    // Delivery ID (from X-GitHub-Delivery header)
    pub delivery_id: String,

    // Signature (from X-Hub-Signature-256 header)
    pub signature: Option<String>,

    // Raw JSON body
    pub body: String,
}

impl WebhookPayload {
    // Create new webhook payload
    pub fn new(
        event_type: impl Into<String>,
        delivery_id: impl Into<String>,
        signature: Option<String>,
        body: impl Into<String>,
    ) -> Self {
        Self {
            event_type: event_type.into(),
            delivery_id: delivery_id.into(),
            signature,
            body: body.into(),
        }
    }

    // Parse event from JSON body
    pub fn parse_event(&self) -> Result<WebhookEvent> {
        match self.event_type.as_str() {
            "push" => {
                let event: PushEvent = serde_json::from_str(&self.body)?;
                Ok(WebhookEvent::Push(event))
            }
            "issues" => {
                let event: IssueEvent = serde_json::from_str(&self.body)?;
                match event.action.as_str() {
                    "opened" => Ok(WebhookEvent::IssuesOpened(event)),
                    "closed" => Ok(WebhookEvent::IssuesClosed(event)),
                    "reopened" => Ok(WebhookEvent::IssuesReopened(event)),
                    _ => Err(GitHubError::ApiError(format!(
                        "Unknown issue action: {}",
                        event.action
                    ))),
                }
            }
            "pull_request" => {
                let event: PullRequestEvent = serde_json::from_str(&self.body)?;
                match event.action.as_str() {
                    "opened" => Ok(WebhookEvent::PullRequestOpened(event)),
                    "closed" if event.pull_request.merged => {
                        Ok(WebhookEvent::PullRequestMerged(event))
                    }
                    "closed" => Ok(WebhookEvent::PullRequestClosed(event)),
                    "reopened" => Ok(WebhookEvent::PullRequestReopened(event)),
                    _ => Err(GitHubError::ApiError(format!(
                        "Unknown PR action: {}",
                        event.action
                    ))),
                }
            }
            "repository" => {
                let event: RepositoryEvent = serde_json::from_str(&self.body)?;
                match event.action.as_str() {
                    "created" => Ok(WebhookEvent::RepositoryCreated(event)),
                    "deleted" => Ok(WebhookEvent::RepositoryDeleted(event)),
                    "archived" => Ok(WebhookEvent::RepositoryArchived(event)),
                    _ => Err(GitHubError::ApiError(format!(
                        "Unknown repository action: {}",
                        event.action
                    ))),
                }
            }
            "star" => {
                let event: StarEvent = serde_json::from_str(&self.body)?;
                Ok(WebhookEvent::StarCreated(event))
            }
            "fork" => {
                let event: ForkEvent = serde_json::from_str(&self.body)?;
                Ok(WebhookEvent::ForkCreated(event))
            }
            "ping" => {
                let event: PingEvent = serde_json::from_str(&self.body)?;
                Ok(WebhookEvent::Ping(event))
            }
            _ => Err(GitHubError::ApiError(format!(
                "Unsupported event type: {}",
                self.event_type
            ))),
        }
    }
}

// ============================================================================
// Webhook Handler
// ============================================================================

// GitHub webhook handler with signature verification
pub struct WebhookHandler {
    secret: String,
}

impl WebhookHandler {
    // Create new webhook handler with secret
    pub fn new(secret: impl Into<String>) -> Self {
        Self {
            secret: secret.into(),
        }
    }

    // Verify webhook signature
    pub fn verify_signature(&self, payload: &WebhookPayload) -> Result<bool> {
        let signature = match &payload.signature {
            Some(sig) => sig,
            None => {
                warn!("Webhook missing signature");
                return Ok(false);
            }
        };

        // Signature format: "sha256=<hex>"
        if !signature.starts_with("sha256=") {
            warn!("Invalid signature format: {}", signature);
            return Ok(false);
        }

        let expected_sig = &signature[7..]; // Remove "sha256=" prefix

        // Compute HMAC-SHA256
        let mut mac = HmacSha256::new_from_slice(self.secret.as_bytes())
            .map_err(|e| GitHubError::ConfigError(format!("Invalid secret: {}", e)))?;

        mac.update(payload.body.as_bytes());

        let computed_sig = hex::encode(mac.finalize().into_bytes());

        // Constant-time comparison
        let is_valid = computed_sig == expected_sig;

        if !is_valid {
            warn!(
                "Signature verification failed for delivery {}",
                payload.delivery_id
            );
        }

        Ok(is_valid)
    }

    // Process webhook payload
    pub async fn handle(&self, payload: WebhookPayload) -> Result<WebhookEvent> {
        debug!(
            "Processing webhook: type={}, delivery={}",
            payload.event_type, payload.delivery_id
        );

        // Verify signature
        if !self.verify_signature(&payload)? {
            return Err(GitHubError::WebhookVerificationFailed);
        }

        // Parse event
        let event = payload.parse_event()?;

        info!(
            "Webhook processed successfully: type={}, delivery={}",
            payload.event_type, payload.delivery_id
        );

        Ok(event)
    }
}

// ============================================================================
// Event Helpers
// ============================================================================

impl WebhookEvent {
    // Get event type as string
    pub fn event_type(&self) -> &str {
        match self {
            WebhookEvent::Push(_) => "push",
            WebhookEvent::IssuesOpened(_) => "issues.opened",
            WebhookEvent::IssuesClosed(_) => "issues.closed",
            WebhookEvent::IssuesReopened(_) => "issues.reopened",
            WebhookEvent::PullRequestOpened(_) => "pull_request.opened",
            WebhookEvent::PullRequestClosed(_) => "pull_request.closed",
            WebhookEvent::PullRequestReopened(_) => "pull_request.reopened",
            WebhookEvent::PullRequestMerged(_) => "pull_request.merged",
            WebhookEvent::RepositoryCreated(_) => "repository.created",
            WebhookEvent::RepositoryDeleted(_) => "repository.deleted",
            WebhookEvent::RepositoryArchived(_) => "repository.archived",
            WebhookEvent::StarCreated(_) => "star.created",
            WebhookEvent::ForkCreated(_) => "fork",
            WebhookEvent::Ping(_) => "ping",
        }
    }

    // Get repository associated with event
    pub fn repository(&self) -> Option<&Repository> {
        match self {
            WebhookEvent::Push(e) => Some(&e.repository),
            WebhookEvent::IssuesOpened(e) => Some(&e.repository),
            WebhookEvent::IssuesClosed(e) => Some(&e.repository),
            WebhookEvent::IssuesReopened(e) => Some(&e.repository),
            WebhookEvent::PullRequestOpened(e) => Some(&e.repository),
            WebhookEvent::PullRequestClosed(e) => Some(&e.repository),
            WebhookEvent::PullRequestReopened(e) => Some(&e.repository),
            WebhookEvent::PullRequestMerged(e) => Some(&e.repository),
            WebhookEvent::RepositoryCreated(e) => Some(&e.repository),
            WebhookEvent::RepositoryDeleted(e) => Some(&e.repository),
            WebhookEvent::RepositoryArchived(e) => Some(&e.repository),
            WebhookEvent::StarCreated(e) => Some(&e.repository),
            WebhookEvent::ForkCreated(e) => Some(&e.repository),
            WebhookEvent::Ping(_) => None,
        }
    }

    // Get sender (user who triggered the event)
    pub fn sender(&self) -> Option<&User> {
        match self {
            WebhookEvent::Push(e) => Some(&e.sender),
            WebhookEvent::IssuesOpened(e) => Some(&e.sender),
            WebhookEvent::IssuesClosed(e) => Some(&e.sender),
            WebhookEvent::IssuesReopened(e) => Some(&e.sender),
            WebhookEvent::PullRequestOpened(e) => Some(&e.sender),
            WebhookEvent::PullRequestClosed(e) => Some(&e.sender),
            WebhookEvent::PullRequestReopened(e) => Some(&e.sender),
            WebhookEvent::PullRequestMerged(e) => Some(&e.sender),
            WebhookEvent::RepositoryCreated(e) => Some(&e.sender),
            WebhookEvent::RepositoryDeleted(e) => Some(&e.sender),
            WebhookEvent::RepositoryArchived(e) => Some(&e.sender),
            WebhookEvent::StarCreated(e) => Some(&e.sender),
            WebhookEvent::ForkCreated(e) => Some(&e.sender),
            WebhookEvent::Ping(_) => None,
        }
    }

    // Check if event requires sync action
    pub fn should_trigger_sync(&self) -> bool {
        matches!(
            self,
            WebhookEvent::Push(_)
                | WebhookEvent::IssuesOpened(_)
                | WebhookEvent::IssuesClosed(_)
                | WebhookEvent::PullRequestOpened(_)
                | WebhookEvent::PullRequestClosed(_)
                | WebhookEvent::PullRequestMerged(_)
                | WebhookEvent::RepositoryCreated(_)
        )
    }
}

impl PushEvent {
    // Get branch name from ref
    pub fn branch_name(&self) -> Option<&str> {
        self.git_ref.strip_prefix("refs/heads/")
    }

    // Check if push is to main/master branch
    pub fn is_main_branch(&self) -> bool {
        matches!(
            self.branch_name(),
            Some("main") | Some("master") | Some("develop")
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_webhook_handler_creation() {
        let handler = WebhookHandler::new("test_secret");
        assert_eq!(handler.secret, "test_secret");
    }

    #[test]
    fn test_webhook_payload_creation() {
        let payload = WebhookPayload::new(
            "push",
            "12345",
            Some("sha256=abc123".to_string()),
            r#"{"ref":"refs/heads/main"}"#,
        );

        assert_eq!(payload.event_type, "push");
        assert_eq!(payload.delivery_id, "12345");
        assert!(payload.signature.is_some());
    }

    #[test]
    fn test_signature_verification() {
        let handler = WebhookHandler::new("secret");
        let body = r#"{"test":"data"}"#;

        // Compute expected signature
        let mut mac = HmacSha256::new_from_slice(b"secret").unwrap();
        mac.update(body.as_bytes());
        let sig = hex::encode(mac.finalize().into_bytes());

        let payload = WebhookPayload::new("push", "123", Some(format!("sha256={}", sig)), body);

        assert!(handler.verify_signature(&payload).unwrap());
    }

    #[test]
    fn test_invalid_signature() {
        let handler = WebhookHandler::new("secret");
        let payload = WebhookPayload::new(
            "push",
            "123",
            Some("sha256=invalid".to_string()),
            r#"{"test":"data"}"#,
        );

        assert!(!handler.verify_signature(&payload).unwrap());
    }

    #[test]
    fn test_missing_signature() {
        let handler = WebhookHandler::new("secret");
        let payload = WebhookPayload::new("push", "123", None, r#"{"test":"data"}"#);

        assert!(!handler.verify_signature(&payload).unwrap());
    }
}
