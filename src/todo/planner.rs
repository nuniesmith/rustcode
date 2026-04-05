// Todo planner — generate a batched GAMEPLAN from `todo.md` using xAI LLM
//
// This module is the backend for `rustcode todo-plan <todo-md>`.
// It reads `todo.md`, optionally collects source context from the repo,
// sends everything to the xAI (Grok) API, and returns a structured
// `GamePlan` containing ordered `GamePlanBatch` work items ready to be
// executed by `todo-work`.
//
// # Output shape
//
// ```json
// {
//   "generated_at": "2024-01-01T00:00:00Z",
//   "todo_path": "todo.md",
//   "model": "grok-4-turbo",
//   "batches": [
//     {
//       "id": "batch-001",
//       "title": "Fix admin module ApiState fields",
//       "priority": "high",
//       "estimated_effort": "small",
//       "items": [
//         {
//           "todo_id": "a1b2c3d4",
//           "description": "Uncomment pub mod admin and fix ApiState field references",
//           "files": ["src/api/mod.rs", "src/api/admin.rs", "src/api/handlers.rs"],
//           "approach": "Align AdminState fields with the current ApiState definition"
//         }
//       ],
//       "rationale": "Blocking — admin routes are completely disabled",
//       "dependencies": []
//     }
//   ]
// }
// ```

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use crate::error::{AuditError, Result};
use crate::grok_client::GrokClient;
use crate::todo::todo_file::{Priority, TodoFile, TodoItem};

// ============================================================================
// Configuration
// ============================================================================

// Configuration for the planner
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlannerConfig {
    // Maximum number of batches to generate
    pub max_batches: usize,
    // Maximum number of work items per batch
    pub max_items_per_batch: usize,
    // Maximum number of source context snippets to include in the LLM prompt
    pub max_context_snippets: usize,
    // Maximum characters of source context to include per snippet
    pub max_snippet_chars: usize,
    // Whether to include completed (✅) items in the prompt for context
    pub include_done_items: bool,
    // LLM temperature (0.0–1.0); lower = more deterministic plans
    pub temperature: f32,
    // Override the model name (defaults to `grok-4-turbo`)
    pub model: Option<String>,
    // Additional freeform instructions appended to the system prompt
    pub extra_instructions: Option<String>,
}

impl Default for PlannerConfig {
    fn default() -> Self {
        Self {
            max_batches: 10,
            max_items_per_batch: 5,
            max_context_snippets: 8,
            max_snippet_chars: 1500,
            include_done_items: false,
            temperature: 0.2,
            model: None,
            extra_instructions: None,
        }
    }
}

// ============================================================================
// GamePlan output types
// ============================================================================

// Estimated effort for a batch
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EffortEstimate {
    // < 30 min — single function / config tweak
    Trivial,
    // 30 min – 2 h — single file change
    Small,
    // 2 h – 1 day — multi-file change
    Medium,
    // 1 day+ — architectural / cross-cutting concern
    Large,
}

impl EffortEstimate {
    pub fn as_str(self) -> &'static str {
        match self {
            EffortEstimate::Trivial => "trivial",
            EffortEstimate::Small => "small",
            EffortEstimate::Medium => "medium",
            EffortEstimate::Large => "large",
        }
    }
}

impl std::fmt::Display for EffortEstimate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl std::str::FromStr for EffortEstimate {
    type Err = ();
    fn from_str(s: &str) -> std::result::Result<Self, ()> {
        match s.to_ascii_lowercase().as_str() {
            "trivial" => Ok(EffortEstimate::Trivial),
            "small" => Ok(EffortEstimate::Small),
            "medium" => Ok(EffortEstimate::Medium),
            "large" => Ok(EffortEstimate::Large),
            _ => Ok(EffortEstimate::Small),
        }
    }
}

// A single actionable work item within a batch
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BatchWorkItem {
    // Stable ID of the corresponding `TodoItem` (from `TodoFile`)
    pub todo_id: String,
    // Short description of what to do
    pub description: String,
    // Source files likely to be touched
    pub files: Vec<String>,
    // Suggested approach / strategy
    pub approach: String,
    // Optional acceptance criteria
    pub acceptance_criteria: Option<String>,
}

