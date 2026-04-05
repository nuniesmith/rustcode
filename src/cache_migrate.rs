// # Cache Migration Utility
//
// Provides utilities for migrating cache data between different storage backends.
//
// ## Features
//
// - Migrate from JSON file-based cache to SQLite
// - Validate migration completeness
// - Progress tracking and reporting
// - Safe migration with rollback support
//
// ## Usage
//
// ```rust,no_run
// use rustcode::cache_migrate::CacheMigrator;
//
// #[tokio::main]
// async fn main() -> anyhow::Result<()> {
//     let migrator = CacheMigrator::new(
//         "~/.rustcode/cache/repos",  // JSON source
//         "~/.rustcode/cache.db"       // SQLite destination
//     ).await?;
//
//     // Run migration with progress callback
//     let result = migrator.migrate(|progress| {
//         println!("Progress: {}/{}", progress.migrated, progress.total);
//     }).await?;
//
//     println!("Migrated {} entries", result.total_migrated);
//     Ok(())
// }
// ```

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tracing::{debug, info, warn};

use crate::repo_cache::{CacheType, RepoCacheEntry};
use crate::repo_cache_sql::RepoCacheSql;

// Migration progress information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MigrationProgress {
    // Total entries to migrate
    pub total: usize,
    // Entries migrated so far
    pub migrated: usize,
    // Entries failed
    pub failed: usize,
    // Current file being migrated
    pub current_file: String,
}

// Migration result summary
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MigrationResult {
    // Total entries found in source
    pub total_entries: usize,
    // Successfully migrated entries
    pub total_migrated: usize,
    // Failed migrations
    pub total_failed: usize,
    // Size of source cache (bytes)
    pub source_size: u64,
    // Size of destination cache (bytes)
    pub destination_size: u64,
    // Space savings (bytes)
    pub space_saved: u64,
    // Failed entries with reasons
    pub failures: Vec<MigrationFailure>,
}

// Information about a failed migration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MigrationFailure {
    // File path that failed
    pub file_path: String,
    // Cache type
    pub cache_type: String,
    // Error message
    pub error: String,
}

// Cache migration orchestrator
pub struct CacheMigrator {
    source_path: PathBuf,
    destination_path: PathBuf,
    sql_cache: RepoCacheSql,
}

impl CacheMigrator {
    // Create a new cache migrator
    pub async fn new(
        source_path: impl AsRef<Path>,
        destination_path: impl AsRef<Path>,
    ) -> Result<Self> {
        let source_path = source_path.as_ref().to_path_buf();
        let destination_path = destination_path.as_ref().to_path_buf();

        // Create SQLite cache
        let sql_cache = RepoCacheSql::new(&destination_path).await?;

        Ok(Self {
            source_path,
            destination_path,
            sql_cache,
        })
    }

    // Run the migration
    pub async fn migrate<F>(&self, mut progress_callback: F) -> Result<MigrationResult>
    where
        F: FnMut(MigrationProgress),
    {
        info!("Starting cache migration from JSON to SQLite");
        info!("Source: {}", self.source_path.display());
        info!("Destination: {}", self.destination_path.display());

        // Collect all JSON cache entries
        let entries = self.collect_json_entries()?;
        let total_entries = entries.len();

        info!("Found {} entries to migrate", total_entries);

        let mut migrated = 0;
        let mut failed = 0;
        let mut failures = Vec::new();

        // Migrate each entry
        for (idx, (repo_path, cache_type, entry)) in entries.into_iter().enumerate() {
            let progress = MigrationProgress {
                total: total_entries,
                migrated,
                failed,
                current_file: entry.file_path.clone(),
            };
            progress_callback(progress);

            match self.migrate_entry(&repo_path, cache_type, &entry).await {
                Ok(_) => {
                    migrated += 1;
                    debug!(
                        "Migrated {}/{}: {}",
                        idx + 1,
                        total_entries,
                        entry.file_path
                    );
                }
                Err(e) => {
                    failed += 1;
                    warn!("Failed to migrate {}: {}", entry.file_path, e);
                    failures.push(MigrationFailure {
                        file_path: entry.file_path.clone(),
                        cache_type: cache_type.subdirectory().to_string(),
                        error: e.to_string(),
                    });
                }
            }
        }

        // Calculate sizes
        let source_size = self.calculate_source_size()?;
        let destination_size = self.calculate_destination_size().await?;
        let space_saved = source_size.saturating_sub(destination_size);

        let result = MigrationResult {
            total_entries,
            total_migrated: migrated,
            total_failed: failed,
            source_size,
            destination_size,
            space_saved,
            failures,
        };

        info!("Migration complete!");
        info!("  Migrated: {}", result.total_migrated);
        info!("  Failed: {}", result.total_failed);
        info!(
            "  Space saved: {} bytes ({:.1}%)",
            result.space_saved,
            (result.space_saved as f64 / source_size as f64) * 100.0
        );

        Ok(result)
    }

