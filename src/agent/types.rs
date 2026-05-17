// Data types for the planner → executor → reviewer agent loop.
//
// These are shaped for the iterative agent workflow (`AgentPipeline`) and
// intentionally separate from the project-management-flavoured `ProjectPlan`
// in `src/llm/grok.rs`. If you need an estimate-driven plan, use that one;
// if you want a list of concrete coding steps Sonnet can act on one at a
// time, use these.

use serde::{Deserialize, Serialize};

/// The work item handed to `AgentPipeline::run`.
///
/// `description` is the user-facing task description (e.g. "Add a /v1/agent
/// endpoint to the proxy"). `context` is optional repo-shaped grounding —
/// dependency tree, symbol map, RAG chunks, prior session memories — that the
/// planner should incorporate but isn't itself the task. `memory_scope`
/// narrows memory lookups to entries scoped to a specific project (plus
/// globals); `None` searches across the entire store.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentTask {
    pub description: String,
    #[serde(default)]
    pub context: Option<String>,
    #[serde(default)]
    pub memory_scope: Option<String>,
}

impl AgentTask {
    #[must_use]
    pub fn new(description: impl Into<String>) -> Self {
        Self {
            description: description.into(),
            context: None,
            memory_scope: None,
        }
    }

    #[must_use]
    pub fn with_context(mut self, context: impl Into<String>) -> Self {
        self.context = Some(context.into());
        self
    }

    /// Set the project scope for memory lookups. Typically the task's
    /// `owner/repo` slug, so memories recorded against that project plus
    /// any global entries (`project IS NULL`) surface.
    #[must_use]
    pub fn with_memory_scope(mut self, scope: impl Into<String>) -> Self {
        self.memory_scope = Some(scope.into());
        self
    }
}

/// A structured plan emitted by Opus during the planner phase.
///
/// `steps` is intentionally a flat sequence. If you need phases, group the
/// steps in the caller — keeping the data shape flat lets the executor loop
/// over `&plan.steps` without recursion.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Plan {
    pub summary: String,
    pub steps: Vec<PlanStep>,
    /// Open questions / risks the planner flagged. Surfaced to the reviewer
    /// so it can score how well execution addressed them.
    #[serde(default)]
    pub risks: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanStep {
    /// 1-indexed step number for traceability in logs / dashboards.
    pub id: u32,
    pub description: String,
    /// Free-text criteria the executor should satisfy. The reviewer compares
    /// the actual step output against this.
    #[serde(default)]
    pub success_criteria: String,
}

/// The result of executing a single step during the executor phase.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepExecutionResult {
    pub step_id: u32,
    pub step_description: String,
    /// What Sonnet produced for the step — text body, file content, command
    /// transcript, etc. In the tool-use path this is the final text turn
    /// from the model after all tool calls have settled; without tools
    /// it's the single-shot response.
    pub output: String,
    pub status: StepStatus,
    /// Trace of each tool invocation made during this step (empty when the
    /// pipeline is running without tools).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<crate::agent::tools::ToolCallRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StepStatus {
    /// The step ran end-to-end and reported success.
    Completed,
    /// Either the LLM call failed (transport, parse, rate limit) or the model
    /// itself reported the step couldn't be done.
    Failed { error: String },
}

impl StepStatus {
    #[must_use]
    pub fn is_completed(&self) -> bool {
        matches!(self, StepStatus::Completed)
    }
}

/// Reviewer verdict emitted by Opus after the executor phase.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ReviewOutcome {
    /// All steps satisfy the plan + task; pipeline can stop iterating.
    Approved { summary: String },
    /// The reviewer wants another planner→executor pass. `critique` explains
    /// what went wrong; `suggestions` is the planner's input for the rerun.
    Revise {
        critique: String,
        suggestions: Vec<String>,
    },
}

impl ReviewOutcome {
    #[must_use]
    pub fn is_approved(&self) -> bool {
        matches!(self, ReviewOutcome::Approved { .. })
    }
}

/// Full record of a single pipeline run — exactly what gets persisted to
/// `tasks/results/{id}.json` for an agent-driven task.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelineResult {
    pub task: AgentTask,
    /// One entry per iteration. Length ≤ `max_iterations`.
    pub iterations: Vec<PipelineIteration>,
    /// Final review outcome (echoes `iterations.last().review`). Convenient
    /// for callers that don't want to walk the iteration list.
    pub final_review: ReviewOutcome,
    /// True when the loop terminated because the reviewer approved; false
    /// when it hit `max_iterations` while still revising.
    pub converged: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelineIteration {
    pub iteration: u32,
    pub plan: Plan,
    pub step_results: Vec<StepExecutionResult>,
    pub review: ReviewOutcome,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn step_status_round_trips_through_json() {
        let completed = StepStatus::Completed;
        let failed = StepStatus::Failed {
            error: "boom".to_string(),
        };
        let c = serde_json::to_string(&completed).unwrap();
        let f = serde_json::to_string(&failed).unwrap();
        assert_eq!(
            serde_json::from_str::<StepStatus>(&c).unwrap().is_completed(),
            true
        );
        assert_eq!(
            serde_json::from_str::<StepStatus>(&f).unwrap().is_completed(),
            false
        );
    }

    #[test]
    fn review_outcome_round_trips_through_json() {
        let approved = ReviewOutcome::Approved {
            summary: "looks good".to_string(),
        };
        let revise = ReviewOutcome::Revise {
            critique: "missed step 2".to_string(),
            suggestions: vec!["redo step 2".to_string()],
        };
        let a = serde_json::to_string(&approved).unwrap();
        let r = serde_json::to_string(&revise).unwrap();
        assert!(serde_json::from_str::<ReviewOutcome>(&a).unwrap().is_approved());
        assert!(!serde_json::from_str::<ReviewOutcome>(&r).unwrap().is_approved());
    }

    #[test]
    fn agent_task_builder_sets_context() {
        let task = AgentTask::new("do the thing").with_context("repo tree: src/lib.rs");
        assert_eq!(task.description, "do the thing");
        assert_eq!(task.context.as_deref(), Some("repo tree: src/lib.rs"));
        assert!(task.memory_scope.is_none());
    }

    #[test]
    fn agent_task_builder_sets_memory_scope() {
        let task = AgentTask::new("fix it")
            .with_context("ctx")
            .with_memory_scope("owner/repo");
        assert_eq!(task.memory_scope.as_deref(), Some("owner/repo"));
    }

    #[test]
    fn agent_task_default_memory_scope_is_none() {
        // Round-trip a body without `memory_scope` to confirm
        // `#[serde(default)]` keeps old payloads compatible.
        let body = r#"{"description":"task","context":"ctx"}"#;
        let task: AgentTask = serde_json::from_str(body).expect("parse");
        assert!(task.memory_scope.is_none());
        assert_eq!(task.description, "task");
    }
}
