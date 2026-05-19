# rustcode — TODO

> **Repo:** `github.com/nuniesmith/rustcode`
> **Last synced from master todo:** 2026-04-03
> **RC-CRATES-A resolved:** 2026-04-05
> **Code review completed:** 2026-04-05 — see notes below each relevant item
> **RC-CRATES-D plugin migration prep completed:** 2026-04-06 — three tools migrated to bundled plugins
> **Architecture review completed:** 2026-04-07 — Claude provider wiring, two-tier routing, agent loop, and memory system added
> **CLAUDE-A/B/C resolved:** 2026-05-17 — Claude wired into `dispatch()`, two-tier routing live, prompt cache attached

---

## P0 — Wire Claude as Primary Provider

> `AnthropicClient` in `crates/api/src/providers/anthropic.rs` is fully built (retry, streaming,
> OAuth, prompt-cache tracking) but is **never called from the proxy hot path** — everything
> still dispatches to Grok via `GrokClient`. These three items wire it in. Everything below
> (planner loop, agent memory, cost reduction) depends on this being done first.

- [x] **CLAUDE-A: add `ModelTarget::Claude` and wire `AnthropicClient` into `dispatch()`**
  > **Done 2026-05-17.** `ModelTarget::Claude { model, tier }` added to
  > `src/model_router.rs`; `dispatch()` in `src/api/proxy.rs` now has a third arm
  > (`dispatch_claude`) that calls `AnthropicClient::send_message()` and translates
  > the `MessageResponse` back into the proxy's `DispatchOutcome`. `RepoAppState`
  > carries an `Option<Arc<AnthropicClient>>` built once at startup, preserving the
  > attached `PromptCache` across requests. `route_from_model_field` now picks the
  > right Claude tier from `claude-*` slugs. `ANTHROPIC_API_KEY` was added to
  > `.env.example` and `ModelConfig` in `src/config.rs`. Streaming dispatch
  > synthesises a single-delta stream from `send_message` (native SSE via
  > `AnthropicClient::stream_message` left as follow-up).

- [x] **CLAUDE-B: two-tier routing — Opus 4.7 (planner) vs Sonnet 4.6 (executor)**
  > **Done 2026-05-17.** `ClaudeTier { Planner, Executor }` and `TaskKind::tier()`
  > land in `src/model_router.rs`. `ModelRouterConfig` gained `planner_model`,
  > `executor_model`, and `anthropic_enabled` fields; defaults are `claude-opus-4-7`
  > / `claude-sonnet-4-6`. `route()` now picks the Claude target when
  > `anthropic_enabled` is true. Aliases in `crates/api/src/providers/mod.rs`
  > were updated so `"opus"` → `"claude-opus-4-7"`. New tests cover both tier
  > selection paths (`claude_planner_for_review_and_architecture`,
  > `claude_executor_for_scaffold`, `task_kind_tier_mapping`).
  > Outstanding: verify the exact Opus slug live (see DEPLOY-C) before shipping.

- [x] **CLAUDE-C: enable Anthropic prompt caching in the proxy dispatch**
  > **Done 2026-05-17.** `AnthropicClient` is constructed once at startup via
  > `.with_prompt_cache(PromptCache::new("rustcode-proxy"))` and shared through
  > `RepoAppState`. `RaMetadata` now carries `cache_creation_input_tokens` and
  > `cache_read_input_tokens` (skip-serialized when `None`), populated from
  > `MessageResponse::usage`. `CachedProxyResponse` round-trips both fields so
  > cache hits also reflect actual savings.
  > Outstanding: streaming path does not yet surface cache token counts (would
  > require plumbing usage observation through `StreamChunk`).

---

## P0 — Async Task Agent Pipeline

> The core new capability: drop a JSON file → rustcode picks it up, does the work, opens a PR.

- [x] **TASK-A:** Implement `tasks/` directory watcher — `notify` crate, debounced, ignore `.tmp` files
  > **Done.** `src/task_watcher.rs` polls the `tasks/` directory every 500 ms, skips `.tmp`
  > files, validates each `.json` against `TaskFile::from_file`, and pushes accepted
  > tasks through an mpsc channel. Spawned from `server.rs` when
  > `config.task_watcher.enabled` is true.