    // Collect all JSON cache entries from the source
    fn collect_json_entries(&self) -> Result<Vec<(String, CacheType, RepoCacheEntry)>> {
        let mut entries = Vec::new();

        // Check if source path exists
        if !self.source_path.exists() {
            warn!("Source path does not exist: {}", self.source_path.display());
            return Ok(entries);
        }

        // Iterate through repo directories
        for repo_entry in std::fs::read_dir(&self.source_path)? {
            let repo_entry = repo_entry?;
            let repo_path_dir = repo_entry.path();

            if !repo_path_dir.is_dir() {
                continue;
            }

            // Read meta.json to get repo path
            let meta_path = repo_path_dir.join("meta.json");
            let repo_path = if meta_path.exists() {
                let meta_content = std::fs::read_to_string(&meta_path)?;
                let meta: serde_json::Value = serde_json::from_str(&meta_content)?;
                meta["path"].as_str().unwrap_or_default().to_string()
            } else {
                repo_path_dir.to_string_lossy().to_string()
            };

            // Iterate through cache type directories
            let cache_dir = repo_path_dir.join("cache");
            if !cache_dir.exists() {
                continue;
            }

            for cache_type in &[
                CacheType::Analysis,
                CacheType::Docs,
                CacheType::Refactor,
                CacheType::Todos,
            ] {
                let type_dir = cache_dir.join(cache_type.subdirectory());
                if !type_dir.exists() {
                    continue;
                }

                // Collect all .json files
                self.collect_entries_from_dir(&type_dir, &repo_path, *cache_type, &mut entries)?;
            }
        }

        Ok(entries)
    }

    // Recursively collect entries from a directory
    fn collect_entries_from_dir(
        &self,
        dir: &Path,
        repo_path: &str,
        cache_type: CacheType,
        entries: &mut Vec<(String, CacheType, RepoCacheEntry)>,
    ) -> Result<()> {
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();

            if path.is_dir() {
                // Recurse into subdirectories
                self.collect_entries_from_dir(&path, repo_path, cache_type, entries)?;
            } else if path.extension().and_then(|s| s.to_str()) == Some("json") {
                // Read and parse JSON entry
                match std::fs::read_to_string(&path) {
                    Ok(content) => match serde_json::from_str::<RepoCacheEntry>(&content) {
                        Ok(cache_entry) => {
                            entries.push((repo_path.to_string(), cache_type, cache_entry));
                        }
                        Err(e) => {
                            warn!("Failed to parse {}: {}", path.display(), e);
                        }
                    },
                    Err(e) => {
                        warn!("Failed to read {}: {}", path.display(), e);
                    }
                }
            }
        }

