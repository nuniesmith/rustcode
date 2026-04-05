//! Database Configuration
//!
//! Handles PostgreSQL connection pool configuration and initialization.
//! Reads DATABASE_URL from the environment (set via .env or docker-compose).

use anyhow::{Context, Result};
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use tracing::info;

// ============================================================================
// Configuration
// ============================================================================

/// Database configuration loaded from environment
#[derive(Debug, Clone)]
pub struct DatabaseConfig {
    /// Full Postgres connection URL
    /// e.g. postgresql://rustcode:changeme@postgres:5432/rustcode
    pub url: String,
    /// Whether to run migrations on startup
    pub auto_migrate: bool,
    /// Maximum connections in pool
    pub max_connections: u32,
    /// Whether this is a development environment
    pub is_dev: bool,
}

impl Default for DatabaseConfig {
    fn default() -> Self {
        Self {
            url: get_default_db_url(),
            auto_migrate: true,
            max_connections: 10,
            is_dev: cfg!(debug_assertions),
        }
    }
}

impl DatabaseConfig {
    /// Load configuration from environment variables
    pub fn from_env() -> Self {
        let url = std::env::var("DATABASE_URL").unwrap_or_else(|_| get_default_db_url());

        let auto_migrate = std::env::var("RUSTASSISTANT_AUTO_MIGRATE")
            .map(|v| v == "true" || v == "1")
            .unwrap_or(true);

        let max_connections = std::env::var("RUSTASSISTANT_DB_MAX_CONN")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(10);

        let is_dev = std::env::var("RUSTASSISTANT_ENV")
            .map(|v| v == "development" || v == "dev")
            .unwrap_or_else(|_| cfg!(debug_assertions));

        Self {
            url,
            auto_migrate,
            max_connections,
            is_dev,
        }
    }
}

// ============================================================================
// URL Resolution
// ============================================================================

/// Get the default Postgres URL.
/// Precedence:
///   1. DATABASE_URL env var (handled in from_env)
///   2. Compose default (postgres service on localhost)
///   3. Local dev fallback
fn get_default_db_url() -> String {
    // Try compose-style env vars as a fallback construction
    if let (Ok(user), Ok(password), Ok(host), Ok(db)) = (
        std::env::var("POSTGRES_USER"),
        std::env::var("POSTGRES_PASSWORD"),
        std::env::var("POSTGRES_HOST"),
        std::env::var("JANUS_DB"),
    ) {
        let port = std::env::var("POSTGRES_PORT").unwrap_or_else(|_| "5432".to_string());
        return format!(
            "postgresql://{}:{}@{}:{}/{}",
            user, password, host, port, db
        );
    }

    // Development fallback — matches docker-compose defaults
    "postgresql://rustcode:changeme@localhost:5432/rustcode".to_string()
}

// ============================================================================
// Pool Creation
// ============================================================================

/// Initialize the PostgreSQL connection pool.
///
/// Connects using DATABASE_URL, runs sqlx migrations if `auto_migrate` is set,
/// and returns a ready-to-use `PgPool`.
pub async fn init_pool(config: &DatabaseConfig) -> Result<PgPool> {
    info!(url = %redact_url(&config.url), "Connecting to PostgreSQL");

    let pool = PgPoolOptions::new()
        .max_connections(config.max_connections)
        .acquire_timeout(std::time::Duration::from_secs(30))
        .connect(&config.url)
        .await
        .with_context(|| {
            format!(
                "Failed to connect to PostgreSQL at {}",
                redact_url(&config.url)
            )
        })?;

    if config.auto_migrate {
        run_migrations(&pool).await?;
    }

    info!(
        "PostgreSQL pool ready ({} max connections)",
        config.max_connections
    );
    Ok(pool)
}

/// Run all pending sqlx migrations from the `./migrations` directory.
async fn run_migrations(pool: &PgPool) -> Result<()> {
    info!("Running database migrations...");
    sqlx::migrate!("./migrations")
        .run(pool)
        .await
        .context("Failed to run database migrations")?;
    info!("Migrations complete");
    Ok(())
}

// ============================================================================
// Utilities
// ============================================================================

/// Redact the password from a database URL for safe logging.
///
/// `postgresql://user:secret@host:5432/db` → `postgresql://user:***@host:5432/db`
fn redact_url(url: &str) -> String {
    // Quick heuristic: find `:` after `//user` and replace until `@`
    if let Some(at_pos) = url.find('@') {
        if let Some(colon_pos) = url[..at_pos].rfind(':') {
            let scheme_end = url.find("://").map(|i| i + 3).unwrap_or(0);
            if colon_pos > scheme_end {
                return format!("{}:***@{}", &url[..colon_pos], &url[at_pos + 1..]);
            }
        }
    }
    url.to_string()
}

// ============================================================================
// Health Check
// ============================================================================

