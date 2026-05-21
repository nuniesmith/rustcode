// Git repository management for audit service
//
// RC-CRATES-C (2026-05-21): the five subprocess `git` invocations
// (`clone_repo`, `clone_repo_with_token`, `clone_with_askpass`,
// `push_with_askpass`, `push_branch_with_token`) now go through
// `runtime::execute_bash`. Behavioural note: the previous code used
// `Command::status()` which inherits stdio, so `git clone`'s progress
// streamed to the parent terminal. `execute_bash` captures stdout/stderr,
// so the progress no longer surfaces interactively — callers see the
// captured text only on error. Acceptable for the audit/CLI context.

use crate::error::{AuditError, Result};
use git2::Repository;
use runtime::{BashCommandInput, BashCommandOutput, execute_bash, shell_quote};
use std::path::{Path, PathBuf};
use tracing::info;

// Build a `<KEY>=<val> <KEY>=<val> git <arg> <arg> ...` shell command.
// All `env` values and `args` are shell-quoted; env keys are not (they
// must already be safe identifiers).
fn build_git_command(env: &[(&str, &str)], args: &[&str]) -> String {
    let env_part = env
        .iter()
        .map(|(k, v)| format!("{}={}", k, shell_quote(v)))
        .collect::<Vec<_>>()
        .join(" ");
    let args_part = std::iter::once("git".to_string())
        .chain(args.iter().map(|a| shell_quote(a)))
        .collect::<Vec<_>>()
        .join(" ");
    if env_part.is_empty() {
        args_part
    } else {
        format!("{env_part} {args_part}")
    }
}

// Run a constructed shell command via `runtime::execute_bash` with
// sandboxing disabled (matches the previous `Command::new("git")` path
// which had none) and the requested `cwd`.
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

// Git repository manager
pub struct GitManager {
    // Workspace directory where repos are cloned
    workspace_dir: PathBuf,
    // Whether to do shallow clones
    #[allow(dead_code)]
    shallow_clone: bool,
}

impl GitManager {
    // Create a new git manager
    pub fn new(workspace_dir: PathBuf, shallow_clone: bool) -> Result<Self> {
        // Create workspace directory if it doesn't exist
        std::fs::create_dir_all(&workspace_dir)?;

        Ok(Self {
            workspace_dir,
            shallow_clone,
        })
    }

    /// Get a reference to the configured workspace directory.
    ///
    /// This accessor allows other modules to read the manager's workspace path
    /// without accessing the private field directly.
    pub fn workspace_dir(&self) -> &PathBuf {
        &self.workspace_dir
    }

    // Clone a repository using the git CLI for simplicity.
    // Falls back to using the system `git` command which tends to be more robust
    // across credential helpers and user environments than libgit2 in some cases.
    pub fn clone_repo(&self, url: &str, name: Option<&str>) -> Result<PathBuf> {
        let repo_name = name.unwrap_or_else(|| {
            url.split('/')
                .next_back()
                .unwrap_or("repo")
                .trim_end_matches(".git")
        });

        let target_path = self.workspace_dir.join(repo_name);

        // Remove existing directory if it exists
        if target_path.exists() {
            info!("Removing existing repository at {}", target_path.display());
            std::fs::remove_dir_all(&target_path)?;
        }

        info!("Cloning repository {} to {}", url, target_path.display());

        // Use git CLI for cloning. This avoids lower-level libgit2 pitfalls and aligns with
        // typical developer environments.
        let target_str = target_path.display().to_string();
        let command = build_git_command(
            &[("GIT_TERMINAL_PROMPT", "0")],
            &["clone", "--depth=1", url, &target_str],
        );
        let output = run_git_command(command, None)
            .map_err(|e| AuditError::other(format!("Failed to spawn git clone: {}", e)))?;

        if output.return_code_interpretation.is_some() {
            return Err(AuditError::other(format!(
                "git clone failed for {} ({})",
                url,
                output
                    .return_code_interpretation
                    .as_deref()
                    .unwrap_or("non-zero exit"),
            )));
        }

        Ok(target_path)
    }

