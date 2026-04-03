//! Webhook System Module
//!
//! Provides event-driven webhook notifications for system events.
//! Supports HTTP POST webhooks with retry logic, filtering, and signatures.
//!
//! # Features
//!
//! - **Event Types**: Document indexed, search performed, job completed, etc.
//! - **HTTP POST**: JSON payload delivery to configured endpoints
//! - **Retry Logic**: Automatic retry with exponential backoff
//! - **Filtering**: Subscribe to specific event types
//! - **Signatures**: HMAC-SHA256 signatures for verification
//! - **Async Delivery**: Non-blocking webhook execution
//!
//! # Example
//!
//! ```rust,no_run
//! use rustcode::webhooks::{WebhookManager, WebhookConfig, WebhookEvent};
//!
//! # async fn example() -> anyhow::Result<()> {
//! let config = WebhookConfig::default();
//! let manager = WebhookManager::new(config);
//!
//! // Register a webhook
//! manager.register(
//!     "https://example.com/webhook".to_string(),
//!     vec!["document.indexed".to_string()],
//!     Some("secret_key".to_string())
//! ).await?;
//!
//! // Trigger an event
//! manager.trigger(WebhookEvent::DocumentIndexed {
//!     document_id: 123,
//!     chunks: 10,
//!     duration_ms: 1500,
//! }).await?;
//! # Ok(())
//! # }
//! ```

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use uuid::Uuid;

// ============================================================================
// Configuration
// ============================================================================

/// Webhook system configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebhookConfig {
    /// Maximum number of retry attempts
    pub max_retries: u32,

    /// Initial retry delay in milliseconds
    pub initial_retry_delay_ms: u64,

    /// Maximum retry delay in milliseconds
    pub max_retry_delay_ms: u64,

    /// Request timeout in seconds
    pub timeout_seconds: u64,

    /// Maximum number of concurrent deliveries
    pub max_concurrent_deliveries: usize,

    /// Enable webhook signatures (HMAC-SHA256)
    pub enable_signatures: bool,
}

impl Default for WebhookConfig {
    fn default() -> Self {
        Self {
            max_retries: 3,
            initial_retry_delay_ms: 1000,
            max_retry_delay_ms: 60000,
            timeout_seconds: 30,
            max_concurrent_deliveries: 10,
            enable_signatures: true,
        }
    }
}

// ============================================================================
// Webhook Events
// ============================================================================

/// Webhook event types
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event_type", content = "data")]
pub enum WebhookEvent {
    /// Document was uploaded
    DocumentUploaded {
        document_id: i64,
        title: String,
        doc_type: String,
        created_at: DateTime<Utc>,
    },

    /// Document indexing completed
    DocumentIndexed {
        document_id: i64,
        chunks: usize,
        duration_ms: u64,
    },

    /// Document indexing failed
    DocumentIndexingFailed { document_id: i64, error: String },

    /// Search was performed
    SearchPerformed {
        query: String,
        results_count: usize,
        execution_time_ms: u64,
        search_type: String,
    },

    /// Indexing job started
    JobStarted {
        job_id: String,
        document_count: usize,
    },

    /// Indexing job completed
    JobCompleted {
        job_id: String,
        document_count: usize,
        success_count: usize,
        failed_count: usize,
        duration_ms: u64,
    },

    /// Indexing job failed
    JobFailed { job_id: String, error: String },

    /// Document was deleted
    DocumentDeleted { document_id: i64 },

    /// System health check failed
    HealthCheckFailed { service: String, error: String },
}

impl WebhookEvent {
    /// Get the event type as a string
    pub fn event_type(&self) -> &str {
        match self {
            Self::DocumentUploaded { .. } => "document.uploaded",
            Self::DocumentIndexed { .. } => "document.indexed",
            Self::DocumentIndexingFailed { .. } => "document.indexing_failed",
            Self::SearchPerformed { .. } => "search.performed",
            Self::JobStarted { .. } => "job.started",
            Self::JobCompleted { .. } => "job.completed",
            Self::JobFailed { .. } => "job.failed",
            Self::DocumentDeleted { .. } => "document.deleted",
            Self::HealthCheckFailed { .. } => "health.check_failed",
        }
    }
}

// ============================================================================
// Webhook Registration
// ============================================================================

