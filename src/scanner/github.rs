//! GitHub Repository Scanner
//!
//! Scans repositories under the configured GitHub username for:
//! - TODO/FIXME/HACK comments
//! - Files needing analysis
//! - Directory structure caching

use crate::db::core::create_task;
use crate::db::queue::GITHUB_USERNAME;
use crate::queue::processor::capture_todo;
use anyhow::Result;
use chrono::Utc;
use regex::Regex;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use std::path::{Path, PathBuf};
use tracing::{debug, error, info};
use walkdir::WalkDir;

// ============================================================================
// Configuration
// ============================================================================

/// Patterns for TODO detection
const TODO_PATTERNS: &[&str] = &[
    r"(?i)\b(TODO|FIXME|HACK|XXX|BUG|NOTE)[\s:]+(.+?)(?:\*/|\n|$)",
    r"(?i)#\s*(TODO|FIXME|HACK|XXX|BUG|NOTE)[\s:]+(.+?)$",
    r"(?i)//\s*(TODO|FIXME|HACK|XXX|BUG|NOTE)[\s:]+(.+?)$",
];

/// File extensions to scan for TODOs
const SCANNABLE_EXTENSIONS: &[&str] = &[
    "rs", "py", "js", "ts", "jsx", "tsx", "go", "java", "c", "cpp", "h", "hpp", "rb", "php",
    "swift", "kt", "scala", "sh", "bash", "zsh", "sql", "md", "txt", "yaml", "yml", "toml", "json",
    "html", "css", "scss", "vue", "svelte",
];

/// Directories to skip
const SKIP_DIRS: &[&str] = &[
    "node_modules",
    "target",
    "dist",
    "build",
    ".git",
    ".svn",
    "vendor",
    "__pycache__",
    ".venv",
    "venv",
    ".idea",
    ".vscode",
    "coverage",
];

// ============================================================================
// GitHub API Client
// ============================================================================

/// GitHub repository info
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitHubRepo {
    pub id: i64,
    pub name: String,
    pub full_name: String,
    pub description: Option<String>,
    pub html_url: String,
    pub clone_url: String,
    pub ssh_url: String,
    pub default_branch: String,
    pub language: Option<String>,
    pub stargazers_count: i32,
    pub forks_count: i32,
    pub open_issues_count: i32,
    pub size: i64,
    pub created_at: String,
    pub updated_at: String,
    pub pushed_at: String,
    pub topics: Vec<String>,
    pub archived: bool,
    pub disabled: bool,
    pub private: bool,
}

/// Fetch all repositories for the configured user
pub async fn fetch_user_repos(token: Option<&str>) -> Result<Vec<GitHubRepo>> {
    let client = reqwest::Client::new();
    let mut repos = Vec::new();
    let mut page = 1;

    loop {
        let url = format!(
            "https://api.github.com/users/{}/repos$1per_page=100&page={}&sort=updated",
            GITHUB_USERNAME, page
        );

        let mut request = client
            .get(&url)
            .header("User-Agent", "rustcode")
            .header("Accept", "application/vnd.github.v3+json");

        if let Some(t) = token {
            request = request.header("Authorization", format!("Bearer {}", t));
        }

        let response = request.send().await?;

        if !response.status().is_success() {
            error!("GitHub API error: {}", response.status());
            break;
        }

        let page_repos: Vec<GitHubRepo> = response.json().await?;

        if page_repos.is_empty() {
            break;
        }

        repos.extend(page_repos);
        page += 1;

        // Rate limit protection
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
    }

    info!(
        "Fetched {} repositories for {}",
        repos.len(),
        GITHUB_USERNAME
    );
    Ok(repos)
}

/// Sync GitHub repos to local database
pub async fn sync_repos_to_db(pool: &PgPool, token: Option<&str>) -> Result<Vec<String>> {
    let github_repos = fetch_user_repos(token).await?;
    let now = Utc::now().timestamp();
    let mut repo_ids = Vec::new();

    for gh_repo in github_repos {
        if gh_repo.archived || gh_repo.disabled {
            continue; // Skip archived/disabled repos
        }

        let id = format!("gh-{}", gh_repo.id);

        // Upsert repository
        sqlx::query(
            r#"
            INSERT INTO repositories (id, url, name, created_at, updated_at)
            VALUES ($1, $2, $3, $4, $5)
            ON CONFLICT(id) DO UPDATE SET
                name = excluded.name,
                url = excluded.url,
                updated_at = excluded.updated_at
        "#,
        )
        .bind(&id)
        .bind(&gh_repo.clone_url)
        .bind(&gh_repo.name)
        .bind(now)
        .bind(now)
        .execute(pool)
        .await?;

        // Ensure repo_cache entry exists
        sqlx::query(
            r#"
            INSERT INTO repo_cache (id, repo_id, created_at, updated_at)
            VALUES ($1, $2, $3, $4)
            ON CONFLICT (id) DO NOTHING
        "#,
        )
        .bind(format!("cache-{}", id))
        .bind(&id)
        .bind(now)
        .bind(now)
        .execute(pool)
        .await?;

        repo_ids.push(id);
    }

    info!("Synced {} active repositories", repo_ids.len());
    Ok(repo_ids)
}

