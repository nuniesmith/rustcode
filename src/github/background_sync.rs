//! Background sync job system for GitHub integration
//!
//! This module provides a background job system that periodically syncs
//! GitHub data to keep the local database up-to-date.

use super::{GitHubClient, SyncEngine, SyncOptions};
use sqlx::PgPool;
use std::sync::Arc;
use std::time::Duration;
use tokio::time::interval;
use tracing::{error, info, warn};

/// Configuration for background sync jobs
#[derive(Debug, Clone)]
pub struct BackgroundSyncConfig {
    /// Interval between full syncs (in seconds)
    pub full_sync_interval: u64,

    /// Interval between incremental syncs (in seconds)
    pub incremental_sync_interval: u64,

    /// Maximum number of items to sync per repository
    pub max_items_per_repo: Option<i64>,

    /// Enable automatic sync on startup
    pub sync_on_startup: bool,
}

impl Default for BackgroundSyncConfig {
    fn default() -> Self {
        Self {
            full_sync_interval: 86400,       // 24 hours
            incremental_sync_interval: 3600, // 1 hour
            max_items_per_repo: Some(100),
            sync_on_startup: true,
        }
    }
}

/// Background sync job manager
pub struct BackgroundSyncManager {
    pool: PgPool,
    client: GitHubClient,
    config: BackgroundSyncConfig,
}

impl BackgroundSyncManager {
    /// Create a new background sync manager
    pub fn new(pool: PgPool, client: GitHubClient, config: BackgroundSyncConfig) -> Self {
        Self {
            pool,
            client,
            config,
        }
    }

    /// Start the background sync job
    ///
    /// This will spawn a background task that runs indefinitely,
    /// performing periodic syncs based on the configuration.
    pub async fn start(self) -> Result<(), Box<dyn std::error::Error>> {
        let manager = Arc::new(self);

        // Initial sync on startup if enabled
        if manager.config.sync_on_startup {
            info!("🚀 Running initial GitHub sync on startup...");
            let manager_clone = Arc::clone(&manager);
            tokio::spawn(async move {
                if let Err(e) = manager_clone.run_full_sync().await {
                    error!("Failed to run initial sync: {}", e);
                } else {
                    info!("✅ Initial sync completed successfully");
                }
            });
        }

        // Start incremental sync loop
        let manager_clone = Arc::clone(&manager);
        let incremental_handle = tokio::spawn(async move {
            manager_clone.run_incremental_sync_loop().await;
        });

        // Start full sync loop
        let manager_clone = Arc::clone(&manager);
        let full_handle = tokio::spawn(async move {
            manager_clone.run_full_sync_loop().await;
        });

        info!(
            "📡 Background sync started (incremental: {}s, full: {}s)",
            manager.config.incremental_sync_interval, manager.config.full_sync_interval
        );

        // Wait for both tasks (they run indefinitely)
        tokio::try_join!(incremental_handle, full_handle)?;

        Ok(())
    }

    /// Run incremental sync loop
    async fn run_incremental_sync_loop(&self) {
        let mut timer = interval(Duration::from_secs(self.config.incremental_sync_interval));

        loop {
            timer.tick().await;

            info!("🔄 Running incremental GitHub sync...");
            if let Err(e) = self.run_incremental_sync().await {
                error!("Incremental sync failed: {}", e);
            } else {
                info!("✅ Incremental sync completed");
            }
        }
    }

    /// Run full sync loop
    async fn run_full_sync_loop(&self) {
        let mut timer = interval(Duration::from_secs(self.config.full_sync_interval));

        loop {
            timer.tick().await;

            info!("🔄 Running full GitHub sync...");
            if let Err(e) = self.run_full_sync().await {
                error!("Full sync failed: {}", e);
            } else {
                info!("✅ Full sync completed");
            }
        }
    }

    /// Perform an incremental sync
    async fn run_incremental_sync(&self) -> Result<(), Box<dyn std::error::Error>> {
        let sync_engine = SyncEngine::new(self.client.clone(), self.pool.clone());

        // Sync only recent items
        let result = sync_engine
            .sync_with_options(SyncOptions::default())
            .await?;
        info!("  Synced {} repositories", result.repos_synced);
        info!("  Synced {} issues", result.issues_synced);
        info!("  Synced {} pull requests", result.prs_synced);

        // Update last sync timestamp
        self.update_last_sync_time().await?;

        Ok(())
    }