- [x] **TASK-B:** Define task file schema (JSON): `id`, `repo`, `description`, `steps[]`, `branch`, `labels[]`, `auto_merge` bool
  > **Done.** `src/task/schema.rs` defines `TaskFile`, `TaskResult`, and `StepResult`
  > with serde + a `validate()` method that enforces alphanumeric ids, `owner/repo`
  > format, non-empty steps, etc.
  > Naming-collision cleanup (`src/tasks.rs` vs `src/task/`) is tracked separately under
  > RC-CLEANUP-D.
- [~] **TASK-C:** Task executor — for each step, call appropriate tool (LLM scaffold, file create/edit, run tests, commit)
  > **Partial 2026-05-17.** `TaskExecutor::execute_real` clones, branches, writes one
  > placeholder file per step, commits, runs the per-language test runner against the
  > working tree, pushes, and opens the PR. The placeholder files are still literal
  > "Step: ..." text; **LLM-driven step execution is the remaining gap** and will
  > likely be done by wiring `AnthropicClient` tool-use with `write_file` / `edit_file`
  > / `run_command` tools once `AGENT-A` lands.
- [x] **TASK-C:** Per-language test runner: `cargo check` / `cargo test` for Rust; `pytest -x` for Python; `npm run build` for TS
  > **Done.** `task_executor::run_tests_for_workspace` calls `TestRunner::detect_project_types`
  > then `run_tests_for_type` for each detected type. Aggregated pass/fail flag drives
  > the abort decision before push; the human-readable summary is attached to the last
  > `StepResult::test_output` and the PR body.
- [~] **TASK-D:** GitHub PR creation — use existing `github::client` to: create branch, push commits, open PR with task description + step log
  > **Partial 2026-05-17.** PR creation + label application now live in `execute_real`.
  > `GitHubClient::add_labels` was added and is invoked after PR open.
- [x] **TASK-D:** Auto-merge logic: if all CI checks pass and `auto_merge: true`, merge the PR
  > **Done 2026-05-18.**
  > - `src/task/automerge.rs::poll_and_merge` polls the PR's combined
  >   CI status every `poll_interval` (default 15s) up to a `timeout`
  >   (default 10 minutes), returning a `MergeState` enum
  >   (`Merged | NeedsReview | Timeout | MergeFailed`).
  > - On `state == "success"` → calls `merge_pull_request` with the
  >   configured `merge_method` (default `"squash"`).
  > - On `state == "failure" | "error"` → calls `add_labels` with the
  >   configured `failure_label` (default `"needs-review"`).
  >   **Completes the failure-handling half of TASK-F** that was
  >   marked partial in PR #2.
  > - Refetches the PR each iteration so it always looks at the
  >   latest head SHA (handles users pushing follow-up commits).
  > - **Background execution:** `spawn_auto_merge` fires
  >   `tokio::spawn` after PR creation in all three executor paths
  >   (`materialize_and_push`, `run_agent_tool_phases`,
  >   `execute_real`). Watcher returns the `TaskResult` immediately;
  >   the poller updates `tasks/results/{id}.json` in place once CI
  >   settles, atomic-rename via temp file.
  > - `TaskResult` schema additions:
  >   - `auto_merge_requested: bool` (copied from `task.auto_merge`)
  >   - `merge_state: Option<MergeState>` (filled by the poller)
  >   Both are `#[serde(default, skip_serializing_if = ...)]` for
  >   backward compat with old result files.
  > - **Configurable via env:**
  >   - `RC_AUTOMERGE_POLL_SECS` (default 15)
  >   - `RC_AUTOMERGE_TIMEOUT_SECS` (default 600)
  >   - `RC_AUTOMERGE_METHOD` (default `squash`)
  >   - `RC_AUTOMERGE_FAILURE_LABEL` (default `needs-review`)
  > - **Tests:** 4 new unit tests cover `AutoMergeConfig::default()`,
  >   `MergeState` serde shape (`kind: "merged" | "needs_review"`),
  >   `MergeState` round-trip, and
  >   `update_result_with_merge_state` patching exactly the
  >   `merge_state` field of an on-disk JSON file (all other fields
  >   preserved verbatim).

