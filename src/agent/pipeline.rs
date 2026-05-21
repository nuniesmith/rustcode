// Three-phase agent loop: Opus plans → Sonnet executes → Opus reviews,
// repeating up to `max_iterations` while the reviewer asks for revisions.
//
// The pipeline holds two `AnthropicClient`s — one configured for the planner
// model (Opus 4.7) and one for the executor model (Sonnet 4.6). Both clients
// share a `PromptCache` so the system prompt and task description get the
// `cache_control: ephemeral` treatment across iterations.
//
// Phase prompts are tuned to elicit strict JSON output. We do best-effort
// repair on the response (strip ```json fences, trim whitespace) and parse;
// a parse failure surfaces as a step/review error rather than panicking.
//
// What this module does NOT do (intentional, follow-up work):
//   - Tool use. The executor's step output is text only. Wiring file edits,
//     bash, and search is AGENT-D.
//   - Memory injection. Once `AgentMemory` (MEM-A) lands, the planner +
//     executor prompts should call `memory.search(...)` and prepend the
//     top-k entries. This module leaves a hook (`memories: &[String]`) so
//     MEM-B can be a one-line wiring change.
//   - SSE streaming. `run()` is one shot; the AGENT-B endpoint will wrap
//     this with `stream_message` once we want incremental UI.

use ::api::{
    AnthropicClient, InputContentBlock, InputMessage, MessageRequest, MessageResponse,
    OutputContentBlock, ToolResultContentBlock,
};
use serde_json::Value;
use std::sync::Arc;
use tracing::{debug, info, warn};

use super::tools::{ToolBackend, ToolCallRecord, ToolCallStatus};
use super::types::{
    AgentTask, Plan, PipelineIteration, PipelineResult, PlanStep, ReviewOutcome,
    StepExecutionResult, StepStatus,
};

/// Default cap on the planner→execute→review iteration count.
pub const DEFAULT_MAX_ITERATIONS: u32 = 3;

/// Max tokens we'll ask either model to generate per phase. Generous enough
/// for multi-step plans / detailed reviews; small enough to bound cost.
const PLANNER_MAX_TOKENS: u32 = 4096;
const EXECUTOR_MAX_TOKENS: u32 = 4096;
const REVIEWER_MAX_TOKENS: u32 = 2048;

/// Cap on tool-use turns within a single step. The model alternates between
/// `tool_use` and `tool_result` messages until it produces a pure text turn;
/// this stops a malformed plan from looping indefinitely on read_file.
const MAX_TOOL_ITERATIONS_PER_STEP: u32 = 12;

/// Default number of memory entries to retrieve per LLM call when memory
/// injection is enabled. Five is enough to bring real prior context into a
/// task without saturating the prompt with marginal hits.
pub const DEFAULT_MEMORY_TOP_K: usize = 5;

/// Max tokens budgeted for the consolidation response (Sonnet returning a
/// JSON array of memory entries). 2 KiB easily fits dozens of entries.
const CONSOLIDATION_MAX_TOKENS: u32 = 2048;

/// Cap on the serialized trace we hand to the consolidation prompt. Beyond
/// this we truncate — the consolidator only needs the gist, not the full
/// step output.
const CONSOLIDATION_TRACE_BUDGET: usize = 16_000;

#[derive(Debug, Clone)]
pub struct AgentPipeline {
    planner: Arc<AnthropicClient>,
    executor: Arc<AnthropicClient>,
    planner_model: String,
    executor_model: String,
    /// Optional memory store. When attached, the planner and executor
    /// prompts get a `[Memory]` block prepended with the top-k matches
    /// for the task description / current step. The project scope for
    /// each call is read from `AgentTask::memory_scope` so a single
    /// pipeline can serve tasks across multiple repos.
    memory: Option<Arc<crate::memory::AgentMemory>>,
    /// Top-k memories retrieved per LLM call.
    memory_top_k: usize,
    /// When true and `memory` is `Some`, every successful pipeline run
    /// spawns a background task that asks Sonnet to extract durable
    /// memories from the trace and writes them via `AgentMemory::record`.
    /// Disabled by default at construction; flipped on by `with_memory`
    /// so callers that opt in to memory injection also get consolidation
    /// for free. Call `without_consolidation` to turn it back off (useful
    /// for benchmarking the no-learning baseline).
    consolidation_enabled: bool,
}

impl AgentPipeline {
    /// Construct a pipeline from two pre-built clients and the model slugs
    /// they should target.
    ///
    /// Callers typically build a single `AnthropicClient` at startup (see
    /// `server.rs`) and pass it as both `planner` and `executor` — the
    /// per-request `model` slug is what actually drives which Claude tier
    /// runs the call.
    #[must_use]
    pub fn new(
        planner: Arc<AnthropicClient>,
        executor: Arc<AnthropicClient>,
        planner_model: impl Into<String>,
        executor_model: impl Into<String>,
    ) -> Self {
        Self {
            planner,
            executor,
            planner_model: planner_model.into(),
            executor_model: executor_model.into(),
            memory: None,
            memory_top_k: DEFAULT_MEMORY_TOP_K,
            consolidation_enabled: false,
        }
    }

    /// Attach a persistent agent memory store. When set, planner and
    /// executor prompts get a `[Memory]` block prepended with up to
    /// `top_k` matches. Per-call project scope comes from
    /// `AgentTask::memory_scope`, so a single pipeline can serve tasks
    /// across multiple repos.
    ///
    /// Memory injection is best-effort: a `search` failure logs a warning
    /// and the pipeline proceeds with the unmodified prompt.
    #[must_use]
    pub fn with_memory(
        mut self,
        memory: Arc<crate::memory::AgentMemory>,
        top_k: usize,
    ) -> Self {
        self.memory = Some(memory);
        self.memory_top_k = top_k.max(1);
        // Default on: opting in to memory injection also opts in to the
        // session consolidation that keeps the store fed. Call
        // `without_consolidation` immediately after if you want injection
        // without the post-run extraction LLM call.
        self.consolidation_enabled = true;
        self
    }

