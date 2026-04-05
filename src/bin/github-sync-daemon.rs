// GitHub Background Sync Daemon
//
// A simple daemon that runs continuous background sync for GitHub repositories.
// This is designed to run as a long-lived process or service.

use rustcode::db::init_db;
use rustcode::github::{
    start_background_sync_with_config, BackgroundSyncConfig, GitHubClient,
};
use std::env;
use tokio::signal;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Load environment variables
    dotenvy::dotenv().ok();

    // Initialize tracing
    tracing_subscriber::fmt()
        .with_env_filter(
            env::var("RUST_LOG").unwrap_or_else(|_| "info,rustcode=debug".to_string()),
        )
        .init();

    tracing::info!("🚀 Starting GitHub Background Sync Daemon");

    // Get GitHub token
    let token = env::var("GITHUB_TOKEN").expect(
        "GITHUB_TOKEN environment variable must be set. \
         Create one at https://github.com/settings/tokens",
    );

    // Get database URL
    let database_url =
        env::var("DATABASE_URL").unwrap_or_else(|_| "sqlite:data/rustcode.db".to_string());

    tracing::info!("📁 Database: {}", database_url);

    // Initialize database
    let pool = init_db(&database_url).await?;
    tracing::info!("✅ Database initialized");

    // Initialize GitHub client
    let client = GitHubClient::new(&token)?;
    tracing::info!("✅ GitHub client initialized");

    // Configure background sync
    let config = BackgroundSyncConfig {
        full_sync_interval: env::var("GITHUB_FULL_SYNC_INTERVAL")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(86400), // 24 hours default
        incremental_sync_interval: env::var("GITHUB_INCREMENTAL_SYNC_INTERVAL")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(3600), // 1 hour default
        max_items_per_repo: env::var("GITHUB_MAX_ITEMS_PER_REPO")
            .ok()
            .and_then(|s| s.parse().ok()),
        sync_on_startup: env::var("GITHUB_SYNC_ON_STARTUP")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(true),
    };

    tracing::info!("⚙️  Sync Configuration:");
    tracing::info!(
        "   Full sync interval: {} seconds ({} hours)",
        config.full_sync_interval,
        config.full_sync_interval / 3600
    );
    tracing::info!(
        "   Incremental sync: {} seconds ({} minutes)",
        config.incremental_sync_interval,
        config.incremental_sync_interval / 60
    );
    tracing::info!("   Max items per repo: {:?}", config.max_items_per_repo);
    tracing::info!("   Sync on startup: {}", config.sync_on_startup);

    tracing::info!("🎯 Starting background sync loop...");
    tracing::info!("   Press Ctrl+C to shutdown gracefully");

    // Set up graceful shutdown handler
    let shutdown = async {
        signal::ctrl_c()
            .await
            .expect("Failed to install CTRL+C signal handler");
        tracing::info!("🛑 Shutdown signal received, stopping sync...");
    };

    // Run background sync with graceful shutdown
    tokio::select! {
        result = start_background_sync_with_config(pool, client, config) => {
            if let Err(e) = result {
                tracing::error!("❌ Background sync failed: {}", e);
                return Err(anyhow::anyhow!("Background sync failed: {e}"));
            }
        }
        () = shutdown => {
            tracing::info!("✅ Daemon stopped gracefully");
        }
    }

    Ok(())
}