// A batch of related work items that should be done together
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GamePlanBatch {
    // Unique batch identifier (e.g. `"batch-001"`)
    pub id: String,
    // Human-readable title
    pub title: String,
    // Priority level (mirrors `todo_file::Priority`)
    pub priority: String,
    // Effort estimate
    pub estimated_effort: EffortEstimate,
    // Work items in this batch
    pub items: Vec<BatchWorkItem>,
    // LLM rationale for grouping these items
    pub rationale: String,
    // Batch IDs that should be completed before this one
    pub dependencies: Vec<String>,
}

impl GamePlanBatch {
    // Check whether this batch has no unresolved dependencies
    pub fn is_ready(&self, completed: &[String]) -> bool {
        self.dependencies.iter().all(|d| completed.contains(d))
    }
}

// The full game plan returned by the planner
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GamePlan {
    pub generated_at: DateTime<Utc>,
    pub todo_path: PathBuf,
    pub model: String,
    pub batches: Vec<GamePlanBatch>,
    // Total pending items captured in this plan
    pub total_items_planned: usize,
    // Items that were skipped (e.g. too vague, already done)
    pub skipped_items: Vec<String>,
    // Raw LLM response (kept for debugging / re-parse)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw_llm_response: Option<String>,
}

impl GamePlan {
    // Serialise to pretty-printed JSON
    pub fn to_json_pretty(&self) -> Result<String> {
        serde_json::to_string_pretty(self)
            .map_err(|e| AuditError::other(format!("JSON serialisation failed: {}", e)))
    }

    // Serialise to compact JSON
    pub fn to_json(&self) -> Result<String> {
        serde_json::to_string(self)
            .map_err(|e| AuditError::other(format!("JSON serialisation failed: {}", e)))
    }

    // Deserialise from a JSON string
    pub fn from_json(json: &str) -> Result<Self> {
        serde_json::from_str(json)
            .map_err(|e| AuditError::other(format!("GamePlan JSON parse error: {}", e)))
    }

    // Load a GamePlan from a `.json` file on disk
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let content = std::fs::read_to_string(path.as_ref()).map_err(AuditError::Io)?;
        Self::from_json(&content)
    }

    // Return batches ordered: ready (no unresolved deps) first, then by priority string
    pub fn ordered_batches(&self) -> Vec<&GamePlanBatch> {
        let completed: Vec<String> = Vec::new();
        let mut ready: Vec<&GamePlanBatch> = self
            .batches
            .iter()
            .filter(|b| b.is_ready(&completed))
            .collect();
        let mut blocked: Vec<&GamePlanBatch> = self
            .batches
            .iter()
            .filter(|b| !b.is_ready(&completed))
            .collect();

        // Sort ready batches: high → medium → low
        ready.sort_by_key(|b| priority_rank(&b.priority));
        blocked.sort_by_key(|b| priority_rank(&b.priority));

        ready.extend(blocked);
        ready
    }
}

fn priority_rank(p: &str) -> u8 {
    match p.to_ascii_lowercase().as_str() {
        "high" => 0,
        "medium" => 1,
        "low" => 2,
        _ => 3,
    }
}

// ============================================================================
// Planner
// ============================================================================

// Generates a `GamePlan` from a `todo.md` file using the xAI LLM
pub struct TodoPlanner {
    config: PlannerConfig,
    client: GrokClient,
}

impl TodoPlanner {
    // Create a planner from environment — reads `XAI_API_KEY` automatically
    pub async fn from_env(config: PlannerConfig, db: crate::db::Database) -> Result<Self> {
        let client = GrokClient::from_env(db)
            .await
            .map_err(|e| AuditError::other(format!("Failed to create GrokClient: {}", e)))?;
        Ok(Self { config, client })
    }

    // Create a planner with an explicit `GrokClient`
    pub fn new(config: PlannerConfig, client: GrokClient) -> Self {
        Self { config, client }
    }

    // -----------------------------------------------------------------------
    // Public API
    // -----------------------------------------------------------------------

