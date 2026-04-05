use std::io::{self, Write};
use std::path::PathBuf;

use crate::args::{OutputFormat, PermissionMode};
use crate::input::{LineEditor, ReadOutcome};
use crate::render::{Spinner, TerminalRenderer};
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
        command: SlashCommand::Clear { confirm: false },
        summary: "Start a fresh local session",
    },
];

pub struct CliApp {
    config: SessionConfig,
    renderer: TerminalRenderer,
    state: SessionState,
    conversation_client: ProviderClient,
    conversation_history: Vec<ConversationMessage>,
    runtime: tokio::runtime::Runtime,
}

impl CliApp {
    pub fn new(config: SessionConfig) -> Result<Self, RuntimeError> {
        let state = SessionState::new(config.model.clone());
        let conversation_client = ProviderClient::from_model(&config.model)
            .map_err(|e| RuntimeError::new(e.to_string()))?;

        // Build a current-thread runtime and store it on the CLI app for reuse across turns.
        let runtime = tokio::runtime::Builder::new_current_thread()
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

    fn handle_stream_event(
        renderer: &TerminalRenderer,
        event: StreamEvent,
        stream_spinner: &mut Spinner,
        tool_spinner: &mut Spinner,
        saw_text: &mut bool,
        turn_usage: &mut Usage,
        out: &mut impl Write,
    ) {
        use api::{ContentBlockDelta, OutputContentBlock};

        match event {
            StreamEvent::MessageStart(start) => {
                // Print any immediate content blocks from the start event.
                for block in start.message.content {
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
            StreamEvent::ContentBlockStart(start) => match start.content_block {
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
            StreamEvent::ContentBlockDelta(delta_event) => match delta_event.delta {
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
                    // Tool input fragments — show minimally in spinner text.
                    let _ = tool_spinner.tick(
                        &format!("Collecting tool input: {}", partial_json),
                        renderer.color_theme(),
                        out,
                    );
                }
                ContentBlockDelta::ThinkingDelta { .. }
                | ContentBlockDelta::SignatureDelta { .. } => {}
            },
            StreamEvent::ContentBlockStop(_) => {
                // Close off any running markdown/tool output; ensure newline for clarity.
                let _ = tool_spinner.finish("Tool completed", renderer.color_theme(), out);
            }
            StreamEvent::MessageDelta(delta) => {
                // Extract usage info if present.
                *turn_usage = delta.usage;
            }
            StreamEvent::MessageStop(_) => {
                // end-of-message marker; nothing special here for now.
            }
        }
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
        let mut stream_spinner = Spinner::new();
        stream_spinner.tick(
            "Opening conversation stream",
            self.renderer.color_theme(),
            out,
        )?;

        let renderer = &self.renderer;

        // Build a streaming MessageRequest and process events as they arrive.
        let message_request = MessageRequest {
            model: self.config.model.clone(),
            max_tokens: 64_000,
            messages: vec![InputMessage::user_text(input)],
            system: None,
            tools: None,
            tool_choice: None,
            stream: true,
        };

        // Prepare local mutable state used during streaming rendering and collection.
        let mut assistant_messages: Vec<ConversationMessage> = Vec::new();
        let mut turn_usage: Usage = Usage::default();
        let mut tool_spinner = Spinner::new();
        let mut saw_text = false;

        // Clone client for use inside the runtime.
        let client = self.conversation_client.clone();

        // Reuse the runtime stored on the CLI instance (created in `new`) so we don't recreate
        // a runtime on every turn.

        // Run the streaming loop and process events as they arrive.
        let stream_err = self.runtime.block_on(async {
            let mut stream = client
                .stream_message(&message_request)
                .await
                .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?;

            while let Some(event) = stream
                .next_event()
                .await
                .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?
            {
                // Render the event synchronously using the existing handler.
                // We pass mutable references to local spinners/flags so the handler can update them.
                Self::handle_stream_event(
                    renderer,
                    event.clone(),
                    &mut stream_spinner,
                    &mut tool_spinner,
                    &mut saw_text,
                    &mut turn_usage,
                    out,
                );

                // Inspect the event to capture assistant text into collected messages.
                match event {
                    StreamEvent::MessageStart(start) => {
                        for block in start.message.content {
                            if let OutputContentBlock::Text { text } = block {
                                assistant_messages.push(ConversationMessage {
                                    role: MessageRole::Assistant,
                                    blocks: vec![ContentBlock::Text { text }],
                                    usage: None,
                                });
                            }
                        }
                    }
                    StreamEvent::ContentBlockStart(start) => {
                        if let OutputContentBlock::Text { text } = start.content_block {
                            assistant_messages.push(ConversationMessage {
                                role: MessageRole::Assistant,
                                blocks: vec![ContentBlock::Text { text }],
                                usage: None,
                            });
                        }
                    }
                    StreamEvent::ContentBlockDelta(delta) => {
                        if let api::ContentBlockDelta::TextDelta { text } = delta.delta {
                            if !text.is_empty() {
                                assistant_messages.push(ConversationMessage {
                                    role: MessageRole::Assistant,
                                    blocks: vec![ContentBlock::Text { text }],
                                    usage: None,
                                });
                            }
                        }
                    }
                    StreamEvent::ContentBlockStop(_) => {
                        // Content block finished — nothing to accumulate for the summary here.
                    }
                    StreamEvent::MessageDelta(delta) => {
                        // Update turn usage when a MessageDelta with usage arrives.
                        turn_usage = delta.usage;
                    }
                    StreamEvent::MessageStop(_) => {
                        // End of message marker; nothing special to do here.
                    }
                }
            }

            Ok::<(), io::Error>(())
        });

        if let Err(e) = stream_err {
            stream_spinner.fail(
                "Streaming response failed",
                self.renderer.color_theme(),
                out,
            )?;
            return Err(e);
        }

        // Update last usage from the usage we observed during streaming.
        self.state.last_usage = turn_usage.clone();

        // Ensure spacing after streamed text.
        if saw_text {
            writeln!(out)?;
        } else {
            stream_spinner.finish("Streaming response", self.renderer.color_theme(), out)?;
        }

        // Build a TurnSummary from the collected assistant messages and the observed usage.
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
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use crate::args::{OutputFormat, PermissionMode};

    use super::{CommandResult, SessionConfig, SlashCommand};

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
}