    /// Disable session consolidation. Memory injection (`search` at call
    /// time) still works; only the post-run extraction-and-record loop
    /// is skipped. Useful for benchmarking the no-learning baseline.
    #[must_use]
    pub fn without_consolidation(mut self) -> Self {
        self.consolidation_enabled = false;
        self
    }

    // Best-effort memory lookup. Returns the formatted `[Memory]` block
    // (possibly empty) ready to prepend to a prompt. `scope` is typically
    // `task.memory_scope.as_deref()`.
    async fn fetch_memory_block(&self, query: &str, scope: Option<&str>) -> String {
        let Some(memory) = self.memory.as_ref() else {
            return String::new();
        };
        match memory.search(query, scope, self.memory_top_k).await {
            Ok(hits) if !hits.is_empty() => crate::memory::format_memories_for_prompt(&hits),
            Ok(_) => String::new(),
            Err(e) => {
                warn!(error = %e, "Agent pipeline: memory.search failed — continuing without it");
                String::new()
            }
        }
    }

    /// Run plan → execute → review repeatedly until the reviewer approves or
    /// we hit `max_iterations`. A `max_iterations` of 0 is treated as 1
    /// (always at least one pass).
    pub async fn run(
        &self,
        task: AgentTask,
        max_iterations: u32,
    ) -> Result<PipelineResult, AgentPipelineError> {
        self.run_internal(task, max_iterations, None).await
    }

    /// Same as `run`, but emits structured `PipelineEvent`s through the
    /// provided channel as the pipeline progresses. Used by the
    /// `POST /v1/agent/run` SSE endpoint so the client can see plan / step /
    /// review events in real time.
    ///
    /// The returned future resolves with the final `PipelineResult` once all
    /// iterations complete (or one fails). The channel is closed implicitly
    /// when the sender is dropped on return.
    pub async fn run_streaming(
        &self,
        task: AgentTask,
        max_iterations: u32,
        events: tokio::sync::mpsc::Sender<PipelineEvent>,
    ) -> Result<PipelineResult, AgentPipelineError> {
        self.run_internal(task, max_iterations, Some(events), None)
            .await
    }

    /// Run the pipeline with a tool backend attached to the executor.
    ///
    /// When tools are present, each executor step uses Anthropic tool use:
    /// Sonnet may emit `tool_use` blocks which are dispatched to
    /// `tools.execute(...)`, the results are sent back as `tool_result`
    /// blocks, and the loop continues until the model produces a pure
    /// text turn (or hits `MAX_TOOL_ITERATIONS_PER_STEP`).
    pub async fn run_with_tools(
        &self,
        task: AgentTask,
        max_iterations: u32,
        tools: &dyn ToolBackend,
    ) -> Result<PipelineResult, AgentPipelineError> {
        self.run_internal(task, max_iterations, None, Some(tools))
            .await
    }

    /// Same as `run_with_tools` but also emits `PipelineEvent`s through
    /// the provided channel for SSE streaming.
    pub async fn run_streaming_with_tools(
        &self,
        task: AgentTask,
        max_iterations: u32,
        events: tokio::sync::mpsc::Sender<PipelineEvent>,
        tools: &dyn ToolBackend,
    ) -> Result<PipelineResult, AgentPipelineError> {
        self.run_internal(task, max_iterations, Some(events), Some(tools))
            .await
    }