    /// Clone a repository using an embedded token in the HTTPS URL.
    ///
    /// This helper is intended for programmatic workflows where a personal access token
    /// is available. It constructs an authenticated HTTPS URL of the form:
    /// `https://<token>@github.com/owner/repo.git` and performs a shallow clone.
    ///
    /// NOTE: Callers must avoid logging the token or the constructed URL.
    pub fn clone_repo_with_token(
        &self,
        url: &str,
        name: Option<&str>,
        token: &str,
    ) -> Result<PathBuf> {
        let repo_name = name.unwrap_or_else(|| {
            url.split('/')
                .next_back()
                .unwrap_or("repo")
                .trim_end_matches(".git")
        });

        let target_path = self.workspace_dir.join(repo_name);

        // Remove existing directory if it exists
        if target_path.exists() {
            info!("Removing existing repository at {}", target_path.display());
            std::fs::remove_dir_all(&target_path)?;
        }

        // Validate HTTPS URL
        if !url.starts_with("https://") {
            return Err(AuditError::other(format!(
                "Unsupported clone URL (only https supported): {}",
                url
            )));
        }

        // Build authenticated URL (do not log)
        let auth_url = url.replacen("https://", &format!("https://{}@", token), 1);

        info!("Cloning (auth) repository to {}", target_path.display());

        let target_str = target_path.display().to_string();
        let command = build_git_command(
            &[("GIT_TERMINAL_PROMPT", "0")],
            &["clone", "--depth=1", &auth_url, &target_str],
        );
        let output = run_git_command(command, None)
            .map_err(|e| AuditError::other(format!("Failed to spawn git clone: {}", e)))?;

        if output.return_code_interpretation.is_some() {
            return Err(AuditError::other(format!(
                "git clone (auth) failed for {} ({})",
                url,
                output
                    .return_code_interpretation
                    .as_deref()
                    .unwrap_or("non-zero exit"),
            )));
        }

        Ok(target_path)
    }

    // Open an existing repository
    pub fn open(&self, path: &Path) -> Result<Repository> {
        Repository::open(path).map_err(|e| {
            AuditError::other(format!(
                "Failed to open repository at {}: {}",
                path.display(),
                e
            ))
        })
    }

    /// Clone a repository using a temporary GIT_ASKPASS helper to provide the token securely.
    ///
    /// This avoids embedding the token into the remote URL and reduces the chance of leaking
    /// credentials to process listings. The helper is written to a temporary file and removed
    /// after the clone completes.
    pub fn clone_with_askpass(
        &self,
        remote_repo_url: &str,
        name: Option<&str>,
        token: &str,
    ) -> Result<PathBuf> {
        use std::os::unix::fs::PermissionsExt;
        use std::time::{SystemTime, UNIX_EPOCH};

        if !remote_repo_url.starts_with("https://") {
            return Err(AuditError::other(format!(
                "Unsupported clone URL (only https supported): {}",
                remote_repo_url
            )));
        }

        let repo_name = name.unwrap_or_else(|| {
            remote_repo_url
                .split('/')
                .next_back()
                .unwrap_or("repo")
                .trim_end_matches(".git")
        });

        let target_path = self.workspace_dir.join(repo_name);

        if target_path.exists() {
            info!("Removing existing repository at {}", target_path.display());
            std::fs::remove_dir_all(&target_path)?;
        }

        // Create an askpass helper script in the system temp directory.
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        let askpass_path = std::env::temp_dir().join(format!("git_askpass_{}.sh", nanos));
        let script = format!("#!/bin/sh\nprintf '%s' '{}'\n", token);

        std::fs::write(&askpass_path, script)?;
        // Make it executable
        let mut perms = std::fs::metadata(&askpass_path)?.permissions();
        perms.set_mode(0o700);
        std::fs::set_permissions(&askpass_path, perms)?;

        // Run git clone with GIT_ASKPASS pointing to the helper.
        let askpass_str = askpass_path.display().to_string();
        let target_str = target_path.display().to_string();
        let command = build_git_command(
            &[
                ("GIT_ASKPASS", &askpass_str),
                ("GIT_TERMINAL_PROMPT", "0"),
            ],
            &["clone", "--depth=1", remote_repo_url, &target_str],
        );
        let output = run_git_command(command, None)
            .map_err(|e| AuditError::other(format!("Failed to spawn git clone: {}", e)))?;

        // Remove helper script regardless of outcome
        let _ = std::fs::remove_file(&askpass_path);

        if output.return_code_interpretation.is_some() {
            return Err(AuditError::other(format!(
                "git clone (askpass) failed for {} ({})",
                remote_repo_url,
                output
                    .return_code_interpretation
                    .as_deref()
                    .unwrap_or("non-zero exit"),
            )));
        }

        Ok(target_path)
    }

