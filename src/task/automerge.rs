// Background poller that watches a PR's combined CI status after the agent
// opens it, then either merges it (if `task.auto_merge` and CI passes) or
// tags it `needs-review` (if CI fails) — finishing TASK-D and the
// failure-handling half of TASK-F.
//
// The watcher path / SSE endpoint kicks this off as a `tokio::spawn` after
// PR creation so the foreground call returns the `TaskResult` immediately.
// When the poller eventually settles it rewrites
// `tasks/results/{id}.json` in place, setting `merge_state`.

use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use serde_json::to_writer_pretty;
use std::fs;
use std::io::BufWriter;
use std::path::Path;
use tracing::{debug, info, warn};

use crate::github::GitHubClient;
use crate::task::{MergeState, TaskResult};

/// Tunables for the auto-merge poller. Defaults are conservative:
/// 15-second poll interval, 10-minute total timeout, squash merges,
/// `needs-review` failure label.
#[derive(Debug, Clone)]
pub struct AutoMergeConfig {
    /// How long to wait between successive CI status polls.
    pub poll_interval: Duration,
    /// Total wall-clock budget. Past this point we record `Timeout`
    /// and leave the PR open.
    pub timeout: Duration,
    /// Passed to `GitHubClient::merge_pull_request` once CI is green.
    /// One of `"merge"`, `"squash"`, `"rebase"`.
    pub merge_method: String,
    /// Label applied to the PR when CI fails. Picked up by humans /
    /// triage workflows.
    pub failure_label: String,
}

impl AutoMergeConfig {
    /// Read overrides from environment:
    ///   - `RC_AUTOMERGE_POLL_SECS`         (default 15)
    ///   - `RC_AUTOMERGE_TIMEOUT_SECS`      (default 600)
    ///   - `RC_AUTOMERGE_METHOD`            (default "squash")
    ///   - `RC_AUTOMERGE_FAILURE_LABEL`     (default "needs-review")
    #[must_use]
    pub fn from_env() -> Self {
        let default = Self::default();
        Self {
            poll_interval: std::env::var("RC_AUTOMERGE_POLL_SECS")
                .ok()
                .and_then(|s| s.parse().ok())
                .map(Duration::from_secs)
                .unwrap_or(default.poll_interval),
            timeout: std::env::var("RC_AUTOMERGE_TIMEOUT_SECS")
                .ok()
                .and_then(|s| s.parse().ok())
                .map(Duration::from_secs)
                .unwrap_or(default.timeout),
            merge_method: std::env::var("RC_AUTOMERGE_METHOD").unwrap_or(default.merge_method),
            failure_label: std::env::var("RC_AUTOMERGE_FAILURE_LABEL")
                .unwrap_or(default.failure_label),
        }
    }
}

impl Default for AutoMergeConfig {
    fn default() -> Self {
        Self {
            poll_interval: Duration::from_secs(15),
            timeout: Duration::from_secs(600),
            merge_method: "squash".to_string(),
            failure_label: "needs-review".to_string(),
        }
    }
}

/// Poll the PR's combined status, then merge / tag accordingly.
/// Returns the terminal `MergeState`; the caller is responsible for
/// persisting it (see `update_result_with_merge_state`).
pub async fn poll_and_merge(
    gh: GitHubClient,
    owner: String,
    repo: String,
    pr_number: i32,
    config: AutoMergeConfig,
) -> MergeState {
    let started = Instant::now();
    let mut ticker = tokio::time::interval(config.poll_interval);
    // Burn the first immediate tick — CI almost never reports back
    // within milliseconds of PR open.
    ticker.tick().await;

    loop {
        ticker.tick().await;

        let elapsed = started.elapsed();
        if elapsed > config.timeout {
            warn!(
                pr = pr_number,
                waited_secs = elapsed.as_secs(),
                "auto-merge: CI did not settle within timeout"
            );
            return MergeState::Timeout {
                waited_secs: elapsed.as_secs(),
            };
        }

        // Re-fetch the PR so we always look at the *latest* head SHA —
        // the user may have pushed more commits between iterations.
        let pr = match gh.get_pull_request(&owner, &repo, pr_number).await {
            Ok(pr) => pr,
            Err(e) => {
                warn!(error = %e, pr = pr_number, "auto-merge: get_pull_request failed");
                continue;
            }
        };
        let sha = &pr.head.sha;

        let status = match gh.get_commit_combined_status(&owner, &repo, sha).await {
            Ok(s) => s,
            Err(e) => {
                warn!(error = %e, sha = %sha, "auto-merge: combined status fetch failed");
                continue;
            }
        };
        let state = status
            .get("state")
            .and_then(|v| v.as_str())
            .unwrap_or("pending");
        debug!(pr = pr_number, sha = %sha, state, "auto-merge: poll");

        match state {
            "success" => {
                info!(pr = pr_number, "auto-merge: CI green — merging");
                return match gh
                    .merge_pull_request(&owner, &repo, pr_number, &config.merge_method)
                    .await
                {
                    Ok(_) => MergeState::Merged {
                        merge_method: config.merge_method,
                    },
                    Err(e) => {
                        warn!(error = %e, pr = pr_number, "auto-merge: merge API call failed");
                        MergeState::MergeFailed {
                            error: e.to_string(),
                        }
                    }
                };
            }
            "failure" | "error" => {
                warn!(
                    pr = pr_number,
                    state, "auto-merge: CI failed — tagging needs-review"
                );
                if let Err(e) = gh
                    .add_labels(
                        &owner,
                        &repo,
                        pr_number,
                        std::slice::from_ref(&config.failure_label),
                    )
                    .await
                {
                    warn!(error = %e, pr = pr_number, "auto-merge: failed to apply needs-review label");
                }
                return MergeState::NeedsReview {
                    reason: format!("CI state was {}", state),
                };
            }
            _ => {
                // "pending" or unknown — keep polling.
                continue;
            }
        }
    }
}

