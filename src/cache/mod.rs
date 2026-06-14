// Cache subsystem — file-based audit cache, multi-tier Redis+LRU cache,
// and the Postgres-backed LLM response cache.
//
// Before RC-CLEANUP-B these lived as top-level siblings
// (`src/cache.rs`, `src/cache_layer.rs`, `src/response_cache.rs`).
// They've been consolidated under `crate::cache::*` without behaviour
// changes; this module groups them and re-exports the `audit` submodule's
// public API at the module root so callers that imported from the previous
// `crate::cache::*` path keep working unchanged. The SQLite->SQL
// `cache_migrate.rs` was removed in CLEANUP-H once SQLite was dropped.
//
// Naming-collision note: each of `audit`, `layer`, and `responses`
// defines its own `CacheStats` struct. To avoid an ambiguity at the
// module root we only re-export `audit::CacheStats` here (it's the
// one external callers see as `rustcode::CacheStats`). The other two
// are reached through their submodule paths
// (`crate::cache::layer::CacheStats`, `crate::cache::responses::CacheStats`)
// or via the top-level aliases in `src/lib.rs`
// (`CacheLayerStats`, `ResponseCacheStats`).

pub mod audit;
pub mod layer;
pub mod responses;

// Re-export the `audit` submodule's public API at the module root
// so existing `use crate::cache::{AuditCache, CacheEntry, CACHE_DIR}`
// imports continue to resolve. These were the items exported by the
// original `src/cache.rs`.
pub use audit::{AuditCache, CACHE_DIR, CacheEntry, CacheStats};
