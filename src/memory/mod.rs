// Persistent agent memory layer.
//
// `AgentMemory` (in `store.rs`) is a Postgres-backed store for the
// observations, decisions, preferences, patterns, and task outcomes the
// agent loop accumulates over time. MEM-A delivers the store; MEM-B will
// thread `AgentMemory::search` into the planner / executor / reviewer
// prompts so each new task starts grounded in what we already know.
//
// Embeddings come from the existing `crate::embeddings::EmbeddingGenerator`
// (a shared `Arc` works fine — the underlying fastembed model is loaded
// lazily on first use). Schema lives in `sql/023_agent_memory.sql`.

pub mod store;
pub mod types;

pub use store::{AgentMemory, DEFAULT_IMPORTANCE, MAX_CANDIDATES};
pub use types::{MemoryEntry, MemoryKind, MemorySearchHit, NewMemory, cosine_similarity};