/// Rewrite `tasks/results/{task_id}.json` with the auto-merge outcome
/// patched into `merge_state`. All other fields are preserved verbatim.
///
/// Used by the background poller after `poll_and_merge` settles. We
/// re-read the file (rather than threading the in-memory `TaskResult`)
/// because the watcher already wrote it before spawning the poller and
/// other code paths (MEM-C consolidation, future post-run hooks) may
/// have touched it too.
pub fn update_result_with_merge_state(task_id: &str, state: MergeState) -> Result<()> {
    let path = Path::new("tasks")
        .join("results")
        .join(format!("{}.json", task_id));
    let content = fs::read_to_string(&path)
        .with_context(|| format!("read result file {}", path.display()))?;
    let mut result: TaskResult = serde_json::from_str(&content).context("parse result file")?;
    result.merge_state = Some(state);

    let tmp_path = path.with_extension("json.tmp");
    {
        let file = fs::File::create(&tmp_path)
            .with_context(|| format!("create temp file {}", tmp_path.display()))?;
        let writer = BufWriter::new(file);
        to_writer_pretty(writer, &result).context("write result")?;
    }
    fs::rename(&tmp_path, &path)
        .with_context(|| format!("rename {} -> {}", tmp_path.display(), path.display()))?;
    info!(task = %task_id, "auto-merge: result file updated");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_matches_documented_values() {
        let c = AutoMergeConfig::default();
        assert_eq!(c.poll_interval, Duration::from_secs(15));
        assert_eq!(c.timeout, Duration::from_secs(600));
        assert_eq!(c.merge_method, "squash");
        assert_eq!(c.failure_label, "needs-review");
    }

    #[test]
    fn merge_state_serializes_with_kind_tag() {
        let json = serde_json::to_string(&MergeState::Merged {
            merge_method: "squash".to_string(),
        })
        .unwrap();
        assert!(json.contains("\"kind\":\"merged\""));
        assert!(json.contains("\"merge_method\":\"squash\""));

        let json = serde_json::to_string(&MergeState::NeedsReview {
            reason: "CI state was failure".to_string(),
        })
        .unwrap();
        assert!(json.contains("\"kind\":\"needs_review\""));
    }

    #[test]
    fn merge_state_round_trips() {
        let original = MergeState::Timeout { waited_secs: 600 };
        let json = serde_json::to_string(&original).unwrap();
        let back: MergeState = serde_json::from_str(&json).unwrap();
        match back {
            MergeState::Timeout { waited_secs } => assert_eq!(waited_secs, 600),
            _ => panic!("wrong variant after round-trip"),
        }
    }

    #[test]
    fn update_result_with_merge_state_patches_only_merge_state() {
        use crate::task::StepResult;
        let dir = tempfile::tempdir().unwrap();
        let results = dir.path().join("tasks").join("results");
        std::fs::create_dir_all(&results).unwrap();

        let task_id = "automerge-test-1";
        let path = results.join(format!("{}.json", task_id));

        let original = TaskResult {
            task_id: task_id.to_string(),
            status: "success".to_string(),
            pr_url: Some("https://example.test/pr/1".to_string()),
            branch: "feat/x".to_string(),
            step_results: vec![StepResult {
                step: "do thing".to_string(),
                status: "success".to_string(),
                actions: vec!["wrote file".to_string()],
                test_output: Some("ok".to_string()),
                error: None,
                completed_at: Some(0),
            }],
            error: None,
            started_at: 1,
            completed_at: 2,
            duration_secs: 1,
            agent_trace: None,
            auto_merge_requested: true,
            merge_state: None,
        };
        let file = std::fs::File::create(&path).unwrap();
        to_writer_pretty(std::io::BufWriter::new(file), &original).unwrap();

        // We can't change the working directory of the test runner —
        // run the helper with `cwd = dir` by chdir'ing for the duration
        // of this test only.
        let prev_cwd = std::env::current_dir().unwrap();
        std::env::set_current_dir(dir.path()).unwrap();
        let result = update_result_with_merge_state(
            task_id,
            MergeState::Merged {
                merge_method: "squash".to_string(),
            },
        );
        let _ = std::env::set_current_dir(prev_cwd);
        result.expect("update should succeed");

        let updated_raw = std::fs::read_to_string(&path).unwrap();
        let updated: TaskResult = serde_json::from_str(&updated_raw).unwrap();
        assert_eq!(updated.task_id, task_id);
        assert_eq!(updated.pr_url, original.pr_url);
        assert_eq!(updated.step_results.len(), 1);
        assert!(updated.auto_merge_requested);
        assert!(matches!(
            updated.merge_state,
            Some(MergeState::Merged { .. })
        ));
    }
}
