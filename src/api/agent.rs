// `POST /v1/agent/run` — run a task through the planner/executor/reviewer
// pipeline and stream the events as Server-Sent Events.
//
// The pipeline itself lives in `src/agent/`. This module is just the HTTP
// surface: parse the JSON body, kick off the run on a background task, and
// forward each `PipelineEvent` as an SSE frame the client can consume
// incrementally.
//
// The request body intentionally accepts a superset of the task-file schema
// (TASK-B), so the same JSON that gets dropped in `tasks/` can also be POSTed
// here. Fields that only matter for the watcher path (`repo`, `branch`,
// `labels`, `auto_merge`) are silently ignored.

use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use axum::{
    Json,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{
        IntoResponse, Response,
        sse::{Event, KeepAlive, Sse},
    },
};
use futures::stream::{self, StreamExt};
use serde::{Deserialize, Serialize};
use tokio_stream::wrappers::ReceiverStream;
use tracing::{info, warn};

use crate::agent::{
    AgentPipeline, AgentPipelineError, AgentTask, DEFAULT_MAX_ITERATIONS, Plan, PipelineEvent,
    ReviewOutcome, StepExecutionResult,
};
use crate::api::proxy::{OaiError, ProxyState};

// Cap on iterations a client may request. Higher values would risk
// expensive runaway runs even though the pipeline itself has its own
// `max_iterations.max(1)` guard.
const MAX_ALLOWED_ITERATIONS: u32 = 6;

// Channel buffer between the pipeline task and the SSE stream. Pipeline
// events are emitted at roughly one-per-step granularity, so a small
// buffer is plenty.
const EVENT_CHANNEL_BUFFER: usize = 32;

/// Body of `POST /v1/agent/run`. Extra fields (e.g. `repo`, `branch`,
/// `labels`, `auto_merge`) are tolerated and ignored — they're only
/// meaningful to the task watcher path.
#[derive(Debug, Deserialize)]
pub struct AgentRunRequest {
    /// Task description / user prompt.
    pub description: String,
    /// Optional grounding context (RAG snippets, symbol map, repo tree)
    /// the planner should incorporate.
    #[serde(default)]
    pub context: Option<String>,
    /// Iteration cap. `None` defaults to `DEFAULT_MAX_ITERATIONS`; values
    /// over `MAX_ALLOWED_ITERATIONS` are clamped.
    #[serde(default)]
    pub max_iterations: Option<u32>,
    /// Accepted but ignored — kept here so a watcher task file can be
    /// POSTed verbatim. The planner generates its own plan from
    /// `description` + `context`.
    #[serde(default)]
    pub steps: Option<Vec<String>>,
}

/// What the SSE consumer sees. Mirrors `PipelineEvent` one-to-one plus an
/// `error` variant for the case where the pipeline returns `Err`. Each
/// frame is a single JSON object with a `kind` discriminator.
#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum AgentSseEvent {
    IterationStarted {
        iteration: u32,
        max: u32,
    },
    PlanCompleted {
        iteration: u32,
        plan: Plan,
    },
    StepStarted {
        iteration: u32,
        step_id: u32,
        description: String,
    },
    StepCompleted {
        iteration: u32,
        result: StepExecutionResult,
    },
    ReviewCompleted {
        iteration: u32,
        review: ReviewOutcome,
    },
    PipelineCompleted {
        converged: bool,
        iterations_count: u32,
    },
    /// Terminal frame emitted only when the pipeline `Err`s out. `phase`
    /// tells the client which side blew up (planner / executor / reviewer).
    Error {
        phase: String,
        message: String,
    },
}