- [x] **TASK-F:** Failed task handling — if any step fails or tests fail, tag PR `needs-review` and write error details to result file; never auto-merge
  > **Done 2026-05-18** (the second half — tagging the PR
  > `needs-review` when CI fails post-push — landed with TASK-D
  > above). Writing the result file on every code path landed in
  > PR #2. Never auto-merging on failure is enforced by the
  > `match state { "failure" | "error" => add_labels ... }` arm in
  > `poll_and_merge` — the merge branch only runs on `"success"`.
- [x] **TASK-E:** Task result file — write `tasks/results/{id}.json` with outcome, PR URL, test results, any errors
  > **Done.** `write_result_file` is now the single sink; both `execute_dry_run` and
  > `execute_real` go through it on every code path (success and failure).
- [~] **TASK-F:** Failed task handling — if any step fails or tests fail, tag PR `needs-review` and write error details to result file; never auto-merge
  > **Partial 2026-05-17.** Failure now always produces a result file with `status = "failed"`
  > and the error message. Auto-merge isn't wired yet (see TASK-D) so there's no risk of
  > merging on failure. **Remaining:** if a PR was already opened before tests failed (e.g.
  > we push first, then a separate CI run reports failure), apply a `needs-review` label —
  > today we abort before push when tests fail, so the only way to get here is via the
  > auto-merge poller, which lands with the rest of TASK-D.

---

## P1 — Planner-Executor-Reviewer Agent Loop

> Implements the three-phase agent cycle: Opus plans → Sonnet executes → Opus reviews → repeat.
> Prerequisite: CLAUDE-A and CLAUDE-B must be done first.
> The `ProjectPlan` and `ProjectPhase` structs already exist in `src/llm/grok.rs` — reuse them.

- [x] **AGENT-A: `src/agent/pipeline.rs` — `AgentPipeline` struct with three phases**
  > **Done 2026-05-17.** `src/agent/` now contains `pipeline.rs` + `types.rs`:
  > - `AgentPipeline` holds two `Arc<AnthropicClient>` (planner + executor) plus the
  >   model slugs to target. Either client can be the same `Arc` — the per-request
  >   `model` slug is what drives Opus vs Sonnet routing.
  > - `plan()`, `execute()`, `review()`, and `run()` are implemented. `run()` carries
  >   reviewer critique into the next plan so revisions are informed.
  > - Phase prompts ask for strict JSON; `strip_to_json` repairs ```json fences and
  >   leading prose; parse failures surface as `PhaseError::Parse` with a 200-char
  >   excerpt of the raw response.
  > - `ReviewOutcome` is `Approved { summary } | Revise { critique, suggestions }`
  >   (serde-tagged so it round-trips through JSON for the result file).
  > - `PipelineResult` records every iteration (plan + step results + review) plus
  >   a `converged: bool` flag distinguishing approval from "hit max_iterations".
  >
  > **Memory injection (MEM-B) is the documented next wiring point** — the planner
  > and executor methods already accept all the context they'd need, so once
  > `AgentMemory::search(...)` lands it's a one-line prepend in the user prompt.
  > **Tool use (AGENT-D) is the other deferred piece** — the executor's `output` is
  > raw assistant text today; switching it to structured tool calls is additive.

  > **Ride-along compile fix:** PR #1's `proxy.rs` / `repos.rs` / `server.rs` were
  > importing through `::api::providers::...` / `::api::prompt_cache::...` /
  > `::api::types::...`, but those are private modules in the `api` crate. The
  > top-level re-exports (`::api::AnthropicClient`, `::api::PromptCache`, etc.)
  > are the supported path; the sandbox's `ort-sys` CDN block hid the latent
  > compile error from local cargo runs. All three files now use the public path.

- [x] **AGENT-B: `POST /v1/agent/run` endpoint**
  > **Done 2026-05-17.** Wired into the `/v1` proxy router alongside `/chat/completions`.
  > - `src/api/agent.rs::handle_agent_run` accepts either an `AgentRunRequest`
  >   (`{description, context?, max_iterations?}`) or a verbatim task file
  >   (extra fields `repo`/`branch`/`labels`/`auto_merge` are silently ignored).
  > - Auth: same bearer-token check as `/chat/completions` (calls
  >   `ProxyState::is_authorised` and emits the same `OaiError` shape on 401/400).
  > - Streaming: every `PipelineEvent` from `AgentPipeline::run_streaming` is
  >   mapped to an SSE `data:` frame. Frames carry a `kind` discriminator
  >   (`iteration_started`, `plan_completed`, `step_started`, `step_completed`,
  >   `review_completed`, `pipeline_completed`, `error`). A 15-second keepalive
  >   ping is attached so proxies don't drop long-lived runs.
  > - `max_iterations` is clamped to `MAX_ALLOWED_ITERATIONS = 6` to cap cost.
  > - When the pipeline returns `Err`, a final `kind: "error"` frame is emitted
  >   with the failing phase (`planner` / `executor` / `reviewer`) before the
  >   stream closes.
  >
  > Added `AgentPipeline::run_streaming(task, max, sender)` and `PipelineEvent`
  > to the agent crate. `run()` now delegates to a private `run_internal` that
  > optionally emits events, so the existing non-streaming callers (the task
  > watcher) keep working unchanged.

- [x] **AGENT-C: wire `AgentPipeline` into the task file watcher (TASK-C)**
  > **Done 2026-05-17.** `TaskExecutor::execute_with_agent` runs the pipeline
  > first, then only proceeds with clone/materialize/push/PR when the reviewer
  > approves and `converged == true`. When the pipeline doesn't converge (max
  > iterations exhausted while still revising), a `status = "failed"` result is
  > written with the critique in `error` and the full trace in `agent_trace`.
  > The PR description embeds the agent's approval summary, iteration count,
  > and test summary.
  >
  > Wiring lives in `src/server.rs`'s task-watcher block:
  >   - if `dry_run` → `execute_dry_run`
  >   - else if `agent_pipeline` is built → `execute_with_agent` (works with
  >     or without a `GITHUB_TOKEN`; without one we persist the trace but
  >     skip the push + PR)
  >   - else if `GITHUB_TOKEN` set → fall back to `execute_real`
  >   - else → degrade to `execute_dry_run`
  >
  > `TaskResult.agent_trace: Option<PipelineResult>` is the new field carrying
  > the full plan + step outputs + review across every iteration; it's
  > `#[serde(default, skip_serializing_if = "Option::is_none")]` so existing
  > result files keep working.

