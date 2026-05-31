# code-review Plugin

LLM-powered code review tool that sends a file to the configured LLM and returns structured findings.

## Description

The `code-review` plugin dispatches through rustcode's LLM router to perform automated code review of source files. It analyzes code for quality, security concerns, architectural patterns, and provides actionable improvement recommendations. The active provider depends on rustcode's configuration.

## Tool: `code_review`

Reviews a source file with the configured LLM. Returns quality, security, and improvement notes.

### Input Schema

```json
{
  "type": "object",
  "required": ["file_path"],
  "additionalProperties": false,
  "properties": {
    "file_path": {
      "type": "string",
      "description": "Relative or absolute path to the file to review"
    },
    "model": {
      "type": "string",
      "description": "LLM model override; defaults to the runtime's configured review model"
    },
    "focus": {
      "type": "string",
      "description": "Optional review focus: security | quality | architecture | all (default: all)"
    }
  }
}
```

### Parameters

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `file_path` | string | ✅ | Path to the source file to review |
| `model` | string | ❌ | Override the default review model (provider-specific identifier — pick a slug the configured router accepts) |
| `focus` | string | ❌ | Review focus area: `security`, `quality`, `architecture`, or `all` |

### Example Usage

```bash
rustcode tool code-review --file-path src/main.rs --focus security
rustcode tool code-review --file-path lib/handler.rs
rustcode tool code-review --file-path tests/integration.rs
```

## Output

Returns structured findings including:
- Code quality assessment
- Security vulnerabilities and concerns
- Architectural recommendations
- Performance considerations
- Improvement suggestions with priorities

## Configuration

This plugin requires:
- The configured LLM provider's API key in the environment. The current implementation routes through xAI, so `XAI_API_KEY` is required; the impl is slated to migrate to the Anthropic backend, after which `ANTHROPIC_API_KEY` will apply instead.
- Read-only file permissions on the target source files

## Requirements

- Rustcode CLI with API integration enabled
- API credentials for the configured LLM provider
- Target file must be readable and parseable

## Status

- **Default Enabled**: `true`
- **Permission Level**: read-only
- **Command**: `rustcode`
