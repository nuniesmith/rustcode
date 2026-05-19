// Repository subsystem — caching, syncing, analysis, and management.
//
// RC-CLEANUP-A consolidates five top-level repo-related siblings into
// this module:
//
//   src/repo_cache.rs       -> src/repo/file_cache.rs   (file-based; legacy)
//   src/repo_cache_sql.rs   -> src/repo/cache.rs        (SQL-backed; canonical)
//   src/repo_manager.rs     -> src/repo/manager.rs
//   src/repo_sync.rs        -> src/repo/sync.rs
//   src/repo_analysis.rs    -> src/repo/analysis.rs
//
// `file_cache` is the original file-based cache implementation. The SQL
// path (`cache`) replaced it in production; the file-based version is
// kept for a deprecation window so external callers depending on
// `rustcode::RepoCache` keep working. A follow-up PR will delete it
// after verification — the only remaining inbound references inside the
// crate are from `cache_migrate.rs` (which only exists to move data
// between the two) and the deprecated re-exports block.
//
// Beyond the move, no behaviour changes here. Public APIs are
// re-exported through `lib.rs` so external callers' import paths stay
// the same.

pub mod analysis;
pub mod cache;
pub mod file_cache;
pub mod manager;
pub mod sync;
