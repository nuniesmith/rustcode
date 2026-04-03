//! Prometheus Metrics Module
//!
//! Provides comprehensive metrics collection for monitoring and observability.
//! Tracks API requests, search performance, indexing jobs, and system health.
//!
//! # Features
//!
//! - **Request Metrics**: Track API endpoint usage, latency, and errors
//! - **Search Metrics**: Monitor search performance and quality
//! - **Indexing Metrics**: Track background job performance
//! - **System Metrics**: Database, cache, and resource utilization
//! - **Custom Metrics**: Application-specific business metrics
//!
//! # Example
//!
//! ```rust,no_run
//! use rustcode::metrics::MetricsRegistry;
//!
//! # async fn example() -> anyhow::Result<()> {
//! let registry = MetricsRegistry::new();
//!
//! // Track API request
//! let timer = registry.start_request_timer("POST", "/api/documents");
//! // ... process request ...
//! timer.observe_with_status(200).await;
//!
//! // Record search metrics
//! registry.record_search("hybrid", 10, 45).await;
//!
//! // Export metrics (async)
//! let metrics = registry.export_prometheus().await;
//! println!("{}", metrics);
//! # Ok(())
//! # }
//! ```

use chrono::Utc;
use serde::Serialize;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::RwLock;

// ============================================================================
// Metrics Registry
// ============================================================================

/// Main metrics registry
pub struct MetricsRegistry {
    counters: Arc<RwLock<HashMap<String, Counter>>>,
    gauges: Arc<RwLock<HashMap<String, Gauge>>>,
    histograms: Arc<RwLock<HashMap<String, Histogram>>>,
    start_time: Instant,
}

impl MetricsRegistry {
    /// Create a new metrics registry
    pub fn new() -> Self {
        Self {
            counters: Arc::new(RwLock::new(HashMap::new())),
            gauges: Arc::new(RwLock::new(HashMap::new())),
            histograms: Arc::new(RwLock::new(HashMap::new())),
            start_time: Instant::now(),
        }
    }

    /// Increment a counter
    pub async fn increment_counter(&self, name: &str, labels: HashMap<String, String>) {
        let mut counters = self.counters.write().await;
        let key = Self::metric_key(name, &labels);
        counters
            .entry(key)
            .or_insert_with(|| Counter::new(name.to_string(), labels))
            .increment();
    }

    /// Set a gauge value
    pub async fn set_gauge(&self, name: &str, value: f64, labels: HashMap<String, String>) {
        let mut gauges = self.gauges.write().await;
        let key = Self::metric_key(name, &labels);
        gauges
            .entry(key)
            .or_insert_with(|| Gauge::new(name.to_string(), labels))
            .set(value);
    }

    /// Observe a histogram value
    pub async fn observe_histogram(&self, name: &str, value: f64, labels: HashMap<String, String>) {
        let mut histograms = self.histograms.write().await;
        let key = Self::metric_key(name, &labels);
        histograms
            .entry(key)
            .or_insert_with(|| Histogram::new(name.to_string(), labels))
            .observe(value);
    }

    /// Start a request timer
    pub fn start_request_timer(&self, method: &str, path: &str) -> RequestTimer {
        let mut labels = HashMap::new();
        labels.insert("method".to_string(), method.to_string());
        labels.insert("path".to_string(), path.to_string());

        RequestTimer {
            registry: self.clone_arc(),
            labels,
            start: Instant::now(),
        }
    }

    /// Record API request
    pub async fn record_request(&self, method: &str, path: &str, status: u16, duration_ms: f64) {
        let mut labels = HashMap::new();
        labels.insert("method".to_string(), method.to_string());
        labels.insert("path".to_string(), path.to_string());
        labels.insert("status".to_string(), status.to_string());

        self.increment_counter("http_requests_total", labels.clone())
            .await;

        self.observe_histogram("http_request_duration_ms", duration_ms, labels)
            .await;
    }

