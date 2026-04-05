// Automatic Repository Scanner
//
// Provides background scanning of enabled repositories at configurable intervals.
// Monitors git status and automatically re-analyzes changed files.
//
// ## Static Pre-Filter Integration (2026-02-08)
//
// Before sending any file to the LLM, the scanner runs a zero-cost static
// analysis pass via [`StaticAnalyzer`]. Based on the recommendation:
//
// - **Skip**: Generated code, trivial files, or provably clean files are skipped entirely.
// - **Minimal**: Small clean files use a cheaper prompt (fewer response tokens).
// - **Standard**: Normal analysis path.
// - **DeepDive**: Files with red flags (unsafe without SAFETY, high unwrap density,
//   potential secrets) get the full deep-analysis prompt.
//
// This reduces LLM spend by 30–50% based on observed scan data where 66% of files
// returned zero issues from the LLM.

use anyhow::{Context, Result};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;
use tracing::{debug, error, info, warn};

use crate::cost_tracker::{CostTracker, StaticDecisionRecord};
use crate::db::scan_events;
use crate::db::{Database, Repository};
use crate::prompt_router::{PromptRouter, TierKind};
use crate::refactor_assistant::RefactorAssistant;
use crate::repo_cache_sql::RepoCacheSql;
use crate::repo_manager::RepoManager;
use crate::static_analysis::{AnalysisRecommendation, StaticAnalyzer};
use crate::todo_scanner::TodoScanner;

// Maximum file size to send to LLM analysis (100 KB)
const MAX_ANALYSIS_FILE_SIZE: u64 = 100 * 1024;

// Default per-scan cost budget in dollars
const DEFAULT_SCAN_COST_BUDGET: f64 = 3.00;

// Grok 4.1 Fast pricing constants (mirrors grok_client.rs)
const COST_PER_MILLION_INPUT: f64 = 0.20;
const COST_PER_MILLION_OUTPUT: f64 = 0.50;

// Directories to always skip during scanning
const SKIP_DIRS: &[&str] = &[
    "/dist/",
    "/build/",
    "/node_modules/",
    "/target/",
    "/.git/",
    "/vendor/",
    "/__pycache__/",
    "/.next/",
    "/out/",
    "/coverage/",
    "/.cache/",
];

// File patterns to always skip (suffix match)
const SKIP_SUFFIXES: &[&str] = &[
    ".min.js",
    ".min.css",
    ".map",
    ".bundle.js",
    ".chunk.js",
    ".min.mjs",
    ".d.ts",
    ".lock",
];

// Auto-scanner configuration
#[derive(Debug, Clone)]
pub struct AutoScannerConfig {
    // Global enable/disable
    pub enabled: bool,
    // Default scan interval in minutes
    pub default_interval_minutes: u64,
    // Maximum concurrent scans
    pub max_concurrent_scans: usize,
    // Per-scan cost budget in dollars (0.0 = unlimited)
    pub scan_cost_budget: f64,
}

impl Default for AutoScannerConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            default_interval_minutes: 60,
            max_concurrent_scans: 2,
            scan_cost_budget: DEFAULT_SCAN_COST_BUDGET,
        }
    }
}

// Git status for a file
#[derive(Debug, Clone, PartialEq)]
pub enum FileStatus {
    Unmodified,
    Modified,
    Added,
    Deleted,
    Renamed,
    Untracked,
}

// Result of analyzing a single file
struct FileAnalysisResult {
    issues_found: i64,
    cost_usd: f64,
    #[allow(dead_code)]
    tokens_used: Option<usize>,
    was_cache_hit: bool,
}

// Repository scan state
#[derive(Debug, Clone)]
pub struct RepoScanState {
    pub repo_id: String,
    pub repo_path: PathBuf,
    pub last_scan: Option<i64>,
    pub last_git_hash: Option<String>,
    pub modified_files: Vec<PathBuf>,
}

// Background repository scanner
pub struct AutoScanner {
    config: AutoScannerConfig,
    pool: sqlx::PgPool,
    repos_dir: PathBuf,
    scan_states: Arc<RwLock<HashMap<String, RepoScanState>>>,
    repo_manager: Arc<RepoManager>,
    // Static analyzer for pre-filtering files before LLM analysis
    static_analyzer: Arc<StaticAnalyzer>,
    // Prompt router for tier-based prompt selection (Minimal/Standard/DeepDive)
    prompt_router: Arc<PromptRouter>,
    // TodoScanner for richer TODO/FIXME priority classification
    todo_scanner: Arc<TodoScanner>,
    // Cost tracker for logging static analysis decisions and savings
    cost_tracker: Option<Arc<CostTracker>>,
}

impl AutoScanner {
    // Create a new auto-scanner
    pub fn new(config: AutoScannerConfig, pool: sqlx::PgPool, repos_dir: PathBuf) -> Self {
        // Get GitHub token from environment for private repos
        let github_token = std::env::var("GITHUB_TOKEN").ok();

        let repo_manager = Arc::new(
            RepoManager::new(&repos_dir, github_token).expect("Failed to create RepoManager"),
        );

        let static_analyzer = Arc::new(StaticAnalyzer::new());
        let prompt_router = Arc::new(PromptRouter::new());
        let todo_scanner = Arc::new(TodoScanner::new().expect("Failed to create TodoScanner"));

        Self {
            config,
            pool,
            repos_dir,
            scan_states: Arc::new(RwLock::new(HashMap::new())),
            repo_manager,
            static_analyzer,
            prompt_router,
            todo_scanner,
            cost_tracker: None,
        }
    }

    // Attach a cost tracker for savings reporting.
    // When set, every file decision (skip/minimal/standard/deep) is logged.
    pub fn with_cost_tracker(mut self, tracker: Arc<CostTracker>) -> Self {
        self.cost_tracker = Some(tracker);
        self
    }

    // Start the background scanner
    pub async fn start(self: Arc<Self>) -> Result<()> {
        if !self.config.enabled {
            info!("Auto-scanner is disabled");
            return Ok(());
        }

        info!(
            "Starting auto-scanner with {} minute intervals",
            self.config.default_interval_minutes
        );

        // Main scan loop
        loop {
            if let Err(e) = self.scan_enabled_repos().await {
                error!("Error during scan cycle: {}", e);
            }

            // Sleep for 1 minute, then check which repos need scanning
            tokio::time::sleep(Duration::from_secs(60)).await;
        }
    }

    // Scan all enabled repositories
    async fn scan_enabled_repos(&self) -> Result<()> {
        let repos = self.get_enabled_repos().await?;

        if repos.is_empty() {
            debug!("No enabled repositories to scan");
            return Ok(());
        }

        info!("Checking {} enabled repositories", repos.len());

        // Process repos in parallel (limited concurrency)
        let semaphore = Arc::new(tokio::sync::Semaphore::new(
            self.config.max_concurrent_scans,
        ));
        let mut tasks = vec![];

        for repo in repos {
            let self_clone = Arc::new(self.clone_scanner());
            let semaphore_clone = semaphore.clone();

            let task = tokio::spawn(async move {
                let _permit = semaphore_clone.acquire().await.ok();
                if let Err(e) = self_clone.check_and_scan_repo(&repo).await {
                    error!("Failed to scan repo {}: {}", repo.name, e);
                }
            });

            tasks.push(task);
        }

        // Wait for all scans to complete
        for task in tasks {
            let _ = task.await;
        }

        Ok(())
    }

    // Get all repositories with auto_scan_enabled = 1
    async fn get_enabled_repos(&self) -> Result<Vec<Repository>> {
        let repos = sqlx::query_as::<_, Repository>(
            r#"
            SELECT *
            FROM repositories
            WHERE auto_scan = 1
            "#,
        )
        .fetch_all(&self.pool)
        .await?;

        Ok(repos)
    }

