// src/model_router.rs
// RustCode ModelRouter — routes tasks between local Ollama and remote Grok API

use serde::{Deserialize, Serialize};
use std::fmt;
use std::time::Duration;
use tracing::{debug, info, warn};

// ---------------------------------------------------------------------------
// Task classification
// ---------------------------------------------------------------------------

// Describes the nature of a code/chat task so the router can pick the right model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TaskKind {
    // Generate a stub, skeleton, or boilerplate (80% quality target — local model)
    ScaffoldStub,
    // Insert TODO / FIXME / STUB tags into existing code
    TodoTagging,
    // Walk a project tree and summarise structure
    TreeSummary,
    // Extract symbols (fns, structs, traits, impls) from a file
    SymbolExtraction,
    // Answer a general question about a repo or codebase
    RepoQuestion,
    // Complex architectural reasoning or multi-file refactor (remote model)
    ArchitecturalReason,
    // Final review / critique of generated code (remote model)
    CodeReview,
    // Anything that doesn't clearly fit above — fall back to remote
    Unknown,
}

impl TaskKind {
    // True if this task should be handled by the local model.
    pub fn is_local(&self) -> bool {
        matches!(
            self,
            TaskKind::ScaffoldStub
                | TaskKind::TodoTagging
                | TaskKind::TreeSummary
                | TaskKind::SymbolExtraction
                | TaskKind::RepoQuestion
        )
    }
}

impl fmt::Display for TaskKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}", self)
    }
}

// ---------------------------------------------------------------------------
// Model targets
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ModelTarget {
    // Local Ollama instance (e.g. Qwen2.5-Coder:7b)
    Local { model: String, base_url: String },
    // Remote xAI Grok API
    Remote { model: String, api_key: String },
}

impl fmt::Display for ModelTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ModelTarget::Local { model, .. } => write!(f, "local/{}", model),
            ModelTarget::Remote { model, .. } => write!(f, "remote/{}", model),
        }
    }
}

// ---------------------------------------------------------------------------
// Router config
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelRouterConfig {
    pub local_model: String,
    pub local_base_url: String,
    pub remote_model: String,
    pub remote_api_key: String,
    // If true, always use remote regardless of task kind (useful for debugging)
    pub force_remote: bool,
    // If local Ollama is unreachable, fall back to remote automatically
    pub fallback_to_remote: bool,
}

impl Default for ModelRouterConfig {
    fn default() -> Self {
        Self {
            local_model: "qwen2.5-coder:7b".to_string(),
            local_base_url: "http://localhost:11434".to_string(),
            remote_model: "grok-4-1-fast-reasoning".to_string(),
            remote_api_key: String::new(),
            force_remote: false,
            fallback_to_remote: true,
        }
    }
}

// ---------------------------------------------------------------------------
// Router
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct ModelRouter {
    config: ModelRouterConfig,
}

impl ModelRouter {
    pub fn new(config: ModelRouterConfig) -> Self {
        Self { config }
    }

    // Classify a raw user prompt into a `TaskKind`.
    //
    // Strategy (in order):
    // 1. Try a one-shot LLM classification against the local Ollama instance
    //    (fast, <200 ms on a warm model).  The model is asked to reply with
    //    exactly one label from the fixed set.
    // 2. If Ollama is unreachable or returns an unrecognised label, fall back
    //    to the deterministic keyword heuristic below.
    //
    // This is the sync surface — callers that want the async LLM path should
    // use `classify_prompt_async` instead.  The sync version always falls back
    // to keywords immediately (useful in tests and for the `route_prompt` API
    // that is called from sync contexts).
    pub fn classify_prompt(&self, prompt: &str) -> TaskKind {
        Self::keyword_classify(prompt)
    }

    // Async version of `classify_prompt` — tries the local Ollama model first,
    // falls back to keywords if Ollama is unreachable or gives a bad response.
    //
    // Callers in async contexts (e.g. `handle_chat`) should prefer this.
    pub async fn classify_prompt_async(&self, prompt: &str) -> TaskKind {
        // Only attempt LLM classification when not forced-remote and the local
        // model is configured.
        if !self.config.force_remote {
            if let Some(kind) = self.llm_classify(prompt).await {
                return kind;
            }
        }
        Self::keyword_classify(prompt)
    }

    // One-shot LLM classification via a tiny Ollama request.
    //
    // Returns `None` if Ollama is unreachable, times out, or returns an
    // unrecognised label — callers should fall back to keyword matching.
    async fn llm_classify(&self, prompt: &str) -> Option<TaskKind> {
        const CLASSIFY_SYSTEM: &str = "\
You are a task classifier. Classify the user message into EXACTLY ONE of these labels \
(reply with only the label, nothing else):\n\
ScaffoldStub | TodoTagging | TreeSummary | SymbolExtraction | \
RepoQuestion | ArchitecturalReason | CodeReview | Unknown";

        let classify_prompt = format!(
            "Classify this message:\n\"\"\"\n{}\n\"\"\"",
            &prompt[..prompt.len().min(400)] // cap to keep the request tiny
        );

        let url = format!("{}/api/chat", self.config.local_base_url);

        #[derive(serde::Serialize)]
        struct Req<'a> {
            model: &'a str,
            messages: [Msg<'a>; 2],
            stream: bool,
            options: Opts,
        }
        #[derive(serde::Serialize)]
        struct Msg<'a> {
            role: &'a str,
            content: &'a str,
        }
        #[derive(serde::Serialize)]
        struct Opts {
            temperature: f32,
            num_predict: u32,
        }
        #[derive(serde::Deserialize)]
        struct Resp {
            message: RespMsg,
        }
        #[derive(serde::Deserialize)]
        struct RespMsg {
            content: String,
        }

