// Tool backends for the executor phase of `AgentPipeline`.
//
// When the agent has tools attached, the executor's per-step LLM call uses
// Anthropic tool use: Sonnet emits `ToolUse` content blocks, we execute each
// tool, send the results back as `ToolResult` blocks, and repeat until the
// model produces a pure-text turn. Without tools the executor falls back to
// the single-turn text-only flow from PR #3.
//
// `FileSystemTools` is the default backend: it exposes `write_file`,
// `edit_file`, `read_file`, and `run_command` operating inside a sandbox
// rooted at a single directory. All relative paths are resolved against
// that root; absolute paths and `..` traversals return `PathEscape`.

use std::path::{Component, Path, PathBuf};

use async_trait::async_trait;
use ::api::ToolDefinition;
use runtime::{BashCommandInput, execute_bash, shell_quote};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::io::AsyncWriteExt;
use tracing::warn;

/// Errors that a tool implementation can surface to the model. The
/// `Display` text is what the model sees in the corresponding
/// `ToolResultContentBlock`, so messages should be informative without
/// leaking host details.
#[derive(Debug, thiserror::Error)]
pub enum ToolError {
    #[error("unknown tool: {0}")]
    UnknownTool(String),
    #[error("invalid arguments for tool `{tool}`: {message}")]
    InvalidArguments { tool: String, message: String },
    #[error("path escapes sandbox root: {0}")]
    PathEscape(String),
    #[error("io error: {0}")]
    Io(String),
    #[error("command not allowed in this sandbox")]
    CommandDisabled,
    #[error("command `{command}` exited with status {status}\nstderr: {stderr}")]
    CommandFailed {
        command: String,
        status: i32,
        stderr: String,
    },
    #[error("search string not found in {path}")]
    StringNotFound { path: String },
}

impl From<std::io::Error> for ToolError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e.to_string())
    }
}

/// Backend that executes tool calls coming from the executor LLM.
///
/// `tool_definitions` is sent to the model as the `tools` array on
/// `MessageRequest`; `execute` runs the tool call by name and returns the
/// content surfaced back to the model as a `ToolResultContentBlock::Text`.
#[async_trait]
pub trait ToolBackend: Send + Sync {
    fn tool_definitions(&self) -> Vec<ToolDefinition>;
    async fn execute(&self, tool_name: &str, input: Value) -> Result<String, ToolError>;
}

/// Trace record for one tool invocation, persisted to `agent_trace`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallRecord {
    /// Anthropic-issued `id` for this tool_use block. Useful for
    /// correlating with the model's reasoning in debug traces.
    pub tool_use_id: String,
    pub tool_name: String,
    pub input: Value,
    pub status: ToolCallStatus,
    /// Stringified tool result (or error). Truncated at 4 KiB to keep
    /// result files manageable when the output is large (e.g. file dumps).
    pub output: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolCallStatus {
    Success,
    Error,
}

/// File-system tool backend scoped to a working directory.
///
/// All relative paths in tool inputs are resolved against `root`. Absolute
/// paths and any path containing a `..` component return `PathEscape`. By
/// default `run_command` is disabled — call `with_command_execution(true)`
/// to opt in.
pub struct FileSystemTools {
    root: PathBuf,
    allow_command: bool,
    /// Cap on captured `run_command` output (stdout+stderr concatenated).
    /// Anything past this is truncated with an explicit marker.
    output_truncate_bytes: usize,
}

