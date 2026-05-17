# rustcode — TODO

> **Repo:** `github.com/nuniesmith/rustcode`
> **Last synced from master todo:** 2026-04-03
> **RC-CRATES-A resolved:** 2026-04-05
> **Code review completed:** 2026-04-05 — see notes below each relevant item
> **RC-CRATES-D plugin migration prep completed:** 2026-04-06 — three tools migrated to bundled plugins
> **Architecture review completed:** 2026-04-07 — Claude provider wiring, two-tier routing, agent loop, and memory system added

---

## P0 — Wire Claude as Primary Provider

> `AnthropicClient` in `crates/api/src/providers/anthropic.rs` is fully built (retry, streaming,
> OAuth, prompt-cache tracking) but is **never called from the proxy hot path** — everything
> still dispatches to Grok via `GrokClient`. These three items wire it in. Everything below
> (planner loop, agent memory, cost reduction) depends on this being done first.

- [ ] **CLAUDE-A: add `ModelTarget::Claude` and wire `AnthropicClient` into `dispatch()`**
  > `src/api/proxy.rs` `dispatch()` only handles `ModelTarget::Local` (Ollama) and
  > `ModelTarget::Remote` (Grok). Add a third arm:
  >
  > ```rust
  > ModelTarget::Claude { model, tier } => {
  >     let client = AnthropicClient::from_env()  // reads ANTHROPIC_API_KEY
  >         .with_prompt_cache(PromptCache::new()); // wire caching
  >     // call client.send_message() and map response to (String, String, bool, Option<u32>)
  > }
  > ```
  >
  > Add `ANTHROPIC_API_KEY` to `.env.example` and `config.rs`.
  > `AnthropicClient::from_env()` already reads this env var — no new auth code needed.
  > `route_from_model_field()` already catches `claude-*` model names; update it to return
  > `ModelTarget::Claude` instead of routing back to the Grok remote target.

- [ ] **CLAUDE-B: two-tier routing — Opus 4.7 (planner) vs Sonnet 4.6 (executor)**
  > Add `ClaudeTier` enum to `src/model_router.rs` and extend `ModelTarget::Claude`:
  >
  > ```rust
  > pub enum ClaudeTier { Planner, Executor }
  >
  > // Planner = claude-opus-4-7  → ArchitecturalReason, CodeReview, Unknown
  > // Executor = claude-sonnet-4-6 → ScaffoldStub, TodoTagging, TreeSummary,
  > //                                  SymbolExtraction, RepoQuestion
  > ```
  >
  > Update `TaskKind` with a `tier()` method that returns `ClaudeTier`.
  > Update `ModelRouterConfig::default()` — remove `remote_model: "grok-4-1-fast-reasoning"`,
  > replace with `planner_model: "claude-opus-4-7"` and `executor_model: "claude-sonnet-4-6"`.
  > Verify exact model slugs against Anthropic API before hardcoding.
  > Also update model aliases in `crates/api/src/providers/mod.rs`:
  > `"opus"` → `"claude-opus-4-7"`, `"sonnet"` → `"claude-sonnet-4-6"`.

- [ ] **CLAUDE-C: enable Anthropic prompt caching in the proxy dispatch**
  > `PromptCache` in `crates/api/src/prompt_cache.rs` is fully built but unused in the proxy.
  > When calling `AnthropicClient` from `dispatch()`, attach a `PromptCache` instance so that
  > repeated system prompts and long repo contexts get `cache_control: {"type": "ephemeral"}`
  > headers automatically. The `AnthropicClient::with_prompt_cache()` method accepts it.
  >
  > Also surface `cache_read_input_tokens` and `cache_creation_input_tokens` from
  > `Usage` (already tracked in `crates/api/src/types.rs`) into `x_ra_metadata` on the
  > proxy response so cost tracking reflects actual cache savings.
  >
  > Expected savings: 80–90% token cost reduction on repeated repo-context calls.

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

## P1 — Planner-Executor-Reviewer Agent Loop

> Implements the three-phase agent cycle: Opus plans → Sonnet executes → Opus reviews → repeat.
> Prerequisite: CLAUDE-A and CLAUDE-B must be done first.
> The `ProjectPlan` and `ProjectPhase` structs already exist in `src/llm/grok.rs` — reuse them.