    // Check if repo needs scanning and scan if necessary
    async fn check_and_scan_repo(&self, repo: &Repository) -> Result<()> {
        let repo_name = &repo.name;
        let now = chrono::Utc::now().timestamp();
        let interval_secs = repo.scan_interval_minutes as i64 * 60;

        // ── On-demand project review (bypasses interval check) ──────────
        // The API sets review_requested = 1 when a client sends a
        // "📋 Re-run Review" request.  We handle it here so it fires on
        // the next 60-second loop iteration regardless of scan_interval_mins.
        let review_requested = repo.review_requested.unwrap_or(false);
        if review_requested {
            info!(
                "📋 API-requested project review for {} — bypassing interval check",
                repo.name
            );

            // Clear the flag immediately so we don't re-fire
            sqlx::query("UPDATE repositories SET review_requested = 0 WHERE id = $1")
                .bind(&repo.id)
                .execute(&self.pool)
                .await
                .ok();

            // Resolve repo path — try the stored path first, then the
            // repos_dir clone location.
            let primary = PathBuf::from(&repo.path);
            let alt = self.repos_dir.join(&repo.name);
            let resolved = if primary.exists() {
                Some(primary.clone())
            } else if alt.exists() {
                Some(alt.clone())
            } else {
                None
            };

            match resolved {
                Some(repo_path) => {
                    match self
                        .generate_project_review(&repo.id, &repo.name, &repo_path)
                        .await
                    {
                        Ok(task_count) => {
                            info!(
                                "📋 Requested review complete for {}: {} tasks generated",
                                repo.name, task_count
                            );
                            let _ = scan_events::log_info(
                                &self.pool,
                                Some(&repo.id),
                                "project_review_complete",
                                &format!("On-demand review generated {} tasks", task_count),
                            )
                            .await;
                        }
                        Err(e) => {
                            error!("Requested project review failed for {}: {}", repo.name, e);
                            let _ = scan_events::log_error(
                                &self.pool,
                                Some(&repo.id),
                                "project_review_error",
                                "On-demand project review failed",
                                &e.to_string(),
                            )
                            .await;
                        }
                    }
                }
                None => {
                    warn!(
                        "Cannot run review for {} — repo path not found at {} or {}",
                        repo.name,
                        primary.display(),
                        alt.display()
                    );
                }
            }

            // Update scan check time so we don't immediately re-scan
            self.update_last_scan_check(&repo.id, now).await?;
            return Ok(());
        }

        // Check if enough time has passed since last scan
        if let Some(last_check) = repo.last_scan_check {
            if now - last_check < interval_secs {
                debug!(
                    "Skipping {} - scanned {} seconds ago",
                    repo.name,
                    now - last_check
                );
                return Ok(());
            }
        }

        info!("Scanning repository: {} ({})", repo.name, repo.path);

        // Track scan start time for duration calculation
        let scan_start = std::time::Instant::now();

        // Log scan start event
        if let Err(e) = scan_events::log_info(
            &self.pool,
            Some(&repo.id),
            "scan_start",
            &format!("Starting scan of {}", repo.name),
        )
        .await
        {
            warn!("Failed to log scan start event: {}", e);
        }

        // Ensure the repo exists locally — clone from git_url if missing
        let repo_path = PathBuf::from(&repo.path);
        let repo_path = if !repo_path.exists() || !repo_path.join(".git").exists() {
            if let Some(ref git_url) = repo.git_url {
                info!(
                    "Local path {} not found, cloning from {}",
                    repo_path.display(),
                    git_url
                );
                match self.clone_or_update_repo(git_url, &repo.name) {
                    Ok(cloned_path) => {
                        // Update the stored path in the database to the new clone location
                        let new_path = cloned_path.to_string_lossy().to_string();
                        if let Err(e) = self.update_repo_path(&repo.id, &new_path).await {
                            error!("Failed to update repo path in DB: {}", e);
                        }
                        info!("Cloned {} to {}", repo.name, cloned_path.display());

                        // Log clone event
                        if let Err(e) = scan_events::log_info(
                            &self.pool,
                            Some(&repo.id),
                            "repo_cloned",
                            &format!("Cloned repository to {}", cloned_path.display()),
                        )
                        .await
                        {
                            warn!("Failed to log clone event: {}", e);
                        }

                        cloned_path
                    }
                    Err(e) => {
                        error!("Failed to clone {} from {}: {}", repo.name, git_url, e);

                        // Log clone error event
                        if let Err(err) = scan_events::log_error(
                            &self.pool,
                            Some(&repo.id),
                            "clone_error",
                            &format!("Failed to clone {}", repo.name),
                            &e.to_string(),
                        )
                        .await
                        {
                            warn!("Failed to log clone error event: {}", err);
                        }

                        return Ok(());
                    }
                }
            } else {
                debug!(
                    "Repo {} path {} does not exist and no git_url configured — skipping",
                    repo.name,
                    repo_path.display()
                );
                return Ok(());
            }
        } else {
            repo_path
        };

        // Update repository if it exists (git pull)
        if let Some(ref git_url) = repo.git_url {
            match self.clone_or_update_repo(git_url, &repo.name) {
                Ok(_) => {
                    // Log successful update
                    if let Err(e) = scan_events::log_info(
                        &self.pool,
                        Some(&repo.id),
                        "git_update",
                        &format!("Updated repository {}", repo.name),
                    )
                    .await
                    {
                        warn!("Failed to log git update event: {}", e);
                    }
                }
                Err(e) => {
                    warn!("Failed to update {}: {}", repo.name, e);
                }
            }
        }

        // Check for changes (both committed and uncommitted)
        let current_head = self.get_head_hash(&repo_path)?;
        let changed_files = self
            .get_changed_files(
                &repo_path,
                repo.last_commit_hash.as_deref(),
                current_head.as_deref(),
            )
            .await?;

        if changed_files.is_empty() {
            debug!("No changes detected in {}", repo.name);
            // Still update the commit hash so we don't re-diff the same range
            if let Some(ref hash) = current_head {
                self.update_last_commit_hash(&repo.id, hash).await?;
            }
            // Update last_scan_check for interval tracking
            self.update_last_scan_check(&repo.id, now).await?;
            return Ok(());
        }

        info!(
            "Found {} changed files in {}",
            changed_files.len(),
            repo.name
        );

        // Start progress tracking
        let total_files = changed_files.len() as i64;
        if let Err(e) = crate::db::core::start_scan(&self.pool, &repo.id, total_files).await {
            error!("Failed to start scan progress tracking: {}", e);
        }

        // Mark scan start with timestamp for ETA calculation and reset enhanced columns
        sqlx::query(
            "UPDATE repositories SET scan_started_at = $1, scan_cost_accumulated = 0.0, scan_cache_hits = 0, scan_api_calls = 0 WHERE id = $2"
        )
        .bind(chrono::Utc::now().timestamp())
        .bind(&repo.id)
        .execute(&self.pool)
        .await
        .ok();

        // Log scan progress event
        if let Err(e) =
            scan_events::mark_scan_started(&self.pool, &repo.id, total_files as i32).await
        {
            warn!("Failed to mark scan as started: {}", e);
        }

        // Analyze changed files with progress tracking
        let result = self
            .analyze_changed_files_with_progress(&repo.id, repo_name, &repo_path, &changed_files)
            .await;

        match result {
            Ok((files_analyzed, issues_found, budget_halted)) => {
                // Calculate scan duration
                let duration_ms = scan_start.elapsed().as_millis() as i64;

                // Complete scan with metrics
                if let Err(e) = crate::db::core::complete_scan(
                    &self.pool,
                    &repo.id,
                    duration_ms,
                    files_analyzed,
                    issues_found,
                )
                .await
                {
                    error!("Failed to complete scan progress tracking: {}", e);
                }

                // Log scan completion event
                if let Err(e) = scan_events::mark_scan_complete(
                    &self.pool,
                    &repo.id,
                    files_analyzed as i32,
                    issues_found as i32,
                    duration_ms,
                )
                .await
                {
                    warn!("Failed to mark scan as complete: {}", e);
                }

                info!(
                    "Scan completed for {}: {} files, {} issues in {}ms",
                    repo.name, files_analyzed, issues_found, duration_ms
                );

                // Update last_analyzed timestamp
                self.update_last_analyzed(&repo.id, now).await?;

                // CRITICAL: Only store the commit hash if ALL files were analyzed.
                // If the budget cap halted the scan, we leave the hash unstored so
                // the next scan cycle will re-diff, hit cache on already-analyzed
                // files (free), and continue analyzing remaining files.
                if !budget_halted {
                    // === Phase C: Final Project Review ===
                    // All files analyzed — run a project-wide review to synthesize
                    // individual analyses into a prioritized, grouped task list.
                    info!(
                        "📊 All {} files analyzed for {}. Starting final project review...",
                        files_analyzed, repo.name
                    );

                    match self
                        .generate_project_review(&repo.id, &repo.name, &repo_path)
                        .await
                    {
                        Ok(task_count) => {
                            info!(
                                "📋 Final review complete for {}: {} tasks generated → queue",
                                repo.name, task_count
                            );

                            // Log review event
                            if let Err(e) = scan_events::log_info(
                                &self.pool,
                                Some(&repo.id),
                                "project_review_complete",
                                &format!(
                                    "Project review generated {} tasks from {} file analyses",
                                    task_count, files_analyzed
                                ),
                            )
                            .await
                            {
                                warn!("Failed to log review event: {}", e);
                            }
                        }
                        Err(e) => {
                            error!("Final project review failed for {}: {}", repo.name, e);

                            if let Err(err) = scan_events::log_error(
                                &self.pool,
                                Some(&repo.id),
                                "project_review_error",
                                "Final project review failed",
                                &e.to_string(),
                            )
                            .await
                            {
                                warn!("Failed to log review error event: {}", err);
                            }
                            // Non-fatal — scan data is still valid, just no review tasks
                        }
                    }

                    if let Some(ref hash) = current_head {
                        self.update_last_commit_hash(&repo.id, hash).await?;
                    }
                } else {
                    info!(
                        "Skipping commit hash update — budget halted scan. \
                         Next cycle will resume from cache hits."
                    );
                }
            }
            Err(e) => {
                error!("Scan failed for {}: {}", repo.name, e);
                if let Err(err) =
                    crate::db::core::fail_scan(&self.pool, &repo.id, &e.to_string()).await
                {
                    error!("Failed to mark scan as failed: {}", err);
                }

                // Log scan error event
                if let Err(err) =
                    scan_events::mark_scan_error(&self.pool, &repo.id, &e.to_string()).await
                {
                    warn!("Failed to log scan error: {}", err);
                }

                return Err(e);
            }
        }

        Ok(())
    }

    // Clone or update a repository from a git URL into the repos directory
    fn clone_or_update_repo(&self, git_url: &str, name: &str) -> Result<PathBuf> {
        self.repo_manager
            .clone_or_update(git_url, name)
            .context(format!(
                "Failed to clone or update {} from {}",
                name, git_url
            ))
    }

