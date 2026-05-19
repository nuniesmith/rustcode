// Context module — context-building infrastructure for LLM analysis.
//
// Two submodules with related but distinct responsibilities:
//
// - `global` — builds a `GlobalContextBundle` for deep code analysis:
//   signature maps, dependency graphs, architectural rules, diff
//   context, test coverage. Geared at 2M-token-window analyses where
//   the model wants holistic codebase understanding. Previously at
//   `src/context_llm.rs`.
//
// - `rag` — builds RAG-shaped context for response augmentation: load
//   repo files into a query-aware context window, manage token
//   budgets, cache responses. Previously at `src/context_rag.rs`.
//
// Consolidated under `src/context/` in RC-CLEANUP-E so the
// related-but-distinct concerns sit together instead of dangling as
// two top-level siblings with no grouping.

pub mod global;
pub mod rag;