    async fn run_internal(
        &self,
        task: AgentTask,
        max_iterations: u32,
        events: Option<tokio::sync::mpsc::Sender<PipelineEvent>>,
        tools: Option<&dyn ToolBackend>,
    ) -> Result<PipelineResult, AgentPipelineError> {
        let max = max_iterations.max(1);
        let mut iterations: Vec<PipelineIteration> = Vec::with_capacity(max as usize);
        let mut critique_carry: Option<(String, Vec<String>)> = None;

        for iter in 1..=max {
            info!(
                iteration = iter,
                max, task = %task.description, "Agent pipeline: starting iteration"
            );
            if let Some(tx) = events.as_ref() {
                let _ = tx
                    .send(PipelineEvent::IterationStarted { iteration: iter, max })
                    .await;
            }

            let plan = self
                .plan(&task, critique_carry.as_ref())
                .await
                .map_err(|e| AgentPipelineError::Planner(e.to_string()))?;
            debug!(steps = plan.steps.len(), "Planner emitted plan");
            if let Some(tx) = events.as_ref() {
                let _ = tx
                    .send(PipelineEvent::PlanCompleted {
                        iteration: iter,
                        plan: plan.clone(),
                    })
                    .await;
            }

            let step_results = self
                .execute_internal(
                    &plan,
                    task.context.as_deref(),
                    task.memory_scope.as_deref(),
                    iter,
                    events.as_ref(),
                    tools,
                )
                .await
                .map_err(|e| AgentPipelineError::Executor(e.to_string()))?;
            debug!(
                completed = step_results.iter().filter(|r| r.status.is_completed()).count(),
                total = step_results.len(),
                "Executor finished steps"
            );

            let review = self
                .review(&task, &plan, &step_results)
                .await
                .map_err(|e| AgentPipelineError::Reviewer(e.to_string()))?;
            if let Some(tx) = events.as_ref() {
                let _ = tx
                    .send(PipelineEvent::ReviewCompleted {
                        iteration: iter,
                        review: review.clone(),
                    })
                    .await;
            }

            let approved = review.is_approved();
            iterations.push(PipelineIteration {
                iteration: iter,
                plan,
                step_results,
                review: review.clone(),
            });

            if approved {
                info!(iteration = iter, "Agent pipeline approved");
                let final_result = PipelineResult {
                    task,
                    final_review: review,
                    iterations,
                    converged: true,
                };
                if let Some(tx) = events.as_ref() {
                    let _ = tx
                        .send(PipelineEvent::PipelineCompleted {
                            converged: true,
                            iterations_count: final_result.iterations.len() as u32,
                        })
                        .await;
                }
                // Kick off session consolidation as a background task so it
                // never blocks the caller (SSE streams need to close
                // promptly; the watcher needs to return the TaskResult).
                // A failure inside consolidation is logged but never
                // propagated — the pipeline already succeeded.
                if self.consolidation_enabled() {
                    let pipeline = self.clone();
                    let trace = final_result.clone();
                    tokio::spawn(async move {
                        match pipeline.consolidate_session(&trace).await {
                            Ok(memories) if !memories.is_empty() => {
                                info!(
                                    count = memories.len(),
                                    "background: session consolidation recorded memories"
                                );
                            }
                            Ok(_) => {
                                debug!("background: session consolidation produced no memories");
                            }
                            Err(e) => {
                                warn!(error = %e, "background: session consolidation failed");
                            }
                        }
                    });
                }
                return Ok(final_result);
            }

            // Reviewer asked for revisions — carry the critique into the next plan.
            if let ReviewOutcome::Revise {
                critique,
                suggestions,
            } = &review
            {
                critique_carry = Some((critique.clone(), suggestions.clone()));
            }
        }

        // Exceeded max_iterations without converging. Return the final state
        // so the caller can persist + decide whether to escalate.
        let final_review = iterations
            .last()
            .map(|i| i.review.clone())
            .unwrap_or_else(|| ReviewOutcome::Revise {
                critique: "no iterations completed".to_string(),
                suggestions: Vec::new(),
            });
        warn!(max, "Agent pipeline hit max_iterations without approval");
        let final_result = PipelineResult {
            task,
            final_review,
            iterations,
            converged: false,
        };
        if let Some(tx) = events.as_ref() {
            let _ = tx
                .send(PipelineEvent::PipelineCompleted {
                    converged: false,
                    iterations_count: final_result.iterations.len() as u32,
                })
                .await;
        }
        Ok(final_result)
    }

    /// Phase 1 — Opus plans. Returns a structured `Plan` parsed from JSON.
    ///
    /// When `revision_input` is present, the planner is told the previous
    /// attempt was rejected and must address the listed critique.
    pub async fn plan(
        &self,
        task: &AgentTask,
        revision_input: Option<&(String, Vec<String>)>,
    ) -> Result<Plan, PhaseError> {
        let system = PLANNER_SYSTEM_PROMPT.to_string();

        let memory_block = self
            .fetch_memory_block(&task.description, task.memory_scope.as_deref())
            .await;
        let mut user = String::new();
        if !memory_block.is_empty() {
            user.push_str(&memory_block);
            user.push('\n');
        }
        user.push_str(&format!("## Task\n{}\n\n", task.description));
        if let Some(ctx) = task.context.as_deref() {
            user.push_str(&format!("## Repo context\n{}\n\n", ctx));
        }
        if let Some((critique, suggestions)) = revision_input {
            user.push_str("## Revision request\nThe previous plan was rejected. Address this critique:\n");
            user.push_str(critique);
            user.push_str("\n\nReviewer suggestions:\n");
            for s in suggestions {
                user.push_str(&format!("- {}\n", s));
            }
            user.push('\n');
        }
        user.push_str(
            "Respond with strict JSON matching the schema in the system prompt. No prose, no fences.",
        );

        let request = MessageRequest {
            model: self.planner_model.clone(),
            max_tokens: PLANNER_MAX_TOKENS,
            messages: vec![InputMessage::user_text(user)],
            system: Some(system),
            tools: None,
            tool_choice: None,
            temperature: None,
            stream: false,
        };

        let response = self
            .planner
            .send_message(&request)
            .await
            .map_err(|e| PhaseError::Transport(e.to_string()))?;
        let text = extract_text(&response);
        parse_plan(&text)
    }

    /// Phase 2 — Sonnet executes each step. Steps run sequentially; a failed
    /// step does not short-circuit the loop (we still let the reviewer see
    /// the whole trace), but downstream steps may inherit context describing
    /// the failure.
    pub async fn execute(
        &self,
        plan: &Plan,
        repo_context: Option<&str>,
    ) -> Result<Vec<StepExecutionResult>, PhaseError> {
        self.execute_internal(plan, repo_context, None, 0, None, None)
            .await
    }

    // Internal step loop. `events` is `Some` when called from `run_streaming`
    // so per-step start/complete events can be emitted between LLM calls.
    // `iteration` is the iteration index attached to those events (0 when
    // the caller doesn't track iterations). `tools` is `Some` to switch
    // the per-step LLM call into Anthropic tool-use mode. `memory_scope`
    // is `Some(project)` to filter memory lookups to that scope (plus
    // globals); `None` lets every project's memory match.
    async fn execute_internal(
        &self,
        plan: &Plan,
        repo_context: Option<&str>,
        memory_scope: Option<&str>,
        iteration: u32,
        events: Option<&tokio::sync::mpsc::Sender<PipelineEvent>>,
        tools: Option<&dyn ToolBackend>,
    ) -> Result<Vec<StepExecutionResult>, PhaseError> {
        let mut results: Vec<StepExecutionResult> = Vec::with_capacity(plan.steps.len());

        for step in &plan.steps {
            if let Some(tx) = events {
                let _ = tx
                    .send(PipelineEvent::StepStarted {
                        iteration,
                        step_id: step.id,
                        description: step.description.clone(),
                    })
                    .await;
            }
            let outcome = match tools {
                Some(t) => {
                    self.execute_step_with_tools(
                        plan,
                        step,
                        &results,
                        repo_context,
                        memory_scope,
                        t,
                    )
                    .await
                }
                None => {
                    self.execute_step(plan, step, &results, repo_context, memory_scope)
                        .await
                }
            };
            let result = outcome.unwrap_or_else(|e| StepExecutionResult {
                step_id: step.id,
                step_description: step.description.clone(),
                output: String::new(),
                status: StepStatus::Failed {
                    error: e.to_string(),
                },
                tool_calls: Vec::new(),
            });
            if let Some(tx) = events {
                let _ = tx
                    .send(PipelineEvent::StepCompleted {
                        iteration,
                        result: result.clone(),
                    })
                    .await;
            }
            results.push(result);
        }

        Ok(results)
    }