// ============================================================================
// TODO Scanner
// ============================================================================

/// A detected TODO in code
#[derive(Debug, Clone)]
pub struct DetectedTodo {
    pub todo_type: String,
    pub content: String,
    pub file_path: String,
    pub line_number: i32,
}

/// Scan a directory for TODOs
pub fn scan_directory_for_todos(root: &Path) -> Result<Vec<DetectedTodo>> {
    let mut todos = Vec::new();
    let patterns: Vec<Regex> = TODO_PATTERNS
        .iter()
        .filter_map(|p| Regex::new(p).ok())
        .collect();

    for entry in WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| !should_skip_entry(e))
    {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };

        if !entry.file_type().is_file() {
            continue;
        }

        let path = entry.path();

        // Check extension
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");

        if !SCANNABLE_EXTENSIONS.contains(&ext) {
            continue;
        }

        // Read file
        let content = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(_) => continue, // Skip binary or unreadable files
        };

        // Scan for TODOs
        for (line_num, line) in content.lines().enumerate() {
            for pattern in &patterns {
                for caps in pattern.captures_iter(line) {
                    if let (Some(todo_type), Some(todo_content)) = (caps.get(1), caps.get(2)) {
                        todos.push(DetectedTodo {
                            todo_type: todo_type.as_str().to_uppercase(),
                            content: todo_content.as_str().trim().to_string(),
                            file_path: path
                                .strip_prefix(root)
                                .unwrap_or(path)
                                .to_string_lossy()
                                .to_string(),
                            line_number: (line_num + 1) as i32,
                        });
                    }
                }
            }
        }
    }

    info!("Found {} TODOs in {:?}", todos.len(), root);
    Ok(todos)
}

fn should_skip_entry(entry: &walkdir::DirEntry) -> bool {
    let name = entry.file_name().to_string_lossy();

    // Skip hidden files/dirs (except .github)
    if name.starts_with('.') && name != ".github" {
        return true;
    }

    // Skip known directories
    if entry.file_type().is_dir() && SKIP_DIRS.contains(&name.as_ref()) {
        return true;
    }

    false
}

/// Scan a repo and save TODOs to database + queue
pub async fn scan_repo_for_todos(
    pool: &PgPool,
    repo_id: &str,
    repo_path: &Path,
) -> Result<ScanResult> {
    let detected = scan_directory_for_todos(repo_path)?;
    let now = Utc::now().timestamp();

    // Deduplicate against existing tasks for this repo so repeated scans
    // don't create duplicate rows in the tasks table.
    let existing_source_ids: std::collections::HashSet<String> = sqlx::query_as::<_, (String,)>(
        "SELECT source_id FROM tasks WHERE repo_id = $1 AND source = 'github_scanner' AND source_id IS NOT NULL"
    )
    .bind(repo_id)
    .fetch_all(pool)
    .await
    .unwrap_or_default()
    .into_iter()
    .map(|(s,)| s)
    .collect();

    let mut new_todos = 0;
    let mut updated_todos = 0;

    for todo in &detected {
        let content_hash = format!(
            "{:x}",
            md5::compute(format!(
                "{}:{}:{}",
                todo.file_path, todo.line_number, todo.content
            ))
        );

        // Skip if a task for this exact content already exists.
        if existing_source_ids.contains(&content_hash) {
            updated_todos += 1;
            continue;
        }

        // Derive a numeric priority from the TODO type.
        let priority: i32 = match todo.todo_type.to_uppercase().as_str() {
            "FIXME" | "BUG" | "XXX" => 1,
            "HACK" => 2,
            _ => 3, // TODO, NOTE, etc.
        };

        let title = format!(
            "[{}] {} ({}:{})",
            todo.todo_type, todo.content, todo.file_path, todo.line_number
        );

        // Write directly to the tasks table.
        if let Err(e) = create_task(
            pool,
            &title,
            None,
            priority,
            "github_scanner",
            Some(content_hash.as_str()), // source_id doubles as dedup key
            Some(repo_id),
            Some(todo.file_path.as_str()),
            Some(todo.line_number),
        )
        .await
        {
            tracing::warn!(error = %e, "scan_repo_for_todos: failed to create task — skipping");
        } else {
            // Also enqueue for LLM analysis so the queue processor can
            // enrich the task with tags and an LLM summary.
            let _ = capture_todo(
                pool,
                &todo.content,
                repo_id,
                &todo.file_path,
                todo.line_number,
            )
            .await;

            new_todos += 1;
        }
    }

    // Count how many tasks exist for this repo from the scanner.
    let active_count: (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM tasks WHERE repo_id = $1 AND source = 'github_scanner'",
    )
    .bind(repo_id)
    .fetch_one(pool)
    .await
    .unwrap_or((0,));

    let removed: (i64,) = (0,); // No longer tracked per-item; removal is handled by task status.

    sqlx::query(
        "UPDATE repo_cache SET total_todos = $1, active_todos = $2, last_scan_at = $3, updated_at = $4 WHERE repo_id = $5"
    )
    .bind(detected.len() as i32)
    .bind(active_count.0 as i32)
    .bind(now)
    .bind(now)
    .bind(repo_id)
    .execute(pool)
    .await?;

    Ok(ScanResult {
        total_found: detected.len(),
        new_todos,
        updated_todos,
        removed_todos: removed.0 as usize,
    })
}

