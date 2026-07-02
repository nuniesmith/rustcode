# rustcode

**AI coding assistant — LLM proxy, RAG, semantic search, code audit, and async task agent.**

Rustcode is a general-purpose Rust backend service that provides an OpenAI-compatible LLM proxy, repository indexing with vector search, semantic code search, automated code auditing, and an async task pipeline. It works with any codebase — not FKS-specific.

---

## What it does

| Capability | Endpoint |
|-----------|----------|
| OpenAI-compatible LLM proxy | `POST /v1/chat/completions`, `GET /v1/models` |
| Repository indexing + RAG | `POST /api/v1/repos`, `POST /api/v1/repos/:id/sync` |
| Semantic search over code | `POST /api/v1/search` |
| Code audit pipeline | `POST /api/v1/audit`, `GET /api/v1/audit/:id` |
| Document indexing | `POST /api/v1/documents` |
| TODO scanner + planner | `POST /api/v1/todo/scan`, `POST /api/v1/todo/plan` |
| GitHub webhook receiver | `POST /api/github/webhook` |
| Health + metrics | `GET /healthz`, `GET /metrics` |

## LLM routing

Requests to `/v1/chat/completions` are classified by `ModelRouter` and routed:

```
Request → ModelRouter::classify_prompt_async()
    │
    ├── (when ANTHROPIC_API_KEY is set):
    │   ├── ArchitecturalReason / CodeReview / Unknown → Claude Opus 4.7 (planner)
    │   └── ScaffoldStub / TodoTagging / TreeSummary /
    │       SymbolExtraction / RepoQuestion            → Claude Sonnet 4.6 (executor)
    │
    ├── (fallback when only XAI_API_KEY is set):
    │   └── all kinds → xAI Grok
    │
    └── (when Ollama is enabled and reachable):
        ├── ScaffoldStub / TodoTagging / TreeSummary → Ollama (local)
        ├── RepoQuestion / SymbolExtraction          → Ollama (local, fast)
        └── ArchitecturalReason / CodeReview         → Grok (remote)
```

Anthropic is the primary path when `ANTHROPIC_API_KEY` is set. Grok and Ollama are
fallbacks; either can be enabled independently.

## Install

### Prebuilt binaries (no compiling)