- [ ] **AGENT-A: `src/agent/pipeline.rs` — `AgentPipeline` struct with three phases**
  > ```rust
  > pub struct AgentPipeline {
  >     planner:  AnthropicClient,  // claude-opus-4-7
  >     executor: AnthropicClient,  // claude-sonnet-4-6
  >     memory:   AgentMemory,      // see MEM-A below
  > }
  >
  > impl AgentPipeline {
  >     // Phase 1: Opus generates a structured JSON plan
  >     async fn plan(&self, task: &str, context: &str) -> Result<ProjectPlan>
  >
  >     // Phase 2: Sonnet executes each step; injects memory + RAG context per step
  >     async fn execute(&self, plan: &ProjectPlan, repo: &RepoContext) -> Result<Vec<StepResult>>
  >
  >     // Phase 3: Opus reviews all step results; returns pass/revise + critique notes
  >     async fn review(&self, task: &str, results: &[StepResult]) -> Result<ReviewOutcome>
  >
  >     // Orchestrator: runs plan→execute→review, repeats if review says revise
  >     // Max iterations configurable (default 3) to cap cost
  >     pub async fn run(&self, task: AgentTask, max_iterations: u32) -> Result<PipelineResult>
  > }
  > ```
  >
  > `ReviewOutcome` should be an enum: `Approved { summary }` | `Revise { critique, suggestions }`.
  > `PipelineResult` records the full trace: plan, per-step results, review outcome, iteration count.

- [ ] **AGENT-B: `POST /v1/agent/run` endpoint**
  > Wire `AgentPipeline::run()` behind a new Axum route in `src/api/`.
  > Request body mirrors the task file schema (TASK-B) so the same JSON works
  > both as a dropped task file and as a direct API call.
  > Response: streaming SSE so the client sees plan → step output → review in real time.
  > Auth: same bearer-token gate as `/v1/chat/completions`.

- [ ] **AGENT-C: wire `AgentPipeline` into the task file watcher (TASK-C)**
  > When the `tasks/` watcher picks up a file (TASK-A), hand it to `AgentPipeline::run()`
  > instead of the current single-shot LLM scaffold. Write `PipelineResult` to
  > `tasks/results/{id}.json` (TASK-E). Only open a PR (TASK-D) if `ReviewOutcome::Approved`.

- [ ] **AGENT-D: per-step tool execution inside the executor phase**
  > During Phase 2, Sonnet may emit tool calls (file create/edit, bash, search).
  > Wire `runtime::execute_bash` (RC-CRATES-C) and `plugins::PluginLifecycle` (RC-CRATES-D)
  > as the tool backends so the executor can actually modify files, not just describe changes.
  > Per-language test runner (TASK-C) should be called automatically after each file-modifying step.

---

## P1 — Agent Memory System

> Persistent cross-session memory that accumulates knowledge about your projects, preferences,
> and patterns over time. Injected into the system prompt before each LLM call to give agents
> context without re-reading the full codebase. This is the "personalization" layer.
> `fastembed` is already in workspace deps — use it for memory embeddings.

- [ ] **MEM-A: `src/memory/store.rs` — `AgentMemory` backed by SQLite**
  > ```rust
  > pub struct AgentMemory { db: SqlitePool, embedder: TextEmbedding }
  >
  > pub enum MemoryKind {
  >     Observation,  // "project X uses pattern Y"
  >     Decision,     // "we chose approach A over B because..."
  >     Preference,   // "user prefers idiomatic Rust over verbose code"
  >     Pattern,      // recurring architectural pattern seen across projects
  >     TaskOutcome,  // what worked / what failed for a given task type
  > }
  >
  > pub struct MemoryEntry {
  >     pub id: Uuid,
  >     pub project: Option<String>,   // None = global, Some = project-scoped
  >     pub kind: MemoryKind,
  >     pub content: String,
  >     pub embedding: Vec<f32>,
  >     pub importance: f32,           // 0.0–1.0; drives retrieval ranking
  >     pub created_at: DateTime<Utc>,
  >     pub last_accessed: DateTime<Utc>,
  >     pub access_count: u32,
  > }
  > ```
  >
  > Add SQL migration `sql/023_agent_memory.sql`.
  > `AgentMemory::search(query, top_k)` — embed query with fastembed, cosine-rank stored entries.
  > `AgentMemory::record(entry)` — embed content, write to DB.

- [ ] **MEM-B: inject memories into every LLM call**
  > Before calling `AnthropicClient::send_message()` in `dispatch()` and inside `AgentPipeline`,
  > call `memory.search(user_prompt, 5)` and prepend the top-k results to the system prompt:
  >
  > ```
  > [Memory]
  > - (Decision) Prefer sqlx over diesel for async-friendly DB access in this project.
  > - (Pattern) All Axum handlers use State<Arc<AppState>>; never clone the pool directly.
  > - (Preference) User wants streaming responses for all long-running operations.
  > ```
  >
  > Gate behind a feature flag `memory_injection: bool` in `ModelRouterConfig` so it
  > can be disabled for benchmarking / cost comparison.

