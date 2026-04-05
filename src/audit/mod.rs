// Audit module — backend for the `/api/audit` endpoint
//
// This module exposes the LLM audit pipeline as a first-class Rust API so the
// `llm-audit.yml` workflow can POST to `rustcode` instead of running raw
// Python. Results are cached in Redis (same `allkeys-lru` pool used for LLM
// responses) and written to `docs/audit/` for commit history.
//
// # Sub-modules (to be implemented)
//
// | Sub-module    | Responsibility                                                        |
// |---------------|-----------------------------------------------------------------------|
// | `endpoint`    | Axum handler for `GET /api/audit` and `POST /api/audit`               |
// | `runner`      | Orchestrates `StaticAnalyzer` → `GrokClient` → result serialisation  |
// | `report`      | Renders audit findings to Markdown / JSON for `docs/audit/`           |
// | `cache`       | Redis-backed deduplication — skip files whose hash hasn't changed     |
//
// # Planned CLI command
//
// ```text
// rustcode audit <repo-path> [--output docs/audit/report.md] [--json]
// ```
//
// # Integration notes
//
// - Wired into `src/server.rs` router under `/api/audit`.
// - Uses `src/llm_audit.rs` for the core audit logic (already exists).
// - Uses `src/auto_scanner.rs` for file triage before LLM calls.
// - Uses `src/grok_client.rs` for xAI API calls with cost tracking.
// - Results are appended to the target repo's `todo.md` via `TodoFile::append_item`
//   (see `src/todo/todo_file.rs`) so new findings automatically enter the pipeline.
//
// # TODO(scaffolder): implement
//
// All sub-modules below are stubs. Implement in this order:
// 1. `types`    — shared request/response structs
// 2. `runner`   — core orchestration logic
// 3. `report`   — Markdown + JSON rendering
// 4. `cache`    — Redis dedup layer
// 5. `endpoint` — Axum handler wiring everything together

pub mod cache;
pub mod endpoint;
pub mod full_audit;
pub mod report;
pub mod runner;
pub mod types;

// ============================================================================
// Convenience re-exports
// ============================================================================

pub use cache::{AuditCache, AuditCacheConfig};
pub use endpoint::{audit_router, handle_audit_get, handle_audit_post};
pub use full_audit::{
    db_get_audit_report_json, db_get_audit_report_markdown, db_get_audit_status,
    db_get_runs_for_repo, db_list_audit_runs, AuditRunStatus, AuditRunSummary, FileAuditResult,
    FileSeverity, FullAuditConfig, FullAuditEngine, FullAuditReport,
};
pub use report::{AuditReport, ReportFormat};
pub use runner::{AuditRunner, AuditRunnerConfig};
pub use types::{AuditFinding, AuditRequest, AuditResponse, AuditSeverity, AuditStatus};
