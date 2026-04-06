# rustcode — TODO

> **Repo:** `github.com/nuniesmith/rustcode`
> **Last synced from master todo:** 2026-04-03
> **RC-CRATES-A resolved:** 2026-04-05
> **Code review completed:** 2026-04-05 — see notes below each relevant item
> **RC-CRATES-D plugin migration prep completed:** 2026-04-06 — three tools migrated to bundled plugins

---

## P0 — Async Task Agent Pipeline

> The core new capability: drop a JSON file → rustcode picks it up, does the work, opens a PR.

- [ ] **TASK-A:** Implement `tasks/` directory watcher — `notify` crate, debounced, ignore `.tmp` files
  > `notify = "6"` dep is already in `src/rc/Cargo.toml`. Entry point: add `src/task_watcher.rs`,
  > spawn it from `server.rs` via `tokio::spawn`. Debounce with a 500ms window using `tokio::time::sleep`.
- [ ] **TASK-B:** Define task file schema (JSON): `id`, `repo`, `description`, `steps[]`, `branch`, `labels[]`, `auto_merge` bool
  > Add `src/task/schema.rs` with serde structs. Wire `Deserialize` only — task files are read-only input.
  > Note: `src/task/` already has `models.rs` with a DB-backed `Task` type and `src/tasks.rs` has
  > an audit-oriented `TaskGenerator`. Pick one home and consolidate naming before adding more task types —
  > see RC-CLEANUP-D below.
- [ ] **TASK-C:** Task executor — for each step, call appropriate tool (LLM scaffold, file create/edit, run tests, commit)
- [ ] **TASK-C:** Per-language test runner: `cargo check` / `cargo test` for Rust; `pytest -x` for Python; `npm run build` for TS
  > Can re-use `src/tests_runner.rs` (`TestRunner`) which already handles cargo/pytest dispatch.
- [ ] **TASK-D:** GitHub PR creation — use existing `github::client` to: create branch, push commits, open PR with task description + step log
- [ ] **TASK-D:** Auto-merge logic: if all CI checks pass and `auto_merge: true`, merge the PR
- [ ] **TASK-E:** Task result file — write `tasks/results/{id}.json` with outcome, PR URL, test results, any errors
- [ ] **TASK-F:** Failed task handling — if any step fails or tests fail, tag PR `needs-review` and write error details to result file; never auto-merge

---

## P1 — API & Configuration

### RC-LLM: Routing & Proxy Validation
- [ ] Verify `rc-app` starts and serves `/v1/chat/completions` with only `XAI_API_KEY` set (no Ollama) on live stack
- [ ] Verify `ModelRouter` falls back to Grok when Ollama is unreachable (implemented, needs live validation)
- [ ] Test `curl` end-to-end: auth header, model=auto, multi-turn context preservation
- [ ] Test `x_repo_id` injection: register a repo, send domain-specific question, confirm `rag_chunks_used > 0`
- [ ] Test cache: send identical request twice, confirm `cached: true` on second response
- [ ] Test auth: request without `Authorization` header → 401

### RC-API: Security & Config
- [ ] Make skip-extensions configurable per-repo — `skip_extensions: Vec<String>` in repo config struct; pass through `AutoScanner` and `StaticAnalysis` call sites
- [ ] Routing heuristic tuning — after deployment, measure local vs Grok classification quality and adjust `ModelRouter::llm_classify` system prompt

### OpenClaw Integration (CLAW-A/B)
- [ ] Add `DISCORD_BOT_TOKEN` and `OPENCLAW_GATEWAY_TOKEN` to `.env` on deployment target
- [ ] Replace `DISCORD_WEBHOOK_ACTIONS` (passive webhook) with active Discord bot
- [ ] Build or pull the OpenClaw Docker image (`OPENCLAW_IMAGE` env var) — resolve OOM issue first (node needs ~760MB heap)
- [ ] Smoke test: `@openclaw status` → RustCode health + latest commit
- [ ] Register the RustCode repo itself via `POST /api/v1/repos` — gives OpenClaw RAG over its own codebase
- [ ] Confirm `session-scribe` agent calls `POST .../documents` to index session notes
- [ ] Confirm `rag_chunks_used > 0` in a pre-session brief referencing prior work

---

## P1 — New Crates Integration (RC-CRATES)

> 9 new crates in `crates/` — wire into existing binary.

