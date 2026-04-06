// Axum API server for the audit service + RustCode dashboard

use crate::api::auth::require_api_key;
use crate::api::proxy::{ProxyState, proxy_router};
use crate::api::repos::{RepoAppState, repo_router};
use crate::audit::endpoint::{AuditState, audit_router};
use crate::auto_scanner::{AutoScannerConfig, AutoScanner};
use crate::config::Config;
use crate::db::{self, Database,Repository, init_db};
use crate::error::{AuditError, Result};
use crate::git::GitManager;
use crate::github::webhook::{WebhookHandler, WebhookPayload};
use crate::llm::LlmClient;
use crate::model_router::{ModelRouter, ModelRouterConfig};
use crate::queue::{QueueStats, get_queue_stats};
use crate::repo_sync::RepoSyncService;
use crate::research::worker::refresh_rag_index;
use crate::scanner::Scanner;
use crate::scanner::github::sync_repos_to_db;
use crate::sync_scheduler::{SyncScheduler, SyncSchedulerConfig};
use crate::tags::TagScanner;
use crate::task_executor::{TaskExecutor, TaskExecutorOptions};
use crate::task_watcher::{WatchedTaskFile, TaskWatcherConfig,watch_tasks_directory};

use crate::types::{AuditRequest, AuditTag};
use axum::{
    Router,
    extract::{Json, Path, Query, State},
    http::{HeaderMap, Method, StatusCode, header},
    middleware,
    response::{IntoResponse, Response},
    routing::{get, post, put},
};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tower_http::cors::CorsLayer;
use tower_http::trace::TraceLayer;
use tracing::{info, warn};

// Application state shared across handlers
#[derive(Clone)]
pub struct AppState {
    config: Arc<Config>,
    pub(crate) git_manager: Arc<GitManager>,
    #[allow(dead_code)]
    llm_client: Option<Arc<LlmClient>>,
    pub(crate) db_pool: PgPool,
}

impl AppState {
    // Create new application state
    pub async fn new(config: Config) -> Result<Self> {
        let git_manager = Arc::new(GitManager::new(
            config.git.workspace_dir.clone(),
            config.git.shallow_clone,
        )?);

        let llm_client = if config.llm.enabled {
            if let Some(api_key) = &config.llm.api_key {
                let client = LlmClient::new_with_provider(
                    api_key.clone(),
                    config.llm.provider.clone(),
                    config.llm.model.clone(),
                    config.llm.max_tokens,
                    config.llm.temperature,
                )?;
                Some(Arc::new(client))
            } else {
                return Err(AuditError::config("LLM enabled but no API key provided"));
            }
        } else {
            None
        };

        // Initialize database — URL is centralised in Config::database.url
        let db_pool = db::init_db(&config.database.url)
            .await
            .map_err(|e| AuditError::other(format!("Failed to initialize database: {}", e)))?;

        Ok(Self {
            config: Arc::new(config),
            git_manager,
            llm_client,
            db_pool,
        })
    }
}

// Run the audit server
// Combined state for the GitHub webhook handler (requires both `AppState` and
// the `RepoSyncService` so it can trigger a repo sync on push events).
#[derive(Clone)]
struct WebhookState {
    sync_service: Arc<tokio::sync::RwLock<RepoSyncService>>,
    webhook_secret: String,
}