    /// Record search metrics
    pub async fn record_search(&self, search_type: &str, results_count: usize, duration_ms: u64) {
        let mut labels = HashMap::new();
        labels.insert("search_type".to_string(), search_type.to_string());

        self.increment_counter("search_requests_total", labels.clone())
            .await;

        self.set_gauge("search_results_count", results_count as f64, labels.clone())
            .await;

        self.observe_histogram("search_duration_ms", duration_ms as f64, labels)
            .await;
    }

    /// Record indexing job metrics
    pub async fn record_indexing_job(&self, documents: usize, duration_ms: u64, success: bool) {
        let mut labels = HashMap::new();
        labels.insert(
            "status".to_string(),
            if success { "success" } else { "failed" }.to_string(),
        );

        self.increment_counter("indexing_jobs_total", labels.clone())
            .await;

        self.set_gauge("indexing_documents_count", documents as f64, labels.clone())
            .await;

        self.observe_histogram("indexing_duration_ms", duration_ms as f64, labels)
            .await;
    }

    /// Record cache metrics
    pub async fn record_cache_hit(&self, cache_type: &str) {
        let mut labels = HashMap::new();
        labels.insert("cache_type".to_string(), cache_type.to_string());
        labels.insert("result".to_string(), "hit".to_string());

        self.increment_counter("cache_requests_total", labels).await;
    }

    pub async fn record_cache_miss(&self, cache_type: &str) {
        let mut labels = HashMap::new();
        labels.insert("cache_type".to_string(), cache_type.to_string());
        labels.insert("result".to_string(), "miss".to_string());

        self.increment_counter("cache_requests_total", labels).await;
    }

    /// Record webhook delivery
    pub async fn record_webhook_delivery(&self, success: bool, retry_count: u32) {
        let mut labels = HashMap::new();
        labels.insert(
            "status".to_string(),
            if success { "success" } else { "failed" }.to_string(),
        );
        labels.insert("retry_count".to_string(), retry_count.to_string());

        self.increment_counter("webhook_deliveries_total", labels)
            .await;
    }

    /// Get system uptime in seconds
    pub fn uptime_seconds(&self) -> f64 {
        self.start_time.elapsed().as_secs_f64()
    }

    /// Export metrics in Prometheus format
    pub async fn export_prometheus(&self) -> String {
        let mut output = String::new();

        // System uptime
        output.push_str(&format!(
            "# HELP process_uptime_seconds Time since server started\n\
             # TYPE process_uptime_seconds gauge\n\
             process_uptime_seconds {}\n\n",
            self.uptime_seconds()
        ));

        // Export counters
        let counters = self.counters.read().await;
        for counter in counters.values() {
            output.push_str(&counter.export_prometheus());
        }

        // Export gauges
        let gauges = self.gauges.read().await;
        for gauge in gauges.values() {
            output.push_str(&gauge.export_prometheus());
        }

        // Export histograms
        let histograms = self.histograms.read().await;
        for histogram in histograms.values() {
            output.push_str(&histogram.export_prometheus());
        }

        output
    }

    /// Export metrics as JSON
    pub async fn export_json(&self) -> serde_json::Value {
        let counters = self.counters.read().await;
        let gauges = self.gauges.read().await;
        let histograms = self.histograms.read().await;

        serde_json::json!({
            "uptime_seconds": self.uptime_seconds(),
            "timestamp": Utc::now().to_rfc3339(),
            "counters": counters.values().collect::<Vec<_>>(),
            "gauges": gauges.values().collect::<Vec<_>>(),
            "histograms": histograms.values().map(|h| h.to_summary()).collect::<Vec<_>>(),
        })
    }

    /// Get metric statistics
    pub async fn get_stats(&self) -> MetricsStats {
        let counters = self.counters.read().await;
        let gauges = self.gauges.read().await;
        let histograms = self.histograms.read().await;

        MetricsStats {
            total_counters: counters.len(),
            total_gauges: gauges.len(),
            total_histograms: histograms.len(),
            uptime_seconds: self.uptime_seconds(),
        }
    }