- [x] **RC-CRATES-A: workspace integration + `cargo check --workspace` clean**
  > **Done — three files created, one FKS root patch required:**
  >
  > Files created in this session:
  > - `src/rc/Cargo.toml` — root rustcode package (no [workspace]; member of FKS root)
  > - `src/rc/crates/tools/src/lib.rs` — AgentOutput struct + pub mod lane_completion
  > - `src/rc/crates/rusty-claude-cli/src/main.rs` — claw binary entrypoint
  >
  > One manual step required — apply fks_root_cargo_patch.diff to the FKS root Cargo.toml:
  > 1. Add `publish = false` to [workspace.package]
  > 2. Add the 9 rc crates as workspace members under the "src/rc" entry:
  >      "src/rc/crates/api",
  >      "src/rc/crates/commands",
  >      "src/rc/crates/compat-harness",
  >      "src/rc/crates/mock-anthropic-service",
  >      "src/rc/crates/plugins",
  >      "src/rc/crates/runtime",
  >      "src/rc/crates/rusty-claude-cli",
  >      "src/rc/crates/telemetry",
  >      "src/rc/crates/tools",
  > 3. Run `cargo check --workspace` from the FKS root to verify.
  >
  > Known sqlx feature caveat: src/rc/Cargo.toml uses `runtime-tokio` + `tls-rustls-ring`
  > (sqlx 0.8 feature names). Adjust if your FKS sqlx version differs.

- [ ] **RC-CRATES-B: replace LLM call code with `api` crate**
  > `src/simple_client.rs` is already migrated — it wraps `api::OpenAiCompatClient`.
  > Remaining raw-reqwest LLM callers to migrate, in priority order:
  >
  > 1. `src/grok_client.rs` — heaviest user; replace `reqwest::Client` with `api::OpenAiCompatClient`.
  >    Also carries cost-tracking logic — once migrated, this file should be deleted and its
  >    cost-tracking folded into `src/llm/` (see RC-CLEANUP-A).
  > 2. `src/grok_reasoning.rs` — uses `GrokReasoningClient`; same pattern. After migration,
  >    consolidate into `src/llm/reasoning.rs`.
  > 3. `src/llm/grok.rs` — third independent client using raw reqwest, used by the queue processor.
  >    Migrate to `api::OpenAiCompatClient`, then merge into the unified `src/llm/client.rs`.
  > 4. `src/model_router.rs` — uses raw reqwest for Ollama health-check + Grok fallback;
  >    wire Ollama path via `api::OpenAiCompatClient` with base URL override.
  > 5. `src/ollama_client.rs` — wire `api::OpenAiCompatClient` with Ollama base URL override;
  >    move to `src/llm/ollama.rs` after migration.
  >
  > End state: all five files above are gone. `src/llm/` owns one coherent set of clients.
  > `src/llm/simple_client.rs` is the template to follow for all of them.

- [ ] **RC-CRATES-C: replace scanner with `runtime` crate**
  > `runtime::execute_bash`, `runtime::ProviderClient`, `runtime::worker_boot` are the key
  > integration points. Start by replacing `src/tests_runner.rs` subprocess logic with
  > `runtime::execute_bash` — it handles sandboxing, timeout, and output capture already.

- [ ] **RC-CRATES-D: wire `tools` + `plugins` for tool execution**
  > `tools::AgentOutput` and `tools::detect_lane_completion` are now exported.
  >
  > **Preparation step completed (2026-04-06):**
  > - Migrated three plugin manifests from project root `.toml` files to bundled plugin structure:
  >   - `crates/plugins/bundled/todo-scan/` — TodoScanner plugin for repository TODO/FIXME/HACK marker scanning
  >   - `crates/plugins/bundled/file-summary/` — LLM-powered file summarization using Grok 4.20
  >   - `crates/plugins/bundled/code-review/` — Automated code review using Grok multi-agent
  > - Converted all manifests from TOML to JSON format per project plugin standard
  > - Added comprehensive README.md documentation for each plugin
  > - All three plugins follow the same layout as `example-bundled` and `sample-hooks`
  >
  > **Next:** wire `plugins::PluginLifecycle` into the task executor (TASK-C) so that
  > bundled plugins (example-bundled, sample-hooks, todo-scan, file-summary, code-review)
  > run pre/post hooks around each step.

- [ ] **RC-CRATES-E: `--server` flag for MCP-style tool endpoint on :3501**
  > Add `--mcp-server` flag to `src/bin/server.rs`. When set, start a second Axum
  > listener on port 3501 serving `runtime::mcp_tool_bridge`.
  > The claw binary Login/Logout subcommands also need runtime OAuth wiring
  > (stubs left in `crates/rusty-claude-cli/src/main.rs` with TODO comments).

- [ ] **RC-CRATES-F: `claw-cli` binary in Docker image**
  > `crates/rusty-claude-cli` builds the claw binary. Add a second stage to the
  > rustcode Dockerfile copying `target/release/claw` into the image.
  > Verify: `docker run rustcode claw --help`