pub async fn run_server(config: Config) -> Result<()> {
    let addr = format!("{}:{}", config.server.host, config.server.port);
    let socket_addr: SocketAddr = addr
        .parse()
        .map_err(|e| AuditError::config(format!("Invalid server address: {}", e)))?;

    info!("Starting RustCode server on {}", socket_addr);

    // Initialize tracing (try_init to avoid panic if already initialized by caller)
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .try_init();

    // Create application state
    let state = AppState::new(config.clone()).await?;

    // ------------------------------------------------------------------
    // Build RepoSyncService + ModelRouter + SyncScheduler
    // ------------------------------------------------------------------

    // Re-use the same PostgreSQL pool that the rest of the app already has.
    // This ensures registered_repos rows land in the same database.
    let sync_repo_service = {
        match init_db(&config.database.url).await {
            Ok(pool) => {
                let mut svc = RepoSyncService::with_db(pool);
                match svc.load_from_db().await {
                    Ok(n) => info!(count = n, "Loaded persisted repos from PostgreSQL"),
                    Err(e) => {
                        warn!(error = %e, "Failed to load repos from PostgreSQL — starting empty")
                    }
                }
                svc
            }
            Err(e) => {
                warn!(error = %e, "Could not open DB for RepoSyncService — using in-memory only");
                RepoSyncService::new()
            }
        }
    };

    let sync_service = Arc::new(tokio::sync::RwLock::new(sync_repo_service));

    let model_router = Arc::new(ModelRouter::new(ModelRouterConfig {
        remote_api_key: config.model.xai_api_key.clone().unwrap_or_default(),
        remote_model: config.model.remote_model.clone(),
        local_model: config.model.local_model.clone(),
        local_base_url: config.model.ollama_base_url.clone(),
        force_remote: config.model.force_remote,
        fallback_to_remote: true,
    }));

    // Build GrokClient for the repo chat handler (None when XAI_API_KEY is unset).
    // Re-use the already-initialised PgPool from AppState so we don't open a second
    // connection pool. Database::from_pool wraps an existing PgPool without reconnecting.
    let grok_for_repo: Option<Arc<crate::grok_client::GrokClient>> =
        match config.model.xai_api_key.clone().filter(|k| !k.is_empty()) {
            Some(api_key) => {
                let db = Database::from_pool(state.db_pool.clone());
                let client = crate::grok_client::GrokClient::new(api_key, db);
                info!("GrokClient ready for repo chat handler (sharing existing PgPool)");
                Some(Arc::new(client))
            }
            None => {
                info!("XAI_API_KEY not set — repo chat will use local model only");
                None
            }
        };

    // Clone before moving into RepoAppState so AuditState can share the same client
    let _grok_for_audit = grok_for_repo.clone();

    let repo_app_state = RepoAppState::from_env(
        Arc::clone(&sync_service),
        Arc::clone(&model_router),
        grok_for_repo,
    )
    .await;

    // Start background sync scheduler
    SyncScheduler::new(
        SyncSchedulerConfig {
            interval: std::time::Duration::from_secs(config.git.sync_interval_secs),
            ..SyncSchedulerConfig::default()
        },
        Arc::clone(&sync_service),
    )
    .start();
    info!(
        interval_secs = config.git.sync_interval_secs,
        "SyncScheduler started"
    );

    // ------------------------------------------------------------------
    // Bootstrap RAG index from existing embeddings in Postgres.
    // Run in the background so it doesn't block server startup.
    // ------------------------------------------------------------------
    {
        let rag_pool = state.db_pool.clone();
        tokio::spawn(async move {
            match refresh_rag_index(&rag_pool).await {
                Ok(n) => info!(vectors = n, "RAG index loaded at startup"),
                Err(e) => {
                    warn!(error = %e, "RAG index failed to load at startup — RAG will be empty until next refresh")
                }
            }
        });
    }

    // ------------------------------------------------------------------
    // Build AuditState for the new audit router
    // ------------------------------------------------------------------
    let audit_db = Database::from_pool(state.db_pool.clone());
    let audit_state = Arc::new(AuditState::from_env(audit_db).await);

    // ------------------------------------------------------------------
    // Build WebhookState for the GitHub push-event → sync trigger
    // ------------------------------------------------------------------
    let webhook_state = WebhookState {
        sync_service: Arc::clone(&sync_service),
        webhook_secret: config.security.webhook_secret.clone(),
    };

    // SECURITY: Configure restrictive CORS policy
    let cors = build_cors_layer();

    // ------------------------------------------------------------------
    // Compose routers
    // ------------------------------------------------------------------
    // ------------------------------------------------------------------
    // Public routes — no authentication required
    // ------------------------------------------------------------------
    let public_routes = Router::new()
        .route("/health", get(health_check))
        .route("/healthz", get(health_check));

    // ------------------------------------------------------------------
    // Protected routes — require API key when RUSTCODE_PROXY_API_KEYS is set
    // ------------------------------------------------------------------
    let protected_routes = Router::new()
        // Legacy endpoints (AppState)
        .route("/api/clone", post(clone_repository))
        .route("/api/scan/tags", post(scan_tags))
        .route("/api/scan/static", post(scan_static))
        .route("/api/repos", get(list_repos))
        .route("/api/repos/scan", post(scan_repos))
        .route("/api/queue/status", get(queue_status))
        .route("/api/github/stats", get(github_stats))
        .route("/api/github/repos", get(github_repos))
        .route("/api/github/issues", get(github_issues))
        .route("/api/github/prs", get(github_prs))
        .route("/api/github/search", get(github_search))
        .route("/api/github/sync", post(github_sync))
        // Task & stats endpoints (consolidated from bin/server.rs)
        .route("/api/tasks", get(list_tasks_handler))
        .route("/api/tasks/next", get(get_next_task_handler))
        .route("/api/tasks/{id}", put(update_task_handler))
        .route("/api/stats", get(get_statistics))
        .with_state(state.clone())
        // New audit pipeline (AuditState) — replaces the legacy /api/audit routes
        .merge(audit_router(audit_state))
        // GitHub push-event webhook → repo sync trigger
        .route(
            "/api/github/webhook",
            post(handle_github_webhook).with_state(webhook_state),
        )
        // Repo management + chat API at /api/v1
        .nest("/api/v1", repo_router(repo_app_state.clone()))
        // OpenAI-compatible proxy at /v1  (for external apps e.g. futures trading bot)
        .nest("/v1", proxy_router(ProxyState::new(repo_app_state)))
        // Auth middleware — no-op when RUSTCODE_PROXY_API_KEYS is unset (dev mode)
        .layer(middleware::from_fn(require_api_key));

    let app = public_routes
        .merge(protected_routes)
        // Global middleware (applied last, wraps everything)
        .layer(cors)
        .layer(TraceLayer::new_for_http());

    if crate::api::auth::auth_disabled() {
        info!("API key auth is DISABLED (RUSTCODE_PROXY_API_KEYS not set — dev mode)");
    } else {
        info!("API key auth is ENABLED — all /api/* and /v1/* routes require Bearer token");
    }

    // ------------------------------------------------------------------
    // Start auto-scanner in background if enabled
    // ------------------------------------------------------------------
    std::fs::create_dir_all(&config.git.repos_dir)
        .map_err(|e| AuditError::other(format!("Failed to create repos directory: {}", e)))?;

    let scanner_config = AutoScannerConfig {
        enabled: config.auto_scan.enabled,
        default_interval_minutes: config.auto_scan.interval_minutes,
        max_concurrent_scans: config.auto_scan.max_concurrent,
        scan_cost_budget: config.auto_scan.cost_budget,
    };

    if scanner_config.enabled {
        info!(
            "🔍 Starting auto-scanner (interval: {} minutes)",
            scanner_config.default_interval_minutes
        );
        let scanner = Arc::new(AutoScanner::new(
            scanner_config,
            state.db_pool.clone(),
            config.git.repos_dir.clone(),
        ));
        let scanner_clone = scanner.clone();
        tokio::spawn(async move {
            if let Err(e) = scanner_clone.start().await {
                tracing::error!("Auto-scanner error: {}", e);
            }
        });
    } else {
        info!("Auto-scanner is disabled");
    }

    // Start task watcher in background if enabled
    if config.task_watcher.enabled {
        info!("Starting task watcher");
        let (task_tx, task_rx) = tokio::sync::mpsc::channel(100);
        let tasks_dir = PathBuf::from("tasks");
        tokio::spawn(async move {
            if let Err(e) = watch_tasks_directory(tasks_dir, task_tx).await {
                tracing::error!("Task watcher error: {}", e);
            }
        });
        let task_executor = Arc::new(TaskExecutor::new(
            TaskExecutorOptions {
                workspace_dir: config.git.repos_dir.clone(),
                dry_run: true,
            },
            state.git_manager.clone(),
        ));
        tokio::spawn(async move {
            while let Some(watched_task) = task_rx.recv().await {
                match task_executor.execute_dry_run(&watched_task.task).await {
                    Ok(()) => info!(
                        "Task {} executed successfully in dry-run",
                        watched_task.task.id
                    ),
                    Err(e) => {
                        tracing::error!("Task {} execution failed: {}", watched_task.task.id, e)
                    }
                }
            }
        });
    } else {
        info!("Task watcher is disabled");
    }

    info!("RustCode API-only server on http://{}/", socket_addr);
    // Start task watcher in background if enabled
    if config.task_watcher.enabled {
        info!("Starting task watcher");
        let (task_tx, task_rx) = tokio::sync::mpsc::channel(100);
        let tasks_dir = PathBuf::from("tasks");
        tokio::spawn(async move {
            if let Err(e) = watch_tasks_directory(tasks_dir, task_tx).await {
                tracing::error!("Task watcher error: {}", e);
            }
        });
        let task_executor = Arc::new(TaskExecutor::new(
            TaskExecutorOptions {
                workspace_dir: config.git.repos_dir.clone(),
                dry_run: true,
            },
            state.git_manager.clone(),
        ));
        tokio::spawn(async move {
            while let Some(watched_task) = task_rx.recv().await {
                match task_executor.execute_dry_run(&watched_task.task).await {
                    Ok(()) => info!(
                        "Task {} executed successfully in dry-run",
                        watched_task.task.id
                    ),
                    Err(e) => {
                        tracing::error!("Task {} execution failed: {}", watched_task.task.id, e)
                    }
                }
            }
        });
    } else {
        info!("Task watcher is disabled");
    }

    // Start task watcher in background if enabled
    if config.task_watcher.enabled {
        info!("Starting task watcher");
        let (task_tx, task_rx) = tokio::sync::mpsc::channel(100);
        let tasks_dir = PathBuf::from("tasks");
        tokio::spawn(async move {
            if let Err(e) = watch_tasks_directory(tasks_dir, task_tx).await {
                tracing::error!("Task watcher error: {}", e);
            }
        });
        let task_executor = Arc::new(TaskExecutor::new(
            TaskExecutorOptions {
                workspace_dir: config.git.repos_dir.clone(),
                dry_run: true,
            },
            state.git_manager.clone(),
        ));
        tokio::spawn(async move {
            while let Some(watched_task) = task_rx.recv().await {
                match task_executor.execute_dry_run(&watched_task.task).await {
                    Ok(()) => info!(
                        "Task {} executed successfully in dry-run",
                        watched_task.task.id
                    ),
                    Err(e) => {
                        tracing::error!("Task {} execution failed: {}", watched_task.task.id, e)
                    }
                }
            }
        });
    } else {
        info!("Task watcher is disabled");
    }

    info!("RustCode API-only server on http://{}/", socket_addr);
    // Start task watcher in background if enabled
    if config.task_watcher.enabled {
        info!("Starting task watcher");
        let (task_tx, task_rx) = tokio::sync::mpsc::channel(100);
        let tasks_dir = PathBuf::from("tasks");
        tokio::spawn(async move {
            if let Err(e) = watch_tasks_directory(tasks_dir, task_tx).await {
                tracing::error!("Task watcher error: {}", e);
            }
        });
        let task_executor = Arc::new(TaskExecutor::new(
            TaskExecutorOptions {
                workspace_dir: config.git.repos_dir.clone(),
                dry_run: true,
            },
            state.git_manager.clone(),
        ));
        tokio::spawn(async move {
            while let Some(watched_task) = task_rx.recv().await {
                match task_executor.execute_dry_run(&watched_task.task).await {
                    Ok(()) => info!(
                        "Task {} executed successfully in dry-run",
                        watched_task.task.id
                    ),
                    Err(e) => {
                        tracing::error!("Task {} execution failed: {}", watched_task.task.id, e)
                    }
                }
            }
        });
    } else {
        info!("Task watcher is disabled");
    }

    info!("RustCode API-only server on http://{}/", socket_addr);
    // Start task watcher in background if enabled
    if config.task_watcher.enabled {
        info!("Starting task watcher");
        let (task_tx, task_rx) = tokio::sync::mpsc::channel(100);
        let tasks_dir = PathBuf::from("tasks");
        tokio::spawn(async move {
            if let Err(e) = watch_tasks_directory(tasks_dir, task_tx).await {
                tracing::error!("Task watcher error: {}", e);
            }
        });
        let task_executor = Arc::new(TaskExecutor::new(
            TaskExecutorOptions {
                workspace_dir: config.git.repos_dir.clone(),
                dry_run: true,
            },
            state.git_manager.clone(),
        ));
        tokio::spawn(async move {
            while let Some(watched_task) = task_rx.recv().await {
                match task_executor.execute_dry_run(&watched_task.task).await {
                    Ok(()) => info!(
                        "Task {} executed successfully in dry-run",
                        watched_task.task.id
                    ),
                    Err(e) => {
                        tracing::error!("Task {} execution failed: {}", watched_task.task.id, e)
                    }
                }
            }
        });
    } else {
        info!("Task watcher is disabled");
    }

    info!("RustCode API-only server on http://{}/", socket_addr);
    // Start task watcher in background if enabled
    if config.task_watcher.enabled {
        info!("Starting task watcher");
        let (task_tx, task_rx) = tokio::sync::mpsc::channel(100);
        let tasks_dir = PathBuf::from("tasks");
        tokio::spawn(async move {
            if let Err(e) = watch_tasks_directory(tasks_dir, task_tx).await {
                tracing::error!("Task watcher error: {}", e);
            }
        });
        let task_executor = Arc::new(TaskExecutor::new(
            TaskExecutorOptions {
                workspace_dir: config.git.repos_dir.clone(),
                dry_run: true,
            },
            state.git_manager.clone(),
        ));
        tokio::spawn(async move {
            while let Some(watched_task) = task_rx.recv().await {
                match task_executor.execute_dry_run(&watched_task.task).await {
                    Ok(()) => info!(
                        "Task {} executed successfully in dry-run",
                        watched_task.task.id
                    ),
                    Err(e) => {
                        tracing::error!("Task {} execution failed: {}", watched_task.task.id, e)
                    }
                }
            }
        });
    } else {
        info!("Task watcher is disabled");
    }

    info!("RustCode API-only server on http://{}/", socket_addr);
    info!(
        "OpenAI-compatible proxy http://{}/v1/chat/completions",
        socket_addr
    );
    info!("Health check            http://{}/healthz", socket_addr);

    // Start server
    let listener = tokio::net::TcpListener::bind(&socket_addr)
        .await
        .map_err(|e| AuditError::other(format!("Failed to bind to {}: {}", socket_addr, e)))?;

    axum::serve(listener, app)
        .await
        .map_err(|e| AuditError::other(format!("Server error: {}", e)))?;

    Ok(())
}