#[derive(Debug)]
pub struct ScanResult {
    pub total_found: usize,
    pub new_todos: usize,
    pub updated_todos: usize,
    pub removed_todos: usize,
}

// ============================================================================
// File Analysis Tracking
// ============================================================================

/// Get files that haven't been analyzed yet
pub async fn get_unanalyzed_files(
    pool: &PgPool,
    repo_id: &str,
    repo_path: &Path,
    limit: i32,
) -> Result<Vec<PathBuf>> {
    // Get all analyzed file hashes for this repo
    let analyzed: Vec<(String,)> =
        sqlx::query_as("SELECT content_hash FROM file_analysis WHERE repo_id = $1")
            .bind(repo_id)
            .fetch_all(pool)
            .await?;

    let analyzed_hashes: std::collections::HashSet<_> =
        analyzed.into_iter().map(|(h,)| h).collect();

    let mut unanalyzed = Vec::new();

    for entry in WalkDir::new(repo_path)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| !should_skip_entry(e))
    {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };

        if !entry.file_type().is_file() {
            continue;
        }

        let path = entry.path();
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");

        if !SCANNABLE_EXTENSIONS.contains(&ext) {
            continue;
        }

        // Calculate content hash
        if let Ok(content) = std::fs::read(path) {
            let hash = format!("{:x}", md5::compute(&content));

            if !analyzed_hashes.contains(&hash) {
                unanalyzed.push(path.to_path_buf());

                if unanalyzed.len() >= limit as usize {
                    break;
                }
            }
        }
    }

    debug!(
        "Found {} unanalyzed files in {:?}",
        unanalyzed.len(),
        repo_path
    );
    Ok(unanalyzed)
}

/// Record file analysis result
pub async fn save_file_analysis(
    pool: &PgPool,
    repo_id: &str,
    file_path: &str,
    content: &[u8],
    analysis: &crate::queue::processor::FileAnalysisResult,
) -> Result<()> {
    let now = Utc::now().timestamp();
    let content_hash = format!("{:x}", md5::compute(content));
    let id = uuid::Uuid::new_v4().to_string();
    let line_count = std::str::from_utf8(content)
        .map(|s| s.lines().count())
        .unwrap_or(0) as i32;
    let ext = Path::new(file_path)
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_string());

    sqlx::query(r#"
        INSERT INTO file_analysis
        (id, repo_id, file_path, extension, content_hash, size_bytes, line_count,
         summary, purpose, language, complexity_score, quality_score, security_notes,
         improvements, dependencies, exports, tags, needs_attention, analyzed_at, created_at, updated_at)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18, $19, $20, $21)
        ON CONFLICT(repo_id, file_path) DO UPDATE SET
            content_hash = excluded.content_hash,
            size_bytes = excluded.size_bytes,
            line_count = excluded.line_count,
            summary = excluded.summary,
            purpose = excluded.purpose,
            language = excluded.language,
            complexity_score = excluded.complexity_score,
            quality_score = excluded.quality_score,
            security_notes = excluded.security_notes,
            improvements = excluded.improvements,
            dependencies = excluded.dependencies,
            exports = excluded.exports,
            tags = excluded.tags,
            needs_attention = excluded.needs_attention,
            analyzed_at = excluded.analyzed_at,
            updated_at = excluded.updated_at
    "#)
    .bind(&id)
    .bind(repo_id)
    .bind(file_path)
    .bind(&ext)
    .bind(&content_hash)
    .bind(content.len() as i64)
    .bind(line_count)
    .bind(&analysis.summary)
    .bind(&analysis.purpose)
    .bind(&analysis.language)
    .bind(analysis.complexity_score)
    .bind(analysis.quality_score)
    .bind(serde_json::to_string(&analysis.security_notes)?)
    .bind(serde_json::to_string(&analysis.improvements)?)
    .bind(serde_json::to_string(&analysis.dependencies)?)
    .bind(serde_json::to_string(&analysis.exports)?)
    .bind(analysis.tags.join(","))
    .bind(analysis.needs_attention)
    .bind(now)
    .bind(now)
    .bind(now)
    .execute(pool)
    .await?;

    // Update repo cache analyzed count
    sqlx::query(
        "UPDATE repo_cache SET analyzed_files = analyzed_files + 1, updated_at = $1 WHERE repo_id = $2"
    )
    .bind(now)
    .bind(repo_id)
    .execute(pool)
    .await?;

    Ok(())
}