/// Check database connectivity and return basic statistics.
pub async fn health_check(pool: &PgPool) -> Result<DatabaseHealth> {
    let start = std::time::Instant::now();

    // Verify connection is alive
    let result: (i32,) = sqlx::query_as("SELECT 1")
        .fetch_one(pool)
        .await
        .context("Database health check failed")?;

    let latency = start.elapsed();

    // Task count (best-effort — table may not exist yet during first boot)
    let task_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM tasks")
        .fetch_one(pool)
        .await
        .unwrap_or(0);

    Ok(DatabaseHealth {
        connected: result.0 == 1,
        latency_ms: latency.as_millis() as u64,
        task_count,
    })
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct DatabaseHealth {
    pub connected: bool,
    pub latency_ms: u64,
    pub task_count: i64,
}

// ============================================================================
// Backup Utilities
// ============================================================================

/// Create a logical backup of the database using pg_dump (shell out).
///
/// `backup_path` should end in `.sql` or `.dump`.
/// Requires `pg_dump` to be available on `PATH`.
pub async fn backup_database(_pool: &PgPool, backup_path: &std::path::Path) -> Result<()> {
    use std::process::Command;

    if let Some(parent) = backup_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let db_url = std::env::var("DATABASE_URL").unwrap_or_else(|_| get_default_db_url());

    let status = Command::new("pg_dump")
        .arg("--format=custom")
        .arg("--file")
        .arg(backup_path)
        .arg(&db_url)
        .status()
        .context("Failed to launch pg_dump — is it installed?")?;

    if !status.success() {
        anyhow::bail!("pg_dump exited with status: {}", status);
    }

    info!("Database backed up to: {}", backup_path.display());
    Ok(())
}

/// Get a timestamped backup file path.
pub fn get_backup_path() -> std::path::PathBuf {
    let timestamp = chrono::Utc::now().format("%Y%m%d_%H%M%S");
    std::path::PathBuf::from(format!("./data/backups/rustcode_{}.dump", timestamp))
}

// ============================================================================
// Environment Variables Documentation
// ============================================================================

/// Print help for database-related environment variables to stdout.
pub fn print_env_help() {
    println!(
        r#"
Database Environment Variables (PostgreSQL):
============================================

DATABASE_URL
    Full PostgreSQL connection URL.
    Default: postgresql://rustcode:changeme@localhost:5432/rustcode
    Example: postgresql://user:password@host:5432/dbname

POSTGRES_USER / POSTGRES_PASSWORD / POSTGRES_HOST / POSTGRES_PORT / JANUS_DB
    Alternative: individual components used to build the URL when DATABASE_URL
    is not set. Useful with docker-compose environment blocks.

RUSTASSISTANT_AUTO_MIGRATE
    Run migrations on startup. Values: true, false, 1, 0
    Default: true

RUSTASSISTANT_DB_MAX_CONN
    Maximum connections in the pool.
    Default: 10

RUSTASSISTANT_ENV
    Environment mode. Values: development, dev, production, prod
    Default: development (debug builds), production (release builds)

Example — docker-compose:
--------------------------
environment:
  - DATABASE_URL=postgresql://rustcode:${{POSTGRES_PASSWORD}}@postgres:5432/rustcode

Example — local dev (.env):
----------------------------
DATABASE_URL=postgresql://rustcode:changeme@localhost:5432/rustcode
"#
    );
}

// ============================================================================
// Legacy shim — previously exported from this module
// ============================================================================

/// Ensure the data directory exists (no-op for Postgres; kept for API compat).
pub fn ensure_data_dir(_config: &DatabaseConfig) -> Result<()> {
    Ok(())
}

/// Get the data directory.  Returns `./data` as a conventional location for
/// non-DB artefacts (logs, backups, cache).
pub fn get_data_dir(_config: &DatabaseConfig) -> std::path::PathBuf {
    std::path::PathBuf::from("./data")
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = DatabaseConfig::default();
        assert!(config.auto_migrate);
        assert_eq!(config.max_connections, 10);
        assert!(config.url.starts_with("postgresql://"));
    }

    #[test]
    fn test_config_from_env() {
        // SAFETY: This test is single-threaded; no other thread reads these vars.
        unsafe {
            std::env::set_var(
                "DATABASE_URL",
                "postgresql://test:pass@localhost:5432/testdb",
            );
            std::env::set_var("RUSTASSISTANT_AUTO_MIGRATE", "false");
        }

        let config = DatabaseConfig::from_env();
        assert_eq!(config.url, "postgresql://test:pass@localhost:5432/testdb");
        assert!(!config.auto_migrate);

        // SAFETY: This test is single-threaded; no other thread reads these vars.
        unsafe {
            std::env::remove_var("DATABASE_URL");
            std::env::remove_var("RUSTASSISTANT_AUTO_MIGRATE");
        }
    }

    #[test]
    fn test_redact_url() {
        assert_eq!(
            redact_url("postgresql://rustcode:supersecret@localhost:5432/rustcode"),
            "postgresql://rustcode:***@localhost:5432/rustcode"
        );
        // No password in URL — returned as-is
        assert_eq!(
            redact_url("postgresql://localhost/rustcode"),
            "postgresql://localhost/rustcode"
        );
    }
}
