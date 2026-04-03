//! OpenTelemetry Tracing Module
//!
//! Provides distributed tracing capabilities using OpenTelemetry.
//! Exports traces to OTLP-compatible backends (Jaeger, Tempo, etc.).
//!
//! # Features
//!
//! - **Distributed Tracing**: Track requests across services
//! - **Span Attributes**: Rich context with semantic conventions
//! - **OTLP Export**: Compatible with Jaeger, Tempo, Honeycomb, etc.
//! - **Sampling**: Configurable trace sampling
//! - **Resource Detection**: Automatic service metadata
//!
//! # Example
//!
//! ```rust,no_run
//! use rustcode::telemetry::{init_telemetry, TelemetryConfig};
//! use tracing::instrument;
//!
//! #[instrument]
//! async fn process_document(doc_id: &str) {
//!     tracing::info!("Processing document");
//!     // Work happens here
//! }
//!
//! # #[tokio::main]
//! # async fn main() -> anyhow::Result<()> {
//! let config = TelemetryConfig::default();
//! init_telemetry(config).await?;
//!
//! process_document("doc-123").await;
//! # Ok(())
//! # }
//! ```

use anyhow::{Context, Result};
use opentelemetry::{global, KeyValue};
use opentelemetry_otlp::WithExportConfig;
use opentelemetry_sdk::{
    trace::{self, RandomIdGenerator, Sampler},
    Resource,
};
use opentelemetry_semantic_conventions as semconv;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter, Layer};

// ============================================================================
// Configuration
// ============================================================================

/// Telemetry configuration
#[derive(Debug, Clone)]
pub struct TelemetryConfig {
    /// Service name
    pub service_name: String,

    /// Service version
    pub service_version: String,

    /// Environment (dev, staging, prod)
    pub environment: String,

    /// OTLP endpoint (e.g., "http://localhost:4317")
    pub otlp_endpoint: String,

    /// Enable telemetry
    pub enabled: bool,

    /// Sampling rate (0.0 to 1.0)
    pub sampling_rate: f64,

    /// Enable stdout logging
    pub enable_stdout: bool,

    /// Log level filter
    pub log_level: String,

    /// Additional resource attributes
    pub resource_attributes: Vec<(String, String)>,
}

impl Default for TelemetryConfig {
    fn default() -> Self {
        Self {
            service_name: "rustcode".to_string(),
            service_version: env!("CARGO_PKG_VERSION").to_string(),
            environment: "development".to_string(),
            otlp_endpoint: "http://localhost:4317".to_string(),
            enabled: false,
            sampling_rate: 1.0,
            enable_stdout: true,
            log_level: "info".to_string(),
            resource_attributes: Vec::new(),
        }
    }
}

impl TelemetryConfig {
    /// Create production configuration
    pub fn production(otlp_endpoint: String) -> Self {
        Self {
            service_name: "rustcode".to_string(),
            service_version: env!("CARGO_PKG_VERSION").to_string(),
            environment: "production".to_string(),
            otlp_endpoint,
            enabled: true,
            sampling_rate: 0.1, // Sample 10% in production
            enable_stdout: false,
            log_level: "warn".to_string(),
            resource_attributes: Vec::new(),
        }
    }

    /// Create development configuration
    pub fn development() -> Self {
        Self {
            service_name: "rustcode".to_string(),
            service_version: env!("CARGO_PKG_VERSION").to_string(),
            environment: "development".to_string(),
            otlp_endpoint: "http://localhost:4317".to_string(),
            enabled: true,
            sampling_rate: 1.0,
            enable_stdout: true,
            log_level: "debug".to_string(),
            resource_attributes: Vec::new(),
        }
    }

    /// Add custom resource attribute
    pub fn with_attribute(mut self, key: String, value: String) -> Self {
        self.resource_attributes.push((key, value));
        self
    }
}

// ============================================================================
// Initialization
// ============================================================================

/// Initialize OpenTelemetry tracing
pub async fn init_telemetry(config: TelemetryConfig) -> Result<()> {
    if !config.enabled {
        // Just set up basic logging without tracing
        init_basic_logging(&config);
        return Ok(());
    }

    // Build resource with service information
    let resource = build_resource(&config);

    // Create OTLP tracer
    let tracer = opentelemetry_otlp::new_pipeline()
        .tracing()
        .with_exporter(
            opentelemetry_otlp::new_exporter()
                .tonic()
                .with_endpoint(&config.otlp_endpoint),
        )
        .with_trace_config(
            trace::config()
                .with_sampler(Sampler::TraceIdRatioBased(config.sampling_rate))
                .with_id_generator(RandomIdGenerator::default())
                .with_resource(resource),
        )
        .install_batch(opentelemetry_sdk::runtime::Tokio)
        .context("Failed to install OTLP tracer")?;

    // Create tracing layer
    let telemetry_layer = tracing_opentelemetry::layer().with_tracer(tracer);

    // Create env filter
    let env_filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(&config.log_level));

    // Build subscriber
    let subscriber = tracing_subscriber::registry()
        .with(env_filter)
        .with(telemetry_layer);

    // Optionally add stdout logging
    if config.enable_stdout {
        let fmt_layer = tracing_subscriber::fmt::layer()
            .with_target(true)
            .with_thread_ids(true)
            .with_level(true)
            .with_filter(EnvFilter::new(&config.log_level));

        subscriber.with(fmt_layer).init();
    } else {
        subscriber.init();
    }

    tracing::info!(
        service = %config.service_name,
        version = %config.service_version,
        environment = %config.environment,
        "Telemetry initialized"
    );

    Ok(())
}

