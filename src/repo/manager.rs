// Repository Manager
//
// Manages git repository cloning, updating, and synchronization at runtime.
// Eliminates the need for bind-mounted host directories by cloning repos into
// container-managed storage.
//
// RC-CRATES-C (2026-05-21): six subprocess `git` invocations migrated
// from raw `std::process::Command` to `runtime::execute_bash` via local
// `build_git_command` / `run_git_command` helpers (same shape as
// `src/git.rs` and `src/task_executor.rs`).

use anyhow::{Context, Result, anyhow};
use runtime::{BashCommandInput, BashCommandOutput, execute_bash, shell_quote};
use std::path::{Path, PathBuf};
use tracing::{debug, error, info, warn};

// Build a `GIT_TERMINAL_PROMPT=0 git <arg> ...` shell command with all
// args shell-quoted. The env var prevents git from interactively
// prompting for credentials; the previous `Command::env(...)` calls
// did the same.
fn build_git_command(args: &[&str]) -> String {
    let args_part = std::iter::once("git".to_string())
        .chain(args.iter().map(|a| shell_quote(a)))
        .collect::<Vec<_>>()
        .join(" ");
    format!("GIT_TERMINAL_PROMPT=0 {args_part}")
}

// Run a constructed shell command via `runtime::execute_bash` with
// sandboxing disabled (matches the previous `Command::new("git")` path
// which had none).
fn run_git_command(command: String, cwd: Option<&Path>) -> std::io::Result<BashCommandOutput> {
    execute_bash(BashCommandInput {
        command,
        timeout: None,
        description: None,
        run_in_background: Some(false),
        dangerously_disable_sandbox: Some(true),
        namespace_restrictions: None,
        isolate_network: None,
        filesystem_mode: None,
        allowed_mounts: None,
        cwd: cwd.map(Path::to_path_buf),
    })
}

// Repository manager for git operations
pub struct RepoManager {
    // Base directory where repos are cloned
    repos_dir: PathBuf,
    // GitHub token for authentication (optional)
    github_token: Option<String>,
    // Default branch name
    default_branch: String,
}

impl RepoManager {
    // Create a new repository manager
    //
    // # Arguments
    // * `repos_dir` - Base directory for cloning repositories
    // * `github_token` - Optional GitHub token for private repos
    pub fn new<P: AsRef<Path>>(repos_dir: P, github_token: Option<String>) -> Result<Self> {
        let repos_dir = repos_dir.as_ref().to_path_buf();

        // Create repos directory if it doesn't exist
        if !repos_dir.exists() {
            std::fs::create_dir_all(&repos_dir)
                .context(format!("Failed to create repos directory: {:?}", repos_dir))?;
            info!("Created repos directory: {:?}", repos_dir);
        }

        Ok(Self {
            repos_dir,
            github_token,
            default_branch: "main".to_string(),
        })
    }

    // Clone a repository or update if it already exists
    //
    // # Arguments
    // * `git_url` - Git clone URL (HTTPS)
    // * `repo_name` - Local directory name for the repo
    //
    // # Returns
    // Path to the cloned/updated repository
    pub fn clone_or_update(&self, git_url: &str, repo_name: &str) -> Result<PathBuf> {
        let repo_path = self.repos_dir.join(repo_name);

        if repo_path.exists() {
            self.update_repo(&repo_path, git_url)
        } else {
            self.clone_repo(git_url, repo_name)
        }
    }

    // Clone a fresh repository
    fn clone_repo(&self, git_url: &str, repo_name: &str) -> Result<PathBuf> {
        let repo_path = self.repos_dir.join(repo_name);

        info!("Cloning repository {} to {:?}", git_url, repo_path);

        // Build authenticated URL if token is available
        let clone_url = self.build_authenticated_url(git_url)?;

        let repo_path_str = repo_path.display().to_string();
        let command = build_git_command(&["clone", "--depth=1", &clone_url, &repo_path_str]);
        let output = run_git_command(command, None)
            .context("Failed to execute git clone command")?;

        if output.return_code_interpretation.is_some() {
            error!("Git clone failed: {}", output.stderr);
            return Err(anyhow!("Git clone failed: {}", output.stderr));
        }

        info!("Successfully cloned {} to {:?}", repo_name, repo_path);
        Ok(repo_path)
    }