impl From<PipelineEvent> for AgentSseEvent {
    fn from(event: PipelineEvent) -> Self {
        match event {
            PipelineEvent::IterationStarted { iteration, max } => {
                Self::IterationStarted { iteration, max }
            }
            PipelineEvent::PlanCompleted { iteration, plan } => {
                Self::PlanCompleted { iteration, plan }
            }
            PipelineEvent::StepStarted {
                iteration,
                step_id,
                description,
            } => Self::StepStarted {
                iteration,
                step_id,
                description,
            },
            PipelineEvent::StepCompleted { iteration, result } => {
                Self::StepCompleted { iteration, result }
            }
            PipelineEvent::ReviewCompleted { iteration, review } => {
                Self::ReviewCompleted { iteration, review }
            }
            PipelineEvent::PipelineCompleted {
                converged,
                iterations_count,
            } => Self::PipelineCompleted {
                converged,
                iterations_count,
            },
        }
    }
}

pub async fn handle_agent_run(
    State(state): State<ProxyState>,
    headers: HeaderMap,
    Json(req): Json<AgentRunRequest>,
) -> Response {
    if let Some(err) = check_auth(&state, &headers) {
        return err.into_response();
    }

    if req.description.trim().is_empty() {
        return OaiError::bad_request("description must not be empty").into_response();
    }

    let Some(anthropic_client) = state.repo_state.anthropic_client.clone() else {
        return OaiError::bad_request(
            "agent pipeline unavailable — ANTHROPIC_API_KEY is not configured",
        )
        .into_response();
    };

    let planner_model = state.repo_state.model_router.planner_model().to_string();
    let executor_model = state.repo_state.model_router.executor_model().to_string();
    let max_iterations = req
        .max_iterations
        .unwrap_or(DEFAULT_MAX_ITERATIONS)
        .min(MAX_ALLOWED_ITERATIONS);

    let task = {
        let mut t = AgentTask::new(req.description.clone());
        if let Some(ctx) = req.context.clone() {
            t = t.with_context(ctx);
        }
        t
    };

    info!(
        max_iterations,
        planner_model = %planner_model,
        executor_model = %executor_model,
        description = %task.description,
        "Agent run request accepted"
    );

    // Two channels: one for the pipeline's structured events, and a
    // oneshot to ferry the final `Result` (Ok | error-phase) back to the
    // stream so we can emit a terminal `error` frame if needed.
    let (event_tx, event_rx) = tokio::sync::mpsc::channel::<PipelineEvent>(EVENT_CHANNEL_BUFFER);
    let (result_tx, result_rx) = tokio::sync::oneshot::channel::<Option<(String, String)>>();

    let pipeline = Arc::new(AgentPipeline::new(
        Arc::clone(&anthropic_client),
        Arc::clone(&anthropic_client),
        planner_model,
        executor_model,
    ));
    let pipeline_for_task = Arc::clone(&pipeline);
    tokio::spawn(async move {
        let outcome = pipeline_for_task
            .run_streaming(task, max_iterations, event_tx)
            .await;
        let final_payload = match outcome {
            Ok(_) => None,
            Err(e) => {
                let (phase, message) = match e {
                    AgentPipelineError::Planner(m) => ("planner", m),
                    AgentPipelineError::Executor(m) => ("executor", m),
                    AgentPipelineError::Reviewer(m) => ("reviewer", m),
                };
                warn!(phase, error = %message, "Agent pipeline failed");
                Some((phase.to_string(), message))
            }
        };
        // Ignore send errors — the receiver may have hung up if the
        // client disconnected mid-stream.
        let _ = result_tx.send(final_payload);
    });

    // Map each PipelineEvent → JSON SSE frame. After the event channel
    // drains, await the oneshot and (if Err) emit one final `error` frame.
    let event_stream = ReceiverStream::new(event_rx).map(|event| {
        let frame: AgentSseEvent = event.into();
        encode_frame(&frame)
    });

    let trailer = stream::once(async move {
        match result_rx.await {
            Ok(Some((phase, message))) => Some(encode_frame(&AgentSseEvent::Error {
                phase,
                message,
            })),
            _ => None,
        }
    })
    .filter_map(|opt| async move { opt });

    let merged = event_stream.chain(trailer).map(|data| {
        Ok::<Event, Infallible>(Event::default().data(data))
    });

    Sse::new(merged)
        .keep_alive(KeepAlive::new().interval(Duration::from_secs(15)).text("ping"))
        .into_response()
}