- [x] **AGENT-D: per-step tool execution inside the executor phase**
  > **Done 2026-05-17.**
  > - `src/agent/tools.rs` defines a `ToolBackend` trait with one default impl,
  >   `FileSystemTools`, scoped to a sandbox root. It exposes `write_file`,
  >   `edit_file`, `read_file`, and (opt-in) `run_command`. Absolute paths and
  >   `..` traversals return `PathEscape`; outputs are truncated past 16 KiB.
  > - `AgentPipeline` gained `run_with_tools` and `run_streaming_with_tools`.
  >   The executor phase switches into Anthropic tool use when a backend is
  >   present: each step alternates `tool_use`/`tool_result` turns up to
  >   `MAX_TOOL_ITERATIONS_PER_STEP = 12` before falling out as a step failure.
  > - `StepExecutionResult.tool_calls: Vec<ToolCallRecord>` records every
  >   invocation (input, status, truncated output) for the result file trace.
  > - `TaskExecutor::execute_with_agent_tools` is the new watcher entry point:
  >   clones first, then runs the pipeline with `FileSystemTools` rooted at
  >   the clone (with `run_command` enabled — the working tree is throwaway),
  >   commits the agent's edits, runs the per-language test suite, pushes,
  >   opens the PR with the tool-call count in the body. `execute_with_agent`
  >   stays for the no-token / text-only fallback path.
  > - `src/server.rs` dispatch now prefers `execute_with_agent_tools` when
  >   both an agent and `GITHUB_TOKEN` are available.
  >
  > **Deferred:** the TODO calls for `runtime::execute_bash` /
  > `plugins::PluginLifecycle` integration; that's gated on RC-CRATES-C and
  > RC-CRATES-D. For now `FileSystemTools` runs commands directly via
  > `tokio::process` rooted at the workspace, which is enough to drive
  > `cargo`, `pytest`, `npm`, and other build / test runners. Replacing the
  > backend with the runtime-crate sandbox is a one-line trait swap.
  > **Also deferred:** auto-running the test runner *between* file-modifying
  > steps. Today we run it once after the pipeline converges; per-step would
  > require parsing tool calls to detect "this step touched files" — a
  > follow-up.

