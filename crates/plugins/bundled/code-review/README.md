# code-review Plugin

LLM-powered code review tool that sends a file to Grok 4.20 and returns structured findings.

## Description

The `code-review` plugin integrates with the xAI Grok API to perform automated, multi-agent code review of source files. It analyzes code for quality, security concerns, architectural patterns, and provides actionable improvement recommendations.

## Tool: `code_review`

Reviews a source file with Grok 4.20 multi-agent. Returns quality, security, and improvement notes.

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
      "description": "xAI model override (default: grok-4.20-multi-agent-0309)"
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
| `model` | string | ❌ | Override the default Grok model (e.g., `grok-4.20-multi-agent-0309`) |
| `focus` | string | ❌ | Review focus area: `security`, `quality`, `architecture`, or `all` |

### Example Usage

```bash
rustcode tool code-review --file-path src/main.rs --focus security
rustcode tool code-review --file-path lib/handler.rs --model grok-4.20-multi-agent-0309
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
- `XAI_API_KEY` environment variable (xAI Grok API credentials)
- Read-only file permissions on the target source files

## Requirements

- Rustcode CLI with API integration enabled
- Active xAI (Grok) API access
- Target file must be readable and parseable

## Status

- **Default Enabled**: `true`
- **Permission Level**: read-only
- **Command**: `rustcode`
