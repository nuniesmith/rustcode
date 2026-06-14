// API state for the RAG subsystem.
//
// The document/search/index HTTP handlers and the unmounted document-RAG router
// (create_api_router / create_default_api_router / ApiServer) were removed — the
// live server serves RAG through `repo_router` (/api/v1) and `proxy_router` (/v1).
// Only `ApiState` remains here, because the `todo` subsystem constructs it.

use std::sync::Arc;

use crate::indexing::IndexingConfig;
use crate::search::{SearchConfig, SemanticSearcher};
use rag::EmbeddingGenerator;
use sqlx::PgPool;

#[derive(Clone)]
pub struct ApiState {
    pub db_pool: PgPool,
    pub embedding_generator: Arc<tokio::sync::Mutex<EmbeddingGenerator>>,
    pub searcher: Arc<SemanticSearcher>,
    pub job_queue: Arc<super::jobs::JobQueue>,
    pub start_time: std::time::SystemTime,
}

impl ApiState {
    pub async fn new(
        db_pool: PgPool,
        embedding_generator: Arc<tokio::sync::Mutex<EmbeddingGenerator>>,
        indexing_config: IndexingConfig,
        job_queue_config: super::jobs::JobQueueConfig,
    ) -> Self {
        let searcher = Arc::new(
            SemanticSearcher::new(SearchConfig::default())
                .await
                .expect("Failed to create semantic searcher"),
        );

        let job_queue = Arc::new(super::jobs::JobQueue::new(
            job_queue_config,
            db_pool.clone(),
            embedding_generator.clone(),
            indexing_config,
        ));

        Self {
            db_pool,
            embedding_generator,
            searcher,
            job_queue,
            start_time: std::time::SystemTime::now(),
        }
    }
}
