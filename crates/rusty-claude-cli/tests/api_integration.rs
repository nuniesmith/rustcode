// Integration coverage for the `api` crate — RC-CRATES-G
//
// Three end-to-end paths exercised here:
//   1. `api::AnthropicClient::send_message` round-trips through `MockAnthropicService`
//   2. `api::OpenAiCompatClient::send_message` consumes a canned xAI-shaped response
//      from a tiny inline TCP mock (the workspace mock is Anthropic-shaped)
//   3. `api::PromptCache` short-circuits the second identical request — the mock
//      records exactly one captured request even though `send_message` is called twice

use std::io;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use api::{
    AnthropicClient, AuthSource, InputMessage, MessageRequest, OpenAiCompatClient,
    OpenAiCompatConfig, OutputContentBlock, PromptCache,
};
use mock_anthropic_service::{MockAnthropicService, SCENARIO_PREFIX};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

fn assistant_text(blocks: &[OutputContentBlock]) -> String {
    blocks
        .iter()
        .filter_map(|block| match block {
            OutputContentBlock::Text { text } => Some(text.clone()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("")
}

fn scenario_request(model: &str, scenario: &str) -> MessageRequest {
    MessageRequest {
        model: model.to_string(),
        max_tokens: 64,
        messages: vec![InputMessage::user_text(format!(
            "{SCENARIO_PREFIX}{scenario}"
        ))],
        system: None,
        tools: None,
        tool_choice: None,
        temperature: None,
        response_format: None,
        stream: false,
    }
}

// G-mock-1 — Round-trip via api::AnthropicClient through MockAnthropicService.
//
// The mock returns a canned `MessageResponse` for the `streaming_text` scenario.
// We send a single non-streaming request through `AnthropicClient::send_message`
// and assert (a) the body parses into the expected text and (b) the mock observed
// exactly one captured request with the matching scenario tag.
#[test]
fn anthropic_client_round_trips_through_mock_service() {
    let runtime = tokio::runtime::Runtime::new().expect("tokio runtime should build");
    runtime.block_on(async {
        let mock = MockAnthropicService::spawn()
            .await
            .expect("mock service should start");

        let client = AnthropicClient::from_auth(AuthSource::ApiKey("test-key".into()))
            .with_base_url(mock.base_url());

        let request = scenario_request("claude-sonnet-4-6", "streaming_text");
        let response = client
            .send_message(&request)
            .await
            .expect("send_message should succeed");

        let text = assistant_text(&response.content);
        assert!(
            text.contains("Mock streaming"),
            "expected streaming_text canned reply, got: {text:?}"
        );

        let captured = mock.captured_requests().await;
        assert_eq!(captured.len(), 1, "mock should observe one request");
        assert_eq!(captured[0].scenario, "streaming_text");
        assert_eq!(captured[0].method, "POST");
        assert!(
            captured[0]
                .headers
                .get("x-api-key")
                .is_some_and(|v| v == "test-key"),
            "x-api-key header should propagate, got headers: {:?}",
            captured[0].headers
        );
    });
}

// G-mock-1b — AnthropicClient strips `response_format` from the outgoing body.
//
// `response_format` is an OpenAI-only concept; Anthropic's `/v1/messages`
// endpoint would reject it as an unknown top-level field. Verify the captured
// request body has no `response_format` key even when the caller set it.
#[test]
fn anthropic_client_strips_response_format_from_request_body() {
    use api::ResponseFormat;
    let runtime = tokio::runtime::Runtime::new().expect("tokio runtime should build");
    runtime.block_on(async {
        let mock = MockAnthropicService::spawn()
            .await
            .expect("mock service should start");

        let client = AnthropicClient::from_auth(AuthSource::ApiKey("test-key".into()))
            .with_base_url(mock.base_url());

        let request = scenario_request("claude-sonnet-4-6", "streaming_text")
            .with_response_format(ResponseFormat::JsonObject);

        let _ = client
            .send_message(&request)
            .await
            .expect("send_message should succeed");

        let captured = mock.captured_requests().await;
        assert_eq!(captured.len(), 1);
        let body: serde_json::Value =
            serde_json::from_str(&captured[0].raw_body).expect("captured body should parse");
        assert!(
            body.get("response_format").is_none(),
            "response_format must be stripped before sending to Anthropic; got body: {body}"
        );
    });
}

// G-mock-2 — OpenAiCompatClient consumes a canned xAI-shaped response.
//
// The workspace mock is Anthropic-shaped, so we spin up a tiny inline TCP server
// that returns one OpenAI/xAI-style chat completion. The response is normalized
// through `api::providers::openai_compat::normalize_response` and surfaced as a
// `MessageResponse` with `OutputContentBlock::Text`.
#[test]
fn openai_compat_client_consumes_openai_shaped_response() {
    let runtime = tokio::runtime::Runtime::new().expect("tokio runtime should build");
    runtime.block_on(async {
        let mock = SimpleHttpMock::spawn(
            r#"{
                "id": "chatcmpl-grok-test",
                "model": "grok-3",
                "choices": [
                    {
                        "message": {
                            "role": "assistant",
                            "content": "hello from xAI mock"
                        },
                        "finish_reason": "stop"
                    }
                ],
                "usage": {"prompt_tokens": 5, "completion_tokens": 4}
            }"#,
        )
        .await
        .expect("simple mock should start");

        let client = OpenAiCompatClient::new("xai-test-key", OpenAiCompatConfig::xai())
            .with_base_url(mock.base_url());

        let request = MessageRequest {
            model: "grok-3".into(),
            max_tokens: 32,
            messages: vec![InputMessage::user_text("ping")],
            system: None,
            tools: None,
            tool_choice: None,
            temperature: None,
            response_format: None,
            stream: false,
        };

        let response = client
            .send_message(&request)
            .await
            .expect("openai-compat send_message should succeed");

        let text = assistant_text(&response.content);
        assert_eq!(text, "hello from xAI mock");
        assert_eq!(response.model, "grok-3");

        let captured = mock.captured_requests();
        assert_eq!(captured.len(), 1, "mock should observe one request");
        assert_eq!(captured[0].method, "POST");
        assert!(
            captured[0].path.ends_with("/chat/completions"),
            "request should hit /chat/completions, got path: {:?}",
            captured[0].path
        );
        assert!(
            captured[0]
                .headers
                .get("authorization")
                .is_some_and(|v| v == "Bearer xai-test-key"),
            "Bearer auth header should propagate, got headers: {:?}",
            captured[0].headers
        );
        // Outgoing body should include the model and the user message text.
        assert!(
            captured[0].body.contains("\"grok-3\""),
            "outgoing body should carry the model, got: {}",
            captured[0].body
        );
        assert!(
            captured[0].body.contains("ping"),
            "outgoing body should carry the user prompt, got: {}",
            captured[0].body
        );
    });
}

// G-mock-3 — PromptCache returns a cached response on the second identical call.
//
// `AnthropicClient::send_message` consults the attached `PromptCache` before
// making a network request. After one successful round-trip the response is
// persisted on disk under a unique `session_id`. A second `send_message` with
// the same `MessageRequest` should be served from cache and the mock service
// should only observe a single captured request.
#[test]
fn prompt_cache_short_circuits_second_identical_request() {
    // `PromptCache` resolves its on-disk root from `CLAUDE_CONFIG_HOME`; we set it
    // once globally so concurrent tests in this binary share the same temp dir.
    // Inside that root each test still gets a unique session_id, so no collisions.
    let _config_home = ensure_test_config_home();

    let runtime = tokio::runtime::Runtime::new().expect("tokio runtime should build");
    runtime.block_on(async {
        let mock = MockAnthropicService::spawn()
            .await
            .expect("mock service should start");

        let session_id = format!(
            "prompt-cache-roundtrip-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        );
        let cache = PromptCache::new(&session_id);
        let client = AnthropicClient::from_auth(AuthSource::ApiKey("test-key".into()))
            .with_base_url(mock.base_url())
            .with_prompt_cache(cache.clone());

        let request = scenario_request("claude-sonnet-4-6", "streaming_text");

        let first = client
            .send_message(&request)
            .await
            .expect("first send_message should succeed");
        let first_text = assistant_text(&first.content);

        // Second identical call — should be served from cache without a second
        // request hitting the mock.
        let second = client
            .send_message(&request)
            .await
            .expect("second send_message should succeed");
        let second_text = assistant_text(&second.content);

        assert_eq!(
            first_text, second_text,
            "cached response should match the live response"
        );
        assert!(!first_text.is_empty(), "response text should be non-empty");

        let captured = mock.captured_requests().await;
        assert_eq!(
            captured.len(),
            1,
            "mock should only observe one request; second should be served from cache"
        );

        let stats = cache.stats();
        assert_eq!(stats.completion_cache_misses, 1, "first call must miss");
        assert_eq!(stats.completion_cache_hits, 1, "second call must hit");
        assert_eq!(stats.completion_cache_writes, 1, "exactly one write");

        // Cleanup the per-session subdir; the shared root cleans up via OnceLock.
        let paths = cache.paths();
        let _ = std::fs::remove_dir_all(&paths.session_dir);
    });
}

// ----------------------------------------------------------------------------
// Test helpers
// ----------------------------------------------------------------------------

// Set `CLAUDE_CONFIG_HOME` to a unique temp dir for the duration of this test
// binary. `PromptCache` reads it on every `for_session` lookup so any cache test
// in this file should call this first. Returns a guard whose Drop best-effort
// cleans up the temp dir.
#[allow(unsafe_code)]
fn ensure_test_config_home() -> &'static ConfigHomeGuard {
    static GUARD: OnceLock<ConfigHomeGuard> = OnceLock::new();
    GUARD.get_or_init(|| {
        let root = std::env::temp_dir().join(format!(
            "rustcode-api-integration-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).expect("temp config home should be creatable");
        // SAFETY: set_var is unsafe in Rust 2024. We initialise exactly once via
        // OnceLock before any test reads `CLAUDE_CONFIG_HOME`, so there is no
        // race with concurrent readers within this binary.
        unsafe {
            std::env::set_var("CLAUDE_CONFIG_HOME", &root);
        }
        ConfigHomeGuard { root }
    })
}

struct ConfigHomeGuard {
    root: std::path::PathBuf,
}

impl Drop for ConfigHomeGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

// Minimal HTTP/1.1 mock that returns a single canned JSON body to every POST.
// Captures the request line, headers, and body so tests can assert on the
// outgoing shape (path, auth header, etc.).
struct SimpleHttpMock {
    base_url: String,
    captured: std::sync::Arc<Mutex<Vec<CapturedHttpRequest>>>,
    _handle: JoinHandle<()>,
}

#[derive(Debug, Clone)]
struct CapturedHttpRequest {
    method: String,
    path: String,
    headers: std::collections::HashMap<String, String>,
    body: String,
}

impl SimpleHttpMock {
    async fn spawn(canned_body: &'static str) -> io::Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let captured = std::sync::Arc::new(Mutex::new(Vec::new()));
        let captured_state = std::sync::Arc::clone(&captured);
        // Each accepted connection serves the same canned body. We do not loop
        // for keep-alive — every connection handles one request and closes.
        let counter = std::sync::Arc::new(AtomicU32::new(0));
        let handle = tokio::spawn(async move {
            loop {
                let Ok((socket, _)) = listener.accept().await else {
                    break;
                };
                let captured_state = std::sync::Arc::clone(&captured_state);
                let counter = std::sync::Arc::clone(&counter);
                tokio::spawn(async move {
                    counter.fetch_add(1, Ordering::SeqCst);
                    let _ = serve_one(socket, captured_state, canned_body).await;
                });
            }
        });
        Ok(Self {
            base_url: format!("http://{address}"),
            captured,
            _handle: handle,
        })
    }

    fn base_url(&self) -> String {
        self.base_url.clone()
    }

    fn captured_requests(&self) -> Vec<CapturedHttpRequest> {
        self.captured.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

async fn serve_one(
    mut socket: tokio::net::TcpStream,
    captured: std::sync::Arc<Mutex<Vec<CapturedHttpRequest>>>,
    canned_body: &'static str,
) -> io::Result<()> {
    let mut buffer = Vec::new();
    let mut header_end = None;
    loop {
        let mut chunk = [0_u8; 1024];
        let read = socket.read(&mut chunk).await?;
        if read == 0 {
            break;
        }
        buffer.extend_from_slice(&chunk[..read]);
        if let Some(position) = buffer.windows(4).position(|w| w == b"\r\n\r\n") {
            header_end = Some(position);
            break;
        }
    }
    let header_end = header_end
        .ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "missing http headers"))?;
    let (header_bytes, remaining) = buffer.split_at(header_end);
    let header_text = String::from_utf8(header_bytes.to_vec())
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
    let mut lines = header_text.split("\r\n");
    let request_line = lines
        .next()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing request line"))?;
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("").to_string();
    let path = parts.next().unwrap_or("").to_string();

    let mut headers = std::collections::HashMap::new();
    let mut content_length = 0_usize;
    for line in lines {
        if line.is_empty() {
            continue;
        }
        let (name, value) = line.split_once(':').ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, "malformed http header line")
        })?;
        let value = value.trim().to_string();
        if name.eq_ignore_ascii_case("content-length") {
            content_length = value.parse().unwrap_or(0);
        }
        headers.insert(name.to_ascii_lowercase(), value);
    }

    let mut body = remaining[4..].to_vec();
    while body.len() < content_length {
        let mut chunk = vec![0_u8; content_length - body.len()];
        let read = socket.read(&mut chunk).await?;
        if read == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..read]);
    }
    let body = String::from_utf8_lossy(&body).into_owned();

    captured
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push(CapturedHttpRequest {
            method,
            path,
            headers,
            body,
        });

    let response = format!(
        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
        canned_body.len(),
        canned_body
    );
    socket.write_all(response.as_bytes()).await?;
    Ok(())
}
