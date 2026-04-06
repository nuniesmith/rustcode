// Core runtime primitives for the `claw` CLI and supporting crates.
//
// This crate owns session persistence, permission evaluation, prompt assembly,
// MCP plumbing, tool-facing file operations, and the core conversation loop
// that drives interactive and one-shot turns.

mod bash;
pub mod bash_validation;
mod bootstrap;
mod compact;
mod config;
mod conversation;
mod file_ops;
pub mod green_contract;
mod hooks;
mod json;
mod lane_events;
pub mod lsp_client;
mod mcp;
mod mcp_client;
pub mod mcp_lifecycle_hardened;
mod mcp_stdio;
pub mod mcp_tool_bridge;
mod oauth;
pub mod permission_enforcer;
mod permissions;
pub mod plugin_lifecycle;
mod policy_engine;
mod prompt;
pub mod recovery_recipes;
mod remote;
pub mod sandbox;
mod session;
pub mod session_control;
mod sse;
pub mod stale_branch;
pub mod summary_compression;
pub mod task_packet;
pub mod task_registry;
pub mod team_cron_registry;
pub mod trust_resolver;
mod usage;
pub mod worker_boot;

pub use bash::{BashCommandInput, BashCommandOutput, execute_bash};
pub use bootstrap::{BootstrapPhase, BootstrapPlan};
pub use compact::{
    CompactionConfig, CompactionResult, compact_session, estimate_session_tokens,
    format_compact_summary, get_compact_continuation_message, should_compact,
};
pub use config::{
    CLAW_SETTINGS_SCHEMA_NAME, ConfigEntry, ConfigError, ConfigLoader, ConfigSource,
    McpConfigCollection, McpManagedProxyServerConfig, McpOAuthConfig, McpRemoteServerConfig,
    McpSdkServerConfig, McpServerConfig, McpStdioServerConfig, McpTransport,
    McpWebSocketServerConfig, OAuthConfig, ResolvedPermissionMode, RuntimeConfig,
    RuntimeFeatureConfig, RuntimeHookConfig, RuntimePermissionRuleConfig, RuntimePluginConfig,
    ScopedMcpServerConfig,
};
pub use conversation::{
    ApiClient, ApiRequest, AssistantEvent, AutoCompactionEvent, ConversationRuntime,
    PromptCacheEvent, RuntimeError, StaticToolExecutor, ToolError, ToolExecutor, TurnSummary,
    auto_compaction_threshold_from_env,
};
pub use file_ops::{
    EditFileOutput, GlobSearchOutput, GrepSearchInput, GrepSearchOutput, ReadFileOutput,
    StructuredPatchHunk, TextFilePayload, WriteFileOutput, edit_file, glob_search, grep_search,
    read_file, write_file,
};
pub use hooks::{
    HookAbortSignal, HookEvent, HookProgressEvent, HookProgressReporter, HookRunResult, HookRunner,
};
pub use lane_events::{
    LaneEvent, LaneEventBlocker, LaneEventName, LaneEventStatus, LaneFailureClass,
};
pub use mcp::{
    mcp_server_signature, mcp_tool_name, mcp_tool_prefix, normalize_name_for_mcp,
    scoped_mcp_config_hash, unwrap_ccr_proxy_url,
};
pub use mcp_client::{
    McpClientAuth, McpClientBootstrap, McpClientTransport, McpManagedProxyTransport,
    McpRemoteTransport, McpSdkTransport, McpStdioTransport,
};
pub use mcp_lifecycle_hardened::{
    McpDegradedReport, McpErrorSurface, McpFailedServer, McpLifecyclePhase, McpLifecycleState,
    McpLifecycleValidator, McpPhaseResult,
};
pub use mcp_stdio::{
    JsonRpcError, JsonRpcId, JsonRpcRequest, JsonRpcResponse, ManagedMcpTool, McpDiscoveryFailure,
    McpInitializeClientInfo, McpInitializeParams, McpInitializeResult, McpInitializeServerInfo,
    McpListResourcesParams, McpListResourcesResult, McpListToolsParams, McpListToolsResult,
    McpReadResourceParams, McpReadResourceResult, McpResource, McpResourceContents,
    McpServerManager, McpServerManagerError, McpStdioProcess, McpTool, McpToolCallContent,
    McpToolCallParams, McpToolCallResult, McpToolDiscoveryReport, UnsupportedMcpServer,
    spawn_mcp_stdio_process,
};
pub use oauth::{
    OAuthAuthorizationRequest, OAuthCallbackParams, OAuthRefreshRequest, OAuthTokenExchangeRequest,
    OAuthTokenSet, PkceChallengeMethod, PkceCodePair, clear_oauth_credentials, code_challenge_s256,
    credentials_path, generate_pkce_pair, generate_state, load_oauth_credentials,
    loopback_redirect_uri, parse_oauth_callback_query, parse_oauth_callback_request_target,
    save_oauth_credentials,
};
pub use permissions::{
    PermissionContext, PermissionMode, PermissionOutcome, PermissionOverride, PermissionPolicy,
    PermissionPromptDecision, PermissionPrompter, PermissionRequest,
};
pub use plugin_lifecycle::{
    DegradedMode, DiscoveryResult, PluginHealthcheck, PluginLifecycle, PluginLifecycleEvent,
    PluginState, ResourceInfo, ServerHealth, ServerStatus, ToolInfo,
};
pub use policy_engine::{
    DiffScope, GreenLevel, LaneBlocker, LaneContext, PolicyAction, PolicyCondition, PolicyEngine,
    PolicyRule, ReconcileReason, ReviewStatus, evaluate,
};
pub use prompt::{
    ContextFile, FRONTIER_MODEL_NAME, ProjectContext, PromptBuildError,
    SYSTEM_PROMPT_DYNAMIC_BOUNDARY, SystemPromptBuilder, load_system_prompt, prepend_bullets,
};
pub use recovery_recipes::{
    EscalationPolicy, FailureScenario, RecoveryContext, RecoveryEvent, RecoveryRecipe,
    RecoveryResult, RecoveryStep, attempt_recovery, recipe_for,
};
pub use remote::{
    DEFAULT_REMOTE_BASE_URL, DEFAULT_SESSION_TOKEN_PATH, DEFAULT_SYSTEM_CA_BUNDLE, NO_PROXY_HOSTS,
    RemoteSessionContext, UPSTREAM_PROXY_ENV_KEYS, UpstreamProxyBootstrap, UpstreamProxyState,
    inherited_upstream_proxy_env, no_proxy_list, read_token, upstream_proxy_ws_url,
};
pub use sandbox::{
    ContainerEnvironment, FilesystemIsolationMode, LinuxSandboxCommand, SandboxConfig,
    SandboxDetectionInputs, SandboxRequest, SandboxStatus, build_linux_sandbox_command,
    detect_container_environment, detect_container_environment_from, resolve_sandbox_status,
    resolve_sandbox_status_for_request,
};
pub use session::{
    ContentBlock, ConversationMessage, MessageRole, Session, SessionCompaction, SessionError,
    SessionFork,
};
pub use sse::{IncrementalSseParser, SseEvent};
pub use stale_branch::{
    BranchFreshness, StaleBranchAction, StaleBranchEvent, StaleBranchPolicy, apply_policy,
    check_freshness,
};
pub use task_packet::{TaskPacket, TaskPacketValidationError, ValidatedPacket, validate_packet};
pub use trust_resolver::{TrustConfig, TrustDecision, TrustEvent, TrustPolicy, TrustResolver};
pub use usage::{
    ModelPricing, TokenUsage, UsageCostEstimate, UsageTracker, format_usd, pricing_for_model,
};
pub use worker_boot::{
    Worker, WorkerEvent, WorkerEventKind, WorkerEventPayload, WorkerFailure, WorkerFailureKind,
    WorkerPromptTarget, WorkerReadySnapshot, WorkerRegistry, WorkerStatus, WorkerTrustResolution,
};

