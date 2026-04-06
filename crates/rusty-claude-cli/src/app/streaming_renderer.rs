use std::io::Write;

use crate::render::{Spinner, TerminalRenderer};
use api::{ContentBlockDelta, OutputContentBlock, StreamEvent};

/// Shared renderer for streaming events.
///
/// This function consolidates the terminal rendering behavior used by both the
/// CLI app and the streaming collector. It writes textual output and advances
/// spinners based on the incoming `StreamEvent`s, updates the `saw_text` flag
/// when we transition into text output, and updates `turn_usage` when a
/// `MessageDelta` arrives.
///
/// Note: this function intentionally takes `&StreamEvent` to avoid unnecessary
/// clones by callers that already have a borrowed event.
pub fn render_stream_event(
    renderer: &TerminalRenderer,
    event: &StreamEvent,
    stream_spinner: &mut Spinner,
    tool_spinner: &mut Spinner,
    saw_text: &mut bool,
    turn_usage: &mut api::Usage,
    out: &mut impl Write,
) {
    match event {
        StreamEvent::MessageStart(start) => {
            for block in &start.message.content {
                match block {
                    OutputContentBlock::Text { text } => {
                        if !*saw_text {
                            let _ = stream_spinner.finish(
                                "Streaming response",
                                renderer.color_theme(),
                                out,
                            );
                            *saw_text = true;
                        }
                        let _ = write!(out, "{}", text);
                        let _ = out.flush();
                    }
                    OutputContentBlock::ToolUse { name, input, .. } => {
                        if *saw_text {
                            let _ = writeln!(out);
                        }
                        let _ = tool_spinner.tick(
                            &format!("Running tool `{}` with {}", name, input),
                            renderer.color_theme(),
                            out,
                        );
                    }
                    _ => {}
                }
            }
        }

        StreamEvent::ContentBlockStart(start) => match &start.content_block {
            OutputContentBlock::Text { text } => {
                if !text.is_empty() {
                    if !*saw_text {
                        let _ = stream_spinner.finish(
                            "Streaming response",
                            renderer.color_theme(),
                            out,
                        );
                        *saw_text = true;
                    }
                    let _ = write!(out, "{}", text);
                    let _ = out.flush();
                }
            }
            OutputContentBlock::ToolUse { name, input, .. } => {
                if *saw_text {
                    let _ = writeln!(out);
                }
                let _ = tool_spinner.tick(
                    &format!("Running tool `{}` with {}", name, input),
                    renderer.color_theme(),
                    out,
                );
            }
            _ => {}
        },

        StreamEvent::ContentBlockDelta(delta) => match &delta.delta {
            ContentBlockDelta::TextDelta { text } => {
                if !text.is_empty() {
                    if !*saw_text {
                        let _ = stream_spinner.finish(
                            "Streaming response",
                            renderer.color_theme(),
                            out,
                        );
                        *saw_text = true;
                    }
                    let _ = write!(out, "{}", text);
                    let _ = out.flush();
                }
            }
            ContentBlockDelta::InputJsonDelta { partial_json } => {
                let _ = tool_spinner.tick(
                    &format!("Collecting tool input: {}", partial_json),
                    renderer.color_theme(),
                    out,
                );
            }
            ContentBlockDelta::ThinkingDelta { .. } | ContentBlockDelta::SignatureDelta { .. } => {}
        },

        StreamEvent::ContentBlockStop(_) => {
            let _ = tool_spinner.finish("Tool completed", renderer.color_theme(), out);
        }

        StreamEvent::MessageDelta(delta) => {
            // Update usage for the current turn.
            *turn_usage = delta.usage.clone();
        }

        StreamEvent::MessageStop(_) => {
            // End marker - nothing to do here for rendering.
        }
    }
}

