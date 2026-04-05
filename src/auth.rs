// API key authentication middleware for the RustCode server.
//
// Reads valid keys from the `RUSTCODE_PROXY_API_KEYS` environment variable
// (comma-separated list). When the variable is unset or empty, authentication
// is disabled (dev / testing mode).
//
// Keys are cached in a [`std::sync::OnceLock`] so the environment variable is
// read at most once per process lifetime.
//
// # Usage
//
// ```rust,ignore
// use axum::{Router, middleware, routing::get};
// use rustcode::auth::require_api_key;
//
// let protected = Router::new()
//     .route("/api/things", get(handler))
//     .layer(middleware::from_fn(require_api_key));
// ```

use axum::{
    extract::Request,
    http::StatusCode,
    middleware::Next,
    response::{IntoResponse, Response},
};
use serde::Serialize;
use std::sync::OnceLock;

// ---------------------------------------------------------------------------
// Cached key store
// ---------------------------------------------------------------------------

// Parsed set of valid API keys, loaded once from the environment.
//
// An empty `Vec` means authentication is disabled (dev mode).
fn valid_keys() -> &'static Vec<String> {
    static KEYS: OnceLock<Vec<String>> = OnceLock::new();
    KEYS.get_or_init(|| {
        let raw = std::env::var("RUSTCODE_PROXY_API_KEYS").unwrap_or_default();
        if raw.trim().is_empty() {
            Vec::new()
        } else {
            raw.split(',')
                .map(|k| k.trim().to_owned())
                .filter(|k| !k.is_empty())
                .collect()
        }
    })
}

// Returns `true` when no keys are configured (auth disabled / dev mode).
pub fn auth_disabled() -> bool {
    valid_keys().is_empty()
}

// ---------------------------------------------------------------------------
// JSON error body
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct AuthError {
    error: String,
    status: u16,
}

impl AuthError {
    fn new(status: StatusCode, message: impl Into<String>) -> (StatusCode, axum::Json<Self>) {
        let code = status.as_u16();
        (
            status,
            axum::Json(Self {
                error: message.into(),
                status: code,
            }),
        )
    }

    fn unauthorized(message: impl Into<String>) -> (StatusCode, axum::Json<Self>) {
        Self::new(StatusCode::UNAUTHORIZED, message)
    }
}

// ---------------------------------------------------------------------------
// Middleware
// ---------------------------------------------------------------------------

// Axum middleware that validates the `Authorization: Bearer <key>` header
// against the keys stored in `RUSTCODE_PROXY_API_KEYS`.
//
// When no keys are configured the middleware is a no-op (dev mode).
pub async fn require_api_key(request: Request, next: Next) -> Result<Response, Response> {
    let keys = valid_keys();

    // No keys configured → auth disabled (dev / testing mode).
    if keys.is_empty() {
        return Ok(next.run(request).await);
    }

    let auth_header = request
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok());

    match auth_header {
        Some(header) if header.starts_with("Bearer ") => {
            let token = &header[7..];
            if keys.iter().any(|k| k == token) {
                Ok(next.run(request).await)
            } else {
                Err(AuthError::unauthorized("Invalid API key").into_response())
            }
        }
        Some(_) => Err(AuthError::unauthorized(
            "Malformed Authorization header. Expected: Bearer <key>",
        )
        .into_response()),
        None => Err(AuthError::unauthorized(
            "Missing Authorization header. Expected: Bearer <key>",
        )
        .into_response()),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Router, body::Body, middleware, routing::get};
    use http::Request as HttpRequest;
    use tower::ServiceExt; // for `oneshot`

    async fn ok_handler() -> &'static str {
        "ok"
    }

    fn test_app() -> Router {
        Router::new()
            .route("/protected", get(ok_handler))
            .layer(middleware::from_fn(require_api_key))
    }

    #[test]
    fn test_valid_keys_caching() {
        // OnceLock means subsequent calls return the same reference.
        let a = valid_keys() as *const _;
        let b = valid_keys() as *const _;
        assert_eq!(a, b, "OnceLock should return the same allocation");
    }

    #[tokio::test]
    async fn test_auth_disabled_allows_all() {
        // When RUSTCODE_PROXY_API_KEYS is unset / empty the middleware is a pass-through.
        // Because OnceLock is process-global we can only really assert the helper:
        // in CI the env var is typically unset, so auth_disabled() == true.
        if !auth_disabled() {
            // Skip when running in an environment that sets the var.
            return;
        }

        let app = test_app();
        let req = HttpRequest::builder()
            .uri("/protected")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }
}
