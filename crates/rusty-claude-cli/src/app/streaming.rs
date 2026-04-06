use std::io::{self, Write};

mod streaming_renderer {
    include!("streaming_renderer.rs");
}
use crate::render::{Spinner, TerminalRenderer};
use api::Usage;
use api::{ContentBlockDelta, OutputContentBlock, StreamEvent};
use runtime::{ContentBlock, ConversationMessage, MessageRole};
pub use streaming_renderer::render_stream_event;

/// Pure accumulation function that processes a stream event without any I/O or rendering.
///
/// This function updates the messages vector and usage based on the incoming event.
/// It has no side effects and is fully testable in isolation, making it easy to verify
/// accumulation logic independently from rendering behavior.
pub(crate) fn accumulate_stream_event(
    messages: &mut Vec<ConversationMessage>,
    usage: &mut Usage,
    event: &StreamEvent,
) {
    match event {
        StreamEvent::MessageStart(start) => {
            for block in &start.message.content {
                match block {
                    OutputContentBlock::Text { text } => {
                        if let Some(last) = messages.last_mut() {
                            if let Some(ContentBlock::Text { text: prev }) = last.blocks.last_mut()
                            {
                                prev.push_str(text);
                            } else {
                                last.blocks.push(ContentBlock::Text { text: text.clone() });
                            }
                        } else {
                            messages.push(ConversationMessage {
                                role: MessageRole::Assistant,
                                blocks: vec![ContentBlock::Text { text: text.clone() }],
                                usage: None,
                            });
                        }
                    }
                    OutputContentBlock::ToolUse { id, name, input } => {
                        messages.push(ConversationMessage {
                            role: MessageRole::Assistant,
                            blocks: vec![ContentBlock::ToolUse {
                                id: id.clone(),
                                name: name.clone(),
                                input: input.to_string(),
                            }],
                            usage: None,
                        });
                    }
                    _ => {}
                }
            }
        }
        StreamEvent::ContentBlockStart(start) => match &start.content_block {
            OutputContentBlock::Text { text } => {
                if let Some(last) = messages.last_mut() {
                    if let Some(ContentBlock::Text { text: prev }) = last.blocks.last_mut() {
                        prev.push_str(text);
                    } else {
                        last.blocks.push(ContentBlock::Text { text: text.clone() });
                    }
                } else {
                    messages.push(ConversationMessage {
                        role: MessageRole::Assistant,
                        blocks: vec![ContentBlock::Text { text: text.clone() }],
                        usage: None,
                    });
                }
            }
            OutputContentBlock::ToolUse { id, name, input } => {
                messages.push(ConversationMessage {
                    role: MessageRole::Assistant,
                    blocks: vec![ContentBlock::ToolUse {
                        id: id.clone(),
                        name: name.clone(),
                        input: input.to_string(),
                    }],
                    usage: None,
                });
            }
            _ => {}
        },
        StreamEvent::ContentBlockDelta(delta) => match &delta.delta {
            api::ContentBlockDelta::TextDelta { text } => {
                if !text.is_empty() {
                    if let Some(last) = messages.last_mut() {
                        if let Some(ContentBlock::Text { text: prev }) = last.blocks.last_mut() {
                            prev.push_str(text);
                        } else {
                            last.blocks.push(ContentBlock::Text { text: text.clone() });
                        }
                    } else {
                        messages.push(ConversationMessage {
                            role: MessageRole::Assistant,
                            blocks: vec![ContentBlock::Text { text: text.clone() }],
                            usage: None,
                        });
                    }
                }
            }
            api::ContentBlockDelta::InputJsonDelta { partial_json } => {
                if let Some(last) = messages.last_mut() {
                    last.blocks.push(ContentBlock::ToolResult {
                        tool_use_id: "unknown".to_string(),
                        tool_name: "unknown".to_string(),
                        output: partial_json.clone(),
                        is_error: false,
                    });
                }
            }
            api::ContentBlockDelta::ThinkingDelta { .. }
            | api::ContentBlockDelta::SignatureDelta { .. } => {}
        },
        StreamEvent::ContentBlockStop(_) => {
            // no-op for accumulation
        }
        StreamEvent::MessageDelta(delta) => {
            // Update usage on each delta to capture the latest usage stats.
            *usage = delta.usage.clone();
        }
        StreamEvent::MessageStop(_) => {
            // end marker - no change to accumulation
        }
    }
}