    // Update the stored path for a repository in the database
    async fn update_repo_path(&self, repo_id: &str, new_path: &str) -> Result<()> {
        sqlx::query(
            r#"
            UPDATE repositories
            SET local_path = $1, updated_at = $2
            WHERE id = $3
            "#,
        )
        .bind(new_path)
        .bind(chrono::Utc::now().timestamp())
        .bind(repo_id)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    // Get the current HEAD commit hash for a repository
    fn get_head_hash(&self, repo_path: &Path) -> Result<Option<String>> {
        use std::process::Command;

        let output = Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(repo_path)
            .output()
            .context("Failed to run git rev-parse HEAD")?;

        if !output.status.success() {
            warn!("git rev-parse HEAD failed for {}", repo_path.display());
            return Ok(None);
        }

        let hash = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if hash.is_empty() {
            Ok(None)
        } else {
            Ok(Some(hash))
        }
    }

    // Get list of modified files from both committed and uncommitted changes
    async fn get_changed_files(
        &self,
        repo_path: &Path,
        last_commit_hash: Option<&str>,
        current_head: Option<&str>,
    ) -> Result<Vec<PathBuf>> {
        use std::collections::HashSet;
        use std::process::Command;

        let mut changed_set: HashSet<PathBuf> = HashSet::new();

        // 1. Check for committed changes since last known hash
        if let (Some(old_hash), Some(new_hash)) = (last_commit_hash, current_head) {
            if old_hash != new_hash {
                let output = Command::new("git")
                    .args(["diff", "--name-status", old_hash, new_hash])
                    .current_dir(repo_path)
                    .output();

                match output {
                    Ok(out) if out.status.success() => {
                        let stdout = String::from_utf8_lossy(&out.stdout);
                        for line in stdout.lines() {
                            let parts: Vec<&str> = line.split('\t').collect();
                            if parts.len() < 2 {
                                continue;
                            }
                            let status = parts[0];
                            // Skip deleted files
                            if status.starts_with('D') {
                                continue;
                            }
                            // For renames (R100), the new path is the last element
                            let file_path = parts.last().unwrap().trim();
                            if Self::should_analyze_file(file_path) {
                                let full_path = repo_path.join(file_path);
                                if full_path.exists() {
                                    changed_set.insert(full_path);
                                } else {
                                    debug!(
                                        "Skipping {} - file does not exist on disk (deleted in later commit$1)",
                                        file_path
                                    );
                                }
                            }
                        }
                        info!(
                            "Found {} files changed between commits {}..{}",
                            changed_set.len(),
                            &old_hash[..8.min(old_hash.len())],
                            &new_hash[..8.min(new_hash.len())]
                        );
                    }
                    Ok(out) => {
                        // git diff failed - old hash may no longer exist (force push, etc.)
                        // Fall back to listing all files in the latest commit
                        warn!(
                            "git diff failed for {}..{} ({}), falling back to HEAD diff",
                            &old_hash[..8.min(old_hash.len())],
                            &new_hash[..8.min(new_hash.len())],
                            String::from_utf8_lossy(&out.stderr).trim()
                        );
                        self.get_files_from_recent_commits(repo_path, &mut changed_set)?;
                    }
                    Err(e) => {
                        warn!("Failed to run git diff: {}", e);
                    }
                }
            }
        } else if last_commit_hash.is_none() && current_head.is_some() {
            // First scan - no stored hash yet. Check recent commits to seed initial analysis.
            info!(
                "First scan for {} - checking recent commits",
                repo_path.display()
            );
            self.get_files_from_recent_commits(repo_path, &mut changed_set)?;
        }

        // 2. Also check for uncommitted changes (working tree + staged)
        let output = Command::new("git")
            .args(["status", "--porcelain"])
            .current_dir(repo_path)
            .output()
            .context("Failed to run git status")?;

        if output.status.success() {
            let stdout = String::from_utf8_lossy(&output.stdout);
            for line in stdout.lines() {
                if line.len() < 3 {
                    continue;
                }

                let status = &line[0..2];
                let file_path = line[3..].trim();

                // Skip deleted files
                if status.contains('D') {
                    continue;
                }

                if Self::should_analyze_file(file_path) {
                    let full_path = repo_path.join(file_path);
                    if full_path.exists() {
                        changed_set.insert(full_path);
                    } else {
                        debug!("Skipping {} - file does not exist on disk", file_path);
                    }
                }
            }
        }

        Ok(changed_set.into_iter().collect())
    }

    // Get changed files from recent commits (used for first scan or fallback)
    fn get_files_from_recent_commits(
        &self,
        repo_path: &Path,
        changed_set: &mut std::collections::HashSet<PathBuf>,
    ) -> Result<()> {
        use std::process::Command;

        // Try to get files changed in the last 5 commits first.
        // This may fail for repos that have fewer than 5 commits (e.g. HEAD~5
        // doesn't exist), so we fall back to listing every tracked file in HEAD.
        let diff_output = Command::new("git")
            .args(["diff", "--name-only", "HEAD~5", "HEAD"])
            .current_dir(repo_path)
            .output();

        let used_diff = match diff_output {
            Ok(ref out) if out.status.success() => {
                let stdout = String::from_utf8_lossy(&out.stdout);
                let mut found = false;
                for line in stdout.lines() {
                    let file_path = line.trim();
                    if !file_path.is_empty() && Self::should_analyze_file(file_path) {
                        let full_path = repo_path.join(file_path);
                        if full_path.exists() {
                            changed_set.insert(full_path);
                            found = true;
                        } else {
                            debug!("Skipping {} - file does not exist on disk", file_path);
                        }
                    }
                }
                found
            }
            _ => {
                debug!(
                    "Could not get recent commits for {} (too few commits or git error) — \
                     falling back to full tree listing",
                    repo_path.display()
                );
                false
            }
        };

        // Fallback: list every file tracked in HEAD so a brand-new or shallow
        // clone still gets a full initial scan instead of being silently skipped.
        if !used_diff {
            info!(
                "First-scan fallback: listing all tracked files in HEAD for {}",
                repo_path.display()
            );
            let ls_output = Command::new("git")
                .args(["ls-tree", "-r", "--name-only", "HEAD"])
                .current_dir(repo_path)
                .output();

            match ls_output {
                Ok(out) if out.status.success() => {
                    let stdout = String::from_utf8_lossy(&out.stdout);
                    for line in stdout.lines() {
                        let file_path = line.trim();
                        if !file_path.is_empty() && Self::should_analyze_file(file_path) {
                            let full_path = repo_path.join(file_path);
                            if full_path.exists() {
                                changed_set.insert(full_path);
                            } else {
                                debug!("Skipping {} - file does not exist on disk", file_path);
                            }
                        }
                    }
                    info!(
                        "ls-tree listed {} analyzable files for {}",
                        changed_set.len(),
                        repo_path.display()
                    );
                }
                Ok(out) => {
                    warn!(
                        "git ls-tree failed for {}: {}",
                        repo_path.display(),
                        String::from_utf8_lossy(&out.stderr).trim()
                    );
                }
                Err(e) => {
                    warn!(
                        "Failed to run git ls-tree for {}: {}",
                        repo_path.display(),
                        e
                    );
                }
            }
        }

        Ok(())
    }

    // Check if a file extension is one we should analyze
    fn is_analyzable_file(file_path: &str) -> bool {
        file_path.ends_with(".rs")
            || file_path.ends_with(".py")
            || file_path.ends_with(".js")
            || file_path.ends_with(".ts")
            || file_path.ends_with(".tsx")
            || file_path.ends_with(".sh")
            || file_path.ends_with(".kt")
            || file_path.ends_with(".java")
            || file_path.ends_with(".go")
            || file_path.ends_with(".rb")
    }

    // Check if a file should be skipped based on path patterns.
    // This catches generated/bundled/vendored code that wastes API budget.
    fn should_skip_path(file_path: &str) -> bool {
        // Normalize to forward slashes for consistent matching
        let normalized = file_path.replace('\\', "/");
        // Ensure we match directory components properly by wrapping in slashes
        let with_leading = if normalized.starts_with('/') {
            normalized.clone()
        } else {
            format!("/{}", normalized)
        };

        // Check directory patterns
        for dir in SKIP_DIRS {
            if with_leading.contains(dir) {
                return true;
            }
        }

        // Check suffix patterns (minified files, sourcemaps, etc.)
        for suffix in SKIP_SUFFIXES {
            if normalized.ends_with(suffix) {
                return true;
            }
        }

        false
    }

    // Combined filter: is it a code file AND not in a skip path?
    fn should_analyze_file(file_path: &str) -> bool {
        Self::is_analyzable_file(file_path) && !Self::should_skip_path(file_path)
    }

    // Analyze changed files with progress tracking and cost budget enforcement.
    // Returns (files_analyzed, issues_found)
    async fn analyze_changed_files_with_progress(
        &self,
        repo_id: &str,
        repo_name: &str,
        repo_path: &Path,
        files: &[PathBuf],
    ) -> Result<(i64, i64, bool)> {
        // Compute and store cache hash in DB if not already set
        let cache_hash = RepoCacheSql::compute_repo_hash(repo_path);
        sqlx::query("UPDATE repositories SET cache_hash = $1 WHERE id = $2 AND cache_hash IS NULL")
            .bind(&cache_hash)
            .bind(repo_id)
            .execute(&self.pool)
            .await
            .ok();

        let cache = RepoCacheSql::new_for_repo(repo_path).await?;
        let mut files_analyzed = 0i64;
        let mut issues_found = 0i64;
        let mut cumulative_cost = 0.0f64;
        let mut cache_hits = 0i64;
        let mut api_calls = 0i64;
        let mut budget_halted = false;

        // Pre-filter files that match skip patterns (extra safety — get_changed_files
        // already filters, but files may have been added to the list via other paths)
        let analyzable_files: Vec<&PathBuf> = files
            .iter()
            .filter(|f| {
                let path_str = f.to_string_lossy();
                if Self::should_skip_path(&path_str) {
                    let rel = f.strip_prefix(repo_path).unwrap_or(f);
                    info!(
                        "Pre-filter: skipping {} — matches skip pattern",
                        rel.display()
                    );
                    false
                } else {
                    true
                }
            })
            .collect();

        let original_count = files.len();
        let filtered_count = analyzable_files.len();
        if original_count != filtered_count {
            info!(
                "Filtered {} → {} files ({} skipped by path/pattern rules)",
                original_count,
                filtered_count,
                original_count - filtered_count
            );
        }

        // --- Checkpoint resume: check for existing checkpoint ---
        let checkpoint = self.load_scan_checkpoint(repo_id, filtered_count).await;
        let start_index = if let Some(ref cp) = checkpoint {
            info!(
                "📍 Resuming scan from checkpoint: [{}/{}] (${:.4} spent, {} cached so far)",
                cp.last_completed_index + 1,
                filtered_count,
                cp.cumulative_cost,
                cp.files_cached,
            );
            cumulative_cost = cp.cumulative_cost;
            files_analyzed = cp.files_analyzed;
            cache_hits = cp.files_cached;
            cp.last_completed_index + 1
        } else {
            0
        };

        info!(
            "🔍 Starting scan: {} files to analyze (starting at index {})",
            filtered_count, start_index
        );

        for (idx, file) in analyzable_files.iter().enumerate() {
            // Skip files before checkpoint
            if idx < start_index {
                continue;
            }

            // Check cost budget before each file (using actual accumulated cost)
            if self.config.scan_cost_budget > 0.0 && cumulative_cost >= self.config.scan_cost_budget
            {
                warn!(
                    "[{}/{}] ⚠️  Scan cost budget reached (${:.4} >= ${:.2} limit). \
                     Stopping analysis with {} files remaining.",
                    idx + 1,
                    filtered_count,
                    cumulative_cost,
                    self.config.scan_cost_budget,
                    filtered_count - idx
                );
                budget_halted = true;
                break;
            }

            let rel_path = file
                .strip_prefix(repo_path)
                .unwrap_or(file)
                .to_string_lossy()
                .to_string();

            match self
                .analyze_file(
                    repo_id,
                    repo_name,
                    repo_path,
                    file,
                    &cache,
                    idx,
                    filtered_count,
                )
                .await
            {
                Ok(file_result) => {
                    files_analyzed += 1;
                    issues_found += file_result.issues_found;
                    cumulative_cost += file_result.cost_usd;
                    if file_result.was_cache_hit {
                        cache_hits += 1;
                    } else {
                        api_calls += 1;
                    }

                    // Log cost milestone every $0.50
                    if cumulative_cost > 0.0
                        && (cumulative_cost * 2.0) as i64
                            > ((cumulative_cost - file_result.cost_usd) * 2.0) as i64
                    {
                        info!(
                            "💰 Scan cost milestone: ${:.4} / ${:.2} budget ({} files analyzed)",
                            cumulative_cost, self.config.scan_cost_budget, files_analyzed
                        );
                    }

                    // Persist checkpoint after every successful file
                    if let Err(e) = self
                        .save_scan_checkpoint(
                            repo_id,
                            idx,
                            &rel_path,
                            files_analyzed,
                            cache_hits,
                            cumulative_cost,
                            filtered_count,
                        )
                        .await
                    {
                        warn!("Failed to save scan checkpoint: {}", e);
                    }

                    // Update DB progress on every file for the HTMX live progress bar
                    sqlx::query(
                        "UPDATE repositories SET
                            scan_files_processed = ?,
                            scan_current_file = ?,
                            scan_cost_accumulated = ?,
                            scan_cache_hits = ?,
                            scan_api_calls = ?
                        WHERE id = ?",
                    )
                    .bind((idx + 1) as i64)
                    .bind(&rel_path)
                    .bind(cumulative_cost)
                    .bind(cache_hits)
                    .bind(api_calls)
                    .bind(repo_id)
                    .execute(&self.pool)
                    .await
                    .ok();
                }
                Err(e) => {
                    error!(
                        "[{}/{}] ❌ Failed to analyze {}: {}",
                        idx + 1,
                        filtered_count,
                        file.display(),
                        e
                    );
                }
            }
        }

        info!(
            "📊 Scan summary: analyzed={}, cache_hits={}, issues={}, actual_cost=${:.4}, budget_halted={}",
            files_analyzed, cache_hits, issues_found, cumulative_cost, budget_halted
        );

        // Clear checkpoint on successful completion (not budget halt)
        if !budget_halted {
            if let Err(e) = self.clear_scan_checkpoint(repo_id).await {
                warn!("Failed to clear scan checkpoint: {}", e);
            }
        }

        Ok((files_analyzed, issues_found, budget_halted))
    }

    // Create tasks from file analysis results if critical/high severity issues are found.
    // This provides incremental task creation during scans, not just at the final review.
    async fn create_tasks_from_file_analysis(
        &self,
        repo_id: &str,
        _repo_name: &str,
        file_path: &str,
        analysis: &crate::refactor_assistant::RefactoringAnalysis,
    ) -> Result<usize> {
        use crate::refactor_assistant::{RefactoringType, SmellSeverity};

        let mut task_count = 0;

        // Only create tasks for critical/high severity code smells to avoid noise
        for smell in &analysis.code_smells {
            if !matches!(
                smell.severity,
                SmellSeverity::Critical | SmellSeverity::High
            ) {
                continue;
            }

            let priority = match smell.severity {
                SmellSeverity::Critical => 1,
                SmellSeverity::High => 2,
                SmellSeverity::Medium => 3,
                SmellSeverity::Low => 4,
            };

            let line_number = smell
                .location
                .as_ref()
                .and_then(|loc| loc.line_start.map(|l| l as i32));

            let title = format!("{}: {}", smell.smell_type, file_path);
            let description = format!(
                "**Severity:** {:?}\n\n{}\n\n**File:** {}\n**Lines:** {}\n\n*Source: File scan analysis*",
                smell.severity,
                smell.description,
                file_path,
                line_number
                    .map(|l| l.to_string())
                    .unwrap_or_else(|| "unknown".to_string())
            );

            match crate::db::core::create_task(
                &self.pool,
                &title,
                Some(&description),
                priority,
                "file_scan",
                None,
                Some(repo_id),
                Some(file_path),
                line_number,
            )
            .await
            {
                Ok(_) => {
                    task_count += 1;
                    debug!("Created task for {} in {}", smell.smell_type, file_path);
                }
                Err(e) => {
                    warn!(
                        "Failed to create task for code smell in {}: {}",
                        file_path, e
                    );
                }
            }
        }

        // Also create tasks for high-impact refactoring suggestions
        for suggestion in &analysis.suggestions {
            // Only create tasks for certain high-value refactoring types
            let should_create_task = matches!(
                suggestion.refactoring_type,
                RefactoringType::ExtractFunction
                    | RefactoringType::ExtractModule
                    | RefactoringType::ImproveErrorHandling
                    | RefactoringType::ReduceCoupling
                    | RefactoringType::SplitFunction
            );

            if !should_create_task {
                continue;
            }

            let priority = 3; // Medium priority for refactoring suggestions
            let line_number: Option<i32> = None; // RefactoringSuggestion doesn't have location

            let title = format!("Refactor: {} in {}", suggestion.title, file_path);
            let description = format!(
                "**Type:** {:?}\n**Effort:** {:?}\n\n{}\n\n**File:** {}\n\n*Source: File scan analysis*",
                suggestion.refactoring_type, suggestion.effort, suggestion.description, file_path
            );

            match crate::db::core::create_task(
                &self.pool,
                &title,
                Some(&description),
                priority,
                "file_scan",
                None,
                Some(repo_id),
                Some(file_path),
                line_number,
            )
            .await
            {
                Ok(_) => {
                    task_count += 1;
                    debug!(
                        "Created refactoring task for {} in {}",
                        suggestion.title, file_path
                    );
                }
                Err(e) => {
                    warn!("Failed to create refactoring task in {}: {}", file_path, e);
                }
            }
        }

        Ok(task_count)
    }

    // Analyze a single file with progress-aware logging.
    // Returns `FileAnalysisResult` with issues, cost, tokens, and cache-hit flag.
    #[allow(clippy::too_many_arguments)]
    async fn analyze_file(
        &self,
        repo_id: &str,
        repo_name: &str,
        repo_path: &Path,
        file_path: &Path,
        cache: &RepoCacheSql,
        progress_idx: usize,
        progress_total: usize,
    ) -> Result<FileAnalysisResult> {
        let rel_path = file_path
            .strip_prefix(repo_path)
            .unwrap_or(file_path)
            .to_string_lossy()
            .to_string();

        let progress_tag = format!("[{}/{}]", progress_idx + 1, progress_total);

        // Skip non-existent files (deleted between diff and analysis)
        if !file_path.exists() {
            debug!(
                "{} ⏭️  Skipping {} — file no longer exists",
                progress_tag, rel_path
            );
            return Ok(FileAnalysisResult {
                issues_found: 0,
                cost_usd: 0.0,
                tokens_used: None,
                was_cache_hit: false,
            });
        }

        // Check file size before reading
        let metadata = tokio::fs::metadata(file_path).await?;
        let file_size = metadata.len();

        if file_size > MAX_ANALYSIS_FILE_SIZE {
            info!(
                "{} ⏭️  Skipping {} — too large ({} KB > {} KB limit)",
                progress_tag,
                rel_path,
                file_size / 1024,
                MAX_ANALYSIS_FILE_SIZE / 1024
            );
            return Ok(FileAnalysisResult {
                issues_found: 0,
                cost_usd: 0.0,
                tokens_used: None,
                was_cache_hit: false,
            });
        }

        if file_size == 0 {
            debug!("{} ⏭️  Skipping {} — empty file", progress_tag, rel_path);
            return Ok(FileAnalysisResult {
                issues_found: 0,
                cost_usd: 0.0,
                tokens_used: None,
                was_cache_hit: false,
            });
        }

        // Read file content
        let content = match tokio::fs::read_to_string(file_path).await {
            Ok(c) => c,
            Err(e) => {
                warn!(
                    "{} ⏭️  Cannot read {} (possibly binary): {}",
                    progress_tag, rel_path, e
                );
                return Ok(FileAnalysisResult {
                    issues_found: 0,
                    cost_usd: 0.0,
                    tokens_used: None,
                    was_cache_hit: false,
                });
            }
        };

        // Skip if content is suspiciously dense (likely minified/bundled).
        // Heuristic: if average line length > 500 chars and fewer than 50 lines,
        // it's almost certainly generated or minified code.
        let line_count = content.lines().count().max(1);
        let avg_line_len = content.len() / line_count;
        if avg_line_len > 500 && line_count < 50 {
            info!(
                "{} ⏭️  Skipping {} — likely minified (avg line: {} chars, {} lines)",
                progress_tag, rel_path, avg_line_len, line_count
            );
            return Ok(FileAnalysisResult {
                issues_found: 0,
                cost_usd: 0.0,
                tokens_used: None,
                was_cache_hit: false,
            });
        }

        // ====================================================================
        // STATIC PRE-FILTER: Run zero-cost analysis before touching the LLM
        // Uses TodoScanner integration for richer priority classification
        // ====================================================================
        let static_result =
            self.static_analyzer
                .analyze_with_todos(&rel_path, &content, &self.todo_scanner);

        // Determine prompt tier for non-skip files
        let prompt_tier = self
            .prompt_router
            .route(&rel_path, &content, &static_result);
        let tier_kind = prompt_tier.tier;

        // Estimate what an LLM call would cost for this file (for savings tracking)
        let estimated_file_cost = CostTracker::estimate_file_cost(content.len());

        match static_result.recommendation {
            AnalysisRecommendation::Skip => {
                let reason = static_result
                    .skip_reason
                    .as_ref()
                    .map(|r| r.to_string())
                    .unwrap_or_else(|| "static filter".to_string());
                info!(
                    "{} 🚫 SKIP   {} — {} (saved LLM call ~${:.4}, static issues: {})",
                    progress_tag,
                    rel_path,
                    reason,
                    estimated_file_cost,
                    static_result.static_issue_count
                );

                // Log the savings decision
                if let Some(ref tracker) = self.cost_tracker {
                    let _ = tracker
                        .log_static_decision(&StaticDecisionRecord {
                            file_path: rel_path.clone(),
                            repo_id: repo_id.to_string(),
                            recommendation: "SKIP".to_string(),
                            skip_reason: static_result.skip_reason.as_ref().map(|r| r.to_string()),
                            static_issue_count: static_result.static_issue_count as i64,
                            estimated_llm_value: static_result.estimated_llm_value,
                            llm_called: false,
                            estimated_cost_saved_usd: estimated_file_cost,
                            actual_cost_usd: 0.0,
                            prompt_tier: None,
                        })
                        .await;
                }

                return Ok(FileAnalysisResult {
                    issues_found: static_result.static_issue_count as i64,
                    cost_usd: 0.0,
                    tokens_used: None,
                    was_cache_hit: false,
                });
            }
            AnalysisRecommendation::Minimal => {
                debug!(
                    "{} 🔹 MINIMAL {} — {} tier (value: {:.2}, est. tokens: {})",
                    progress_tag,
                    rel_path,
                    tier_kind,
                    static_result.estimated_llm_value,
                    prompt_tier.estimated_input_tokens
                );
            }
            AnalysisRecommendation::DeepDive => {
                info!(
                    "{} 🔴 DEEP   {} — {} tier (static issues: {}, value: {:.2}, est. tokens: {})",
                    progress_tag,
                    rel_path,
                    tier_kind,
                    static_result.static_issue_count,
                    static_result.estimated_llm_value,
                    prompt_tier.estimated_input_tokens
                );
            }
            AnalysisRecommendation::Standard => {
                debug!(
                    "{} 🔵 STD    {} — {} tier (value: {:.2}, est. tokens: {})",
                    progress_tag,
                    rel_path,
                    tier_kind,
                    static_result.estimated_llm_value,
                    prompt_tier.estimated_input_tokens
                );
            }
        }

        // Check cache first
        if cache
            .get(
                crate::repo_cache::CacheType::Refactor,
                &rel_path,
                &content,
                "xai",
                "grok-beta",
                None,
                None,
            )
            .await?
            .is_some()
        {
            debug!("{} 📦 CACHE  {}", progress_tag, rel_path);
            return Ok(FileAnalysisResult {
                issues_found: 0,
                cost_usd: 0.0,
                tokens_used: None,
                was_cache_hit: true,
            });
        }

        info!(
            "{} 🔍 API    Analyzing {} (tier: {}, prompt: {})",
            progress_tag, rel_path, static_result.recommendation, tier_kind
        );

        // Create RefactorAssistant for analysis
        let db = Database::from_pool(self.pool.clone());
        let assistant = RefactorAssistant::new(db).await?;

        // Analyze with LLM
        let analysis = assistant.analyze_file(file_path).await?;

        // Calculate actual cost from API-reported tokens_used
        // Uses Grok 4.1 Fast pricing with ~70% input / 30% output split
        // (observed from actual API logs)
        let actual_cost = if let Some(tokens) = analysis.tokens_used {
            let t = tokens as f64;
            let input_est = t * 0.7;
            let output_est = t * 0.3;
            (input_est / 1_000_000.0) * COST_PER_MILLION_INPUT
                + (output_est / 1_000_000.0) * COST_PER_MILLION_OUTPUT
        } else {
            0.0
        };

        let issues_count = analysis.code_smells.len() as i64 + analysis.suggestions.len() as i64;

        // Cache the result
        let result_json = serde_json::to_value(&analysis)?;
        cache
            .set(crate::repo_cache_sql::CacheSetParams {
                cache_type: crate::repo_cache::CacheType::Refactor,
                repo_path: &repo_path.to_string_lossy(),
                file_path: &rel_path,
                content: &content,
                provider: "xai",
                model: "grok-beta",
                result: result_json,
                tokens_used: analysis.tokens_used,
                prompt_hash: None,
                schema_version: None,
            })
            .await?;

        info!(
            "{} ✅ Cached {} (cost: ${:.4}, tokens: {}, issues: {}, tier: {})",
            progress_tag,
            rel_path,
            actual_cost,
            analysis.tokens_used.unwrap_or(0),
            issues_count,
            tier_kind,
        );

        // Create tasks immediately for critical/high severity issues
        if issues_count > 0 {
            match self
                .create_tasks_from_file_analysis(repo_id, repo_name, &rel_path, &analysis)
                .await
            {
                Ok(tasks_created) => {
                    if tasks_created > 0 {
                        info!(
                            "{} 📋 Created {} task(s) for issues in {}",
                            progress_tag, tasks_created, rel_path
                        );
                    }
                }
                Err(e) => {
                    warn!(
                        "{} Failed to create tasks for {}: {}",
                        progress_tag, rel_path, e
                    );
                }
            }
        }

        // Log the LLM decision with actual cost for savings tracking
        if let Some(ref tracker) = self.cost_tracker {
            // Calculate savings vs what a standard prompt would have cost
            let savings = if tier_kind == TierKind::Minimal {
                // Minimal tier saves tokens vs Standard
                (estimated_file_cost - actual_cost).max(0.0)
            } else {
                0.0
            };

            let _ = tracker
                .log_static_decision(&StaticDecisionRecord {
                    file_path: rel_path.clone(),
                    repo_id: repo_id.to_string(),
                    recommendation: static_result.recommendation.to_string(),
                    skip_reason: None,
                    static_issue_count: static_result.static_issue_count as i64,
                    estimated_llm_value: static_result.estimated_llm_value,
                    llm_called: true,
                    estimated_cost_saved_usd: savings,
                    actual_cost_usd: actual_cost,
                    prompt_tier: Some(tier_kind.to_string()),
                })
                .await;
        }

        Ok(FileAnalysisResult {
            issues_found: issues_count,
            cost_usd: actual_cost,
            tokens_used: analysis.tokens_used,
            was_cache_hit: false,
        })
    }

    // Update last_scan_check timestamp
    async fn update_last_scan_check(&self, repo_id: &str, timestamp: i64) -> Result<()> {
        sqlx::query(
            r#"
            UPDATE repositories
            SET last_scanned_at = $1
            WHERE id = $2
            "#,
        )
        .bind(timestamp)
        .bind(repo_id)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    // Update last_analyzed timestamp
    async fn update_last_analyzed(&self, repo_id: &str, timestamp: i64) -> Result<()> {
        sqlx::query(
            r#"
            UPDATE repositories
            SET last_scanned_at = $1
            WHERE id = $2
            "#,
        )
        .bind(timestamp)
        .bind(repo_id)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    // Update last_commit_hash for a repository
    async fn update_last_commit_hash(&self, repo_id: &str, hash: &str) -> Result<()> {
        sqlx::query(
            r#"
            UPDATE repositories
            SET last_commit_hash = $1
            WHERE id = $2
            "#,
        )
        .bind(hash)
        .bind(repo_id)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    // Clone scanner for async tasks
    fn clone_scanner(&self) -> Self {
        Self {
            config: self.config.clone(),
            pool: self.pool.clone(),
            repos_dir: self.repos_dir.clone(),
            scan_states: self.scan_states.clone(),
            repo_manager: self.repo_manager.clone(),
            static_analyzer: self.static_analyzer.clone(),
            prompt_router: self.prompt_router.clone(),
            todo_scanner: self.todo_scanner.clone(),
            cost_tracker: self.cost_tracker.clone(),
        }
    }

    // ========================================================================
    // Final Project Review
    // ========================================================================

    // After all files have been individually analyzed, collect all cached
    // analyses and send them as one context to Grok to generate a prioritized,
    // grouped task list for the queue. Returns the number of tasks created.
    async fn generate_project_review(
        &self,
        repo_id: &str,
        repo_name: &str,
        repo_path: &Path,
    ) -> Result<usize> {
        let cache = RepoCacheSql::new_for_repo(repo_path).await?;
        let all_entries = cache.get_all_entries().await?;

        if all_entries.is_empty() {
            info!("No cached analyses found for project review — skipping");
            return Ok(0);
        }

        // Build a condensed project summary from all cached analyses
        let mut project_context = String::new();
        let mut total_issues = 0usize;
        let mut files_with_issues = 0usize;

        for entry in &all_entries {
            if entry.cache_type != "refactor" {
                continue;
            }

            if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&entry.result_json) {
                let smells = parsed["code_smells"]
                    .as_array()
                    .map(|a| a.len())
                    .unwrap_or(0);
                let suggestions = parsed["suggestions"]
                    .as_array()
                    .map(|a| a.len())
                    .unwrap_or(0);
                let complexity = parsed["complexity_score"].as_f64().unwrap_or(50.0);

                total_issues += smells + suggestions;

                // Only include files with issues or high complexity in the review prompt
                // to stay within context limits
                if smells > 0 || suggestions > 0 || complexity > 70.0 {
                    files_with_issues += 1;

                    // Truncate per-file analysis to keep total context manageable
                    let analysis_text = &entry.result_json;
                    let truncated_boundary = if analysis_text.len() > 2000 {
                        let mut b = 2000;
                        while b > 0 && !analysis_text.is_char_boundary(b) {
                            b -= 1;
                        }
                        b
                    } else {
                        analysis_text.len()
                    };
                    let truncated = &analysis_text[..truncated_boundary];

                    project_context.push_str(&format!(
                        "\n## {}\n- Complexity: {:.0}\n- Issues: {}\n- Analysis: {}\n",
                        entry.file_path,
                        complexity,
                        smells + suggestions,
                        truncated
                    ));
                }
            }
        }

        info!(
            "📊 Project review context: {} total files, {} with issues, {} total issues",
            all_entries.len(),
            files_with_issues,
            total_issues
        );

        if files_with_issues == 0 {
            info!("No files with issues found — skipping project review");
            return Ok(0);
        }

        // Build the final review prompt
        let prompt = format!(
            r#"You are reviewing a complete codebase analysis for the "{repo_name}" project.

{file_count} files were analyzed. {issue_count} total issues were found across {issue_files} files.

Below is a summary of every file that had issues. Your job is to:

1. Identify CROSS-CUTTING CONCERNS — patterns that appear across multiple files
   (e.g., "error handling is inconsistent across 12 service files")
2. Identify DEPENDENCY CHAINS — where fixing file A should happen before file B
3. Group related issues into ACTIONABLE TASKS that can each be completed in 1-4 hours
4. Prioritize by: Critical (security/crashes) > High (correctness) > Medium (quality) > Low (style)
5. For each task, specify:
   - Title (clear, actionable)
   - Description (what to do, not what's wrong)
   - Files affected (list)
   - Priority (critical/high/medium/low)
   - Estimated effort (small/medium/large)
   - Dependencies (which task titles must complete first)
   - Category

Respond in ONLY valid JSON (no markdown fences):
{{
  "summary": "Brief overview of project health",
  "cross_cutting_concerns": ["..."],
  "tasks": [
    {{
      "title": "...",
      "description": "...",
      "files": ["..."],
      "priority": "critical|high|medium|low",
      "effort": "small|medium|large",
      "dependencies": [],
      "category": "security|error-handling|performance|testing|refactoring|documentation"
    }}
  ]
}}

=== FILE ANALYSES ===
{project_context}"#,
            repo_name = repo_name,
            file_count = all_entries.len(),
            issue_count = total_issues,
            issue_files = files_with_issues,
            project_context = project_context
        );

        // Call Grok with the full project context
        let db = Database::from_pool(self.pool.clone());
        let grok = crate::grok_client::GrokClient::from_env(db).await?;

        let tracked = grok
            .ask_tracked(&prompt, None, "project_review")
            .await
            .context("Failed to generate project review")?;

        info!(
            "📊 Project review API call: {} tokens, ${:.4}",
            tracked.total_tokens, tracked.cost_usd
        );

        // Parse the response and insert tasks into the queue
        match self
            .parse_review_into_tasks(&tracked.content, repo_id, repo_name)
            .await
        {
            Ok(count) => Ok(count),
            Err(first_err) => {
                warn!(
                    "Project review parse failed on full context ({} files with issues). \
                     Retrying with reduced batch...",
                    files_with_issues
                );

                // Retry strategy: rebuild the prompt with only the top ~30 files
                // (sorted by issue count descending) to produce a shorter, more
                // reliable JSON response.
                let retry_result = self
                    .retry_project_review_with_reduced_context(
                        repo_id,
                        repo_name,
                        &all_entries,
                        &grok,
                    )
                    .await;

                match retry_result {
                    Ok(count) => {
                        info!(
                            "✅ Retry succeeded: {} tasks generated from reduced context",
                            count
                        );
                        Ok(count)
                    }
                    Err(retry_err) => {
                        // Both attempts failed — return the original error with context
                        Err(first_err.context(format!(
                            "Retry with reduced context also failed: {}",
                            retry_err
                        )))
                    }
                }
            }
        }
    }

    // Retry the project review with a reduced set of files (top 30 by issue count).
    // Called when the full-context review produces unparseable JSON.
    async fn retry_project_review_with_reduced_context(
        &self,
        repo_id: &str,
        repo_name: &str,
        all_entries: &[crate::repo_cache_sql::CacheEntry],
        grok: &crate::grok_client::GrokClient,
    ) -> Result<usize> {
        // Collect files with issues, sorted by issue count descending
        let mut files_with_issues: Vec<(&str, usize, f64, &str)> = Vec::new();

        for entry in all_entries {
            if entry.cache_type != "refactor" {
                continue;
            }
            if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&entry.result_json) {
                let smells = parsed["code_smells"]
                    .as_array()
                    .map(|a| a.len())
                    .unwrap_or(0);
                let suggestions = parsed["suggestions"]
                    .as_array()
                    .map(|a| a.len())
                    .unwrap_or(0);
                let complexity = parsed["complexity_score"].as_f64().unwrap_or(50.0);
                let issues = smells + suggestions;

                if issues > 0 || complexity > 70.0 {
                    files_with_issues.push((
                        &entry.file_path,
                        issues,
                        complexity,
                        &entry.result_json,
                    ));
                }
            }
        }

        // Sort by issue count descending, take top 30
        files_with_issues.sort_by(|a, b| b.1.cmp(&a.1));
        let batch_size = 30;
        let batch: Vec<_> = files_with_issues.into_iter().take(batch_size).collect();

        if batch.is_empty() {
            return Ok(0);
        }

        let total_issues: usize = batch.iter().map(|(_, count, _, _)| count).sum();
        let mut project_context = String::new();
        for (path, issues, complexity, analysis_json) in &batch {
            let truncated_boundary = if analysis_json.len() > 2000 {
                let mut b = 2000;
                while b > 0 && !analysis_json.is_char_boundary(b) {
                    b -= 1;
                }
                b
            } else {
                analysis_json.len()
            };
            let truncated = &analysis_json[..truncated_boundary];
            project_context.push_str(&format!(
                "\n## {}\n- Complexity: {:.0}\n- Issues: {}\n- Analysis: {}\n",
                path, complexity, issues, truncated
            ));
        }

        info!(
            "📊 Retry review with top {} files ({} issues)",
            batch.len(),
            total_issues
        );

        let prompt = format!(
            r#"You are reviewing a codebase analysis for the "{repo_name}" project.

This is a focused review of the {batch_len} highest-priority files ({total_issues} total issues).

Group related issues into ACTIONABLE TASKS (1-4 hours each).
Prioritize: Critical (security/crashes) > High (correctness) > Medium (quality) > Low (style).

IMPORTANT: Respond with ONLY valid JSON. No markdown fences, no explanation text.
The response must be a single JSON object with this exact structure:
{{
  "summary": "Brief overview",
  "cross_cutting_concerns": ["..."],
  "tasks": [
    {{
      "title": "...",
      "description": "...",
      "files": ["..."],
      "priority": "critical|high|medium|low",
      "effort": "small|medium|large",
      "dependencies": [],
      "category": "security|error-handling|performance|testing|refactoring|documentation"
    }}
  ]
}}

=== FILE ANALYSES ===
{project_context}"#,
            repo_name = repo_name,
            batch_len = batch.len(),
            total_issues = total_issues,
            project_context = project_context,
        );

        let tracked = grok
            .ask_tracked(&prompt, None, "project_review_retry")
            .await
            .context("Failed to generate project review (retry)")?;

        info!(
            "📊 Retry review API call: {} tokens, ${:.4}",
            tracked.total_tokens, tracked.cost_usd
        );

        self.parse_review_into_tasks(&tracked.content, repo_id, repo_name)
            .await
    }

    // Parse the Grok project review JSON response and insert tasks into the DB queue.
    // Returns the number of tasks inserted.
    async fn parse_review_into_tasks(
        &self,
        response: &str,
        repo_id: &str,
        repo_name: &str,
    ) -> Result<usize> {
        // Try to extract JSON from response (may be wrapped in markdown fences)
        let json_str = Self::extract_json_from_response(response);

        // Debug logging: show the edges of the extracted JSON so we can diagnose parse failures
        let preview_len = 500;
        debug!(
            "JSON extract preview — first {}: {}",
            preview_len,
            &json_str[..json_str.len().min(preview_len)]
        );
        debug!(
            "JSON extract preview — last {}: {}",
            preview_len,
            &json_str[json_str.len().saturating_sub(preview_len)..]
        );
        debug!("JSON extract total length: {} chars", json_str.len());

        // First attempt: parse directly
        let json: serde_json::Value = match serde_json::from_str(json_str) {
            Ok(v) => v,
            Err(parse_err) => {
                warn!(
                    "Initial JSON parse failed (line {}, col {}): {}",
                    parse_err.line(),
                    parse_err.column(),
                    parse_err
                );
                // Log more context around the error position for diagnostics
                let err_offset = json_str
                    .lines()
                    .take(parse_err.line().saturating_sub(1))
                    .map(|l| l.len() + 1)
                    .sum::<usize>()
                    + parse_err.column().saturating_sub(1);
                let ctx_start = err_offset.saturating_sub(200);
                let ctx_end = json_str.len().min(err_offset + 200);
                warn!(
                    "Context around parse error (offset ~{}):\n...{}...",
                    err_offset,
                    &json_str[ctx_start..ctx_end]
                );

                // Second attempt: try to repair truncated JSON
                info!("Attempting JSON truncation repair...");
                match Self::repair_truncated_json(json_str) {
                    Some(repaired) => {
                        info!(
                            "Repaired JSON: added {} chars of closing delimiters",
                            repaired.len() - json_str.len()
                        );
                        serde_json::from_str(&repaired).with_context(|| {
                            format!(
                                "Failed to parse project review response as JSON even after repair. \
                                 Original error: {} (line {}, col {}). Response length: {} chars",
                                parse_err, parse_err.line(), parse_err.column(), json_str.len()
                            )
                        })?
                    }
                    None => {
                        return Err(anyhow::anyhow!(
                            "Failed to parse project review response as JSON: {} \
                             (line {}, col {}). Response length: {} chars. \
                             Repair not possible.",
                            parse_err,
                            parse_err.line(),
                            parse_err.column(),
                            json_str.len()
                        ));
                    }
                }
            }
        };

        // Log the summary if present
        if let Some(summary) = json["summary"].as_str() {
            info!("📋 Project review summary: {}", summary);
        }

        // Log cross-cutting concerns
        if let Some(concerns) = json["cross_cutting_concerns"].as_array() {
            for concern in concerns {
                if let Some(c) = concern.as_str() {
                    info!("  🔄 Cross-cutting: {}", c);
                }
            }
        }

        let mut task_count = 0usize;

        if let Some(task_array) = json["tasks"].as_array() {
            for t in task_array {
                let title = t["title"].as_str().unwrap_or("Untitled review task");
                let description = t["description"].as_str().unwrap_or("");
                let priority_str = t["priority"].as_str().unwrap_or("medium");
                let category = t["category"].as_str().unwrap_or("refactoring");
                let effort = t["effort"].as_str().unwrap_or("medium");

                // Map priority string to numeric value
                let priority = match priority_str {
                    "critical" => 1,
                    "high" => 2,
                    "medium" => 3,
                    "low" => 4,
                    _ => 3,
                };

                // Build a rich description including metadata
                let files_list = t["files"]
                    .as_array()
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|f| f.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    })
                    .unwrap_or_default();

                let deps_list = t["dependencies"]
                    .as_array()
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|d| d.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    })
                    .unwrap_or_default();

                let full_description = format!(
                    "{}\n\n**Category:** {}\n**Effort:** {}\n**Files:** {}\n**Dependencies:** {}",
                    description,
                    category,
                    effort,
                    if files_list.is_empty() {
                        "N/A"
                    } else {
                        &files_list
                    },
                    if deps_list.is_empty() {
                        "None"
                    } else {
                        &deps_list
                    },
                );

                // Get first file path for the task
                let first_file = t["files"]
                    .as_array()
                    .and_then(|arr| arr.first())
                    .and_then(|f| f.as_str());

                // Insert into the task queue
                match crate::db::core::create_task(
                    &self.pool,
                    title,
                    Some(&full_description),
                    priority,
                    "project_review",
                    Some(repo_name),
                    Some(repo_id),
                    first_file,
                    None,
                )
                .await
                {
                    Ok(task) => {
                        info!(
                            "  📌 Task created: [{}] {} (priority: {})",
                            task.id, title, priority_str
                        );
                        task_count += 1;
                    }
                    Err(e) => {
                        warn!("Failed to create task '{}': {}", title, e);
                    }
                }
            }
        }