- [ ] **MEM-C: session consolidation — extract and store memories after each session**
  > After a session ends (or when `Session::record_compaction()` fires), call Sonnet
  > with a structured extraction prompt to identify decisions, patterns, and preferences
  > worth saving. Write each to `AgentMemory::record()`.
  > Reuse `runtime::summary_compression` for the pre-extraction summary step.
  > This is the "learns over time" behaviour — each completed session leaves behind
  > durable memories that improve future sessions.

- [ ] **MEM-D: importance scoring + pruning**
  > Increment `access_count` and update `last_accessed` on every memory retrieval.
  > Nightly cron (or manual trigger via `POST /api/v1/memory/prune`):
  > - Mark entries with `access_count == 0` and age > 30 days as low-importance
  > - Delete entries with `importance < 0.1` and age > 90 days
  > - Merge near-duplicate entries (cosine similarity > 0.95) — keep higher-importance copy

---

## P1 — API & Configuration

### RC-LLM: Routing & Proxy Validation
- [ ] Verify `rc-app` starts and serves `/v1/chat/completions` with only `ANTHROPIC_API_KEY` set (no Ollama) on live stack — replaces the old `XAI_API_KEY`-only smoke test
- [ ] Verify `ModelRouter` routes `TaskKind::ScaffoldStub` → Sonnet and `TaskKind::ArchitecturalReason` → Opus after CLAUDE-B lands
- [ ] Verify fallback: if `ANTHROPIC_API_KEY` absent but `XAI_API_KEY` present, router falls back to Grok gracefully
- [ ] Test `curl` end-to-end: auth header, `model=auto` classifies correctly, multi-turn context preservation
- [ ] Test `x_repo_id` injection: register a repo, send domain-specific question, confirm `rag_chunks_used > 0`
- [ ] Test prompt cache: send identical request twice, confirm `cache_read_input_tokens > 0` in second response `x_ra_metadata`
- [ ] Test cache: send identical request twice, confirm `cached: true` on second response
- [ ] Test auth: request without `Authorization` header → 401

### RC-API: Security & Config
- [ ] Make skip-extensions configurable per-repo — `skip_extensions: Vec<String>` in repo config struct; pass through `AutoScanner` and `StaticAnalysis` call sites
- [ ] Routing heuristic tuning — after CLAUDE-B deploys, measure Opus vs Sonnet classification quality and adjust `ModelRouter::llm_classify` system prompt; log `task_kind` per request to make this measurable
- [ ] Add `ANTHROPIC_API_KEY`, `RC_PLANNER_MODEL`, `RC_EXECUTOR_MODEL` to `.env.example` and README config table

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
  >   - `crates/plugins/bundled/file-summary/` — LLM-powered file summarization (update model ref from Grok → Sonnet after CLAUDE-A)
  >   - `crates/plugins/bundled/code-review/` — Automated code review (update model ref from Grok → Opus after CLAUDE-A)
  > - Converted all manifests from TOML to JSON format per project plugin standard
  > - Added comprehensive README.md documentation for each plugin
  > - All three plugins follow the same layout as `example-bundled` and `sample-hooks`
  >
  > **Next:** wire `plugins::PluginLifecycle` into the task executor (TASK-C) / `AgentPipeline`
  > (AGENT-D) so that bundled plugins run pre/post hooks around each step.
  > Update `file-summary` and `code-review` plugin manifests to reference Claude models once CLAUDE-A is done.

- [ ] **RC-CRATES-E: `--server` flag for MCP-style tool endpoint on :3501**
  > Add `--mcp-server` flag to `src/bin/server.rs`. When set, start a second Axum
  > listener on port 3501 serving `runtime::mcp_tool_bridge`.
  > The claw binary Login/Logout subcommands also need runtime OAuth wiring
  > (stubs left in `crates/rusty-claude-cli/src/main.rs` with TODO comments).

- [ ] **RC-CRATES-F: `claw-cli` binary in Docker image**
  > `crates/rusty-claude-cli` builds the claw binary. Add a second stage to the
  > rustcode Dockerfile copying `target/release/claw` into the image.
  > Verify: `docker run rustcode claw --help`