    // Generate a `GamePlan` from a `todo.md` file.
    //
    // `source_root` (optional) is the repo root — used to collect relevant
    // source snippets that are referenced in the TODO items.
    pub async fn plan(
        &self,
        todo_path: impl AsRef<Path>,
        source_root: Option<&Path>,
    ) -> Result<GamePlan> {
        let todo_path = todo_path.as_ref();
        let todo_file = TodoFile::load(todo_path)?;

        // Collect pending items
        let pending: Vec<&TodoItem> = todo_file
            .all_items()
            .filter(|i| {
                if self.config.include_done_items {
                    true
                } else {
                    !i.is_done()
                }
            })
            .collect();

        if pending.is_empty() {
            return Ok(GamePlan {
                generated_at: Utc::now(),
                todo_path: todo_path.to_path_buf(),
                model: self.model_name(),
                batches: Vec::new(),
                total_items_planned: 0,
                skipped_items: Vec::new(),
                raw_llm_response: None,
            });
        }

        // Collect optional source context
        let source_context = if let Some(root) = source_root {
            self.collect_source_context(&pending, root)
        } else {
            String::new()
        };

        // Build the prompt
        let prompt = self.build_prompt(&todo_file, &pending, &source_context);

        // Call the LLM
        let raw = self
            .client
            .ask(&prompt, None)
            .await
            .map_err(|e| AuditError::other(format!("LLM call failed: {}", e)))?;

        // Parse the JSON game plan out of the response
        let batches = self.parse_batches_from_response(&raw, &pending)?;

        let total_items_planned = batches.iter().map(|b| b.items.len()).sum();

        // Determine which items were not picked up by the LLM
        let planned_ids: std::collections::HashSet<&str> = batches
            .iter()
            .flat_map(|b| b.items.iter())
            .map(|i| i.todo_id.as_str())
            .collect();

        let skipped_items: Vec<String> = pending
            .iter()
            .filter(|i| !planned_ids.contains(i.id.as_str()))
            .map(|i| i.text.clone())
            .collect();

        Ok(GamePlan {
            generated_at: Utc::now(),
            todo_path: todo_path.to_path_buf(),
            model: self.model_name(),
            batches,
            total_items_planned,
            skipped_items,
            raw_llm_response: Some(raw),
        })
    }

    // -----------------------------------------------------------------------
    // Prompt construction
    // -----------------------------------------------------------------------

    fn build_prompt(
        &self,
        todo_file: &TodoFile,
        pending: &[&TodoItem],
        source_context: &str,
    ) -> String {
        let max_batches = self.config.max_batches;
        let max_per_batch = self.config.max_items_per_batch;

        let items_list = pending
            .iter()
            .enumerate()
            .map(|(n, item)| {
                let priority_tag = todo_file
                    .blocks
                    .iter()
                    .find(|b| {
                        b.sections
                            .iter()
                            .any(|s| s.items.iter().any(|i| i.id == item.id))
                    })
                    .map(|b| match b.priority {
                        Priority::High => "HIGH",
                        Priority::Medium => "MEDIUM",
                        Priority::Low => "LOW",
                        Priority::Notes => "NOTE",
                    })
                    .unwrap_or("MEDIUM");

                format!(
                    "{}. [{}] [id:{}] {}",
                    n + 1,
                    priority_tag,
                    item.id,
                    item.text
                )
            })
            .collect::<Vec<_>>()
            .join("\n");

        let context_section = if source_context.is_empty() {
            String::new()
        } else {
            format!(
                "\n## Relevant source context\n\n```\n{}\n```\n",
                source_context
            )
        };

        let extra = self
            .config
            .extra_instructions
            .as_deref()
            .map(|s| format!("\n## Additional instructions\n\n{}\n", s))
            .unwrap_or_default();

        format!(
            r#"You are a senior Rust engineer helping plan work for the `rustcode` project.

## Task

Analyse the following pending TODO items and produce a structured GAMEPLAN as a JSON object.
Group related items into batches that can be worked on together. Prioritise high-impact,
low-risk items first. Each batch should be completable in a single focused work session.

## Constraints

- Produce at most {max_batches} batches.
- Each batch may contain at most {max_per_batch} items.
- Return ONLY valid JSON — no markdown fences, no prose before or after.
- Every item in the input MUST appear in exactly one batch OR in `skipped_items`.
- Use the `[id:xxxxxxxx]` tokens verbatim as `todo_id` values.
- `estimated_effort` must be one of: trivial, small, medium, large.
- `priority` must be one of: high, medium, low.
- `dependencies` should list batch `id` values of batches that must complete first.

## Pending TODO items

{items_list}
{context_section}{extra}
## Required output shape

{{
  "batches": [
    {{
      "id": "batch-001",
      "title": "<short title>",
      "priority": "high|medium|low",
      "estimated_effort": "trivial|small|medium|large",
      "items": [
        {{
          "todo_id": "<id from input>",
          "description": "<what to do>",
          "files": ["<path>", ...],
          "approach": "<strategy>",
          "acceptance_criteria": "<optional>"
        }}
      ],
      "rationale": "<why group these>",
      "dependencies": ["<batch-id>", ...]
    }}
  ],
  "skipped_items": ["<todo text>", ...]
}}
"#
        )
    }

