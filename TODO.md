# rustcode — TODO

> **Repo:** `github.com/nuniesmith/rustcode`
> **Last synced from master todo:** 2026-04-03
> **RC-CRATES-A resolved:** 2026-04-05

---

## P0 — Async Task Agent Pipeline

> The core new capability: drop a JSON file → rustcode picks it up, does the work, opens a PR.

- [ ] **TASK-A:** Implement `tasks/` directory watcher — `notify` crate, debounced, ignore `.tmp` files
  > `notify = "6"` dep is already in `src/rc/Cargo.toml`. Entry point: add `src/task_watcher.rs`,
  > spawn it from `server.rs` via `tokio::spawn`. Debounce with a 500ms window using `tokio::time::sleep`.
- [ ] **TASK-B:** Define task file schema (JSON): `id`, `repo`, `description`, `steps[]`, `branch`, `labels[]`, `auto_merge` bool
  > Add `src/task/schema.rs` with serde structs. Wire `Deserialize` only — task files are read-only input.
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
  > src/simple_client.rs is already migrated -- it wraps api::OpenAiCompatClient.
  > Remaining raw-reqwest LLM callers to migrate, in priority order:
  > 1. src/grok_client.rs — heaviest user; replace reqwest::Client with api::OpenAiCompatClient
  > 2. src/grok_reasoning.rs — uses GrokReasoningClient; same pattern
  > 3. src/model_router.rs — uses reqwest for Ollama health-check + Grok fallback
  > 4. src/ollama_client.rs — wire api::OpenAiCompatClient with Ollama base URL override

- [ ] **RC-CRATES-C: replace scanner with `runtime` crate**
  > runtime::execute_bash, runtime::ConversationClient, runtime::worker_boot are the key
  > integration points. Start by replacing src/tests_runner.rs subprocess logic with
  > runtime::execute_bash — it handles sandboxing, timeout, and output capture already.

- [ ] **RC-CRATES-D: wire `tools` + `plugins` for tool execution**
  > tools::AgentOutput and tools::detect_lane_completion are now exported.
  > Next: wire plugins::PluginLifecycle into the task executor (TASK-C) so that
  > bundled plugins (example-bundled, sample-hooks) run pre/post hooks around each step.

- [ ] **RC-CRATES-E: `--server` flag for MCP-style tool endpoint on :3501**
  > Add --mcp-server flag to src/bin/server.rs. When set, start a second Axum
  > listener on port 3501 serving runtime::mcp_tool_bridge.
  > The claw binary Login/Logout subcommands also need runtime OAuth wiring
  > (stubs left in crates/rusty-claude-cli/src/main.rs with TODO comments).

- [ ] **RC-CRATES-F: `claw-cli` binary in Docker image**
  > crates/rusty-claude-cli builds the claw binary. Add a second stage to the
  > rustcode Dockerfile copying target/release/claw into the image.
  > Verify: `docker run rustcode claw --help`

- [ ] **RC-CRATES-G: Grok integration test suite; validate then switch to Claude API**
  > crates/mock-anthropic-service is the test harness — already a dev-dep of rusty-claude-cli.
  > Add integration tests in crates/rusty-claude-cli/tests/ that:
  > 1. Full round-trip through api::AnthropicClient
  > 2. api::OpenAiCompatClient with xAI base URL (Grok)
  > 3. Migrate grok_client.rs to api crate, re-run tests to confirm parity

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

- [ ] OSS-D: Adapt FKS agent personas (quantitative-analyst, rust-systems-engineer, devops-engineer, trading-ui-developer) into `prompt_router.rs` system prompt templates for task-based routing

---

## P3 — Future

- [ ] Consider workspace split: `rc-core`, `rc-api`, `rc-rag`, `rc-llm` — 81K LoC single crate
- [ ] OSS-B: OpenViking — stand up Docker instance, ingest FKS docs/strategies, compare retrieval quality vs current HNSW
- [ ] CI/CD re-enable: move `ci-cd.yml` back to `.github/workflows/` after OpenClaw OOM fix + Tailscale second-device verification
