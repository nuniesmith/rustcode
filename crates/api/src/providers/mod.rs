#![allow(dead_code)]

use std::future::Future;
use std::pin::Pin;

use crate::error::ApiError;
use crate::types::{MessageRequest, MessageResponse};

pub mod anthropic;
pub mod openai_compat;

pub type ProviderFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, ApiError>> + Send + 'a>>;

pub trait Provider {
    type Stream;

    fn send_message<'a>(
        &'a self,
        request: &'a MessageRequest,
    ) -> ProviderFuture<'a, MessageResponse>;

    fn stream_message<'a>(
        &'a self,
        request: &'a MessageRequest,
    ) -> ProviderFuture<'a, Self::Stream>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderKind {
    Anthropic,
    Xai,
    OpenAi,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProviderMetadata {
    pub provider: ProviderKind,
    pub auth_env: &'static str,
    pub base_url_env: &'static str,
    pub default_base_url: &'static str,
}

const MODEL_REGISTRY: &[(&str, ProviderMetadata)] = &[
    (
        "opus",
        ProviderMetadata {
            provider: ProviderKind::Anthropic,
            auth_env: "ANTHROPIC_API_KEY",
            base_url_env: "ANTHROPIC_BASE_URL",
            default_base_url: anthropic::DEFAULT_BASE_URL,
        },
    ),
    (
        "sonnet",
        ProviderMetadata {
            provider: ProviderKind::Anthropic,
            auth_env: "ANTHROPIC_API_KEY",
            base_url_env: "ANTHROPIC_BASE_URL",
            default_base_url: anthropic::DEFAULT_BASE_URL,
        },
    ),
    (
        "haiku",
        ProviderMetadata {
            provider: ProviderKind::Anthropic,
            auth_env: "ANTHROPIC_API_KEY",
            base_url_env: "ANTHROPIC_BASE_URL",
            default_base_url: anthropic::DEFAULT_BASE_URL,
        },
    ),
    (
        "grok",
        ProviderMetadata {
            provider: ProviderKind::Xai,
            auth_env: "XAI_API_KEY",
            base_url_env: "XAI_BASE_URL",
            default_base_url: openai_compat::DEFAULT_XAI_BASE_URL,
        },
    ),
    (
        "grok-3",
        ProviderMetadata {
            provider: ProviderKind::Xai,
            auth_env: "XAI_API_KEY",
            base_url_env: "XAI_BASE_URL",
            default_base_url: openai_compat::DEFAULT_XAI_BASE_URL,
        },
    ),
    (
        "grok-mini",
        ProviderMetadata {
            provider: ProviderKind::Xai,
            auth_env: "XAI_API_KEY",
            base_url_env: "XAI_BASE_URL",
            default_base_url: openai_compat::DEFAULT_XAI_BASE_URL,
        },
    ),
    (
        "grok-3-mini",
        ProviderMetadata {
            provider: ProviderKind::Xai,
            auth_env: "XAI_API_KEY",
            base_url_env: "XAI_BASE_URL",
            default_base_url: openai_compat::DEFAULT_XAI_BASE_URL,
        },
    ),
    (
        "grok-2",
        ProviderMetadata {
            provider: ProviderKind::Xai,
            auth_env: "XAI_API_KEY",
            base_url_env: "XAI_BASE_URL",
            default_base_url: openai_compat::DEFAULT_XAI_BASE_URL,
        },
    ),
];

#[must_use]
pub fn resolve_model_alias(model: &str) -> String {
    let trimmed = model.trim();
    let lower = trimmed.to_ascii_lowercase();
    MODEL_REGISTRY
        .iter()
        .find_map(|(alias, metadata)| {
            (*alias == lower).then_some(match metadata.provider {
                // Prefer the undated aliases: they are stable across point
                // releases, whereas a date-suffixed ID rots. The previous
                // "claude-haiku-4-5-20251213" did not correspond to a real
                // snapshot and would have 404'd.
                ProviderKind::Anthropic => match *alias {
                    "opus" => "claude-opus-5",
                    "sonnet" => "claude-sonnet-5",
                    "haiku" => "claude-haiku-4-5",
                    _ => trimmed,
                },
                ProviderKind::Xai => match *alias {
                    "grok" | "grok-3" => "grok-3",
                    "grok-mini" | "grok-3-mini" => "grok-3-mini",
                    "grok-2" => "grok-2",
                    _ => trimmed,
                },
                ProviderKind::OpenAi => trimmed,
            })
        })
        .map_or_else(|| trimmed.to_string(), ToOwned::to_owned)
}