---

## P1 — Agent Memory System

> Persistent cross-session memory that accumulates knowledge about your projects, preferences,
> and patterns over time. Injected into the system prompt before each LLM call to give agents
> context without re-reading the full codebase. This is the "personalization" layer.
> `fastembed` is already in workspace deps — use it for memory embeddings.

- [x] **MEM-A: `src/memory/store.rs` — `AgentMemory` backed by Postgres**
  > **Done 2026-05-17.** Ended up on Postgres rather than SQLite as the
  > original spec called for — the rest of the project shares a `PgPool`
  > via `AppState` and embeddings already live alongside
  > `document_embeddings` in the same schema, so keeping memory in
  > Postgres avoided introducing a second store.
  > - `sql/023_agent_memory.sql` defines the `agent_memory` table with
  >   indexes on `project`, `kind`, `importance DESC`, `created_at DESC`.
  >   Embeddings are stored as JSON TEXT (matching the
  >   `document_embeddings` convention) so no pgvector dependency is
  >   needed for the row volumes we expect (a few thousand per project).
  > - `src/memory/types.rs` defines `MemoryKind`,
  >   `MemoryEntry { id, project, kind, content, embedding, importance,
  >   created_at, last_accessed, access_count }`, `NewMemory` (builder
  >   payload), `MemorySearchHit`, and a free `cosine_similarity` helper
  >   used during ranking.
  > - `src/memory/store.rs` provides `AgentMemory::record`,
  >   `AgentMemory::search`, `AgentMemory::list`, `AgentMemory::count`,
  >   `AgentMemory::delete`, and `AgentMemory::touch`. Search scopes to
  >   `project IS NULL OR project = $1`, fetches up to
  >   `MAX_CANDIDATES = 4096` rows ordered by `importance DESC`,
  >   re-ranks in Rust by `cosine * importance`, returns top-k, and
  >   bumps `access_count`/`last_accessed` for each returned hit.
  > - Unit tests in `types.rs` cover cosine identity / orthogonality /
  >   opposite / mismatched-length / zero-vector / empty edge cases plus
  >   `MemoryKind` round-trip via `as_db_str` / `from_db_str` / serde.
  >
  > Not yet wired into `AppState` / `RepoAppState` — MEM-B will pick
  > that up. Today `AgentMemory::new(pool, Arc<EmbeddingGenerator>)` is
  > the constructor; callers thread their own embedder through.

- [x] **MEM-B: inject memories into every LLM call**
  > **Done 2026-05-17.**
  > - `AgentMemory` is constructed at server startup when
  >   `RC_MEMORY_INJECTION` is not `false` (default: on) and shared via
  >   `RepoAppState.agent_memory`. The embedder loads lazily on first use,
  >   so memory wiring has no startup cost when nothing queries it.
  > - **Proxy hot path:** `dispatch_claude` runs `memory.search(user_prompt,
  >   None, 5)` and prepends `format_memories_for_prompt(&hits)` to the
  >   user message of the `MessageRequest`. Memory failures degrade to
  >   "no memory" silently — a `search` error doesn't fail the request.
  > - **AgentPipeline:** `with_memory(memory, top_k)` builder attaches the
  >   store to the pipeline. Per-call project scope reads from
  >   `AgentTask::memory_scope` (a new `#[serde(default)] Option<String>`
  >   field on `AgentTask`), so one pipeline can serve tasks across
  >   multiple repos. The planner searches against `task.description`;
  >   each executor step searches against `step.description`. Both
  >   prepend a `[Memory]` block to the user message; the system prompts
  >   stay constant for prompt-cache friendliness.
  > - **Watcher integration:** `build_agent_task` sets
  >   `memory_scope = Some(task.repo)` so memory lookups are
  >   project-scoped (plus globals). The `task_executor` watcher path
  >   and the SSE endpoint both attach memory when building their
  >   `AgentPipeline`.
  >
  > `RC_MEMORY_INJECTION=false` is the kill switch for benchmarking the
  > no-memory baseline. New unit tests cover the formatter (empty, all
  > five kinds, content trimming) and the `with_memory_scope` builder.
  > `serde(default)` on the new `memory_scope` field keeps old task-file
  > payloads + result files round-tripping cleanly.

