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
  > `.env.example` and `ModelConfig` in `src/config.rs`.
  >
  > **Native SSE follow-up done 2026-05-25.** The Claude arm of
  > `handle_streaming` now pumps `AnthropicClient::stream_message` directly
  > instead of synthesising a single delta from a blocking `send_message`.
  > Each `StreamEvent::ContentBlockDelta::TextDelta` forwards as a
  > `StreamChunk::Delta`; `MessageStart`/`MessageDelta` accumulate model + usage;
  > `MessageStop` (or stream exhaustion) emits `StreamChunk::Done` via the new
  > `send_claude_done` helper, which keeps cache token counts honest by only
  > emitting `cache_creation_input_tokens` / `cache_read_input_tokens` when
  > Anthropic actually reported a nonzero count. Non-text deltas (InputJson,
  > Thinking, Signature) are dropped to match the non-streaming
  > `extract_text` contract. Channel buffer widened from 4 to 64 to absorb
  > real-stream burstiness without back-pressuring the reqwest chunk pump.

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
  >
  > **Wire-protocol follow-up done 2026-05-24.** The 2026-05-17 work only
  > attached the observability side (the `PromptCache` instance tracks cache
  > token counts surfaced by Anthropic). The actual `cache_control` markers
  > that *opt request blocks into* the cache were never emitted, so
  > `cache_read_input_tokens` was permanently zero. Fixed in
  > `crates/api/src/types.rs` (`SystemBlock`, `CacheControl`, `cache_control`
  > field on `InputContentBlock::Text`; `system: Option<String>` widened to
  > `Option<Vec<SystemBlock>>` to carry the marker) and
  > `src/api/proxy.rs::dispatch_claude` (system prompt gets
  > `cache_control: { type: "ephemeral" }` when it clears the 1024-token
  > minimum, estimated via a 4-chars-per-token heuristic so we don't waste a
  > cache slot on short prompts that wouldn't qualify). The
  > `prompt-caching-scope-2026-01-05` beta header was already in the default
  > `AnthropicRequestProfile`, so no header change was needed.
  >
  > **Streaming-path cache tokens done 2026-05-25.** `StreamChunk::Done`
  > grew `cache_creation_input_tokens: Option<u32>` and
  > `cache_read_input_tokens: Option<u32>` fields. The proxy's Claude
  > streaming arm populates them from `resp.usage`; the Ollama and Grok
  > arms emit `None`. The proxy's accumulator passes both through to the
  > `CachedProxyResponse` write so streamed responses now hit the local
  > cache with the same metadata the non-streaming path stores. Both
  > fields also ride out on the final SSE chunk as
  > `cache_creation_input_tokens` / `cache_read_input_tokens` extension
  > fields (skipped when `None` to keep non-Claude streams
  > OpenAI-shape-compatible), matching the non-streaming path's
  > `x_ra_metadata` exposure. The native-SSE follow-up (driving
  > `AnthropicClient::stream_message` instead of the single-delta
  > `send_message` synth) is still outstanding — see CLAUDE-A's note.

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
- [~] Make skip-extensions configurable per-repo — `skip_extensions: Vec<String>` in repo config struct; pass through `AutoScanner` and `StaticAnalysis` call sites
  > **Split into three PRs.** The TODO bullet understated the work: no
  > per-repo config struct existed, `AutoScanner` uses a hardcoded
  > `SKIP_SUFFIXES` (not the configurable global), and `StaticAnalyzer`
  > doesn't filter by extension at all.
  >
  > **PR A (2026-05-25) — data model + persistence + API.** Done.
  > `RegisteredRepo` gains `skip_extensions: Option<Vec<String>>` with
  > replace-semantics (per-repo *replaces* global, lets a repo opt back
  > in to globally-skipped extensions). Migration 024 adds `TEXT[] NULL`
  > column. `RegisterRepoRequest`/`get_repo` carry the field.
  > `RegisteredRepo::effective_skip_extensions(global_default)` returns
  > the override or the global as a slice without allocation. Unit
  > tests cover the None/Some(list)/Some(empty) three-way contract and
  > the in-memory register-then-get round trip.
  >
  > **PR B (2026-05-25) — wire into `AutoScanner`.** Done.
  > `should_skip_path_with` / `should_analyze_file_with` are new
  > variants accepting `Option<&[String]>`; the existing static
  > methods delegate with `None` so the legacy behaviour is
  > byte-for-byte unchanged (locked by
  > `should_skip_path_with_none_matches_static_method`). Stacked on
  > PR A's branch — the SQL helper references the `skip_extensions`
  > column from migration 024. The scanner resolves the override
  > once per scan via `fetch_skip_extensions_override` (one indexed
  > SELECT against `registered_repos` by `local_path`; treats the
  > lookup as advisory — never blocks a scan if the DB query
  > fails). The override is threaded through `get_changed_files`,
  > `get_files_from_recent_commits`, and
  > `analyze_changed_files_with_progress` to all five historical
  > `should_skip_path` / `should_analyze_file` call sites.
  > `SKIP_DIRS` is never overridable (always-skipped paths like
  > `node_modules/`, `target/`, `.git/`). The match accepts
  > extensions with or without a leading dot so per-repo (`"png"`)
  > and `SKIP_SUFFIXES` (`".png"`) representations both work.
  >
  > **PR C (after B) — wire into audit runner.** `AuditRunnerConfig` and
  > `FullAuditConfig` currently take `skip_extensions` from the global
  > scanner config; need a `for_repo(&RegisteredRepo, &ScannerConfig)`
  > overlay constructor that consults `effective_skip_extensions`. Then
  > update audit call sites that scan a registered repo.
- [~] Routing heuristic tuning — after CLAUDE-B deploys, measure Opus vs Sonnet classification quality and adjust `ModelRouter::llm_classify` system prompt; log `task_kind` per request to make this measurable
  > **Measurement prerequisite done 2026-05-25.** `src/api/proxy.rs` now
  > emits a structured `event = "proxy.dispatch"` log line at the end of
  > every successful request (cache hit, dispatch, and streaming `Done`
  > paths) with `task_kind`, `target` (`local`/`remote`/`claude`),
  > `model`, prompt / completion / cache token counts, `rag_chunks_used`,
  > `repo_context_injected`, `repo_id`, `cached`, `streaming`, and
  > `used_fallback`. Field names are stable surface for downstream
  > metrics — see `target_kind_label_emits_stable_strings_per_variant`.
  >
  > **Error-path symmetry done 2026-05-25.** `DispatchOutcome` now carries
  > an explicit `error: Option<String>` field (populated only by
  > `DispatchOutcome::error`), and `handle_chat_completions` branches on
  > it: on a backend failure it emits a matching `event =
  > "proxy.dispatch_error"` warn-level event (task_kind, target, model,
  > error, repo_id, streaming) and skips the cache write — poisoning the
  > cache with error responses would replay failures on every duplicate
  > request for the TTL. The streaming SSE pump emits the same event on
  > `StreamChunk::Error`. Downstream queries can compute
  > `dispatch_error_rate = count(proxy.dispatch_error) /
  > (count(proxy.dispatch) + count(proxy.dispatch_error))` grouped by
  > `task_kind`/`target`.
  >
  > Outstanding: aggregate the log stream once deployed (probably via
  > Loki/Promtail or a tail-and-roll-up Postgres job) and adjust the
  > classifier prompt based on observed misclassifications.
- [x] Add `ANTHROPIC_API_KEY`, `RC_PLANNER_MODEL`, `RC_EXECUTOR_MODEL` to `.env.example` and README config table
  > Already done. Verified 2026-05-23: `.env.example` has
  > `ANTHROPIC_API_KEY=` (uncommented), `# RC_PLANNER_MODEL=claude-opus-4-7`
  > and `# RC_EXECUTOR_MODEL=claude-sonnet-4-6` (commented overrides
  > showing defaults). README.md config table rows 73–75 document all
  > three with their roles (Recommended / Optional / Optional). No
  > code change needed.

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

- [~] **RC-CRATES-B: replace LLM call code with `api` crate**
  > `src/simple_client.rs` was already migrated — wraps `api::OpenAiCompatClient`.
  >
  > **`src/grok_client.rs` migrated 2026-05-19.** The heaviest raw-reqwest
  > caller now goes through `api::OpenAiCompatClient` configured for xAI.
  > Public surface unchanged — all 12 public methods (`ask`, `ask_tracked`,
  > `ask_with_context`, `score_file`, `analyze_repository`, `find_patterns`,
  > `quick_analysis`, plus the cost-tracking accessors and cache helpers)
  > keep their signatures. `call_api_once` lost its raw HTTP construction
  > (request builder, `Authorization: Bearer` header, `.json().await`)
  > in favour of `inner.send_message(&MessageRequest)`. The outer retry
  > loop in `call_api` stays — its job is to record one DB cost-tracking
  > row per *logical* attempt, separate from the api crate's per-request
  > transport retries. Dropped local types: `ChatCompletionRequest`,
  > `Message`, `ChatCompletionResponse`, `Choice`, and the
  > `GROK_API_BASE` constant. `Usage` stayed (i64-shaped wrapper around
  > `api::Usage`'s u32 fields so the DB schema is unchanged).
  >
  > **`src/llm/grok.rs` migrated 2026-05-19.** `GrokAnalyzer` now uses
  > `OpenAiCompatClient` (xAI). Public surface (`new`,
  > `tokens_used`, the `LlmAnalyzer` trait methods) is unchanged.
  > Local `GrokResponse` / `GrokChoice` / `GrokMessage` / `GrokUsage`
  > types deleted along with the `GROK_API_URL` constant — the api
  > crate's `MessageResponse` carries the same information.
  >
  > **Behavioural caveat (resolved 2026-05-20):** the previous
  > `call_grok(..., json_mode)` parameter set OpenAI's
  > `response_format: {"type": "json_object"}` on requests, but the
  > migrated path lost it. PR #25 added `ResponseFormat::{Text,JsonObject}`
  > and `MessageRequest::response_format` to the api crate;
  > `OpenAiCompatClient` translates the field into the OpenAI/xAI payload
  > and `AnthropicClient` strips it (Anthropic surfaces structured output
  > via tool use). `call_grok` now honors `json_mode` again by setting
  > `response_format: json_mode.then_some(ResponseFormat::JsonObject)`.
  >
  > **`src/model_router.rs` migrated 2026-05-20.** `llm_classify` (the
  > prompt classifier — the TODO originally mis-described this as a
  > `GET /api/tags` health-check, but it was actually a `POST /api/chat`
  > LLM call) now goes through `api::OpenAiCompatClient` pointing at
  > Ollama's OpenAI-compatible `/v1/chat/completions` endpoint. Dropped
  > five inline structs (`Req`, `Msg`, `Opts`, `Resp`, `RespMsg`).
  >
  > **Behavioural caveat (resolved 2026-05-20):** the old Ollama-native
  > payload set `options.temperature = 0.0` and `options.num_predict = 16`
  > on the classification request. `MessageRequest` exposes `max_tokens`
  > (kept as 16) but originally had no `temperature`. PR #24 added
  > `temperature: Option<f32>` to `api::MessageRequest` and `llm_classify`
  > now pins `temperature: Some(0.0)` again. Built an explicit 8 s
  > `tokio::time::timeout` to replace the old reqwest builder timeout, and
  > `with_retry_policy(0, ...)` to preserve one-shot semantics.
  >
  > **Still to migrate (with notes on each):**
  > 2. `src/grok_reasoning.rs` — uses xAI's `/responses` endpoint
  >    (different from `/chat/completions` that `OpenAiCompatClient`
  >    targets). Needs either a new `/responses` client in the api
  >    crate or migration to `/chat/completions` (behavioral change).
  >    **Not a clean drop-in migration.**
  > 5. `src/ollama_client.rs` — depends on Ollama-native features
  >    (`num_ctx` context-window control, NDJSON streaming format,
  >    `prompt_eval_count`/`eval_count` token fields) that aren't
  >    exposed by Ollama's OpenAI-compat endpoint. The TODO's
  >    "wire with Ollama base URL override" suggestion silently
  >    drops `num_ctx`. **Not a clean drop-in migration** — needs
  >    either api-crate Ollama-specific extensions or accepting
  >    feature loss.
  >
  > 3/5 migrations done. The remaining two (`grok_reasoning.rs`,
  > `ollama_client.rs`) both need additional api-crate capabilities first.

- [~] **RC-CRATES-C: replace scanner with `runtime` crate**
  > `runtime::execute_bash`, `runtime::ProviderClient`, `runtime::worker_boot` are the key
  > integration points. Start by replacing `src/tests_runner.rs` subprocess logic with
  > `runtime::execute_bash` — it handles sandboxing, timeout, and output capture already.
  >
  > **`src/tests_runner.rs` migrated 2026-05-21 (PR pending).** All 6
  > `std::process::Command` invocations across the Rust / Python / JS /
  > Kotlin test paths and the Rust / Python coverage paths now go through
  > `runtime::execute_bash`. Required a tiny additive API extension on
  > the runtime crate: `BashCommandInput` gained a `cwd: Option<PathBuf>`
  > field (serde-default, backward-compatible JSON shape) so the test
  > runner can target an audited project's root without mutating the
  > process-wide cwd. Each call site passes
  > `dangerously_disable_sandbox: Some(true)` because the test toolchain
  > needs real filesystem access. Path arguments shell-quoted via a
  > local POSIX single-quote helper. New `BashCommandOutput.stdout`
  > is already `String`, so the four `String::from_utf8_lossy(&output.stdout)`
  > sites became cheap `.clone()`s.
  >
  > **`src/auto_scanner.rs` migrated 2026-05-21 (PR pending).** Five
  > `Command::new("git")` invocations (`rev-parse HEAD`, `diff
  > --name-status`, `status --porcelain`, `diff --name-only HEAD~5
  > HEAD`, `ls-tree -r --name-only HEAD`) now go through
  > `runtime::execute_bash` via a local `run_git_in(repo_path, args)`
  > helper. Each call passes `cwd: Some(repo_path.to_path_buf())` to
  > target the audited repo and `dangerously_disable_sandbox: Some(true)`
  > to preserve the previous unsandboxed behaviour. The
  > `output.status.success()` checks all mapped to
  > `output.return_code_interpretation.is_none()` (None = exit 0). Five
  > `String::from_utf8_lossy(&output.stdout)` / `.stderr` calls
  > collapsed since `BashCommandOutput` already exposes both as
  > `String`. Path/hash args shell-quoted via a per-file POSIX
  > single-quote helper (duplicated from `tests_runner` per YAGNI; will
  > extract on third caller).
  >
  > **`src/git.rs` migrated 2026-05-21 (PR pending).** Five subprocess
  > sites — `clone_repo`, `clone_repo_with_token`, `clone_with_askpass`,
  > `push_with_askpass`, `push_branch_with_token` — now go through
  > `runtime::execute_bash`. The previous `Command::new("git").env(...)`
  > pattern is replaced by an inline `KEY=val git ...` shell prefix via
  > a new local helper `build_git_command(env, args)`; this avoids
  > extending the runtime API with an `env` field for now (YAGNI — no
  > other migrated caller has needed it yet). Two pre-existing
  > `String::from_utf8_lossy(line.content())` sites in `get_diff` are
  > unrelated to subprocess output (they're inside a `git2::Diff` print
  > callback) and were left untouched.
  >
  > Behavioural note: the previous code used `Command::status()` which
  > inherits stdio, so `git clone`'s progress streamed to the parent
  > terminal. `execute_bash` captures stdout/stderr, so the progress is
  > no longer interactively visible. Acceptable for the audit/CLI
  > context; on error the captured `return_code_interpretation` surfaces
  > in the returned `AuditError`.
  >
  > **`src/formatter.rs` migrated 2026-05-21 (PR pending).** Eight
  > subprocess sites — four `is_available()` version probes
  > (`cargo fmt --version`, `ktlint --version`, `npx prettier --version`,
  > `black --version`) and four format functions (`format_rust`,
  > `format_kotlin`, `format_prettier`, `format_python`) — now go through
  > `runtime::execute_bash`. The version probes use a tiny `run_succeeds`
  > helper; the format functions build the command string with file path
  > lists shell-quoted (any future path with spaces or shell
  > metacharacters survives `sh -lc` parsing). `format_rust` swapped
  > the previous `cmd.current_dir(cargo_dir)` pattern for `cwd:
  > Some(cargo_dir)` on the `BashCommandInput`. The two pre-existing
  > `String::from_utf8_lossy(&output.stdout)` / `&output.stderr` sites
  > collapsed to direct field access.
  >
  > **`src/task_executor.rs` migrated 2026-05-21 (PR pending).** All
  > 10 `Command::new("git")` invocations in the agent task pipeline
  > consolidated through the existing module-local `run_git(cwd, args)`
  > helper, which now routes through `runtime::execute_bash`. Added a
  > sibling `run_git_allow_fail(cwd, args)` to cover the three
  > `git commit -m ...` sites where "nothing to commit" is expected
  > and the caller still wants to push (previously open-coded as
  > `let _ = Command::new(...)`). The three `checkout -b` and three
  > `push -u <auth_url> <branch>` sites all collapse to one-line
  > helper calls inside their existing `tokio::task::spawn_blocking`
  > closures. `cwd` field on `BashCommandInput` replaces the previous
  > `Command::new("git").arg("-C").arg(cwd)` pattern; the
  > `GIT_TERMINAL_PROMPT=0` env var is inlined as a shell prefix the
  > same way `src/git.rs` does it. The captured stderr now surfaces in
  > the error message on non-zero exit (small improvement over the
  > previous `"git checkout -b failed"` no-context message).
  >
  > Behavioural note: same as PR #34 — the previous `Command::status()`
  > inherited stdio, so `git push`/`git checkout` progress streamed
  > to the parent terminal. `execute_bash` captures it, so live progress
  > is gone. Acceptable for the agent task pipeline (no human watching);
  > stderr surfaces on error.
  >
  > **`shell_quote` deduplicated 2026-05-21 (PR pending).** The
  > byte-identical local `fn shell_quote` that PRs #32, #33, #34, #35,
  > and #36 each added in `src/{tests_runner,auto_scanner,git,formatter,
  > task_executor}.rs` is now `pub fn runtime::shell_quote`, re-exported
  > via `crates/runtime/src/lib.rs`. The five duplicates removed; all
  > five callers now import `runtime::shell_quote` alongside the other
  > runtime types they already use. Added three lock-in tests in the
  > runtime crate: basic single-quote wrapping, embedded-single-quote
  > escape, and a `sh -lc` round-trip with a deliberately dangerous
  > shell injection payload that verifies the quoting prevents
  > metacharacter interpretation.
  >
  > **`src/backup/mod.rs` migrated 2026-05-21 (PR pending).** Nine
  > subprocess sites — eight `rclone` invocations (`version`,
  > `listremotes`, `copy`, `size --json`, `lsf --dirs-only`, `purge`,
  > `lsjson --dirs-only`, plus the restore-side `copy`) and one
  > `sqlite3 .backup` invocation — now go through `runtime::execute_bash`
  > via a local `run_command(command)` helper (no cwd; rclone/sqlite3
  > calls use absolute paths). All remote paths / DB paths / dot-commands
  > shell-quoted via `runtime::shell_quote`. The sqlite3 `.backup` site
  > needed a small adaptation: the existing code already builds a
  > sqlite-dot-command string `.backup '<path>'`, so we shell-quote
  > that whole string a second time on the way to `sh -lc` (sqlite3
  > then sees the inner quotes as part of its own dot-command parsing).
  > `String::from_utf8_lossy(&output.stdout)` parsing dropped to direct
  > field access on `output.stdout`; `serde_json::from_slice(&output.stdout)`
  > became `serde_json::from_str(&output.stdout)`.
  >
  > **`src/repo/manager.rs` migrated 2026-05-21 (PR pending).** Six
  > subprocess `git` invocations (`clone --depth=1`, `pull --rebase`
  > × 2, `rev-parse HEAD`, `status --porcelain`, `rev-parse
  > --abbrev-ref HEAD`) now go through `runtime::execute_bash` via
  > local `build_git_command(args)` + `run_git_command(command, cwd)`
  > helpers (same shape as `src/git.rs` from PR #34). Five of the six
  > sites use `cwd: Some(repo_path)` instead of the previous
  > `git -C <path>` invocation; the clone site has no `cwd` (target
  > path is built as an absolute path). All path/URL args shell-quoted;
  > `GIT_TERMINAL_PROMPT=0` set as an inline `KEY=val git ...` shell
  > prefix. Three `String::from_utf8_lossy(&output.stderr)` sites and
  > two `String::from_utf8_lossy(&output.stdout).trim().to_string()`
  > sites collapsed to direct field access on the now-`String` fields.
  >
  > **`src/{tree_state,code_review,db/config,bin/cli}.rs` migrated
  > 2026-05-21 (PR pending).** Bundled four small-caller migrations
  > into one PR (6 sites total) since the per-file pattern is uniform.
  > - `tree_state.rs` (2 sites) — `git rev-parse HEAD` + `git rev-parse
  >   --abbrev-ref HEAD` for fetching commit/branch info. Both wrapped
  >   in a tiny local closure since the call sites use `.ok()` /
  >   `.filter()` chains rather than explicit status checks.
  > - `code_review.rs` (2 sites) — `git diff --name-status [branch]`
  >   and `git diff --numstat [branch] -- <file>`. Branch + file args
  >   shell-quoted; one `run_in(cwd, command)` helper for both.
  > - `db/config.rs` (1 site) — `pg_dump --format=custom --file <path>
  >   <db_url>` for the backup helper. The `DATABASE_URL` may contain a
  >   password, so the URL is shell-quoted; the previous `Command::new`
  >   argv path was inert against shell metacharacters.
  > - `bin/cli.rs` (1 site) — `cargo check` after a CLI write batch,
  >   with `SQLX_OFFLINE=true` + `RUSTFLAGS='-A warnings'` env vars
  >   inlined as a shell prefix (same pattern as PRs #34/#36/#40).
  >
  > **`src/{static_analysis,context/global,agent/tools}.rs` migrated
  > 2026-05-21 (PR pending) — `Command::new` rollout in `src/` complete.**
  > Five subprocess sites:
  > - `static_analysis::run_clippy` (`cargo clippy --message-format=json
  >   --all-targets --quiet`) — `async fn`, wrapped in
  >   `tokio::task::spawn_blocking` since `execute_bash` is sync.
  > - `static_analysis::check_file_staleness` (`git log -1 --format=%ct
  >   -- <file>`) — sync, direct call.
  > - `context::global::build_diff_context` — two `git log --since
  >   <h>hours --oneline` and `git diff --stat 'HEAD@{<h>hours ago}'`
  >   calls; the second site quotes the `HEAD@{...}` ref since the
  >   spaces inside the brace would otherwise split on `sh -lc`.
  > - `agent::tools::op_run_command` — the agent's user-controlled
  >   "run arbitrary command" tool. Caller-supplied `command` + `args`
  >   are shell-quoted into a single `GIT_TERMINAL_PROMPT=0 <cmd> <args>`
  >   string. Wrapped in `spawn_blocking` like `run_clippy`. The
  >   `ToolError::CommandFailed { status: i32 }` field is preserved by
  >   parsing `return_code_interpretation = Some("exit_code:N")` back
  >   to an int (fall back to -1 on timeout / unparseable strings).
  >
  > `Command::new` / `process::Command` callers in `src/`: **none**.
  > Remaining `runtime` integration points: `ProviderClient`,
  > `worker_boot` (entry-point wiring, not subprocess migration). The
  > executor pieces gated on RC-CRATES-C by lines 242 and 356 above are
  > fully unblocked.

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

- [x] **RC-CRATES-G: integration test suite covering both Claude and Grok paths**
  > `crates/mock-anthropic-service` is the test harness — already a dev-dep of rusty-claude-cli.
  > Add integration tests in `crates/rusty-claude-cli/tests/` that:
  > 1. Full round-trip through `api::AnthropicClient` (primary path after CLAUDE-A) ✓
  > 2. Two-tier routing: verify Opus is selected for `ArchitecturalReason`, Sonnet for `ScaffoldStub` ✓
  > 3. `api::OpenAiCompatClient` with xAI base URL (Grok) — kept as fallback path ✓
  > 4. Prompt cache hit: send same request twice, assert no second network call ✓
  >
  > **2026-05-19**: Repaired `tests/test_grok_integration.rs` against the current api crate
  > (stale `response_cache` path, missing `complete()` method, `ClawApi*` → `Anthropic*`,
  > `opus` alias now `claude-opus-4-7`). Trimmed bonus test to registered aliases.
  >
  > **2026-05-19** (follow-through): Added the four remaining tests.
  > - `crates/rusty-claude-cli/tests/api_integration.rs` — three new live-mock tests:
  >   `anthropic_client_round_trips_through_mock_service`,
  >   `openai_compat_client_consumes_openai_shaped_response` (against an inline xAI-shaped
  >   TCP mock since the workspace mock is Anthropic-shaped), and
  >   `prompt_cache_short_circuits_second_identical_request` (asserts mock observes one
  >   request even though `send_message` is called twice).
  > - `tests/test_grok_integration.rs::test_model_router_two_tier_claude_routing_planner_vs_executor`
  >   — with `anthropic_enabled=true`, asserts `ArchitecturalReason`/`CodeReview`/`Unknown` →
  >   `ClaudeTier::Planner` + `claude-opus-4-7`, all other kinds → `ClaudeTier::Executor` +
  >   `claude-sonnet-4-6`.
  > - Drive-by: fixed pre-existing compile break in
  >   `crates/rusty-claude-cli/src/app/streaming.rs` — `MessageStopEvent` was simplified to an
  >   empty struct but the inline test still used `{ index: 0 }`.

---

## P1 — src/ Cleanup (RC-CLEANUP)

> Identified during 2026-04-05 code review. ~82K LoC in a 48-module god crate.
> Items ordered by risk/reward — do before the crate extractions in P2.

- [x] **RC-CLEANUP-A: consolidate LLM clients into `src/llm/`**
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
  >
  > **Slice 1 done 2026-05-21 (PR pending).** Consolidated `simple_client`:
  > the migrated top-level orphan `src/simple_client.rs` (an `api`-crate
  > wrapper that was never declared in `lib.rs`) replaced the raw-reqwest
  > `src/llm/simple_client.rs`, dropping ~50 lines of manual HTTP plumbing
  > from the actually-used `crate::llm::GrokClient` path. Public surface
  > preserved: `new`, `from_env`, `with_model`, `generate`. Default model
  > kept at `grok-4.1` (no behaviour change vs. shipping production).
  > Unused additions from the orphan (`complete`, `chat`) dropped per YAGNI.
  >
  > **Slice 2 done 2026-05-21 (PR pending).** Moved `src/token_budget.rs`
  > → `src/llm/usage/budget.rs` and `src/cost_tracker.rs` →
  > `src/llm/usage/costs.rs` via `git mv` (history preserved). Added
  > `src/llm/usage/mod.rs` with `pub mod {budget, costs};` and registered
  > `pub mod usage;` in `src/llm/mod.rs`. The two `pub mod token_budget;`
  > / `pub mod cost_tracker;` lines in `lib.rs` are gone; the three
  > `lib.rs` re-exports (`CostTracker` + friends at top level, `TokenPricing`
  > + friends at top level, and the `prelude` re-export of `CostTracker`)
  > now point at `llm::usage::{costs,budget}`. Three in-tree importers
  > rewritten: `auto_scanner.rs`, `repo/file_cache.rs` (two refs), and
  > `repo/cache.rs`. The crate-root `rustcode::CostTracker` /
  > `rustcode::TokenPricing` re-exports are unchanged, so any external
  > callers via the flat name still work.
  >
  > **CostTracker cache-token split done 2026-05-21 (PR pending).**
  > `TokenUsage` gained `cache_creation_input_tokens: u64` and
  > `cache_read_input_tokens: u64` fields (the names mirror
  > `api::Usage` and `runtime::TokenUsage` for consistency with the
  > Anthropic wire shape). The legacy `cached_tokens` field stays as a
  > backward-compat aggregate for non-Anthropic providers (Grok prompt
  > cache) and for old rows in `llm_costs`. Schema migrated via two
  > idempotent `ALTER TABLE ... ADD COLUMN IF NOT EXISTS` statements;
  > the `log_call` INSERT now binds all three cache counters and
  > `get_stats_for_period` exposes the split via two new
  > `CostStats.total_cache_*` fields. `calculate_cost` now prices
  > cache writes at 1.25× the input rate and cache reads at 0.1× (the
  > Anthropic pricing model); the legacy `cached_tokens` path keeps
  > its previous Grok pricing. New `From<api::Usage>` impl gives a
  > one-line conversion for any Anthropic response. Two new unit
  > tests verify the conversion preserves the split and that
  > `cache_creation` cost dwarfs `cache_read` cost.
  >
  > **Slice 3 done 2026-05-21 (PR pending).** Moved `src/llm_config.rs`
  > → `src/llm/config.rs` via `git mv`. Updated `src/llm/mod.rs` to add
  > `pub mod config;`, dropped `pub mod llm_config;` from `lib.rs` and
  > rewrote the `pub use llm_config::{LlmConfig, CacheConfig, ...}`
  > re-export through `llm::config`. Three in-tree importers rewritten:
  > `src/cache/audit.rs` (6 refs — function sig + 5 test cases),
  > `src/grok_reasoning.rs`, `src/llm_audit.rs`. Crate-root re-exports
  > (`rustcode::LlmConfig`, etc.) keep flat names.
  >
  > **Slice 4 done 2026-05-21 (PR pending).** Moved `src/model_router.rs`
  > → `src/llm/router.rs` via `git mv`. Updated `src/llm/mod.rs` to add
  > `pub mod router;`, dropped `pub mod model_router;` from `lib.rs`
  > (no top-level re-export existed). Four in-tree importers rewritten:
  > `src/server.rs`, `src/api/proxy.rs`, `src/api/repos.rs` (2 refs), and
  > `tests/test_grok_integration.rs` (3 refs — `rustcode::model_router::*`
  > → `rustcode::llm::router::*`). Updated the moved file's leading path
  > comment.
  >
  > **Slice 5 done 2026-05-21 (PR pending).** Moved `src/ollama_client.rs`
  > → `src/llm/ollama.rs` via `git mv`. Updated `src/llm/mod.rs` to add
  > `pub mod ollama;`, dropped `pub mod ollama_client;` from `lib.rs`
  > (no top-level re-export existed). Three in-tree importers rewritten:
  > `src/api/repos.rs` (2 refs — `OllamaClient`), `src/api/proxy.rs`
  > (1 ref — `StreamChunk`). Updated two stale `model_router` comments
  > in the moved file to point at `llm::router`. The `OllamaClient` is
  > still raw `reqwest::Client`; migrating to `api::OpenAiCompatClient`
  > requires a new `ollama()` factory in the api crate (no `bearer_auth`
  > on the no-auth Ollama path) and is left for a follow-up.
  >
  > **Slice 6 done 2026-05-21 (PR pending) — RC-CLEANUP-A complete.**
  > Moved `src/grok_client.rs` (669 lines) → `src/llm/grok_client.rs`
  > and `src/grok_reasoning.rs` (1,513 lines) → `src/llm/grok_reasoning.rs`
  > via `git mv`. Both `pub mod` declarations dropped from `lib.rs`;
  > the four re-exports (top-level + prelude, for both modules)
  > rewritten through `llm::{grok_client,grok_reasoning}::*`. Crate-root
  > re-exports (`rustcode::GrokClient`, `rustcode::FileScoreResult`,
  > `rustcode::QuickAnalysisResult`, `rustcode::GrokReasoningClient`,
  > etc.) keep their flat names so external/test consumers are unaffected.
  >
  > 16 in-tree importer files rewritten by script (~25 individual refs):
  > `server.rs`, `test_generator.rs`, `refactor_assistant.rs`,
  > `code_review.rs`, `auto_scanner.rs`, `llm/ollama.rs`,
  > `audit/{full_audit,endpoint,runner}.rs`, `api/{repos,proxy}.rs`,
  > `bin/cli.rs`, `todo/{planner,scaffolder,worker}.rs`, plus a
  > doc-comment example in `llm/grok_client.rs` itself.
  >
  > **`lib.rs` module count: 48 → 40 ✓** (matches TODO target).
  >
  > Deferred to a separate PR: the "unified client" design (`llm/client.rs`
  > wrapping `api::AnthropicClient` + `api::OpenAiCompatClient`) and the
  > optional split of `cache_creation_tokens` / `cache_read_tokens` in
  > `CostTracker`. These are design decisions that warrant their own
  > thinking time rather than being folded into a pure-restructure pass.

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

- [~] **RC-CLEANUP-C: remove the old file-based repo cache**
  > **Structural half done 2026-05-19.** Five top-level repo files
  > consolidated under `src/repo/`:
  >   - `src/repo_cache_sql.rs`  → `src/repo/cache.rs` (canonical)
  >   - `src/repo_cache.rs`      → `src/repo/file_cache.rs` (legacy, kept for now)
  >   - `src/repo_manager.rs`    → `src/repo/manager.rs`
  >   - `src/repo_sync.rs`       → `src/repo/sync.rs`
  >   - `src/repo_analysis.rs`   → `src/repo/analysis.rs`
  >
  > New `src/repo/mod.rs` declares the five submodules. `lib.rs`
  > drops 5 top-level `pub mod`s, gains 1 (`repo`). External public
  > API (`rustcode::{RepoCache, RepoCacheSql, RepoSyncService,
  > RepoAnalyzer, ...}`) is preserved by re-routing all top-level
  > `pub use` lines through the new submodule paths. 30 internal
  > callsites across 9 files updated in one bulk pass.
  >
  > **Deletion deferred.** The file-based `RepoCache` lives at
  > `src/repo/file_cache.rs` for now. Removing it cleanly needs:
  > (a) verification the SQL path is stable in production;
  > (b) extracting the shared `CacheType` / `RepoCacheEntry` types
  > out of the file-based version so the SQL version stops
  > importing from it (today `repo::cache.rs` imports
  > `crate::repo::file_cache::{CacheType, ...}` for the enum);
  > (c) deleting `src/cache/migrate.rs` since its sole job —
  > moving data from file-based to SQL — becomes vacuous;
  > (d) deciding what to do with `src/bin/cli.rs`'s
  > `cache init` subcommand — it calls `RepoCache::new` to lay
  > down the file-cache directory structure. Either remove the
  > subcommand or migrate it to no-op the file path. **2026-05-19
  > follow-up fixed cli.rs's stale `rustcode::repo_cache::*` and
  > `rustcode::repo_cache_sql::*` import paths (broken by the
  > structural move); the cli still functions but now goes through
  > the top-level `rustcode::{CacheType, RepoCache, RepoCacheSql}`
  > re-exports and `rustcode::repo::cache::CacheSetParams`.**
  > Easier as a focused follow-up.

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

- [~] **RC-EXTRACT-A: `crates/rag` — semantic indexing pipeline**
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
  >
  > **Slice 1 done 2026-05-23 (PR pending).** Created `crates/rag/` with
  > the two dependency-leaf modules: `chunking.rs` (739 LOC, no `crate::`
  > deps) and `vector_index.rs` (712 LOC, no `crate::` deps). Cargo.toml
  > pulls just `anyhow + bincode + rand + serde + serde_json + tracing`
  > (no fastembed / ort-sys yet — that lives with the deferred slice).
  > Workspace member added; rustcode depends on `rag = { path = "crates/rag" }`.
  > Two in-tree importers rewritten: `src/indexing.rs` → `use rag::*`,
  > `src/research/worker.rs` → `use rag::vector_index::*`. The three
  > `lib.rs` re-exports (top-level `chunking::*`, top-level `vector_index::*`,
  > prelude `chunking::*`) now go through `rag::*`, so crate-root names
  > (`rustcode::ChunkConfig`, `rustcode::VectorIndex`, etc.) keep working
  > for external consumers. The 17 unit tests previously in
  > `src/{chunking,vector_index}.rs` ride along into `crates/rag` and all
  > pass.
  >
  > **Slice 2 done 2026-05-23 (PR pending).** Moved `src/embeddings.rs`
  > (427 LOC) into `crates/rag/src/embeddings.rs` via `git mv`. The
  > `rag` Cargo.toml gains `fastembed` + `tokio` (workspace deps);
  > `rag/src/lib.rs` adds `pub mod embeddings;` and re-exports the
  > five public types (`Embedding`, `EmbeddingConfig`, `EmbeddingGenerator`,
  > `EmbeddingModelType`, `EmbeddingStats`). The two `rustcode::lib.rs`
  > re-exports (top-level and prelude) now route through `rag::` — the
  > flat `rustcode::EmbeddingGenerator` name keeps working for the
  > prelude consumers. Ten in-tree importers rewritten by script
  > (`crate::embeddings::*` → `rag::*`): `research/worker.rs`,
  > `search.rs`, `api/{jobs,handlers,mod}.rs`, `server.rs`,
  > `indexing.rs`, `repo/sync.rs`, `memory/{mod,store}.rs`. The new
  > `rag` crate now inherits the same `ort-sys` CDN sandbox restriction
  > that gates rustcode compile; CI verifies both.
  >
  > **Slice 4 done 2026-05-23 (PR pending).** Moved the bulk of
  > `src/code_chunker.rs` (~2,149 LOC of chunking logic) into
  > `crates/rag/src/code_chunker.rs`, with a 67-line shim left at
  > `src/code_chunker.rs` that:
  >   - `pub use rag::code_chunker::*;` to keep historical paths
  >     (`crate::code_chunker::CodeChunk`, `rustcode::CodeChunker`,
  >     etc.) and the existing `lib.rs` re-exports working unchanged.
  >   - Hosts the three `CodeChunk → ChunkRecord` conversion helpers
  >     (`chunk_to_record`, `chunk_to_location`, `chunks_to_records`),
  >     which can't live in `rag` because they reference
  >     `crate::db::chunks::ChunkRecord` — putting them in `rag` would
  >     force a `rag → rustcode::db` circular dep. The existing comment
  >     block already noted this constraint.
  >
  > Also lifted `FileLanguage` out of `src/static_analysis.rs` into
  > the new `rag::file_language` module (so both `code_chunker` and
  > `static_analysis` consume one source of truth). `src/static_analysis.rs`
  > now does `pub use rag::FileLanguage;` so the historical path
  > `crate::static_analysis::FileLanguage` keeps working for the one
  > in-tree caller (`src/prompt_tier.rs`) and any external rustcode users.
  > Added workspace deps to `rag`: `regex` + `sha2` (needed by the
  > moved chunker logic).
  >
  > Remaining slice:
  > - **Slice 3**: extract a `Storage` trait that `src/db/chunks` and
  >   the embedding-store fns implement, then move `src/indexing.rs`
  >   and `src/search.rs` over. This is the gnarly one (DB trait
  >   design + cascading updates).

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