// Build a restrictive CORS layer
//
// SECURITY: This replaces the previous `CorsLayer::permissive()` which allowed
// any origin to make requests, exposing the API to CSRF/XSS attacks.
fn build_cors_layer() -> CorsLayer {
    // Get allowed origins from environment or use defaults
    let allowed_origins: Vec<String> = std::env::var("CORS_ALLOWED_ORIGINS")
        .map(|s| s.split(',').map(|o| o.trim().to_string()).collect())
        .unwrap_or_else(|_| {
            vec![
                "http://localhost:3000".to_string(),
                "http://localhost:8080".to_string(),
                "http://127.0.0.1:3000".to_string(),
                "http://127.0.0.1:8080".to_string(),
                // Tailscale IP — allows Zed and other local-network clients on oryx
                "http://100.113.72.63:8080".to_string(),
            ]
        });

    info!("CORS allowed origins: {:?}", allowed_origins);

    // Build the CORS layer with restrictive settings
    CorsLayer::new()
        // Only allow specific origins (not wildcard)
        .allow_origin(
            allowed_origins
                .iter()
                .filter_map(|o| o.parse().ok())
                .collect::<Vec<header::HeaderValue>>(),
        )
        // Only allow specific HTTP methods
        .allow_methods([Method::GET, Method::POST, Method::OPTIONS])
        // Only allow specific headers
        .allow_headers([header::CONTENT_TYPE, header::AUTHORIZATION, header::ACCEPT])
        // Don't allow credentials by default (enable explicitly if needed)
        .allow_credentials(false)
        // Cache preflight requests for 1 hour
        .max_age(Duration::from_secs(3600))
}

