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

pub mod agent;
pub mod api;
pub mod audit;
pub mod auto_scanner;
pub mod backup;
pub mod cache;
pub mod cli;
pub mod code_chunker;
pub mod code_review;
pub mod config;
pub mod context_llm;
pub mod context_rag;
pub mod db;
pub mod directory_tree;
pub mod doc_generator;
// RC-CLEANUP-F: `enhanced_scanner` moved to `crate::scanner::enhanced`.
pub mod error;
pub mod formatter;
pub mod git;
pub mod github;
pub mod indexing;
pub mod llm;
pub mod llm_audit;
pub mod memory;
pub mod metrics;
pub mod multi_tenant;
pub mod parser;
pub mod prompt_hashes;
pub mod prompt_tier;
pub mod query_analytics;
pub mod query_router;
pub mod query_templates;
pub mod queue;
pub mod refactor_assistant;
// RC-CLEANUP-A: the five `repo_*` top-level modules
// (`repo_analysis`, `repo_cache`, `repo_cache_sql`, `repo_manager`,
// `repo_sync`) consolidated under `crate::repo::*`.
pub mod repo;
pub mod research;
pub mod scanner;
pub mod scoring;
pub mod search;
pub mod server;
pub mod static_analysis;
pub mod sync_scheduler;
pub mod tag_schema;
pub mod tags;
pub mod task;
pub mod task_executor;
pub mod task_watcher;
pub mod telemetry;
pub mod test_generator;
pub mod tests_runner;
pub mod todo;
// RC-CLEANUP-D: `todo_scanner` moved to `crate::todo::legacy_scanner`.
pub mod tree_state;
pub mod types;
pub mod webhooks;

