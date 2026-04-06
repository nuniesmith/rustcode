/*
Task Executor (TASK-C) — Dry-run executor

This module implements a simple dry-run task executor that:
- Simulates cloning a repository (by creating a local repo directory and initializing a git repo)
- Creates a branch
- Iterates steps and logs simulated actions for each step
- Writes a TaskResult JSON file to `tasks/results/{task_id}.json`

The implementation intentionally avoids network clones and instead performs a local init so
the executor can be used safely in CI/dev environments without external Git access.

Exports:
- `TaskExecutor` with `execute_dry_run` method.

Note: This is a starting point — the full executor (real clone, LLM actions, commits, PR creation)
should be implemented later and will reuse parts of the logic here.
*/

use crate::git::GitManager;
use crate::github::GitHubClient;
use crate::task::{StepResult, TaskFile, TaskResult};
use chrono::Utc;
use git2::{Repository, Signature};
use serde_json::to_writer_pretty;
use std::fs;
use std::io::BufWriter;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tempfile::TempDir;
use tokio::task;
use tracing::{info, warn};

/// TaskExecutorOptions control behavior of the executor.
#[derive(Debug, Clone)]
pub struct TaskExecutorOptions {
    /// Base directory where repositories are (or will be) placed for dry-run work.
    /// If None, a temporary directory will be used.
    pub workspace_dir: Option<PathBuf>,

    /// When true, the executor performs a dry-run simulation (no network clone).
    pub dry_run: bool,
}

impl Default for TaskExecutorOptions {
    fn default() -> Self {
        Self {
            workspace_dir: None,
            dry_run: true,
        }
    }
}

/// TaskExecutor performs execution of `TaskFile` items.
/// Current implementation is a dry-run executor that simulates actions and writes a result file.
pub struct TaskExecutor {
    git_manager: Arc<GitManager>,
    opts: TaskExecutorOptions,
}

impl TaskExecutor {
    /// Create a new TaskExecutor.
    pub fn new(git_manager: Arc<GitManager>, opts: TaskExecutorOptions) -> Self {
        Self { git_manager, opts }
    }

    /// Execute the given task in dry-run mode.
    ///
    /// Steps:
    /// 1. Prepare workspace directory
    /// 2. Create or init a local git repo for the target `owner/repo`
    /// 3. Create branch specified by task.branch
    /// 4. For each step, record a simulated StepResult
    /// 5. Write TaskResult JSON to `tasks/results/{task_id}.json`
    pub fn execute_dry_run(&self, task: &TaskFile) -> anyhow::Result<TaskResult> {
        // Prepare workspace directory
        let workspace = match &self.opts.workspace_dir {
            Some(p) => p.clone(),
            None => {
                // Use configured git_manager workspace if available, otherwise temp dir
                let gm_dir = self.git_manager.workspace_dir().clone();
                if gm_dir.exists() {
                    gm_dir
                } else {
                    // Fallback to tempdir
                    let tmp = TempDir::new()?;
                    tmp.into_path()
                }
            }
        };

        info!(
            "TaskExecutor (dry-run): using workspace {}",
            workspace.display()
        );

        // Derive repository name and local path
        let repo_name = match task.repo.split('/').last() {
            Some(s) => s.trim_end_matches(".git"),
            None => &task.repo,
        };
        let local_repo_path = workspace.join(repo_name);

        // Ensure parent exists
        if let Some(parent) = local_repo_path.parent() {
            fs::create_dir_all(parent)?;
        }

        // Initialize or open repository locally (simulate clone)
        let repo = match Repository::open(&local_repo_path) {
            Ok(r) => {
                info!(
                    "Opened existing repository at {}",
                    local_repo_path.display()
                );
                r
            }
            Err(_) => {
                info!(
                    "Simulating clone: initializing local repository at {}",
                    local_repo_path.display()
                );
                fs::create_dir_all(&local_repo_path)?;
                let r = Repository::init(&local_repo_path)?;
                // Create an initial commit so we can branch off it
                Self::make_initial_commit(&r)?;
                r
            }
        };

        // Attempt to create the branch specified by the task
        let mut branch_created = false;
        let branch_name = task.branch.trim();
        if !branch_name.is_empty() {
            match Self::create_branch(&repo, branch_name) {
                Ok(_) => {
                    info!("Created branch '{}' (dry-run)", branch_name);
                    branch_created = true;
                }
                Err(e) => {
                    warn!(
                        "Failed to create branch '{}': {} (continuing)",
                        branch_name, e
                    );
                }
            }
        }

        // Process steps: simulate LLM / action execution
        let mut step_results: Vec<StepResult> = Vec::with_capacity(task.steps.len());
        for step in &task.steps {
            info!("Simulating step: {}", step);

            // Simulated actions (in future: call LLM + runtime to produce actions)
            let actions = vec![format!(
                "simulated: would perform action for step: {}",
                step
            )];

            let sr = StepResult {
                step: step.clone(),
                status: "success".to_string(),
                actions,
                test_output: None,
                error: None,
                completed_at: Some(Utc::now().timestamp()),
            };

            step_results.push(sr);
        }

        // Build TaskResult
        let started_at = Utc::now().timestamp();
        let completed_at = Utc::now().timestamp();

        let result = TaskResult {
            task_id: task.id.clone(),
            status: "success".to_string(),
            pr_url: None,
            branch: branch_name.to_string(),
            step_results,
            error: None,
            started_at,
            completed_at,
            duration_secs: 0,
        };

        // Write result file to tasks/results/{id}.json
        let results_dir = Path::new("tasks").join("results");
        if !results_dir.exists() {
            fs::create_dir_all(&results_dir)?;
        }

        let result_path = results_dir.join(format!("{}.json", task.id));
        let file = fs::File::create(&result_path)?;
        let writer = BufWriter::new(file);
        to_writer_pretty(writer, &result)?;

        info!("Dry-run TaskResult written to {}", result_path.display());

        Ok(result)
    }