        info!(
            "📋 Inserted {} tasks into queue from project review of {}",
            task_count, repo_name
        );

        Ok(task_count)
    }

    // Extract JSON from a response that might be wrapped in markdown code fences.
    //
    // Handles: ```json fences, generic ``` fences (with or without closing fence
    // for truncated responses), preamble/postamble text, and raw JSON objects.
    fn extract_json_from_response(response: &str) -> &str {
        let trimmed = response.trim();

        // Try to find JSON block in ```json ... ``` fences
        if let Some(start) = trimmed.find("```json") {
            let json_start = start + 7; // skip ```json
            // Skip any trailing whitespace/newline after the language tag
            let json_start = trimmed[json_start..]
                .find(['{', '['])
                .map(|n| json_start + n)
                .unwrap_or(json_start);
            if let Some(end) = trimmed[json_start..].find("```") {
                return trimmed[json_start..json_start + end].trim();
            }
            // No closing fence — response was likely truncated.
            // Return everything from the JSON start to the end.
            debug!("Found opening ```json fence but no closing fence — response may be truncated");
            return trimmed[json_start..].trim();
        }

        // Try generic code fence
        if let Some(start) = trimmed.find("```") {
            let after_fence = start + 3;
            // Skip optional language identifier on the same line
            let json_start = trimmed[after_fence..]
                .find('\n')
                .map(|n| after_fence + n + 1)
                .unwrap_or(after_fence);
            if let Some(end) = trimmed[json_start..].find("```") {
                return trimmed[json_start..json_start + end].trim();
            }
            // No closing fence — truncated
            debug!("Found opening ``` fence but no closing fence — response may be truncated");
            return trimmed[json_start..].trim();
        }

        // Try to find raw JSON object
        if let Some(start) = trimmed.find('{') {
            // Use rfind for '}' but validate it's not inside trailing text after JSON.
            // For robustness: if there's a closing brace, use it; the JSON parser
            // will catch structural issues inside.
            if let Some(end) = trimmed.rfind('}') {
                if end > start {
                    return &trimmed[start..=end];
                }
            }
            // No closing brace — truncated response, return from '{' to end
            debug!("Found opening '{{' but no closing '}}' — response may be truncated");
            return &trimmed[start..];
        }

        trimmed
    }

    // Attempt to repair truncated JSON by closing unclosed braces, brackets, and strings.
    //
    // This handles the common case where Grok hits its output token limit mid-response,
    // leaving the JSON structurally incomplete. We walk the string tracking nesting depth
    // and append the necessary closing delimiters.
    fn repair_truncated_json(json_str: &str) -> Option<String> {
        // Quick sanity check: must start with '{' or '['
        let first_meaningful = json_str.trim_start().chars().next()?;
        if first_meaningful != '{' && first_meaningful != '[' {
            return None;
        }

        let mut stack: Vec<char> = Vec::new();
        let mut in_string = false;
        let mut escape_next = false;

        for ch in json_str.chars() {
            if escape_next {
                escape_next = false;
                continue;
            }
            if in_string {
                match ch {
                    '\\' => escape_next = true,
                    '"' => in_string = false,
                    _ => {}
                }
                continue;
            }
            match ch {
                '"' => in_string = true,
                '{' => stack.push('}'),
                '[' => stack.push(']'),
                '}' | ']' => {
                    // Pop matching delimiter; ignore mismatches (best-effort)
                    if let Some(&expected) = stack.last() {
                        if expected == ch {
                            stack.pop();
                        }
                    }
                }
                _ => {}
            }
        }

        if stack.is_empty() && !in_string {
            // JSON is already balanced — the parse error is something else
            return None;
        }

        let mut repaired = json_str.to_string();

        // If we were mid-string, close it
        if in_string {
            // Truncate back to last complete-looking field if possible,
            // otherwise just close the string
            repaired.push('"');
        }

        // Try to cleanly end the current value context.
        // If the last non-whitespace char suggests we're mid-value (e.g., after a ':'),
        // add a null placeholder.
        let last_significant = repaired.trim_end().chars().last().unwrap_or(' ');
        if last_significant == ':' || last_significant == ',' {
            repaired.push_str("null");
        }

        // Close all unclosed delimiters in reverse order
        for closer in stack.iter().rev() {
            repaired.push(*closer);
        }

        Some(repaired)
    }

    // ========================================================================
    // Scan Checkpoint Persistence
    // ========================================================================

    // Load the most recent scan checkpoint for a repo.
    // Returns `None` if no checkpoint exists or if the file count has changed
    // (indicating the file list was modified since the last run).
    async fn load_scan_checkpoint(
        &self,
        repo_id: &str,
        current_total_files: usize,
    ) -> Option<ScanCheckpoint> {
        let row = sqlx::query_as::<_, (i64, String, i64, i64, f64, i64, i64)>(
            r#"
            SELECT last_completed_index, last_completed_file, files_analyzed,
                   files_cached, cumulative_cost, total_files, updated_at
            FROM scan_checkpoints
            WHERE repo_id = $1
            "#,
        )
        .bind(repo_id)
        .fetch_optional(&self.pool)
        .await
        .ok()
        .flatten()?;

        let checkpoint = ScanCheckpoint {
            last_completed_index: row.0 as usize,
            last_completed_file: row.1,
            files_analyzed: row.2,
            files_cached: row.3,
            cumulative_cost: row.4,
            total_files: row.5 as usize,
        };

        // Only use the checkpoint if the file count matches
        if checkpoint.total_files == current_total_files {
            Some(checkpoint)
        } else {
            info!(
                "⚠️  File list changed since last checkpoint ({} -> {}), restarting scan",
                checkpoint.total_files, current_total_files
            );
            // Clear stale checkpoint
            let _ = self.clear_scan_checkpoint(repo_id).await;
            None
        }
    }

    // Persist a scan checkpoint after each successfully analyzed file.
    #[allow(clippy::too_many_arguments)]
    async fn save_scan_checkpoint(
        &self,
        repo_id: &str,
        last_completed_index: usize,
        last_completed_file: &str,
        files_analyzed: i64,
        files_cached: i64,
        cumulative_cost: f64,
        total_files: usize,
    ) -> Result<()> {
        let now = chrono::Utc::now().timestamp();

        sqlx::query(
            r#"
            INSERT INTO scan_checkpoints
                (repo_id, last_completed_index, last_completed_file,
                 files_analyzed, files_cached, cumulative_cost, total_files, updated_at)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
            ON CONFLICT (repo_id) DO UPDATE SET
                last_completed_index = EXCLUDED.last_completed_index,
                last_completed_file = EXCLUDED.last_completed_file,
                files_analyzed = EXCLUDED.files_analyzed,
                files_cached = EXCLUDED.files_cached,
                cumulative_cost = EXCLUDED.cumulative_cost,
                total_files = EXCLUDED.total_files,
                updated_at = EXCLUDED.updated_at
            "#,
        )
        .bind(repo_id)
        .bind(last_completed_index as i64)
        .bind(last_completed_file)
        .bind(files_analyzed)
        .bind(files_cached)
        .bind(cumulative_cost)
        .bind(total_files as i64)
        .bind(now)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    // Clear the scan checkpoint for a repo (called on successful completion).
    async fn clear_scan_checkpoint(&self, repo_id: &str) -> Result<()> {
        sqlx::query("DELETE FROM scan_checkpoints WHERE repo_id = $1")
            .bind(repo_id)
            .execute(&self.pool)
            .await?;

        debug!("Cleared scan checkpoint for repo {}", repo_id);
        Ok(())
    }
}