        Ok(())
    }

    // Migrate a single entry to SQLite
    async fn migrate_entry(
        &self,
        repo_path: &str,
        cache_type: CacheType,
        entry: &RepoCacheEntry,
    ) -> Result<()> {
        // Compute cache_key if it's missing (for old cache entries)
        let cache_key = if entry.cache_key.is_empty() {
            // Compute the cache key from available fields
            use sha2::{Digest, Sha256};

            let prompt_hash = if entry.prompt_hash.is_empty() {
                crate::prompt_hashes::get_prompt_hash_for_type(cache_type)
            } else {
                entry.prompt_hash.clone()
            };

            let mut hasher = Sha256::new();
            hasher.update(entry.file_hash.as_bytes());
            hasher.update(entry.model.as_bytes());
            hasher.update(prompt_hash.as_bytes());
            hasher.update(entry.schema_version.to_string().as_bytes());
            let hash = hasher.finalize();
            format!("{:x}", hash)[..32].to_string()
        } else {
            entry.cache_key.clone()
        };

        let prompt_hash = if entry.prompt_hash.is_empty() {
            crate::prompt_hashes::get_prompt_hash_for_type(cache_type)
        } else {
            entry.prompt_hash.clone()
        };

        self.sql_cache
            .set_with_cache_key(
                cache_type,
                repo_path,
                &entry.file_path,
                &entry.file_hash,
                &cache_key,
                &entry.provider,
                &entry.model,
                &prompt_hash,
                entry.schema_version as i32,
                entry.result.clone(),
                entry.tokens_used,
                entry.file_size,
            )
            .await
            .context("Failed to insert into SQLite cache")?;

        Ok(())
    }

    // Calculate total size of source cache
    fn calculate_source_size(&self) -> Result<u64> {
        let mut total_size = 0u64;

        if !self.source_path.exists() {
            return Ok(0);
        }

        for entry in walkdir::WalkDir::new(&self.source_path) {
            let entry = entry?;
            if entry.file_type().is_file() {
                total_size += entry.metadata()?.len();
            }
        }

        Ok(total_size)
    }

    // Calculate size of SQLite database
    async fn calculate_destination_size(&self) -> Result<u64> {
        let metadata = std::fs::metadata(&self.destination_path)?;
        Ok(metadata.len())
    }

    // Verify migration by comparing counts
    pub async fn verify(&self) -> Result<bool> {
        let json_entries = self.collect_json_entries()?;
        let stats = self.sql_cache.stats().await?;

        info!("Verification:");
        info!("  JSON entries: {}", json_entries.len());
        info!("  SQLite entries: {}", stats.total_entries);

        Ok(json_entries.len() == stats.total_entries as usize)
    }

    // Create a backup of the source cache
    pub fn backup(&self, backup_path: impl AsRef<Path>) -> Result<()> {
        let backup_path = backup_path.as_ref();

        info!("Creating backup at {}", backup_path.display());

        if !self.source_path.exists() {
            warn!("Source path does not exist, nothing to backup");
            return Ok(());
        }

        // Copy entire source directory to backup location
        self.copy_dir_recursive(&self.source_path, backup_path)?;

        info!("Backup created successfully");
        Ok(())
    }

    // Recursively copy directory
    fn copy_dir_recursive(&self, src: &Path, dst: &Path) -> Result<()> {
        std::fs::create_dir_all(dst)?;

        for entry in std::fs::read_dir(src)? {
            let entry = entry?;
            let path = entry.path();
            let file_name = entry.file_name();
            let dst_path = dst.join(&file_name);

            if path.is_dir() {
                self.copy_dir_recursive(&path, &dst_path)?;
            } else {
                std::fs::copy(&path, &dst_path)?;
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    #[ignore = "CacheMigrator depends on RepoCacheSql (SQLite); not available in postgres-only build"]
    async fn test_migrator_creation() {
        let temp_dir = TempDir::new().unwrap();
        let source = temp_dir.path().join("source");
        let dest = temp_dir.path().join("cache.db");

        std::fs::create_dir_all(&source).unwrap();

        let migrator = CacheMigrator::new(&source, &dest).await;
        assert!(migrator.is_ok());
    }

    #[tokio::test]
    #[ignore = "CacheMigrator depends on RepoCacheSql (SQLite); not available in postgres-only build"]
    async fn test_empty_migration() {
        let temp_dir = TempDir::new().unwrap();
        let source = temp_dir.path().join("source");
        let dest = temp_dir.path().join("cache.db");

        std::fs::create_dir_all(&source).unwrap();

        let migrator = CacheMigrator::new(&source, &dest).await.unwrap();
        let result = migrator.migrate(|_| {}).await.unwrap();

        assert_eq!(result.total_entries, 0);
        assert_eq!(result.total_migrated, 0);
        assert_eq!(result.total_failed, 0);
    }

    #[tokio::test]
    #[ignore = "CacheMigrator depends on RepoCacheSql (SQLite); not available in postgres-only build"]
    async fn test_backup_creation() {
        let temp_dir = TempDir::new().unwrap();
        let source = temp_dir.path().join("source");
        let dest = temp_dir.path().join("cache.db");
        let backup = temp_dir.path().join("backup");

        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(source.join("test.txt"), "test content").unwrap();

        let migrator = CacheMigrator::new(&source, &dest).await.unwrap();
        let result = migrator.backup(&backup);

        assert!(result.is_ok());
        assert!(backup.join("test.txt").exists());
    }
}
