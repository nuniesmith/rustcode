// LLM usage tracking: per-call token budgets and cumulative cost telemetry.
//
// RC-CLEANUP-A slice 2 (2026-05-21): both submodules moved here from the
// top-level `src/` flat layout (`token_budget.rs`, `cost_tracker.rs`).
// Pure restructure — no behaviour change.

pub mod budget;
pub mod costs;