// ============================================================================
// GitHub Webhook → Repo Sync
// ============================================================================

// `POST /api/github/webhook`
//
// Receives GitHub push (and other) webhook events and triggers a
// `RepoSyncService::sync` for any registered repo whose `remote_url`
// matches the repository in the event payload.
//
// The endpoint always returns **200 OK** quickly — the sync itself runs in a
// background `tokio::spawn` so GitHub doesn't time out waiting for us.
async fn handle_github_webhook(
    State(wh_state): State<WebhookState>,
    headers: HeaderMap,
    body: String,
) -> impl IntoResponse {
    use crate::github::webhook::WebhookEvent;

    let event_type = headers
        .get("x-github-event")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("unknown")
        .to_string();

    let delivery_id = headers
        .get("x-github-delivery")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("unknown")
        .to_string();

    let signature = headers
        .get("x-hub-signature-256")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());

    let payload = WebhookPayload::new(&event_type, &delivery_id, signature, &body);

    // Verify signature when a secret is configured.
    if !wh_state.webhook_secret.is_empty() {
        let handler = WebhookHandler::new(&wh_state.webhook_secret);
        match handler.verify_signature(&payload) {
            Ok(true) => {}
            Ok(false) => {
                warn!(delivery = %delivery_id, "Webhook signature verification failed — ignoring");
                return (
                    StatusCode::UNAUTHORIZED,
                    Json(serde_json::json!({ "error": "Invalid webhook signature" })),
                )
                    .into_response();
            }
            Err(e) => {
                warn!(delivery = %delivery_id, error = %e, "Webhook signature error");
                return (
                    StatusCode::BAD_REQUEST,
                    Json(serde_json::json!({ "error": format!("Signature error: {}", e) })),
                )
                    .into_response();
            }
        }
    }

    // Parse the event — unsupported types are silently acked.
    let event = match payload.parse_event() {
        Ok(e) => e,
        Err(e) => {
            info!(
                delivery = %delivery_id,
                event_type = %event_type,
                "Unrecognised webhook event type — acking without action: {}",
                e
            );
            return (
                StatusCode::OK,
                Json(serde_json::json!({ "status": "ignored" })),
            )
                .into_response();
        }
    };

    // Only push events trigger a repo sync.
    if let WebhookEvent::Push(ref push) = event {
        let repo_full_name = push.repository.full_name.clone();
        let branch = push.branch_name().unwrap_or("unknown").to_string();

        info!(
            delivery = %delivery_id,
            repo = %repo_full_name,
            branch = %branch,
            "Push webhook received — checking for matching registered repo"
        );

        // Clone the sync_service Arc so we can move it into the background task.
        let sync_service = Arc::clone(&wh_state.sync_service);

        tokio::spawn(async move {
            let svc = sync_service.read().await;

            // Find any registered repo whose remote_url ends with the GitHub
            // full name (handles both HTTPS and SSH remote URL formats).
            let matching_ids: Vec<String> = svc
                .list_repos()
                .iter()
                .filter(|r| {
                    r.remote_url
                        .as_deref()
                        .map(|u| u.contains(&repo_full_name))
                        .unwrap_or(false)
                })
                .map(|r| r.id.clone())
                .collect();

            drop(svc); // release read lock before acquiring write lock

            if matching_ids.is_empty() {
                info!(
                    repo = %repo_full_name,
                    "No registered repo matches push event — skipping sync"
                );
                return;
            }

            let mut svc = sync_service.write().await;
            for repo_id in matching_ids {
                info!(repo_id = %repo_id, "Triggering sync from push webhook");
                match svc.sync(&repo_id).await {
                    Ok(result) => info!(
                        repo_id = %repo_id,
                        files = result.files_walked,
                        todos = result.todos_found,
                        duration_ms = result.duration_ms,
                        "Webhook-triggered sync complete"
                    ),
                    Err(e) => warn!(
                        repo_id = %repo_id,
                        error = %e,
                        "Webhook-triggered sync failed"
                    ),
                }
            }
        });
    }

    (
        StatusCode::OK,
        Json(serde_json::json!({ "status": "accepted" })),
    )
        .into_response()
}