/// Webhook endpoint configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebhookEndpoint {
    pub id: String,
    pub url: String,
    pub event_types: Vec<String>, // Filter for specific events (empty = all)
    pub secret: Option<String>,
    pub enabled: bool,
    pub created_at: DateTime<Utc>,
    pub last_triggered: Option<DateTime<Utc>>,
    pub total_triggers: u64,
    pub failed_triggers: u64,
}

impl WebhookEndpoint {
    pub fn new(url: String, event_types: Vec<String>, secret: Option<String>) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            url,
            event_types,
            secret,
            enabled: true,
            created_at: Utc::now(),
            last_triggered: None,
            total_triggers: 0,
            failed_triggers: 0,
        }
    }

    /// Check if this endpoint should receive the event
    pub fn should_receive(&self, event: &WebhookEvent) -> bool {
        if !self.enabled {
            return false;
        }

        if self.event_types.is_empty() {
            return true; // Receive all events
        }

        self.event_types.contains(&event.event_type().to_string())
    }
}

// ============================================================================
// Webhook Payload
// ============================================================================

/// Webhook HTTP payload
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebhookPayload {
    pub id: String,
    pub event: WebhookEvent,
    pub timestamp: DateTime<Utc>,
    pub signature: Option<String>,
}

impl WebhookPayload {
    pub fn new(event: WebhookEvent, secret: Option<&str>) -> Self {
        let id = Uuid::new_v4().to_string();
        let timestamp = Utc::now();

        let signature = secret.map(|s| {
            let payload = serde_json::to_string(&event).unwrap_or_default();
            Self::compute_signature(&payload, s)
        });

        Self {
            id,
            event,
            timestamp,
            signature,
        }
    }

    /// Compute HMAC-SHA256 signature
    fn compute_signature(payload: &str, secret: &str) -> String {
        use hmac::{Hmac, Mac};
        use sha2::Sha256;

        type HmacSha256 = Hmac<Sha256>;

        let mut mac =
            HmacSha256::new_from_slice(secret.as_bytes()).expect("HMAC can take key of any size");
        mac.update(payload.as_bytes());
        hex::encode(mac.finalize().into_bytes())
    }

    /// Verify payload signature
    pub fn verify_signature(&self, secret: &str) -> bool {
        if let Some(ref sig) = self.signature {
            let payload = serde_json::to_string(&self.event).unwrap_or_default();
            let expected = Self::compute_signature(&payload, secret);
            sig == &expected
        } else {
            false
        }
    }
}