    async fn execute_step(
        &self,
        plan: &Plan,
        step: &PlanStep,
        prior_results: &[StepExecutionResult],
        repo_context: Option<&str>,
        memory_scope: Option<&str>,
    ) -> Result<StepExecutionResult, PhaseError> {
        let memory_block = self.fetch_memory_block(&step.description, memory_scope).await;
        let body = build_executor_user_message(plan, step, prior_results, repo_context);
        let user = prepend_memory_block(&memory_block, &body);
        let request = MessageRequest {
            model: self.executor_model.clone(),
            max_tokens: EXECUTOR_MAX_TOKENS,
            messages: vec![InputMessage::user_text(user)],
            system: Some(EXECUTOR_SYSTEM_PROMPT.to_string()),
            tools: None,
            tool_choice: None,
            temperature: None,
            stream: false,
        };

        let response = self
            .executor
            .send_message(&request)
            .await
            .map_err(|e| PhaseError::Transport(e.to_string()))?;
        let output = extract_text(&response);
        Ok(StepExecutionResult {
            step_id: step.id,
            step_description: step.description.clone(),
            output,
            status: StepStatus::Completed,
            tool_calls: Vec::new(),
        })
    }

    // Tool-use variant of `execute_step`. The conversation is multi-turn:
    // we keep appending assistant turns containing `tool_use` blocks and
    // user turns containing the corresponding `tool_result` blocks until
    // the model returns a pure-text turn. `MAX_TOOL_ITERATIONS_PER_STEP`
    // caps runaway loops.
    async fn execute_step_with_tools(
        &self,
        plan: &Plan,
        step: &PlanStep,
        prior_results: &[StepExecutionResult],
        repo_context: Option<&str>,
        memory_scope: Option<&str>,
        tools: &dyn ToolBackend,
    ) -> Result<StepExecutionResult, PhaseError> {
        let memory_block = self.fetch_memory_block(&step.description, memory_scope).await;
        let body = build_executor_user_message(plan, step, prior_results, repo_context);
        let initial_user = prepend_memory_block(&memory_block, &body);
        let tool_defs = tools.tool_definitions();

        let mut messages: Vec<InputMessage> = vec![InputMessage::user_text(initial_user)];
        let mut tool_calls_made: Vec<ToolCallRecord> = Vec::new();
        let mut accumulated_text = String::new();

        for _ in 0..MAX_TOOL_ITERATIONS_PER_STEP {
            let request = MessageRequest {
                model: self.executor_model.clone(),
                max_tokens: EXECUTOR_MAX_TOKENS,
                messages: messages.clone(),
                system: Some(EXECUTOR_SYSTEM_PROMPT.to_string()),
                tools: Some(tool_defs.clone()),
                tool_choice: None,
                temperature: None,
                stream: false,
            };

            let response = self
                .executor
                .send_message(&request)
                .await
                .map_err(|e| PhaseError::Transport(e.to_string()))?;

            // Split the response into tool_use blocks (to dispatch) and
            // text blocks (which become the final output once tool calls
            // stop). We also need to echo the assistant turn verbatim
            // back to the API on the next request, so we collect that.
            let mut assistant_blocks: Vec<InputContentBlock> = Vec::new();
            let mut tool_uses: Vec<(String, String, Value)> = Vec::new();
            for block in &response.content {
                match block {
                    OutputContentBlock::Text { text } => {
                        accumulated_text.push_str(text);
                        assistant_blocks.push(InputContentBlock::Text { text: text.clone() });
                    }
                    OutputContentBlock::ToolUse { id, name, input } => {
                        tool_uses.push((id.clone(), name.clone(), input.clone()));
                        assistant_blocks.push(InputContentBlock::ToolUse {
                            id: id.clone(),
                            name: name.clone(),
                            input: input.clone(),
                        });
                    }
                    // Extended-thinking blocks are not supported in this
                    // flow; skip them. The proxy doesn't enable thinking
                    // by default, so this is rare in practice.
                    OutputContentBlock::Thinking { .. } | OutputContentBlock::RedactedThinking { .. } => {}
                }
            }

            if tool_uses.is_empty() {
                // Pure text turn — we're done.
                return Ok(StepExecutionResult {
                    step_id: step.id,
                    step_description: step.description.clone(),
                    output: accumulated_text,
                    status: StepStatus::Completed,
                    tool_calls: tool_calls_made,
                });
            }

            // Dispatch each tool call and collect the result blocks for the
            // next user turn.
            let mut result_blocks: Vec<InputContentBlock> = Vec::with_capacity(tool_uses.len());
            for (id, name, input) in tool_uses {
                let outcome = tools.execute(&name, input.clone()).await;
                let (output_text, is_error, status) = match outcome {
                    Ok(text) => (text, false, ToolCallStatus::Success),
                    Err(err) => {
                        warn!(tool = %name, error = %err, "Tool call failed");
                        (err.to_string(), true, ToolCallStatus::Error)
                    }
                };
                tool_calls_made.push(ToolCallRecord {
                    tool_use_id: id.clone(),
                    tool_name: name.clone(),
                    input,
                    status,
                    output: truncate(&output_text, 4096),
                });
                result_blocks.push(InputContentBlock::ToolResult {
                    tool_use_id: id,
                    content: vec![ToolResultContentBlock::Text { text: output_text }],
                    is_error,
                });
            }

            // Echo the assistant turn (with tool_use blocks) back, followed
            // by a user turn carrying the tool results.
            messages.push(InputMessage {
                role: "assistant".to_string(),
                content: assistant_blocks,
            });
            messages.push(InputMessage {
                role: "user".to_string(),
                content: result_blocks,
            });
        }

        // Exceeded the per-step tool-iteration cap. Return what we have so
        // the reviewer can see the trace and decide to revise.
        Ok(StepExecutionResult {
            step_id: step.id,
            step_description: step.description.clone(),
            output: format!(
                "{}\n\n[step exceeded MAX_TOOL_ITERATIONS_PER_STEP = {}]",
                accumulated_text, MAX_TOOL_ITERATIONS_PER_STEP
            ),
            status: StepStatus::Failed {
                error: format!(
                    "tool loop did not converge within {} iterations",
                    MAX_TOOL_ITERATIONS_PER_STEP
                ),
            },
            tool_calls: tool_calls_made,
        })
    }

