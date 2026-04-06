# file-summary Plugin

Generate structured LLM summaries for source files using Grok 4.20.

## Overview

The `file-summary` plugin analyzes a single source file and generates a comprehensive, structured summary that includes:

- **Purpose**: What the file does and its role in the codebase
- **Language**: Programming language and framework detection
- **Complexity Score**: Estimated cyclomatic/structural complexity
- **Key Exports**: Public functions, classes, and types

## Usage

### Tool Definition

- **Name**: `file_summary`
- **Command**: `rustcode tool file-summary`
- **Permission**: `read-only`

### Input Schema

| Parameter | Type | Required | Default | Description |
|-----------|------|----------|---------|-------------|
| `file_path` | string | ✓ | — | Path to the file to summarise |
| `max_tokens` | integer | | 1024 | Maximum response tokens (256–8000) |
| `include_code_snippet` | boolean | | false | Include a representative code snippet in output |

### Example Request

```json
{
  "file_path": "src/main.rs",
  "max_tokens": 2048,
  "include_code_snippet": true
}
```

### Example Response

```json
{
  "purpose": "Entry point for the rustcode CLI. Initializes configuration, parses arguments, and dispatches to subcommands.",
  "language": "Rust",
  "complexity_score": 6.2,
  "key_exports": [
    "fn main()",
    "struct Config",
    "enum Commands"
  ],
  "code_snippet": "pub fn main() {\n    // Initialize...\n}"
}
```

## Integration

This plugin is bundled with rustcode and available when the `--server` mode is enabled. It integrates with the Grok xAI API for LLM analysis.

### Prerequisites

- `XAI_API_KEY` environment variable must be set
- Target file must be readable by the rustcode process

### Performance

- Typical response time: 2–5 seconds per file
- Token usage varies by file size and complexity
- Estimated cost per analysis: $0.01–$0.05 USD

## Related Plugins

- [`code-review`](../code-review/README.md) — Detailed per-file code review
- [`todo-scan`](../todo-scan/README.md) — Find TODO/FIXME markers