    /// Reset all metrics
    pub async fn reset(&self) {
        self.counters.write().await.clear();
        self.gauges.write().await.clear();
        self.histograms.write().await.clear();
    }

    fn metric_key(name: &str, labels: &HashMap<String, String>) -> String {
        let mut label_pairs: Vec<_> = labels.iter().collect();
        label_pairs.sort_by_key(|(k, _)| *k);

        let label_str = label_pairs
            .iter()
            .map(|(k, v)| format!("{}=\"{}\"", k, v))
            .collect::<Vec<_>>()
            .join(",");

        format!("{}:{}", name, label_str)
    }

    fn clone_arc(&self) -> Arc<Self> {
        Arc::new(Self {
            counters: Arc::clone(&self.counters),
            gauges: Arc::clone(&self.gauges),
            histograms: Arc::clone(&self.histograms),
            start_time: self.start_time,
        })
    }
}

impl Default for MetricsRegistry {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// Metric Types
// ============================================================================

/// Counter metric (monotonically increasing)
#[derive(Debug, Clone, Serialize)]
pub struct Counter {
    name: String,
    labels: HashMap<String, String>,
    value: u64,
}

impl Counter {
    fn new(name: String, labels: HashMap<String, String>) -> Self {
        Self {
            name,
            labels,
            value: 0,
        }
    }

    fn increment(&mut self) {
        self.value += 1;
    }

    fn export_prometheus(&self) -> String {
        let labels = self.format_labels();
        format!(
            "# TYPE {} counter\n{}{} {}\n\n",
            self.name, self.name, labels, self.value
        )
    }

    fn format_labels(&self) -> String {
        if self.labels.is_empty() {
            String::new()
        } else {
            let mut pairs: Vec<_> = self.labels.iter().collect();
            pairs.sort_by_key(|(k, _)| *k);
            let formatted = pairs
                .iter()
                .map(|(k, v)| format!("{}=\"{}\"", k, v))
                .collect::<Vec<_>>()
                .join(",");
            format!("{{{}}}", formatted)
        }
    }
}

/// Gauge metric (can go up or down)
#[derive(Debug, Clone, Serialize)]
pub struct Gauge {
    name: String,
    labels: HashMap<String, String>,
    value: f64,
}

impl Gauge {
    fn new(name: String, labels: HashMap<String, String>) -> Self {
        Self {
            name,
            labels,
            value: 0.0,
        }
    }

    fn set(&mut self, value: f64) {
        self.value = value;
    }

    fn export_prometheus(&self) -> String {
        let labels = self.format_labels();
        format!(
            "# TYPE {} gauge\n{}{} {}\n\n",
            self.name, self.name, labels, self.value
        )
    }

    fn format_labels(&self) -> String {
        if self.labels.is_empty() {
            String::new()
        } else {
            let mut pairs: Vec<_> = self.labels.iter().collect();
            pairs.sort_by_key(|(k, _)| *k);
            let formatted = pairs
                .iter()
                .map(|(k, v)| format!("{}=\"{}\"", k, v))
                .collect::<Vec<_>>()
                .join(",");
            format!("{{{}}}", formatted)
        }
    }
}

/// Histogram metric (distribution of values)
#[derive(Debug, Clone)]
pub struct Histogram {
    name: String,
    labels: HashMap<String, String>,
    values: Vec<f64>,
    sum: f64,
    count: u64,
}

impl Histogram {
    fn new(name: String, labels: HashMap<String, String>) -> Self {
        Self {
            name,
            labels,
            values: Vec::new(),
            sum: 0.0,
            count: 0,
        }
    }

    fn observe(&mut self, value: f64) {
        self.values.push(value);
        self.sum += value;
        self.count += 1;
    }