    /// Phase 3 — Opus reviews the whole trace and either approves or asks
    /// for a revision pass.
    pub async fn review(
        &self,
        task: &AgentTask,
        plan: &Plan,
        results: &[StepExecutionResult],
    ) -> Result<ReviewOutcome, PhaseError> {
        let trace = serde_json::to_string_pretty(&serde_json::json!({
            "task": task.description,
            "plan_summary": plan.summary,
            "steps": results.iter().map(|r| {
                let status = match &r.status {
                    StepStatus::Completed => serde_json::json!("completed"),
                    StepStatus::Failed { error } => serde_json::json!({ "failed": error }),
                };
                serde_json::json!({
                    "id": r.step_id,
                    "description": r.step_description,
                    "status": status,
                    "output": truncate(&r.output, 4000),
                })
            }).collect::<Vec<_>>(),
        }))
        .map_err(|e| PhaseError::Parse(format!("serialize trace: {}", e)))?;

        let user = format!(
            "## Original task\n{}\n\n## Trace\n```json\n{}\n```\n\nReturn strict JSON matching the schema in the system prompt.",
            task.description, trace
        );

        let request = MessageRequest {
            model: self.planner_model.clone(),
            max_tokens: REVIEWER_MAX_TOKENS,
            messages: vec![InputMessage::user_text(user)],
            system: Some(REVIEWER_SYSTEM_PROMPT.to_string()),
            tools: None,
            tool_choice: None,
            temperature: None,
            stream: false,
        };

        let response = self
            .planner
            .send_message(&request)
            .await
            .map_err(|e| PhaseError::Transport(e.to_string()))?;
        let text = extract_text(&response);
        parse_review(&text)
    }

    /// Consolidate a completed pipeline run into durable agent memory.
    ///
    /// Sends the trace (task description + per-iteration plans + step
    /// outputs + final verdict) to Sonnet with an extraction prompt, parses
    /// the JSON array of `{kind, content, importance}` entries, and writes
    /// each via `AgentMemory::record`. The project scope for the new
    /// entries comes from `result.task.memory_scope`.
    ///
    /// Returns the entries that were successfully recorded. Returns an
    /// empty `Vec` (without making any LLM call) when memory isn't
    /// configured. Failures recording individual entries are logged and
    /// skipped — they don't fail the whole call.
    pub async fn consolidate_session(
        &self,
        result: &PipelineResult,
    ) -> Result<Vec<crate::memory::MemoryEntry>, AgentPipelineError> {
        let Some(memory) = self.memory.as_ref() else {
            return Ok(Vec::new());
        };

        let trace_json = serde_json::to_string_pretty(&serde_json::json!({
            "task": result.task.description,
            "converged": result.converged,
            "iterations": result.iterations.iter().map(|iter| {
                serde_json::json!({
                    "iteration": iter.iteration,
                    "plan_summary": iter.plan.summary,
                    "step_results": iter.step_results.iter().map(|sr| {
                        serde_json::json!({
                            "id": sr.step_id,
                            "description": sr.step_description,
                            "output": truncate(&sr.output, 1500),
                            "status": match &sr.status {
                                StepStatus::Completed => serde_json::json!("completed"),
                                StepStatus::Failed { error } => serde_json::json!({"failed": error}),
                            },
                            "tool_calls": sr.tool_calls.len(),
                        })
                    }).collect::<Vec<_>>(),
                    "review": iter.review,
                })
            }).collect::<Vec<_>>(),
        }))
        .map_err(|e| AgentPipelineError::Executor(format!("serialize trace: {}", e)))?;

        let user = format!(
            "## Task\n{}\n\n## Trace\n```json\n{}\n```\n\nExtract memories. Return strict JSON.",
            result.task.description,
            truncate(&trace_json, CONSOLIDATION_TRACE_BUDGET),
        );

        let request = MessageRequest {
            model: self.executor_model.clone(),
            max_tokens: CONSOLIDATION_MAX_TOKENS,
            messages: vec![InputMessage::user_text(user)],
            system: Some(CONSOLIDATION_SYSTEM_PROMPT.to_string()),
            tools: None,
            tool_choice: None,
            temperature: None,
            stream: false,
        };

        let response = self
            .executor
            .send_message(&request)
            .await
            .map_err(|e| AgentPipelineError::Executor(e.to_string()))?;
        let text = extract_text(&response);
        let extracted = parse_consolidation(&text)
            .map_err(|e| AgentPipelineError::Executor(e.to_string()))?;

        let mut recorded = Vec::with_capacity(extracted.len());
        for entry in extracted {
            let new = crate::memory::NewMemory {
                project: result.task.memory_scope.clone(),
                kind: entry.kind,
                content: entry.content,
                importance: entry.importance,
            };
            match memory.record(new).await {
                Ok(m) => recorded.push(m),
                Err(e) => warn!(error = %e, "session consolidation: record failed"),
            }
        }

        info!(
            recorded = recorded.len(),
            project = ?result.task.memory_scope,
            "session consolidation complete"
        );
        Ok(recorded)
    }

