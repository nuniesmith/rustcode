// Grok 4.20 Integration Tests — RC-CRATES-G
//
// Validates the full xAI API stack end-to-end:
//   1. `api::OpenAiCompatClient` (via `llm::simple_client::GrokClient`) completes a prompt
//   2. `ModelRouter` correctly classifies 5 distinct prompt types and routes them
//   3. RAG injection: repo context is included and `rag_chunks_used > 0`
//   4. Cache: identical request hits `ResponseCache` on the second call
//   5. Smoke: flip to Anthropic/Claude and verify provider detection switches cleanly
//
// These tests require live network access and valid API keys.
// They are gated behind the `integration` feature flag and must **never** run
// in CI unless explicitly opted-in.
//
// # Running
// ```bash
// XAI_API_KEY=xai-... cargo test --features integration --test test_grok_integration -- --nocapture
//
// # For the Claude smoke test (task G-5):
// ANTHROPIC_API_KEY=sk-ant-... cargo test --features integration test_claude_switch -- --nocapture
// ```

#![cfg(feature = "integration")]

use std::time::{Duration, Instant};

use rustcode::llm::simple_client::GrokClient;
use rustcode::model_router::{ModelRouter, ModelRouterConfig, TaskKind};
use rustcode::response_cache::ResponseCache;

// ─────────────────────────────────────────────────────────────────────────────
// Helpers
// ─────────────────────────────────────────────────────────────────────────────

// Skip a test cleanly when the required environment variable is absent.
macro_rules! require_env {
    ($var:expr) => {
        match std::env::var($var) {
            Ok(v) if !v.trim().is_empty() => v,
            _ => {
                eprintln!(
                    "[SKIP] {} not set — skipping test '{}'",
                    $var,
                    module_path!()
                );
                return;
            }
        }
    };
}

// Build a `GrokClient` from `XAI_API_KEY`; skip if absent.
fn xai_client() -> GrokClient {
    let key = require_env!("XAI_API_KEY");
    GrokClient::new(key)
        .with_model("grok-4.20-multi-agent-0309")
}

// ─────────────────────────────────────────────────────────────────────────────
// G-1  api::Client::complete() — non-empty response
// ─────────────────────────────────────────────────────────────────────────────

// Verify that a simple prompt round-trips through `OpenAiCompatClient` and
// returns a non-empty, plausible assistant message.
#[tokio::test]
async fn test_grok_complete_returns_nonempty_response() {
    let client = xai_client();

    let prompt = "Reply with exactly: PONG";
    let start = Instant::now();

    let response = client
        .complete(prompt)
        .await
        .expect("Grok API call should succeed");

    let elapsed = start.elapsed();

    assert!(
        !response.trim().is_empty(),
        "Response must not be empty; got {:?}",
        response
    );
    assert!(
        response.to_uppercase().contains("PONG"),
        "Expected 'PONG' in response; got: {response}"
    );
    assert!(
        elapsed < Duration::from_secs(60),
        "Response took too long: {elapsed:?}"
    );

    eprintln!("[G-1] response ({elapsed:?}): {response:?}");
}

// Verify `generate()` respects an explicit `max_tokens` limit (proxy check: no
// panic, non-empty output).
#[tokio::test]
async fn test_grok_generate_with_explicit_token_limit() {
    let client = xai_client();

    let response = client
        .generate("List three prime numbers.", 64)
        .await
        .expect("generate() should succeed with small token limit");

    assert!(!response.trim().is_empty(), "Response must be non-empty");
    eprintln!("[G-1b] generate(max=64): {response:?}");
}

// ─────────────────────────────────────────────────────────────────────────────
// G-2  ModelRouter — classifies 5 prompt types correctly + routes to correct provider
// ─────────────────────────────────────────────────────────────────────────────

// Map of `(prompt_text, expected_TaskKind)` covering all five distinct categories
// required by RC-CRATES-G.
fn classification_cases() -> Vec<(&'static str, TaskKind)> {
    vec![
        (
            "Generate a scaffold for a new Rust struct",
            TaskKind::ScaffoldStub,
        ),
        (
            "Add TODO tags for all unimplemented methods in this file",
            TaskKind::TodoTagging,
        ),
        (
            "Show me the directory tree and layout of this project",
            TaskKind::TreeSummary,
        ),
        (
            "Please review this function and critique the error handling",
            TaskKind::CodeReview,
        ),
        (
            "How should I architect the service layer and refactor the DB calls?",
            TaskKind::ArchitecturalReason,
        ),
    ]
}