// Checkpoint data loaded from the database
struct ScanCheckpoint {
    last_completed_index: usize,
    #[allow(dead_code)]
    last_completed_file: String,
    files_analyzed: i64,
    files_cached: i64,
    cumulative_cost: f64,
    total_files: usize,
}

// Enable auto-scan for a repository
pub async fn enable_auto_scan(
    pool: &sqlx::PgPool,
    repo_id: &str,
    interval_minutes: Option<i64>,
) -> Result<()> {
    let interval = interval_minutes.unwrap_or(60);

    sqlx::query(
        r#"
        UPDATE repositories
        SET auto_scan = 1, scan_interval_mins = $1
        WHERE id = $2
        "#,
    )
    .bind(interval)
    .bind(repo_id)
    .execute(pool)
    .await?;

    info!(
        "Enabled auto-scan for repo {} (interval: {} minutes)",
        repo_id, interval
    );

    Ok(())
}

// Disable auto-scan for a repository
pub async fn disable_auto_scan(pool: &sqlx::PgPool, repo_id: &str) -> Result<()> {
    sqlx::query(
        r#"
        UPDATE repositories
        SET auto_scan = 0
        WHERE id = $1
        "#,
    )
    .bind(repo_id)
    .execute(pool)
    .await?;

    info!("Disabled auto-scan for repo {}", repo_id);

    Ok(())
}