    // -----------------------------------------------------------------------
    // Response parsing
    // -----------------------------------------------------------------------

    // Extract batches from the raw LLM response text.
    //
    // The LLM is instructed to return pure JSON, but may occasionally wrap it
    // in markdown fences. We try several strategies to extract valid JSON.
    fn parse_batches_from_response(
        &self,
        raw: &str,
        _pending: &[&TodoItem],
    ) -> Result<Vec<GamePlanBatch>> {
        // 1. Try the whole string
        if let Ok(plan) = self.try_parse_plan_json(raw) {
            return Ok(plan);
        }

        // 2. Strip markdown code fences and try again
        let stripped = strip_markdown_fences(raw);
        if let Ok(plan) = self.try_parse_plan_json(&stripped) {
            return Ok(plan);
        }

        // 3. Find the first `{` … last `}` and try that substring
        if let Some(start) = raw.find('{') {
            if let Some(end) = raw.rfind('}') {
                if end > start {
                    let slice = &raw[start..=end];
                    if let Ok(plan) = self.try_parse_plan_json(slice) {
                        return Ok(plan);
                    }
                }
            }
        }

        // 4. Partial-JSON recovery: the LLM response may have been truncated by
        //    the max_tokens limit before it could close the outer JSON object.
        //    Try to salvage complete batch objects by scanning for every
        //    `{"id":` … `}` group and parsing each individually.
        let partial = self.recover_partial_batches(raw);
        if !partial.is_empty() {
            tracing::warn!(
                "LLM game plan response was truncated; recovered {} complete batch(es) via partial parse.",
                partial.len()
            );
            return Ok(partial);
        }

        // 5. Fall back to an empty plan rather than hard-failing — the caller
        //    can inspect `raw_llm_response` to diagnose.
        tracing::warn!(
            "Could not parse LLM game plan JSON. Raw response (first 500 chars): {}",
            &raw[..raw.len().min(500)]
        );
        Ok(Vec::new())
    }

    // Walk `raw` and extract every complete JSON object that looks like a
    // `GamePlanBatch` (i.e. contains an `"id"` field).  This is used as a
    // last-resort recovery path when the LLM response was cut off by
    // `max_tokens` before the outer wrapper was closed.
    fn recover_partial_batches(&self, raw: &str) -> Vec<GamePlanBatch> {
        let mut batches = Vec::new();
        let bytes = raw.as_bytes();
        let len = bytes.len();
        let mut pos = 0;

        while pos < len {
            // Find the next `{`
            let Some(start) = raw[pos..].find('{').map(|o| pos + o) else {
                break;
            };

            // Walk forward tracking brace depth to find the matching `}`
            let mut depth: i32 = 0;
            let mut in_string = false;
            let mut escape_next = false;
            let mut end = None;

            for (i, &b) in bytes[start..].iter().enumerate() {
                if escape_next {
                    escape_next = false;
                    continue;
                }
                match b {
                    b'\\' if in_string => escape_next = true,
                    b'"' => in_string = !in_string,
                    b'{' if !in_string => depth += 1,
                    b'}' if !in_string => {
                        depth -= 1;
                        if depth == 0 {
                            end = Some(start + i);
                            break;
                        }
                    }
                    _ => {}
                }
            }

            let Some(end) = end else {
                // No matching closing brace found — rest of string is truncated.
                break;
            };

            let slice = &raw[start..=end];
            // Only try to parse objects that look like a batch (contain `"id"`)
            if slice.contains("\"id\"") {
                if let Ok(batch) = serde_json::from_str::<GamePlanBatch>(slice) {
                    batches.push(batch);
                }
            }

            pos = end + 1;
        }

        batches
    }