/// Drain a std::sync mpsc receiver of StreamEvent results, render events to `out` using
/// `renderer`, and accumulate a Vec<ConversationMessage> plus Usage and a saw_text flag.
///
/// This function orchestrates the full streaming pipeline:
/// 1. Receives events from the channel
/// 2. Renders them to the terminal (side effects)
/// 3. Accumulates them into messages and usage (pure state)
///
/// By separating rendering and accumulation into independent functions, this keeps
/// the main loop clean and makes testing easier.
pub fn collect_stream_events(
    rx: std::sync::mpsc::Receiver<Result<StreamEvent, String>>,
    out: &mut impl Write,
    renderer: &TerminalRenderer,
) -> io::Result<(Vec<ConversationMessage>, Usage, bool)> {
    let mut assistant_messages: Vec<ConversationMessage> = Vec::new();
    let mut turn_usage = Usage::default();
    let mut stream_spinner = Spinner::new();
    let mut tool_spinner = Spinner::new();
    let mut saw_text = false;

    loop {
        match rx.recv() {
            Ok(Ok(event)) => {
                // Render the event to terminal (handles UI spinners, text output, etc).
                streaming_renderer::render_stream_event(
                    renderer,
                    &event,
                    &mut stream_spinner,
                    &mut tool_spinner,
                    &mut saw_text,
                    &mut turn_usage,
                    out,
                );

                // Accumulate the event into messages (pure state update).
                accumulate_stream_event(&mut assistant_messages, &mut turn_usage, &event);
            }
            Ok(Err(err_str)) => {
                stream_spinner.fail("Streaming response failed", renderer.color_theme(), out)?;
                return Err(io::Error::new(io::ErrorKind::Other, err_str));
            }
            Err(std::sync::mpsc::RecvError) => {
                break;
            }
        }
    }

    // finalize spinner behavior
    if saw_text {
        let _ = writeln!(out);
    } else {
        let _ = stream_spinner.finish("Streaming response", renderer.color_theme(), out);
    }

    Ok((assistant_messages, turn_usage, saw_text))
}

#[cfg(test)]
mod tests {
    use super::*;
    use api::{ContentBlockDeltaEvent, MessageDeltaEvent, MessageResponse, MessageStartEvent};

    #[test]
    fn accumulate_merges_consecutive_text_blocks() {
        // Test that multiple text events are merged into a single message
        let mut messages: Vec<ConversationMessage> = Vec::new();
        let mut usage = Usage::default();

        // First text event
        let start = MessageStartEvent {
            message: MessageResponse {
                id: "msg1".to_string(),
                kind: "message".to_string(),
                role: "assistant".to_string(),
                content: vec![OutputContentBlock::Text {
                    text: "Hello ".to_string(),
                }],
                model: "test".to_string(),
                stop_reason: None,
                stop_sequence: None,
                usage: Usage::default(),
                request_id: None,
            },
        };
        accumulate_stream_event(&mut messages, &mut usage, &StreamEvent::MessageStart(start));

        // Second text delta
        let delta = ContentBlockDeltaEvent {
            index: 0,
            delta: api::ContentBlockDelta::TextDelta {
                text: "world".to_string(),
            },
        };
        accumulate_stream_event(
            &mut messages,
            &mut usage,
            &StreamEvent::ContentBlockDelta(delta),
        );

        assert_eq!(messages.len(), 1);
        assert_eq!(
            messages[0].blocks,
            vec![ContentBlock::Text {
                text: "Hello world".to_string()
            }]
        );
    }

    #[test]
    fn accumulate_creates_separate_messages_for_tool_uses() {
        // Test that tool use blocks create new messages
        let mut messages: Vec<ConversationMessage> = Vec::new();
        let mut usage = Usage::default();

        // First message: text
        let start = MessageStartEvent {
            message: MessageResponse {
                id: "msg1".to_string(),
                kind: "message".to_string(),
                role: "assistant".to_string(),
                content: vec![OutputContentBlock::Text {
                    text: "Calling tool".to_string(),
                }],
                model: "test".to_string(),
                stop_reason: None,
                stop_sequence: None,
                usage: Usage::default(),
                request_id: None,
            },
        };
        accumulate_stream_event(&mut messages, &mut usage, &StreamEvent::MessageStart(start));

        // Add a tool use block
        let tool_start = api::ContentBlockStartEvent {
            index: 1,
            content_block: OutputContentBlock::ToolUse {
                id: "tool_1".to_string(),
                name: "calculate".to_string(),
                input: serde_json::json!({"x": 5}),
            },
        };
        accumulate_stream_event(
            &mut messages,
            &mut usage,
            &StreamEvent::ContentBlockStart(tool_start),
        );

        assert_eq!(messages.len(), 2);
        assert_eq!(
            messages[0].blocks[0],
            ContentBlock::Text {
                text: "Calling tool".to_string()
            }
        );
        match &messages[1].blocks[0] {
            ContentBlock::ToolUse { id, name, .. } => {
                assert_eq!(id, "tool_1");
                assert_eq!(name, "calculate");
            }
            other => panic!("expected ToolUse, got {:?}", other),
        }
    }