- [x] **MEM-C: session consolidation — extract and store memories after each session**
  > **Done 2026-05-17.**
  > - `AgentPipeline::consolidate_session(&result)` is the new public
  >   method. It serializes the trace (task + per-iteration plan +
  >   step outputs + review verdicts) as JSON, sends it to Sonnet with
  >   a strict-JSON extraction prompt, parses the response into
  >   `ExtractedMemory { kind, content, importance }` records, and
  >   writes each via `AgentMemory::record(NewMemory)`. Project scope
  >   for the new entries comes from `result.task.memory_scope`.
  > - `with_memory` now flips on `consolidation_enabled` by default —
  >   opting in to memory injection opts in to keeping the store fed.
  >   `without_consolidation()` opts out (benchmarking the no-learning
  >   baseline).
  > - **Auto-invocation:** after a successful pipeline run inside
  >   `run_internal`, when consolidation is enabled, we spawn a
  >   `tokio::spawn` background task that calls
  >   `consolidate_session(&final_result)`. SSE streams close
  >   promptly; the watcher returns the `TaskResult` immediately;
  >   consolidation happens out-of-band. Failures inside consolidation
  >   are logged but never propagated — the pipeline already
  >   succeeded.
  > - **Kill switch:** `RC_MEMORY_CONSOLIDATION=false` flips both the
  >   watcher- and SSE-built pipelines to `without_consolidation()`.
  > - **Parser:** `parse_consolidation` strips ```json fences and
  >   leading prose, filters out entries with empty `content` (since
  >   `AgentMemory::record` rejects those), and surfaces malformed
  >   JSON / unknown `kind` values as `PhaseError::Parse`.
  > - **Tests:** 8 new unit tests cover the parser end-to-end —
  >   empty arrays, all five kinds, optional `importance`, fence
  >   stripping, prose stripping, empty-content filtering, parse-error
  >   surface, and unknown-kind rejection.
  >
  > **Deferred:** the TODO mentioned reusing
  > `runtime::summary_compression` for a pre-extraction summary step.
  > That's gated on RC-CRATES-C (runtime crate integration). Today
  > the full trace JSON is truncated to 16 KiB before being handed to
  > Sonnet — enough for normal runs; the summary-compression step
  > would let us handle very long traces.

- [x] **MEM-D: importance scoring + pruning**
  > **Done 2026-05-17.**
  > - Access tracking already lands in MEM-A: every `AgentMemory::search`
  >   call ends with a `touch_many` that bumps `access_count` and
  >   `last_accessed` for every returned hit. No change needed here.
  > - `AgentMemory::prune(&PruneConfig)` is the new three-phase pass:
  >   1. **Decay** (pure SQL) — entries with `access_count == 0` and
  >      `created_at < NOW() - decay_age_days` have their importance
  >      lowered to `decay_to`.
  >   2. **Delete** (pure SQL) — entries with
  >      `importance < delete_importance_below` AND
  >      `created_at < NOW() - delete_age_days` are removed.
  >   3. **Dedupe** (per-project Rust loop) — for each project scope
  >      independently, find pairs with cosine ≥ `dedupe_similarity`
  >      and keep the higher-importance entry (ties broken by older
  >      `last_accessed`). Capped at `MAX_CANDIDATES` per scope.
  > - `PruneConfig::default()` matches the TODO spec: 30-day decay
  >   window, 90-day delete window, 0.1 importance floor, 0.95 cosine
  >   dedupe threshold.
  > - `PruneReport { decayed, deleted, merged }` returned from every
  >   call.
  > - **Manual trigger:** `POST /api/v1/memory/prune` accepts an
  >   optional body to override individual `PruneConfig` fields and
  >   returns the report. Returns 503 when memory isn't configured.
  > - **Nightly cron:** `server.rs` spawns a `tokio::time::interval`
  >   loop that calls `prune(&PruneConfig::default())` every
  >   `RC_MEMORY_PRUNE_INTERVAL_SECS` (default 86400 = 24h). Skips
  >   the first tick so boot isn't slammed.
  > - **Tests:** 7 new unit tests on the in-memory dedupe logic
  >   (duplicates collapse to higher importance, dissimilar pairs not
  >   flagged, tie-broken-by-last-accessed, multiple near-duplicates
  >   chain correctly, empty/single inputs, defaults match TODO spec,
  >   report totals). Plus 3 endpoint tests on the `PruneRequest`
  >   override semantics.

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

- [x] **RC-CLEANUP-B: consolidate cache modules into `src/cache/`**
  > **Done 2026-05-18.** Pure restructure — no behaviour changes.
  > Four top-level files moved under `src/cache/`:
  > - `src/cache.rs` → `src/cache/audit.rs`
  > - `src/cache_layer.rs` → `src/cache/layer.rs`
  > - `src/response_cache.rs` → `src/cache/responses.rs`
  > - `src/cache_migrate.rs` → `src/cache/migrate.rs`
  >
  > `src/cache/mod.rs` declares the four submodules and re-exports the
  > `audit` module's public API at the module root so existing
  > `use crate::cache::{AuditCache, CacheEntry, CACHE_DIR}` callers
  > (in `tree_state.rs`, `grok_reasoning.rs`, `llm_audit.rs`) keep
  > working without changes. The `CacheStats` naming collision (all
  > three of audit/layer/responses define one) is resolved by
  > re-exporting only `audit::CacheStats` at the root; the other two
  > are reached via submodule paths.
  >
  > Top-level `pub use` lines in `src/lib.rs` updated to use the new
  > `cache::layer::*`, `cache::responses::*`, `cache::migrate::*`
  > paths — external API (`rustcode::CacheLayer`,
  > `rustcode::ResponseCache`, etc.) is unchanged.
  >
  > In-source callers (`grok_client.rs`, `query_router.rs`,
  > `api/repos.rs`) updated to import from the new paths.
  >
  > `lib.rs` loses 3 top-level mods (`cache_layer`, `cache_migrate`,
  > `response_cache`), gains 0 (already had `pub mod cache`).

- [ ] **RC-CLEANUP-C: remove the old file-based repo cache**
  > `src/repo_cache.rs` is the original file-based implementation.
  > `src/repo_cache_sql.rs` is the SQL replacement.
  > `src/cache_migrate.rs` exists to move data between them.
  > If the SQL path is stable, delete `src/repo_cache.rs`, rename `src/repo_cache_sql.rs`
  > to `src/repo/cache.rs`, and move `src/repo_manager.rs`, `src/repo_sync.rs`,
  > `src/repo_analysis.rs` alongside it into `src/repo/`.

- [x] **RC-CLEANUP-D: resolve task/todo naming collisions**
  > **Done 2026-05-18.** Two halves landed across two PRs.
  >
  > **Tasks half (earlier PR):** `src/tasks.rs` (audit-driven
  > `TaskGenerator`) moved to `src/audit/tasks.rs` where it
  > conceptually belongs. The top-level
  > `pub use rustcode::TaskGenerator` is preserved as a shim
  > re-exporting from the new location. `crate::tasks` is no
  > longer a module — `src/task/` (DB-backed task management) is
  > the only `task*` module at the top level now.
  >
  > **Todos half (this PR):** Structural cleanup option chosen
  > over full API migration. `src/todo_scanner.rs` moved to
  > `src/todo/legacy_scanner.rs` (`git mv` preserves history). The
  > top-level `pub mod todo_scanner` declaration in `src/lib.rs`
  > is gone; the top-level `pub use rustcode::{TodoItem,
  > TodoPriority, TodoScanner, TodoSummary}` re-export now flows
  > through `todo::legacy_scanner`, keeping the external API
  > unchanged. The new module path is intentionally NOT
  > re-exported at the `crate::todo` root because `TodoItem`
  > would collide with `todo::todo_file::TodoItem` (different
  > shape — checkbox/section vs. file/line/text). Internal callers
  > (`scoring.rs`, `static_analysis.rs`, `auto_scanner.rs`)
  > updated to `crate::todo::legacy_scanner::*`.
  >
  > **Still deferred:** full API migration from the legacy
  > scanner to `todo::scanner` (`TodoCommentScanner` etc.). That
  > requires field-level adaptation of the three callers
  > (`TodoItem.category` doesn't exist on `TodoCommentItem`) and
  > deserves its own focused PR.

- [x] **RC-CLEANUP-E: consolidate context modules into `src/context/`**
  > **Done 2026-05-19.** `git mv`s preserve history.
  > - `src/context_llm.rs` → `src/context/global.rs` (1016 lines)
  > - `src/context_rag.rs` → `src/context/rag.rs` (553 lines)
  > - New `src/context/mod.rs` declares the two submodules with
  >   docstrings calling out the global-vs-RAG distinction.
  >
  > Callsites updated to the new paths (`crate::context::global::*`
  > and `crate::context::rag::*`):
  > - `src/query_router.rs`
  > - `src/grok_client.rs` (3 method signatures)
  > - `src/types.rs`
  > - `src/scanner/enhanced.rs` (post-cleanup path; see below)
  > - `src/lib.rs` deprecated re-exports block
  >
  > `lib.rs` loses two top-level `pub mod`s (`context_llm`,
  > `context_rag`).

- [x] **RC-CLEANUP-F: move integration test file out of `src/`**
  > **Done 2026-05-19.** `src/test_grok_integration.rs` moved to
  > `tests/test_grok_integration.rs` (`git mv` preserves history).
  > The file was already gated by `#![cfg(feature = "integration")]`
  > but had no Cargo wiring — it sat as an orphan and a stray
  > `pub mod` candidate. `Cargo.toml` now defines the `integration`
  > feature alongside `clipboard`, plus a `[[test]] name =
  > "test_grok_integration"` entry with
  > `required-features = ["integration"]` so a plain `cargo test`
  > continues to skip it.
  >
  > **Bonus:** also folded `src/enhanced_scanner.rs` (360 lines,
  > only the deprecated re-exports block referenced it) into
  > `src/scanner/enhanced.rs`. `src/scanner/mod.rs` declares the
  > new submodule and re-exports `EnhancedScanner` so the external
  > public API (`rustcode::EnhancedScanner`) is unchanged.

