//! Git repository management for audit service

use crate::error::{AuditError, Result};
use git2::Repository;
use std::path::{Path, PathBuf};
use tracing::info;

/// Git repository manager
pub struct GitManager {
    /// Workspace directory where repos are cloned
    workspace_dir: PathBuf,
    /// Whether to do shallow clones
    #[allow(dead_code)]
    shallow_clone: bool,
}

impl GitManager {
    /// Create a new git manager
    pub fn new(workspace_dir: PathBuf, shallow_clone: bool) -> Result<Self> {
        // Create workspace directory if it doesn't exist
        std::fs::create_dir_all(&workspace_dir)?;

        Ok(Self {
            workspace_dir,
            shallow_clone,
        })
    }

    /// Clone a repository
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

        // For now, use simple clone (can be enhanced with shallow clone options)
        Repository::clone(url, &target_path).map_err(|e| {
            AuditError::other(format!("Failed to clone repository from {}: {}", url, e))
        })?;

        Ok(target_path)
    }

    /// Open an existing repository
    pub fn open(&self, path: &Path) -> Result<Repository> {
        Repository::open(path).map_err(|e| {
            AuditError::other(format!(
                "Failed to open repository at {}: {}",
                path.display(),
                e
            ))
        })
    }

    /// Get the diff for a repository
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

    /// Checkout a specific branch
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

    /// Get the current branch name
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

    /// Get repository statistics
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

    /// Check if a path is a git repository
    pub fn is_repository(&self, path: &Path) -> bool {
        Repository::open(path).is_ok()
    }

    /// Update (pull) an existing repository
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

/// Repository statistics
#[derive(Debug, Clone)]
pub struct RepoStats {
    /// Total commit count
    pub commit_count: usize,
    /// Number of branches
    pub branch_count: usize,
    /// Latest commit information
    pub latest_commit: CommitInfo,
}

/// Commit information
#[derive(Debug, Clone)]
pub struct CommitInfo {
    /// Commit hash
    pub hash: String,
    /// Commit message
    pub message: String,
    /// Author name
    pub author: String,
    /// Timestamp (seconds since epoch)
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