// Health check endpoint
async fn health_check() -> impl IntoResponse {
    Json(HealthResponse {
        status: "healthy".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
    })
}

// Clone a repository endpoint
async fn clone_repository(
    State(state): State<AppState>,
    Json(request): Json<CloneRequest>,
) -> Result<Json<CloneResponse>> {
    info!("Cloning repository: {}", request.url);

    // SECURITY: Validate Git URL against whitelist to prevent SSRF attacks
    // This prevents attackers from using the clone endpoint to:
    // 1. Access internal services (e.g., http://localhost, http://169.254.169.254)
    // 2. Clone from untrusted/malicious repositories
    // 3. Exfiltrate data to attacker-controlled servers
    state.config.security.validate_git_url(&request.url)?;

    let repo_path = state.git_manager.clone_repo(&request.url, None)?;

    if let Some(branch) = &request.branch {
        state.git_manager.checkout(&repo_path, branch)?;
    }

    let stats = state.git_manager.stats(&repo_path)?;

    Ok(Json(CloneResponse {
        path: repo_path.to_string_lossy().to_string(),
        branch: state
            .git_manager
            .current_branch(&repo_path)
            .unwrap_or_default(),
        commit_count: stats.commit_count,
    }))
}

// Scan for tags only
async fn scan_tags(
    State(_state): State<AppState>,
    Json(request): Json<ScanRequest>,
) -> Result<Json<TagsResponse>> {
    info!("Scanning for tags in: {}", request.path);

    let tag_scanner = TagScanner::new()?;
    let tags = tag_scanner.scan_directory(&std::path::PathBuf::from(&request.path))?;

    let grouped = tag_scanner.group_by_type(&tags);

    let by_type: HashMap<String, usize> = grouped
        .into_iter()
        .map(|(k, v)| (format!("{:?}", k), v.len()))
        .collect();

    Ok(Json(TagsResponse {
        total: tags.len(),
        by_type,
        tags,
    }))
}

