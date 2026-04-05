// # RustCode - Developer Workflow Management System
//
// A Rust-based workflow manager for solo developers to track repos, capture ideas,
// and leverage LLM-powered insights.
//
// ## Features
//
// - **Note & Thought Capture**: Quick note input with tag-based categorization
// - **Repository Management**: Track GitHub repos with cached directory trees
// - **LLM-Powered Analysis**: Grok 4.1 API integration for code insights
// - **File Scoring**: Quality, security, and complexity assessment
// - **Task Generation**: Automatically generate actionable tasks
// - **Solo Dev Workflow**: Research → Planning → Prototype → Production
// - **RAG System**: Git-friendly vector storage for semantic search
// - **Cost Management**: Efficient LLM usage with budget controls
//
// ## Architecture
//
// - Static analysis for fast pattern detection
// - LLM integration for deep code insights
// - Git operations for repository tracking
// - Vector storage for RAG system
// - RESTful API and CLI interface

pub mod api;
pub mod audit;
pub mod backup;
pub mod cli;
pub mod db;
pub mod github;
pub mod llm;
pub mod queue;
pub mod research;
pub mod scanner;
pub mod task;
pub mod todo;
pub mod auto_scanner;
pub mod cache;
pub mod cache_layer;
pub mod cache_migrate;
pub mod chunking;
pub mod code_chunker;
pub mod code_review;
pub mod config;
pub mod context_llm;
pub mod context_rag;
pub mod cost_tracker;
pub mod directory_tree;
pub mod doc_generator;
pub mod embeddings;
pub mod enhanced_scanner;
pub mod error;
pub mod formatter;
pub mod git;
pub mod grok_client;
pub mod grok_reasoning;
pub mod indexing;
pub mod llm_audit;
pub mod llm_config;
pub mod metrics;
pub mod model_router;
pub mod multi_tenant;
pub mod ollama_client;
pub mod parser;
pub mod prompt_hashes;
pub mod prompt_router;
pub mod query_analytics;
pub mod query_router;
pub mod query_templates;
pub mod refactor_assistant;
pub mod repo_analysis;
pub mod repo_cache;
pub mod repo_cache_sql;
pub mod repo_manager;
pub mod repo_sync;
pub mod response_cache;
pub mod scoring;
pub mod search;
pub mod server;
pub mod static_analysis;
pub mod sync_scheduler;
pub mod tag_schema;
pub mod tags;
pub mod tasks;
pub mod telemetry;
pub mod test_generator;
pub mod tests_runner;
pub mod todo_scanner;
pub mod token_budget;
pub mod tree_state;
pub mod types;
pub mod vector_index;
pub mod webhooks;