// ============================================================================
// Directory Tree Builder
// ============================================================================

/// Node in the directory tree
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TreeNode {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extension: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub analyzed: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub score: Option<i32>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub children: Vec<TreeNode>,
}

/// Build directory tree for a repository
pub fn build_dir_tree(root: &Path, max_depth: usize) -> Result<TreeNode> {
    fn build_node(path: &Path, root: &Path, depth: usize, max_depth: usize) -> Option<TreeNode> {
        if depth > max_depth {
            return None;
        }

        let name = path.file_name()?.to_string_lossy().to_string();
        let rel_path = path.strip_prefix(root).ok()?.to_string_lossy().to_string();

        // Skip hidden and common skip dirs
        if name.starts_with('.') && name != ".github" {
            return None;
        }
        if SKIP_DIRS.contains(&name.as_str()) {
            return None;
        }

        if path.is_dir() {
            let mut children = Vec::new();

            if let Ok(entries) = std::fs::read_dir(path) {
                for entry in entries.filter_map(Result::ok) {
                    if let Some(child) = build_node(&entry.path(), root, depth + 1, max_depth) {
                        children.push(child);
                    }
                }
            }

            // Sort: directories first, then by name
            children.sort_by(|a, b| match (a.is_dir, b.is_dir) {
                (true, false) => std::cmp::Ordering::Less,
                (false, true) => std::cmp::Ordering::Greater,
                _ => a.name.cmp(&b.name),
            });

            Some(TreeNode {
                name,
                path: rel_path,
                is_dir: true,
                extension: None,
                size: None,
                analyzed: None,
                score: None,
                children,
            })
        } else {
            let metadata = std::fs::metadata(path).ok()?;
            let ext = path.extension().and_then(|e| e.to_str()).map(String::from);

            Some(TreeNode {
                name,
                path: rel_path,
                is_dir: false,
                extension: ext,
                size: Some(metadata.len()),
                analyzed: None,
                score: None,
                children: Vec::new(),
            })
        }
    }

    let root_name = root
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "root".to_string());

    let mut tree = build_node(root, root, 0, max_depth).unwrap_or_else(|| TreeNode {
        name: root_name,
        path: String::new(),
        is_dir: true,
        extension: None,
        size: None,
        analyzed: None,
        score: None,
        children: Vec::new(),
    });

    tree.path = String::new(); // Root has empty path
    Ok(tree)
}

/// Save directory tree to repo cache
pub async fn save_dir_tree(pool: &PgPool, repo_id: &str, tree: &TreeNode) -> Result<()> {
    let now = Utc::now().timestamp();
    let tree_json = serde_json::to_string(tree)?;

    // Count total files
    fn count_files(node: &TreeNode) -> i32 {
        if node.is_dir {
            node.children.iter().map(count_files).sum()
        } else {
            1
        }
    }

    let total_files = count_files(tree);

    sqlx::query(
        "UPDATE repo_cache SET dir_tree = $1, total_files = $2, tree_updated_at = $3, updated_at = $4 WHERE repo_id = $5"
    )
    .bind(&tree_json)
    .bind(total_files)
    .bind(now)
    .bind(now)
    .bind(repo_id)
    .execute(pool)
    .await?;

    info!(
        "Saved directory tree for {} ({} files)",
        repo_id, total_files
    );
    Ok(())
}

/// Get cached directory tree
pub async fn get_dir_tree(pool: &PgPool, repo_id: &str) -> Result<Option<TreeNode>> {
    let cache: Option<(Option<String>,)> =
        sqlx::query_as("SELECT dir_tree FROM repo_cache WHERE repo_id = $1")
            .bind(repo_id)
            .fetch_optional(pool)
            .await?;

    if let Some((Some(tree_json),)) = cache {
        let tree: TreeNode = serde_json::from_str(&tree_json)?;
        Ok(Some(tree))
    } else {
        Ok(None)
    }
}