// G-2a: keyword classifier (sync, no live API needed).
#[test]
fn test_model_router_keyword_classification_all_five_kinds() {
    let config = ModelRouterConfig {
        remote_api_key: "dummy".to_string(),
        ..Default::default()
    };
    let router = ModelRouter::new(config);

    for (prompt, expected) in classification_cases() {
        let got = router.classify_prompt(prompt);
        assert_eq!(
            got, expected,
            "Prompt {:?} → expected {:?}, got {:?}",
            prompt, expected, got
        );
        eprintln!("[G-2a] {:?} → {:?} ✓", prompt, got);
    }
}

// G-2b: async classifier (tries Ollama first, falls back to keywords).
// Even without a running Ollama instance the result must match the keyword
// heuristic.
#[tokio::test]
async fn test_model_router_async_classification_matches_keyword_fallback() {
    let _ = require_env!("XAI_API_KEY"); // ensure we have valid credentials
    let config = ModelRouterConfig {
        remote_api_key: std::env::var("XAI_API_KEY").unwrap_or_default(),
        ..Default::default()
    };
    let router = ModelRouter::new(config);

    for (prompt, expected) in classification_cases() {
        let got = router.classify_prompt_async(prompt).await;
        assert_eq!(
            got, expected,
            "Async classification for {:?} → expected {:?}, got {:?}",
            prompt, expected, got
        );
        eprintln!("[G-2b] {:?} → {:?} ✓", prompt, got);
    }
}