// Serialize an `AgentSseEvent` to a JSON string. We swallow serialization
// errors and substitute a fallback `error` frame; the alternative —
// panicking inside the stream — would tear down the connection.
fn encode_frame(frame: &AgentSseEvent) -> String {
    serde_json::to_string(frame).unwrap_or_else(|e| {
        serde_json::json!({
            "kind": "error",
            "phase": "encoder",
            "message": format!("frame serialization failed: {}", e),
        })
        .to_string()
    })
}

// Local auth check — mirrors the proxy's `check_auth` so we don't expose
// proxy internals as `pub(crate)` just for this caller.
fn check_auth(state: &ProxyState, headers: &HeaderMap) -> Option<(StatusCode, Json<OaiError>)> {
    if state.allowed_key_hashes.is_empty() {
        return None;
    }
    let raw_key = headers
        .get("Authorization")
        .or_else(|| headers.get("X-API-Key"))
        .and_then(|v| v.to_str().ok())
        .map(|s| s.strip_prefix("Bearer ").unwrap_or(s));
    match raw_key {
        None => Some(OaiError::auth_response(
            "No API key provided. Use Authorization: Bearer <key> or X-API-Key: <key>.",
        )),
        Some(key) if state.is_authorised(key) => None,
        Some(_) => Some(OaiError::auth_response("Invalid API key.")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_accepts_task_file_shape() {
        // A task-file JSON body should deserialize cleanly, ignoring
        // fields the agent endpoint doesn't use.
        let body = r#"{
            "id": "ignored",
            "repo": "ignored/repo",
            "description": "Add a /v1/agent/run endpoint",
            "steps": ["draft the endpoint", "add tests"],
            "branch": "ignored",
            "labels": ["ignored"],
            "auto_merge": false
        }"#;
        let req: AgentRunRequest =
            serde_json::from_str(body).expect("should deserialize task-file body");
        assert_eq!(req.description, "Add a /v1/agent/run endpoint");
        assert_eq!(req.steps.as_ref().unwrap().len(), 2);
        assert!(req.context.is_none());
    }

    #[test]
    fn request_accepts_minimal_body() {
        let body = r#"{"description": "do the thing"}"#;
        let req: AgentRunRequest = serde_json::from_str(body).expect("should deserialize");
        assert_eq!(req.description, "do the thing");
        assert!(req.max_iterations.is_none());
    }

    #[test]
    fn sse_event_serializes_with_kind_tag() {
        let frame = AgentSseEvent::IterationStarted {
            iteration: 1,
            max: 3,
        };
        let json = encode_frame(&frame);
        assert!(json.contains("\"kind\":\"iteration_started\""));
        assert!(json.contains("\"iteration\":1"));
        assert!(json.contains("\"max\":3"));
    }

    #[test]
    fn sse_error_event_carries_phase_and_message() {
        let frame = AgentSseEvent::Error {
            phase: "planner".to_string(),
            message: "rate limited".to_string(),
        };
        let json = encode_frame(&frame);
        assert!(json.contains("\"kind\":\"error\""));
        assert!(json.contains("\"phase\":\"planner\""));
        assert!(json.contains("\"message\":\"rate limited\""));
    }

    #[test]
    fn pipeline_event_round_trips_via_from() {
        let event = PipelineEvent::PipelineCompleted {
            converged: true,
            iterations_count: 2,
        };
        let frame: AgentSseEvent = event.into();
        let json = encode_frame(&frame);
        assert!(json.contains("\"kind\":\"pipeline_completed\""));
        assert!(json.contains("\"converged\":true"));
    }
}
