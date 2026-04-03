//! Background job queue for asynchronous document indexing

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{Mutex, RwLock};
use uuid::Uuid;

use crate::embeddings::EmbeddingGenerator;
use crate::indexing::{DocumentIndexer, IndexingConfig};
use sqlx::PgPool;

// ============================================================================
// Job Types
// ============================================================================

/// Job status
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum JobStatus {
    Queued,
    Processing,
    Completed,
    Failed,
    Cancelled,
}

/// Index job
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexJob {
    pub id: String,
    pub document_ids: Vec<String>,
    pub status: JobStatus,
    pub created_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    pub progress: JobProgress,
    pub error: Option<String>,
    pub retry_count: u32,
    pub force_reindex: bool,
}

/// Job progress tracking
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobProgress {
    pub total: usize,
    pub completed: usize,
    pub failed: usize,
    pub current_document_id: Option<String>,
}

impl JobProgress {
    fn new(total: usize) -> Self {
        Self {
            total,
            completed: 0,
            failed: 0,
            current_document_id: None,
        }
    }

    #[allow(dead_code)]
    fn is_complete(&self) -> bool {
        self.completed + self.failed >= self.total
    }

    #[allow(dead_code)]
    fn success_rate(&self) -> f64 {
        if self.total == 0 {
            0.0
        } else {
            self.completed as f64 / self.total as f64
        }
    }
}

impl IndexJob {
    pub fn new(document_ids: Vec<String>, force_reindex: bool) -> Self {
        let total = document_ids.len();
        Self {
            id: Uuid::new_v4().to_string(),
            document_ids,
            status: JobStatus::Queued,
            created_at: Utc::now(),
            started_at: None,
            completed_at: None,
            progress: JobProgress::new(total),
            error: None,
            retry_count: 0,
            force_reindex,
        }
    }

    pub fn start(&mut self) {
        self.status = JobStatus::Processing;
        self.started_at = Some(Utc::now());
    }

    pub fn complete(&mut self) {
        self.status = JobStatus::Completed;
        self.completed_at = Some(Utc::now());
    }

    pub fn fail(&mut self, error: String) {
        self.status = JobStatus::Failed;
        self.completed_at = Some(Utc::now());
        self.error = Some(error);
    }

    pub fn cancel(&mut self) {
        self.status = JobStatus::Cancelled;
        self.completed_at = Some(Utc::now());
    }

    pub fn can_retry(&self) -> bool {
        self.status == JobStatus::Failed && self.retry_count < 3
    }
}

// ============================================================================
// Job Queue
// ============================================================================

/// Job queue configuration
#[derive(Debug, Clone)]
pub struct JobQueueConfig {
    pub max_concurrent_jobs: usize,
    pub retry_enabled: bool,
    pub max_retries: u32,
}

impl Default for JobQueueConfig {
    fn default() -> Self {
        Self {
            max_concurrent_jobs: 2,
            retry_enabled: true,
            max_retries: 3,
        }
    }
}

/// Background job queue
pub struct JobQueue {
    config: JobQueueConfig,
    jobs: Arc<RwLock<HashMap<String, IndexJob>>>,
    processing: Arc<Mutex<Vec<String>>>,
    db_pool: PgPool,
    _embedding_generator: Arc<Mutex<EmbeddingGenerator>>,
    indexing_config: IndexingConfig,
}

impl JobQueue {
    pub fn new(
        config: JobQueueConfig,
        db_pool: PgPool,
        embedding_generator: Arc<Mutex<EmbeddingGenerator>>,
        indexing_config: IndexingConfig,
    ) -> Self {
        Self {
            config,
            jobs: Arc::new(RwLock::new(HashMap::new())),
            processing: Arc::new(Mutex::new(Vec::new())),
            db_pool,
            _embedding_generator: embedding_generator,
            indexing_config,
        }
    }