    fn consolidation_enabled(&self) -> bool {
        self.consolidation_enabled && self.memory.is_some()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum AgentPipelineError {
    #[error("planner phase failed: {0}")]
    Planner(String),
    #[error("executor phase failed: {0}")]
    Executor(String),
    #[error("reviewer phase failed: {0}")]
    Reviewer(String),
}

#[derive(Debug, thiserror::Error)]
pub enum PhaseError {
    #[error("transport error: {0}")]
    Transport(String),
    #[error("parse error: {0}")]
    Parse(String),
}

/// Events emitted by `AgentPipeline::run_streaming` as the pipeline
/// progresses. Each variant maps directly onto an SSE event the
/// `POST /v1/agent/run` endpoint forwards to the client.
///
/// The `kind` tag in the serialized JSON matches the variant name in
/// `snake_case`, so a client sees `{"kind": "plan_completed", ...}` for
/// `PipelineEvent::PlanCompleted`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PipelineEvent {
    /// New iteration starting. `iteration` is 1-indexed.
    IterationStarted { iteration: u32, max: u32 },
    /// Planner phase finished. The full `Plan` is included so the client
    /// can render it before step execution begins.
    PlanCompleted { iteration: u32, plan: Plan },
    /// Executor is starting on a specific step.
    StepStarted {
        iteration: u32,
        step_id: u32,
        description: String,
    },
    /// Step finished — `result.status` distinguishes success from failure.
    StepCompleted {
        iteration: u32,
        result: StepExecutionResult,
    },
    /// Reviewer phase finished. The next event is either `IterationStarted`
    /// (if revising), `PipelineCompleted` (if approved or max hit), or
    /// nothing further (if the pipeline errored out — see endpoint code
    /// which sends a separate error frame in that case).
    ReviewCompleted {
        iteration: u32,
        review: ReviewOutcome,
    },
    /// Terminal event for a successful pipeline run. `converged = true`
    /// means the reviewer approved; `false` means we hit `max_iterations`.
    PipelineCompleted {
        converged: bool,
        iterations_count: u32,
    },
}

// ---------------------------------------------------------------------------
// System prompts
// ---------------------------------------------------------------------------

const PLANNER_SYSTEM_PROMPT: &str = r#"You are the planner in a three-phase agent loop. Your job is to break a software task into a short, ordered list of concrete steps an executor LLM can carry out one at a time.

Respond with ONLY a JSON object matching this schema (no prose, no markdown fences):

{
  "summary": "<one-paragraph summary of the approach>",
  "steps": [
    {
      "id": 1,
      "description": "<imperative, single-action step>",
      "success_criteria": "<observable criterion the reviewer can check>"
    }
  ],
  "risks": ["<each known risk or open question>"]
}

Rules:
- Keep steps under 8.
- Each step must be small enough that an executor LLM can produce its output in one response (under ~4k tokens).
- Steps must be in dependency order. Do not assume parallelism.
- If the task says "add X", split into design, implementation, and test verification.
- If a revision request is included in the user message, your new plan MUST address the critique."#;

const EXECUTOR_SYSTEM_PROMPT: &str = r#"You are the executor in a three-phase agent loop. The planner has produced a list of steps. You are working on ONE step at a time.

If file/command tools are available, USE THEM to make real changes:
- `read_file` to inspect existing code before editing.
- `edit_file` for targeted in-place changes (the `old_string` must be long enough to match uniquely).
- `write_file` to create new files or rewrite a file end-to-end.
- `run_command` (when present) for build / test / lint commands.
After every tool call wait for the result before deciding the next action.

If no tools are available, fall back to plain text:
- Show the exact file path and complete content you would write.
- Use fenced code blocks for code.

End your final turn with a one-sentence note saying whether the success criterion is met. Do not skip to later steps. Do not summarize the whole plan. Focus only on the current step."#;

const REVIEWER_SYSTEM_PROMPT: &str = r#"You are the reviewer in a three-phase agent loop. You see the original task plus the trace of plan + step outputs and decide whether the execution satisfies the task.

Respond with ONLY a JSON object. Choose ONE of these two shapes:

Approve:
{
  "kind": "approved",
  "summary": "<one-paragraph summary of why the trace satisfies the task>"
}

Request revision:
{
  "kind": "revise",
  "critique": "<what's missing or wrong>",
  "suggestions": ["<specific change the next plan should make>"]
}

Be strict: approve only if every step's output is concrete, the success criteria are visibly met, and the task as a whole is addressed. When in doubt, revise — the loop has a max-iterations cap, so revisions are cheap."#;

const CONSOLIDATION_SYSTEM_PROMPT: &str = r#"You are the memory consolidator for an agent that completes coding tasks. You see a completed task and its trace; your job is to extract durable knowledge worth remembering for future runs.

Categorize each memory by kind:
- `observation`: neutral facts about the codebase (e.g. "project X uses pattern Y").
- `decision`: architectural choices made and why (e.g. "we chose A over B because…").
- `preference`: user / project preferences inferred from the trace (e.g. "this project prefers idiomatic Rust over verbose code").
- `pattern`: recurring patterns observed (e.g. "all Axum handlers use State<Arc<…>>").
- `task_outcome`: what worked or failed for this task type, in a way that informs future similar tasks.

Respond with ONLY a strict JSON array (no prose, no fences). Each element:

{
  "kind": "observation" | "decision" | "preference" | "pattern" | "task_outcome",
  "content": "<one concise sentence (max 200 chars)>",
  "importance": 0.0  // float in [0.0, 1.0]; higher = more relevant for future tasks
}

Rules:
- Return an empty array `[]` when the trace doesn't teach anything notable. Don't pad.
- Keep `content` self-contained. Future readers see it without any context.
- Skip restating the task description verbatim — it's already memorialized in the result file.
- Skip transient details (specific file paths, line numbers) unless they reveal a recurring convention."#;

// ---------------------------------------------------------------------------
// Response helpers
// ---------------------------------------------------------------------------

/// Glue together a memory block (possibly empty) and a prompt body, with
/// a blank line between them when both are non-empty. When the memory
/// block is empty this is a no-op clone of `body`.
fn prepend_memory_block(memory_block: &str, body: &str) -> String {
    if memory_block.is_empty() {
        return body.to_string();
    }
    format!("{}\n{}", memory_block, body)
}

/// Build the user-message body for the executor phase. Used by both the
/// text-only and tool-use step executors so the prompt shape stays in sync.
fn build_executor_user_message(
    plan: &Plan,
    step: &PlanStep,
    prior_results: &[StepExecutionResult],
    repo_context: Option<&str>,
) -> String {
    let mut user = format!(
        "## Plan summary\n{}\n\n## Current step ({}/{})\n{}\n",
        plan.summary,
        step.id,
        plan.steps.len(),
        step.description
    );
    if !step.success_criteria.is_empty() {
        user.push_str(&format!("\n### Success criteria\n{}\n", step.success_criteria));
    }
    if let Some(ctx) = repo_context {
        user.push_str(&format!("\n## Repo context\n{}\n", ctx));
    }
    if !prior_results.is_empty() {
        user.push_str("\n## Prior step results\n");
        for prev in prior_results {
            let status = match &prev.status {
                StepStatus::Completed => "completed".to_string(),
                StepStatus::Failed { error } => format!("failed: {}", error),
            };
            user.push_str(&format!(
                "- Step {} ({}): {}\n  output: {}\n",
                prev.step_id,
                status,
                prev.step_description,
                truncate(&prev.output, 600)
            ));
        }
    }
    user
}

fn extract_text(resp: &MessageResponse) -> String {
    let mut buf = String::new();
    for block in &resp.content {
        if let OutputContentBlock::Text { text } = block {
            if !buf.is_empty() {
                buf.push('\n');
            }
            buf.push_str(text);
        }
    }
    buf
}

// Strip ```json fences / leading prose and return the substring that looks
// like a JSON object. Returns the original string if no object boundary is
// found — `serde_json` will then surface its own parse error.
fn strip_to_json<'a>(raw: &'a str) -> &'a str {
    let trimmed = raw.trim();
    let candidate = trimmed
        .strip_prefix("```json")
        .or_else(|| trimmed.strip_prefix("```"))
        .map(|s| s.strip_suffix("```").unwrap_or(s).trim())
        .unwrap_or(trimmed);
    let start = candidate.find('{');
    let end = candidate.rfind('}');
    match (start, end) {
        (Some(s), Some(e)) if e > s => &candidate[s..=e],
        _ => candidate,
    }
}