- [ ] **RC-CRATES-G: Grok integration test suite; validate then switch to Claude API**
  > `crates/mock-anthropic-service` is the test harness — already a dev-dep of rusty-claude-cli.
  > Add integration tests in `crates/rusty-claude-cli/tests/` that:
  > 1. Full round-trip through `api::AnthropicClient`
  > 2. `api::OpenAiCompatClient` with xAI base URL (Grok)
  > 3. Migrate `grok_client.rs` to api crate, re-run tests to confirm parity
  >
  > Note: `src/test_grok_integration.rs` currently lives inside `src/` and compiles into the
  > library. Move it to `tests/integration/grok.rs` as part of this task — see RC-CLEANUP-F.

---

## P1 — src/ Cleanup (RC-CLEANUP)

> Identified during 2026-04-05 code review. ~82K LoC in a 48-module god crate.
> Items ordered by risk/reward — do before the crate extractions in P2.

- [ ] **RC-CLEANUP-A: consolidate LLM clients into `src/llm/`**
  > Six separate Grok/xAI implementations exist right now (see RC-CRATES-B for the list).
  > Once RC-CRATES-B migrates each caller off raw reqwest, merge all into `src/llm/`:
  > - `src/llm/client.rs` — unified Grok client (merged from grok_client + grok_reasoning + llm/grok)
  > - `src/llm/ollama.rs` — Ollama client (from ollama_client.rs)
  > - `src/llm/router.rs` — ModelRouter (from model_router.rs)
  > - `src/llm/config.rs` — LlmConfig (from llm_config.rs)
  > - `src/llm/usage/budget.rs` — TokenBudget (from token_budget.rs)
  > - `src/llm/usage/costs.rs` — CostTracker (from cost_tracker.rs)
  >
  > Delete: `src/grok_client.rs`, `src/grok_reasoning.rs`, `src/ollama_client.rs`,
  >         `src/simple_client.rs`, `src/model_router.rs`, `src/llm_config.rs`,
  >         `src/token_budget.rs`, `src/cost_tracker.rs`
  >
  > `lib.rs` drops from 48 pub mods to ~40.

- [ ] **RC-CLEANUP-B: consolidate cache modules into `src/cache/`**
  > Four separate cache systems sit as top-level siblings with no grouping:
  > - `src/cache.rs` → `src/cache/audit.rs` (file-based audit cache)
  > - `src/cache_layer.rs` → `src/cache/layer.rs` (Redis + in-memory LRU)
  > - `src/response_cache.rs` → `src/cache/responses.rs` (SQLite LLM response cache)
  > - `src/cache_migrate.rs` → `src/cache/migrate.rs` (migration utility)
  >
  > Create `src/cache/mod.rs` that re-exports each. No logic changes needed yet —
  > pure restructure. `lib.rs` loses 3 top-level mods, gains 1 `pub mod cache`.

- [ ] **RC-CLEANUP-C: remove the old file-based repo cache**
  > `src/repo_cache.rs` is the original file-based implementation.
  > `src/repo_cache_sql.rs` is the SQL replacement.
  > `src/cache_migrate.rs` exists to move data between them.
  > If the SQL path is stable, delete `src/repo_cache.rs`, rename `src/repo_cache_sql.rs`
  > to `src/repo/cache.rs`, and move `src/repo_manager.rs`, `src/repo_sync.rs`,
  > `src/repo_analysis.rs` alongside it into `src/repo/`.

- [ ] **RC-CLEANUP-D: resolve task/todo naming collisions**
  > Two pairs of overlapping names causing confusion:
  >
  > Tasks: `src/tasks.rs` has `TaskGenerator` (converts audit findings → task list).
  > `src/task/` has a DB-backed `Task` management system (CRUD, grouping, status).
  > These are genuinely different things. Rename `src/tasks.rs` → `src/audit_tasks.rs`
  > (or move it into `src/audit/` as `src/audit/tasks.rs` where it conceptually belongs).
  >
  > Todos: `src/todo_scanner.rs` at root defines `TodoItem` and a basic regex scanner.
  > `src/todo/scanner.rs` is the richer version used by the full todo pipeline.
  > The root file is the ancestor that wasn't removed when `src/todo/` was built.
  > Check all callsites of `crate::todo_scanner::TodoItem` — most likely they should
  > import from `crate::todo::scanner` instead — then delete `src/todo_scanner.rs`.

- [ ] **RC-CLEANUP-E: consolidate context modules into `src/context/`**
  > `src/context.rs` builds `GlobalContextBundle` (signature maps, dependency graphs,
  > architectural rules) for LLM analysis.
  > `src/context_builder.rs` builds `ContextBuilder` for RAG (loads repo files into
  > the 2M token window).
  > Related concepts, unrelated positions in `lib.rs`. Move to:
  > - `src/context/global.rs` (from context.rs)
  > - `src/context/rag.rs` (from context_builder.rs)
  > - `src/context/mod.rs` re-exporting both