    /// Submit a new job
    pub async fn submit_job(&self, document_ids: Vec<String>, force_reindex: bool) -> String {
        let job = IndexJob::new(document_ids, force_reindex);
        let job_id = job.id.clone();

        let mut jobs = self.jobs.write().await;
        jobs.insert(job_id.clone(), job);

        // Start processing if possible
        drop(jobs);
        self.process_next_job().await;

        job_id
    }

    /// Return the number of queued (pending) jobs without async overhead.
    ///
    /// Uses `try_read` so it never blocks; returns 0 if the lock is contended.
    pub fn pending_count(&self) -> usize {
        match self.jobs.try_read() {
            Ok(jobs) => jobs
                .values()
                .filter(|j| j.status == JobStatus::Queued)
                .count(),
            Err(_) => 0,
        }
    }

    /// Get job status
    pub async fn get_job(&self, job_id: &str) -> Option<IndexJob> {
        let jobs = self.jobs.read().await;
        jobs.get(job_id).cloned()
    }

    /// List all jobs
    pub async fn list_jobs(&self) -> Vec<IndexJob> {
        let jobs = self.jobs.read().await;
        let mut job_list: Vec<_> = jobs.values().cloned().collect();
        job_list.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        job_list
    }

    /// Cancel a job
    pub async fn cancel_job(&self, job_id: &str) -> Result<(), String> {
        let mut jobs = self.jobs.write().await;
        if let Some(job) = jobs.get_mut(job_id) {
            if job.status == JobStatus::Queued {
                job.cancel();
                Ok(())
            } else {
                Err("Can only cancel queued jobs".to_string())
            }
        } else {
            Err("Job not found".to_string())
        }
    }

    /// Delete a job from queue
    pub async fn delete_job(&self, job_id: &str) -> Result<(), String> {
        let mut jobs = self.jobs.write().await;
        if jobs.remove(job_id).is_some() {
            Ok(())
        } else {
            Err("Job not found".to_string())
        }
    }

    /// Process next queued job
    async fn process_next_job(&self) {
        // Check if we can start a new job
        let processing = self.processing.lock().await;
        if processing.len() >= self.config.max_concurrent_jobs {
            return;
        }
        drop(processing);

        // Find next queued job
        let job_id = {
            let jobs = self.jobs.read().await;
            jobs.values()
                .filter(|j| j.status == JobStatus::Queued)
                .min_by_key(|j| j.created_at)
                .map(|j| j.id.clone())
        };

        if let Some(job_id) = job_id {
            Box::pin(self.process_job(job_id)).await;
        }
    }

    /// Process a specific job
    async fn process_job(&self, job_id: String) {
        // Mark as processing
        {
            let mut processing = self.processing.lock().await;
            processing.push(job_id.clone());

            let mut jobs = self.jobs.write().await;
            if let Some(job) = jobs.get_mut(&job_id) {
                job.start();
            }
        }

        // Clone necessary data
        let (document_ids, force_reindex) = {
            let jobs = self.jobs.read().await;
            let job = jobs.get(&job_id).unwrap();
            (job.document_ids.clone(), job.force_reindex)
        };

        // Process documents
        let result = self
            .index_documents(job_id.clone(), document_ids, force_reindex)
            .await;

        // Update job status
        {
            let mut jobs = self.jobs.write().await;
            if let Some(job) = jobs.get_mut(&job_id) {
                match result {
                    Ok(_) => job.complete(),
                    Err(e) => {
                        job.fail(e.to_string());
                        job.retry_count += 1;
                    }
                }
            }

            let mut processing = self.processing.lock().await;
            processing.retain(|id| id != &job_id);
        }

        // Process next job
        Box::pin(self.process_next_job()).await;
    }