    fn try_parse_plan_json(&self, s: &str) -> std::result::Result<Vec<GamePlanBatch>, ()> {
        #[derive(Deserialize)]
        struct LlmPlanResponse {
            batches: Vec<GamePlanBatch>,
        }

        serde_json::from_str::<LlmPlanResponse>(s)
            .map(|r| r.batches)
            .map_err(|_| ())
    }

    // -----------------------------------------------------------------------
    // Source context collection
    // -----------------------------------------------------------------------

    // Walk the todo items looking for file references (e.g. `src/api/handlers.rs:132`)
    // and return a condensed snippet of relevant source code for the LLM.
    fn collect_source_context(&self, pending: &[&TodoItem], repo_root: &Path) -> String {
        use std::collections::HashMap;

        // Extract `path:line` references from item text
        let file_line_re =
            regex::Regex::new(r"(src/[^\s:]+\.rs)(?::(\d+))?").unwrap_or_else(|_| {
                regex::Regex::new(r"$^").unwrap() // never matches — safe fallback
            });

        // Map: file_path -> list of (line_number, item_text)
        let mut file_refs: HashMap<PathBuf, Vec<(usize, &str)>> = HashMap::new();

        for item in pending {
            for cap in file_line_re.captures_iter(&item.text) {
                let rel_path = PathBuf::from(&cap[1]);
                let line: usize = cap
                    .get(2)
                    .and_then(|m| m.as_str().parse().ok())
                    .unwrap_or(1);
                file_refs
                    .entry(rel_path)
                    .or_default()
                    .push((line, &item.text));
            }
        }

        let mut snippets: Vec<String> = Vec::new();
        let max_snippets = self.config.max_context_snippets;
        let max_chars = self.config.max_snippet_chars;

        'outer: for (rel_path, refs) in file_refs.iter().take(max_snippets) {
            let abs_path = repo_root.join(rel_path);
            let content = match std::fs::read_to_string(&abs_path) {
                Ok(c) => c,
                Err(_) => continue 'outer,
            };

            let lines: Vec<&str> = content.lines().collect();

            for &(lineno, item_text) in refs {
                let start = lineno.saturating_sub(5).saturating_sub(1);
                let end = (lineno + 10).min(lines.len());

                let snippet: String = lines[start..end]
                    .iter()
                    .enumerate()
                    .map(|(i, l)| format!("{:4} | {}", start + i + 1, l))
                    .collect::<Vec<_>>()
                    .join("\n");

                let snippet_truncated = if snippet.len() > max_chars {
                    format!("{}… [truncated]", &snippet[..max_chars])
                } else {
                    snippet
                };

                snippets.push(format!(
                    "// {} (referenced by: {})\n{}",
                    rel_path.display(),
                    item_text,
                    snippet_truncated
                ));

                if snippets.len() >= max_snippets {
                    break 'outer;
                }
            }
        }

        snippets.join("\n\n---\n\n")
    }

    fn model_name(&self) -> String {
        self.config
            .model
            .clone()
            .unwrap_or_else(|| "grok-4-turbo".to_string())
    }
}

// ============================================================================
// Helpers
// ============================================================================