/// Initialize basic logging without telemetry
fn init_basic_logging(config: &TelemetryConfig) {
    let env_filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(&config.log_level));

    tracing_subscriber::registry()
        .with(env_filter)
        .with(
            tracing_subscriber::fmt::layer()
                .with_target(true)
                .with_thread_ids(false)
                .with_level(true),
        )
        .init();
}

/// Build OpenTelemetry resource
fn build_resource(config: &TelemetryConfig) -> Resource {
    let mut attributes = vec![
        KeyValue::new(semconv::resource::SERVICE_NAME, config.service_name.clone()),
        KeyValue::new(
            semconv::resource::SERVICE_VERSION,
            config.service_version.clone(),
        ),
        KeyValue::new(
            semconv::resource::DEPLOYMENT_ENVIRONMENT,
            config.environment.clone(),
        ),
    ];

    // Add custom attributes
    for (key, value) in &config.resource_attributes {
        attributes.push(KeyValue::new(key.clone(), value.clone()));
    }

    Resource::new(attributes)
}

/// Shutdown telemetry gracefully
pub async fn shutdown_telemetry() {
    global::shutdown_tracer_provider();
}

// ============================================================================
// Tracing Helpers
// ============================================================================

/// Add span attribute helper
#[macro_export]
macro_rules! span_attr {
    ($key:expr, $value:expr) => {
        tracing::Span::current().record($key, &tracing::field::display($value))
    };
}

/// Create error span event
#[macro_export]
macro_rules! span_error {
    ($error:expr) => {
        tracing::error!(error = %$error, "Operation failed")
    };
    ($error:expr, $msg:expr) => {
        tracing::error!(error = %$error, $msg)
    };
}

// ============================================================================
// Common Span Attributes
// ============================================================================

pub mod attributes {
    //! Common span attributes following semantic conventions

    pub const HTTP_METHOD: &str = "http.method";
    pub const HTTP_URL: &str = "http.url";
    pub const HTTP_STATUS_CODE: &str = "http.status_code";
    pub const HTTP_ROUTE: &str = "http.route";
    pub const HTTP_USER_AGENT: &str = "http.user_agent";

    pub const DB_SYSTEM: &str = "db.system";
    pub const DB_OPERATION: &str = "db.operation";
    pub const DB_STATEMENT: &str = "db.statement";

    pub const DOCUMENT_ID: &str = "document.id";
    pub const DOCUMENT_TYPE: &str = "document.type";
    pub const CHUNK_COUNT: &str = "chunk.count";
    pub const EMBEDDING_MODEL: &str = "embedding.model";
    pub const SEARCH_QUERY: &str = "search.query";
    pub const SEARCH_RESULTS: &str = "search.results";
    pub const CACHE_HIT: &str = "cache.hit";
    pub const WEBHOOK_EVENT: &str = "webhook.event";
    pub const JOB_ID: &str = "job.id";
    pub const JOB_STATUS: &str = "job.status";
}

// ============================================================================
// Instrumentation Examples
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_init_telemetry_disabled() {
        let config = TelemetryConfig {
            enabled: false,
            ..Default::default()
        };

        let result = init_telemetry(config).await;
        assert!(result.is_ok());
    }

    #[test]
    fn test_config_defaults() {
        let config = TelemetryConfig::default();
        assert_eq!(config.service_name, "rustcode");
        assert_eq!(config.environment, "development");
        assert!(!config.enabled);
        assert_eq!(config.sampling_rate, 1.0);
    }

    #[test]
    fn test_production_config() {
        let config = TelemetryConfig::production("http://prod:4317".to_string());
        assert_eq!(config.environment, "production");
        assert!(config.enabled);
        assert_eq!(config.sampling_rate, 0.1);
        assert!(!config.enable_stdout);
    }

    #[test]
    fn test_development_config() {
        let config = TelemetryConfig::development();
        assert_eq!(config.environment, "development");
        assert!(config.enabled);
        assert_eq!(config.sampling_rate, 1.0);
        assert!(config.enable_stdout);
    }

    #[test]
    fn test_custom_attributes() {
        let config = TelemetryConfig::default()
            .with_attribute("region".to_string(), "us-west-2".to_string())
            .with_attribute("cluster".to_string(), "prod-1".to_string());

        assert_eq!(config.resource_attributes.len(), 2);
        assert_eq!(config.resource_attributes[0].0, "region");
        assert_eq!(config.resource_attributes[1].1, "prod-1");
    }
}