// Perform static analysis only
async fn scan_static(
    State(state): State<AppState>,
    Json(request): Json<ScanRequest>,
) -> Result<Json<StaticAnalysisResponse>> {
    info!("Running static analysis on: {}", request.path);

    let scanner = Scanner::new(
        std::path::PathBuf::from(&request.path),
        state.config.scanner.max_file_size,
        false,
    )?;

    let audit_request = AuditRequest {
        repository: request.path.clone(),
        branch: None,
        enable_llm: false,
        focus: vec![],
        include_tests: false,
    };

    let report = scanner.scan(&audit_request)?;

    Ok(Json(StaticAnalysisResponse {
        total_files: report.summary.total_files,
        total_issues: report.summary.total_issues,
        critical_files: report.summary.critical_files,
        issues_by_severity: report.issues_by_severity,
    }))
}

// ===== Response Types =====

#[derive(Debug, Serialize)]
struct HealthResponse {
    status: String,
    version: String,
}

#[derive(Debug, Deserialize)]
struct CloneRequest {
    url: String,
    branch: Option<String>,
}

#[derive(Debug, Serialize)]
struct CloneResponse {
    path: String,
    branch: String,
    commit_count: usize,
}

#[derive(Debug, Deserialize)]
struct ScanRequest {
    path: String,
}

#[derive(Debug, Serialize)]
struct TagsResponse {
    total: usize,
    by_type: HashMap<String, usize>,
    tags: Vec<AuditTag>,
}

#[derive(Debug, Serialize)]
struct StaticAnalysisResponse {
    total_files: usize,
    total_issues: usize,
    critical_files: usize,
    issues_by_severity: HashMap<crate::types::IssueSeverity, usize>,
}

// ===== Visualization Endpoints =====

// Neuromorphic visualization endpoints removed - feature specific to another project

// ===== Error Response =====

impl IntoResponse for AuditError {
    fn into_response(self) -> Response {
        let (status, message) = match self {
            AuditError::FileNotFound(_) => (StatusCode::NOT_FOUND, self.to_string()),
            AuditError::Config(_) => (StatusCode::BAD_REQUEST, self.to_string()),
            AuditError::InvalidApiKey { .. } => (StatusCode::UNAUTHORIZED, self.to_string()),
            AuditError::RateLimitExceeded => (StatusCode::TOO_MANY_REQUESTS, self.to_string()),
            _ => (StatusCode::INTERNAL_SERVER_ERROR, self.to_string()),
        };

        let body = Json(ErrorResponse {
            error: message,
            status: status.as_u16(),
        });

        (status, body).into_response()
    }
}

#[derive(Debug, Serialize)]
struct ErrorResponse {
    error: String,
    status: u16,
}

// ============================================================================
// Repository Management Endpoints
// ============================================================================

// List all tracked repositories
async fn list_repos(State(state): State<AppState>) -> Result<Json<Vec<Repository>>> {
    let repos = db::list_repositories(&state.db_pool)
        .await
        .map_err(|e| AuditError::other(format!("Failed to list repositories: {}", e)))?;

    Ok(Json(repos))
}

#[derive(Debug, Deserialize)]
struct ScanReposRequest {
    token: Option<String>,
}

#[derive(Debug, Serialize)]
struct ScanReposResponse {
    synced_count: usize,
    repositories: Vec<Repository>,
}

// Scan and sync repositories from GitHub
async fn scan_repos(
    State(state): State<AppState>,
    Json(req): Json<ScanReposRequest>,
) -> Result<Json<ScanReposResponse>> {
    // Sync repositories from GitHub
    let repo_ids = sync_repos_to_db(&state.db_pool, req.token.as_deref())
        .await
        .map_err(|e| AuditError::other(format!("Failed to sync repositories: {}", e)))?;

    // Fetch the synced repositories
    let repositories = db::list_repositories(&state.db_pool)
        .await
        .map_err(|e| AuditError::other(format!("Failed to list repositories: {}", e)))?;

    Ok(Json(ScanReposResponse {
        synced_count: repo_ids.len(),
        repositories,
    }))
}

// ============================================================================
// Queue Management Endpoints
// ============================================================================

// Get queue status
async fn queue_status(State(state): State<AppState>) -> Result<Json<QueueStats>> {
    let stats = get_queue_stats(&state.db_pool)
        .await
        .map_err(|e| AuditError::other(format!("Failed to get queue stats: {}", e)))?;

    Ok(Json(stats))
}