// Force a full rescan for a repository (reset both timing AND commit hash)
pub async fn force_scan(pool: &sqlx::PgPool, repo_id: &str) -> Result<()> {
    sqlx::query(
        r#"
        UPDATE repositories
        SET last_scanned_at = NULL,
            last_commit_hash = NULL
        WHERE id = $1
        "#,
    )
    .bind(repo_id)
    .execute(pool)
    .await?;

    info!(
        "Forced full rescan for repo {} (cleared commit hash + scan time)",
        repo_id
    );

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = AutoScannerConfig::default();
        assert!(config.enabled);
        assert_eq!(config.default_interval_minutes, 60);
        assert_eq!(config.max_concurrent_scans, 2);
        assert!((config.scan_cost_budget - 3.00).abs() < f64::EPSILON);
    }

    #[test]
    fn test_file_status() {
        let status = FileStatus::Modified;
        assert_eq!(status, FileStatus::Modified);
        assert_ne!(status, FileStatus::Unmodified);
    }

    #[test]
    fn test_should_skip_path_skip_dirs() {
        assert!(AutoScanner::should_skip_path(
            "src/clients/web/dist/bundle.js"
        ));
        assert!(AutoScanner::should_skip_path("frontend/build/index.js"));
        assert!(AutoScanner::should_skip_path(
            "node_modules/lodash/index.js"
        ));
        assert!(AutoScanner::should_skip_path("target/debug/build/main.rs"));
        assert!(AutoScanner::should_skip_path("vendor/third_party/lib.go"));
        assert!(AutoScanner::should_skip_path("app/.next/server/pages.js"));
        assert!(AutoScanner::should_skip_path("project/__pycache__/mod.py"));
        assert!(AutoScanner::should_skip_path(".cache/some/file.js"));
    }

    #[test]
    fn test_should_skip_path_skip_suffixes() {
        assert!(AutoScanner::should_skip_path("src/app.min.js"));
        assert!(AutoScanner::should_skip_path("styles/main.min.css"));
        assert!(AutoScanner::should_skip_path("src/index.js.map"));
        assert!(AutoScanner::should_skip_path("src/chunk.bundle.js"));
        assert!(AutoScanner::should_skip_path("src/vendor.chunk.js"));
        assert!(AutoScanner::should_skip_path("lib/types.d.ts"));
        assert!(AutoScanner::should_skip_path("package-lock.lock"));
        assert!(AutoScanner::should_skip_path("src/utils.min.mjs"));
    }

    #[test]
    fn test_should_skip_path_the_offending_file() {
        // THE file that cost $0.14 in one API call
        assert!(AutoScanner::should_skip_path("dist/fks-web-kmp.js"));
        assert!(AutoScanner::should_skip_path(
            "src/clients/web/dist/fks-web-kmp.js"
        ));
    }

    #[test]
    fn test_should_not_skip_normal_code() {
        assert!(!AutoScanner::should_skip_path("src/main.rs"));
        assert!(!AutoScanner::should_skip_path("src/auto_scanner.rs"));
        assert!(!AutoScanner::should_skip_path("lib/utils.js"));
        assert!(!AutoScanner::should_skip_path("scripts/build.sh"));
        assert!(!AutoScanner::should_skip_path("src/components/App.tsx"));
        assert!(!AutoScanner::should_skip_path("cmd/server/main.go"));
    }

    #[test]
    fn test_should_not_skip_distribution_source_code() {
        // "distribution" in a path should NOT be caught by "/dist/" pattern
        assert!(!AutoScanner::should_skip_path("src/distribution/calc.py"));
        assert!(!AutoScanner::should_skip_path("lib/distribution/normal.rs"));
    }

    #[test]
    fn test_should_analyze_file_good_files() {
        assert!(AutoScanner::should_analyze_file("src/main.rs"));
        assert!(AutoScanner::should_analyze_file("lib/app.js"));
        assert!(AutoScanner::should_analyze_file("src/utils.ts"));
        assert!(AutoScanner::should_analyze_file("src/App.tsx"));
        assert!(AutoScanner::should_analyze_file("scripts/deploy.sh"));
        assert!(AutoScanner::should_analyze_file("src/Main.kt"));
        assert!(AutoScanner::should_analyze_file("src/Main.java"));
        assert!(AutoScanner::should_analyze_file("cmd/main.go"));
        assert!(AutoScanner::should_analyze_file("app.py"));
        assert!(AutoScanner::should_analyze_file("lib/helpers.rb"));
    }

    #[test]
    fn test_should_analyze_file_non_code() {
        assert!(!AutoScanner::should_analyze_file("README.md"));
        assert!(!AutoScanner::should_analyze_file("Cargo.toml"));
        assert!(!AutoScanner::should_analyze_file("data.json"));
        assert!(!AutoScanner::should_analyze_file("image.png"));
        assert!(!AutoScanner::should_analyze_file("styles.css"));
        assert!(!AutoScanner::should_analyze_file(".gitignore"));
    }

    #[test]
    fn test_should_analyze_file_code_in_skip_paths() {
        assert!(!AutoScanner::should_analyze_file("dist/bundle.js"));
        assert!(!AutoScanner::should_analyze_file(
            "node_modules/pkg/index.js"
        ));
        assert!(!AutoScanner::should_analyze_file("src/app.min.js"));
        assert!(!AutoScanner::should_analyze_file(
            "src/clients/web/dist/fks-web-kmp.js"
        ));
        assert!(!AutoScanner::should_analyze_file("build/output.js"));
        assert!(!AutoScanner::should_analyze_file("vendor/lib/helper.rb"));
    }

    #[test]
    fn test_is_analyzable_file() {
        assert!(AutoScanner::is_analyzable_file("main.rs"));
        assert!(AutoScanner::is_analyzable_file("script.py"));
        assert!(AutoScanner::is_analyzable_file("app.js"));
        assert!(AutoScanner::is_analyzable_file("component.tsx"));
        assert!(AutoScanner::is_analyzable_file("build.sh"));
        assert!(!AutoScanner::is_analyzable_file("readme.md"));
        assert!(!AutoScanner::is_analyzable_file("config.toml"));
        assert!(!AutoScanner::is_analyzable_file("data.csv"));
    }

    #[test]
    fn test_windows_path_normalization() {
        // Backslash paths should be normalized
        assert!(AutoScanner::should_skip_path(
            "src\\clients\\web\\dist\\bundle.js"
        ));
        assert!(AutoScanner::should_skip_path(
            "node_modules\\lodash\\index.js"
        ));
        assert!(!AutoScanner::should_skip_path("src\\main.rs"));
    }
}
