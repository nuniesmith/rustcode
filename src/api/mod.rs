// API module.
//
// The live server (src/server.rs) mounts `repo_router` (/api/v1) and
// `proxy_router` (/v1) from this module tree. The legacy document-RAG router
// (create_api_router / create_default_api_router / ApiServer + its HTTP handlers
// and the admin router) was never mounted and has been removed; `ApiState`
// remains in `handlers` because the `todo` subsystem constructs it.

pub mod agent;
pub mod auth;
pub mod handlers;
pub mod jobs;
pub mod memory;
pub mod proxy;
pub mod proxy_client;
pub mod rate_limit;
pub mod repos;
pub mod types;

pub use auth::{AuthConfig, AuthResult, generate_api_key, hash_api_key};
pub use handlers::ApiState;
pub use jobs::{JobQueue, JobQueueConfig, JobStatus};
pub use proxy::{ProxyState, proxy_router};
pub use proxy_client::{
    ChatMessage, ChatReply, ChatRequestBuilder, ProxyClient, ProxyClientConfig,
};
pub use rate_limit::{RateLimitConfig, RateLimiter};
pub use types::*;