    /// Create an initial commit in an empty repository so branch creation is possible.
    fn make_initial_commit(repo: &Repository) -> anyhow::Result<()> {
        // Create a README file
        let repo_path = repo
            .path()
            .parent()
            .ok_or_else(|| anyhow::anyhow!("Repository path invalid when making initial commit"))?;
        let readme = repo_path.join("README.md");
        fs::write(&readme, "# Temporary repo for task executor (dry-run)\n")?;

        // Add README to index
        let mut index = repo.index()?;
        index.add_path(std::path::Path::new("README.md"))?;
        index.write()?;
        let tree_id = index.write_tree()?;
        let tree = repo.find_tree(tree_id)?;

        // Signature
        let sig = Signature::now("rustcode-task-executor", "noreply@example.com")?;

        // Create initial commit with no parent
        repo.commit(
            Some("HEAD"),
            &sig,
            &sig,
            "chore: initial commit (task executor dry-run)",
            &tree,
            &[],
        )?;

        Ok(())
    }

    /// Execute the task in real mode:
    /// - Clone the repo using the provided GitHub token
    /// - Create branch, apply simple changes for each step, commit, push
    /// - Create a GitHub Pull Request and return a TaskResult referencing the PR
    pub async fn execute_real(
        &self,
        task: &TaskFile,
        github_token: &str,
        _github_username: &str,
    ) -> anyhow::Result<TaskResult> {
        use std::process::Command as StdCommand;

        // Parse owner/repo
        let parts: Vec<&str> = task.repo.split('/').collect();
        if parts.len() != 2 {
            return Err(anyhow::anyhow!("task.repo must be in format 'owner/repo'"));
        }
        let owner = parts[0];
        let repo_name = parts[1];

        // Build remote HTTPS URL (no token embedded here; used later only for push)
        let remote_url = format!("https://github.com/{}/{}.git", owner, repo_name);

        // 1) Clone repository with token (blocking)
        let gm = Arc::clone(&self.git_manager);
        let remote_url_clone = remote_url.clone();
        let token_clone = github_token.to_string();
        let clone_result = task::spawn_blocking(move || {
            gm.clone_repo_with_token(&remote_url_clone, None, &token_clone)
        })
        .await
        .map_err(|e| anyhow::anyhow!("clone task join error: {}", e))??;

        let repo_path = clone_result;

        // 2) Create branch (blocking)
        let branch = task.branch.clone();
        let repo_path_branch = repo_path.clone();
        let branch_clone = branch.clone();
        let create_branch_res = task::spawn_blocking(move || {
            let status = StdCommand::new("git")
                .arg("-C")
                .arg(&repo_path_branch)
                .arg("checkout")
                .arg("-b")
                .arg(&branch_clone)
                .env("GIT_TERMINAL_PROMPT", "0")
                .status()
                .map_err(|e| anyhow::anyhow!("failed to spawn git checkout: {}", e))?;
            if !status.success() {
                return Err(anyhow::anyhow!("git checkout -b failed"));
            }
            Ok::<(), anyhow::Error>(())
        })
        .await
        .map_err(|e| anyhow::anyhow!("branch task join error: {}", e))??;

        // 3) Apply changes for each step (simple file writes) and commit
        // Prepare a working copy of needed strings to move into blocking closure
        let repo_path_commit = repo_path.clone();
        let steps = task.steps.clone();
        let task_id = task.id.clone();
        let commit_message = format!("Task {}: apply changes", task_id);
        let token_for_push = github_token.to_string();
        let remote_url_for_push = remote_url.clone();
        let branch_for_push = branch.clone();

        let commit_and_push_res = task::spawn_blocking(move || -> anyhow::Result<()> {
            // For each step create/update a file that documents the step
            for (i, step) in steps.iter().enumerate() {
                let filename = format!("rustcode_task_{}_step_{}.txt", task_id, i);
                let path = repo_path_commit.join(&filename);
                std::fs::write(&path, format!("Step: {}\n", step))?;
            }

            // git add .
            let status = StdCommand::new("git")
                .arg("-C")
                .arg(&repo_path_commit)
                .arg("add")
                .arg(".")
                .status()?;
            if !status.success() {
                return Err(anyhow::anyhow!("git add failed"));
            }

            // git commit -m "<message>"
            let status = StdCommand::new("git")
                .arg("-C")
                .arg(&repo_path_commit)
                .arg("commit")
                .arg("-m")
                .arg(&commit_message)
                .status()?;
            if !status.success() {
                // If there's nothing to commit (no changes), it's not fatal
                // but we continue to push the branch.
            }

            // Construct authenticated push URL for push (do NOT log)
            let auth_push_url = remote_url_for_push.replacen(
                "https://",
                &format!("https://{}@", token_for_push),
                1,
            );

            // git push -u <auth_push_url> <branch>
            let status = StdCommand::new("git")
                .arg("-C")
                .arg(&repo_path_commit)
                .arg("push")
                .arg("-u")
                .arg(&auth_push_url)
                .arg(&branch_for_push)
                .env("GIT_TERMINAL_PROMPT", "0")
                .status()?;
            if !status.success() {
                return Err(anyhow::anyhow!("git push failed"));
            }

            Ok(())
        })
        .await
        .map_err(|e| anyhow::anyhow!("commit/push join error: {}", e))??;

        // 4) Create Pull Request via GitHub API
        let gh_client = GitHubClient::new(github_token.to_string())
            .map_err(|e| anyhow::anyhow!(format!("failed to create GitHub client: {}", e)))?;

        let title = format!("Task: {} — {}", task.id, task.description);
        // Default base branch: main (could be configurable)
        let base = "main";
        let head = branch.as_str();

        let pr = gh_client
            .create_pull_request(
                owner,
                repo_name,
                &title,
                head,
                base,
                Some(&task.description),
                false,
            )
            .await
            .map_err(|e| anyhow::anyhow!(format!("failed to create PR: {}", e)))?;

        // 5) Build TaskResult and persist it
        let start = Utc::now();
        let end = Utc::now();
        let duration = 0u64;

        let step_results: Vec<StepResult> = task
            .steps
            .iter()
            .map(|s| StepResult {
                step: s.clone(),
                status: "success".to_string(),
                actions: vec![format!("applied step: {}", s)],
                test_output: None,
                error: None,
                completed_at: Some(Utc::now().timestamp()),
            })
            .collect();

        let result = TaskResult {
            task_id: task.id.clone(),
            status: "success".to_string(),
            pr_url: Some(pr.html_url.clone()),
            branch: branch.clone(),
            step_results: step_results.clone(),
            error: None,
            started_at: start.timestamp(),
            completed_at: end.timestamp(),
            duration_secs: duration,
        };

        // Write result file
        let results_dir = Path::new("tasks").join("results");
        if !results_dir.exists() {
            std::fs::create_dir_all(&results_dir)?;
        }
        let result_path = results_dir.join(format!("{}.json", task.id));
        let file = fs::File::create(&result_path)?;
        let writer = BufWriter::new(file);
        to_writer_pretty(writer, &result)?;

        Ok(result)
    }