impl FileSystemTools {
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            allow_command: false,
            output_truncate_bytes: 16 * 1024,
        }
    }

    #[must_use]
    pub fn with_command_execution(mut self, allow: bool) -> Self {
        self.allow_command = allow;
        self
    }

    fn resolve_path(&self, raw: &str) -> Result<PathBuf, ToolError> {
        let p = Path::new(raw);
        if p.is_absolute() {
            return Err(ToolError::PathEscape(raw.to_string()));
        }
        if p.components().any(|c| matches!(c, Component::ParentDir)) {
            return Err(ToolError::PathEscape(raw.to_string()));
        }
        Ok(self.root.join(p))
    }

    async fn op_write_file(&self, input: &Value) -> Result<String, ToolError> {
        let path = require_string(input, "write_file", "path")?;
        let content = require_string(input, "write_file", "content")?;
        let target = self.resolve_path(path)?;
        if let Some(parent) = target.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let mut f = tokio::fs::File::create(&target).await?;
        f.write_all(content.as_bytes()).await?;
        f.flush().await?;
        Ok(format!("wrote {} bytes to {}", content.len(), path))
    }

    async fn op_edit_file(&self, input: &Value) -> Result<String, ToolError> {
        let path = require_string(input, "edit_file", "path")?;
        let old_string = require_string(input, "edit_file", "old_string")?;
        let new_string = require_string(input, "edit_file", "new_string")?;
        let target = self.resolve_path(path)?;
        let original = tokio::fs::read_to_string(&target).await?;
        if !original.contains(old_string) {
            return Err(ToolError::StringNotFound {
                path: path.to_string(),
            });
        }
        let updated = original.replacen(old_string, new_string, 1);
        tokio::fs::write(&target, &updated).await?;
        Ok(format!(
            "edited {} ({}→{} bytes)",
            path,
            original.len(),
            updated.len()
        ))
    }

    async fn op_read_file(&self, input: &Value) -> Result<String, ToolError> {
        let path = require_string(input, "read_file", "path")?;
        let target = self.resolve_path(path)?;
        let content = tokio::fs::read_to_string(&target).await?;
        Ok(self.truncate(content))
    }

    async fn op_run_command(&self, input: &Value) -> Result<String, ToolError> {
        if !self.allow_command {
            return Err(ToolError::CommandDisabled);
        }
        let command = require_string(input, "run_command", "command")?;
        let args = input
            .get("args")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(ToOwned::to_owned))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        // Build a `GIT_TERMINAL_PROMPT=0 <cmd> <arg> <arg>` shell command
        // with each token shell-quoted. The agent-supplied `command` and
        // args could contain anything, so quoting is mandatory.
        let shell_command = {
            let mut s = String::from("GIT_TERMINAL_PROMPT=0 ");
            s.push_str(&shell_quote(command));
            for arg in &args {
                s.push(' ');
                s.push_str(&shell_quote(arg));
            }
            s
        };

        // `execute_bash` is sync (builds its own current-thread tokio
        // runtime internally), so route through `spawn_blocking` to
        // avoid nesting runtimes. The previous code used proper
        // `tokio::process::Command` async I/O; `spawn_blocking` runs
        // on a dedicated blocking thread which is functionally
        // equivalent for the agent's run-command use case.
        let cwd = self.root.clone();
        let output = tokio::task::spawn_blocking(move || {
            execute_bash(BashCommandInput {
                command: shell_command,
                timeout: None,
                description: None,
                run_in_background: Some(false),
                dangerously_disable_sandbox: Some(true),
                namespace_restrictions: None,
                isolate_network: None,
                filesystem_mode: None,
                allowed_mounts: None,
                cwd: Some(cwd),
            })
        })
        .await
        .map_err(|e| std::io::Error::other(format!("blocking task panicked: {e}")))??;

        if output.return_code_interpretation.is_some() {
            // `return_code_interpretation` is `Some("exit_code:<n>")`
            // on non-zero exits; parse the integer back so the
            // ToolError surface stays unchanged for callers.
            let status_code = output
                .return_code_interpretation
                .as_deref()
                .and_then(|s| s.strip_prefix("exit_code:"))
                .and_then(|s| s.parse::<i32>().ok())
                .unwrap_or(-1);
            return Err(ToolError::CommandFailed {
                command: command.to_string(),
                status: status_code,
                stderr: self.truncate(output.stderr),
            });
        }

        Ok(self.truncate(format!("{}{}", output.stdout, output.stderr)))
    }

    fn truncate(&self, mut s: String) -> String {
        if s.len() <= self.output_truncate_bytes {
            return s;
        }
        s.truncate(self.output_truncate_bytes);
        s.push_str("\n…[truncated]");
        s
    }
}

fn require_string<'a>(input: &'a Value, tool: &str, key: &str) -> Result<&'a str, ToolError> {
    input
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| ToolError::InvalidArguments {
            tool: tool.to_string(),
            message: format!("missing or non-string `{}`", key),
        })
}