    /// Push a branch using a temporary GIT_ASKPASS helper to provide the token securely.
    ///
    /// This encourages avoiding token-in-URL usage and keeps the token out of process arguments.
    pub fn push_with_askpass(&self, repo_path: &Path, branch: &str, token: &str) -> Result<()> {
        use std::os::unix::fs::PermissionsExt;
        use std::time::{SystemTime, UNIX_EPOCH};

        // Create askpass helper
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        let askpass_path = std::env::temp_dir().join(format!("git_askpass_push_{}.sh", nanos));
        let script = format!("#!/bin/sh\nprintf '%s' '{}'\n", token);

        std::fs::write(&askpass_path, script)?;
        let mut perms = std::fs::metadata(&askpass_path)?.permissions();
        perms.set_mode(0o700);
        std::fs::set_permissions(&askpass_path, perms)?;

        // Use origin remote and let askpass supply credentials.
        // `cwd: Some(repo_path)` replaces the previous `git -C <path>` invocation.
        let askpass_str = askpass_path.display().to_string();
        let command = build_git_command(
            &[
                ("GIT_ASKPASS", &askpass_str),
                ("GIT_TERMINAL_PROMPT", "0"),
            ],
            &["push", "-u", "origin", branch],
        );
        let output = run_git_command(command, Some(repo_path))
            .map_err(|e| AuditError::other(format!("Failed to spawn git push: {}", e)))?;

        // Cleanup helper
        let _ = std::fs::remove_file(&askpass_path);

        if output.return_code_interpretation.is_some() {
            return Err(AuditError::other(format!(
                "git push (askpass) failed for branch {} ({})",
                branch,
                output
                    .return_code_interpretation
                    .as_deref()
                    .unwrap_or("non-zero exit"),
            )));
        }

        Ok(())
    }

