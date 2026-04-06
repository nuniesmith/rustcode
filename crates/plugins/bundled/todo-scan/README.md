# todo-scan Plugin

Scans a repository for TODO/FIXME/HACK markers and produces a prioritised task list.

## Description

The `todo-scan` plugin uses runtime glob search to find TODO, FIXME, and HACK markers across a repository. It then ranks them by priority using the existing TodoScanner logic, providing a structured JSON output of actionable tasks.

## Tool: `todo_scan`

Scans the current workspace for TODO markers. Returns a prioritised JSON list.

### Input Schema

```json
{
  "type": "object",
  "required": [],
  "additionalProperties": false,
  "properties": {
    "path": {
      "type": "string",
      "description": "Root path to scan (defaults to current directory)"
    },
    "include_fixme": {
      "type": "boolean",
      "description": "Include FIXME markers (default: true)"
    },
    "include_hack": {
      "type": "boolean",
      "description": "Include HACK markers (default: true)"
    },
    "max_results": {
      "type": "integer",
      "minimum": 1,
      "description": "Maximum number of results to return (default: 50)"
    }
  }
}
```

### Parameters

| Parameter | Type | Required | Default | Description |
|-----------|------|----------|---------|-------------|
| `path` | string | ❌ | `.` (current dir) | Root path to scan for markers |
| `include_fixme` | boolean | ❌ | `true` | Include FIXME markers in results |
| `include_hack` | boolean | ❌ | `true` | Include HACK markers in results |
| `max_results` | integer | ❌ | 50 | Maximum number of results to return |

### Example Usage

```bash
rustcode tool todo-scan --path . --include-fixme --include-hack --max-results 50
rustcode tool todo-scan --path src/ --include-fixme --max-results 20
rustcode tool todo-scan --path tests/ --include-hack false
```

## Output

Returns a prioritised JSON list of TODO/FIXME/HACK markers:

```json
[
  {
    "file": "src/main.rs",
    "line": 42,
    "priority": "high",
    "marker": "TODO",
    "text": "Implement error handling for network requests"
  },
  {
    "file": "src/lib.rs",
    "line": 128,
    "priority": "medium",
    "marker": "FIXME",
    "text": "Refactor duplicate code in parser module"
  }
]
```

## Priority Classification

The plugin classifies markers by priority:
- **Critical**: Security issues, blocking bugs
- **High**: Important features, significant bugs
- **Medium**: Nice-to-haves, minor improvements
- **Low**: Cosmetic changes, future enhancements

Priority is determined by:
- Marker type (TODO < FIXME < HACK)
- Context and surrounding code
- Historical patterns in the codebase

## Configuration

- **Default Enabled**: `true`
- **Permission Level**: read-only
- **Command**: `rustcode`

## Related Plugins

- [`code-review`](../code-review/README.md) — Detailed code analysis
- [`file-summary`](../file-summary/README.md) — Generate file summaries