pub use api::{
    create_api_router, create_default_api_router, generate_api_key, hash_api_key, ApiConfig,
    ApiResponse, ApiState, AuthConfig, AuthResult, IndexJobResponse, IndexJobStatus,
    IndexStatusResponse, JobQueue, JobQueueConfig, JobStatus, PaginatedResponse, RateLimitConfig,
    RateLimiter, SearchRequest, SearchResponse, SearchType, UploadDocumentRequest,
    UploadDocumentResponse,
};
pub use cache::{AuditCache, CacheEntry, CacheStats};
pub use cache_layer::{
    CacheConfig as CacheLayerConfig, CacheKey, CacheLayer, CacheStats as CacheLayerStats,
};
pub use cache_migrate::{CacheMigrator, MigrationFailure, MigrationProgress, MigrationResult};
pub use chunking::{chunk_document, ChunkConfig, ChunkData};
pub use cli::{
    handle_queue_command, handle_report_command, handle_scan_command, handle_task_command,
    QueueCommands, ReportCommands, ScanCommands, TaskCommands,
};
pub use code_chunker::{
    compute_chunking_stats, compute_content_hash, ChunkerConfig, ChunkingStats, CodeChunk,
    CodeChunker, DedupEntry, DedupIndex, EntityType,
};
pub use code_review::{
    CodeReview, CodeReviewer, FileReview, IssueSeverity, ReviewIssue, ReviewStats,
};
pub use config::Config;
pub use context_llm::{ContextBuilder as OldContextBuilder, GlobalContextBundle};
pub use context_rag::{Context, ContextBuilder, ContextFile, QueryBuilder};
pub use cost_tracker::{
    BudgetStatus, CostStats, CostTracker, OperationCost, SavingsReport, StaticDecisionRecord,
    TokenUsage,
};
pub use db::{
    add_repository, create_note, create_task, delete_note, get_next_task, get_note, get_repository,
    get_repository_by_path, get_stats, init_db, list_notes, list_repositories, list_tasks,
    remove_repository, search_notes, update_note_status, update_repository_analysis,
    update_task_status, DbError, DbResult, DbStats, Note, Repository, Task,
};
pub use directory_tree::{DirectoryTreeBuilder, Hotspot, TreeSummary};
pub use doc_generator::{DocGenerator, FunctionDoc, ModuleDoc, ParameterDoc, ReadmeContent};
pub use embeddings::{
    Embedding, EmbeddingConfig, EmbeddingGenerator, EmbeddingModelType, EmbeddingStats,
};
pub use enhanced_scanner::EnhancedScanner;
pub use error::{AuditError, Result};
pub use formatter::{BatchFormatResult, CodeFormatter, FormatMode, FormatResult, Formatter};
pub use git::GitManager;
pub use grok_client::{FileScoreResult, GrokClient, QuickAnalysisResult};
pub use grok_reasoning::{
    analyze_all_batches, BatchAnalysisResult, FileAnalysisResult as GrokFileAnalysisResult,
    FileBatch, FileForAnalysis, GrokReasoningClient, IdentifiedIssue, Improvement, RetryConfig,
};
pub use indexing::{
    BatchIndexer, DocumentIndexer, IndexingConfig, IndexingProgress, IndexingResult, IndexingStage,
};
pub use llm::{
    GrokAnalyzer, ProjectPhase, ProjectPlan, StandardizationIssue, StandardizationReport,
    TodoAnalysis,
};
pub use llm_audit::{
    ArchitectureInsights, AuditMode, FileAnalysis, FileLlmAnalysis, FileRelationships,
    FullAuditResult, LlmAuditor, MasterReview, Recommendation, RegularAuditResult, SecurityConcern,
    TechDebtArea,
};
pub use llm_config::{
    claude_models, CacheConfig, FileSelectionConfig, LimitsConfig, LlmConfig, ProviderConfig,
    LLM_CONFIG_FILE,
};
pub use query_router::{Action, QueryIntent, QueryRouter, RoutingStats, UserContext};
pub use query_templates::{QueryTemplate, TemplateCategory, TemplateRegistry};
pub use queue::{
    advance_stage, capture_note, capture_thought, capture_todo, enqueue, get_pending_items,
    get_queue_item, get_queue_stats, get_retriable_items, mark_failed, update_analysis,
    AnalysisResult, FileAnalysisResult as QueueFileAnalysisResult, LlmAnalyzer, ProcessorConfig,
    QueueProcessor, QueueStats,
};
pub use refactor_assistant::{
    CodeLocation, CodeSmell, CodeSmellType, EffortEstimate, PlanStep, RefactorAssistant,
    RefactoringAnalysis, RefactoringExample, RefactoringPlan, RefactoringPriority,
    RefactoringSuggestion, RefactoringType, Risk, SmellSeverity,
};
pub use repo_analysis::{
    FileMetadata, LanguageStats, RepoAnalyzer, RepoNodeType, RepoTree, TreeNode,
};
pub use repo_cache::{
    CacheSetParams, CacheStats as RepoCacheStats, CacheStrategy, CacheType, RepoCache,
    RepoCacheEntry,
};
pub use repo_cache_sql::{
    CacheEntry as RepoCacheEntrySql, CacheStats as RepoCacheStatsSql, CacheTypeStats,
    EvictionPolicy, ModelStats, RepoCacheSql,
};

