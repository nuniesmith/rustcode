mod client;
mod error;
mod prompt_cache;
mod providers;
mod sse;
mod types;

/// Install ring as the process-default rustls CryptoProvider, once. reqwest
/// uses `rustls-no-provider` (ring, matching the workspace), so building a
/// client with no default provider panics in `default_rustls_crypto_provider`
/// — notably under `cargo test`, which never runs a binary `main()`. Call this
/// before constructing any reqwest client.
pub(crate) fn ensure_crypto_provider() {
    use std::sync::Once;
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

pub use client::{
    MessageStream, OAuthTokenSet, ProviderClient, oauth_token_is_expired, read_base_url,
    read_xai_base_url, resolve_saved_oauth_token, resolve_startup_auth_source,
};
pub use error::ApiError;
pub use prompt_cache::{
    CacheBreakEvent, PromptCache, PromptCacheConfig, PromptCachePaths, PromptCacheRecord,
    PromptCacheStats,
};
pub use providers::anthropic::{AnthropicClient, AnthropicClient as ApiClient, AuthSource};
pub use providers::openai_compat::{OpenAiCompatClient, OpenAiCompatConfig};
pub use providers::{
    ProviderKind, detect_provider_kind, max_tokens_for_model, resolve_model_alias,
};
pub use sse::{SseParser, parse_frame};
pub use types::{
    CacheControl, ContentBlockDelta, ContentBlockDeltaEvent, ContentBlockStartEvent,
    ContentBlockStopEvent, InputContentBlock, InputMessage, MessageDelta, MessageDeltaEvent,
    MessageStartEvent, MessageStopEvent, OutputContentBlock, ResponseFormat, SystemBlock,
    ToolChoice, ToolDefinition, ToolResultContentBlock,
};

pub use crate::types::{MessageRequest, MessageResponse, StreamEvent, Usage};

pub use telemetry::{
    AnalyticsEvent, AnthropicRequestProfile, ClientIdentity, DEFAULT_ANTHROPIC_VERSION,
    JsonlTelemetrySink, MemoryTelemetrySink, SessionTraceRecord, SessionTracer, TelemetryEvent,
    TelemetrySink,
};