    // Get the diff for a repository
    pub fn get_diff(&self, repo_path: &Path, base: Option<&str>) -> Result<String> {
        let repo = self.open(repo_path)?;

        // Get HEAD commit
        let head = repo
            .head()
            .map_err(|e| AuditError::other(format!("Failed to get HEAD: {}", e)))?;
        let head_commit = head
            .peel_to_commit()
            .map_err(|e| AuditError::other(format!("Failed to peel HEAD to commit: {}", e)))?;

        // If base is provided, diff against that
        if let Some(base_ref) = base {
            let base_obj = repo.revparse_single(base_ref).map_err(|e| {
                AuditError::other(format!("Failed to parse base ref {}: {}", base_ref, e))
            })?;
            let base_commit = base_obj
                .peel_to_commit()
                .map_err(|e| AuditError::other(format!("Failed to peel base to commit: {}", e)))?;

            let base_tree = base_commit
                .tree()
                .map_err(|e| AuditError::other(format!("Failed to get base tree: {}", e)))?;
            let head_tree = head_commit
                .tree()
                .map_err(|e| AuditError::other(format!("Failed to get HEAD tree: {}", e)))?;

            let diff = repo
                .diff_tree_to_tree(Some(&base_tree), Some(&head_tree), None)
                .map_err(|e| AuditError::other(format!("Failed to create diff: {}", e)))?;

            // Format diff as string
            let mut diff_str = String::new();
            diff.print(git2::DiffFormat::Patch, |_delta, _hunk, line| {
                let content = String::from_utf8_lossy(line.content());
                diff_str.push_str(&content);
                true
            })
            .map_err(|e| AuditError::other(format!("Failed to print diff: {}", e)))?;

            return Ok(diff_str);
        }

        // Default: show changes since last commit
        let tree = head_commit
            .tree()
            .map_err(|e| AuditError::other(format!("Failed to get commit tree: {}", e)))?;
        let diff = repo
            .diff_tree_to_workdir_with_index(Some(&tree), None)
            .map_err(|e| AuditError::other(format!("Failed to create diff: {}", e)))?;

        let mut diff_str = String::new();
        diff.print(git2::DiffFormat::Patch, |_delta, _hunk, line| {
            let content = String::from_utf8_lossy(line.content());
            diff_str.push_str(&content);
            true
        })
        .map_err(|e| AuditError::other(format!("Failed to print diff: {}", e)))?;

        Ok(diff_str)
    }

    // Checkout a specific branch
    pub fn checkout(&self, repo_path: &Path, branch: &str) -> Result<()> {
        let repo = self.open(repo_path)?;

        // Find the branch
        let (obj, reference) = repo
            .revparse_ext(branch)
            .map_err(|e| AuditError::other(format!("Failed to find branch {}: {}", branch, e)))?;

        repo.checkout_tree(&obj, None).map_err(|e| {
            AuditError::other(format!(
                "Failed to checkout tree for branch {}: {}",
                branch, e
            ))
        })?;

        // Update HEAD
        if let Some(reference) = reference {
            repo.set_head(reference.name().unwrap())
                .map_err(|e| AuditError::other(format!("Failed to set HEAD: {}", e)))?;
        }

        info!("Checked out branch: {}", branch);
        Ok(())
    }

    /// Push a local branch to the remote repository using a token-authenticated HTTPS URL.
    ///
    /// This helper performs a `git push` using an authenticated HTTPS remote of the form:
    /// `https://<token>@github.com/owner/repo.git`. The function avoids logging the token and
    /// sets `GIT_TERMINAL_PROMPT=0` to prevent interactive credential prompts.
    ///
    /// Arguments:
    /// * `repo_path` - local repository working directory
    /// * `remote_repo_url` - standard HTTPS clone URL (e.g. "https://github.com/owner/repo.git")
    /// * `branch` - branch name to push
    /// * `token` - personal access token used for authentication
    pub fn push_branch_with_token(
        &self,
        repo_path: &Path,
        remote_repo_url: &str,
        branch: &str,
        token: &str,
    ) -> Result<()> {
        // Validate remote URL and construct an authenticated URL for the push.
        // We intentionally do not log `auth_url` since it contains sensitive credentials.
        if !remote_repo_url.starts_with("https://") {
            return Err(AuditError::other(format!(
                "Unsupported remote URL (must be https): {}",
                remote_repo_url
            )));
        }

        // Insert token into URL: https://<token>@github.com/owner/repo.git
        let auth_url = remote_repo_url.replacen("https://", &format!("https://{}@", token), 1);

        // Execute: push -u <auth_url> <branch> in `repo_path` (replaces
        // the previous `git -C <path>` invocation).
        let command = build_git_command(
            &[("GIT_TERMINAL_PROMPT", "0")],
            &["push", "-u", &auth_url, branch],
        );
        let output = run_git_command(command, Some(repo_path))
            .map_err(|e| AuditError::other(format!("Failed to spawn git push: {}", e)))?;

        if output.return_code_interpretation.is_some() {
            return Err(AuditError::other(format!(
                "git push failed ({})",
                output
                    .return_code_interpretation
                    .as_deref()
                    .unwrap_or("non-zero exit"),
            )));
        }

        Ok(())
    }

