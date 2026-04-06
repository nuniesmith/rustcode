use std::io::{self, Write};
use std::path::PathBuf;

use crate::args::{OutputFormat, PermissionMode};
use crate::input::{LineEditor, ReadOutcome};
use crate::render::{Spinner, TerminalRenderer};
mod streaming;
use api::{InputMessage, MessageRequest, OutputContentBlock, ProviderClient, StreamEvent, Usage};
use runtime::{ContentBlock, ConversationMessage, MessageRole, RuntimeError};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionConfig {
    pub model: String,
    pub permission_mode: PermissionMode,
    pub config: Option<PathBuf>,
    pub output_format: OutputFormat,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionState {
    pub turns: usize,
    pub compacted_messages: usize,
    pub last_model: String,
    pub last_usage: Usage,
}

impl SessionState {
    #[must_use]
    pub fn new(model: impl Into<String>) -> Self {
        Self {
            turns: 0,
            compacted_messages: 0,
            last_model: model.into(),
            last_usage: Usage::default(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandResult {
    Continue,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SlashCommand {
    Help,
    Status,
    Compact,
    Model { model: Option<String> },
    Permissions { mode: Option<String> },
    Config { section: Option<String> },
    Memory,
    Cancel,
    Clear { confirm: bool },
    Unknown(String),
}

impl SlashCommand {
    #[must_use]
    pub fn parse(input: &str) -> Option<Self> {
        let trimmed = input.trim();
        if !trimmed.starts_with('/') {
            return None;
        }

        let mut parts = trimmed.trim_start_matches('/').split_whitespace();
        let command = parts.next().unwrap_or_default();
        Some(match command {
            "help" => Self::Help,
            "status" => Self::Status,
            "compact" => Self::Compact,
            "model" => Self::Model {
                model: parts.next().map(ToOwned::to_owned),
            },
            "permissions" => Self::Permissions {
                mode: parts.next().map(ToOwned::to_owned),
            },
            "config" => Self::Config {
                section: parts.next().map(ToOwned::to_owned),
            },
            "memory" => Self::Memory,
            "cancel" => Self::Cancel,
            "clear" => Self::Clear {
                confirm: parts.next() == Some("--confirm"),
            },
            other => Self::Unknown(other.to_string()),
        })
    }
}

struct SlashCommandHandler {
    command: SlashCommand,
    summary: &'static str,
}

const SLASH_COMMAND_HANDLERS: &[SlashCommandHandler] = &[
    SlashCommandHandler {
        command: SlashCommand::Help,
        summary: "Show command help",
    },
    SlashCommandHandler {
        command: SlashCommand::Status,
        summary: "Show current session status",
    },
    SlashCommandHandler {
        command: SlashCommand::Compact,
        summary: "Compact local session history",
    },
    SlashCommandHandler {
        command: SlashCommand::Model { model: None },
        summary: "Show or switch the active model",
    },
    SlashCommandHandler {
        command: SlashCommand::Permissions { mode: None },
        summary: "Show or switch the active permission mode",
    },
    SlashCommandHandler {
        command: SlashCommand::Config { section: None },
        summary: "Inspect current config path or section",
    },
    SlashCommandHandler {
        command: SlashCommand::Memory,
        summary: "Inspect loaded memory/instruction files",
    },
    SlashCommandHandler {
        command: SlashCommand::Cancel,
        summary: "Cancel the currently-streaming response",
    },
    SlashCommandHandler {
        command: SlashCommand::Clear { confirm: false },
        summary: "Start a fresh local session",
    },
];

/// Helper: accumulate a single StreamEvent into the running list of assistant messages.
///
/// This mirrors the accumulation logic used by the streaming loop so it can be exercised in unit tests.
/// Wrapper around the streaming module's accumulate_stream_event to maintain the old API for tests.
fn accumulate_stream_event(collected: &mut Vec<ConversationMessage>, event: &api::StreamEvent) {
    // This test helper doesn't track usage - it's just for message accumulation testing.
    // In real streaming, usage is tracked separately.
    let mut dummy_usage = api::Usage::default();
    streaming::accumulate_stream_event(collected, &mut dummy_usage, event);
}

pub struct CliApp {
    config: SessionConfig,
    renderer: TerminalRenderer,
    state: SessionState,
    conversation_client: ProviderClient,
    conversation_history: Vec<ConversationMessage>,
    runtime: tokio::runtime::Runtime,
    current_stream_cancel: Option<tokio::sync::oneshot::Sender<()>>,
}

impl CliApp {
    pub fn new(config: SessionConfig) -> Result<Self, RuntimeError> {
        let state = SessionState::new(config.model.clone());
        let conversation_client = ProviderClient::from_model(&config.model)
            .map_err(|e| RuntimeError::new(e.to_string()))?;

        // Build a multi-thread runtime and store it on the CLI app for reuse across turns.
        // Use a small fixed worker thread count; adjust as appropriate for your environment.
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(4)
            .enable_all()
            .build()
            .map_err(|e| RuntimeError::new(e.to_string()))?;
        Ok(Self {
            config,
            renderer: TerminalRenderer::new(),
            state,
            conversation_client,
            conversation_history: Vec::new(),
            runtime,
            current_stream_cancel: None,
        })
    }

    pub fn run_repl(&mut self) -> io::Result<()> {
        let mut editor = LineEditor::new("› ", Vec::new());
        println!("Rusty Claude CLI interactive mode");
        println!("Type /help for commands. Shift+Enter or Ctrl+J inserts a newline.");

        loop {
            match editor.read_line()? {
                ReadOutcome::Submit(input) => {
                    if input.trim().is_empty() {
                        continue;
                    }
                    self.handle_submission(&input, &mut io::stdout())?;
                }
                ReadOutcome::Cancel => continue,
                ReadOutcome::Exit => break,
            }
        }

        Ok(())
    }

    pub fn run_prompt(&mut self, prompt: &str, out: &mut impl Write) -> io::Result<()> {
        self.render_response(prompt, out)
    }

    pub fn handle_submission(
        &mut self,
        input: &str,
        out: &mut impl Write,
    ) -> io::Result<CommandResult> {
        if let Some(command) = SlashCommand::parse(input) {
            return self.dispatch_slash_command(command, out);
        }

        self.state.turns += 1;
        self.render_response(input, out)?;
        Ok(CommandResult::Continue)
    }

    fn dispatch_slash_command(
        &mut self,
        command: SlashCommand,
        out: &mut impl Write,
    ) -> io::Result<CommandResult> {
        match command {
            SlashCommand::Help => Self::handle_help(out),
            SlashCommand::Status => self.handle_status(out),
            SlashCommand::Compact => self.handle_compact(out),
            SlashCommand::Model { model } => self.handle_model(model.as_deref(), out),
            SlashCommand::Permissions { mode } => self.handle_permissions(mode.as_deref(), out),
            SlashCommand::Config { section } => self.handle_config(section.as_deref(), out),
            SlashCommand::Memory => self.handle_memory(out),
            SlashCommand::Cancel => self.handle_cancel(out),
            SlashCommand::Clear { confirm } => self.handle_clear(confirm, out),
            SlashCommand::Unknown(name) => {
                writeln!(out, "Unknown slash command: /{name}")?;
                Ok(CommandResult::Continue)
            }
        }
    }

    fn handle_help(out: &mut impl Write) -> io::Result<CommandResult> {
        writeln!(out, "Available commands:")?;
        for handler in SLASH_COMMAND_HANDLERS {
            let name = match handler.command {
                SlashCommand::Help => "/help",
                SlashCommand::Status => "/status",
                SlashCommand::Compact => "/compact",
                SlashCommand::Model { .. } => "/model [model]",
                SlashCommand::Permissions { .. } => "/permissions [mode]",
                SlashCommand::Config { .. } => "/config [section]",
                SlashCommand::Memory => "/memory",
                SlashCommand::Cancel => "/cancel",
                SlashCommand::Clear { .. } => "/clear [--confirm]",
                SlashCommand::Unknown(_) => continue,
            };
            writeln!(out, "  {name:<9} {}", handler.summary)?;
        }
        Ok(CommandResult::Continue)
    }

    fn handle_status(&mut self, out: &mut impl Write) -> io::Result<CommandResult> {
        writeln!(
            out,
            "status: turns={} model={} permission-mode={:?} output-format={:?} last-usage={} in/{} out config={}",
            self.state.turns,
            self.state.last_model,
            self.config.permission_mode,
            self.config.output_format,
            self.state.last_usage.input_tokens,
            self.state.last_usage.output_tokens,
            self.config
                .config
                .as_ref()
                .map_or_else(|| String::from("<none>"), |path| path.display().to_string())
        )?;
        Ok(CommandResult::Continue)
    }

    fn handle_compact(&mut self, out: &mut impl Write) -> io::Result<CommandResult> {
        self.state.compacted_messages += self.state.turns;
        self.state.turns = 0;
        self.conversation_history.clear();
        writeln!(
            out,
            "Compacted session history into a local summary ({} messages total compacted).",
            self.state.compacted_messages
        )?;
        Ok(CommandResult::Continue)
    }

    fn handle_model(
        &mut self,
        model: Option<&str>,
        out: &mut impl Write,
    ) -> io::Result<CommandResult> {
        match model {
            Some(model) => {
                self.config.model = model.to_string();
                self.state.last_model = model.to_string();
                writeln!(out, "Active model set to {model}")?;
            }
            None => {
                writeln!(out, "Active model: {}", self.config.model)?;
            }
        }
        Ok(CommandResult::Continue)
    }

    fn handle_permissions(
        &mut self,
        mode: Option<&str>,
        out: &mut impl Write,
    ) -> io::Result<CommandResult> {
        match mode {
            None => writeln!(out, "Permission mode: {:?}", self.config.permission_mode)?,
            Some("read-only") => {
                self.config.permission_mode = PermissionMode::ReadOnly;
                writeln!(out, "Permission mode set to read-only")?;
            }
            Some("workspace-write") => {
                self.config.permission_mode = PermissionMode::WorkspaceWrite;
                writeln!(out, "Permission mode set to workspace-write")?;
            }
            Some("danger-full-access") => {
                self.config.permission_mode = PermissionMode::DangerFullAccess;
                writeln!(out, "Permission mode set to danger-full-access")?;
            }
            Some(other) => {
                writeln!(out, "Unknown permission mode: {other}")?;
            }
        }
        Ok(CommandResult::Continue)
    }

    fn handle_config(
        &mut self,
        section: Option<&str>,
        out: &mut impl Write,
    ) -> io::Result<CommandResult> {
        match section {
            None => writeln!(
                out,
                "Config path: {}",
                self.config
                    .config
                    .as_ref()
                    .map_or_else(|| String::from("<none>"), |path| path.display().to_string())
            )?,
            Some(section) => writeln!(
                out,
                "Config section `{section}` is not fully implemented yet; current config path is {}",
                self.config
                    .config
                    .as_ref()
                    .map_or_else(|| String::from("<none>"), |path| path.display().to_string())
            )?,
        }
        Ok(CommandResult::Continue)
    }

    fn handle_memory(&mut self, out: &mut impl Write) -> io::Result<CommandResult> {
        writeln!(
            out,
            "Loaded memory/config file: {}",
            self.config
                .config
                .as_ref()
                .map_or_else(|| String::from("<none>"), |path| path.display().to_string())
        )?;
        Ok(CommandResult::Continue)
    }

    fn handle_clear(&mut self, confirm: bool, out: &mut impl Write) -> io::Result<CommandResult> {
        if !confirm {
            writeln!(
                out,
                "Refusing to clear without confirmation. Re-run as /clear --confirm"
            )?;
            return Ok(CommandResult::Continue);
        }

        self.state.turns = 0;
        self.state.compacted_messages = 0;
        self.state.last_usage = Usage::default();
        self.conversation_history.clear();
        writeln!(out, "Started a fresh local session.")?;
        Ok(CommandResult::Continue)
    }

    fn handle_cancel(&mut self, out: &mut impl Write) -> io::Result<CommandResult> {
        if let Some(cancel_tx) = self.current_stream_cancel.take() {
            // Best-effort send cancellation. If the receiver is gone, ignore.
            let _ = cancel_tx.send(());
            writeln!(out, "Cancelled current streaming response.")?;
        } else {
            writeln!(out, "No streaming response in progress.")?;
        }
        Ok(CommandResult::Continue)
    }

    fn handle_stream_event(
        renderer: &TerminalRenderer,
        event: StreamEvent,
        stream_spinner: &mut Spinner,
        tool_spinner: &mut Spinner,
        saw_text: &mut bool,
        turn_usage: &mut Usage,
        out: &mut impl Write,
    ) {
        // Delegate rendering to the shared streaming renderer to avoid duplicated logic.
        // The rendering helper updates spinners, prints text/tool notices, and updates usage.
        streaming::render_stream_event(
            renderer,
            &event,
            stream_spinner,
            tool_spinner,
            saw_text,
            turn_usage,
            out,
        );
    }

    fn write_turn_output(
        &self,
        summary: &runtime::TurnSummary,
        out: &mut impl Write,
    ) -> io::Result<()> {
        // Convert assistant messages into plain text for output/serialization
        let assistant_text = summary
            .assistant_messages
            .iter()
            .map(|msg| {
                msg.blocks
                    .iter()
                    .map(|b| match b {
                        ContentBlock::Text { text } => text.clone(),
                        ContentBlock::ToolUse { name, input, .. } => {
                            format!("[tool {} called with {}]", name, input)
                        }
                        ContentBlock::ToolResult { output, .. } => output.clone(),
                    })
                    .collect::<Vec<_>>()
                    .join("")
            })
            .collect::<Vec<_>>()
            .join("\n---\n");

        match self.config.output_format {
            OutputFormat::Text => {
                writeln!(
                    out,
                    "\nToken usage: {} input / {} output",
                    self.state.last_usage.input_tokens, self.state.last_usage.output_tokens
                )?;
            }
            OutputFormat::Json => {
                writeln!(
                    out,
                    "{}",
                    serde_json::json!({
                        "message": assistant_text,
                        "usage": {
                            "input_tokens": self.state.last_usage.input_tokens,
                            "output_tokens": self.state.last_usage.output_tokens,
                        }
                    })
                )?;
            }
            OutputFormat::Ndjson => {
                writeln!(
                    out,
                    "{}",
                    serde_json::json!({
                        "type": "message",
                        "text": assistant_text,
                        "usage": {
                            "input_tokens": self.state.last_usage.input_tokens,
                            "output_tokens": self.state.last_usage.output_tokens,
                        }
                    })
                )?;
            }
        }
        Ok(())
    }

    fn render_response(&mut self, input: &str, out: &mut impl Write) -> io::Result<()> {
        // Build the request and start the streaming task.
        // Clone the model string to avoid borrowing `self` immutably while we need a mutable borrow
        // later (start_stream takes &mut self). Using a local owned `model` prevents overlapping borrows.
        let model = self.config.model.clone();
        let req = Self::make_stream_request(&model, input);
        let rx = self.start_stream(req);

        // Drain events synchronously on the current thread and render them.
        // Call the free function `collect_stream_events` directly to avoid borrowing `self`
        // for the drain operation (previously went through an &mut self wrapper which caused
        // borrow checker conflicts in some code paths).
        let (assistant_messages, turn_usage, saw_text): (
            Vec<ConversationMessage>,
            api::Usage,
            bool,
        ) = streaming::collect_stream_events(rx, out, &self.renderer)?;

        // Update last usage and build TurnSummary
        self.state.last_usage = turn_usage.clone();

        let summary = runtime::TurnSummary {
            assistant_messages,
            tool_results: Vec::new(),
            prompt_cache_events: Vec::new(),
            iterations: 1,
            usage: turn_usage.token_usage(),
            auto_compaction: None,
        };

        self.write_turn_output(&summary, out)?;
        Ok(())
    }

    // Helper: start streaming for a given MessageRequest. This sets up channels,
    // stores the cancel sender, spawns the async task, and returns the receiver
    // used to synchronously drain events on the current thread.
    fn start_stream(
        &mut self,
        req: MessageRequest,
    ) -> std::sync::mpsc::Receiver<Result<StreamEvent, String>> {
        let (tx, rx) = std::sync::mpsc::channel::<Result<StreamEvent, String>>();
        let (cancel_tx, cancel_rx) = tokio::sync::oneshot::channel::<()>();
        self.current_stream_cancel = Some(cancel_tx);

        // Spawn background task to pull provider events and send them to the receiver
        self.spawn_stream_task(req, tx, cancel_rx);

        rx
    }

    // Testable associated helper: construct a MessageRequest from a model name and input.
    fn make_stream_request(model: &str, input: &str) -> MessageRequest {
        MessageRequest {
            model: model.to_string(),
            max_tokens: 64_000,
            messages: vec![InputMessage::user_text(input)],
            system: None,
            tools: None,
            tool_choice: None,
            stream: true,
        }
    }

    // Helper: build a streaming MessageRequest for the given input using this app's model.
    fn build_stream_request(&self, input: &str) -> MessageRequest {
        Self::make_stream_request(&self.config.model, input)
    }

    // Helper: spawn an async task on the stored runtime to fetch stream events and forward
    // them to the provided std mpsc sender. The cancel receiver is consumed inside the task.
    fn spawn_stream_task(
        &self,
        req: MessageRequest,
        sender: std::sync::mpsc::Sender<Result<StreamEvent, String>>,
        mut cancel_rx: tokio::sync::oneshot::Receiver<()>,
    ) {
        let client = self.conversation_client.clone();
        self.runtime.spawn(async move {
            match client.stream_message(&req).await {
                Ok(mut stream) => loop {
                    tokio::select! {
                        biased;
                        next_res = stream.next_event() => {
                            match next_res {
                                Ok(Some(event)) => {
                                    let _ = sender.send(Ok(event));
                                }
                                Ok(None) => break,
                                Err(e) => {
                                    let _ = sender.send(Err(e.to_string()));
                                    break;
                                }
                            }
                        }
                        _ = &mut cancel_rx => {
                            let _ = sender.send(Err("cancelled".to_string()));
                            break;
                        }
                    }
                },
                Err(e) => {
                    let _ = sender.send(Err(e.to_string()));
                }
            }
        });
    }

    // Helper: synchronously drain events from the given receiver, render them using the
    // existing handler, and accumulate assistant messages and usage for a TurnSummary.
    // This method is now a thin wrapper that delegates the heavy lifting to the free
    // function `collect_stream_events` so the core logic can be tested in isolation.
    fn drain_stream_events(
        &mut self,
        rx: std::sync::mpsc::Receiver<Result<StreamEvent, String>>,
        out: &mut impl Write,
        renderer: &TerminalRenderer,
    ) -> io::Result<(Vec<ConversationMessage>, Usage, bool)> {
        streaming::collect_stream_events(rx, out, renderer)
    }
}

// Free function extracted from the former `drain_stream_events` implementation so it can
// be tested in isolation without constructing a full `CliApp`.
fn collect_stream_events(
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
                // Render the event
                CliApp::handle_stream_event(
                    renderer,
                    event.clone(),
                    &mut stream_spinner,
                    &mut tool_spinner,
                    &mut saw_text,
                    &mut turn_usage,
                    out,
                );

                // Accumulate assistant content for summary
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
                    StreamEvent::MessageDelta(delta) => {
                        turn_usage = delta.usage;
                    }
                    StreamEvent::MessageStop(_) => {
                        // end marker
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

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use crate::args::{OutputFormat, PermissionMode};

    use super::{CommandResult, SessionConfig, SlashCommand, accumulate_stream_event};
    use api::{
        ContentBlockDelta, ContentBlockDeltaEvent, ContentBlockStartEvent, OutputContentBlock,
    };
    use runtime::{ContentBlock, ConversationMessage, MessageRole};

    #[test]
    fn parses_required_slash_commands() {
        assert_eq!(SlashCommand::parse("/help"), Some(SlashCommand::Help));
        assert_eq!(SlashCommand::parse(" /status "), Some(SlashCommand::Status));
        assert_eq!(
            SlashCommand::parse("/compact now"),
            Some(SlashCommand::Compact)
        );
        assert_eq!(
            SlashCommand::parse("/model claude-sonnet"),
            Some(SlashCommand::Model {
                model: Some("claude-sonnet".into()),
            })
        );
        assert_eq!(
            SlashCommand::parse("/permissions workspace-write"),
            Some(SlashCommand::Permissions {
                mode: Some("workspace-write".into()),
            })
        );
        assert_eq!(
            SlashCommand::parse("/config hooks"),
            Some(SlashCommand::Config {
                section: Some("hooks".into()),
            })
        );
        assert_eq!(SlashCommand::parse("/memory"), Some(SlashCommand::Memory));
        assert_eq!(
            SlashCommand::parse("/clear --confirm"),
            Some(SlashCommand::Clear { confirm: true })
        );
    }

    #[test]
    fn help_output_lists_commands() {
        let mut out = Vec::new();
        let result = super::CliApp::handle_help(&mut out).expect("help succeeds");
        assert_eq!(result, CommandResult::Continue);
        let output = String::from_utf8_lossy(&out);
        assert!(output.contains("/help"));
        assert!(output.contains("/status"));
        assert!(output.contains("/compact"));
        assert!(output.contains("/model [model]"));
        assert!(output.contains("/permissions [mode]"));
        assert!(output.contains("/config [section]"));
        assert!(output.contains("/memory"));
        assert!(output.contains("/clear [--confirm]"));
    }

    #[test]
    fn session_state_tracks_config_values() {
        let config = SessionConfig {
            model: "claude".into(),
            permission_mode: PermissionMode::DangerFullAccess,
            config: Some(PathBuf::from("settings.toml")),
            output_format: OutputFormat::Text,
        };

        assert_eq!(config.model, "claude");
        assert_eq!(config.permission_mode, PermissionMode::DangerFullAccess);
        assert_eq!(config.config, Some(PathBuf::from("settings.toml")));
    }

    #[test]
    fn accumulation_merges_text_blocks_and_tools() {
        // Simulate a stream with a start text, a delta, and a tool use block.
        let mut collected: Vec<ConversationMessage> = Vec::new();

        // MessageStart with a text block "Hello"
        let start = api::MessageStartEvent {
            message: api::MessageResponse {
                id: "m1".to_string(),
                kind: "message".to_string(),
                role: "assistant".to_string(),
                content: vec![OutputContentBlock::Text {
                    text: "Hello".to_string(),
                }],
                model: "model".to_string(),
                stop_reason: None,
                stop_sequence: None,
                usage: api::Usage::default(),
                request_id: None,
            },
        };
        let ev_start = api::StreamEvent::MessageStart(start);
        accumulate_stream_event(&mut collected, &ev_start);

        // ContentBlockDelta with text delta " world"
        let delta = api::ContentBlockDeltaEvent {
            index: 0,
            delta: ContentBlockDelta::TextDelta {
                text: " world".to_string(),
            },
        };
        let ev_delta = api::StreamEvent::ContentBlockDelta(delta);
        accumulate_stream_event(&mut collected, &ev_delta);

        // ContentBlockStart with a tool use
        let cbstart = ContentBlockStartEvent {
            index: 1,
            content_block: OutputContentBlock::ToolUse {
                id: "t1".to_string(),
                name: "echo".to_string(),
                input: serde_json::json!({"msg":"ok"}),
            },
        };
        let ev_tool = api::StreamEvent::ContentBlockStart(cbstart);
        accumulate_stream_event(&mut collected, &ev_tool);

        // Expect 2 messages: first with merged text "Hello world", second with tool use block
        assert_eq!(collected.len(), 2);
        assert_eq!(collected[0].role, MessageRole::Assistant);
        assert_eq!(
            collected[0].blocks,
            vec![ContentBlock::Text {
                text: "Hello world".to_string()
            }]
        );
        // Second should contain a ToolUse block
        match &collected[1].blocks[0] {
            ContentBlock::ToolUse { id, name, input } => {
                assert_eq!(id, "t1");
                assert_eq!(name, "echo");
            }
            other => panic!("expected ToolUse block, got {:?}", other),
        }
    }

    #[test]
    fn cancel_signal_stops_task() {
        // Verify that a spawned task listening on a oneshot is notified when the sender sends.
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        rt.block_on(async {
            let (tx, rx) = tokio::sync::oneshot::channel::<()>();
            let handle = tokio::spawn(async move {
                tokio::select! {
                    _ = rx => { true }
                    _ = tokio::time::sleep(std::time::Duration::from_secs(5)) => { false }
                }
            });
            // send cancel
            let _ = tx.send(());
            let res = handle.await.expect("task join");
            assert!(res, "task should have been cancelled via oneshot");
        });
    }

    #[test]
    fn make_stream_request_sets_fields() {
        // Test the associated helper constructs a request with expected defaults.
        let req = super::CliApp::make_stream_request("claude-test", "hello");
        assert_eq!(req.model, "claude-test");
        assert_eq!(req.max_tokens, 64_000);
        assert!(req.stream);
        assert_eq!(req.messages.len(), 1);
    }

    #[test]
    fn collect_stream_events_returns_error_on_stream_failure() {
        // Create a channel and send an error result (simulates provider failure).
        let (tx, rx) = std::sync::mpsc::channel::<Result<api::StreamEvent, String>>();

        // Send an error through the stream and close the sender so the receiver will exit.
        let _ = tx.send(Err("provider failed".to_string()));
        drop(tx);

        let mut out = Vec::new();
        let renderer = super::TerminalRenderer::new();

        let res = super::streaming::collect_stream_events(rx, &mut out, &renderer);
        assert!(
            res.is_err(),
            "expected an error when stream reports failure"
        );

        let output = String::from_utf8_lossy(&out);
        // Spinner.fail should have written a failure line; check for the failure message.
        assert!(
            output.contains("Streaming response failed"),
            "expected spinner failure output; got: {}",
            output
        );
    }

    #[test]
    fn collect_stream_events_finishes_spinner_when_no_text_seen() {
        // If the sender is closed without sending any events, collect_stream_events should
        // finish the spinner (not the text path) and return an empty assistant_messages.
        let (tx, rx) = std::sync::mpsc::channel::<Result<api::StreamEvent, String>>();
        drop(tx); // close immediately

        let mut out = Vec::new();
        let renderer = super::TerminalRenderer::new();

        let (messages, usage, saw_text): (Vec<ConversationMessage>, api::Usage, bool) =
            super::streaming::collect_stream_events(rx, &mut out, &renderer)
                .expect("collect events");

        assert!(
            messages.is_empty(),
            "no messages expected when no events were sent"
        );
        assert!(
            !saw_text,
            "saw_text should be false when no text events were received"
        );
        // Spinner.finish prints a success line; ensure output contains the summary label.
        let output = String::from_utf8_lossy(&out);
        assert!(
            output.contains("Streaming response"),
            "expected spinner finish output; got: {}",
            output
        );
    }
}
