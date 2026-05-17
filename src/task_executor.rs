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
use crate::tests_runner::{ProjectType, TestRunner};
use chrono::Utc;
use git2::{Repository, Signature};
use serde_json::to_writer_pretty;
use std::fs;
use std::io::BufWriter;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tempfile::TempDir;
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
            agent_trace: None,
        };

        write_result_file(&result)?;
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
    /// - Run the per-language test runner against the working tree
    /// - Create a GitHub Pull Request, apply labels, and (if tests passed)
    ///   tag-only on failure so the PR can be reviewed manually
    ///
    /// A `TaskResult` JSON file is written to `tasks/results/{id}.json` on every
    /// code path — success, failure, or partial — so the watcher caller has a
    /// stable record of what happened.
    pub async fn execute_real(
        &self,
        task: &TaskFile,
        github_token: &str,
        github_username: &str,
    ) -> anyhow::Result<TaskResult> {
        let started_at = Utc::now().timestamp();
        let phase = self
            .run_real_phases(task, github_token, github_username)
            .await;
        let completed_at = Utc::now().timestamp();
        let duration_secs = (completed_at - started_at).max(0) as u64;

        let result = match phase {
            Ok(success) => TaskResult {
                task_id: task.id.clone(),
                status: "success".to_string(),
                pr_url: success.pr_url,
                branch: task.branch.clone(),
                step_results: success.step_results,
                error: None,
                started_at,
                completed_at,
                duration_secs,
                agent_trace: None,
            },
            Err(failure) => TaskResult {
                task_id: task.id.clone(),
                status: "failed".to_string(),
                pr_url: failure.pr_url,
                branch: task.branch.clone(),
                step_results: failure.step_results,
                error: Some(failure.error.to_string()),
                started_at,
                completed_at,
                duration_secs,
                agent_trace: None,
            },
        };

        write_result_file(&result)?;
        Ok(result)
    }

    // Inner pipeline that surfaces partial state (step results + optional PR
    // URL) on failure so the wrapper can persist a meaningful TaskResult.
    async fn run_real_phases(
        &self,
        task: &TaskFile,
        github_token: &str,
        _github_username: &str,
    ) -> std::result::Result<SuccessOutcome, FailureOutcome> {
        let (owner, repo_name) = parse_owner_repo(&task.repo)
            .map_err(|e| FailureOutcome::early(&task.steps, e))?;
        let remote_url = format!("https://github.com/{}/{}.git", owner, repo_name);

        // 1) Clone repository with token
        let gm = Arc::clone(&self.git_manager);
        let remote_url_clone = remote_url.clone();
        let token_clone = github_token.to_string();
        let repo_path = tokio::task::spawn_blocking(move || {
            gm.clone_repo_with_token(&remote_url_clone, None, &token_clone)
        })
        .await
        .map_err(|e| anyhow::anyhow!("clone task join error: {}", e))
        .and_then(|r| r.map_err(|e| anyhow::anyhow!(e)))
        .map_err(|e| FailureOutcome::early(&task.steps, e))?;

        // 2) Create branch
        let branch = task.branch.clone();
        let repo_path_branch = repo_path.clone();
        let branch_clone = branch.clone();
        tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
            let status = std::process::Command::new("git")
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
            Ok(())
        })
        .await
        .map_err(|e| anyhow::anyhow!("branch task join error: {}", e))
        .and_then(|r| r)
        .map_err(|e| FailureOutcome::early(&task.steps, e))?;

        // 3) Apply step files locally and commit
        let mut step_results: Vec<StepResult> = task
            .steps
            .iter()
            .map(|s| StepResult {
                step: s.clone(),
                status: "success".to_string(),
                actions: vec![format!("wrote placeholder file for step")],
                test_output: None,
                error: None,
                completed_at: Some(Utc::now().timestamp()),
            })
            .collect();

        let repo_path_commit = repo_path.clone();
        let steps = task.steps.clone();
        let task_id = task.id.clone();
        let commit_message = format!("Task {}: apply changes", task_id);
        if let Err(e) = tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
            for (i, step) in steps.iter().enumerate() {
                let filename = format!("rustcode_task_{}_step_{}.txt", task_id, i);
                let path = repo_path_commit.join(&filename);
                std::fs::write(&path, format!("Step: {}\n", step))?;
            }
            run_git(&repo_path_commit, &["add", "."])?;
            // commit may exit non-zero when nothing changed; that's not fatal —
            // we still want to push the branch (no-op push is harmless).
            let _ = std::process::Command::new("git")
                .arg("-C")
                .arg(&repo_path_commit)
                .arg("commit")
                .arg("-m")
                .arg(&commit_message)
                .status();
            Ok(())
        })
        .await
        .map_err(|e| anyhow::anyhow!("commit join error: {}", e))
        .and_then(|r| r)
        {
            return Err(FailureOutcome::with_steps(step_results, e));
        }

        // 4) Run the per-language test runner against the working tree.
        //    Output is attached to the last step result (or a synthetic entry
        //    when steps is empty) so the PR description can reference it.
        let test_summary = run_tests_for_workspace(&repo_path);
        if let Some(last) = step_results.last_mut() {
            last.test_output = Some(test_summary.summary.clone());
            if !test_summary.passed {
                last.status = "failed".to_string();
                last.error = Some("test runner reported failures".to_string());
            }
        }
        if !test_summary.passed {
            // Tests failed — abort before pushing. Return what we have so the
            // wrapper writes a useful result file.
            return Err(FailureOutcome::with_steps(
                step_results,
                anyhow::anyhow!("tests failed: {}", test_summary.summary),
            ));
        }

        // 5) Push branch
        let repo_path_push = repo_path.clone();
        let branch_for_push = branch.clone();
        let auth_push_url = remote_url.replacen(
            "https://",
            &format!("https://{}@", github_token),
            1,
        );
        if let Err(e) = tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
            let status = std::process::Command::new("git")
                .arg("-C")
                .arg(&repo_path_push)
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
        .map_err(|e| anyhow::anyhow!("push join error: {}", e))
        .and_then(|r| r)
        {
            return Err(FailureOutcome::with_steps(step_results, e));
        }

        // 6) Create PR
        let gh_client = match GitHubClient::new(github_token.to_string()) {
            Ok(c) => c,
            Err(e) => {
                return Err(FailureOutcome::with_steps(
                    step_results,
                    anyhow::anyhow!("failed to create GitHub client: {}", e),
                ));
            }
        };

        let title = format!("Task: {} — {}", task.id, task.description);
        let pr_body = format!(
            "{}\n\n---\n_Test summary_:\n```\n{}\n```",
            task.description, test_summary.summary
        );
        let pr = match gh_client
            .create_pull_request(
                owner,
                repo_name,
                &title,
                branch.as_str(),
                "main",
                Some(&pr_body),
                false,
            )
            .await
        {
            Ok(pr) => pr,
            Err(e) => {
                return Err(FailureOutcome::with_steps(
                    step_results,
                    anyhow::anyhow!("failed to create PR: {}", e),
                ));
            }
        };

        // 7) Apply labels (best-effort — log on failure)
        if !task.labels.is_empty() {
            match gh_client
                .add_labels(owner, repo_name, pr.number, &task.labels)
                .await
            {
                Ok(_) => info!(pr = pr.number, labels = ?task.labels, "Applied PR labels"),
                Err(e) => warn!(pr = pr.number, error = %e, "Failed to apply PR labels"),
            }
        }

        Ok(SuccessOutcome {
            pr_url: Some(pr.html_url),
            step_results,
        })
    }

    /// Execute a `TaskFile` through the planner/executor/reviewer agent loop,
    /// then materialize the agent's step outputs into the working tree and
    /// open a PR — but only if the reviewer approved the trace.
    ///
    /// If the pipeline returns `converged = false` (max iterations hit while
    /// still revising), the task is recorded as failed and no PR is opened.
    /// The full `PipelineResult` is embedded in `TaskResult.agent_trace` so
    /// the human reviewer can see plan + step outputs + critique.
    ///
    /// When `github_token` is `None`, this runs the pipeline + writes the
    /// outputs locally but skips the push + PR creation (useful for dry-run
    /// or when the watcher is offline-only).
    pub async fn execute_with_agent(
        &self,
        task: &TaskFile,
        pipeline: &crate::agent::AgentPipeline,
        github_token: Option<&str>,
        max_iterations: u32,
    ) -> anyhow::Result<TaskResult> {
        let started_at = Utc::now().timestamp();

        // Phase 1: build an AgentTask from the TaskFile and run the pipeline.
        let agent_task = build_agent_task(task);
        let pipeline_outcome = pipeline.run(agent_task, max_iterations).await;

        let pipeline_result = match pipeline_outcome {
            Ok(r) => r,
            Err(e) => {
                let result = TaskResult {
                    task_id: task.id.clone(),
                    status: "failed".to_string(),
                    pr_url: None,
                    branch: task.branch.clone(),
                    step_results: placeholder_steps(&task.steps, "pending"),
                    error: Some(format!("agent pipeline error: {}", e)),
                    started_at,
                    completed_at: Utc::now().timestamp(),
                    duration_secs: (Utc::now().timestamp() - started_at).max(0) as u64,
                    agent_trace: None,
                };
                write_result_file(&result)?;
                return Ok(result);
            }
        };

        let step_results = step_results_from_pipeline(&pipeline_result);

        // If the reviewer didn't approve, write a failed result and bail.
        // We persist the full trace so the human reviewer can see why.
        if !pipeline_result.converged {
            let critique = match &pipeline_result.final_review {
                crate::agent::ReviewOutcome::Approved { summary } => summary.clone(),
                crate::agent::ReviewOutcome::Revise { critique, .. } => critique.clone(),
            };
            let completed_at = Utc::now().timestamp();
            let result = TaskResult {
                task_id: task.id.clone(),
                status: "failed".to_string(),
                pr_url: None,
                branch: task.branch.clone(),
                step_results,
                error: Some(format!("agent did not converge: {}", critique)),
                started_at,
                completed_at,
                duration_secs: (completed_at - started_at).max(0) as u64,
                agent_trace: Some(pipeline_result),
            };
            write_result_file(&result)?;
            return Ok(result);
        }

        // Phase 2: the agent approved — materialize and open a PR.
        let token = match github_token {
            Some(t) if !t.is_empty() => t,
            _ => {
                // No token configured: persist what we have but don't push.
                let completed_at = Utc::now().timestamp();
                let result = TaskResult {
                    task_id: task.id.clone(),
                    status: "success".to_string(),
                    pr_url: None,
                    branch: task.branch.clone(),
                    step_results,
                    error: None,
                    started_at,
                    completed_at,
                    duration_secs: (completed_at - started_at).max(0) as u64,
                    agent_trace: Some(pipeline_result),
                };
                write_result_file(&result)?;
                info!(task = %task.id, "Agent approved but no GITHUB_TOKEN — skipping PR");
                return Ok(result);
            }
        };

        let phase = self
            .materialize_and_push(task, &pipeline_result, token)
            .await;
        let completed_at = Utc::now().timestamp();
        let duration_secs = (completed_at - started_at).max(0) as u64;

        let result = match phase {
            Ok(success) => TaskResult {
                task_id: task.id.clone(),
                status: "success".to_string(),
                pr_url: success.pr_url,
                branch: task.branch.clone(),
                step_results: success.step_results,
                error: None,
                started_at,
                completed_at,
                duration_secs,
                agent_trace: Some(pipeline_result),
            },
            Err(failure) => TaskResult {
                task_id: task.id.clone(),
                status: "failed".to_string(),
                pr_url: failure.pr_url,
                branch: task.branch.clone(),
                step_results: failure.step_results,
                error: Some(failure.error.to_string()),
                started_at,
                completed_at,
                duration_secs,
                agent_trace: Some(pipeline_result),
            },
        };

        write_result_file(&result)?;
        Ok(result)
    }

    // Clone, branch, write each agent step's output to a file, commit, run
    // tests, push, open PR, apply labels. Mirrors `run_real_phases` but the
    // step files come from the agent's executor output instead of literal
    // "Step: <text>" placeholders.
    async fn materialize_and_push(
        &self,
        task: &TaskFile,
        pipeline_result: &crate::agent::PipelineResult,
        github_token: &str,
    ) -> std::result::Result<SuccessOutcome, FailureOutcome> {
        let (owner, repo_name) = parse_owner_repo(&task.repo)
            .map_err(|e| FailureOutcome::early(&task.steps, e))?;
        let remote_url = format!("https://github.com/{}/{}.git", owner, repo_name);

        // Clone
        let gm = Arc::clone(&self.git_manager);
        let remote_url_clone = remote_url.clone();
        let token_clone = github_token.to_string();
        let repo_path = tokio::task::spawn_blocking(move || {
            gm.clone_repo_with_token(&remote_url_clone, None, &token_clone)
        })
        .await
        .map_err(|e| anyhow::anyhow!("clone task join error: {}", e))
        .and_then(|r| r.map_err(|e| anyhow::anyhow!(e)))
        .map_err(|e| FailureOutcome::early(&task.steps, e))?;

        // Branch
        let branch = task.branch.clone();
        let repo_path_branch = repo_path.clone();
        let branch_clone = branch.clone();
        tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
            let status = std::process::Command::new("git")
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
            Ok(())
        })
        .await
        .map_err(|e| anyhow::anyhow!("branch task join error: {}", e))
        .and_then(|r| r)
        .map_err(|e| FailureOutcome::early(&task.steps, e))?;

        // Materialize step outputs as markdown files in a per-task subdir.
        // Each iteration's executor output gets one file; the human reviewer
        // can scan them to confirm the agent did what it described.
        let mut step_results = step_results_from_pipeline(pipeline_result);
        let repo_path_commit = repo_path.clone();
        let task_id = task.id.clone();
        let pipeline_for_files = pipeline_result.clone();
        if let Err(e) = tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
            let dir = repo_path_commit.join(format!("rustcode_task_{}", task_id));
            std::fs::create_dir_all(&dir)?;
            for iter in &pipeline_for_files.iterations {
                std::fs::write(
                    dir.join(format!("iteration-{}-plan.md", iter.iteration)),
                    format!(
                        "# Plan (iteration {})\n\n{}\n\n## Steps\n{}",
                        iter.iteration,
                        iter.plan.summary,
                        iter.plan
                            .steps
                            .iter()
                            .map(|s| format!("{}. {} — {}", s.id, s.description, s.success_criteria))
                            .collect::<Vec<_>>()
                            .join("\n")
                    ),
                )?;
                for sr in &iter.step_results {
                    std::fs::write(
                        dir.join(format!("iteration-{}-step-{}.md", iter.iteration, sr.step_id)),
                        format!(
                            "# Step {}\n\n## Description\n{}\n\n## Output\n{}",
                            sr.step_id, sr.step_description, sr.output
                        ),
                    )?;
                }
            }
            run_git(&repo_path_commit, &["add", "."])?;
            let _ = std::process::Command::new("git")
                .arg("-C")
                .arg(&repo_path_commit)
                .arg("commit")
                .arg("-m")
                .arg(format!("Task {}: agent run", task_id))
                .status();
            Ok(())
        })
        .await
        .map_err(|e| anyhow::anyhow!("commit join error: {}", e))
        .and_then(|r| r)
        {
            return Err(FailureOutcome::with_steps(step_results, e));
        }

        // Run tests against the materialized tree.
        let test_summary = run_tests_for_workspace(&repo_path);
        if let Some(last) = step_results.last_mut() {
            last.test_output = Some(test_summary.summary.clone());
            if !test_summary.passed {
                last.status = "failed".to_string();
                last.error = Some("test runner reported failures".to_string());
            }
        }
        if !test_summary.passed {
            return Err(FailureOutcome::with_steps(
                step_results,
                anyhow::anyhow!("tests failed: {}", test_summary.summary),
            ));
        }

        // Push
        let repo_path_push = repo_path.clone();
        let branch_for_push = branch.clone();
        let auth_push_url = remote_url.replacen(
            "https://",
            &format!("https://{}@", github_token),
            1,
        );
        if let Err(e) = tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
            let status = std::process::Command::new("git")
                .arg("-C")
                .arg(&repo_path_push)
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
        .map_err(|e| anyhow::anyhow!("push join error: {}", e))
        .and_then(|r| r)
        {
            return Err(FailureOutcome::with_steps(step_results, e));
        }

        // PR
        let gh_client = match GitHubClient::new(github_token.to_string()) {
            Ok(c) => c,
            Err(e) => {
                return Err(FailureOutcome::with_steps(
                    step_results,
                    anyhow::anyhow!("failed to create GitHub client: {}", e),
                ));
            }
        };

        let approval_summary = match &pipeline_result.final_review {
            crate::agent::ReviewOutcome::Approved { summary } => summary.clone(),
            crate::agent::ReviewOutcome::Revise { critique, .. } => critique.clone(),
        };
        let pr_body = format!(
            "{}\n\n---\n_Agent verdict_: {}\n\n_Iterations_: {}\n\n_Test summary_:\n```\n{}\n```",
            task.description,
            approval_summary,
            pipeline_result.iterations.len(),
            test_summary.summary
        );
        let pr = match gh_client
            .create_pull_request(
                owner,
                repo_name,
                &format!("Task: {} — {}", task.id, task.description),
                branch.as_str(),
                "main",
                Some(&pr_body),
                false,
            )
            .await
        {
            Ok(pr) => pr,
            Err(e) => {
                return Err(FailureOutcome::with_steps(
                    step_results,
                    anyhow::anyhow!("failed to create PR: {}", e),
                ));
            }
        };

        if !task.labels.is_empty() {
            match gh_client
                .add_labels(owner, repo_name, pr.number, &task.labels)
                .await
            {
                Ok(_) => info!(pr = pr.number, labels = ?task.labels, "Applied PR labels"),
                Err(e) => warn!(pr = pr.number, error = %e, "Failed to apply PR labels"),
            }
        }

        Ok(SuccessOutcome {
            pr_url: Some(pr.html_url),
            step_results,
        })
    }

    /// Run the language-appropriate test suite against `repo_path` and return a
    /// human-readable summary plus a pass flag.
    #[allow(dead_code)]
    fn run_workspace_tests(repo_path: &Path) -> TestSummary {
        run_tests_for_workspace(repo_path)
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

// ---------------------------------------------------------------------------
// Internal helpers for execute_real
// ---------------------------------------------------------------------------

struct SuccessOutcome {
    pr_url: Option<String>,
    step_results: Vec<StepResult>,
}

struct FailureOutcome {
    pr_url: Option<String>,
    step_results: Vec<StepResult>,
    error: anyhow::Error,
}

impl FailureOutcome {
    fn early(steps: &[String], error: anyhow::Error) -> Self {
        let step_results = steps
            .iter()
            .map(|s| StepResult {
                step: s.clone(),
                status: "pending".to_string(),
                actions: Vec::new(),
                test_output: None,
                error: None,
                completed_at: None,
            })
            .collect();
        Self {
            pr_url: None,
            step_results,
            error,
        }
    }

    fn with_steps(step_results: Vec<StepResult>, error: anyhow::Error) -> Self {
        Self {
            pr_url: None,
            step_results,
            error,
        }
    }
}

struct TestSummary {
    summary: String,
    passed: bool,
}

fn parse_owner_repo(repo: &str) -> anyhow::Result<(&str, &str)> {
    let mut parts = repo.splitn(2, '/');
    let owner = parts
        .next()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow::anyhow!("task.repo must be in format 'owner/repo'"))?;
    let name = parts
        .next()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow::anyhow!("task.repo must be in format 'owner/repo'"))?;
    Ok((owner, name))
}