    /// Create a branch pointing to HEAD (requires at least one commit).
    fn create_branch(repo: &Repository, branch: &str) -> anyhow::Result<()> {
        // Resolve HEAD commit
        let obj = repo.revparse_single("HEAD")?;
        let commit = obj
            .peel_to_commit()
            .map_err(|e| anyhow::anyhow!("Failed to peel HEAD to commit: {}", e))?;

        // Create branch
        let _branch_ref = repo.branch(branch, &commit, false)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::GitManager;
    use std::sync::Arc;
    use tempfile::TempDir;

    /// Verify that dry-run execution produces a results file and step results.
    #[test]
    fn test_execute_dry_run_writes_result() {
        // Setup a temporary workspace
        let temp = TempDir::new().expect("tempdir");
        let workspace_dir = temp.path().join("workspace");
        fs::create_dir_all(&workspace_dir).unwrap();

        // Create a GitManager that points to the temporary workspace
        let gm = GitManager::new(workspace_dir.clone(), true).expect("git manager");
        let gm_arc = Arc::new(gm);

        // Executor with explicit workspace_dir
        let opts = TaskExecutorOptions {
            workspace_dir: Some(workspace_dir.clone()),
            dry_run: true,
        };
        let executor = TaskExecutor::new(gm_arc, opts);

        // Build a minimal TaskFile
        let task = TaskFile {
            id: "dryrun-test-1".to_string(),
            repo: "owner/repo".to_string(),
            description: "A test dry-run".to_string(),
            steps: vec!["Create src/lib.rs".to_string(), "Add tests".to_string()],
            branch: "feat/test-branch".to_string(),
            labels: vec!["test".to_string()],
            auto_merge: false,
        };

        // Execute
        let result = executor
            .execute_dry_run(&task)
            .expect("execute should succeed");

        // Validate returned result
        assert_eq!(result.task_id, "dryrun-test-1");
        assert_eq!(result.status, "success");
        assert_eq!(result.step_results.len(), 2);

        // Check result file exists
        let result_path = Path::new("tasks")
            .join("results")
            .join("dryrun-test-1.json");
        assert!(result_path.exists());

        // Clean up created results file for test hygiene
        let _ = fs::remove_file(result_path);
    }
}
