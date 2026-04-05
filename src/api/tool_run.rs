// POST /api/v1/tools/run — execute a registered plugin tool
//
// RC-CRATES-D: Wires `tools::GlobalToolRegistry` into the RC HTTP API so
// external agents (OpenClaw, Claude Code, claw CLI) can invoke any built-in
// or plugin-defined tool over REST.
//
// ## Request
// ```json
// { "tool": "bash", "input": { "command": "echo hello" } }
// ```
//
// ## Response (success)
// ```json
// { "ok": true,  "output": "...", "tool": "bash" }
// ```
//
// ## Response (error)
// ```json
// { "ok": false, "error": "unsupported tool: nope", "tool": "nope" }
// ```

use axum::{Json, extract::State, http::StatusCode, response::IntoResponse};
use plugins::{PluginManager, PluginManagerConfig};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;
use tokio::sync::RwLock;
use tools::GlobalToolRegistry;
use tracing::{info, warn};

// ── Shared state ─────────────────────────────────────────────────────────────

// State injected into the tool-run router.
#[derive(Clone)]
pub struct ToolRunState {
    // Plugin config home (e.g. `infrastructure/config/rustcode/plugins/`)
    pub plugin_config_home: String,
}

impl ToolRunState {
    pub fn new(plugin_config_home: impl Into<String>) -> Self {
        Self {
            plugin_config_home: plugin_config_home.into(),
        }
    }

    // Build a fresh `GlobalToolRegistry` that includes any enabled plugins.
    pub fn registry(&self) -> Result<GlobalToolRegistry, String> {
        let config = PluginManagerConfig::new(&self.plugin_config_home);
        let manager = PluginManager::new(config);
        let plugin_tools = manager
            .aggregated_tools()
            .map_err(|e| format!("plugin load error: {e}"))?;
        GlobalToolRegistry::with_plugin_tools(plugin_tools)
    }
}

// ── Request / Response types ──────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct ToolRunRequest {
    // Tool name, e.g. `"bash"`, `"read_file"`, `"code_review"`.
    pub tool: String,
    // JSON input matching that tool's `inputSchema`.
    pub input: Value,
}

#[derive(Debug, Serialize)]
pub struct ToolRunResponse {
    pub ok: bool,
    pub tool: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

// ── Handlers ──────────────────────────────────────────────────────────────────

// `POST /api/v1/tools/run`
pub async fn run_tool(
    State(state): State<Arc<ToolRunState>>,
    Json(req): Json<ToolRunRequest>,
) -> impl IntoResponse {
    info!("tool_run: executing '{}'", req.tool);

    let registry = match state.registry() {
        Ok(r) => r,
        Err(e) => {
            warn!("tool_run: registry build failed — {e}");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ToolRunResponse {
                    ok: false,
                    tool: req.tool,
                    output: None,
                    error: Some(format!("registry error: {e}")),
                }),
            );
        }
    };

    match registry.execute(&req.tool, &req.input) {
        Ok(raw) => {
            // Try to parse output as JSON; fall back to plain string value
            let output: Value = serde_json::from_str(&raw)
                .unwrap_or_else(|_| Value::String(raw));
            (
                StatusCode::OK,
                Json(ToolRunResponse {
                    ok: true,
                    tool: req.tool,
                    output: Some(output),
                    error: None,
                }),
            )
        }
        Err(e) => {
            warn!("tool_run: '{}' failed — {e}", req.tool);
            (
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(ToolRunResponse {
                    ok: false,
                    tool: req.tool,
                    output: None,
                    error: Some(e),
                }),
            )
        }
    }
}

// `GET /api/v1/tools` — list every available tool name + description.
pub async fn list_tools(
    State(state): State<Arc<ToolRunState>>,
) -> impl IntoResponse {
    let registry = match state.registry() {
        Ok(r) => r,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": e })),
            );
        }
    };

    // definitions() with no filter returns everything
    let defs = registry.definitions(None);
    let tools: Vec<Value> = defs
        .into_iter()
        .map(|d| {
            serde_json::json!({
                "name": d.name,
                "description": d.description,
                "input_schema": d.input_schema,
            })
        })
        .collect();

    (StatusCode::OK, Json(serde_json::json!({ "tools": tools })))
}

// `GET /api/v1/plugins` — list enabled plugins and their tools.
pub async fn list_plugins(
    State(state): State<Arc<ToolRunState>>,
) -> impl IntoResponse {
    let config = PluginManagerConfig::new(&state.plugin_config_home);
    let manager = PluginManager::new(config);

    match manager.list_plugins() {
        Ok(summaries) => {
            let plugins: Vec<Value> = summaries
                .into_iter()
                .map(|s| {
                    serde_json::json!({
                        "id":          s.metadata.id,
                        "name":        s.metadata.name,
                        "version":     s.metadata.version,
                        "description": s.metadata.description,
                        "kind":        format!("{}", s.metadata.kind),
                        "enabled":     s.enabled,
                    })
                })
                .collect();
            (StatusCode::OK, Json(serde_json::json!({ "plugins": plugins })))
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": e.to_string() })),
        ),
    }
}