    // Update an existing repository
    fn update_repo(&self, repo_path: &Path, _git_url: &str) -> Result<PathBuf> {
        debug!("Updating repository at {:?}", repo_path);

        // Verify it's actually a git repo
        if !repo_path.join(".git").exists() {
            warn!("Directory exists but is not a git repo: {:?}", repo_path);
            return Err(anyhow!(
                "Directory exists but is not a git repository: {:?}",
                repo_path
            ));
        }

        // Get current commit hash before update
        let old_hash = self.get_current_commit(repo_path).ok();

        // Fetch and pull latest changes. `cwd: Some(repo_path)` replaces
        // the previous `git -C <path>` invocation.
        let command =
            build_git_command(&["pull", "--rebase", "origin", &self.default_branch]);
        let output =
            run_git_command(command, Some(repo_path)).context("Failed to execute git pull")?;

        if output.return_code_interpretation.is_some() {
            // If branch doesn't exist, try to fetch it
            if output.stderr.contains("couldn't find remote ref") {
                warn!("Branch {} not found, trying 'master'", self.default_branch);
                return self.update_repo_with_branch(repo_path, "master");
            }

            error!("Git pull failed: {}", output.stderr);
            return Err(anyhow!("Git pull failed: {}", output.stderr));
        }

        // Get new commit hash
        let new_hash = self.get_current_commit(repo_path)?;

        if old_hash.as_ref() != Some(&new_hash) {
            info!(
                "Updated repository at {:?}: {} -> {}",
                repo_path,
                old_hash.as_deref().unwrap_or("unknown"),
                new_hash
            );
        } else {
            debug!("Repository at {:?} is already up to date", repo_path);
        }

        Ok(repo_path.to_path_buf())
    }

    // Update repository with a specific branch
    fn update_repo_with_branch(&self, repo_path: &Path, branch: &str) -> Result<PathBuf> {
        let command = build_git_command(&["pull", "--rebase", "origin", branch]);
        let output =
            run_git_command(command, Some(repo_path)).context("Failed to execute git pull")?;

        if output.return_code_interpretation.is_some() {
            error!("Git pull (branch {}) failed: {}", branch, output.stderr);
            return Err(anyhow!("Git pull failed: {}", output.stderr));
        }

        Ok(repo_path.to_path_buf())
    }

    // Get current commit hash of a repository
    pub fn get_current_commit(&self, repo_path: &Path) -> Result<String> {
        let command = build_git_command(&["rev-parse", "HEAD"]);
        let output =
            run_git_command(command, Some(repo_path)).context("Failed to get current commit")?;

        if output.return_code_interpretation.is_some() {
            return Err(anyhow!("Failed to get current commit hash"));
        }

        Ok(output.stdout.trim().to_string())
    }

    // Check if repository has uncommitted changes
    pub fn has_uncommitted_changes(&self, repo_path: &Path) -> Result<bool> {
        let command = build_git_command(&["status", "--porcelain"]);
        let output =
            run_git_command(command, Some(repo_path)).context("Failed to check git status")?;

        if output.return_code_interpretation.is_some() {
            return Err(anyhow!("Failed to check git status"));
        }

        Ok(!output.stdout.is_empty())
    }

    // Get repository information
    pub fn get_repo_info(&self, repo_path: &Path) -> Result<RepoInfo> {
        let commit_hash = self.get_current_commit(repo_path)?;
        let has_changes = self.has_uncommitted_changes(repo_path)?;
        let branch = self.get_current_branch(repo_path)?;

        Ok(RepoInfo {
            path: repo_path.to_path_buf(),
            commit_hash,
            branch,
            has_uncommitted_changes: has_changes,
        })
    }