pub fn test_env_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| std::sync::Mutex::new(()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Safely manipulate environment variables in a test under a lock.
///
/// Acquires the environment lock and calls the provided closure with mutable
/// access to set/remove environment variables. This is safe despite using
/// unsafe internally because the lock serializes all access.
pub fn with_env<F, R>(f: F) -> R
where
    F: FnOnce() -> R,
{
    let _guard = test_env_lock();
    // The closure itself is safe; the lock serializes environment access.
    f()
}

/// Set an environment variable in a test under a lock (safe wrapper).
///
/// This function is marked with `#[allow(unsafe_code)]` because it safely wraps
/// an inherently unsafe operation (`std::env::set_var`) by holding the environment
/// lock for its entire duration, ensuring thread-safe access.
#[allow(unsafe_code)]
pub fn test_set_var(key: &str, value: &str) {
    let _guard = test_env_lock();
    // SAFETY: We hold the lock for the entire duration, preventing races.
    unsafe {
        std::env::set_var(key, value);
    }
}

/// Remove an environment variable in a test under a lock (safe wrapper).
///
/// This function is marked with `#[allow(unsafe_code)]` because it safely wraps
/// an inherently unsafe operation (`std::env::remove_var`) by holding the environment
/// lock for its entire duration, ensuring thread-safe access.
#[allow(unsafe_code)]
pub fn test_remove_var(key: &str) {
    let _guard = test_env_lock();
    // SAFETY: We hold the lock for the entire duration, preventing races.
    unsafe {
        std::env::remove_var(key);
    }
}