pub use metrics::{
    global_registry, track_cache_hit, track_cache_miss, track_indexing_job, track_request,
    track_search, Counter, Gauge, Histogram, HistogramSummary, MetricsRegistry, MetricsStats,
    RequestTimer,
};
pub use multi_tenant::{QuotaType, Tenant, TenantManager, TenantQuota, TenantUsage, UsageMetric};
pub use prompt_router::{
    PromptRouter, PromptRouterConfig, PromptRoutingStats, PromptTier, TierKind,
};
pub use query_analytics::{
    AnalyticsConfig, AnalyticsStats, QueryAnalytics, QueryPattern, SearchAnalytics,
};
pub use response_cache::{CacheStats as ResponseCacheStats, CachedResponse, ResponseCache};
pub use scanner::{
    build_dir_tree, fetch_user_repos, get_dir_tree, get_unanalyzed_files, save_dir_tree,
    save_file_analysis, scan_directory_for_todos, scan_repo_for_todos, sync_repos_to_db,
    DetectedTodo, GitHubRepo, ScanResult, Scanner, TreeNode as ScannerTreeNode,
};
pub use scoring::{
    CodebaseScore, ComplexityIndicators, FileScore, FileScorer, ScoreBreakdown, ScoringWeights,
    TodoBreakdown,
};
pub use search::{
    SearchConfig, SearchFilters, SearchQuery, SearchResult, SearchResultMetadata, SearchStats,
    SemanticSearcher,
};
pub use server::run_server;
pub use static_analysis::{
    analyze_batch, content_hash, run_clippy, strip_for_prompt, AnalysisRecommendation,
    BatchAnalysisReport, ClippyResult, ClippyWarning, FindingConfidence, QualitySignals,
    SecurityFinding, SkipReason, StaticAnalysisResult, StaticAnalyzer, StaticAnalyzerConfig,
};
pub use tag_schema::{
    CodeAge, CodeStatus, Complexity, DirectoryNode, IssuesSummary, NodeStats, NodeType, Priority,
    SimpleIssueDetector, TagCategory, TagSchema, TagValidation,
};
pub use tags::TagScanner;
pub use tasks::TaskGenerator;
pub use telemetry::{init_telemetry, TelemetryConfig};
pub use test_generator::{
    Fixture, GeneratedTests, TestCase, TestFramework, TestGapAnalysis, TestGenerator, TestType,
    UntestFunction,
};
pub use tests_runner::{TestResults, TestRunner};
pub use todo_scanner::{TodoItem, TodoPriority, TodoScanner, TodoSummary};
pub use token_budget::{BudgetConfig, ModelTokenStats, MonthlyTracker, TokenPricing, TokenStats};
pub use tree_state::{
    CategoryChangeSummary, ChangeType, DiffSummary, FileCategory, FileChange, FileState, TreeDiff,
    TreeState, TreeStateManager, TreeSummaryStats,
};
pub use types::*;
pub use vector_index::{
    DistanceMetric, IndexConfig as VectorIndexConfig, SearchResult as VectorSearchResult,
    VectorIndex,
};
pub use webhooks::{
    DeliveryStatus, WebhookConfig, WebhookDelivery, WebhookEndpoint, WebhookEvent, WebhookManager,
    WebhookPayload,
};