pub use api::{
    ApiConfig, ApiResponse, ApiState, AuthConfig, AuthResult, IndexJobResponse, IndexJobStatus,
    IndexStatusResponse, JobQueue, JobQueueConfig, JobStatus, PaginatedResponse, RateLimitConfig,
    RateLimiter, SearchRequest, SearchResponse, SearchType, UploadDocumentRequest,
    UploadDocumentResponse, create_api_router, create_default_api_router, generate_api_key,
    hash_api_key,
};
// RC-CLEANUP-B: cache modules consolidated under `crate::cache::*`.
// Top-level re-exports preserved so external callers using
// `rustcode::CacheLayer`, `rustcode::ResponseCache`, etc. keep working.
pub use cache::{AuditCache, CacheEntry, CacheStats};
pub use cache::layer::{
    CacheConfig as CacheLayerConfig, CacheKey, CacheLayer, CacheStats as CacheLayerStats,
};
pub use cache::migrate::{
    CacheMigrator, MigrationFailure, MigrationProgress, MigrationResult,
};
pub use rag::{ChunkConfig, ChunkData, chunk_document};
pub use cli::{
    QueueCommands, ReportCommands, ScanCommands, TaskCommands, handle_queue_command,
    handle_report_command, handle_scan_command, handle_task_command,
};
pub use code_chunker::{
    ChunkerConfig, ChunkingStats, CodeChunk, CodeChunker, DedupEntry, DedupIndex, EntityType,
    compute_chunking_stats, compute_content_hash,
};
pub use code_review::{
    CodeReview, CodeReviewer, FileReview, IssueSeverity, ReviewIssue, ReviewStats,
};
pub use config::Config;
pub use context_llm::{ContextBuilder as OldContextBuilder, GlobalContextBundle};
pub use context_rag::{Context, ContextBuilder, ContextFile, QueryBuilder};
pub use llm::usage::costs::{
    BudgetStatus, CostStats, CostTracker, OperationCost, SavingsReport, StaticDecisionRecord,
    TokenUsage,
};
pub use db::{
    DbError, DbResult, DbStats, Note, Repository, Task, add_repository, create_note, create_task,
    delete_note, get_next_task, get_note, get_repository, get_repository_by_path, get_stats,
    init_db, list_notes, list_repositories, list_tasks, remove_repository, search_notes,
    update_note_status, update_repository_analysis, update_task_status,
};
pub use directory_tree::{DirectoryTreeBuilder, Hotspot, TreeSummary};
pub use doc_generator::{DocGenerator, FunctionDoc, ModuleDoc, ParameterDoc, ReadmeContent};
pub use rag::{
    Embedding, EmbeddingConfig, EmbeddingGenerator, EmbeddingModelType, EmbeddingStats,
};
// Re-exported from the new location (`crate::scanner::enhanced`) so
// `rustcode::EnhancedScanner` remains a valid external import.
pub use scanner::EnhancedScanner;
pub use error::{AuditError, Result};
pub use formatter::{BatchFormatResult, CodeFormatter, FormatMode, FormatResult, Formatter};
pub use git::GitManager;
pub use llm::grok_client::{FileScoreResult, GrokClient, QuickAnalysisResult};
pub use llm::grok_reasoning::{
    BatchAnalysisResult, FileAnalysisResult as GrokFileAnalysisResult, FileBatch, FileForAnalysis,
    GrokReasoningClient, IdentifiedIssue, Improvement, RetryConfig, analyze_all_batches,
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
pub use llm::config::{
    CacheConfig, FileSelectionConfig, LLM_CONFIG_FILE, LimitsConfig, LlmConfig, ProviderConfig,
    claude_models,
};
pub use query_router::{Action, QueryIntent, QueryRouter, RoutingStats, UserContext};
pub use query_templates::{QueryTemplate, TemplateCategory, TemplateRegistry};
pub use queue::{
    AnalysisResult, FileAnalysisResult as QueueFileAnalysisResult, LlmAnalyzer, ProcessorConfig,
    QueueProcessor, QueueStats, advance_stage, capture_note, capture_thought, capture_todo,
    enqueue, get_pending_items, get_queue_item, get_queue_stats, get_retriable_items, mark_failed,
    update_analysis,
};
pub use refactor_assistant::{
    CodeLocation, CodeSmell, CodeSmellType, EffortEstimate, PlanStep, RefactorAssistant,
    RefactoringAnalysis, RefactoringExample, RefactoringPlan, RefactoringPriority,
    RefactoringSuggestion, RefactoringType, Risk, SmellSeverity,
};
// Top-level re-exports routed through the consolidated `crate::repo::*`.
// External callers using `rustcode::{RepoAnalyzer, RepoCache, RepoCacheSql, ...}`
// keep working.
pub use repo::analysis::{
    FileMetadata, LanguageStats, RepoAnalyzer, RepoNodeType, RepoTree, TreeNode,
};
pub use repo::file_cache::{
    CacheSetParams, CacheStats as RepoCacheStats, CacheStrategy, CacheType, RepoCache,
    RepoCacheEntry,
};
pub use repo::cache::{
    CacheEntry as RepoCacheEntrySql, CacheStats as RepoCacheStatsSql, CacheTypeStats,
    EvictionPolicy, ModelStats, RepoCacheSql,
};

pub use metrics::{
    Counter, Gauge, Histogram, HistogramSummary, MetricsRegistry, MetricsStats, RequestTimer,
    global_registry, track_cache_hit, track_cache_miss, track_indexing_job, track_request,
    track_search,
};
pub use multi_tenant::{QuotaType, Tenant, TenantManager, TenantQuota, TenantUsage, UsageMetric};
pub use prompt_tier::{
    PromptRouter, PromptRouterConfig, PromptRoutingStats, PromptTier, TierKind,
};
pub use query_analytics::{
    AnalyticsConfig, AnalyticsStats, QueryAnalytics, QueryPattern, SearchAnalytics,
};
pub use cache::responses::{CacheStats as ResponseCacheStats, CachedResponse, ResponseCache};
pub use scanner::{
    DetectedTodo, GitHubRepo, ScanResult, Scanner, TreeNode as ScannerTreeNode, build_dir_tree,
    fetch_user_repos, get_dir_tree, get_unanalyzed_files, save_dir_tree, save_file_analysis,
    scan_directory_for_todos, scan_repo_for_todos, sync_repos_to_db,
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
    AnalysisRecommendation, BatchAnalysisReport, ClippyResult, ClippyWarning, FindingConfidence,
    QualitySignals, SecurityFinding, SkipReason, StaticAnalysisResult, StaticAnalyzer,
    StaticAnalyzerConfig, analyze_batch, content_hash, run_clippy, strip_for_prompt,
};
pub use tag_schema::{
    CodeAge, CodeStatus, Complexity, DirectoryNode, IssuesSummary, NodeStats, NodeType, Priority,
    SimpleIssueDetector, TagCategory, TagSchema, TagValidation,
};
pub use tags::TagScanner;
// `TaskGenerator` moved to `crate::audit::tasks` (RC-CLEANUP-D).
// Top-level re-export preserved so external callers using
// `rustcode::TaskGenerator` keep working.
pub use audit::tasks::TaskGenerator;
pub use telemetry::{TelemetryConfig, init_telemetry};
pub use test_generator::{
    Fixture, GeneratedTests, TestCase, TestFramework, TestGapAnalysis, TestGenerator, TestType,
    UntestFunction,
};
pub use tests_runner::{TestResults, TestRunner};
// Re-exported from `crate::todo::legacy_scanner` (was `crate::todo_scanner`
// before RC-CLEANUP-D). Top-level public API is unchanged for external
// callers using `rustcode::{TodoItem, TodoPriority, TodoScanner, TodoSummary}`.
pub use todo::legacy_scanner::{TodoItem, TodoPriority, TodoScanner, TodoSummary};
pub use llm::usage::budget::{
    BudgetConfig, ModelTokenStats, MonthlyTracker, TokenPricing, TokenStats,
};
pub use tree_state::{
    CategoryChangeSummary, ChangeType, DiffSummary, FileCategory, FileChange, FileState, TreeDiff,
    TreeState, TreeStateManager, TreeSummaryStats,
};
pub use types::*;
pub use rag::vector_index::{
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
        ApiConfig, ApiResponse, ApiState, AuthConfig, RateLimitConfig, SearchRequest, SearchType,
        create_api_router, create_default_api_router,
    };
    pub use rag::{ChunkConfig, ChunkData, chunk_document};
    pub use crate::code_chunker::{
        ChunkerConfig, ChunkingStats, CodeChunk, CodeChunker, DedupIndex, EntityType,
    };
    pub use crate::config::Config;
    pub use crate::context::global::{ContextBuilder as OldContextBuilder, GlobalContextBundle};
    pub use crate::context::rag::{Context, ContextBuilder, ContextFile, QueryBuilder};
    pub use crate::llm::usage::costs::{
        BudgetStatus, CostStats, CostTracker, OperationCost, SavingsReport, StaticDecisionRecord,
        TokenUsage,
    };
    pub use crate::db::{
        DbError, DbResult, DbStats, Note, Repository, Task, add_repository, create_note,
        create_task, delete_note, get_next_task, get_note, get_repository, get_repository_by_path,
        get_stats, init_db, list_notes, list_repositories, list_tasks, remove_repository,
        search_notes, update_note_status, update_repository_analysis, update_task_status,
    };
    pub use crate::directory_tree::{DirectoryTreeBuilder, Hotspot, TreeSummary};
    pub use rag::{
        Embedding, EmbeddingConfig, EmbeddingGenerator, EmbeddingModelType, EmbeddingStats,
    };
    pub use crate::scanner::enhanced::EnhancedScanner;
    pub use crate::error::{AuditError, Result};
    pub use crate::git::GitManager;
    pub use crate::llm::grok_client::{FileScoreResult, GrokClient, QuickAnalysisResult};
    pub use crate::llm::grok_reasoning::{
        BatchAnalysisResult, FileAnalysisResult as GrokFileAnalysisResult, FileBatch,
        FileForAnalysis, GrokReasoningClient, IdentifiedIssue, Improvement, RetryConfig,
        analyze_all_batches,
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
        AnalysisResult, FileAnalysisResult as QueueFileAnalysisResult, LlmAnalyzer,
        ProcessorConfig, QueueProcessor, QueueStats, advance_stage, capture_note, capture_thought,
        capture_todo, enqueue, get_pending_items, get_queue_item, get_queue_stats,
        get_retriable_items, mark_failed, update_analysis,
    };
    pub use crate::repo::analysis::{
        FileMetadata, LanguageStats, RepoAnalyzer, RepoNodeType, RepoTree, TreeNode,
    };
    pub use crate::repo::file_cache::{
        CacheStats as RepoCacheStats, CacheType, RepoCache, RepoCacheEntry,
    };
    pub use crate::cache::responses::{
        CacheStats as ResponseCacheStats, CachedResponse, ResponseCache,
    };
    pub use crate::scanner::{
        DetectedTodo, GitHubRepo, ScanResult, Scanner, TreeNode as ScannerTreeNode, build_dir_tree,
        fetch_user_repos, get_dir_tree, get_unanalyzed_files, save_dir_tree, save_file_analysis,
        scan_directory_for_todos, scan_repo_for_todos, sync_repos_to_db,
    };
    pub use crate::search::{
        SearchConfig, SearchFilters, SearchQuery, SearchResult, SearchResultMetadata, SearchStats,
        SemanticSearcher,
    };

    pub use crate::prompt_tier::{PromptRouter, PromptRouterConfig, PromptTier, TierKind};
    pub use crate::static_analysis::{
        AnalysisRecommendation, StaticAnalysisResult, StaticAnalyzer,
    };
    pub use crate::tag_schema::{
        CodeAge, CodeStatus, Complexity, DirectoryNode, IssuesSummary, NodeStats, NodeType,
        Priority, SimpleIssueDetector, TagCategory, TagSchema, TagValidation,
    };
    pub use crate::tags::TagScanner;
    pub use crate::audit::tasks::TaskGenerator;
    pub use crate::tests_runner::{TestResults, TestRunner};
    pub use crate::todo::legacy_scanner::{TodoItem, TodoPriority, TodoScanner, TodoSummary};
    pub use crate::tree_state::{
        CategoryChangeSummary, ChangeType, DiffSummary, FileCategory, FileChange, FileState,
        TreeDiff, TreeState, TreeStateManager, TreeSummaryStats,
    };
    pub use crate::types::*;
}
