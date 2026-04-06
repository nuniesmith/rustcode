// src/repo_sync.rs
// RustCode RepoSyncService
// Handles repo registration, tree snapshots, TODO extraction, and .rustcode/ cache management

use crate::db::store_embedding;
use crate::embeddings::{EmbeddingConfig, EmbeddingGenerator};
use crate::research::worker::refresh_rag_index;
use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Row};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::fs;
use tokio::io::AsyncWriteExt;
use tracing::{debug, error, info, warn};

// ---------------------------------------------------------------------------
// Core types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegisteredRepo {
    pub id: String,
    pub name: String,
    pub local_path: PathBuf,
    pub remote_url: Option<String>,
    pub branch: String,
    pub last_synced: Option<u64>, // unix timestamp
    pub active: bool,
}

impl RegisteredRepo {
    pub fn new(name: impl Into<String>, local_path: impl Into<PathBuf>) -> Self {
        let name = name.into();
        let id = slugify(&name);
        Self {
            id,
            name,
            local_path: local_path.into(),
            remote_url: None,
            branch: "main".to_string(),
            last_synced: None,
            active: true,
        }
    }

    // Path to this repo's .rustcode/ cache dir.
    pub fn cache_dir(&self) -> PathBuf {
        self.local_path.join(".rustcode")
    }

    pub fn manifest_path(&self) -> PathBuf {
        self.cache_dir().join("manifest.json")
    }

    pub fn tree_path(&self) -> PathBuf {
        self.cache_dir().join("tree.txt")
    }

    pub fn todos_path(&self) -> PathBuf {
        self.cache_dir().join("todos.json")
    }

    pub fn symbols_path(&self) -> PathBuf {
        self.cache_dir().join("symbols.json")
    }

    pub fn context_path(&self) -> PathBuf {
        self.cache_dir().join("context.md")
    }
}

// ---------------------------------------------------------------------------
// Manifest (written to .rustcode/manifest.json)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepoManifest {
    pub id: String,
    pub name: String,
    pub remote_url: Option<String>,
    pub branch: String,
    pub synced_at: u64,
    pub file_count: usize,
    pub rust_file_count: usize,
    pub cargo_crate_name: Option<String>,
    pub rustcode_version: String,
}

// ---------------------------------------------------------------------------
// TODO item (written to .rustcode/todos.json)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TodoItem {
    pub kind: TodoKind,
    pub message: String,
    pub file: String, // relative path from repo root
    pub line: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TodoKind {
    Todo,
    Fixme,
    Stub,
    Hack,
    Note,
}