#[async_trait]
impl ToolBackend for FileSystemTools {
    fn tool_definitions(&self) -> Vec<ToolDefinition> {
        let mut defs = vec![
            ToolDefinition {
                name: "write_file".to_string(),
                description: Some(
                    "Create or overwrite a file with the given content. Parent directories \
                     are created automatically. Paths are relative to the workspace root \
                     and may not contain `..` components."
                        .to_string(),
                ),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Relative file path." },
                        "content": { "type": "string", "description": "File content." }
                    },
                    "required": ["path", "content"]
                }),
            },
            ToolDefinition {
                name: "edit_file".to_string(),
                description: Some(
                    "Replace the FIRST occurrence of `old_string` in a file with \
                     `new_string`. Errors if `old_string` is not found. Use a long \
                     enough `old_string` to make the match unique."
                        .to_string(),
                ),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string" },
                        "old_string": { "type": "string" },
                        "new_string": { "type": "string" }
                    },
                    "required": ["path", "old_string", "new_string"]
                }),
            },
            ToolDefinition {
                name: "read_file".to_string(),
                description: Some(
                    "Read the contents of a file. Output is truncated past 16 KiB."
                        .to_string(),
                ),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string" }
                    },
                    "required": ["path"]
                }),
            },
        ];
        if self.allow_command {
            defs.push(ToolDefinition {
                name: "run_command".to_string(),
                description: Some(
                    "Run a command in the workspace root. `command` is the program; \
                     `args` is the argument list. Stdout and stderr are concatenated \
                     and truncated past 16 KiB. Non-zero exit codes are reported as \
                     errors."
                        .to_string(),
                ),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "command": { "type": "string" },
                        "args": {
                            "type": "array",
                            "items": { "type": "string" }
                        }
                    },
                    "required": ["command"]
                }),
            });
        }
        defs
    }

    async fn execute(&self, tool_name: &str, input: Value) -> Result<String, ToolError> {
        match tool_name {
            "write_file" => self.op_write_file(&input).await,
            "edit_file" => self.op_edit_file(&input).await,
            "read_file" => self.op_read_file(&input).await,
            "run_command" => self.op_run_command(&input).await,
            other => {
                warn!(tool = other, "Executor requested unknown tool");
                Err(ToolError::UnknownTool(other.to_string()))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn setup() -> (TempDir, FileSystemTools) {
        let dir = TempDir::new().expect("tempdir");
        let tools = FileSystemTools::new(dir.path().to_path_buf());
        (dir, tools)
    }

    #[tokio::test]
    async fn write_file_creates_nested_directories() {
        let (dir, tools) = setup();
        let input = serde_json::json!({
            "path": "src/lib/foo.rs",
            "content": "fn main() {}\n",
        });
        let out = tools.execute("write_file", input).await.expect("write");
        assert!(out.contains("wrote"));
        let written = tokio::fs::read_to_string(dir.path().join("src/lib/foo.rs"))
            .await
            .expect("read back");
        assert_eq!(written, "fn main() {}\n");
    }

    #[tokio::test]
    async fn edit_file_replaces_first_match() {
        let (dir, tools) = setup();
        let path = dir.path().join("a.txt");
        tokio::fs::write(&path, "foo bar foo").await.unwrap();
        let input = serde_json::json!({
            "path": "a.txt",
            "old_string": "foo",
            "new_string": "baz",
        });
        tools.execute("edit_file", input).await.expect("edit");
        let result = tokio::fs::read_to_string(&path).await.unwrap();
        assert_eq!(result, "baz bar foo");
    }

    #[tokio::test]
    async fn edit_file_errors_when_string_missing() {
        let (dir, tools) = setup();
        tokio::fs::write(dir.path().join("a.txt"), "hello")
            .await
            .unwrap();
        let input = serde_json::json!({
            "path": "a.txt",
            "old_string": "world",
            "new_string": "x",
        });
        let err = tools.execute("edit_file", input).await.unwrap_err();
        assert!(matches!(err, ToolError::StringNotFound { .. }));
    }

    #[tokio::test]
    async fn absolute_paths_rejected() {
        let (_dir, tools) = setup();
        let input = serde_json::json!({
            "path": "/etc/passwd",
            "content": "x",
        });
        let err = tools.execute("write_file", input).await.unwrap_err();
        assert!(matches!(err, ToolError::PathEscape(_)));
    }

    #[tokio::test]
    async fn parent_traversal_rejected() {
        let (_dir, tools) = setup();
        let input = serde_json::json!({
            "path": "../escape.txt",
            "content": "x",
        });
        let err = tools.execute("write_file", input).await.unwrap_err();
        assert!(matches!(err, ToolError::PathEscape(_)));
    }

    #[tokio::test]
    async fn run_command_disabled_by_default() {
        let (_dir, tools) = setup();
        let input = serde_json::json!({ "command": "echo", "args": ["hi"] });
        let err = tools.execute("run_command", input).await.unwrap_err();
        assert!(matches!(err, ToolError::CommandDisabled));
    }

    #[tokio::test]
    async fn run_command_when_enabled_returns_stdout() {
        let (dir, _tools) = setup();
        let tools = FileSystemTools::new(dir.path().to_path_buf()).with_command_execution(true);
        let input = serde_json::json!({ "command": "echo", "args": ["hello"] });
        let out = tools.execute("run_command", input).await.expect("echo");
        assert!(out.contains("hello"));
    }

    #[tokio::test]
    async fn unknown_tool_returns_error() {
        let (_dir, tools) = setup();
        let err = tools
            .execute("nonexistent", serde_json::json!({}))
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::UnknownTool(_)));
    }

    #[test]
    fn tool_definitions_omit_run_command_when_disabled() {
        let dir = TempDir::new().unwrap();
        let tools = FileSystemTools::new(dir.path().to_path_buf());
        let names: Vec<_> = tools.tool_definitions().into_iter().map(|d| d.name).collect();
        assert!(names.contains(&"write_file".to_string()));
        assert!(!names.contains(&"run_command".to_string()));
    }

    #[test]
    fn tool_definitions_include_run_command_when_enabled() {
        let dir = TempDir::new().unwrap();
        let tools = FileSystemTools::new(dir.path().to_path_buf()).with_command_execution(true);
        let names: Vec<_> = tools.tool_definitions().into_iter().map(|d| d.name).collect();
        assert!(names.contains(&"run_command".to_string()));
    }
}