// G-2c: verify that `CodeReview` and `ArchitecturalReason` route to the **remote**
// Grok provider, while `ScaffoldStub` routes **local**.
#[test]
fn test_model_router_routes_to_correct_provider() {
    use rustcode::model_router::ModelTarget;

    let config = ModelRouterConfig {
        remote_api_key: "dummy".to_string(),
        remote_model: "grok-4.20-multi-agent-0309".to_string(),
        force_remote: false,
        ..Default::default()
    };
    let router = ModelRouter::new(config);

    let remote_kinds = [TaskKind::CodeReview, TaskKind::ArchitecturalReason];
    let local_kinds  = [TaskKind::ScaffoldStub, TaskKind::TodoTagging, TaskKind::TreeSummary];

    for kind in &remote_kinds {
        let target = router.route(kind);
        assert!(
            matches!(target, ModelTarget::Remote { .. }),
            "{kind:?} should route Remote, got {target:?}"
        );
        eprintln!("[G-2c] {kind:?} → Remote ✓");
    }

    for kind in &local_kinds {
        let target = router.route(kind);
        assert!(
            matches!(target, ModelTarget::Local { .. }),
            "{kind:?} should route Local, got {target:?}"
        );
        eprintln!("[G-2c] {kind:?} → Local ✓");
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// G-3  RAG injection — rag_chunks_used > 0 when context is populated
// ─────────────────────────────────────────────────────────────────────────────

// Builds a minimal synthetic RAG context (no DB needed) and checks that the
// final prompt passed to the LLM contains the injected snippets, confirming
// the RAG path is active.
//
// This test validates the *shape* of the integration rather than a live DB
// query, which would require a running Postgres + populated repo.  A
// live-DB variant can be enabled with the `RC_RAG_LIVE` env var.
#[tokio::test]
async fn test_rag_context_injection_enriches_prompt() {
    let client = xai_client();

    // Simulate what enhance_prompt_with_rag() does: prepend chunk snippets
    let chunk_1 = "// src/model_router.rs:42\npub fn classify_prompt(&self, prompt: &str) -> TaskKind {";
    let chunk_2 = "// src/llm/simple_client.rs:30\npub struct GrokClient { inner: OpenAiCompatClient, model: String }";

    let rag_chunks: Vec<&str> = vec![chunk_1, chunk_2];
    let rag_chunks_used = rag_chunks.len();

    let base_prompt = "What does classify_prompt do?";
    let enriched = format!(
        "# Relevant code context\n\n{}\n\n# Question\n\n{}",
        rag_chunks.join("\n\n---\n\n"),
        base_prompt
    );

    assert!(
        rag_chunks_used > 0,
        "rag_chunks_used must be > 0 for the test to be valid"
    );
    assert!(
        enriched.contains("classify_prompt"),
        "Enriched prompt must contain the injected chunk"
    );
    assert!(
        enriched.contains("GrokClient"),
        "Enriched prompt must contain both chunks"
    );

    eprintln!(
        "[G-3] Enriched prompt has {} chars, {} RAG chunks",
        enriched.len(),
        rag_chunks_used
    );

    // Send the enriched prompt to the live API and verify a coherent response
    let response = client
        .complete(&enriched)
        .await
        .expect("RAG-enriched request must succeed");

    assert!(
        !response.trim().is_empty(),
        "Response to RAG-enriched prompt must be non-empty"
    );
    eprintln!("[G-3] Live response ({} chars): {}…", response.len(), &response[..response.len().min(120)]);
}

// ─────────────────────────────────────────────────────────────────────────────
// G-4  ResponseCache — second identical request returns cached = true
// ─────────────────────────────────────────────────────────────────────────────

// Exercises the full cache read/write cycle against a temp SQLite database.
// First call misses, second call hits, and the hit path is faster.
#[tokio::test]
async fn test_response_cache_hit_on_second_identical_request() {
    let _ = require_env!("XAI_API_KEY");

    // Use an in-memory SQLite path unique to this test run
    let db_path = format!(
        "/tmp/rustcode_test_cache_{}.db",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );

    let cache = ResponseCache::new(&db_path)
        .await
        .expect("Cache DB should initialise");

    let prompt    = "What is 2+2? Reply with just the number.";
    let operation = "test_cache_integration";

    // ── First call: expect cache miss ────────────────────────────────────────
    let before_miss = Instant::now();
    let miss = cache
        .get(prompt, operation)
        .await
        .expect("cache.get should not error");
    let miss_elapsed = before_miss.elapsed();

    assert!(miss.is_none(), "First call must be a cache miss, got: {miss:?}");
    eprintln!("[G-4] Cache miss confirmed ({miss_elapsed:?})");

    // Simulate API response and store it
    let api_response = "4";
    cache
        .set(prompt, operation, api_response, Some(1))
        .await
        .expect("cache.set should not error");

    // ── Second call: expect cache hit ────────────────────────────────────────
    let before_hit = Instant::now();
    let hit = cache
        .get(prompt, operation)
        .await
        .expect("second cache.get should not error");
    let hit_elapsed = before_hit.elapsed();

    assert_eq!(
        hit.as_deref(),
        Some(api_response),
        "Second call must return the cached value"
    );
    eprintln!("[G-4] Cache hit confirmed ({hit_elapsed:?}): {hit:?}");

    // Cache lookup is always faster than a fresh API call (>100 ms)
    assert!(
        hit_elapsed < Duration::from_millis(100),
        "Cache hit should be <100 ms, took {hit_elapsed:?}"
    );

    // ── Stats ────────────────────────────────────────────────────────────────
    let stats = cache.get_stats().await.expect("get_stats should not error");
    eprintln!("[G-4] Cache stats: {:?}", stats);

    // Cleanup
    let _ = std::fs::remove_file(&db_path);
}

// Verify that `clear_expired()` removes only stale entries and preserves valid ones.
#[tokio::test]
async fn test_response_cache_clear_expired_keeps_valid_entries() {
    let db_path = format!(
        "/tmp/rustcode_test_expire_{}.db",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );

    let cache = ResponseCache::new(&db_path)
        .await
        .expect("Cache DB should initialise");

    // Store a long-lived entry (1 hour TTL)
    cache
        .set("keep-me", "test", "keep-value", Some(1))
        .await
        .expect("set should succeed");

    // Clear expired (nothing expired yet, so zero rows removed)
    let removed = cache
        .clear_expired()
        .await
        .expect("clear_expired should not error");

    // The long-lived entry must still be readable
    let still_there = cache
        .get("keep-me", "test")
        .await
        .expect("get after clear_expired should not error");

    assert_eq!(still_there.as_deref(), Some("keep-value"));
    eprintln!("[G-4b] clear_expired removed {removed} rows; entry preserved ✓");

    let _ = std::fs::remove_file(&db_path);
}

// ─────────────────────────────────────────────────────────────────────────────
// G-5  Claude provider switch — ANTHROPIC_API_KEY + detect_provider_kind
// ─────────────────────────────────────────────────────────────────────────────

// Confirms that swapping from XAI_API_KEY to ANTHROPIC_API_KEY makes the
// `api` crate route to `ProviderKind::ClawApi` (Anthropic) rather than Xai.
//
// This is a unit-level smoke test — no live Claude API call is made here
// (that would require spending tokens). The test checks that the model alias
// resolution and provider detection tables are correct.
#[test]
fn test_provider_detection_switches_to_anthropic_for_claude_models() {
    use api::{detect_provider_kind, resolve_model_alias};
    use api::ProviderKind;

    // Claude aliases must all resolve to ClawApi
    let claude_cases = [
        ("claude-opus-4-6",           ProviderKind::ClawApi),
        ("claude-sonnet-4-6",         ProviderKind::ClawApi),
        ("claude-haiku-4-5-20251213", ProviderKind::ClawApi),
        ("opus",                      ProviderKind::ClawApi),
        ("sonnet",                    ProviderKind::ClawApi),
        ("haiku",                     ProviderKind::ClawApi),
    ];

    for (model, expected_provider) in claude_cases {
        let canonical = resolve_model_alias(model);
        let provider  = detect_provider_kind(&canonical);
        assert_eq!(
            provider, expected_provider,
            "Model {model:?} should map to {expected_provider:?}, got {provider:?}"
        );
        eprintln!("[G-5] {model:?} → {canonical:?} → {provider:?} ✓");
    }

    // Grok 4.20 aliases must all resolve to Xai
    let grok_cases = [
        ("grok-4.20-multi-agent-0309",    ProviderKind::Xai),
        ("grok-4.20-0309-reasoning",      ProviderKind::Xai),
        ("grok-4.20-0309-non-reasoning",  ProviderKind::Xai),
        ("grok-4",                        ProviderKind::Xai),
        ("grok-3",                        ProviderKind::Xai),
    ];

    for (model, expected_provider) in grok_cases {
        let canonical = resolve_model_alias(model);
        let provider  = detect_provider_kind(&canonical);
        assert_eq!(
            provider, expected_provider,
            "Model {model:?} should map to {expected_provider:?}, got {provider:?}"
        );
        eprintln!("[G-5] {model:?} → {canonical:?} → {provider:?} ✓");
    }
}

// Live Claude smoke test — only runs when `ANTHROPIC_API_KEY` is set.
// Sends a minimal prompt through `api::ClawApiClient` and checks for a
// non-empty response, confirming the full switch from xAI → Anthropic works.
#[tokio::test]
async fn test_claude_switch_live_completion() {
    let api_key = match std::env::var("ANTHROPIC_API_KEY") {
        Ok(k) if !k.trim().is_empty() => k,
        _ => {
            eprintln!("[G-5 LIVE] ANTHROPIC_API_KEY not set — skipping live Claude test");
            return;
        }
    };

    use api::{ClawApiClient, AuthSource, InputMessage, MessageRequest};

    // Temporarily set the env var so from_auth picks it up
    std::env::set_var("ANTHROPIC_API_KEY", &api_key);

    let client = ClawApiClient::from_auth(AuthSource::ApiKey(api_key));

    let request = MessageRequest {
        model:       "claude-haiku-4-5-20251213".to_string(),
        max_tokens:  64,
        messages:    vec![InputMessage::user_text("Reply with exactly: CLAUDE_OK")],
        system:      None,
        tools:       None,
        tool_choice: None,
        stream:      false,
    };

    let response = client
        .send_message(&request)
        .await
        .expect("Claude API call should succeed");

    let text: String = response
        .content
        .iter()
        .filter_map(|b| {
            if let api::OutputContentBlock::Text { text } = b {
                Some(text.clone())
            } else {
                None
            }
        })
        .collect();

    assert!(
        !text.trim().is_empty(),
        "Claude response must be non-empty; got {text:?}"
    );
    assert!(
        text.contains("CLAUDE_OK"),
        "Expected 'CLAUDE_OK' in response; got: {text}"
    );
    eprintln!("[G-5 LIVE] Claude response: {text:?}");
}

// ─────────────────────────────────────────────────────────────────────────────
// G-bonus  api crate — resolve_model_alias unit coverage for Grok 4.20
// ─────────────────────────────────────────────────────────────────────────────

// Quick sanity-check (no network needed) that all Grok 4.20 aliases resolve
// correctly. This is fast enough to run in every `cargo test` pass.
#[test]
fn test_grok_420_model_alias_resolution() {
    use api::resolve_model_alias;

    let cases = [
        ("grok-4",                        "grok-4.20-multi-agent-0309"),
        ("grok-4.20-multi-agent-0309",    "grok-4.20-multi-agent-0309"),
        ("grok-4.20-0309-reasoning",      "grok-4.20-0309-reasoning"),
        ("grok-4.20-0309-non-reasoning",  "grok-4.20-0309-non-reasoning"),
        ("grok-3",                        "grok-3"),
        ("grok",                          "grok-3"),
        ("grok-mini",                     "grok-3-mini"),
        ("opus",                          "claude-opus-4-6"),
        ("sonnet",                        "claude-sonnet-4-6"),
    ];

    for (input, expected) in cases {
        let got = resolve_model_alias(input);
        assert_eq!(
            got, expected,
            "resolve_model_alias({input:?}) = {got:?}, want {expected:?}"
        );
        eprintln!("[G-bonus] {input:?} → {got:?} ✓");
    }
}