    // Get the current branch name
    pub fn current_branch(&self, repo_path: &Path) -> Result<String> {
        let repo = self.open(repo_path)?;
        let head = repo
            .head()
            .map_err(|e| AuditError::other(format!("Failed to get HEAD: {}", e)))?;

        if let Some(name) = head.shorthand() {
            Ok(name.to_string())
        } else {
            Err(AuditError::other("Could not determine current branch"))
        }
    }

    // Get repository statistics
    pub fn stats(&self, repo_path: &Path) -> Result<RepoStats> {
        let repo = self.open(repo_path)?;

        // Count commits
        let mut revwalk = repo
            .revwalk()
            .map_err(|e| AuditError::other(format!("Failed to create revwalk: {}", e)))?;
        revwalk
            .push_head()
            .map_err(|e| AuditError::other(format!("Failed to push HEAD to revwalk: {}", e)))?;
        let commit_count = revwalk.count();

        // Count branches
        let branches = repo
            .branches(None)
            .map_err(|e| AuditError::other(format!("Failed to list branches: {}", e)))?
            .count();

        // Get latest commit info
        let head = repo
            .head()
            .map_err(|e| AuditError::other(format!("Failed to get HEAD: {}", e)))?;
        let commit = head
            .peel_to_commit()
            .map_err(|e| AuditError::other(format!("Failed to peel to commit: {}", e)))?;

        let latest_commit = CommitInfo {
            hash: commit.id().to_string(),
            message: commit.message().unwrap_or("").to_string(),
            author: commit.author().name().unwrap_or("").to_string(),
            timestamp: commit.time().seconds(),
        };

        Ok(RepoStats {
            commit_count,
            branch_count: branches,
            latest_commit,
        })
    }

    // Check if a path is a git repository
    pub fn is_repository(&self, path: &Path) -> bool {
        Repository::open(path).is_ok()
    }

    // Update (pull) an existing repository
    pub fn update(&self, repo_path: &Path) -> Result<()> {
        let repo = self.open(repo_path)?;

        info!("Updating repository at {}", repo_path.display());

        // Fetch from origin
        let mut remote = repo
            .find_remote("origin")
            .map_err(|e| AuditError::other(format!("Failed to find remote 'origin': {}", e)))?;

        remote
            .fetch(&["main", "master"], None, None)
            .map_err(|e| AuditError::other(format!("Failed to fetch from origin: {}", e)))?;

        info!("Repository updated successfully");
        Ok(())
    }
}

// Repository statistics
#[derive(Debug, Clone)]
pub struct RepoStats {
    // Total commit count
    pub commit_count: usize,
    // Number of branches
    pub branch_count: usize,
    // Latest commit information
    pub latest_commit: CommitInfo,
}

// Commit information
#[derive(Debug, Clone)]
pub struct CommitInfo {
    // Commit hash
    pub hash: String,
    // Commit message
    pub message: String,
    // Author name
    pub author: String,
    // Timestamp (seconds since epoch)
    pub timestamp: i64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_new_git_manager() {
        let temp = TempDir::new().unwrap();
        let _manager = GitManager::new(temp.path().to_path_buf(), true).unwrap();
        assert!(temp.path().exists());
    }

    #[test]
    fn test_is_repository() {
        let temp = TempDir::new().unwrap();
        let manager = GitManager::new(temp.path().to_path_buf(), true).unwrap();

        // Not a repo yet
        assert!(!manager.is_repository(temp.path()));

        // Initialize a repo
        Repository::init(temp.path()).unwrap();

        // Now it is a repo
        assert!(manager.is_repository(temp.path()));
    }
}