fn parse_plan(raw: &str) -> Result<Plan, PhaseError> {
    let json = strip_to_json(raw);
    serde_json::from_str::<Plan>(json).map_err(|e| {
        PhaseError::Parse(format!(
            "planner output was not valid JSON ({}); first 200 chars: {}",
            e,
            truncate(raw, 200)
        ))
    })
}

fn parse_review(raw: &str) -> Result<ReviewOutcome, PhaseError> {
    let json = strip_to_json(raw);
    serde_json::from_str::<ReviewOutcome>(json).map_err(|e| {
        PhaseError::Parse(format!(
            "reviewer output was not valid JSON ({}); first 200 chars: {}",
            e,
            truncate(raw, 200)
        ))
    })
}

/// One memory entry as Sonnet returns it from the consolidation prompt.
/// We deliberately keep this separate from `crate::memory::NewMemory` so
/// the wire format is decoupled from the storage struct.
#[derive(Debug, serde::Deserialize)]
struct ExtractedMemory {
    kind: crate::memory::MemoryKind,
    content: String,
    #[serde(default)]
    importance: Option<f32>,
}

// Strip ```json fences / leading prose and return the substring that
// looks like a JSON array. Mirrors `strip_to_json` but for `[...]`.
fn strip_to_json_array(raw: &str) -> &str {
    let trimmed = raw.trim();
    let candidate = trimmed
        .strip_prefix("```json")
        .or_else(|| trimmed.strip_prefix("```"))
        .map(|s| s.strip_suffix("```").unwrap_or(s).trim())
        .unwrap_or(trimmed);
    let start = candidate.find('[');
    let end = candidate.rfind(']');
    match (start, end) {
        (Some(s), Some(e)) if e > s => &candidate[s..=e],
        _ => candidate,
    }
}

/// Parse Sonnet's consolidation response into a vector of extracted
/// memories. Entries with empty `content` (after trim) are dropped silently.
fn parse_consolidation(raw: &str) -> Result<Vec<ExtractedMemory>, PhaseError> {
    let json = strip_to_json_array(raw);
    let raw_entries: Vec<ExtractedMemory> = serde_json::from_str(json).map_err(|e| {
        PhaseError::Parse(format!(
            "consolidation output was not valid JSON ({}); first 200 chars: {}",
            e,
            truncate(raw, 200)
        ))
    })?;
    Ok(raw_entries
        .into_iter()
        .filter(|e| !e.content.trim().is_empty())
        .collect())
}

