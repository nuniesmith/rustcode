# rustcode

**AI coding assistant — LLM proxy, RAG, semantic search, code audit, and async task agent.**

Rustcode is a general-purpose Rust backend service that provides an OpenAI-compatible LLM proxy, repository indexing with vector search, semantic code search, automated code auditing, and an async task pipeline. It works with any codebase — not FKS-specific.

Infrastructure for running rustcode alongside FKS lives in [fks](https://github.com/nuniesmith/fks). For standalone deployment, see the Docker instructions below.

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
    ├── xAI (Grok) ← DEFAULT for all requests
    │
    └── (when Ollama is enabled and reachable):
        ├── ScaffoldStub / TodoTagging / TreeSummary → Ollama (local)
        ├── RepoQuestion / SymbolExtraction          → Ollama (local, fast)
        └── ArchitecturalReason / CodeReview         → Grok (remote)
```

Ollama is optional. Set `XAI_API_KEY` and everything routes through Grok.

## Quick start

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
| `XAI_API_KEY` | ✅ | xAI (Grok) API key — default LLM provider |
| `DATABASE_URL` | ✅ | Postgres connection string |
| `RC_PROXY_API_KEYS` | Recommended | Comma-separated bearer tokens for auth |
| `GITHUB_TOKEN` | Optional | PAT for repo sync and webhook |
| `OLLAMA_BASE_URL` | Optional | Enable local inference (default: off) |
| `OLLAMA_ENABLED` | Optional | Set `true` to enable Ollama routing |
| `REPOS_DIR` | Optional | Where to clone repos (default: `/repos`) |
| `REPO_SYNC_INTERVAL_SECS` | Optional | Auto-sync interval (default: 3600) |

Auth is enforced when `RC_PROXY_API_KEYS` is set. All `/api/*` and `/v1/*` routes require `Authorization: Bearer <key>` or `X-API-Key: <key>`. Set `RC_AUTH_DISABLED=true` to opt out (dev only — logs a loud warning).

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
    "answered_by": "grok-3",
    "rag_chunks_used": 4,
    "cached": false,
    "tokens_used": 1240,
    "estimated_cost_usd": 0.0062
  }
}
```

## Stats

- ~81K lines of Rust
- 417 lib + 33 doctests (0 failed)
- 34 Postgres tables
- 80 public modules
- 20 SQL migrations