// Re-export commonly used types
pub mod prelude {
    pub use crate::api::{
        create_api_router, create_default_api_router, ApiConfig, ApiResponse, ApiState, AuthConfig,
        RateLimitConfig, SearchRequest, SearchType,
    };
    pub use crate::chunking::{chunk_document, ChunkConfig, ChunkData};
    pub use crate::code_chunker::{
        ChunkerConfig, ChunkingStats, CodeChunk, CodeChunker, DedupIndex, EntityType,
    };
    pub use crate::config::Config;
    pub use crate::context_llm::{ContextBuilder as OldContextBuilder, GlobalContextBundle};
    pub use crate::context_rag::{Context, ContextBuilder, ContextFile, QueryBuilder};
    pub use crate::cost_tracker::{
        BudgetStatus, CostStats, CostTracker, OperationCost, SavingsReport, StaticDecisionRecord,
        TokenUsage,
    };
    pub use crate::db::{
        add_repository, create_note, create_task, delete_note, get_next_task, get_note,
        get_repository, get_repository_by_path, get_stats, init_db, list_notes, list_repositories,
        list_tasks, remove_repository, search_notes, update_note_status,
        update_repository_analysis, update_task_status, DbError, DbResult, DbStats, Note,
        Repository, Task,
    };
    pub use crate::directory_tree::{DirectoryTreeBuilder, Hotspot, TreeSummary};
    pub use crate::embeddings::{
        Embedding, EmbeddingConfig, EmbeddingGenerator, EmbeddingModelType, EmbeddingStats,
    };
    pub use crate::enhanced_scanner::EnhancedScanner;
    pub use crate::error::{AuditError, Result};
    pub use crate::git::GitManager;
    pub use crate::grok_client::{FileScoreResult, GrokClient, QuickAnalysisResult};
    pub use crate::grok_reasoning::{
        analyze_all_batches, BatchAnalysisResult, FileAnalysisResult as GrokFileAnalysisResult,
        FileBatch, FileForAnalysis, GrokReasoningClient, IdentifiedIssue, Improvement, RetryConfig,
    };
    pub use crate::indexing::{
        BatchIndexer, DocumentIndexer, IndexingConfig, IndexingProgress, IndexingResult,
        IndexingStage,
    };
    pub use crate::llm::{
        GrokAnalyzer, ProjectPhase, ProjectPlan, StandardizationIssue, StandardizationReport,
        TodoAnalysis,
    };
    pub use crate::query_router::{Action, QueryIntent, QueryRouter, RoutingStats, UserContext};
    pub use crate::query_templates::{QueryTemplate, TemplateCategory, TemplateRegistry};
    pub use crate::queue::{
        advance_stage, capture_note, capture_thought, capture_todo, enqueue, get_pending_items,
        get_queue_item, get_queue_stats, get_retriable_items, mark_failed, update_analysis,
        AnalysisResult, FileAnalysisResult as QueueFileAnalysisResult, LlmAnalyzer,
        ProcessorConfig, QueueProcessor, QueueStats,
    };
    pub use crate::repo_analysis::{
        FileMetadata, LanguageStats, RepoAnalyzer, RepoNodeType, RepoTree, TreeNode,
    };
    pub use crate::repo_cache::{
        CacheStats as RepoCacheStats, CacheType, RepoCache, RepoCacheEntry,
    };
    pub use crate::response_cache::{
        CacheStats as ResponseCacheStats, CachedResponse, ResponseCache,
    };
    pub use crate::scanner::{
        build_dir_tree, fetch_user_repos, get_dir_tree, get_unanalyzed_files, save_dir_tree,
        save_file_analysis, scan_directory_for_todos, scan_repo_for_todos, sync_repos_to_db,
        DetectedTodo, GitHubRepo, ScanResult, Scanner, TreeNode as ScannerTreeNode,
    };
    pub use crate::search::{
        SearchConfig, SearchFilters, SearchQuery, SearchResult, SearchResultMetadata, SearchStats,
        SemanticSearcher,
    };

    pub use crate::prompt_router::{PromptRouter, PromptRouterConfig, PromptTier, TierKind};
    pub use crate::static_analysis::{
        AnalysisRecommendation, StaticAnalysisResult, StaticAnalyzer,
    };
    pub use crate::tag_schema::{
        CodeAge, CodeStatus, Complexity, DirectoryNode, IssuesSummary, NodeStats, NodeType,
        Priority, SimpleIssueDetector, TagCategory, TagSchema, TagValidation,
    };
    pub use crate::tags::TagScanner;
    pub use crate::tasks::TaskGenerator;
    pub use crate::tests_runner::{TestResults, TestRunner};
    pub use crate::todo_scanner::{TodoItem, TodoPriority, TodoScanner, TodoSummary};
    pub use crate::tree_state::{
        CategoryChangeSummary, ChangeType, DiffSummary, FileCategory, FileChange, FileState,
        TreeDiff, TreeState, TreeStateManager, TreeSummaryStats,
    };
    pub use crate::types::*;
}