    fn quantile(&self, q: f64) -> f64 {
        if self.values.is_empty() {
            return 0.0;
        }

        let mut sorted = self.values.clone();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());

        let index = ((sorted.len() - 1) as f64 * q).floor() as usize;
        sorted[index]
    }

    fn export_prometheus(&self) -> String {
        let labels = self.format_labels();
        let mut output = format!("# TYPE {} histogram\n", self.name);

        // Export quantiles
        for q in &[0.5, 0.9, 0.95, 0.99] {
            let quantile_labels = self.format_quantile_labels(*q);
            output.push_str(&format!(
                "{}{} {}\n",
                self.name,
                quantile_labels,
                self.quantile(*q)
            ));
        }

        // Export sum and count
        output.push_str(&format!("{}_sum{} {}\n", self.name, labels, self.sum));
        output.push_str(&format!("{}_count{} {}\n\n", self.name, labels, self.count));

        output
    }

    fn format_labels(&self) -> String {
        if self.labels.is_empty() {
            String::new()
        } else {
            let mut pairs: Vec<_> = self.labels.iter().collect();
            pairs.sort_by_key(|(k, _)| *k);
            let formatted = pairs
                .iter()
                .map(|(k, v)| format!("{}=\"{}\"", k, v))
                .collect::<Vec<_>>()
                .join(",");
            format!("{{{}}}", formatted)
        }
    }

    fn format_quantile_labels(&self, quantile: f64) -> String {
        let mut labels = self.labels.clone();
        labels.insert("quantile".to_string(), quantile.to_string());

        let mut pairs: Vec<_> = labels.iter().collect();
        pairs.sort_by_key(|(k, _)| *k);
        let formatted = pairs
            .iter()
            .map(|(k, v)| format!("{}=\"{}\"", k, v))
            .collect::<Vec<_>>()
            .join(",");
        format!("{{{}}}", formatted)
    }