// ============================================================================
// Webhook Delivery
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebhookDelivery {
    pub id: String,
    pub endpoint_id: String,
    pub payload: WebhookPayload,
    pub attempt: u32,
    pub status: DeliveryStatus,
    pub response_code: Option<u16>,
    pub response_body: Option<String>,
    pub error: Option<String>,
    pub created_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum DeliveryStatus {
    Pending,
    InProgress,
    Success,
    Failed,
    Retrying,
}

// ============================================================================
// Webhook Manager
// ============================================================================

pub struct WebhookManager {
    config: WebhookConfig,
    endpoints: Arc<RwLock<HashMap<String, WebhookEndpoint>>>,
    deliveries: Arc<RwLock<Vec<WebhookDelivery>>>,
    client: reqwest::Client,
}

impl WebhookManager {
    /// Create a new webhook manager
    pub fn new(config: WebhookConfig) -> Self {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(config.timeout_seconds))
            .build()
            .expect("Failed to create HTTP client");

        Self {
            config,
            endpoints: Arc::new(RwLock::new(HashMap::new())),
            deliveries: Arc::new(RwLock::new(Vec::new())),
            client,
        }
    }

    /// Register a new webhook endpoint
    pub async fn register(
        &self,
        url: String,
        event_types: Vec<String>,
        secret: Option<String>,
    ) -> Result<String> {
        let endpoint = WebhookEndpoint::new(url, event_types, secret);
        let id = endpoint.id.clone();

        let mut endpoints = self.endpoints.write().await;
        endpoints.insert(id.clone(), endpoint);

        Ok(id)
    }

    /// Unregister a webhook endpoint
    pub async fn unregister(&self, id: &str) -> Result<bool> {
        let mut endpoints = self.endpoints.write().await;
        Ok(endpoints.remove(id).is_some())
    }

    /// List all registered webhooks
    pub async fn list_webhooks(&self) -> Vec<WebhookEndpoint> {
        let endpoints = self.endpoints.read().await;
        endpoints.values().cloned().collect()
    }

    /// Get webhook by ID
    pub async fn get_webhook(&self, id: &str) -> Option<WebhookEndpoint> {
        let endpoints = self.endpoints.read().await;
        endpoints.get(id).cloned()
    }

    /// Enable/disable a webhook
    pub async fn set_enabled(&self, id: &str, enabled: bool) -> Result<()> {
        let mut endpoints = self.endpoints.write().await;
        if let Some(endpoint) = endpoints.get_mut(id) {
            endpoint.enabled = enabled;
            Ok(())
        } else {
            anyhow::bail!("Webhook not found: {}", id)
        }
    }

    /// Trigger a webhook event
    pub async fn trigger(&self, event: WebhookEvent) -> Result<()> {
        let endpoints = self.endpoints.read().await.clone();

        // Find matching endpoints
        let matching: Vec<_> = endpoints
            .values()
            .filter(|e| e.should_receive(&event))
            .cloned()
            .collect();

        drop(endpoints);

        // Deliver to each endpoint
        for endpoint in matching {
            let payload = WebhookPayload::new(event.clone(), endpoint.secret.as_deref());

            self.deliver(endpoint, payload).await;
        }

        Ok(())
    }

    /// Deliver webhook to endpoint
    async fn deliver(&self, endpoint: WebhookEndpoint, payload: WebhookPayload) {
        let delivery_id = Uuid::new_v4().to_string();

        let delivery = WebhookDelivery {
            id: delivery_id.clone(),
            endpoint_id: endpoint.id.clone(),
            payload: payload.clone(),
            attempt: 0,
            status: DeliveryStatus::Pending,
            response_code: None,
            response_body: None,
            error: None,
            created_at: Utc::now(),
            completed_at: None,
        };

        // Store delivery
        {
            let mut deliveries = self.deliveries.write().await;
            deliveries.push(delivery);
        }

        // Spawn async delivery task
        let manager = self.clone_for_delivery();
        tokio::spawn(async move {
            manager
                .execute_delivery(&endpoint, &payload, &delivery_id)
                .await;
        });
    }

    /// Execute webhook delivery with retries
    async fn execute_delivery(
        &self,
        endpoint: &WebhookEndpoint,
        payload: &WebhookPayload,
        delivery_id: &str,
    ) {
        let mut attempt = 0;
        let mut delay = self.config.initial_retry_delay_ms;

        loop {
            attempt += 1;

            // Update delivery status
            self.update_delivery_status(delivery_id, DeliveryStatus::InProgress, attempt)
                .await;

            // Send HTTP request
            match self.send_webhook(&endpoint.url, payload).await {
                Ok(response) => {
                    let status_code = response.status().as_u16();
                    let body = response.text().await.unwrap_or_default();

                    if (200..300).contains(&status_code) {
                        // Success
                        self.mark_delivery_success(delivery_id, status_code, body)
                            .await;
                        self.update_endpoint_stats(&endpoint.id, true).await;
                        break;
                    } else {
                        // HTTP error
                        let error = format!("HTTP {}: {}", status_code, body);

                        if attempt > self.config.max_retries {
                            self.mark_delivery_failed(delivery_id, error.clone()).await;
                            self.update_endpoint_stats(&endpoint.id, false).await;
                            break;
                        } else {
                            self.update_delivery_status(
                                delivery_id,
                                DeliveryStatus::Retrying,
                                attempt,
                            )
                            .await;
                        }
                    }
                }
                Err(e) => {
                    // Network/timeout error
                    let error = format!("Request failed: {}", e);

                    if attempt > self.config.max_retries {
                        self.mark_delivery_failed(delivery_id, error).await;
                        self.update_endpoint_stats(&endpoint.id, false).await;
                        break;
                    } else {
                        self.update_delivery_status(delivery_id, DeliveryStatus::Retrying, attempt)
                            .await;
                    }
                }
            }

            // Exponential backoff
            tokio::time::sleep(tokio::time::Duration::from_millis(delay)).await;
            delay = (delay * 2).min(self.config.max_retry_delay_ms);
        }
    }

    /// Send webhook HTTP request
    async fn send_webhook(&self, url: &str, payload: &WebhookPayload) -> Result<reqwest::Response> {
        let mut request = self
            .client
            .post(url)
            .header("Content-Type", "application/json")
            .header("User-Agent", "RustCode-Webhook/1.0");

        if let Some(ref sig) = payload.signature {
            request = request.header("X-Webhook-Signature", sig);
        }

        request = request.json(payload);

        request.send().await.context("Failed to send webhook")
    }

    /// Update delivery status
    async fn update_delivery_status(&self, id: &str, status: DeliveryStatus, attempt: u32) {
        let mut deliveries = self.deliveries.write().await;
        if let Some(delivery) = deliveries.iter_mut().find(|d| d.id == id) {
            delivery.status = status;
            delivery.attempt = attempt;
        }
    }

    /// Mark delivery as successful
    async fn mark_delivery_success(&self, id: &str, status_code: u16, body: String) {
        let mut deliveries = self.deliveries.write().await;
        if let Some(delivery) = deliveries.iter_mut().find(|d| d.id == id) {
            delivery.status = DeliveryStatus::Success;
            delivery.response_code = Some(status_code);
            delivery.response_body = Some(body);
            delivery.completed_at = Some(Utc::now());
        }
    }

    /// Mark delivery as failed
    async fn mark_delivery_failed(&self, id: &str, error: String) {
        let mut deliveries = self.deliveries.write().await;
        if let Some(delivery) = deliveries.iter_mut().find(|d| d.id == id) {
            delivery.status = DeliveryStatus::Failed;
            delivery.error = Some(error);
            delivery.completed_at = Some(Utc::now());
        }
    }

    /// Update endpoint statistics
    async fn update_endpoint_stats(&self, id: &str, success: bool) {
        let mut endpoints = self.endpoints.write().await;
        if let Some(endpoint) = endpoints.get_mut(id) {
            endpoint.total_triggers += 1;
            endpoint.last_triggered = Some(Utc::now());
            if !success {
                endpoint.failed_triggers += 1;
            }
        }
    }

    /// Get delivery history
    pub async fn get_deliveries(&self, endpoint_id: Option<&str>) -> Vec<WebhookDelivery> {
        let deliveries = self.deliveries.read().await;
        if let Some(id) = endpoint_id {
            deliveries
                .iter()
                .filter(|d| d.endpoint_id == id)
                .cloned()
                .collect()
        } else {
            deliveries.clone()
        }
    }

    /// Clone for async delivery
    fn clone_for_delivery(&self) -> Self {
        Self {
            config: self.config.clone(),
            endpoints: Arc::clone(&self.endpoints),
            deliveries: Arc::clone(&self.deliveries),
            client: self.client.clone(),
        }
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_webhook_registration() {
        let config = WebhookConfig::default();
        let manager = WebhookManager::new(config);

        let id = manager
            .register(
                "https://example.com/webhook".to_string(),
                vec!["document.indexed".to_string()],
                Some("secret".to_string()),
            )
            .await
            .unwrap();

        let webhook = manager.get_webhook(&id).await.unwrap();
        assert_eq!(webhook.url, "https://example.com/webhook");
        assert!(webhook.enabled);
    }

    #[tokio::test]
    async fn test_event_filtering() {
        let endpoint = WebhookEndpoint::new(
            "https://example.com/webhook".to_string(),
            vec!["document.indexed".to_string()],
            None,
        );

        let event1 = WebhookEvent::DocumentIndexed {
            document_id: 1,
            chunks: 10,
            duration_ms: 1000,
        };

        let event2 = WebhookEvent::SearchPerformed {
            query: "test".to_string(),
            results_count: 5,
            execution_time_ms: 50,
            search_type: "hybrid".to_string(),
        };

        assert!(endpoint.should_receive(&event1));
        assert!(!endpoint.should_receive(&event2));
    }

    #[test]
    fn test_signature_verification() {
        let event = WebhookEvent::DocumentIndexed {
            document_id: 123,
            chunks: 10,
            duration_ms: 1000,
        };

        let payload = WebhookPayload::new(event, Some("secret"));
        assert!(payload.verify_signature("secret"));
        assert!(!payload.verify_signature("wrong_secret"));
    }

    #[tokio::test]
    async fn test_enable_disable_webhook() {
        let config = WebhookConfig::default();
        let manager = WebhookManager::new(config);

        let id = manager
            .register("https://example.com/webhook".to_string(), vec![], None)
            .await
            .unwrap();

        manager.set_enabled(&id, false).await.unwrap();
        let webhook = manager.get_webhook(&id).await.unwrap();
        assert!(!webhook.enabled);

        manager.set_enabled(&id, true).await.unwrap();
        let webhook = manager.get_webhook(&id).await.unwrap();
        assert!(webhook.enabled);
    }
}