fn truncate(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max_chars).collect();
    out.push_str("… [truncated]");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_to_json_handles_markdown_fence() {
        let raw = "```json\n{\"a\": 1}\n```";
        assert_eq!(strip_to_json(raw), "{\"a\": 1}");
    }

    #[test]
    fn strip_to_json_handles_bare_object() {
        let raw = "{\"a\": 1}";
        assert_eq!(strip_to_json(raw), "{\"a\": 1}");
    }

    #[test]
    fn strip_to_json_handles_leading_prose() {
        let raw = "Sure! Here you go:\n{\"a\": 1}\nLet me know if you need anything else.";
        assert_eq!(strip_to_json(raw), "{\"a\": 1}");
    }

    #[test]
    fn parse_plan_accepts_minimal_object() {
        let json = r#"{"summary":"do it","steps":[],"risks":[]}"#;
        let plan = parse_plan(json).expect("should parse");
        assert_eq!(plan.summary, "do it");
        assert!(plan.steps.is_empty());
    }

    #[test]
    fn parse_plan_accepts_full_object() {
        let json = r#"{
            "summary": "fix the cache",
            "steps": [
                {"id": 1, "description": "find the bug", "success_criteria": "regression test fails before, passes after"},
                {"id": 2, "description": "fix the bug", "success_criteria": ""}
            ],
            "risks": ["may invalidate live cache entries"]
        }"#;
        let plan = parse_plan(json).expect("should parse");
        assert_eq!(plan.steps.len(), 2);
        assert_eq!(plan.steps[0].id, 1);
        assert_eq!(plan.risks.len(), 1);
    }

    #[test]
    fn parse_plan_surfaces_parse_error() {
        let err = parse_plan("not json").expect_err("should error");
        let msg = err.to_string();
        assert!(msg.contains("planner output was not valid JSON"));
    }

    #[test]
    fn parse_review_accepts_approval() {
        let json = r#"{"kind": "approved", "summary": "trace looks good"}"#;
        let outcome = parse_review(json).expect("should parse");
        assert!(outcome.is_approved());
    }

    #[test]
    fn parse_review_accepts_revision() {
        let json = r#"{"kind": "revise", "critique": "missed step 2", "suggestions": ["redo step 2"]}"#;
        let outcome = parse_review(json).expect("should parse");
        assert!(!outcome.is_approved());
        if let ReviewOutcome::Revise { suggestions, .. } = outcome {
            assert_eq!(suggestions.len(), 1);
        } else {
            unreachable!();
        }
    }

    #[test]
    fn truncate_keeps_short_strings() {
        assert_eq!(truncate("short", 10), "short");
    }

    #[test]
    fn truncate_marks_long_strings() {
        let out = truncate("abcdefghij", 5);
        assert!(out.starts_with("abcde"));
        assert!(out.contains("truncated"));
    }

    #[test]
    fn strip_to_json_array_handles_bare_array() {
        let raw = r#"[{"a":1},{"b":2}]"#;
        assert_eq!(strip_to_json_array(raw), raw);
    }

    #[test]
    fn strip_to_json_array_handles_markdown_fence() {
        let raw = "```json\n[{\"kind\":\"decision\"}]\n```";
        assert_eq!(strip_to_json_array(raw), r#"[{"kind":"decision"}]"#);
    }

    #[test]
    fn strip_to_json_array_handles_leading_prose() {
        let raw = "Sure thing! Here you go:\n[{\"a\":1}]\nLet me know if you need anything.";
        assert_eq!(strip_to_json_array(raw), "[{\"a\":1}]");
    }

    #[test]
    fn parse_consolidation_accepts_empty_array() {
        let entries = parse_consolidation("[]").expect("parse");
        assert!(entries.is_empty());
    }

    #[test]
    fn parse_consolidation_accepts_full_entries() {
        let raw = r#"[
            {"kind": "decision", "content": "use sqlx not diesel", "importance": 0.8},
            {"kind": "pattern", "content": "axum handlers share state via Arc"},
            {"kind": "task_outcome", "content": "DB-heavy tasks benefit from connection pool tuning", "importance": 0.6}
        ]"#;
        let entries = parse_consolidation(raw).expect("parse");
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].kind, crate::memory::MemoryKind::Decision);
        assert_eq!(entries[0].content, "use sqlx not diesel");
        assert_eq!(entries[0].importance, Some(0.8));
        assert_eq!(entries[1].kind, crate::memory::MemoryKind::Pattern);
        assert!(entries[1].importance.is_none());
        assert_eq!(entries[2].kind, crate::memory::MemoryKind::TaskOutcome);
    }

    #[test]
    fn parse_consolidation_strips_empty_content_entries() {
        // A model emitting `{"content": "   "}` should be silently dropped
        // rather than passed through to AgentMemory::record (which rejects
        // empty content with an error).
        let raw = r#"[
            {"kind": "decision", "content": "  "},
            {"kind": "preference", "content": "user wants concise commits"}
        ]"#;
        let entries = parse_consolidation(raw).expect("parse");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].kind, crate::memory::MemoryKind::Preference);
    }

    #[test]
    fn parse_consolidation_surfaces_parse_error() {
        let err = parse_consolidation("not a json array").expect_err("should error");
        let msg = err.to_string();
        assert!(msg.contains("consolidation output was not valid JSON"));
    }

    #[test]
    fn parse_consolidation_rejects_unknown_kind() {
        // Unknown `kind` causes serde to fail the whole array. We surface
        // it as a parse error rather than silently dropping individual
        // entries — a model that fabricates kinds is buggy in a way the
        // caller should see.
        let raw = r#"[{"kind": "nonsense", "content": "x"}]"#;
        let err = parse_consolidation(raw).expect_err("should error");
        assert!(err.to_string().contains("consolidation output"));
    }
}