- [ ] **RC-CRATES-G: integration test suite covering both Claude and Grok paths**
  > `crates/mock-anthropic-service` is the test harness — already a dev-dep of rusty-claude-cli.
  > Add integration tests in `crates/rusty-claude-cli/tests/` that:
  > 1. Full round-trip through `api::AnthropicClient` (primary path after CLAUDE-A)
  > 2. Two-tier routing: verify Opus is selected for `ArchitecturalReason`, Sonnet for `ScaffoldStub`
  > 3. `api::OpenAiCompatClient` with xAI base URL (Grok) — kept as fallback path
  > 4. Prompt cache hit: send same request twice, assert `cache_read_input_tokens > 0` on second call
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
  > - `src/llm/client.rs` — unified client wrapping `api::AnthropicClient` + `api::OpenAiCompatClient`
  > - `src/llm/ollama.rs` — Ollama client (from ollama_client.rs)
  > - `src/llm/router.rs` — ModelRouter with `ClaudeTier` support (from model_router.rs, after CLAUDE-B)
  > - `src/llm/config.rs` — LlmConfig with `planner_model` / `executor_model` fields (from llm_config.rs)
  > - `src/llm/usage/budget.rs` — TokenBudget (from token_budget.rs)
  > - `src/llm/usage/costs.rs` — CostTracker; extend to track `cache_creation_tokens` and `cache_read_tokens` separately
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
- [ ] Set `RC_MODEL=claude-sonnet-4-6` for routine calls, `RC_MODEL=claude-opus-4-7` for planning/review calls in the futures app
- [ ] Validate `x_ra_metadata.cached` flips to `true` on second identical call
- [ ] Validate `x_ra_metadata.cache_read_input_tokens > 0` on cache-warmed calls (Anthropic-side savings)
- [ ] Confirm Grok fallback fires when `ANTHROPIC_API_KEY` is absent but `XAI_API_KEY` is present

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

## P2 — Server Deployment + OpenWebUI

> Rustcode is already OpenAI-compatible — OpenWebUI needs zero code changes.
> This is purely infrastructure/config work.

- [ ] **DEPLOY-A: Docker Compose with OpenWebUI sidecar**
  > Add `docker-compose.yml` at repo root:
  > ```yaml
  > services:
  >   rustcode:
  >     build: .
  >     ports: ["3500:3500"]
  >     env_file: .env
  >   openwebui:
  >     image: ghcr.io/open-webui/open-webui:main
  >     ports: ["3000:8080"]
  >     environment:
  >       OPENAI_API_BASE_URL: http://rustcode:3500/v1
  >       OPENAI_API_KEY: "${RC_PROXY_API_KEY}"
  >     depends_on: [rustcode]
  > ```
  > OpenWebUI will pull the model list from `GET /v1/models` and route all chat through rustcode.
  > Users get the full chat interface + history storage with no extra backend work.

- [ ] **DEPLOY-B: Zed IDE config documentation**
  > Document the Zed `assistant` config block in README:
  > ```json
  > "assistant": {
  >   "version": "2",
  >   "default_model": { "provider": "openai", "model": "claude-sonnet-4-6" },
  >   "openai": {
  >     "api_url": "http://your-server:3500/v1",
  >     "available_models": [{ "name": "claude-opus-4-7" }, { "name": "claude-sonnet-4-6" }]
  >   }
  > }
  > ```
  > Works today against the existing proxy — no code changes needed.

- [ ] **DEPLOY-C: model slug verification**
  > Before hardcoding in CLAUDE-B, confirm exact Anthropic model slugs via:
  > `curl https://api.anthropic.com/v1/models -H "x-api-key: $ANTHROPIC_API_KEY"`
  > Update `crates/api/src/providers/mod.rs` aliases (`"opus"`, `"sonnet"`) to match.
  > Currently set to `claude-opus-4-6` / `claude-sonnet-4-6` — update to `claude-opus-4-7`
  > if that slug is confirmed live.

---

## P3 — Future

- [ ] OSS-B: OpenViking — stand up Docker instance, ingest FKS docs/strategies, compare retrieval quality vs current HNSW
- [ ] CI/CD re-enable: move `ci-cd.yml` back to `.github/workflows/` after OpenClaw OOM fix + Tailscale second-device verification
- [ ] Split `crates/commands/src/lib.rs` (140K, skipped in pack as too large) — it's currently a single-file crate with no internal module structure; break into submodules by command group
- [ ] Split `crates/rusty-claude-cli/src/main.rs` (272K, skipped in pack) — same issue
- [ ] **MEM-E: memory dashboard** — `GET /api/v1/memory` endpoint listing stored entries with importance scores; `DELETE /api/v1/memory/:id` for manual pruning; expose in OpenWebUI via a custom tool
- [ ] **AGENT-E: agent persona memory** — after OSS-D (persona integration) is done, store persona-specific memories separately so the quantitative-analyst agent and rust-systems-engineer agent build independent knowledge bases