    /// Index multiple documents
    async fn index_documents(
        &self,
        job_id: String,
        document_ids: Vec<String>,
        _force_reindex: bool,
    ) -> Result<(), String> {
        let indexer = DocumentIndexer::new(self.indexing_config.clone())
            .await
            .map_err(|e| format!("Failed to create indexer: {}", e))?;

        for doc_id in document_ids.iter() {
            // Update progress
            {
                let mut jobs = self.jobs.write().await;
                if let Some(job) = jobs.get_mut(&job_id) {
                    job.progress.current_document_id = Some(doc_id.clone());
                }
            }

            // Index document
            match indexer.index_document(&self.db_pool, doc_id).await {
                Ok(_) => {
                    let mut jobs = self.jobs.write().await;
                    if let Some(job) = jobs.get_mut(&job_id) {
                        job.progress.completed += 1;
                    }
                }
                Err(e) => {
                    let mut jobs = self.jobs.write().await;
                    if let Some(job) = jobs.get_mut(&job_id) {
                        job.progress.failed += 1;
                    }
                    tracing::warn!("Failed to index document {}: {}", doc_id, e);
                }
            }
        }

        Ok(())
    }

    /// Clean up old completed jobs
    pub async fn cleanup_old_jobs(&self, retention_hours: i64) {
        let cutoff = Utc::now() - chrono::Duration::hours(retention_hours);
        let mut jobs = self.jobs.write().await;

        jobs.retain(|_, job| {
            if let Some(completed_at) = job.completed_at {
                completed_at > cutoff
            } else {
                true // Keep jobs that haven't completed
            }
        });
    }

    /// Get queue statistics
    pub async fn get_stats(&self) -> JobQueueStats {
        let jobs = self.jobs.read().await;
        let processing = self.processing.lock().await;

        let queued = jobs
            .values()
            .filter(|j| j.status == JobStatus::Queued)
            .count();
        let completed = jobs
            .values()
            .filter(|j| j.status == JobStatus::Completed)
            .count();
        let failed = jobs
            .values()
            .filter(|j| j.status == JobStatus::Failed)
            .count();

        JobQueueStats {
            total_jobs: jobs.len(),
            queued,
            processing: processing.len(),
            completed,
            failed,
            max_concurrent: self.config.max_concurrent_jobs,
        }
    }
}

/// Job queue statistics
#[derive(Debug, Clone, Serialize)]
pub struct JobQueueStats {
    pub total_jobs: usize,
    pub queued: usize,
    pub processing: usize,
    pub completed: usize,
    pub failed: usize,
    pub max_concurrent: usize,
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_job_creation() {
        let job = IndexJob::new(
            vec!["1".to_string(), "2".to_string(), "3".to_string()],
            false,
        );
        assert_eq!(job.status, JobStatus::Queued);
        assert_eq!(job.progress.total, 3);
        assert_eq!(job.progress.completed, 0);
        assert!(!job.progress.is_complete());
    }

    #[test]
    fn test_job_lifecycle() {
        let mut job = IndexJob::new(
            vec!["1".to_string(), "2".to_string(), "3".to_string()],
            false,
        );

        // Start job
        job.start();
        assert_eq!(job.status, JobStatus::Processing);
        assert!(job.started_at.is_some());

        // Complete job
        job.complete();
        assert_eq!(job.status, JobStatus::Completed);
        assert!(job.completed_at.is_some());
    }

    #[test]
    fn test_job_failure() {
        let mut job = IndexJob::new(
            vec!["1".to_string(), "2".to_string(), "3".to_string()],
            false,
        );
        job.fail("Test error".to_string());

        assert_eq!(job.status, JobStatus::Failed);
        assert_eq!(job.error, Some("Test error".to_string()));
        assert!(job.can_retry());

        // Exceed retry limit
        job.retry_count = 3;
        assert!(!job.can_retry());
    }

    #[test]
    fn test_progress_tracking() {
        let mut progress = JobProgress::new(10);
        assert_eq!(progress.total, 10);
        assert_eq!(progress.success_rate(), 0.0);

        progress.completed = 7;
        progress.failed = 3;
        assert!(progress.is_complete());
        assert_eq!(progress.success_rate(), 0.7);
    }
}