#[must_use]
pub fn metadata_for_model(model: &str) -> Option<ProviderMetadata> {
    let canonical = resolve_model_alias(model);
    if canonical.starts_with("claude") {
        return Some(ProviderMetadata {
            provider: ProviderKind::Anthropic,
            auth_env: "ANTHROPIC_API_KEY",
            base_url_env: "ANTHROPIC_BASE_URL",
            default_base_url: anthropic::DEFAULT_BASE_URL,
        });
    }
    if canonical.starts_with("grok") {
        return Some(ProviderMetadata {
            provider: ProviderKind::Xai,
            auth_env: "XAI_API_KEY",
            base_url_env: "XAI_BASE_URL",
            default_base_url: openai_compat::DEFAULT_XAI_BASE_URL,
        });
    }
    None
}

#[must_use]
pub fn detect_provider_kind(model: &str) -> ProviderKind {
    if let Some(metadata) = metadata_for_model(model) {
        return metadata.provider;
    }
    if anthropic::has_auth_from_env_or_saved().unwrap_or(false) {
        return ProviderKind::Anthropic;
    }
    if openai_compat::has_api_key("OPENAI_API_KEY") {
        return ProviderKind::OpenAi;
    }
    if openai_compat::has_api_key("XAI_API_KEY") {
        return ProviderKind::Xai;
    }
    ProviderKind::Anthropic
}

/// Context-window size for a model.
///
/// Companion to [`max_tokens_for_model`] — the two are the pair a client needs
/// to validate a request, so they live together and are updated together.
/// Claude 5-family models carry a 1M window; Haiku 4.5 and the Grok models are
/// smaller.
#[must_use]
pub fn context_window_for_model(model: &str) -> u32 {
    let canonical = resolve_model_alias(model);
    if canonical.contains("opus") || canonical.contains("sonnet") {
        1_000_000
    } else if canonical.contains("haiku") {
        200_000
    } else {
        131_072
    }
}

/// Output-token ceiling for a model.
///
/// This used to cap Opus at 32k — *below* the 64k applied to everything else —
/// which inverted the real limits: Opus 5 and Sonnet 5 both support 128k output,
/// while Haiku 4.5 tops out at 64k.
///
/// Values above ~16k require a STREAMING request; a non-streaming call with a
/// large ceiling risks an HTTP timeout before the response completes. Every
/// caller of this function must therefore be on the streaming path.
#[must_use]
pub fn max_tokens_for_model(model: &str) -> u32 {
    let canonical = resolve_model_alias(model);
    if canonical.contains("opus") || canonical.contains("sonnet") {
        128_000
    } else {
        64_000
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ProviderKind, context_window_for_model, detect_provider_kind, max_tokens_for_model,
        resolve_model_alias,
    };

    #[test]
    fn resolves_grok_aliases() {
        assert_eq!(resolve_model_alias("grok"), "grok-3");
        assert_eq!(resolve_model_alias("grok-mini"), "grok-3-mini");
        assert_eq!(resolve_model_alias("grok-2"), "grok-2");
    }

    #[test]
    fn detects_provider_from_model_name_first() {
        assert_eq!(detect_provider_kind("grok"), ProviderKind::Xai);
        assert_eq!(
            detect_provider_kind("claude-sonnet-5"),
            ProviderKind::Anthropic
        );
    }

    #[test]
    fn resolves_anthropic_aliases_to_undated_ids() {
        assert_eq!(resolve_model_alias("opus"), "claude-opus-5");
        assert_eq!(resolve_model_alias("sonnet"), "claude-sonnet-5");
        assert_eq!(resolve_model_alias("haiku"), "claude-haiku-4-5");
    }

    #[test]
    fn context_window_tracks_real_model_limits() {
        assert_eq!(context_window_for_model("opus"), 1_000_000);
        assert_eq!(context_window_for_model("sonnet"), 1_000_000);
        assert_eq!(context_window_for_model("haiku"), 200_000);
        assert_eq!(context_window_for_model("grok-3"), 131_072);
    }

    #[test]
    fn max_tokens_tracks_real_model_ceilings() {
        // Opus 5 / Sonnet 5 support 128k output; Haiku 4.5 tops out at 64k.
        assert_eq!(max_tokens_for_model("opus"), 128_000);
        assert_eq!(max_tokens_for_model("sonnet"), 128_000);
        assert_eq!(max_tokens_for_model("haiku"), 64_000);
        assert_eq!(max_tokens_for_model("grok-3"), 64_000);
    }
}