        let body = Req {
            model: &self.config.local_model,
            messages: [
                Msg {
                    role: "system",
                    content: CLASSIFY_SYSTEM,
                },
                Msg {
                    role: "user",
                    content: &classify_prompt,
                },
            ],
            stream: false,
            options: Opts {
                temperature: 0.0,
                num_predict: 16,
            },
        };

        let client = match reqwest::Client::builder()
            .timeout(Duration::from_secs(8))
            .build()
        {
            Ok(c) => c,
            Err(_) => return None,
        };

        let resp = match client.post(&url).json(&body).send().await {
            Ok(r) if r.status().is_success() => r,
            Ok(r) => {
                warn!(status = %r.status(), "LLM classifier: non-success response");
                return None;
            }
            Err(e) => {
                debug!(error = %e, "LLM classifier: Ollama unreachable — using keywords");
                return None;
            }
        };

        let parsed: Resp = match resp.json().await {
            Ok(p) => p,
            Err(e) => {
                warn!(error = %e, "LLM classifier: failed to parse response");
                return None;
            }
        };

        let label = parsed.message.content.trim().to_string();
        let kind = match label.as_str() {
            "ScaffoldStub" => TaskKind::ScaffoldStub,
            "TodoTagging" => TaskKind::TodoTagging,
            "TreeSummary" => TaskKind::TreeSummary,
            "SymbolExtraction" => TaskKind::SymbolExtraction,
            "RepoQuestion" => TaskKind::RepoQuestion,
            "ArchitecturalReason" => TaskKind::ArchitecturalReason,
            "CodeReview" => TaskKind::CodeReview,
            "Unknown" => TaskKind::Unknown,
            other => {
                warn!(label = %other, "LLM classifier returned unknown label — falling back to keywords");
                return None;
            }
        };

        info!(label = %label, "LLM classifier succeeded");
        Some(kind)
    }

    // Pure keyword heuristic — always fast, no I/O.
    fn keyword_classify(prompt: &str) -> TaskKind {
        let lower = prompt.to_lowercase();

        if lower.contains("stub")
            || lower.contains("scaffold")
            || lower.contains("skeleton")
            || lower.contains("placeholder")
            || lower.contains("boilerplate")
            || lower.contains("generate")
            || lower.contains("create a fn")
            || lower.contains("create a struct")
        {
            return TaskKind::ScaffoldStub;
        }

        if lower.contains("todo") || lower.contains("fixme") || lower.contains("tag") {
            return TaskKind::TodoTagging;
        }

        if lower.contains("tree") || lower.contains("structure") || lower.contains("layout") {
            return TaskKind::TreeSummary;
        }

        if lower.contains("symbol") || lower.contains("extract") || lower.contains("list function")
        {
            return TaskKind::SymbolExtraction;
        }

        if lower.contains("review")
            || lower.contains("critique")
            || lower.contains("is this correct")
        {
            return TaskKind::CodeReview;
        }

        if lower.contains("architect") || lower.contains("design") || lower.contains("refactor") {
            return TaskKind::ArchitecturalReason;
        }

        if lower.contains("repo") || lower.contains("codebase") || lower.contains("where is") {
            return TaskKind::RepoQuestion;
        }

        TaskKind::Unknown
    }

    // Decide which model target to use for a given task.
    pub fn route(&self, task: &TaskKind) -> ModelTarget {
        if self.config.force_remote || !task.is_local() {
            info!(task = %task, target = "remote", "Routing to remote model");
            return ModelTarget::Remote {
                model: self.config.remote_model.clone(),
                api_key: self.config.remote_api_key.clone(),
            };
        }

        debug!(task = %task, target = "local", "Routing to local model");
        ModelTarget::Local {
            model: self.config.local_model.clone(),
            base_url: self.config.local_base_url.clone(),
        }
    }

    // Route by raw prompt (sync) — classifies via keywords then routes.
    // Use `route_prompt_async` in async contexts for LLM-assisted classification.
    pub fn route_prompt(&self, prompt: &str) -> (TaskKind, ModelTarget) {
        let kind = self.classify_prompt(prompt);
        let target = self.route(&kind);
        (kind, target)
    }

    // Route by raw prompt (async) — tries LLM classification first, falls back
    // to keywords.  Prefer this in all async handlers.
    pub async fn route_prompt_async(&self, prompt: &str) -> (TaskKind, ModelTarget) {
        let kind = self.classify_prompt_async(prompt).await;
        let target = self.route(&kind);
        (kind, target)
    }

    // Called when a local model request fails. Returns fallback target if configured.
    pub fn on_local_failure(&self, task: &TaskKind) -> Option<ModelTarget> {
        if self.config.fallback_to_remote {
            warn!(task = %task, "Local model failed — falling back to remote");
            Some(ModelTarget::Remote {
                model: self.config.remote_model.clone(),
                api_key: self.config.remote_api_key.clone(),
            })
        } else {
            None
        }
    }
}