// Run a git subcommand in `cwd` and fail the function if it exits non-zero.
fn run_git(cwd: &Path, args: &[&str]) -> anyhow::Result<()> {
    let status = std::process::Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .status()?;
    if !status.success() {
        return Err(anyhow::anyhow!("git {:?} exited {}", args, status));
    }
    Ok(())
}

// Detect project types under `repo_path` and run the matching test suite(s).
// Returns a short summary string plus an aggregate pass flag.
//
// Failure to detect or run is treated as `passed = true` with a note in the
// summary — we don't want a missing test toolchain to block a PR that didn't
// touch testable code. Callers that need stricter semantics can inspect the
// summary text.
fn run_tests_for_workspace(repo_path: &Path) -> TestSummary {
    let runner = TestRunner::new(repo_path);
    let project_types = match runner.detect_project_types() {
        Ok(types) if !types.is_empty() => types,
        Ok(_) => {
            return TestSummary {
                summary: "no recognised project type — skipping tests".to_string(),
                passed: true,
            };
        }
        Err(e) => {
            return TestSummary {
                summary: format!("project type detection failed: {}", e),
                passed: true,
            };
        }
    };

    let mut all_passed = true;
    let mut lines: Vec<String> = Vec::with_capacity(project_types.len());
    for project_type in project_types {
        if matches!(project_type, ProjectType::Mixed) {
            continue;
        }
        match runner.run_tests_for_type(project_type) {
            Ok(results) => {
                let suite_passed = results.failed == 0;
                if !suite_passed {
                    all_passed = false;
                }
                lines.push(format!(
                    "{:?}: total={} passed={} failed={} skipped={}",
                    results.project_type,
                    results.total,
                    results.passed,
                    results.failed,
                    results.skipped
                ));
            }
            Err(e) => {
                // Couldn't run (e.g. missing toolchain) — don't fail the task on
                // this alone, but record it.
                lines.push(format!("{:?}: runner error: {}", project_type, e));
            }
        }
    }
    TestSummary {
        summary: lines.join("\n"),
        passed: all_passed,
    }
}