    fn to_summary(&self) -> HistogramSummary {
        HistogramSummary {
            name: self.name.clone(),
            labels: self.labels.clone(),
            count: self.count,
            sum: self.sum,
            avg: if self.count > 0 {
                self.sum / self.count as f64
            } else {
                0.0
            },
            p50: self.quantile(0.5),
            p90: self.quantile(0.9),
            p95: self.quantile(0.95),
            p99: self.quantile(0.99),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct HistogramSummary {
    name: String,
    labels: HashMap<String, String>,
    count: u64,
    sum: f64,
    avg: f64,
    p50: f64,
    p90: f64,
    p95: f64,
    p99: f64,
}

// ============================================================================
// Request Timer
// ============================================================================

/// Timer for tracking request duration
pub struct RequestTimer {
    registry: Arc<MetricsRegistry>,
    labels: HashMap<String, String>,
    start: Instant,
}

impl RequestTimer {
    /// Observe the duration and record the metric
    pub async fn observe_duration(self) {
        let duration = self.start.elapsed().as_secs_f64() * 1000.0; // Convert to ms
        self.registry
            .observe_histogram("http_request_duration_ms", duration, self.labels)
            .await;
    }

    /// Observe duration with status code
    pub async fn observe_with_status(self, status: u16) {
        let duration = self.start.elapsed().as_secs_f64() * 1000.0;
        let mut labels = self.labels.clone();
        labels.insert("status".to_string(), status.to_string());

        self.registry
            .increment_counter("http_requests_total", labels.clone())
            .await;

        self.registry
            .observe_histogram("http_request_duration_ms", duration, labels)
            .await;
    }
}

// ============================================================================
// Metrics Statistics
// ============================================================================

#[derive(Debug, Clone, Serialize)]
pub struct MetricsStats {
    pub total_counters: usize,
    pub total_gauges: usize,
    pub total_histograms: usize,
    pub uptime_seconds: f64,
}

// ============================================================================
// Global Registry
// ============================================================================

use std::sync::LazyLock;

static GLOBAL_REGISTRY: LazyLock<Arc<MetricsRegistry>> =
    LazyLock::new(|| Arc::new(MetricsRegistry::new()));

/// Get the global metrics registry
pub fn global_registry() -> Arc<MetricsRegistry> {
    Arc::clone(&GLOBAL_REGISTRY)
}

// ============================================================================
// Helper Functions
// ============================================================================

/// Track an API request
pub async fn track_request(method: &str, path: &str, status: u16, duration_ms: f64) {
    global_registry()
        .record_request(method, path, status, duration_ms)
        .await;
}

/// Track a search request
pub async fn track_search(search_type: &str, results_count: usize, duration_ms: u64) {
    global_registry()
        .record_search(search_type, results_count, duration_ms)
        .await;
}

/// Track an indexing job
pub async fn track_indexing_job(documents: usize, duration_ms: u64, success: bool) {
    global_registry()
        .record_indexing_job(documents, duration_ms, success)
        .await;
}

/// Track cache hit
pub async fn track_cache_hit(cache_type: &str) {
    global_registry().record_cache_hit(cache_type).await;
}

/// Track cache miss
pub async fn track_cache_miss(cache_type: &str) {
    global_registry().record_cache_miss(cache_type).await;
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_counter() {
        let registry = MetricsRegistry::new();
        let mut labels = HashMap::new();
        labels.insert("test".to_string(), "value".to_string());

        registry
            .increment_counter("test_counter", labels.clone())
            .await;
        registry
            .increment_counter("test_counter", labels.clone())
            .await;

        let export = registry.export_prometheus().await;
        assert!(export.contains("test_counter"));
        assert!(export.contains("2"));
    }

    #[tokio::test]
    async fn test_gauge() {
        let registry = MetricsRegistry::new();
        let mut labels = HashMap::new();
        labels.insert("test".to_string(), "value".to_string());

        registry.set_gauge("test_gauge", 42.5, labels).await;

        let export = registry.export_prometheus().await;
        assert!(export.contains("test_gauge"));
        assert!(export.contains("42.5"));
    }

    #[tokio::test]
    async fn test_histogram() {
        let registry = MetricsRegistry::new();
        let mut labels = HashMap::new();
        labels.insert("test".to_string(), "value".to_string());

        registry
            .observe_histogram("test_histogram", 10.0, labels.clone())
            .await;
        registry
            .observe_histogram("test_histogram", 20.0, labels.clone())
            .await;
        registry
            .observe_histogram("test_histogram", 30.0, labels)
            .await;

        let export = registry.export_prometheus().await;
        assert!(export.contains("test_histogram"));
        assert!(export.contains("_sum"));
        assert!(export.contains("_count"));
    }

    #[tokio::test]
    async fn test_request_timer() {
        let registry = MetricsRegistry::new();
        let timer = registry.start_request_timer("GET", "/api/test");

        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
        timer.observe_with_status(200).await;

        let export = registry.export_prometheus().await;
        assert!(export.contains("http_request_duration_ms"));
        assert!(export.contains("http_requests_total"));
    }

    #[test]
    fn test_histogram_quantiles() {
        let mut histogram = Histogram::new("test".to_string(), HashMap::new());

        for i in 1..=100 {
            histogram.observe(i as f64);
        }

        assert_eq!(histogram.quantile(0.5), 50.0);
        assert!(histogram.quantile(0.9) >= 90.0);
        assert!(histogram.quantile(0.99) >= 99.0);
    }

    #[tokio::test]
    async fn test_json_export() {
        let registry = MetricsRegistry::new();
        let mut labels = HashMap::new();
        labels.insert("test".to_string(), "value".to_string());

        registry
            .increment_counter("test_counter", labels.clone())
            .await;
        registry.set_gauge("test_gauge", 42.0, labels).await;

        let json = registry.export_json().await;
        assert!(json["counters"].is_array());
        assert!(json["gauges"].is_array());
        assert!(json["uptime_seconds"].is_number());
    }
}
