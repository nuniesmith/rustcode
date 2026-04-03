// src/sync_scheduler.rs
// Background sync scheduler for registered repos
// TODO: hook into GitHub webhook events for push-triggered syncs

use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{RwLock, Semaphore};
use tokio::task::JoinSet;
use tokio::time;
use tracing::{error, info, warn};

use crate::repo_sync::RepoSyncService;

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct SyncSchedulerConfig {
    /// How often to run background syncs (default: 5 minutes)
    pub interval: Duration,
    /// Max repos to sync concurrently
    pub concurrency: usize,
    /// Skip repos that were synced within this window
    pub skip_if_synced_within: Duration,
}

impl Default for SyncSchedulerConfig {
    fn default() -> Self {
        Self {
            interval: Duration::from_secs(300), // 5 min
            concurrency: std::env::var("REPO_SYNC_CONCURRENCY")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(3),
            skip_if_synced_within: Duration::from_secs(60),
        }
    }
}

// ---------------------------------------------------------------------------
// Scheduler
// ---------------------------------------------------------------------------

pub struct SyncScheduler {
    config: SyncSchedulerConfig,
    service: Arc<RwLock<RepoSyncService>>,
}

impl SyncScheduler {
    pub fn new(config: SyncSchedulerConfig, service: Arc<RwLock<RepoSyncService>>) -> Self {
        Self { config, service }
    }

    /// Spawn the background sync loop. Call once at startup.
    pub fn start(self) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            info!(
                interval_secs = self.config.interval.as_secs(),
                concurrency = self.config.concurrency,
                "SyncScheduler started"
            );
            let mut ticker = time::interval(self.config.interval);
            // The first tick fires immediately — skip it so we don't hammer the
            // disk before the server has finished starting up.
            ticker.tick().await;
            loop {
                ticker.tick().await;
                self.run_sync_pass().await;
            }
        })
    }

    // -----------------------------------------------------------------------
    // Sync pass — concurrent with semaphore-gated JoinSet
    // -----------------------------------------------------------------------

    async fn run_sync_pass(&self) {
        // Collect the IDs of repos that are due for a sync.
        let due_ids: Vec<String> = {
            let service = self.service.read().await;
            let threshold = self.config.skip_if_synced_within.as_secs();
            service
                .list_repos()
                .into_iter()
                .filter(|r| {
                    match r.last_synced {
                        // Synced recently — skip.
                        Some(last) => unix_now().saturating_sub(last) >= threshold,
                        // Never synced — always include.
                        None => true,
                    }
                })
                .map(|r| r.id.clone())
                .collect()
        };

        if due_ids.is_empty() {
            return;
        }

        info!(
            count = due_ids.len(),
            concurrency = self.config.concurrency,
            "Running scheduled sync pass"
        );

        // Gate concurrency with a semaphore so we never run more than
        // `config.concurrency` syncs simultaneously.
        let semaphore = Arc::new(Semaphore::new(self.config.concurrency));
        let mut join_set: JoinSet<(String, anyhow::Result<crate::repo_sync::SyncResult>)> =
            JoinSet::new();

        for id in due_ids {
            let sem = Arc::clone(&semaphore);
            let service = Arc::clone(&self.service);
            let id_clone = id.clone();

            join_set.spawn(async move {
                // Acquire a permit before starting the sync.
                let _permit = sem.acquire().await.expect("Semaphore closed");

                let result = {
                    let mut svc = service.write().await;
                    svc.sync(&id_clone).await
                };

                (id_clone, result)
            });
        }

        // Drain results as tasks complete.
        while let Some(outcome) = join_set.join_next().await {
            match outcome {
                Ok((id, Ok(result))) => {
                    info!(
                        repo = %id,
                        files = result.files_walked,
                        todos = result.todos_found,
                        symbols = result.symbols_found,
                        duration_ms = result.duration_ms,
                        "Scheduled sync complete"
                    );
                    if !result.errors.is_empty() {
                        warn!(
                            repo = %id,
                            errors = ?result.errors,
                            "Sync completed with non-fatal errors"
                        );
                    }
                }
                Ok((id, Err(e))) => {
                    error!(repo = %id, error = %e, "Scheduled sync failed");
                }
                Err(join_err) => {
                    error!(error = %join_err, "Sync task panicked");
                }
            }
        }
    }
}

fn unix_now() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

// =============================================================================
// .rustcode/  directory spec
// =============================================================================
//
// Written here as a doc constant so it can be emitted by the registration flow.

pub const RUSTASSISTANT_DIR_SPEC: &str = r#"
# .rustcode/ Directory Specification
# Generated and managed by RustCode — do not edit manually.
#
# manifest.json   — repo identity, sync timestamps, crate metadata
# tree.txt        — full file tree snapshot (regenerated on sync)
# todos.json      — all TODO/STUB/FIXME/HACK tags with file:line refs
# symbols.json    — public functions, structs, traits, impls
# context.md      — human-readable summary injected into LLM prompts
# embeddings.bin  — cached vector embeddings (excluded from git)
#
# Add to .gitignore:
#   .rustcode/embeddings.bin
#
# Commit everything else — the cache files are useful diffs across branches.
"#;

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_concurrency_gt_zero() {
        let cfg = SyncSchedulerConfig::default();
        assert!(cfg.concurrency > 0);
        assert!(cfg.interval.as_secs() > 0);
        assert!(cfg.skip_if_synced_within.as_secs() > 0);
    }

    #[test]
    fn unix_now_is_nonzero() {
        assert!(unix_now() > 0);
    }
}
