//! Backup System
//!
//! Handles database and cache backup to Google Drive using rclone.
//! No API keys needed - uses rclone's OAuth flow.

use anyhow::{Context, Result};
use chrono::Utc;
use std::path::{Path, PathBuf};
use std::process::Command;
use tracing::{info, warn};

// ============================================================================
// Backup Configuration
// ============================================================================

#[derive(Debug, Clone)]
pub struct BackupConfig {
    /// Local data directory to backup
    pub data_dir: PathBuf,

    /// rclone remote name (e.g., "gdrive")
    pub remote_name: String,

    /// Remote path for backups
    pub remote_path: String,

    /// Number of backups to keep
    pub retention_count: usize,

    /// Backup schedule (cron format)
    pub schedule: Option<String>,
}

impl Default for BackupConfig {
    fn default() -> Self {
        Self {
            data_dir: PathBuf::from("/var/lib/rustcode"),
            remote_name: "gdrive".to_string(),
            remote_path: "rustcode-backups".to_string(),
            retention_count: 30,
            schedule: Some("0 2 * * *".to_string()), // Daily at 2 AM
        }
    }
}

impl BackupConfig {
    pub fn from_env() -> Self {
        let mut config = Self::default();

        if let Ok(dir) = std::env::var("RUSTASSISTANT_DATA_DIR") {
            config.data_dir = PathBuf::from(dir);
        }
        if let Ok(remote) = std::env::var("BACKUP_REMOTE_NAME") {
            config.remote_name = remote;
        }
        if let Ok(path) = std::env::var("BACKUP_REMOTE_PATH") {
            config.remote_path = path;
        }
        if let Ok(count) = std::env::var("BACKUP_RETENTION_COUNT") {
            config.retention_count = count.parse().unwrap_or(30);
        }

        config
    }
}

// ============================================================================
// Backup Manager
// ============================================================================

pub struct BackupManager {
    config: BackupConfig,
}

impl BackupManager {
    pub fn new(config: BackupConfig) -> Self {
        Self { config }
    }

    /// Check if rclone is installed and configured
    pub fn check_rclone(&self) -> Result<bool> {
        let output = Command::new("rclone").args(["version"]).output().context(
            "rclone not found. Install with: curl https://rclone.org/install.sh | sudo bash",
        )?;

        if !output.status.success() {
            return Ok(false);
        }

        // Check if remote is configured
        let list_output = Command::new("rclone").args(["listremotes"]).output()?;

        let remotes = String::from_utf8_lossy(&list_output.stdout);
        let remote_exists = remotes.contains(&format!("{}:", self.config.remote_name));

        if !remote_exists {
            warn!(
                "Remote '{}' not configured. Run: rclone config",
                self.config.remote_name
            );
        }

        Ok(remote_exists)
    }

    /// Create a backup of the data directory
    pub fn create_backup(&self) -> Result<BackupResult> {
        info!("Starting backup to {}", self.config.remote_name);

        let timestamp = Utc::now().format("%Y%m%d_%H%M%S").to_string();
        let backup_name = format!("backup_{}", timestamp);
        let remote_dest = format!(
            "{}:{}/{}",
            self.config.remote_name, self.config.remote_path, backup_name
        );

        // Create a local snapshot first (SQLite safe backup)
        let snapshot_dir = self.create_snapshot(&timestamp)?;

        // Sync to remote
        let output = Command::new("rclone")
            .args([
                "copy",
                snapshot_dir.to_str().unwrap(),
                &remote_dest,
                "--progress",
                "-v",
            ])
            .output()
            .context("Failed to run rclone copy")?;

        // Cleanup local snapshot
        std::fs::remove_dir_all(&snapshot_dir).ok();

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(anyhow::anyhow!("Backup failed: {}", stderr));
        }

        // Get backup size
        let size = self.get_remote_size(&remote_dest)?;

        info!("Backup complete: {} ({} bytes)", backup_name, size);

        // Cleanup old backups
        self.cleanup_old_backups()?;