// ============================================================================
// GitHub Integration Endpoints
// ============================================================================

#[derive(Debug, Serialize)]
struct GitHubStatsResponse {
    repositories: i64,
    issues: i64,
    pull_requests: i64,
    commits: i64,
    events: i64,
    last_sync: Option<String>,
    top_repos: Vec<TopRepo>,
}

#[derive(Debug, Serialize)]
struct TopRepo {
    name: String,
    stars: i64,
}

#[derive(Debug, Deserialize)]
struct GitHubSearchQuery {
    q: String,
    #[serde(default = "default_limit")]
    limit: i32,
}

fn default_limit() -> i32 {
    20
}

#[derive(Debug, Deserialize)]
struct GitHubIssuesQuery {
    repo: Option<String>,
    state: Option<String>,
    #[serde(default = "default_limit")]
    limit: i32,
}

#[derive(Debug, Deserialize)]
struct GitHubPrsQuery {
    repo: Option<String>,
    state: Option<String>,
    #[serde(default = "default_limit")]
    limit: i32,
}

#[derive(Debug, Deserialize)]
struct GitHubReposQuery {
    language: Option<String>,
    #[serde(default = "default_limit")]
    limit: i32,
}

#[derive(Debug, Deserialize)]
struct GitHubSyncRequest {
    full: Option<bool>,
    repo: Option<String>,
}

// Get GitHub integration statistics
async fn github_stats(State(state): State<AppState>) -> Result<Json<GitHubStatsResponse>> {
    let stats: (i64, i64, i64, i64, i64) = sqlx::query_as(
        r#"
        SELECT
            (SELECT COUNT(*) FROM github_repositories) as repos,
            (SELECT COUNT(*) FROM github_issues) as issues,
            (SELECT COUNT(*) FROM github_pull_requests) as prs,
            (SELECT COUNT(*) FROM github_commits) as commits,
            (SELECT COUNT(*) FROM github_events) as events
        "#,
    )
    .fetch_one(&state.db_pool)
    .await
    .map_err(|e| AuditError::other(format!("Failed to get GitHub stats: {}", e)))?;

    let last_sync: Option<String> = sqlx::query_scalar(
        "SELECT MAX(last_synced_at) FROM github_repositories WHERE last_synced_at IS NOT NULL",
    )
    .fetch_optional(&state.db_pool)
    .await
    .map_err(|e| AuditError::other(format!("Failed to get last sync time: {}", e)))?;

    let top_repos: Vec<(String, i64)> = sqlx::query_as(
        "SELECT full_name, stargazers_count FROM github_repositories
         ORDER BY stargazers_count DESC LIMIT 5",
    )
    .fetch_all(&state.db_pool)
    .await
    .map_err(|e| AuditError::other(format!("Failed to get top repos: {}", e)))?;

    Ok(Json(GitHubStatsResponse {
        repositories: stats.0,
        issues: stats.1,
        pull_requests: stats.2,
        commits: stats.3,
        events: stats.4,
        last_sync,
        top_repos: top_repos
            .into_iter()
            .map(|(name, stars)| TopRepo { name, stars })
            .collect(),
    }))
}

// Search GitHub repositories
async fn github_repos(
    State(state): State<AppState>,
    Query(params): Query<GitHubReposQuery>,
) -> Result<Json<Vec<crate::github::search::SearchResult>>> {
    use crate::github::search::{GitHubSearcher, SearchQuery, SearchType};

    let searcher = GitHubSearcher::new(state.db_pool.clone());
    let mut query = SearchQuery::new("")
        .with_type(SearchType::Repositories)
        .limit(params.limit);

    if let Some(lang) = params.language {
        query = query.with_language(lang);
    }

    let results = searcher
        .search(query)
        .await
        .map_err(|e| AuditError::other(format!("Failed to search repositories: {}", e)))?;

    Ok(Json(results))
}

// Get GitHub issues
async fn github_issues(
    State(state): State<AppState>,
    Query(params): Query<GitHubIssuesQuery>,
) -> Result<Json<Vec<crate::github::search::SearchResult>>> {
    use crate::github::search::{GitHubSearcher, SearchQuery, SearchType};

    let searcher = GitHubSearcher::new(state.db_pool.clone());
    let mut query = SearchQuery::new("")
        .with_type(SearchType::Issues)
        .limit(params.limit);

    let state_param = params.state.as_deref().unwrap_or("open");
    if state_param == "open" {
        query = query.only_open();
    } else if state_param == "closed" {
        query = query.only_closed();
    }

    if let Some(repo) = params.repo {
        query = query.in_repo(repo);
    }

    let results = searcher
        .search(query)
        .await
        .map_err(|e| AuditError::other(format!("Failed to search issues: {}", e)))?;

    Ok(Json(results))
}

// Get GitHub pull requests
async fn github_prs(
    State(state): State<AppState>,
    Query(params): Query<GitHubPrsQuery>,
) -> Result<Json<Vec<crate::github::search::SearchResult>>> {
    use crate::github::search::{GitHubSearcher, SearchQuery, SearchType};

    let searcher = GitHubSearcher::new(state.db_pool.clone());
    let mut query = SearchQuery::new("")
        .with_type(SearchType::PullRequests)
        .limit(params.limit);

    let state_param = params.state.as_deref().unwrap_or("open");
    if state_param == "open" {
        query = query.only_open();
    } else if state_param == "closed" {
        query = query.only_closed();
    }

    if let Some(repo) = params.repo {
        query = query.in_repo(repo);
    }

    let results = searcher
        .search(query)
        .await
        .map_err(|e| AuditError::other(format!("Failed to search pull requests: {}", e)))?;

    Ok(Json(results))
}

