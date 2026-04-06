use std::io::{self, Write};

mod streaming_renderer {
    include!("streaming_renderer.rs");
}
use crate::render::{Spinner, TerminalRenderer};
use api::Usage;
use api::{ContentBlockDelta, OutputContentBlock, StreamEvent};
use runtime::{ContentBlock, ConversationMessage, MessageRole};
pub use streaming_renderer::render_stream_event;

/// Drain a std::sync mpsc receiver of StreamEvent results, render events to `out` using
/// `renderer`, and accumulate a Vec<ConversationMessage> plus Usage and a saw_text flag.
///
/// This is a standalone, testable function intended to be called from the CLI app.
/// It duplicates the rendering/accumulation logic so it does not rely on private helpers
/// in other modules.
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
                // Delegate rendering to the shared renderer to avoid duplication.
                streaming_renderer::render_stream_event(
                    renderer,
                    &event,
                    &mut stream_spinner,
                    &mut tool_spinner,
                    &mut saw_text,
                    &mut turn_usage,
                    out,
                );

                // Accumulate assistant content for summary (mirror UI accumulation logic)
                match event {
                    StreamEvent::MessageStart(start) => {
                        for block in start.message.content {
                            match block {
                                OutputContentBlock::Text { text } => {
                                    if let Some(last) = assistant_messages.last_mut() {
                                        if let Some(ContentBlock::Text { text: prev }) =
                                            last.blocks.last_mut()
                                        {
                                            prev.push_str(&text);
                                        } else {
                                            last.blocks.push(ContentBlock::Text { text });
                                        }
                                    } else {
                                        assistant_messages.push(ConversationMessage {
                                            role: MessageRole::Assistant,
                                            blocks: vec![ContentBlock::Text { text }],
                                            usage: None,
                                        });
                                    }
                                }
                                OutputContentBlock::ToolUse { id, name, input } => {
                                    assistant_messages.push(ConversationMessage {
                                        role: MessageRole::Assistant,
                                        blocks: vec![ContentBlock::ToolUse {
                                            id,
                                            name,
                                            input: input.to_string(),
                                        }],
                                        usage: None,
                                    });
                                }
                                _ => {}
                            }
                        }
                    }
                    StreamEvent::ContentBlockStart(start) => match start.content_block {
                        OutputContentBlock::Text { text } => {
                            if let Some(last) = assistant_messages.last_mut() {
                                if let Some(ContentBlock::Text { text: prev }) =
                                    last.blocks.last_mut()
                                {
                                    prev.push_str(&text);
                                } else {
                                    last.blocks.push(ContentBlock::Text { text });
                                }
                            } else {
                                assistant_messages.push(ConversationMessage {
                                    role: MessageRole::Assistant,
                                    blocks: vec![ContentBlock::Text { text }],
                                    usage: None,
                                });
                            }
                        }
                        OutputContentBlock::ToolUse { id, name, input } => {
                            assistant_messages.push(ConversationMessage {
                                role: MessageRole::Assistant,
                                blocks: vec![ContentBlock::ToolUse {
                                    id,
                                    name,
                                    input: input.to_string(),
                                }],
                                usage: None,
                            });
                        }
                        _ => {}
                    },
                    StreamEvent::ContentBlockDelta(delta) => match delta.delta {
                        api::ContentBlockDelta::TextDelta { text } => {
                            if !text.is_empty() {
                                if let Some(last) = assistant_messages.last_mut() {
                                    if let Some(ContentBlock::Text { text: prev }) =
                                        last.blocks.last_mut()
                                    {
                                        prev.push_str(&text);
                                    } else {
                                        last.blocks.push(ContentBlock::Text { text });
                                    }
                                } else {
                                    assistant_messages.push(ConversationMessage {
                                        role: MessageRole::Assistant,
                                        blocks: vec![ContentBlock::Text { text }],
                                        usage: None,
                                    });
                                }
                            }
                        }
                        api::ContentBlockDelta::InputJsonDelta { partial_json } => {
                            if let Some(last) = assistant_messages.last_mut() {
                                last.blocks.push(ContentBlock::ToolResult {
                                    tool_use_id: "unknown".to_string(),
                                    tool_name: "unknown".to_string(),
                                    output: partial_json,
                                    is_error: false,
                                });
                            }
                        }
                        _ => {}
                    },
                    StreamEvent::ContentBlockStop(_) => {
                        // no-op
                    }
                    StreamEvent::MessageDelta(_) => {
                        // handled above
                    }
                    StreamEvent::MessageStop(_) => {
                        // no-op
                    }
                }
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