        Ok(BackupResult {
            name: backup_name,
            timestamp,
            size_bytes: size,
            remote_path: remote_dest,
        })
    }

    /// Create a local snapshot of databases
    fn create_snapshot(&self, timestamp: &str) -> Result<PathBuf> {
        let snapshot_dir = std::env::temp_dir()
            .join("rustcode-backup")
            .join(timestamp);

        std::fs::create_dir_all(&snapshot_dir)?;

        // Backup SQLite database using VACUUM INTO (atomic copy)
        let db_path = self.config.data_dir.join("rustcode.db");
        if db_path.exists() {
            let snapshot_db = snapshot_dir.join("rustcode.db");

            // Use sqlite3 CLI for safe backup
            let status = Command::new("sqlite3")
                .arg(&db_path)
                .arg(format!(".backup '{}'", snapshot_db.display()))
                .status();

            match status {
                Ok(s) if s.success() => {
                    info!("Database snapshot created");
                }
                _ => {
                    // Fallback: direct copy (less safe but works)
                    warn!("sqlite3 backup failed, using direct copy");
                    std::fs::copy(&db_path, &snapshot_db)?;
                }
            }
        }

        // Copy cache and other files
        let cache_path = self.config.data_dir.join("cache");
        if cache_path.exists() {
            let snapshot_cache = snapshot_dir.join("cache");
            copy_dir_recursive(&cache_path, &snapshot_cache)?;
        }

        // Copy config files
        for file in ["config.toml", ".env"] {
            let src = self.config.data_dir.join(file);
            if src.exists() {
                std::fs::copy(&src, snapshot_dir.join(file))?;
            }
        }

        Ok(snapshot_dir)
    }

    /// Get size of remote backup
    fn get_remote_size(&self, remote_path: &str) -> Result<u64> {
        let output = Command::new("rclone")
            .args(["size", remote_path, "--json"])
            .output()?;

        if output.status.success() {
            let json: serde_json::Value = serde_json::from_slice(&output.stdout)?;
            Ok(json["bytes"].as_u64().unwrap_or(0))
        } else {
            Ok(0)
        }
    }

    /// Remove old backups beyond retention count
    fn cleanup_old_backups(&self) -> Result<()> {
        let remote_base = format!("{}:{}", self.config.remote_name, self.config.remote_path);

        // List existing backups
        let output = Command::new("rclone")
            .args(["lsf", &remote_base, "--dirs-only"])
            .output()?;

        if !output.status.success() {
            return Ok(()); // No backups yet
        }

        let mut backups: Vec<String> = String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter(|l| l.starts_with("backup_"))
            .map(|l| l.trim_end_matches('/').to_string())
            .collect();

        // Sort by name (which includes timestamp)
        backups.sort();
        backups.reverse();

        // Remove old ones
        if backups.len() > self.config.retention_count {
            let to_remove = &backups[self.config.retention_count..];

            for backup in to_remove {
                let path = format!("{}/{}", remote_base, backup);
                info!("Removing old backup: {}", backup);

                Command::new("rclone").args(["purge", &path]).output().ok();
            }
        }

        Ok(())
    }

    /// List available backups
    pub fn list_backups(&self) -> Result<Vec<BackupInfo>> {
        let remote_base = format!("{}:{}", self.config.remote_name, self.config.remote_path);

        let output = Command::new("rclone")
            .args(["lsjson", &remote_base, "--dirs-only"])
            .output()?;

        if !output.status.success() {
            return Ok(vec![]);
        }

        #[derive(serde::Deserialize)]
        struct RcloneEntry {
            #[serde(rename = "Name")]
            name: String,
            #[serde(rename = "ModTime")]
            mod_time: String,
        }

        let entries: Vec<RcloneEntry> = serde_json::from_slice(&output.stdout)?;

        let backups: Vec<BackupInfo> = entries
            .into_iter()
            .filter(|e| e.name.starts_with("backup_"))
            .map(|e| BackupInfo {
                name: e.name,
                created_at: e.mod_time,
            })
            .collect();

        Ok(backups)
    }

    /// Restore from a specific backup
    pub fn restore(&self, backup_name: &str) -> Result<()> {
        info!("Restoring from backup: {}", backup_name);

        let remote_src = format!(
            "{}:{}/{}",
            self.config.remote_name, self.config.remote_path, backup_name
        );

        // Create restore directory
        let restore_dir = self.config.data_dir.join("restore");
        std::fs::create_dir_all(&restore_dir)?;

        // Download backup
        let output = Command::new("rclone")
            .args([
                "copy",
                &remote_src,
                restore_dir.to_str().unwrap(),
                "--progress",
                "-v",
            ])
            .output()
            .context("Failed to download backup")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(anyhow::anyhow!("Restore download failed: {}", stderr));
        }

        // Stop any running services (user should do this)
        warn!("Please stop rustcode service before continuing");

        // Move restored files into place
        let db_restore = restore_dir.join("rustcode.db");
        if db_restore.exists() {
            let db_dest = self.config.data_dir.join("rustcode.db");

            // Backup current db first
            if db_dest.exists() {
                let backup = self.config.data_dir.join("rustcode.db.pre-restore");
                std::fs::rename(&db_dest, backup)?;
            }

            std::fs::rename(db_restore, db_dest)?;
        }

        // Restore cache
        let cache_restore = restore_dir.join("cache");
        if cache_restore.exists() {
            let cache_dest = self.config.data_dir.join("cache");
            if cache_dest.exists() {
                std::fs::remove_dir_all(&cache_dest)?;
            }
            std::fs::rename(cache_restore, cache_dest)?;
        }

        // Cleanup restore directory
        std::fs::remove_dir_all(&restore_dir)?;

        info!("Restore complete! Please restart rustcode service.");

        Ok(())
    }
}