impl TodoKind {
    fn from_tag(tag: &str) -> Option<Self> {
        match tag.to_uppercase().as_str() {
            "TODO" => Some(Self::Todo),
            "FIXME" => Some(Self::Fixme),
            "STUB" => Some(Self::Stub),
            "HACK" => Some(Self::Hack),
            "NOTE" => Some(Self::Note),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Symbol (written to .rustcode/symbols.json)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Symbol {
    pub kind: SymbolKind,
    pub name: String,
    pub file: String,
    pub line: usize,
    pub is_pub: bool,
    pub is_async: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SymbolKind {
    Function,
    Struct,
    Enum,
    Trait,
    Impl,
    TypeAlias,
    Const,
    Mod,
}

// ---------------------------------------------------------------------------
// SyncResult
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncResult {
    pub repo_id: String,
    pub files_walked: usize,
    pub todos_found: usize,
    pub symbols_found: usize,
    pub duration_ms: u64,
    pub errors: Vec<String>,
}

// ---------------------------------------------------------------------------
// RepoSyncService
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub struct RepoSyncService {
    // In-memory registry (always authoritative; SQLite is the persistence backing store).
    repos: HashMap<String, RegisteredRepo>,
    // Optional PostgreSQL pool for persistent repo registration.
    db: Option<PgPool>,
    // File extensions to index (skip target/, .git/, node_modules/ etc.)
    include_extensions: Vec<String>,
    // Directories to always skip
    skip_dirs: Vec<String>,
}

impl Default for RepoSyncService {
    fn default() -> Self {
        Self {
            repos: HashMap::new(),
            db: None,
            include_extensions: vec![
                "rs".into(),
                "toml".into(),
                "md".into(),
                "sh".into(),
                "yml".into(),
                "yaml".into(),
                "json".into(),
                "sql".into(),
            ],
            skip_dirs: vec![
                "target".into(),
                ".git".into(),
                "node_modules".into(),
                ".sqlx".into(),
                "dist".into(),
            ],
        }
    }
}

impl RepoSyncService {
    // Create an in-memory-only service (no persistence across restarts).
    pub fn new() -> Self {
        Self::default()
    }

    // Create a service backed by an existing `PgPool`.
    //
    // Call [`load_from_db`] afterwards to populate the in-memory map from
    // any rows already in the `registered_repos` table.
    pub fn with_db(pool: PgPool) -> Self {
        Self {
            db: Some(pool),
            ..Self::default()
        }
    }

    // Return a clone of the underlying `PgPool` if one is configured.
    //
    // Used by the RAG pipeline to query embeddings without holding a lock
    // on the sync service itself.
    pub fn db_pool(&self) -> Option<PgPool> {
        self.db.clone()
    }

    // Load all active repos from `registered_repos` into the in-memory map.
    //
    // Safe to call at startup; silently skips rows whose `local_path` cannot
    // be parsed as a valid UTF-8 path.
    pub async fn load_from_db(&mut self) -> anyhow::Result<usize> {
        let pool = match &self.db {
            Some(p) => p,
            None => return Ok(0),
        };

        let rows = sqlx::query(
            r#"
            SELECT id, name, local_path, remote_url, branch, last_synced, active
            FROM registered_repos
            WHERE active = TRUE
            "#,
        )
        .fetch_all(pool)
        .await?;

        let count = rows.len();
        for row in rows {
            let id: String = row.try_get("id")?;
            let name: String = row.try_get("name")?;
            let local_path: String = row.try_get("local_path")?;
            let remote_url: Option<String> = row.try_get("remote_url")?;
            let branch: String = row.try_get("branch")?;
            let last_synced: Option<i64> = row.try_get("last_synced")?;
            let active: bool = row.try_get("active")?;

            let repo = RegisteredRepo {
                id: id.clone(),
                name,
                local_path: PathBuf::from(&local_path),
                remote_url,
                branch,
                last_synced: last_synced.map(|v| v as u64),
                active,
            };
            self.repos.insert(repo.id.clone(), repo);
        }

        info!(count, "Loaded registered repos from PostgreSQL");
        Ok(count)
    }

    // -----------------------------------------------------------------------
    // Registration
    // -----------------------------------------------------------------------

    // Register a repo by local path. Creates `.rustcode/` dir if missing.
    //
    // Persists to PostgreSQL when a pool is configured (upsert semantics — re-registering
    // an existing path is safe and updates the name / branch). The upsert uses
    // `ON CONFLICT (local_path)` so that re-registering the same physical path
    // always refreshes the record rather than inserting a duplicate.
    pub async fn register(&mut self, repo: RegisteredRepo) -> anyhow::Result<String> {
        let id = repo.id.clone();
        info!(repo = %id, path = ?repo.local_path, "Registering repo");

        // Ensure .rustcode/ dir exists
        let cache_dir = repo.cache_dir();
        if !cache_dir.exists() {
            fs::create_dir_all(&cache_dir).await?;
            info!(path = ?cache_dir, "Created .rustcode/ cache dir");
        }

        // Write a .gitignore inside .rustcode/ to exclude embeddings binary
        let gitignore = cache_dir.join(".gitignore");
        if !gitignore.exists() {
            fs::write(&gitignore, "embeddings.bin\n").await?;
        }

        // Persist to PostgreSQL (upsert by local_path — safe to call repeatedly).
        //
        // We conflict on `local_path` (unique partial index on active=TRUE in
        // migration 015) rather than `id` so that renaming a repo (new id/slug)
        // on an already-registered path still resolves to a single live row.
        if let Some(ref pool) = self.db {
            let local_path = repo.local_path.to_string_lossy().to_string();
            let last_synced: Option<i64> = repo.last_synced.map(|v| v as i64);
            if let Err(e) = sqlx::query(
                r#"
                INSERT INTO registered_repos
                    (id, name, local_path, remote_url, branch, last_synced, active)
                VALUES ($1, $2, $3, $4, $5, $6, $7)
                ON CONFLICT (local_path) WHERE active = TRUE DO UPDATE SET
                    id         = EXCLUDED.id,
                    name       = EXCLUDED.name,
                    remote_url = EXCLUDED.remote_url,
                    branch     = EXCLUDED.branch,
                    last_synced = COALESCE(EXCLUDED.last_synced, registered_repos.last_synced),
                    active     = EXCLUDED.active
                "#,
            )
            .bind(&repo.id)
            .bind(&repo.name)
            .bind(&local_path)
            .bind(&repo.remote_url)
            .bind(&repo.branch)
            .bind(last_synced)
            .bind(repo.active)
            .execute(pool)
            .await
            {
                error!(repo = %id, error = %e, "Failed to persist repo registration to PostgreSQL");
                // Continue — the in-memory map is the source of truth; DB failure is non-fatal.
            }
        }

        self.repos.insert(id.clone(), repo);
        Ok(id)
    }

    pub fn get_repo(&self, id: &str) -> Option<&RegisteredRepo> {
        self.repos.get(id)
    }

    pub fn list_repos(&self) -> Vec<&RegisteredRepo> {
        self.repos.values().filter(|r| r.active).collect()
    }

    // Remove a repo from the in-memory map (synchronous, no DB write).
    pub fn remove_repo(&mut self, id: &str) -> bool {
        self.repos.remove(id).is_some()
    }

    // Async version of `remove_repo` that also soft-deletes in PostgreSQL.
    pub async fn remove_repo_async(&mut self, id: &str) -> bool {
        let existed = self.repos.remove(id).is_some();
        if existed {
            if let Some(ref pool) = self.db {
                if let Err(e) =
                    sqlx::query("UPDATE registered_repos SET active = FALSE WHERE id = $1")
                        .bind(id)
                        .execute(pool)
                        .await
                {
                    error!(repo = %id, error = %e, "Failed to soft-delete repo in PostgreSQL");
                }
            }
        }
        existed
    }

    // -----------------------------------------------------------------------
    // Full sync
    // -----------------------------------------------------------------------

    // Perform a full sync of a registered repo: tree + todos + symbols + manifest.
    pub async fn sync(&mut self, repo_id: &str) -> anyhow::Result<SyncResult> {
        let repo = self
            .repos
            .get(repo_id)
            .ok_or_else(|| anyhow::anyhow!("Repo '{}' not registered", repo_id))?
            .clone();

        info!(repo = %repo_id, "Starting sync");
        let start = std::time::Instant::now();
        let mut errors = Vec::new();

        // 0. Read the previous tree.txt snapshot (if any) so we can diff it
        //    against the new file list and only re-embed changed/added .rs files.
        let prev_rs_paths: std::collections::HashSet<String> =
            tokio::fs::read_to_string(repo.tree_path())
                .await
                .unwrap_or_default()
                .lines()
                // tree.txt lines look like "  src/foo.rs" — strip leading whitespace.
                .map(|l| l.trim().to_string())
                .filter(|l| l.ends_with(".rs"))
                .collect();

        // 1. Walk tree
        let (tree_txt, walked_files) = self.walk_tree(&repo).await.unwrap_or_else(|e| {
            errors.push(format!("tree walk failed: {e}"));
            (String::new(), vec![])
        });

        // 2. Extract TODOs
        let todos = self
            .extract_todos(&repo, &walked_files)
            .await
            .unwrap_or_else(|e| {
                errors.push(format!("todo extraction failed: {e}"));
                vec![]
            });

        // 3. Extract symbols
        let symbols = self
            .extract_symbols(&repo, &walked_files)
            .await
            .unwrap_or_else(|e| {
                errors.push(format!("symbol extraction failed: {e}"));
                vec![]
            });

        let rust_files = walked_files
            .iter()
            .filter(|p| p.extension().map(|e| e == "rs").unwrap_or(false))
            .count();

        // 4. Write cache files
        let _cache_dir = repo.cache_dir();

        write_file(&repo.tree_path(), &tree_txt).await?;
        write_json(&repo.todos_path(), &todos).await?;
        write_json(&repo.symbols_path(), &symbols).await?;

        // 5. Write manifest
        let manifest = RepoManifest {
            id: repo.id.clone(),
            name: repo.name.clone(),
            remote_url: repo.remote_url.clone(),
            branch: repo.branch.clone(),
            synced_at: unix_now(),
            file_count: walked_files.len(),
            rust_file_count: rust_files,
            cargo_crate_name: read_crate_name(&repo.local_path).await,
            rustcode_version: env!("CARGO_PKG_VERSION").to_string(),
        };
        write_json(&repo.manifest_path(), &manifest).await?;

        // 6. Generate context.md summary
        let context = build_context_md(&repo, &manifest, &todos, &symbols);
        write_file(&repo.context_path(), &context).await?;

        // 7. Update last_synced timestamp in memory and SQLite
        let now = unix_now();
        if let Some(r) = self.repos.get_mut(repo_id) {
            r.last_synced = Some(now);
        }
        if let Some(ref pool) = self.db {
            let ts = now as i64;
            if let Err(e) =
                sqlx::query("UPDATE registered_repos SET last_synced = $1 WHERE id = $2")
                    .bind(ts)
                    .bind(repo_id)
                    .execute(pool)
                    .await
            {
                warn!(repo = %repo_id, error = %e, "Failed to update last_synced in PostgreSQL");
            }
        }

        // 8. Incremental embedding pass: only re-embed .rs files that are new
        //    or whose on-disk mtime is newer than the last sync timestamp.
        //    If no previous tree.txt existed (first sync) every file is embedded.
        if let Some(pool) = self.db.clone() {
            let last_synced_secs = repo.last_synced.unwrap_or(0);
            let is_first_sync = prev_rs_paths.is_empty();

            let changed_rs_files: Vec<PathBuf> = walked_files
                .iter()
                .filter(|p| p.extension().map(|e| e == "rs").unwrap_or(false))
                .filter(|p| {
                    if is_first_sync {
                        // Embed everything on the first sync.
                        return true;
                    }
                    // Relative path as it appears in tree.txt (relative to repo root).
                    let rel = p
                        .strip_prefix(&repo.local_path)
                        .map(|r| r.to_string_lossy().replace('\\', "/"))
                        .unwrap_or_else(|_| p.to_string_lossy().into_owned());

                    // New file: not present in the previous snapshot.
                    if !prev_rs_paths.contains(&rel) {
                        return true;
                    }

                    // Changed file: mtime more recent than last_synced.
                    if last_synced_secs > 0 {
                        if let Ok(meta) = std::fs::metadata(p) {
                            if let Ok(modified) = meta.modified() {
                                let secs = modified
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .map(|d| d.as_secs())
                                    .unwrap_or(0);
                                return secs > last_synced_secs;
                            }
                        }
                    }

                    false
                })
                .cloned()
                .collect();

            let total_rs = rust_files;
            let changed_count = changed_rs_files.len();

            if changed_rs_files.is_empty() {
                info!(
                    repo = %repo_id,
                    total_rs,
                    "Incremental embed: no changed .rs files — skipping embedding pass"
                );
            } else {
                info!(
                    repo = %repo_id,
                    changed = changed_count,
                    total_rs,
                    "Incremental embed: spawning background pass for changed files"
                );

                tokio::spawn(async move {
                    if let Err(e) = embed_rust_files(&pool, &changed_rs_files).await {
                        warn!(error = %e, "Background incremental embedding pass failed");
                    } else {
                        // Rebuild the in-process HNSW index so chat handler RAG is current.
                        if let Err(e) = refresh_rag_index(&pool).await {
                            warn!(error = %e, "RAG index refresh failed after incremental embed");
                        }
                    }
                });
            }
        }

        let duration_ms = start.elapsed().as_millis() as u64;
        info!(
            repo = %repo_id,
            files = walked_files.len(),
            todos = todos.len(),
            symbols = symbols.len(),
            duration_ms,
            "Sync complete"
        );

        Ok(SyncResult {
            repo_id: repo_id.to_string(),
            files_walked: walked_files.len(),
            todos_found: todos.len(),
            symbols_found: symbols.len(),
            duration_ms,
            errors,
        })
    }

    // -----------------------------------------------------------------------
    // Tree walker
    // -----------------------------------------------------------------------

    async fn walk_tree(&self, repo: &RegisteredRepo) -> anyhow::Result<(String, Vec<PathBuf>)> {
        let root = &repo.local_path;
        let mut lines = Vec::new();
        let mut files = Vec::new();

        walk_dir(
            root,
            root,
            &self.skip_dirs,
            &self.include_extensions,
            &mut lines,
            &mut files,
        )
        .await?;

        lines.sort();
        let tree_txt = format!(
            "# Project tree: {}\n# Generated: {}\n\n{}\n",
            repo.name,
            unix_now(),
            lines.join("\n")
        );

        Ok((tree_txt, files))
    }

    // -----------------------------------------------------------------------
    // TODO extractor
    // -----------------------------------------------------------------------

    async fn extract_todos(
        &self,
        repo: &RegisteredRepo,
        files: &[PathBuf],
    ) -> anyhow::Result<Vec<TodoItem>> {
        let mut todos = Vec::new();
        // Deduplication key: (file, line) — prevents duplicates on repeat syncs.
        let mut seen: HashSet<(String, usize)> = HashSet::new();
        let root = &repo.local_path;

        for path in files {
            // Only scan text files likely to contain comments
            let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
            if !["rs", "toml", "md", "sh", "yml", "yaml"].contains(&ext) {
                continue;
            }

            let content = match fs::read_to_string(path).await {
                Ok(c) => c,
                Err(e) => {
                    warn!(path = ?path, error = %e, "Could not read file for TODO extraction");
                    continue;
                }
            };

            let relative = path.strip_prefix(root).unwrap_or(path);
            let rel_str = relative.to_string_lossy();

            for (line_idx, line) in content.lines().enumerate() {
                let line_num = line_idx + 1;
                // Match: // TODO: ..., // FIXME: ..., // STUB: ..., # TODO: ...
                if let Some(item) = parse_todo_line(line, &rel_str, line_num) {
                    let key = (item.file.clone(), item.line);
                    if seen.insert(key) {
                        todos.push(item);
                    }
                }
            }
        }

        debug!(count = todos.len(), "Extracted TODO items");
        Ok(todos)
    }

    // -----------------------------------------------------------------------
    // Symbol extractor — syn AST parser with line-scanner fallback
    // -----------------------------------------------------------------------

    async fn extract_symbols(
        &self,
        repo: &RegisteredRepo,
        files: &[PathBuf],
    ) -> anyhow::Result<Vec<Symbol>> {
        let mut symbols = Vec::new();
        let root = &repo.local_path;

        for path in files {
            let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
            if ext != "rs" {
                continue;
            }

            let content = match fs::read_to_string(path).await {
                Ok(c) => c,
                Err(_) => continue,
            };

            let relative = path.strip_prefix(root).unwrap_or(path);
            let rel_str = relative.to_string_lossy();

            // Try syn AST parsing first; fall back to the line scanner if the
            // file doesn't parse (e.g. macro-heavy or generated code).
            let file_syms = extract_symbols_syn(&content, &rel_str)
                .unwrap_or_else(|| extract_symbols_lines(&content, &rel_str));

            symbols.extend(file_syms);
        }

        debug!(count = symbols.len(), "Extracted symbols");
        Ok(symbols)
    }

    // -----------------------------------------------------------------------
    // Context builder for chat injection
    // -----------------------------------------------------------------------

    // Build a compact context string suitable for LLM prompt injection.
    //
    // Sections (kept under ~3000 chars total to avoid context bloat):
    // - Crate name from manifest
    // - Project tree (first 80 lines)
    // - Top 10 open TODOs
    // - Top 5 public symbols (functions + structs)
    pub async fn build_prompt_context(&self, repo_id: &str) -> anyhow::Result<String> {
        let repo = self
            .repos
            .get(repo_id)
            .ok_or_else(|| anyhow::anyhow!("Repo not found: {}", repo_id))?;

        let tree = fs::read_to_string(repo.tree_path())
            .await
            .unwrap_or_default();
        let todos_raw = fs::read_to_string(repo.todos_path())
            .await
            .unwrap_or_default();
        let symbols_raw = fs::read_to_string(repo.symbols_path())
            .await
            .unwrap_or_default();
        let manifest_raw = fs::read_to_string(repo.manifest_path())
            .await
            .unwrap_or_default();

        let todos: Vec<TodoItem> = serde_json::from_str(&todos_raw).unwrap_or_default();
        let symbols: Vec<Symbol> = serde_json::from_str(&symbols_raw).unwrap_or_default();
        let manifest: Option<RepoManifest> = serde_json::from_str(&manifest_raw).ok();

        // Crate name header
        let crate_name = manifest
            .as_ref()
            .and_then(|m| m.cargo_crate_name.as_deref())
            .unwrap_or(&repo.name);

        // Truncate tree to first 80 lines
        let tree_snippet: String = tree.lines().take(80).collect::<Vec<_>>().join("\n");

        // Top 10 TODOs
        let todo_snippet: String = if todos.is_empty() {
            "  (none)".to_string()
        } else {
            todos
                .iter()
                .take(10)
                .map(|t| format!("  [{:?}] {}:{} — {}", t.kind, t.file, t.line, t.message))
                .collect::<Vec<_>>()
                .join("\n")
        };

        // Top 5 public symbols (fns first, then structs/enums/traits)
        let sym_snippet: String = {
            let mut pub_syms: Vec<&Symbol> = symbols.iter().filter(|s| s.is_pub).collect();
            // Put functions first, then types
            pub_syms.sort_by_key(|s| match s.kind {
                SymbolKind::Function => 0,
                SymbolKind::Struct | SymbolKind::Enum | SymbolKind::Trait => 1,
                _ => 2,
            });
            if pub_syms.is_empty() {
                "  (none)".to_string()
            } else {
                pub_syms
                    .iter()
                    .take(5)
                    .map(|s| {
                        let async_tag = if s.is_async { "async " } else { "" };
                        format!(
                            "  {}{:?} {} ({}:{})",
                            async_tag, s.kind, s.name, s.file, s.line
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            }
        };

        Ok(format!(
            "### Repo: {} (crate: `{}`)\n\n\
             #### Project Tree (truncated)\n```\n{}\n```\n\n\
             #### Open TODOs\n{}\n\n\
             #### Key Public Symbols\n{}\n",
            repo.name, crate_name, tree_snippet, todo_snippet, sym_snippet
        ))
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

// Recursive async directory walker.
#[async_recursion::async_recursion]
// ---------------------------------------------------------------------------
// Background embedding helper
// ---------------------------------------------------------------------------

// Chunk each `.rs` file into overlapping windows and upsert vector embeddings
// into the `document_embeddings` / `document_chunks` tables so the RAG index
// has fresh data after every sync.
//
// Uses BGE-small-EN (384D) via `fastembed` — the same model used by the
// startup `refresh_rag_index` pass.  Each file becomes one "document" whose
// chunks are stored under a stable chunk_id derived from `<repo>/<rel_path>#<idx>`.
//
// This is intentionally lenient: individual file failures are logged and
// skipped without aborting the rest of the batch.
async fn embed_rust_files(pool: &sqlx::PgPool, files: &[PathBuf]) -> anyhow::Result<()> {
    if files.is_empty() {
        return Ok(());
    }

    let generator = match EmbeddingGenerator::new(EmbeddingConfig::default()) {
        Ok(g) => g,
        Err(e) => {
            warn!(error = %e, "Could not initialise embedding model — skipping embed pass");
            return Ok(());
        }
    };

    let model_name = generator.model_name().to_string();
    let mut total_chunks = 0usize;

    for path in files {
        let content = match tokio::fs::read_to_string(path).await {
            Ok(c) => c,
            Err(e) => {
                warn!(path = ?path, error = %e, "embed_rust_files: skipping unreadable file");
                continue;
            }
        };

        if content.trim().is_empty() {
            continue;
        }

        // Simple fixed-size window chunking: 60 lines per chunk, 10-line overlap.
        let lines: Vec<&str> = content.lines().collect();
        let window = 60usize;
        let overlap = 10usize;
        let step = window.saturating_sub(overlap).max(1);

        let path_str = path.to_string_lossy();
        let chunks: Vec<String> = (0..lines.len())
            .step_by(step)
            .map(|start| {
                let end = (start + window).min(lines.len());
                lines[start..end].join("\n")
            })
            .filter(|c| !c.trim().is_empty())
            .collect();

        for (idx, chunk_text) in chunks.iter().enumerate() {
            let chunk_id = format!("repo-sync:{}#{}", path_str, idx);

            let embedding = match generator.embed(chunk_text).await {
                Ok(e) => e,
                Err(e) => {
                    warn!(chunk_id = %chunk_id, error = %e, "embed failed — skipping chunk");
                    continue;
                }
            };

            if let Err(e) =
                store_embedding(pool, chunk_id.clone(), embedding.vector, model_name.clone()).await
            {
                warn!(chunk_id = %chunk_id, error = %e, "store_embedding failed");
            } else {
                total_chunks += 1;
            }
        }
    }

    info!(
        files = files.len(),
        chunks = total_chunks,
        "embed_rust_files complete"
    );
    Ok(())
}

fn walk_dir<'a>(
    root: &'a Path,
    current: &'a Path,
    skip_dirs: &'a [String],
    include_exts: &'a [String],
    lines: &'a mut Vec<String>,
    files: &'a mut Vec<PathBuf>,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<()>> + Send + 'a>> {
    Box::pin(async move {
        // TODO: replace with tokio::fs::ReadDir stream for better performance on large repos
        let mut entries = fs::read_dir(current).await?;

        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();

            if path.is_dir() {
                if skip_dirs.iter().any(|s| s == &name) {
                    continue;
                }
                walk_dir(root, &path, skip_dirs, include_exts, lines, files).await?;
            } else {
                let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
                if !include_exts.iter().any(|e| e == ext) {
                    continue;
                }
                let relative = path.strip_prefix(root).unwrap_or(&path);
                lines.push(relative.to_string_lossy().to_string());
                files.push(path);
            }
        }

        Ok(())
    })
}

fn parse_todo_line(line: &str, file: &str, line_num: usize) -> Option<TodoItem> {
    // Match: // TODO: message  OR  # TODO: message  OR  -- TODO: message
    let patterns = ["// ", "# ", "-- ", "/* "];
    let tags = ["TODO", "FIXME", "STUB", "HACK", "NOTE"];

    let stripped = line.trim();
    for prefix in &patterns {
        if let Some(rest) = stripped.strip_prefix(prefix) {
            for tag in &tags {
                let tag_colon = format!("{}:", tag);
                let tag_space = format!("{} ", tag);
                let msg = if let Some(m) = rest.strip_prefix(&tag_colon) {
                    Some(m.trim().to_string())
                } else {
                    rest.strip_prefix(&tag_space).map(|m| m.trim().to_string())
                };
                if let Some(message) = msg {
                    return Some(TodoItem {
                        kind: TodoKind::from_tag(tag).unwrap_or(TodoKind::Todo),
                        message,
                        file: file.to_string(),
                        line: line_num,
                    });
                }
            }
        }
    }
    None
}

// ---------------------------------------------------------------------------
// syn-based symbol extractor
// ---------------------------------------------------------------------------

// Extract symbols from a Rust source file using the `syn` AST parser.
//
// Returns `None` when the file fails to parse (e.g. heavily macro-generated
// or non-standard syntax) so the caller can fall back to the line scanner.
fn extract_symbols_syn(content: &str, file: &str) -> Option<Vec<Symbol>> {
    use syn::{Item, Visibility};

    let ast = syn::parse_file(content).ok()?;

    // Build a byte-offset → line-number lookup using the raw source.
    // syn doesn't expose line numbers directly in its AST nodes, so we
    // pre-compute a sorted list of newline byte positions.
    let newline_offsets: Vec<usize> = content
        .bytes()
        .enumerate()
        .filter_map(|(i, b)| if b == b'\n' { Some(i) } else { None })
        .collect();

    let byte_to_line = |byte_offset: usize| -> usize {
        // Binary search for the newline immediately before this offset.
        match newline_offsets.binary_search(&byte_offset) {
            Ok(idx) => idx + 2,  // exactly on a newline → next line
            Err(idx) => idx + 1, // idx is the count of newlines before this position
        }
    };

    // syn doesn't store byte offsets in the main AST; we approximate line
    // numbers by scanning the source text for the symbol name.
    // For most files this is accurate; for files with duplicate names it
    // may be slightly off — acceptable for context purposes.
    let find_line = |name: &str, kind_hint: &str| -> usize {
        let needle_pub = format!("pub {} {}", kind_hint, name);
        let needle_bare = format!("{} {}", kind_hint, name);
        for (i, line) in content.lines().enumerate() {
            let t = line.trim();
            if t.contains(&needle_pub) || t.contains(&needle_bare) {
                return i + 1;
            }
        }
        1
    };

    let _ = byte_to_line; // suppress unused warning — kept for future use

    let mut symbols = Vec::new();

    for item in &ast.items {
        let sym = match item {
            Item::Fn(f) => {
                let name = f.sig.ident.to_string();
                let is_pub = matches!(f.vis, Visibility::Public(_));
                let is_async = f.sig.asyncness.is_some();
                let line = find_line(&name, "fn");
                Symbol {
                    kind: SymbolKind::Function,
                    name,
                    file: file.to_string(),
                    line,
                    is_pub,
                    is_async,
                }
            }
            Item::Struct(s) => {
                let name = s.ident.to_string();
                let is_pub = matches!(s.vis, Visibility::Public(_));
                let line = find_line(&name, "struct");
                Symbol {
                    kind: SymbolKind::Struct,
                    name,
                    file: file.to_string(),
                    line,
                    is_pub,
                    is_async: false,
                }
            }
            Item::Enum(e) => {
                let name = e.ident.to_string();
                let is_pub = matches!(e.vis, Visibility::Public(_));
                let line = find_line(&name, "enum");
                Symbol {
                    kind: SymbolKind::Enum,
                    name,
                    file: file.to_string(),
                    line,
                    is_pub,
                    is_async: false,
                }
            }
            Item::Trait(t) => {
                let name = t.ident.to_string();
                let is_pub = matches!(t.vis, Visibility::Public(_));
                let line = find_line(&name, "trait");
                Symbol {
                    kind: SymbolKind::Trait,
                    name,
                    file: file.to_string(),
                    line,
                    is_pub,
                    is_async: false,
                }
            }
            Item::Impl(i) => {
                // impl Trait for Type  →  use the self_ty name
                let name = match i.self_ty.as_ref() {
                    syn::Type::Path(tp) => tp
                        .path
                        .segments
                        .last()
                        .map(|s| s.ident.to_string())
                        .unwrap_or_else(|| "impl".to_string()),
                    _ => "impl".to_string(),
                };
                let trait_name = i.trait_.as_ref().map(|(_, p, _)| {
                    p.segments
                        .last()
                        .map(|s| s.ident.to_string())
                        .unwrap_or_default()
                });
                let full_name = match trait_name {
                    Some(t) if !t.is_empty() => format!("{} for {}", t, name),
                    _ => name.clone(),
                };
                let line = find_line(&name, "impl");
                Symbol {
                    kind: SymbolKind::Impl,
                    name: full_name,
                    file: file.to_string(),
                    line,
                    is_pub: false,
                    is_async: false,
                }
            }
            Item::Type(t) => {
                let name = t.ident.to_string();
                let is_pub = matches!(t.vis, Visibility::Public(_));
                let line = find_line(&name, "type");
                Symbol {
                    kind: SymbolKind::TypeAlias,
                    name,
                    file: file.to_string(),
                    line,
                    is_pub,
                    is_async: false,
                }
            }
            Item::Const(c) => {
                let name = c.ident.to_string();
                let is_pub = matches!(c.vis, Visibility::Public(_));
                let line = find_line(&name, "const");
                Symbol {
                    kind: SymbolKind::Const,
                    name,
                    file: file.to_string(),
                    line,
                    is_pub,
                    is_async: false,
                }
            }
            Item::Mod(m) => {
                let name = m.ident.to_string();
                let is_pub = matches!(m.vis, Visibility::Public(_));
                let line = find_line(&name, "mod");
                Symbol {
                    kind: SymbolKind::Mod,
                    name,
                    file: file.to_string(),
                    line,
                    is_pub,
                    is_async: false,
                }
            }
            _ => continue,
        };
        symbols.push(sym);
    }

    Some(symbols)
}

// ---------------------------------------------------------------------------
// Line-scanner fallback (used when syn fails to parse a file)
// ---------------------------------------------------------------------------

fn extract_symbols_lines(content: &str, file: &str) -> Vec<Symbol> {
    let mut symbols = Vec::new();
    for (line_idx, line) in content.lines().enumerate() {
        let trimmed = line.trim();
        if let Some(sym) = parse_symbol_line(trimmed, file, line_idx + 1) {
            symbols.push(sym);
        }
    }
    symbols
}

fn parse_symbol_line(line: &str, file: &str, line_num: usize) -> Option<Symbol> {
    let is_pub = line.starts_with("pub ");
    let is_async = line.contains("async fn");

    let check_line = line
        .trim_start_matches("pub ")
        .trim_start_matches("async ")
        .trim_start_matches("unsafe ");

    let (kind, name) = if check_line.starts_with("fn ") {
        let n = extract_name(check_line, "fn ")?;
        (SymbolKind::Function, n)
    } else if check_line.starts_with("struct ") {
        let n = extract_name(check_line, "struct ")?;
        (SymbolKind::Struct, n)
    } else if check_line.starts_with("enum ") {
        let n = extract_name(check_line, "enum ")?;
        (SymbolKind::Enum, n)
    } else if check_line.starts_with("trait ") {
        let n = extract_name(check_line, "trait ")?;
        (SymbolKind::Trait, n)
    } else if check_line.starts_with("impl ") {
        let n = extract_impl_name(check_line)?;
        (SymbolKind::Impl, n)
    } else if check_line.starts_with("type ") {
        let n = extract_name(check_line, "type ")?;
        (SymbolKind::TypeAlias, n)
    } else if check_line.starts_with("const ") {
        let n = extract_name(check_line, "const ")?;
        (SymbolKind::Const, n)
    } else if check_line.starts_with("mod ") {
        let n = extract_name(check_line, "mod ")?;
        (SymbolKind::Mod, n)
    } else {
        return None;
    };

    Some(Symbol {
        kind,
        name,
        file: file.to_string(),
        line: line_num,
        is_pub,
        is_async,
    })
}

fn extract_name(line: &str, prefix: &str) -> Option<String> {
    let rest = line.strip_prefix(prefix)?;
    let name: String = rest
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();
    if name.is_empty() { None } else { Some(name) }
}

fn extract_impl_name(line: &str) -> Option<String> {
    // impl Foo  OR  impl<T> Foo  OR  impl Trait for Foo
    let rest = line.strip_prefix("impl")?;
    let rest = rest.trim();
    // Skip generic params
    let rest = if rest.starts_with('<') {
        let end = rest.find('>')?;
        rest[end + 1..].trim()
    } else {
        rest
    };
    // If "Trait for Type", take the type
    let name_part = if let Some(idx) = rest.find(" for ") {
        &rest[idx + 5..]
    } else {
        rest
    };
    let name: String = name_part
        .trim()
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();
    if name.is_empty() { None } else { Some(name) }
}

async fn read_crate_name(path: &Path) -> Option<String> {
    let cargo_toml = path.join("Cargo.toml");
    let content = fs::read_to_string(cargo_toml).await.ok()?;
    for line in content.lines() {
        if let Some(rest) = line.strip_prefix("name") {
            let val = rest.trim().trim_start_matches('=').trim().trim_matches('"');
            if !val.is_empty() {
                return Some(val.to_string());
            }
        }
    }
    None
}

fn build_context_md(
    repo: &RegisteredRepo,
    manifest: &RepoManifest,
    todos: &[TodoItem],
    symbols: &[Symbol],
) -> String {
    let todo_count = todos.len();
    let stub_count = todos.iter().filter(|t| t.kind == TodoKind::Stub).count();
    let fixme_count = todos.iter().filter(|t| t.kind == TodoKind::Fixme).count();
    let pub_fn_count = symbols
        .iter()
        .filter(|s| s.kind == SymbolKind::Function && s.is_pub)
        .count();
    let struct_count = symbols
        .iter()
        .filter(|s| s.kind == SymbolKind::Struct)
        .count();

    format!(
        r#"# RustCode Context: {}

**Crate:** {}
**Branch:** {}
**Synced:** {}
**Files:** {} total, {} Rust

## Annotations
- {} total TODOs ({} STUBs, {} FIXMEs)

## Symbols
- {} public functions
- {} structs

## Remote
{}
"#,
        repo.name,
        manifest.cargo_crate_name.as_deref().unwrap_or("unknown"),
        repo.branch,
        manifest.synced_at,
        manifest.file_count,
        manifest.rust_file_count,
        todo_count,
        stub_count,
        fixme_count,
        pub_fn_count,
        struct_count,
        repo.remote_url.as_deref().unwrap_or("not set"),
    )
}

async fn write_file(path: &Path, content: &str) -> anyhow::Result<()> {
    let mut f = fs::File::create(path).await?;
    f.write_all(content.as_bytes()).await?;
    Ok(())
}

async fn write_json<T: Serialize>(path: &Path, value: &T) -> anyhow::Result<()> {
    let json = serde_json::to_string_pretty(value)?;
    write_file(path, &json).await
}

fn slugify(s: &str) -> String {
    s.to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .trim_matches('-')
        .to_string()
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_todo_rust_comment() {
        let item = parse_todo_line("    // TODO: implement retry logic", "src/webhooks.rs", 42);
        assert!(item.is_some());
        let item = item.unwrap();
        assert_eq!(item.kind, TodoKind::Todo);
        assert_eq!(item.line, 42);
        assert!(item.message.contains("retry"));
    }

    #[test]
    fn parse_stub_tag() {
        let item = parse_todo_line("// STUB: generated by rustcode", "src/cache_layer.rs", 10);
        assert!(item.is_some());
        assert_eq!(item.unwrap().kind, TodoKind::Stub);
    }

    #[test]
    fn parse_symbol_pub_fn() {
        let sym = parse_symbol_line("pub async fn handle_webhook(", "src/webhooks.rs", 55);
        assert!(sym.is_some());
        let sym = sym.unwrap();
        assert_eq!(sym.kind, SymbolKind::Function);
        assert!(sym.is_pub);
        assert!(sym.is_async);
        assert_eq!(sym.name, "handle_webhook");
    }

    #[test]
    fn parse_symbol_struct() {
        let sym = parse_symbol_line("pub struct WebhookEvent {", "src/webhooks.rs", 12);
        assert!(sym.is_some());
        assert_eq!(sym.unwrap().kind, SymbolKind::Struct);
    }

    #[test]
    fn slugify_basic() {
        assert_eq!(slugify("My Cool Repo"), "my-cool-repo");
        assert_eq!(slugify("rustcode"), "rustcode");
    }

    // Verify that `register()` writes `.rustcode/.gitignore` containing
    // `embeddings.bin` so that the binary embedding file is never accidentally
    // committed to the target repo.
    #[tokio::test]
    async fn register_writes_gitignore_with_embeddings_bin() {
        let tmp = tempfile::tempdir().expect("failed to create tempdir");
        let repo_path = tmp.path().to_path_buf();

        let repo = RegisteredRepo::new("test-gitignore-repo", &repo_path);
        let mut svc = RepoSyncService::new();
        svc.register(repo).await.expect("register failed");

        let gitignore_path = repo_path.join(".rustcode").join(".gitignore");
        assert!(
            gitignore_path.exists(),
            ".rustcode/.gitignore was not created by register()"
        );

        let contents = std::fs::read_to_string(&gitignore_path).expect("failed to read .gitignore");
        assert!(
            contents.contains("embeddings.bin"),
            ".gitignore does not contain 'embeddings.bin'; got: {:?}",
            contents
        );
    }

    // Re-registering the same repo must not overwrite an existing `.gitignore`
    // (idempotency — users may add their own entries).
    #[tokio::test]
    async fn register_does_not_overwrite_existing_gitignore() {
        let tmp = tempfile::tempdir().expect("failed to create tempdir");
        let repo_path = tmp.path().to_path_buf();

        // Pre-create the cache dir and a custom .gitignore
        let cache_dir = repo_path.join(".rustcode");
        std::fs::create_dir_all(&cache_dir).expect("create cache dir");
        let gitignore_path = cache_dir.join(".gitignore");
        std::fs::write(&gitignore_path, "embeddings.bin\nmy-custom-entry\n")
            .expect("write gitignore");

        let repo = RegisteredRepo::new("test-gitignore-idempotent", &repo_path);
        let mut svc = RepoSyncService::new();
        svc.register(repo).await.expect("register failed");

        let contents = std::fs::read_to_string(&gitignore_path).expect("failed to read .gitignore");
        assert!(
            contents.contains("my-custom-entry"),
            "register() overwrote the existing .gitignore; contents: {:?}",
            contents
        );
    }
}