    /// Perform a full sync
    async fn run_full_sync(&self) -> Result<(), Box<dyn std::error::Error>> {
        let sync_engine = SyncEngine::new(self.client.clone(), self.pool.clone());

        // Full sync without limits
        let result = sync_engine
            .sync_with_options(SyncOptions::default().force_full())
            .await?;
        info!("  Synced {} repositories", result.repos_synced);
        info!("  Synced {} issues", result.issues_synced);
        info!("  Synced {} pull requests", result.prs_synced);

        // Update last sync timestamp
        self.update_last_sync_time().await?;

        Ok(())
    }

    /// Update the last sync timestamp in the database
    async fn update_last_sync_time(&self) -> Result<(), Box<dyn std::error::Error>> {
        sqlx::query(
            r#"
            INSERT INTO github_sync_metadata (key, value, updated_at)
            VALUES ('last_sync', now()::text, now()::text)
            ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value, updated_at = EXCLUDED.updated_at
            "#,
        )
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    /// Get the last sync time
    pub async fn get_last_sync_time(&self) -> Result<Option<String>, Box<dyn std::error::Error>> {
        let result: Option<String> =
            sqlx::query_scalar("SELECT value FROM github_sync_metadata WHERE key = 'last_sync'")
                .fetch_optional(&self.pool)
                .await?;

        Ok(result)
    }

    /// Trigger a manual sync
    pub async fn trigger_manual_sync(&self, full: bool) -> Result<(), Box<dyn std::error::Error>> {
        if full {
            info!("🔄 Manual full sync triggered");
            self.run_full_sync().await?;
        } else {
            info!("🔄 Manual incremental sync triggered");
            self.run_incremental_sync().await?;
        }

        Ok(())
    }

    /// Check GitHub API rate limits
    pub async fn check_rate_limits(&self) -> Result<(), Box<dyn std::error::Error>> {
        let rate_limit = self.client.get_rate_limit().await?;

        if rate_limit.resources.core.remaining < 100 {
            warn!(
                "⚠️  GitHub API rate limit low: {}/{} remaining",
                rate_limit.resources.core.remaining, rate_limit.resources.core.limit
            );
        }

        if rate_limit.resources.search.remaining < 10 {
            warn!(
                "⚠️  GitHub Search API rate limit low: {}/{} remaining",
                rate_limit.resources.search.remaining, rate_limit.resources.search.limit
            );
        }

        Ok(())
    }
}

/// Start background sync with default configuration
pub async fn start_background_sync(
    pool: PgPool,
    client: GitHubClient,
) -> Result<(), Box<dyn std::error::Error>> {
    let config = BackgroundSyncConfig::default();
    let manager = BackgroundSyncManager::new(pool, client, config);
    manager.start().await
}

/// Start background sync with custom configuration
pub async fn start_background_sync_with_config(
    pool: PgPool,
    client: GitHubClient,
    config: BackgroundSyncConfig,
) -> Result<(), Box<dyn std::error::Error>> {
    let manager = BackgroundSyncManager::new(pool, client, config);
    manager.start().await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = BackgroundSyncConfig::default();
        assert_eq!(config.full_sync_interval, 86400);
        assert_eq!(config.incremental_sync_interval, 3600);
        assert_eq!(config.max_items_per_repo, Some(100));
        assert!(config.sync_on_startup);
    }

    #[test]
    fn test_custom_config() {
        let config = BackgroundSyncConfig {
            full_sync_interval: 7200,
            incremental_sync_interval: 1800,
            max_items_per_repo: Some(50),
            sync_on_startup: false,
        };

        assert_eq!(config.full_sync_interval, 7200);
        assert_eq!(config.incremental_sync_interval, 1800);
        assert_eq!(config.max_items_per_repo, Some(50));
        assert!(!config.sync_on_startup);
    }
}