// ============================================================================
// Helper Types
// ============================================================================

#[derive(Debug, Clone)]
pub struct BackupResult {
    pub name: String,
    pub timestamp: String,
    pub size_bytes: u64,
    pub remote_path: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct BackupInfo {
    pub name: String,
    pub created_at: String,
}

// ============================================================================
// Utility Functions
// ============================================================================

fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<()> {
    std::fs::create_dir_all(dst)?;

    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let path = entry.path();
        let dest_path = dst.join(entry.file_name());

        if path.is_dir() {
            copy_dir_recursive(&path, &dest_path)?;
        } else {
            std::fs::copy(&path, &dest_path)?;
        }
    }

    Ok(())
}

// ============================================================================
// CLI Commands
// ============================================================================

pub fn print_rclone_setup_instructions() {
    println!(
        r#"
Google Drive Backup Setup (No API Key Required!)
================================================

1. Install rclone:
   curl https://rclone.org/install.sh | sudo bash

2. Configure Google Drive remote:
   rclone config

   Choose:
   - n (new remote)
   - Name: gdrive
   - Storage: drive (Google Drive)
   - client_id: (leave blank)
   - client_secret: (leave blank)
   - scope: 1 (full access)
   - root_folder_id: (leave blank)
   - service_account_file: (leave blank)
   - Use auto config: y (if on Pi with browser) or n (if headless)

3. For headless Pi setup:
   - Run 'rclone config' on a machine with a browser
   - Copy the token to your Pi when prompted
   - Or use 'rclone authorize "drive"' on browser machine

4. Test connection:
   rclone lsd gdrive:

5. Set environment variables (optional):
   export BACKUP_REMOTE_NAME="gdrive"
   export BACKUP_REMOTE_PATH="rustcode-backups"
   export BACKUP_RETENTION_COUNT="30"

6. Create your first backup:
   rustcode backup create

7. Set up automatic backups (cron):
   crontab -e
   # Add: 0 2 * * * /usr/local/bin/rustcode backup create >> /var/log/rustcode-backup.log 2>&1
"#
    );
}
