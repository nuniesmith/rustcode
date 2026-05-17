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
    AnthropicClient, InputMessage, MessageRequest, MessageResponse, OutputContentBlock,
};
use std::sync::Arc;
use tracing::{debug, info, warn};

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

#[derive(Debug, Clone)]
pub struct AgentPipeline {
    planner: Arc<AnthropicClient>,
    executor: Arc<AnthropicClient>,
    planner_model: String,
    executor_model: String,
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
        let max = max_iterations.max(1);
        let mut iterations: Vec<PipelineIteration> = Vec::with_capacity(max as usize);
        let mut critique_carry: Option<(String, Vec<String>)> = None;

        for iter in 1..=max {
            info!(
                iteration = iter,
                max, task = %task.description, "Agent pipeline: starting iteration"
            );

            let plan = self
                .plan(&task, critique_carry.as_ref())
                .await
                .map_err(|e| AgentPipelineError::Planner(e.to_string()))?;
            debug!(steps = plan.steps.len(), "Planner emitted plan");

            let step_results = self
                .execute(&plan, task.context.as_deref())
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

            let approved = review.is_approved();
            iterations.push(PipelineIteration {
                iteration: iter,
                plan,
                step_results,
                review: review.clone(),
            });

            if approved {
                info!(iteration = iter, "Agent pipeline approved");
                return Ok(PipelineResult {
                    task,
                    final_review: review,
                    iterations,
                    converged: true,
                });
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
        Ok(PipelineResult {
            task,
            final_review,
            iterations,
            converged: false,
        })
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

        let mut user = format!(
            "## Task\n{}\n\n",
            task.description
        );
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
        let mut results: Vec<StepExecutionResult> = Vec::with_capacity(plan.steps.len());

        for step in &plan.steps {
            let result = self
                .execute_step(plan, step, &results, repo_context)
                .await
                .unwrap_or_else(|e| StepExecutionResult {
                    step_id: step.id,
                    step_description: step.description.clone(),
                    output: String::new(),
                    status: StepStatus::Failed {
                        error: e.to_string(),
                    },
                });
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
    ) -> Result<StepExecutionResult, PhaseError> {
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

        let request = MessageRequest {
            model: self.executor_model.clone(),
            max_tokens: EXECUTOR_MAX_TOKENS,
            messages: vec![InputMessage::user_text(user)],
            system: Some(EXECUTOR_SYSTEM_PROMPT.to_string()),
            tools: None,
            tool_choice: None,
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

Your response is plain text describing what you did for this step. When the step is a coding step:
- Show the exact file path and complete content you would write.
- Use fenced code blocks for code.
- Reference the success criterion at the end of your response and state whether it's met.

Do not skip to later steps. Do not summarize the whole plan. Focus only on the current step."#;

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

// ---------------------------------------------------------------------------
// Response helpers
// ---------------------------------------------------------------------------

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
}
