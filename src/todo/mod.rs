// Todo module — Rust-native TODO system
//
// This module provides the full lifecycle for managing `todo.md` files and
// inline source-code TODO comments:
//
// | Sub-module       | Responsibility                                                              |
// |------------------|-----------------------------------------------------------------------------|
// | `todo_file`      | Parse, mutate, and write back `todo.md` (`TodoFile` struct)                 |
// | `scanner`        | Walk source trees, extract TODO/FIXME/HACK/XXX comment items                |
// | `scaffolder`     | **Step 1** — read `todo.md`, generate all files/folders/stubs on disk       |
// | `planner`        | **Step 2** — call xAI LLM to generate a batched GAMEPLAN from `todo.md`    |
// | `worker`         | **Step 3** — execute a single gameplan batch: generate + apply code changes |
// | `sync`           | **Step 4** — update `todo.md` status markers after a batch run              |
//
// # Pipeline overview
//
// ```text
// todo.md (source of truth)
//    │
//    ├─▶ todo-scan     — extract inline TODO/FIXME/HACK/XXX comments → JSON
//    │
//    ├─▶ todo-scaffold — Step 1: read todo.md, ask LLM what files/dirs need
//    │                   to exist, create stubs on disk, update todo.md with
//    │                   a "Scaffolded files" section so later steps know
//    │                   where to go.  Idempotent — safe to re-run.
//    │
//    ├─▶ todo-plan     — Step 2: read updated todo.md + source context, call
//    │                   xAI to produce a batched GAMEPLAN JSON.
//    │
//    ├─▶ todo-work     — Step 3: execute one batch from the GAMEPLAN.
//    │                   Reads source stubs, generates real code via LLM,
//    │                   applies file changes, writes WorkResult JSON.
//    │
//    └─▶ todo-sync     — Step 4: apply WorkResult JSON back to todo.md,
//                        marking items ✅ / ⚠️ / ❌ as appropriate.
// ```
//
// # CLI commands (wired in `src/bin/cli.rs`)
//
// ```text
// rustcode todo-scan     <repo-path>   [--json] [--filter high|medium|low]
// rustcode todo-scaffold <repo-path>   [--dry-run] [--overwrite] [--output scaffold.json]
// rustcode todo-plan     <todo-md>     [--context <dir>] [--output gameplan.json]
// rustcode todo-work     <batch-json>  [--dry-run]
// rustcode todo-sync     <todo-md>     <results-json> [--dry-run] [--append-summary]
// ```
//
// # Testing against rustcode itself
//
// The pipeline is designed to be self-hosting: running it against the
// `rustcode` repo is the canonical integration test.  All repos managed
// by RustCode share the same project layout conventions, so the same
// binary works across every project.

pub mod planner;
pub mod scaffolder;
pub mod scanner;
pub mod sync;
pub mod todo_file;
pub mod worker;

// ============================================================================
// Convenience re-exports
// ============================================================================

pub use planner::{BatchWorkItem, GamePlan, GamePlanBatch, PlannerConfig, TodoPlanner};
pub use scaffolder::{
    EntryKind, EntryOutcome, EntryResult, ScaffoldConfig, ScaffoldEntry, ScaffoldPlan,
    ScaffoldResult, TodoScaffolder,
};
pub use scanner::{
    CommentKind, CommentPriority, ScanConfig, ScanOutput, ScanSummary, TodoCommentItem,
    TodoCommentScanner,
};
pub use sync::{OldStatus, SyncChange, SyncConfig, SyncResult, TodoSyncer};
pub use todo_file::{
    CheckboxState, Priority, PriorityBlock, StatusMarker, TodoCounts, TodoFile, TodoItem,
    TodoSection,
};
pub use worker::{
    FileChange, FileChangeType, ItemResult, ItemStatus, TodoWorker, WorkBatch, WorkConfig,
    WorkResult,
};
