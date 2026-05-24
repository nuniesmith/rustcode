// Three-phase agent loop: planner → executor → reviewer.
//
// See `pipeline.rs` for the orchestration and `types.rs` for the data shapes.
// Wiring into the HTTP layer (`POST /v1/agent/run`) and the task watcher
// (`tasks/*.json` → `AgentPipeline::run`) lives behind AGENT-B and AGENT-C
// and is intentionally out of scope here.

pub mod pipeline;
pub mod tools;
pub mod types;

pub use pipeline::{
    AgentPipeline, AgentPipelineError, DEFAULT_MAX_ITERATIONS, DEFAULT_MEMORY_TOP_K, PhaseError,
    PipelineEvent,
};
pub use tools::{FileSystemTools, ToolBackend, ToolCallRecord, ToolCallStatus, ToolError};
pub use types::{
    AgentTask, PipelineIteration, PipelineResult, Plan, PlanStep, ReviewOutcome,
    StepExecutionResult, StepStatus,
};