// Search GitHub data
async fn github_search(
    State(state): State<AppState>,
    Query(params): Query<GitHubSearchQuery>,
) -> Result<Json<serde_json::Value>> {
    use crate::github::search::{GitHubSearcher, SearchQuery, SearchType};

    let searcher = GitHubSearcher::new(state.db_pool.clone());

    // Search all types and return combined results
    let repos_query = SearchQuery::new(&params.q)
        .with_type(SearchType::Repositories)
        .limit(params.limit.min(10));
    let repos = searcher
        .search(repos_query)
        .await
        .map_err(|e| AuditError::other(format!("Failed to search repositories: {}", e)))?;

    let issues_query = SearchQuery::new(&params.q)
        .with_type(SearchType::Issues)
        .limit(params.limit.min(10));
    let issues = searcher
        .search(issues_query)
        .await
        .map_err(|e| AuditError::other(format!("Failed to search issues: {}", e)))?;

    let prs_query = SearchQuery::new(&params.q)
        .with_type(SearchType::PullRequests)
        .limit(params.limit.min(10));
    let prs = searcher
        .search(prs_query)
        .await
        .map_err(|e| AuditError::other(format!("Failed to search pull requests: {}", e)))?;

    Ok(Json(serde_json::json!({
        "repositories": repos,
        "issues": issues,
        "pull_requests": prs,
    })))
}

// Trigger GitHub sync
async fn github_sync(
    State(state): State<AppState>,
    Json(params): Json<GitHubSyncRequest>,
) -> Result<Json<serde_json::Value>> {
    use crate::github::{GitHubClient, SyncEngine, SyncOptions};
    use std::env;

    let token = env::var("GITHUB_TOKEN")
        .map_err(|_| AuditError::other("GITHUB_TOKEN environment variable not set"))?;

    let client = GitHubClient::new(token)
        .map_err(|e| AuditError::other(format!("Failed to create GitHub client: {}", e)))?;

    let sync_engine = SyncEngine::new(client.clone(), state.db_pool.clone());

    let options = if params.full.unwrap_or(false) {
        SyncOptions::default().force_full()
    } else {
        SyncOptions::default()
    };

    let options = if let Some(repo) = params.repo {
        options.with_repos(vec![repo])
    } else {
        options
    };

    let result = sync_engine
        .sync_with_options(options)
        .await
        .map_err(|e| AuditError::other(format!("Failed to sync: {}", e)))?;

    Ok(Json(serde_json::json!({
        "status": "success",
        "repositories": result.repos_synced,
        "issues": result.issues_synced,
        "pull_requests": result.prs_synced,
        "duration_secs": result.duration_secs
    })))
}

// ============================================================================
// Task & Stats Endpoints (consolidated from bin/server.rs)
// ============================================================================

#[derive(Debug, Deserialize)]
struct ListTasksQuery {
    limit: Option<i64>,
    status: Option<String>,
    priority: Option<i32>,
    repo_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct UpdateStatusRequest {
    status: String,
}

// `GET /api/tasks` — list tasks with optional filters
async fn list_tasks_handler(
    State(state): State<AppState>,
    Query(query): Query<ListTasksQuery>,
) -> Result<Json<Vec<db::Task>>> {
    let limit = query.limit.unwrap_or(50);
    let tasks = db::list_tasks(
        &state.db_pool,
        limit,
        query.status.as_deref(),
        query.priority,
        query.repo_id.as_deref(),
    )
    .await
    .map_err(|e| AuditError::other(format!("Failed to list tasks: {}", e)))?;
    Ok(Json(tasks))
}

// `GET /api/tasks/next` — get the highest-priority pending task
async fn get_next_task_handler(State(state): State<AppState>) -> Result<Json<serde_json::Value>> {
    match db::get_next_task(&state.db_pool).await {
        Ok(Some(task)) => Ok(Json(serde_json::json!(task))),
        Ok(None) => Ok(Json(serde_json::json!({"message": "No pending tasks"}))),
        Err(e) => Err(AuditError::other(format!("Failed to get next task: {}", e))),
    }
}

// `PUT /api/tasks/{id}` — update a task's status
async fn update_task_handler(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<UpdateStatusRequest>,
) -> Result<Json<serde_json::Value>> {
    db::update_task_status(&state.db_pool, &id, &req.status)
        .await
        .map_err(|e| AuditError::other(format!("Failed to update task: {}", e)))?;
    Ok(Json(serde_json::json!({"updated": true})))
}

// `GET /api/stats` — get database statistics
async fn get_statistics(State(state): State<AppState>) -> Result<Json<db::DbStats>> {
    let stats = db::get_stats(&state.db_pool)
        .await
        .map_err(|e| AuditError::other(format!("Failed to get stats: {}", e)))?;
    Ok(Json(stats))
}