- [ ] **RC-CLEANUP-F: move integration test file out of `src/`**
  > `src/test_grok_integration.rs` requires live API keys and is gated behind a feature flag,
  > but it currently compiles into the library crate. Move to `tests/integration/grok.rs`.
  > Remove it from `lib.rs` and add the `[[test]]` entry in `Cargo.toml`.

- [ ] **RC-CLEANUP-G: rename `prompt_router.rs` to avoid confusion with `query_router.rs`**
  > `src/query_router.rs` — routes user queries by intent (greeting/search/analysis).
  > `src/prompt_router.rs` — routes *files* to prompt tiers (minimal/standard/deep-dive)
  > based on static analysis scores. Different layers, confusingly similar names.
  > Rename `prompt_router.rs` → `prompt_tier.rs` and update all use sites.

---

## P1 — OpenClaw LLM Wiring

- [ ] Wire futures trading app to RC proxy: add `RC_BASE_URL`, `RC_API_KEY`, `RC_TIMEOUT_SECS`, `RC_MODEL`, `RC_REPO_ID` to futures app env
- [ ] Validate `x_ra_metadata.cached` flips to `true` on second identical call
- [ ] Confirm Grok fallback fires when Ollama container is killed

---

## P2 — Promptfoo CI

- [ ] Create CI step that runs `promptfoo eval` on prompt changes — add `.github/workflows/prompt-eval.yml`
- [ ] Red-team trading-related prompts live (requires running stack + API keys)

---

## P2 — Agent Persona Integration

- [ ] OSS-D: Adapt FKS agent personas (quantitative-analyst, rust-systems-engineer, devops-engineer, trading-ui-developer) into `prompt_tier.rs` system prompt templates for task-based routing
  > Note: file will be renamed from `prompt_router.rs` — see RC-CLEANUP-G

---

## P2 — Crate Extractions (RC-EXTRACT)

> Do these *after* the RC-CLEANUP items above — extracting before the modules are clean
> just moves the mess into a new crate boundary.
> Prerequisite: RC-CLEANUP-A, RC-CLEANUP-B, RC-CLEANUP-C done first.

- [ ] **RC-EXTRACT-A: `crates/rag` — semantic indexing pipeline**
  > Candidates: `src/chunking.rs`, `src/code_chunker.rs`, `src/embeddings.rs`,
  > `src/vector_index.rs`, `src/indexing.rs`, `src/search.rs`
  >
  > This is a coherent pipeline (chunk → embed → store → retrieve) with no server,
  > GitHub, or LLM-config dependencies. The only thing to untangle: `src/indexing.rs`
  > calls into `src/db/` — introduce a `Storage` trait so the crate doesn't depend on
  > the full DB layer.
  >
  > Once extracted: usable independently by `crates/runtime` (its `summary_compression`
  > does related chunking work) and testable without spinning up the full server.

- [ ] **RC-EXTRACT-B: `crates/code-analysis` — zero-cost pre-filter pipeline**
  > Candidates: `src/static_analysis.rs`, `src/parser.rs`, `src/scoring.rs`,
  > `src/formatter.rs`, plus the cleaned-up todo scanner (after RC-CLEANUP-D)
  >
  > Entirely synchronous, no network or DB deps. Could also be used by
  > `crates/runtime`'s bash validation or `crates/plugins`.

- [ ] **RC-EXTRACT-C: `crates/github-client` — GitHub API client**
  > Candidates: `src/github/client.rs`, `src/github/models.rs`, `src/github/search.rs`
  >
  > The pure HTTP client side is self-contained. Leave the sync logic
  > (`github/sync.rs`, `github/background_sync.rs`, `github/webhook.rs`) in `src/`
  > since it ties into the DB and queue.

- [ ] **RC-EXTRACT-D: `crates/llm` — unified LLM client surface**
  > Prerequisite: RC-CLEANUP-A and RC-CRATES-B fully done.
  > Once all clients are consolidated under `src/llm/`, the whole module
  > (`llm/`, with router, config, usage tracking) can become a standalone crate
  > wrapping `crates/api`. Exposes a clean `ModelRouter` + typed client API to
  > the rest of the workspace.

---

## P3 — Future

- [ ] OSS-B: OpenViking — stand up Docker instance, ingest FKS docs/strategies, compare retrieval quality vs current HNSW
- [ ] CI/CD re-enable: move `ci-cd.yml` back to `.github/workflows/` after OpenClaw OOM fix + Tailscale second-device verification
- [ ] Split `crates/commands/src/lib.rs` (140K, skipped in pack as too large) — it's currently a single-file crate with no internal module structure; break into submodules by command group
- [ ] Split `crates/rusty-claude-cli/src/main.rs` (272K, skipped in pack) — same issue