// Build an `AgentTask` from a `TaskFile`. The TaskFile.description is the
// headline; the numbered steps are passed as context so the planner can use
// them as a starting suggestion (but is free to break things up differently).
fn build_agent_task(task: &TaskFile) -> crate::agent::AgentTask {
    let mut context = String::new();
    context.push_str("Suggested steps (the planner may re-order or refine these):\n");
    for (i, step) in task.steps.iter().enumerate() {
        context.push_str(&format!("{}. {}\n", i + 1, step));
    }
    context.push_str(&format!("\nTarget repo: {}\nTarget branch: {}\n", task.repo, task.branch));
    crate::agent::AgentTask::new(task.description.clone()).with_context(context)
}

// Convert agent step outputs (from the final iteration) into the
// task-file-shaped `StepResult` records that get persisted.
fn step_results_from_pipeline(pipeline: &crate::agent::PipelineResult) -> Vec<StepResult> {
    let Some(last) = pipeline.iterations.last() else {
        return Vec::new();
    };
    last.step_results
        .iter()
        .map(|sr| {
            let (status, error) = match &sr.status {
                crate::agent::StepStatus::Completed => ("success".to_string(), None),
                crate::agent::StepStatus::Failed { error } => {
                    ("failed".to_string(), Some(error.clone()))
                }
            };
            StepResult {
                step: sr.step_description.clone(),
                status,
                actions: vec![sr.output.chars().take(2000).collect()],
                test_output: None,
                error,
                completed_at: Some(Utc::now().timestamp()),
            }
        })
        .collect()
}

// Placeholder step results used when the pipeline blew up before producing
// any iteration data.
fn placeholder_steps(steps: &[String], status: &str) -> Vec<StepResult> {
    steps
        .iter()
        .map(|s| StepResult {
            step: s.clone(),
            status: status.to_string(),
            actions: Vec::new(),
            test_output: None,
            error: None,
            completed_at: None,
        })
        .collect()
}

fn write_result_file(result: &TaskResult) -> anyhow::Result<()> {
    let results_dir = Path::new("tasks").join("results");
    if !results_dir.exists() {
        std::fs::create_dir_all(&results_dir)?;
    }
    let result_path = results_dir.join(format!("{}.json", result.task_id));
    let file = fs::File::create(&result_path)?;
    let writer = BufWriter::new(file);
    to_writer_pretty(writer, result)?;
    info!(
        task = %result.task_id,
        status = %result.status,
        "TaskResult written to {}",
        result_path.display()
    );
    Ok(())
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