    #[test]
    fn accumulate_tracks_usage_from_message_delta() {
        // Test that usage is updated from MessageDelta events
        let mut messages: Vec<ConversationMessage> = Vec::new();
        let mut usage = Usage::default();

        assert_eq!(usage.input_tokens, 0);
        assert_eq!(usage.output_tokens, 0);

        let delta = MessageDeltaEvent {
            delta: api::MessageDelta {
                stop_reason: None,
                stop_sequence: None,
            },
            usage: Usage {
                input_tokens: 100,
                output_tokens: 25,
                cache_creation_input_tokens: 0,
                cache_read_input_tokens: 0,
            },
        };
        accumulate_stream_event(&mut messages, &mut usage, &StreamEvent::MessageDelta(delta));

        assert_eq!(usage.input_tokens, 100);
        assert_eq!(usage.output_tokens, 25);
    }

    #[test]
    fn accumulate_ignores_empty_text_deltas() {
        // Test that empty text deltas don't create messages
        let mut messages: Vec<ConversationMessage> = Vec::new();
        let mut usage = Usage::default();

        let delta = ContentBlockDeltaEvent {
            index: 0,
            delta: api::ContentBlockDelta::TextDelta {
                text: "".to_string(),
            },
        };
        accumulate_stream_event(
            &mut messages,
            &mut usage,
            &StreamEvent::ContentBlockDelta(delta),
        );

        assert!(messages.is_empty());
    }

    #[test]
    fn accumulate_handles_multiple_messages_with_mixed_content() {
        // Complex scenario: text, tool use, more text, another tool
        let mut messages: Vec<ConversationMessage> = Vec::new();
        let mut usage = Usage::default();

        // Message 1: initial text
        let start1 = MessageStartEvent {
            message: MessageResponse {
                id: "msg1".to_string(),
                kind: "message".to_string(),
                role: "assistant".to_string(),
                content: vec![OutputContentBlock::Text {
                    text: "I'll help. ".to_string(),
                }],
                model: "test".to_string(),
                stop_reason: None,
                stop_sequence: None,
                usage: Usage::default(),
                request_id: None,
            },
        };
        accumulate_stream_event(
            &mut messages,
            &mut usage,
            &StreamEvent::MessageStart(start1),
        );

        // Text delta
        let delta1 = ContentBlockDeltaEvent {
            index: 0,
            delta: api::ContentBlockDelta::TextDelta {
                text: "Let me call a tool.".to_string(),
            },
        };
        accumulate_stream_event(
            &mut messages,
            &mut usage,
            &StreamEvent::ContentBlockDelta(delta1),
        );

        // Tool use
        let tool = api::ContentBlockStartEvent {
            index: 1,
            content_block: OutputContentBlock::ToolUse {
                id: "t1".to_string(),
                name: "search".to_string(),
                input: serde_json::json!({"q": "example"}),
            },
        };
        accumulate_stream_event(
            &mut messages,
            &mut usage,
            &StreamEvent::ContentBlockStart(tool),
        );

        // Update usage
        let usage_delta = MessageDeltaEvent {
            delta: api::MessageDelta {
                stop_reason: Some("end_turn".to_string()),
                stop_sequence: None,
            },
            usage: Usage {
                input_tokens: 50,
                output_tokens: 10,
                cache_creation_input_tokens: 0,
                cache_read_input_tokens: 0,
            },
        };
        accumulate_stream_event(
            &mut messages,
            &mut usage,
            &StreamEvent::MessageDelta(usage_delta),
        );

        assert_eq!(messages.len(), 2);
        assert_eq!(
            messages[0].blocks[0],
            ContentBlock::Text {
                text: "I'll help. Let me call a tool.".to_string()
            }
        );
        assert_eq!(usage.input_tokens, 50);
        assert_eq!(usage.output_tokens, 10);
    }

    #[test]
    fn accumulate_is_idempotent_for_non_accumulating_events() {
        // ContentBlockStop, MessageStop should not change state
        let mut messages: Vec<ConversationMessage> = Vec::new();
        let mut usage = Usage::default();

        accumulate_stream_event(
            &mut messages,
            &mut usage,
            &StreamEvent::ContentBlockStop(api::ContentBlockStopEvent { index: 0 }),
        );
        accumulate_stream_event(
            &mut messages,
            &mut usage,
            &StreamEvent::MessageStop(api::MessageStopEvent { index: 0 }),
        );

        assert!(messages.is_empty());
        assert_eq!(usage.input_tokens, 0);
    }
}