Tagged releases publish prebuilt binaries for Linux, macOS, and Windows on the
[Releases page](https://github.com/nuniesmith/rustcode/releases). Download and
extract an archive, or let [cargo-binstall](https://github.com/cargo-bins/cargo-binstall)
fetch them for you:

```bash
cargo binstall --git https://github.com/nuniesmith/rustcode rustcode          # server + helpers
cargo binstall --git https://github.com/nuniesmith/rustcode rusty-claude-cli  # claw CLI
```

### From source

Compile and install with Cargo. Requires a recent stable Rust toolchain (1.85+
for the 2024 edition) and a C compiler for the native dependencies; the first
build compiles everything from source.

```bash
# Server + helper binaries (installs `rustcode`, `rustcode-cli`, `github-sync-daemon`)
cargo install --git https://github.com/nuniesmith/rustcode --locked rustcode

# Interactive CLI (installs `claw`)
cargo install --git https://github.com/nuniesmith/rustcode --locked rusty-claude-cli

# Pin to a released tag instead of the default branch
cargo install --git https://github.com/nuniesmith/rustcode --tag v0.1.0 --locked rustcode

# Just the server binary, nothing else
cargo install --git https://github.com/nuniesmith/rustcode --locked --bin rustcode rustcode
```

Then configure and run:

```bash
cp .env.example .env     # set ANTHROPIC_API_KEY (or XAI_API_KEY) and DATABASE_URL
rustcode --server        # start the server
claw                     # start the interactive CLI
```

> **Not on crates.io.** The names `rustcode`, `api`, `runtime`, `tools`,
> `plugins`, `rag`, and `telemetry` are all already taken on crates.io, so
> rustcode is distributed via `cargo install --git`. Cut a new tagged release
> with `./scripts/release.sh patch|minor|major`.

## Build from source

```bash
# Clone and build
git clone https://github.com/nuniesmith/rustcode
cd rustcode
cargo build --release

# Configure
cp .env.example .env
# Set XAI_API_KEY (required), GITHUB_TOKEN (optional), DATABASE_URL (Postgres)

# Run
./target/release/rustcode --server
```

Or via Docker:

```bash
docker build -t rustcode .
docker run -p 3500:3500 --env-file .env rustcode
```

## Configuration

| Env var | Required | Description |
|---------|----------|-------------|
| `ANTHROPIC_API_KEY` | Recommended | Anthropic API key — when set, Claude is the primary LLM |
| `RC_PLANNER_MODEL` | Optional | Override planner-tier model (default: `claude-opus-4-7`) |
| `RC_EXECUTOR_MODEL` | Optional | Override executor-tier model (default: `claude-sonnet-4-6`) |
| `XAI_API_KEY` | Fallback | xAI (Grok) API key — fallback when `ANTHROPIC_API_KEY` is absent |
| `DATABASE_URL` | ✅ | Postgres connection string |
| `RUSTCODE_PROXY_API_KEYS` | Recommended | Comma-separated bearer tokens for auth |
| `GITHUB_TOKEN` | Optional | PAT for repo sync and webhook |
| `OLLAMA_BASE_URL` | Optional | Enable local inference (default: off) |
| `OLLAMA_ENABLED` | Optional | Set `true` to enable Ollama routing |
| `REPOS_DIR` | Optional | Where to clone repos (default: `/repos`) |
| `REPO_SYNC_INTERVAL_SECS` | Optional | Auto-sync interval (default: 3600) |

At least one of `ANTHROPIC_API_KEY` or `XAI_API_KEY` must be set for the LLM proxy to work.

Auth is enforced when `RUSTCODE_PROXY_API_KEYS` is set. All `/api/*` and `/v1/*` routes require `Authorization: Bearer <key>` or `X-API-Key: <key>`. Set `RUSTCODE_AUTH_DISABLED=true` to opt out (dev only — logs a loud warning).

## Async task agent (task file pipeline)

Rustcode watches a `tasks/` directory for JSON task files. Drop a file in, rustcode picks it up, executes the steps, and opens a GitHub PR with the result. You review; if checks pass it merges automatically.

**Task file format:**
```json
{
  "id": "unique-task-id",
  "repo": "nuniesmith/fks-ruby",
  "description": "Add ModuleRegistry base classes",
  "steps": [
    "Create src/ruby/src/core/module_registry.py with FKSModule ABC",
    "Add __fks_module__ sentinel to indicators/trend/exponential_moving_average.py",
    "Add unit test in tests/test_module_registry.py"
  ],
  "branch": "feat/module-registry-pilot",
  "labels": ["auto-pr"]
}
```

Drop `tasks/my-task.json` → rustcode scans, plans, scaffolds, runs `cargo check` / `pytest` per language, commits, and opens a PR. PRs that pass CI merge automatically; failing ones are flagged for manual review.

**GitHub credentials** are configured via `GITHUB_TOKEN` in `.env`.

## Per-repo cache (`.rustcode/`)

Each indexed repo gets a `.rustcode/` directory:

```
.rustcode/
├── manifest.json    — repo identity, last_synced, branch
├── tree.txt         — rolling file tree snapshot
├── todos.json       — all TODO/STUB/FIXME/HACK tags
├── symbols.json     — public API (syn-parsed for Rust)
├── context.md       — LLM-ready summary
├── embeddings.bin   — cached vector embeddings (gitignored)
└── results/         — WorkResult JSON files per batch
```

## Proxy response extensions

Responses from `/v1/chat/completions` include an `x_ra_metadata` field:

```json
{
  "x_ra_metadata": {
    "task_kind": "ArchitecturalReason",
    "used_fallback": false,
    "repo_context_injected": true,
    "rag_chunks_used": 4,
    "cached": false,
    "cache_key": "proxy:a1b2c3d4e5f6g7h8",
    "cache_creation_input_tokens": 1820,
    "cache_read_input_tokens": 240
  }
}
```

`cache_creation_input_tokens` and `cache_read_input_tokens` are only populated for
Claude responses and reflect Anthropic prompt-cache activity (80–90% cost reduction
on repeated repo-context calls).

## Client integrations

Because rustcode is OpenAI-compatible, any client that speaks the OpenAI API can
point at it with **zero code changes** — just set the base URL to
`http://<host>:3500/v1` and use one of your `RUSTCODE_PROXY_API_KEYS` as the API
key (or leave auth disabled in dev).

### OpenWebUI

The `docker-compose.yml` ships an optional OpenWebUI sidecar behind the `webui`
profile:

```bash
docker compose --profile webui up -d
```

Browse to `http://localhost:3000`, sign up locally, and the model list populates
from rustcode's `GET /v1/models`. All chat routes through
`POST /v1/chat/completions`. Set `OPENWEBUI_API_KEY` (must match one of
`RUSTCODE_PROXY_API_KEYS`) and optionally `OPENWEBUI_HOST_PORT` in `.env`.

### Zed

Add an `openai_compatible` provider to your Zed `settings.json` (open with
`zed: open settings`). This works today against the running proxy — no
server-side changes needed:

```json
{
  "language_models": {
    "openai_compatible": {
      "rustcode": {
        "api_url": "http://your-server:3500/v1",
        "available_models": [
          {
            "name": "claude-opus-4-7",
            "display_name": "rustcode planner",
            "max_tokens": 200000,
            "max_output_tokens": 32000,
            "capabilities": { "tools": true, "images": false }
          },
          {
            "name": "claude-sonnet-4-6",
            "display_name": "rustcode executor",
            "max_tokens": 200000,
            "max_output_tokens": 64000,
            "capabilities": { "tools": true, "images": false }
          },
          {
            "name": "auto",
            "display_name": "rustcode auto-route",
            "max_tokens": 131072,
            "max_output_tokens": 32768,
            "capabilities": { "tools": false, "images": false }
          }
        ]
      }
    }
  },
  "agent": {
    "default_model": { "provider": "rustcode", "model": "claude-sonnet-4-6" }
  }
}
```

Zed prompts for the API key in the Agent Panel provider settings — set it to one
of your `RUSTCODE_PROXY_API_KEYS` values, or leave auth disabled for local dev.
The model names are tier selectors: the actual upstream model slugs come from
`RC_PLANNER_MODEL` / `RC_EXECUTOR_MODEL`. Tool calling (Zed agent mode) is
supported on the Claude-tier models.

## Stats

- ~81K lines of Rust
- 417 lib + 33 doctests (0 failed)
- 34 Postgres tables
- 80 public modules
- 20 SQL migrations