// ---------------------------------------------------------------------------
// LLM completion request/response (shared shape for both targets)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompletionRequest {
    pub system_prompt: Option<String>,
    pub user_prompt: String,
    pub max_tokens: u32,
    pub temperature: f32,
    // Injected repo context (tree, symbols, todos) — prepended to user_prompt
    pub repo_context: Option<String>,
}

impl CompletionRequest {
    pub fn for_stub(user_prompt: impl Into<String>, repo_context: Option<String>) -> Self {
        Self {
            system_prompt: Some(RUST_STUB_SYSTEM_PROMPT.to_string()),
            user_prompt: user_prompt.into(),
            max_tokens: 1024,
            temperature: 0.2, // low temp for deterministic scaffold
            repo_context,
        }
    }

    // Build the final prompt string injecting repo context if present.
    pub fn build_prompt(&self) -> String {
        match &self.repo_context {
            Some(ctx) => format!(
                "### Repo Context\n{}\n\n### Task\n{}",
                ctx, self.user_prompt
            ),
            None => self.user_prompt.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompletionResponse {
    pub content: String,
    pub model_used: String,
    pub task_kind: TaskKind,
    pub used_fallback: bool,
    pub tokens_used: Option<u32>,
}

// ---------------------------------------------------------------------------
// System prompt for Rust stub generation
// ---------------------------------------------------------------------------

pub const RUST_STUB_SYSTEM_PROMPT: &str = r#"
You are a Rust code scaffolding assistant. Your job is to generate high-quality stub code (~80% complete).

Rules:
- Always use `// TODO: <description>` on lines that need real implementation
- Always use `// STUB: generated by rustcode` at the top of each generated block
- Prefer `unimplemented!("stub: <reason>")` over `todo!()` for fn bodies
- Preserve existing type signatures exactly — do not invent types
- Match the module structure shown in the repo context
- For async fns, use `async fn` and return `Result<T, crate::error::AppError>`
- Always derive `Debug` on new structs unless there's a reason not to
- Add `#[allow(dead_code)]` to stub impls to avoid compiler noise
- Output ONLY valid Rust code — no markdown fences, no prose
"#;

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn router() -> ModelRouter {
        ModelRouter::new(ModelRouterConfig::default())
    }

    #[test]
    fn classifies_stub_prompts() {
        let r = router();
        assert_eq!(
            r.classify_prompt("generate a stub for the retry handler"),
            TaskKind::ScaffoldStub
        );
        assert_eq!(
            r.classify_prompt("scaffold the webhook module"),
            TaskKind::ScaffoldStub
        );
    }

    #[test]
    fn classifies_review_as_remote() {
        let r = router();
        let (kind, target) = r.route_prompt("review this code and tell me if it's correct");
        assert_eq!(kind, TaskKind::CodeReview);
        assert!(matches!(target, ModelTarget::Remote { .. }));
    }

    #[test]
    fn stub_routes_local() {
        let r = router();
        let (kind, target) = r.route_prompt("create a stub for the cache invalidation fn");
        assert_eq!(kind, TaskKind::ScaffoldStub);
        assert!(matches!(target, ModelTarget::Local { .. }));
    }

    #[test]
    fn force_remote_overrides() {
        let config = ModelRouterConfig {
            force_remote: true,
            ..ModelRouterConfig::default()
        };
        let r = ModelRouter::new(config);
        let (_, target) = r.route_prompt("generate a stub");
        assert!(matches!(target, ModelTarget::Remote { .. }));
    }

    #[test]
    fn keyword_classify_all_kinds() {
        assert_eq!(
            ModelRouter::keyword_classify("scaffold a new struct"),
            TaskKind::ScaffoldStub
        );
        assert_eq!(
            ModelRouter::keyword_classify("add TODO tags here"),
            TaskKind::TodoTagging
        );
        assert_eq!(
            ModelRouter::keyword_classify("show the tree structure"),
            TaskKind::TreeSummary
        );
        assert_eq!(
            ModelRouter::keyword_classify("extract all symbols"),
            TaskKind::SymbolExtraction
        );
        assert_eq!(
            ModelRouter::keyword_classify("review this code"),
            TaskKind::CodeReview
        );
        assert_eq!(
            ModelRouter::keyword_classify("design the architecture"),
            TaskKind::ArchitecturalReason
        );
        assert_eq!(
            ModelRouter::keyword_classify("where is this in the repo"),
            TaskKind::RepoQuestion
        );
        assert_eq!(
            ModelRouter::keyword_classify("hello there"),
            TaskKind::Unknown
        );
    }
}