    // Get current branch name
    fn get_current_branch(&self, repo_path: &Path) -> Result<String> {
        let command = build_git_command(&["rev-parse", "--abbrev-ref", "HEAD"]);
        let output =
            run_git_command(command, Some(repo_path)).context("Failed to get current branch")?;

        if output.return_code_interpretation.is_some() {
            return Err(anyhow!("Failed to get current branch"));
        }

        Ok(output.stdout.trim().to_string())
    }

    // Build authenticated URL with GitHub token if available
    fn build_authenticated_url(&self, git_url: &str) -> Result<String> {
        if let Some(token) = &self.github_token {
            // Parse the URL and inject the token
            if git_url.starts_with("https://github.com/") {
                let url_without_protocol = git_url.trim_start_matches("https://");
                return Ok(format!("https://{}@{}", token, url_without_protocol));
            } else if git_url.starts_with("https://") {
                // Generic HTTPS URL - inject token
                let url_without_protocol = git_url.trim_start_matches("https://");
                return Ok(format!("https://{}@{}", token, url_without_protocol));
            }
        }

        // No token or non-HTTPS URL, return as-is
        Ok(git_url.to_string())
    }

    // Remove a cloned repository
    pub fn remove_repo(&self, repo_name: &str) -> Result<()> {
        let repo_path = self.repos_dir.join(repo_name);

        if !repo_path.exists() {
            return Ok(()); // Already removed
        }

        info!("Removing repository at {:?}", repo_path);
        std::fs::remove_dir_all(&repo_path)
            .context(format!("Failed to remove repository at {:?}", repo_path))?;

        Ok(())
    }

    // List all cloned repositories
    pub fn list_repos(&self) -> Result<Vec<String>> {
        let mut repos = Vec::new();

        if !self.repos_dir.exists() {
            return Ok(repos);
        }

        for entry in std::fs::read_dir(&self.repos_dir).context("Failed to read repos directory")? {
            let entry = entry.context("Failed to read directory entry")?;
            let path = entry.path();

            if path.is_dir() && path.join(".git").exists() {
                if let Some(name) = path.file_name() {
                    repos.push(name.to_string_lossy().to_string());
                }
            }
        }

        Ok(repos)
    }

    // Get the path where a repository would be cloned
    pub fn get_repo_path(&self, repo_name: &str) -> PathBuf {
        self.repos_dir.join(repo_name)
    }
}

// Repository information
#[derive(Debug, Clone)]
pub struct RepoInfo {
    pub path: PathBuf,
    pub commit_hash: String,
    pub branch: String,
    pub has_uncommitted_changes: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_repo_manager_creation() {
        let temp_dir = TempDir::new().unwrap();
        let _manager = RepoManager::new(temp_dir.path(), None).unwrap();
        assert!(temp_dir.path().exists());
    }

    #[test]
    fn test_build_authenticated_url() {
        let temp_dir = TempDir::new().unwrap();
        let token = "ghp_test123".to_string();
        let manager = RepoManager::new(temp_dir.path(), Some(token)).unwrap();

        let url = "https://github.com/user/repo.git";
        let auth_url = manager.build_authenticated_url(url).unwrap();
        assert_eq!(auth_url, "https://ghp_test123@github.com/user/repo.git");
    }

    #[test]
    fn test_build_authenticated_url_no_token() {
        let temp_dir = TempDir::new().unwrap();
        let manager = RepoManager::new(temp_dir.path(), None).unwrap();

        let url = "https://github.com/user/repo.git";
        let auth_url = manager.build_authenticated_url(url).unwrap();
        assert_eq!(auth_url, url);
    }

    #[test]
    fn test_get_repo_path() {
        let temp_dir = TempDir::new().unwrap();
        let manager = RepoManager::new(temp_dir.path(), None).unwrap();

        let path = manager.get_repo_path("test-repo");
        assert_eq!(path, temp_dir.path().join("test-repo"));
    }
}
