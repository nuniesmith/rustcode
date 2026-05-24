use runtime::{TokenUsage, UsageCostEstimate, pricing_for_model};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MessageRequest {
    pub model: String,
    pub max_tokens: u32,
    pub messages: Vec<InputMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system: Option<Vec<SystemBlock>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<ToolDefinition>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<ToolChoice>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_format: Option<ResponseFormat>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub stream: bool,
}

impl MessageRequest {
    #[must_use]
    pub fn with_streaming(mut self) -> Self {
        self.stream = true;
        self
    }

    #[must_use]
    pub fn with_temperature(mut self, temperature: f32) -> Self {
        self.temperature = Some(temperature);
        self
    }

    #[must_use]
    pub fn with_response_format(mut self, format: ResponseFormat) -> Self {
        self.response_format = Some(format);
        self
    }

    /// Replace the system prompt with a single uncached text block.
    #[must_use]
    pub fn with_system_text(mut self, text: impl Into<String>) -> Self {
        self.system = Some(vec![SystemBlock::text(text)]);
        self
    }

    /// Replace the system prompt with a single text block marked
    /// `cache_control: { type: "ephemeral" }`. The Anthropic API only
    /// honours the marker when the block is ≥ 1024 tokens
    /// (≥ 2048 for Haiku); shorter content is sent but not cached.
    #[must_use]
    pub fn with_cached_system_text(mut self, text: impl Into<String>) -> Self {
        self.system = Some(vec![SystemBlock::cached_text(text)]);
        self
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InputMessage {
    pub role: String,
    pub content: Vec<InputContentBlock>,
}

impl InputMessage {
    #[must_use]
    pub fn user_text(text: impl Into<String>) -> Self {
        Self {
            role: "user".to_string(),
            content: vec![InputContentBlock::Text {
                text: text.into(),
                cache_control: None,
            }],
        }
    }

    #[must_use]
    pub fn user_tool_result(
        tool_use_id: impl Into<String>,
        content: impl Into<String>,
        is_error: bool,
    ) -> Self {
        Self {
            role: "user".to_string(),
            content: vec![InputContentBlock::ToolResult {
                tool_use_id: tool_use_id.into(),
                content: vec![ToolResultContentBlock::Text {
                    text: content.into(),
                }],
                is_error,
            }],
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum InputContentBlock {
    Text {
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cache_control: Option<CacheControl>,
    },
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
    ToolResult {
        tool_use_id: String,
        content: Vec<ToolResultContentBlock>,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        is_error: bool,
    },
}

/// Anthropic prompt-cache marker. Attach to a `SystemBlock` or
/// `InputContentBlock::Text` to opt that block into the prompt cache.
///
/// Anthropic currently defines a single variant, `ephemeral`, which keeps the
/// cached prefix alive for ~5 minutes of inactivity. The block must be at
/// least 1024 tokens (2048 for Haiku) for the marker to take effect; shorter
/// blocks are still sent but bypass the cache.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CacheControl {
    #[serde(rename = "type")]
    pub kind: String,
}

impl CacheControl {
    #[must_use]
    pub fn ephemeral() -> Self {
        Self {
            kind: "ephemeral".to_string(),
        }
    }
}

/// A single block inside the Anthropic `system` field. The wire format
/// expects an array of `{ type: "text", text, cache_control? }` entries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SystemBlock {
    #[serde(rename = "type")]
    pub kind: String,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_control: Option<CacheControl>,
}

impl SystemBlock {
    /// Build an uncached `{ type: "text", text }` block.
    #[must_use]
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            kind: "text".to_string(),
            text: text.into(),
            cache_control: None,
        }
    }

    /// Build a `{ type: "text", text, cache_control: { type: "ephemeral" } }`
    /// block. The marker is only honoured by Anthropic when the block is
    /// large enough (≥ 1024 tokens for Sonnet/Opus, ≥ 2048 for Haiku).
    #[must_use]
    pub fn cached_text(text: impl Into<String>) -> Self {
        Self {
            kind: "text".to_string(),
            text: text.into(),
            cache_control: Some(CacheControl::ephemeral()),
        }
    }
}

impl From<String> for SystemBlock {
    fn from(value: String) -> Self {
        Self::text(value)
    }
}

impl From<&str> for SystemBlock {
    fn from(value: &str) -> Self {
        Self::text(value.to_string())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToolResultContentBlock {
    Text { text: String },
    Json { value: Value },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolDefinition {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub input_schema: Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToolChoice {
    Auto,
    Any,
    Tool { name: String },
}

// Provider-side response format hint. Currently honored only by
// `OpenAiCompatClient` (OpenAI / xAI) which translates it into the
// `response_format` field on the outgoing JSON payload. `AnthropicClient`
// strips the field before sending — Anthropic models surface structured
// output via tool use rather than a top-level format hint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ResponseFormat {
    Text,
    JsonObject,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct MessageResponse {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub role: String,
    pub content: Vec<OutputContentBlock>,
    pub model: String,
    #[serde(default)]
    pub stop_reason: Option<String>,
    #[serde(default)]
    pub stop_sequence: Option<String>,
    pub usage: Usage,
    #[serde(default)]
    pub request_id: Option<String>,
}

impl MessageResponse {
    #[must_use]
    pub fn total_tokens(&self) -> u32 {
        self.usage.total_tokens()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum OutputContentBlock {
    Text {
        text: String,
    },
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
    Thinking {
        #[serde(default)]
        thinking: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        signature: Option<String>,
    },
    RedactedThinking {
        data: Value,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Usage {
    pub input_tokens: u32,
    #[serde(default)]
    pub cache_creation_input_tokens: u32,
    #[serde(default)]
    pub cache_read_input_tokens: u32,
    pub output_tokens: u32,
}

impl Usage {
    #[must_use]
    pub const fn total_tokens(&self) -> u32 {
        self.input_tokens
            + self.output_tokens
            + self.cache_creation_input_tokens
            + self.cache_read_input_tokens
    }

    #[must_use]
    pub const fn token_usage(&self) -> TokenUsage {
        TokenUsage {
            input_tokens: self.input_tokens,
            output_tokens: self.output_tokens,
            cache_creation_input_tokens: self.cache_creation_input_tokens,
            cache_read_input_tokens: self.cache_read_input_tokens,
        }
    }

    #[must_use]
    pub fn estimated_cost_usd(&self, model: &str) -> UsageCostEstimate {
        let usage = self.token_usage();
        pricing_for_model(model).map_or_else(
            || usage.estimate_cost_usd(),
            |pricing| usage.estimate_cost_usd_with_pricing(pricing),
        )
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MessageStartEvent {
    pub message: MessageResponse,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MessageDeltaEvent {
    pub delta: MessageDelta,
    pub usage: Usage,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageDelta {
    #[serde(default)]
    pub stop_reason: Option<String>,
    #[serde(default)]
    pub stop_sequence: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContentBlockStartEvent {
    pub index: u32,
    pub content_block: OutputContentBlock,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContentBlockDeltaEvent {
    pub index: u32,
    pub delta: ContentBlockDelta,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlockDelta {
    TextDelta { text: String },
    InputJsonDelta { partial_json: String },
    ThinkingDelta { thinking: String },
    SignatureDelta { signature: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContentBlockStopEvent {
    pub index: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageStopEvent {}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StreamEvent {
    MessageStart(MessageStartEvent),
    MessageDelta(MessageDeltaEvent),
    ContentBlockStart(ContentBlockStartEvent),
    ContentBlockDelta(ContentBlockDeltaEvent),
    ContentBlockStop(ContentBlockStopEvent),
    MessageStop(MessageStopEvent),
}

#[cfg(test)]
mod tests {
    use runtime::format_usd;
    use serde_json::Value;

    use super::{InputMessage, MessageRequest, MessageResponse, Usage};

    #[test]
    fn usage_total_tokens_includes_cache_tokens() {
        let usage = Usage {
            input_tokens: 10,
            cache_creation_input_tokens: 2,
            cache_read_input_tokens: 3,
            output_tokens: 4,
        };

        assert_eq!(usage.total_tokens(), 19);
        assert_eq!(usage.token_usage().total_tokens(), 19);
    }

    #[test]
    fn message_response_estimates_cost_from_model_usage() {
        let response = MessageResponse {
            id: "msg_cost".to_string(),
            kind: "message".to_string(),
            role: "assistant".to_string(),
            content: Vec::new(),
            model: "claude-sonnet-4-20250514".to_string(),
            stop_reason: Some("end_turn".to_string()),
            stop_sequence: None,
            usage: Usage {
                input_tokens: 1_000_000,
                cache_creation_input_tokens: 100_000,
                cache_read_input_tokens: 200_000,
                output_tokens: 500_000,
            },
            request_id: None,
        };

        let cost = response.usage.estimated_cost_usd(&response.model);
        assert_eq!(format_usd(cost.total_cost_usd()), "$54.6750");
        assert_eq!(response.total_tokens(), 1_800_000);
    }

    #[test]
    fn temperature_serializes_when_set_and_is_omitted_when_none() {
        let mut request = MessageRequest {
            model: "claude-sonnet-4-6".to_string(),
            max_tokens: 32,
            messages: vec![InputMessage::user_text("hi")],
            system: None,
            tools: None,
            tool_choice: None,
            temperature: None,
            response_format: None,
            stream: false,
        };

        // None → field absent from the serialized JSON
        let json_none: Value = serde_json::to_value(&request).unwrap();
        assert!(json_none.get("temperature").is_none(), "got: {json_none}");

        // Builder method sets the field
        request = request.with_temperature(0.0);
        assert_eq!(request.temperature, Some(0.0));

        let json_some: Value = serde_json::to_value(&request).unwrap();
        assert_eq!(json_some.get("temperature").and_then(Value::as_f64), Some(0.0));
    }

    #[test]
    fn cached_system_text_serializes_with_cache_control_marker() {
        use super::{InputContentBlock, SystemBlock};

        let request = MessageRequest {
            model: "claude-sonnet-4-6".to_string(),
            max_tokens: 32,
            messages: vec![InputMessage::user_text("hi")],
            system: Some(vec![SystemBlock::cached_text("you are a helpful assistant")]),
            tools: None,
            tool_choice: None,
            temperature: None,
            response_format: None,
            stream: false,
        };

        let body: Value = serde_json::to_value(&request).unwrap();
        assert_eq!(
            body["system"],
            serde_json::json!([{
                "type": "text",
                "text": "you are a helpful assistant",
                "cache_control": { "type": "ephemeral" }
            }]),
            "got: {body}"
        );

        // Uncached text blocks must omit cache_control entirely.
        let plain = InputContentBlock::Text {
            text: "hello".to_string(),
            cache_control: None,
        };
        let plain_json = serde_json::to_value(&plain).unwrap();
        assert!(
            plain_json.get("cache_control").is_none(),
            "got: {plain_json}"
        );
    }

    #[test]
    fn system_text_helper_round_trips_through_with_system_text() {
        use super::SystemBlock;

        let request = MessageRequest {
            model: "claude-sonnet-4-6".to_string(),
            max_tokens: 32,
            messages: vec![InputMessage::user_text("hi")],
            system: None,
            tools: None,
            tool_choice: None,
            temperature: None,
            response_format: None,
            stream: false,
        }
        .with_system_text("base instructions");

        assert_eq!(
            request.system.as_deref(),
            Some([SystemBlock::text("base instructions")].as_slice())
        );
    }

    #[test]
    fn response_format_serializes_with_tagged_type_and_is_omitted_when_none() {
        use super::ResponseFormat;
        let mut request = MessageRequest {
            model: "grok-3".to_string(),
            max_tokens: 32,
            messages: vec![InputMessage::user_text("hi")],
            system: None,
            tools: None,
            tool_choice: None,
            temperature: None,
            response_format: None,
            stream: false,
        };

        let json_none: Value = serde_json::to_value(&request).unwrap();
        assert!(
            json_none.get("response_format").is_none(),
            "got: {json_none}"
        );

        request = request.with_response_format(ResponseFormat::JsonObject);
        assert_eq!(request.response_format, Some(ResponseFormat::JsonObject));

        let json_some: Value = serde_json::to_value(&request).unwrap();
        assert_eq!(
            json_some.get("response_format"),
            Some(&serde_json::json!({"type": "json_object"})),
            "got: {json_some}"
        );
    }
}