// Remove ` ```json `, ` ``` `, and similar markdown fences from LLM output
fn strip_markdown_fences(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_fence = false;

    for line in s.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("```") {
            in_fence = !in_fence;
            continue; // skip the fence line itself
        }
        if !in_fence || trimmed.starts_with('{') || trimmed.starts_with('"') {
            out.push_str(line);
            out.push('\n');
        }
    }

    // If we never actually hit a fence, return the original
    if out.trim().is_empty() {
        s.to_string()
    } else {
        out
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_strip_markdown_fences_plain_json() {
        let input = r#"{"batches":[],"skipped_items":[]}"#;
        let result = strip_markdown_fences(input);
        assert!(result.contains("batches"));
    }

    #[test]
    fn test_strip_markdown_fences_with_fence() {
        let input = "Sure! Here's the plan:\n```json\n{\"batches\":[],\"skipped_items\":[]}\n```";
        let result = strip_markdown_fences(input);
        assert!(result.contains("batches"));
        assert!(!result.contains("```"));
    }

    #[test]
    fn test_gameplan_serialise_round_trip() {
        let plan = GamePlan {
            generated_at: Utc::now(),
            todo_path: PathBuf::from("todo.md"),
            model: "grok-4-turbo".to_string(),
            batches: vec![GamePlanBatch {
                id: "batch-001".to_string(),
                title: "Fix admin module".to_string(),
                priority: "high".to_string(),
                estimated_effort: EffortEstimate::Small,
                items: vec![BatchWorkItem {
                    todo_id: "deadbeef".to_string(),
                    description: "Uncomment pub mod admin".to_string(),
                    files: vec!["src/api/mod.rs".to_string()],
                    approach: "Align ApiState fields".to_string(),
                    acceptance_criteria: Some("admin routes return 200".to_string()),
                }],
                rationale: "Admin routes are currently dead code".to_string(),
                dependencies: Vec::new(),
            }],
            total_items_planned: 1,
            skipped_items: Vec::new(),
            raw_llm_response: None,
        };

        let json = plan.to_json_pretty().unwrap();
        let loaded = GamePlan::from_json(&json).unwrap();

        assert_eq!(loaded.batches.len(), 1);
        assert_eq!(loaded.batches[0].id, "batch-001");
        assert_eq!(loaded.batches[0].items[0].todo_id, "deadbeef");
        assert_eq!(loaded.batches[0].estimated_effort, EffortEstimate::Small);
    }

    #[test]
    fn test_ordered_batches_ready_first() {
        let plan = GamePlan {
            generated_at: Utc::now(),
            todo_path: PathBuf::from("todo.md"),
            model: "grok-4-turbo".to_string(),
            batches: vec![
                GamePlanBatch {
                    id: "batch-002".to_string(),
                    title: "Depends on 001".to_string(),
                    priority: "high".to_string(),
                    estimated_effort: EffortEstimate::Medium,
                    items: Vec::new(),
                    rationale: String::new(),
                    dependencies: vec!["batch-001".to_string()],
                },
                GamePlanBatch {
                    id: "batch-001".to_string(),
                    title: "No deps".to_string(),
                    priority: "medium".to_string(),
                    estimated_effort: EffortEstimate::Small,
                    items: Vec::new(),
                    rationale: String::new(),
                    dependencies: Vec::new(),
                },
            ],
            total_items_planned: 0,
            skipped_items: Vec::new(),
            raw_llm_response: None,
        };

        let ordered = plan.ordered_batches();
        assert_eq!(ordered[0].id, "batch-001"); // ready batch comes first
        assert_eq!(ordered[1].id, "batch-002"); // blocked batch comes second
    }

    #[test]
    fn test_priority_rank_ordering() {
        assert!(priority_rank("high") < priority_rank("medium"));
        assert!(priority_rank("medium") < priority_rank("low"));
    }

    #[test]
    fn test_effort_estimate_round_trip() {
        use std::str::FromStr;
        for (s, expected) in &[
            ("trivial", EffortEstimate::Trivial),
            ("small", EffortEstimate::Small),
            ("medium", EffortEstimate::Medium),
            ("large", EffortEstimate::Large),
        ] {
            let parsed = EffortEstimate::from_str(s).unwrap();
            assert_eq!(&parsed, expected);
            assert_eq!(parsed.as_str(), *s);
        }
    }

    #[test]
    fn test_is_ready_with_completed_deps() {
        let batch = GamePlanBatch {
            id: "batch-002".to_string(),
            title: "Blocked".to_string(),
            priority: "medium".to_string(),
            estimated_effort: EffortEstimate::Small,
            items: Vec::new(),
            rationale: String::new(),
            dependencies: vec!["batch-001".to_string()],
        };

        assert!(!batch.is_ready(&[] as &[String]));
        assert!(batch.is_ready(&["batch-001".to_string()]));
    }
}