- [x] **RC-CLEANUP-G: rename `prompt_router.rs` to avoid confusion with `query_router.rs`**
  > **Done 2026-05-18.** `git mv src/prompt_router.rs src/prompt_tier.rs`
  > preserves history. Three use sites updated: `src/lib.rs`
  > (`pub mod`, top-level `pub use`, deprecated re-exports block) and
  > `src/auto_scanner.rs` (`use crate::prompt_tier::{PromptRouter, TierKind}`).
  > The struct name `PromptRouter` is unchanged — only the module path
  > is `prompt_tier` now, so the file name and the type's role
  > (routing files to prompt tiers) match.

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
- [x] **MEM-E: memory dashboard** — `GET /api/v1/memory` endpoint listing stored entries with importance scores; `DELETE /api/v1/memory/:id` for manual pruning; expose in OpenWebUI via a custom tool
  > **Done 2026-05-18.**
  > - `GET /api/v1/memory?project=&kind=&limit=` returns
  >   `{total, entries: [MemoryEntryView]}`. The view strips the
  >   `embedding` field (large + useless to dashboard consumers); all
  >   other fields including `importance`, `access_count`,
  >   `last_accessed` are exposed. `limit` is clamped to `[1, 500]`
  >   with a default of 50.
  > - `DELETE /api/v1/memory/:id` removes a single entry by UUID,
  >   returning `{deleted: true|false, id}` (404 when the id was
  >   unknown).
  > - Both endpoints reuse `AgentMemory::list` / `delete` and return
  >   503 with a structured error body when memory isn't configured
  >   (matching the `POST /memory/prune` behaviour).
  > - Routes registered alongside `/memory/prune` in `repo_router`
  >   so they all sit under `/api/v1/memory*` and inherit the
  >   standard bearer-token auth middleware.
  > - **Tests:** 4 new unit tests cover the view's embedding-strip,
  >   limit clamping (oversize + negative), and `ListQuery` serde
  >   round-trip.
  > - **OpenWebUI tool:** out of scope for this PR (config-only).
- [ ] **AGENT-E: agent persona memory** — after OSS-D (persona integration) is done, store persona-specific memories separately so the quantitative-analyst agent and rust-systems-engineer agent build independent knowledge bases
