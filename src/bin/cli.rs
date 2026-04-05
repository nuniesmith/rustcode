// Rustassistant CLI
//
// Command-line interface for managing notes, repositories, and tasks.

use clap::{Parser, Subcommand};
use colored::Colorize;

use std::path::PathBuf;

// Import from our crate
use rustcode::cli::{
    handle_github_command, handle_queue_command, handle_report_command, handle_scan_command,
    GithubCommands, QueueCommands, ReportCommands, ScanCommands,
};
use rustcode::db::{
    self, create_note, get_next_task, get_stats, list_notes, list_repositories, list_tasks,
    search_notes, update_task_status,
};
use rustcode::repo_cache::{CacheType, RepoCache};
use rustcode::repo_cache_sql::{CacheSetParams as SqlCacheSetParams, RepoCacheSql};

// Todo pipeline imports
use rustcode::todo::{
    PlannerConfig, ScaffoldConfig, ScanConfig, SyncConfig, TodoCommentScanner, TodoPlanner,
    TodoScaffolder, TodoSyncer, TodoWorker, WorkBatch, WorkConfig,
};

// ============================================================================
// CLI Structure
// ============================================================================

#[derive(Parser)]
#[command(name = "rustcode")]
#[command(about = "Developer workflow management tool", version)]
#[command(author = "nuniesmith")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    // Manage notes
    Note {
        #[command(subcommand)]
        action: NoteAction,
    },

    // Manage repositories
    Repo {
        #[command(subcommand)]
        action: RepoAction,
    },

    // Manage tasks
    Tasks {
        #[command(subcommand)]
        action: TaskAction,
    },

    // Manage processing queue
    Queue {
        #[command(subcommand)]
        action: QueueCommands,
    },

    // Scan repositories
    Scan {
        #[command(subcommand)]
        action: ScanCommands,
    },

    // Generate reports
    Report {
        #[command(subcommand)]
        action: ReportCommands,
    },

    // Get the next recommended task
    Next,

    // Show statistics
    Stats,

    // Test API connection (XAI/Grok)
    TestApi,

    // Generate documentation
    Docs {
        #[command(subcommand)]
        action: DocsAction,
    },

    // Refactoring assistant
    Refactor {
        #[command(subcommand)]
        action: RefactorAction,
    },

    // Manage repository cache
    Cache {
        #[command(subcommand)]
        action: CacheAction,
    },

    // GitHub integration
    Github {
        #[command(subcommand)]
        action: GithubCommands,
    },

    // Rust-native TODO pipeline (scan → scaffold → plan → work → sync)
    Todo {
        #[command(subcommand)]
        action: TodoCommands,
    },
}

// ============================================================================
// Todo Pipeline Subcommands
// ============================================================================

#[derive(Subcommand)]
enum TodoCommands {
    // STEP 0 — Scan source code for inline TODO/FIXME/HACK/XXX comments
    //
    // Walks the repo tree and extracts every TODO-style comment into
    // structured JSON.  No LLM calls — purely static.
    //
    // Examples:
    //   rustcode todo scan .
    //   rustcode todo scan . --json
    //   rustcode todo scan . --filter high --output scan.json
    Scan {
        // Path to the repository root (default: current directory)
        #[arg(default_value = ".")]
        repo: String,

        // Emit raw JSON instead of a human-readable table
        #[arg(long)]
        json: bool,

        // Minimum priority to include: low | medium | high
        #[arg(long, default_value = "low")]
        filter: String,

        // Write output to a file instead of stdout
        #[arg(short, long)]
        output: Option<String>,
    },

    // STEP 1 — Scaffold files/folders/stubs described in todo.md
    //
    // Reads todo.md, asks the LLM which files/dirs need to exist, creates
    // stubs on disk, and writes a "Scaffolded files" section back into
    // todo.md.  Idempotent — safe to re-run.
    //
    // Examples:
    //   rustcode todo scaffold .
    //   rustcode todo scaffold . --dry-run
    //   rustcode todo scaffold . --overwrite --output scaffold.json
    Scaffold {
        // Path to the repository root (default: current directory)
        #[arg(default_value = ".")]
        repo: String,

        // Preview what would be created without touching the file system
        #[arg(long)]
        dry_run: bool,

        // Overwrite existing stubs (default: skip)
        #[arg(long)]
        overwrite: bool,

        // Write the `ScaffoldPlan` JSON to a file
        #[arg(short, long)]
        output: Option<String>,
    },

    // STEP 2 — Generate a batched GAMEPLAN from todo.md via the LLM
    //
    // Reads todo.md plus optional source-context snippets and asks the
    // LLM to produce a prioritised, dependency-aware `GamePlan` JSON.
    //
    // Examples:
    //   rustcode todo plan todo.md
    //   rustcode todo plan todo.md --context . --output .rustcode/gameplan.json
    Plan {
        // Path to todo.md
        todo_md: String,

        // Optional repo root for source-context snippets
        #[arg(long)]
        context: Option<String>,

        // Write the `GamePlan` JSON to this file
        #[arg(short, long)]
        output: Option<String>,
    },

    // STEP 3 — Execute a single batch from a `GamePlan`
    //
    // Reads a `GamePlan` JSON (or a single batch JSON), calls the LLM to
    // generate real code for each item, applies hunks/replacements to disk,
    // creates backups, and writes a `WorkResult` JSON.
    //
    // Examples:
    //   rustcode todo work .rustcode/gameplan.json --batch batch-001 --dry-run
    //   rustcode todo work .rustcode/gameplan.json --batch batch-001
    //   rustcode todo work .rustcode/gameplan.json --batch batch-001 --auto-sync
    Work {
        // Path to a `GamePlan` JSON file (or a single-batch JSON)
        gameplan: String,

        // Batch ID to execute (required when file is a full `GamePlan`)
        #[arg(long)]
        batch: Option<String>,

        // Dry-run: build prompts and plan changes but do not write to disk
        #[arg(long)]
        dry_run: bool,

        // Root of the repository being modified (default: current directory)
        #[arg(long, default_value = ".")]
        repo: String,

        // Skip the post-work `cargo check` compile verification and automatic
        // rollback on failure.  By default a check is run after applying
        // changes; pass this flag to skip it (e.g. for non-Rust repos or
        // when you want to review changes manually before checking).
        #[arg(long)]
        no_check: bool,

        // Automatically run `todo sync` after a successful work + compile-check
        // pass, eliminating the manual step 4 invocation.
        // The todo.md path defaults to `<repo>/todo.md`.
        #[arg(long)]
        auto_sync: bool,

        // Path to todo.md used by --auto-sync (default: <repo>/todo.md)
        #[arg(long)]
        todo_md: Option<String>,
    },

    // STEP 4 — Apply a `WorkResult` back to todo.md (update status markers)
    //
    // Reads a `WorkResult` JSON produced by `todo work` and updates the
    // corresponding todo.md items with ✅ / ⚠️ / ❌ status markers.
    //
    // Examples:
    //   rustcode todo sync todo.md .rustcode/results/batch-001.json
    //   rustcode todo sync todo.md result.json --dry-run --append-summary
    Sync {
        // Path to todo.md
        todo_md: String,

        // Path to the `WorkResult` JSON produced by `todo work`
        results: String,

        // Preview changes without writing todo.md
        #[arg(long)]
        dry_run: bool,

        // Append a human-readable summary section to todo.md
        #[arg(long)]
        append_summary: bool,
    },
}

#[derive(Subcommand)]
enum NoteAction {
    // Add a new note
    Add {
        // Note content
        content: String,

        // Tags (comma-separated)
        #[arg(short, long)]
        tags: Option<String>,

        // Project name
        #[arg(short, long)]
        project: Option<String>,
    },

    // List notes
    List {
        // Maximum number of notes to show
        #[arg(short, long, default_value = "10")]
        limit: i64,

        // Filter by status (inbox, processed, archived)
        #[arg(short, long)]
        status: Option<String>,

        // Filter by project
        #[arg(short, long)]
        project: Option<String>,

        // Filter by tag
        #[arg(long)]
        tag: Option<String>,
    },

    // Search notes
    Search {
        // Search query
        query: String,

        // Maximum results
        #[arg(short, long, default_value = "10")]
        limit: i64,
    },
}

#[derive(Subcommand)]
enum RepoAction {
    // Add a repository to track
    Add {
        // Path to repository
        path: String,

        // Display name (defaults to directory name)
        #[arg(short, long)]
        name: Option<String>,
    },

    // List tracked repositories
    List,

    // Remove a repository
    Remove {
        // Repository ID
        id: String,
    },

    // Enable auto-scanning for a repository
    EnableAutoScan {
        // Repository ID or path
        repo: String,

        // Scan interval in minutes (default: 60)
        #[arg(short, long)]
        interval: Option<i64>,
    },

    // Disable auto-scanning for a repository
    DisableAutoScan {
        // Repository ID or path
        repo: String,
    },

    // Force an immediate scan check
    ForceScan {
        // Repository ID or path
        repo: String,
    },
}

#[derive(Subcommand)]
enum DocsAction {
    // Generate documentation for a module/file
    Module {
        // File path
        file: String,

        // Output file (prints to stdout if not specified)
        #[arg(short, long)]
        output: Option<String>,
    },

    // Generate README for repository
    Readme {
        // Repository path
        #[arg(default_value = ".")]
        repo: String,

        // Output file (prints to stdout if not specified)
        #[arg(short, long)]
        output: Option<String>,
    },
}

#[derive(Subcommand)]
enum RefactorAction {
    // Analyze a file for refactoring opportunities
    Analyze {
        // File path to analyze
        file: String,
    },

    // Generate refactoring plan for a file
    Plan {
        // File path
        file: String,

        // Specific smell ID to focus on (optional)
        #[arg(short, long)]
        smell: Option<String>,
    },
}

#[derive(Subcommand)]
enum CacheAction {
    // Initialize cache structure in a repository
    Init {
        // Repository path (defaults to current directory)
        #[arg(short, long)]
        path: Option<String>,
    },

    // Show cache status and statistics
    Status {
        // Repository path (defaults to current directory)
        #[arg(short, long)]
        path: Option<String>,
    },

    // Clear cache entries
    Clear {
        // Repository path (defaults to current directory)
        #[arg(short, long)]
        path: Option<String>,

        // Cache type to clear (analysis, docs, refactor, todos)
        #[arg(short = 't', long)]
        cache_type: Option<String>,

        // Clear all cache types
        #[arg(short, long)]
        all: bool,
    },

    // Migrate cache from JSON to `SQLite`
    Migrate {
        // Source path (JSON cache directory)
        #[arg(short, long)]
        source: Option<String>,

        // Destination path (`SQLite` database file)
        #[arg(short, long)]
        destination: Option<String>,

        // Create backup before migration
        #[arg(short, long)]
        backup: bool,

        // Verify migration after completion
        #[arg(short, long)]
        verify: bool,
    },
}

#[derive(Subcommand)]
enum TaskAction {
    // List tasks
    List {
        // Maximum number of tasks
        #[arg(short, long, default_value = "20")]
        limit: i64,

        // Filter by status (pending, `in_progress`, done)
        #[arg(short, long)]
        status: Option<String>,

        // Filter by max priority (1=critical, 2=high, 3=medium, 4=low)
        #[arg(short, long)]
        priority: Option<i32>,
    },

    // Mark a task as done
    Done {
        // Task ID
        id: String,
    },

    // Start working on a task
    Start {
        // Task ID
        id: String,
    },
}

// ============================================================================
// Main
// ============================================================================

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Load environment
    dotenvy::dotenv().ok();

    // Initialize tracing for debug logging
    tracing_subscriber::fmt::init();

    let cli = Cli::parse();

    // Get database URL
    let database_url = std::env::var("DATABASE_URL").unwrap_or_else(|_| {
        "postgresql://rustcode:changeme@localhost:5432/rustcode.db".into()
    });

    // Initialize database
    let pool = db::init_db(&database_url).await?;

    match cli.command {
        Commands::Note { action } => handle_note_action(&pool, action).await?,
        Commands::Repo { action } => handle_repo_action(&pool, action).await?,
        Commands::Tasks { action } => handle_task_action(&pool, action).await?,
        Commands::Queue { action } => handle_queue_command(&pool, action).await?,
        Commands::Scan { action } => handle_scan_command(&pool, action).await?,
        Commands::Report { action } => handle_report_command(&pool, action).await?,
        Commands::Next => handle_next(&pool).await?,
        Commands::Stats => handle_stats(&pool).await?,
        Commands::TestApi => handle_test_api(&pool).await?,
        Commands::Docs { action } => handle_docs_action(&pool, action).await?,
        Commands::Refactor { action } => handle_refactor_action(&pool, action).await?,
        Commands::Cache { action } => handle_cache_action(action).await?,
        Commands::Github { action } => handle_github_command(action, &pool).await?,
        Commands::Todo { action } => handle_todo_command(action, &pool).await?,
    }

    Ok(())
}

// ============================================================================
// Todo Pipeline Handlers
// ============================================================================

async fn handle_todo_command(action: TodoCommands, pool: &sqlx::PgPool) -> anyhow::Result<()> {
    match action {
        TodoCommands::Scan {
            repo,
            json,
            filter,
            output,
        } => handle_todo_scan(repo, json, filter, output).await,

        TodoCommands::Scaffold {
            repo,
            dry_run,
            overwrite,
            output,
        } => handle_todo_scaffold(repo, dry_run, overwrite, output, pool).await,

        TodoCommands::Plan {
            todo_md,
            context,
            output,
        } => handle_todo_plan(todo_md, context, output, pool).await,

        TodoCommands::Work {
            gameplan,
            batch,
            dry_run,
            repo,
            no_check,
            auto_sync,
            todo_md,
        } => {
            handle_todo_work(
                gameplan, batch, dry_run, no_check, auto_sync, todo_md, repo, pool,
            )
            .await
        }

        TodoCommands::Sync {
            todo_md,
            results,
            dry_run,
            append_summary,
        } => handle_todo_sync(todo_md, results, dry_run, append_summary).await,
    }
}

// ---------------------------------------------------------------------------
// todo scan
// ---------------------------------------------------------------------------

async fn handle_todo_scan(
    repo: String,
    json: bool,
    filter: String,
    output: Option<String>,
) -> anyhow::Result<()> {
    use rustcode::todo::CommentPriority;

    let repo_path = std::path::Path::new(&repo)
        .canonicalize()
        .unwrap_or_else(|_| std::path::PathBuf::from(&repo));

    let config = ScanConfig {
        relative_paths: true,
        ..ScanConfig::default()
    };

    let scanner = TodoCommentScanner::with_config(config)?;

    eprintln!(
        "{}  Scanning {}…",
        "🔍".bold(),
        repo_path.display().to_string().cyan()
    );

    let scan_output = scanner.scan_repo(&repo_path)?;

    // Apply priority filter
    let min_priority = match filter.to_lowercase().as_str() {
        "high" => CommentPriority::High,
        "medium" => CommentPriority::Medium,
        _ => CommentPriority::Low,
    };
    let filtered_items = scan_output.filter_by_priority(min_priority);

    let rendered = if json {
        // Re-use the full ScanOutput serialisation but only emit the filtered items
        let filtered_output = rustcode::todo::ScanOutput {
            repo_path: scan_output.repo_path.clone(),
            scanned_at: scan_output.scanned_at,
            total_files_scanned: scan_output.total_files_scanned,
            items: filtered_items.iter().map(|i| (*i).clone()).collect(),
            summary: scan_output.summary.clone(),
        };
        filtered_output.to_json_pretty()?
    } else {
        render_scan_table_items(&filtered_items)
    };

    if let Some(out_path) = output {
        std::fs::write(&out_path, &rendered)?;
        eprintln!("{}  Wrote scan output → {}", "✅".bold(), out_path.green());
    } else {
        println!("{rendered}");
    }

    eprintln!(
        "\n{}  {} item(s) shown ({} total) across {} file(s) ({} high / {} medium / {} low)",
        "📊".bold(),
        filtered_items.len().to_string().bold(),
        scan_output.items.len().to_string().dimmed(),
        scan_output.summary.files_with_todos.to_string().cyan(),
        scan_output
            .summary
            .by_priority
            .get("high")
            .copied()
            .unwrap_or(0)
            .to_string()
            .red(),
        scan_output
            .summary
            .by_priority
            .get("medium")
            .copied()
            .unwrap_or(0)
            .to_string()
            .yellow(),
        scan_output
            .summary
            .by_priority
            .get("low")
            .copied()
            .unwrap_or(0)
            .to_string()
            .green(),
    );

    Ok(())
}

// Render a filtered list of scan items as a coloured human-readable table.
fn render_scan_table_items(items: &[&rustcode::todo::TodoCommentItem]) -> String {
    use std::fmt::Write as FmtWrite;

    let mut buf = String::new();
    let _ = writeln!(
        buf,
        "\n{:<8}  {:<12}  {:<40}  {}",
        "PRIORITY".bold(),
        "KIND".bold(),
        "FILE:LINE".bold(),
        "TEXT".bold()
    );
    let _ = writeln!(buf, "{}", "─".repeat(100).dimmed());

    for item in items {
        let priority_col = match item.priority {
            rustcode::todo::CommentPriority::High => "high".red().bold().to_string(),
            rustcode::todo::CommentPriority::Medium => "medium".yellow().to_string(),
            rustcode::todo::CommentPriority::Low => "low".green().to_string(),
        };
        let loc = format!("{}:{}", item.file.display(), item.line);
        let text = if item.text.chars().count() > 60 {
            let truncated: String = item.text.chars().take(57).collect();
            format!("{truncated}…")
        } else {
            item.text.clone()
        };
        let _ = writeln!(
            buf,
            "{:<17}  {:<12}  {:<40}  {}",
            priority_col,
            item.kind.as_str().cyan(),
            loc.dimmed(),
            text
        );
    }

    buf
}

// ---------------------------------------------------------------------------
// todo scaffold
// ---------------------------------------------------------------------------

async fn handle_todo_scaffold(
    repo: String,
    dry_run: bool,
    overwrite: bool,
    output: Option<String>,
    pool: &sqlx::PgPool,
) -> anyhow::Result<()> {
    let repo_path = std::path::Path::new(&repo)
        .canonicalize()
        .unwrap_or_else(|_| std::path::PathBuf::from(&repo));

    let config = ScaffoldConfig {
        dry_run,
        overwrite,
        ..ScaffoldConfig::default()
    };

    let db = rustcode::db::Database::from_pool(pool.clone());

    if dry_run {
        eprintln!(
            "{}  [dry-run] Scaffolding {}…",
            "🔧".bold(),
            repo_path.display().to_string().cyan()
        );
    } else {
        eprintln!(
            "{}  Scaffolding {}…",
            "🔧".bold(),
            repo_path.display().to_string().cyan()
        );
    }

    let scaffolder = TodoScaffolder::from_env(config, db).await?;
    let result = scaffolder.scaffold(&repo_path).await?;

    // Optionally write the plan to disk
    if let Some(out_path) = &output {
        let plan_json = result.plan.to_json_pretty()?;
        std::fs::write(out_path, &plan_json)?;
        eprintln!(
            "{}  Wrote scaffold plan → {}",
            "📄".bold(),
            out_path.green()
        );
    }

    result.print_summary();

    Ok(())
}

// ---------------------------------------------------------------------------
// todo plan
// ---------------------------------------------------------------------------

async fn handle_todo_plan(
    todo_md: String,
    context: Option<String>,
    output: Option<String>,
    pool: &sqlx::PgPool,
) -> anyhow::Result<()> {
    let todo_path = std::path::PathBuf::from(&todo_md);
    let source_root = context.as_deref().map(std::path::Path::new);

    let config = PlannerConfig::default();
    let db = rustcode::db::Database::from_pool(pool.clone());

    eprintln!(
        "{}  Planning from {}…",
        "📋".bold(),
        todo_path.display().to_string().cyan()
    );

    let planner = TodoPlanner::from_env(config, db).await?;
    let gameplan = planner.plan(&todo_path, source_root).await?;

    let json = gameplan.to_json_pretty()?;

    if let Some(out_path) = &output {
        // Ensure parent directory exists
        if let Some(parent) = std::path::Path::new(out_path).parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(out_path, &json)?;
        eprintln!("{}  Wrote gameplan → {}", "✅".bold(), out_path.green());
    } else {
        println!("{json}");
    }

    // Human-readable summary
    eprintln!(
        "\n{}  {} batch(es), {} item(s) planned, {} item(s) skipped",
        "📊".bold(),
        gameplan.batches.len().to_string().bold(),
        gameplan.total_items_planned.to_string().cyan(),
        gameplan.skipped_items.len().to_string().yellow(),
    );

    for batch in gameplan.ordered_batches() {
        eprintln!(
            "   {} [{}] {} — {} item(s), effort: {}",
            "▸".dimmed(),
            batch.id.cyan(),
            batch.title.bold(),
            batch.items.len().to_string().yellow(),
            batch.estimated_effort.to_string().green(),
        );
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// todo work
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
async fn handle_todo_work(
    gameplan: String,
    batch: Option<String>,
    dry_run: bool,
    no_check: bool,
    auto_sync: bool,
    auto_sync_todo_md: Option<String>,
    repo: String,
    pool: &sqlx::PgPool,
) -> anyhow::Result<()> {
    let repo_path = std::path::Path::new(&repo)
        .canonicalize()
        .unwrap_or_else(|_| std::path::PathBuf::from(&repo));

    // Load the work batch — either from a standalone batch JSON or from a full GamePlan
    let work_batch: WorkBatch = if let Some(batch_id) = &batch {
        WorkBatch::load_from_gameplan(std::path::Path::new(&gameplan), batch_id)?
    } else {
        WorkBatch::load(std::path::Path::new(&gameplan))?
    };

    let mut config = WorkConfig::for_repo(&repo_path).with_skip_todo_md_update();
    if dry_run {
        config = config.as_dry_run();
    }

    let db = rustcode::db::Database::from_pool(pool.clone());

    if dry_run {
        eprintln!(
            "{}  [dry-run] Executing batch {} in {}…",
            "⚙️ ".bold(),
            work_batch.batch.id.cyan(),
            repo_path.display().to_string().cyan()
        );
    } else {
        eprintln!(
            "{}  Executing batch {} in {}…",
            "⚙️ ".bold(),
            work_batch.batch.id.cyan(),
            repo_path.display().to_string().cyan()
        );
    }

    // Snapshot the files the batch intends to touch *before* applying changes
    // so we can roll back if the post-work compile check fails.
    let pre_snapshots: std::collections::HashMap<String, Option<Vec<u8>>> = if dry_run {
        Default::default()
    } else {
        work_batch
            .batch
            .items
            .iter()
            .flat_map(|i| i.files.iter())
            .map(|f| {
                let abs = repo_path.join(f);
                let contents = std::fs::read(&abs).ok();
                (f.clone(), contents)
            })
            .collect()
    };

    let worker = TodoWorker::from_env(config, db).await?;
    let result = worker.execute(&work_batch).await?;

    // -----------------------------------------------------------------------
    // Post-work compile check (Rust repos only, skipped with --no-check or
    // --dry-run).  Detects whether the LLM-generated changes introduced any
    // compile errors and, if so, rolls every touched file back to its
    // pre-change snapshot.
    // -----------------------------------------------------------------------
    let check_passed = if !dry_run && !no_check && result.items_succeeded > 0 {
        let is_rust_repo = repo_path.join("Cargo.toml").exists();
        if is_rust_repo {
            eprintln!("\n{}  Running compile check on changed files…", "🔬".bold());

            let check_output = std::process::Command::new("cargo")
                .arg("check")
                .env("SQLX_OFFLINE", "true")
                // Suppress the full warning wall — we only care about errors.
                .env("RUSTFLAGS", "-A warnings")
                .current_dir(&repo_path)
                .output();

            match check_output {
                Ok(out) if out.status.success() => {
                    eprintln!("{}  Compile check passed ✅", "🔬".bold());
                    true
                }
                Ok(out) => {
                    let stderr = String::from_utf8_lossy(&out.stderr);
                    // Extract just the error lines for a concise report.
                    let error_lines: Vec<&str> = stderr
                        .lines()
                        .filter(|l| l.starts_with("error") || l.contains('^'))
                        .take(20)
                        .collect();

                    eprintln!(
                        "\n{}  Compile check FAILED — rolling back {} touched file(s)…",
                        "🔴".bold(),
                        pre_snapshots.len()
                    );

                    // Print the first batch of errors so the user can review.
                    for line in &error_lines {
                        eprintln!("   {}", line.red());
                    }
                    if stderr.lines().filter(|l| l.starts_with("error")).count() > 20 {
                        eprintln!(
                            "   {} (truncated — run `cargo check` for full output)",
                            "…".dimmed()
                        );
                    }

                    // Roll back every file the batch touched.
                    let mut rolled_back = 0usize;
                    let mut rollback_errors = Vec::new();

                    for (rel_path, maybe_original) in &pre_snapshots {
                        let abs = repo_path.join(rel_path);
                        match maybe_original {
                            Some(original_bytes) => {
                                // File existed before — restore it.
                                match std::fs::write(&abs, original_bytes) {
                                    Ok(()) => {
                                        eprintln!("   ↩  Restored {}", rel_path.yellow());
                                        rolled_back += 1;
                                    }
                                    Err(e) => {
                                        rollback_errors
                                            .push(format!("  restore {rel_path}: {e}"));
                                    }
                                }
                            }
                            None => {
                                // File was newly created — remove it.
                                if abs.exists() {
                                    match std::fs::remove_file(&abs) {
                                        Ok(()) => {
                                            eprintln!(
                                                "   ↩  Removed new file {}",
                                                rel_path.yellow()
                                            );
                                            rolled_back += 1;
                                        }
                                        Err(e) => {
                                            rollback_errors
                                                .push(format!("  remove {rel_path}: {e}"));
                                        }
                                    }
                                }
                            }
                        }
                    }

                    eprintln!(
                        "{}  Rollback complete — {} file(s) restored. WorkResult NOT written.",
                        if rollback_errors.is_empty() {
                            "↩ ".bold()
                        } else {
                            "⚠️ ".bold()
                        },
                        rolled_back
                    );

                    if !rollback_errors.is_empty() {
                        eprintln!("{}  Some rollbacks failed:", "⚠️ ".bold());
                        for e in &rollback_errors {
                            eprintln!("   • {}", e.red());
                        }
                        eprintln!(
                            "{}  Check {} for manual restoration.",
                            "💡".bold(),
                            repo_path
                                .join(".rustcode/backups")
                                .display()
                                .to_string()
                                .cyan()
                        );
                    }

                    false
                }
                Err(e) => {
                    // cargo not on PATH or some other OS error — warn but don't
                    // fail the whole command, since this may be a non-Rust env.
                    eprintln!(
                        "{}  Could not run `cargo check` ({}). Skipping compile verification.",
                        "⚠️ ".bold(),
                        e
                    );
                    true // treat as passed so we still write the result
                }
            }
        } else {
            // Not a Rust repo — skip compile check silently.
            true
        }
    } else {
        true
    };

    // Only persist the WorkResult when the compile check passed (or was skipped).
    if check_passed {
        let results_dir = repo_path.join(".rustcode").join("results");
        std::fs::create_dir_all(&results_dir)?;
        let result_path = results_dir.join(format!("{}.json", result.batch_id));
        let result_path_str = result_path.display().to_string();
        let result_json = result.to_json_pretty()?;

        if !dry_run {
            std::fs::write(&result_path, &result_json)?;
            eprintln!(
                "{}  Wrote work result → {}",
                "📄".bold(),
                result_path_str.green()
            );
        }

        // ------------------------------------------------------------------
        // Auto-sync: automatically run `todo sync` after a successful work
        // + compile-check pass so the caller doesn't need to invoke step 4
        // manually.  Only runs when --auto-sync is set and not in dry-run.
        // ------------------------------------------------------------------
        if auto_sync && !dry_run && result.items_succeeded > 0 {
            let todo_md_path = auto_sync_todo_md.map_or_else(|| repo_path.join("todo.md"), std::path::PathBuf::from);

            if todo_md_path.exists() {
                eprintln!(
                    "\n{}  --auto-sync: running `todo sync {} {}`…",
                    "🔄".bold(),
                    todo_md_path.display().to_string().cyan(),
                    result_path_str.green(),
                );

                match handle_todo_sync(
                    todo_md_path.to_string_lossy().to_string(),
                    result_path_str.clone(),
                    false, // not dry-run
                    false, // no append-summary (keep it clean)
                )
                .await
                {
                    Ok(()) => eprintln!("{}  auto-sync complete ✅", "🔄".bold()),
                    Err(e) => eprintln!(
                        "{}  auto-sync failed ({}). Run manually: rustcode todo sync {} {}",
                        "⚠️ ".bold(),
                        e,
                        todo_md_path.display().to_string().cyan(),
                        result_path_str.green(),
                    ),
                }
            } else {
                eprintln!(
                    "{}  --auto-sync: todo.md not found at {} — skipping sync step",
                    "⚠️ ".bold(),
                    todo_md_path.display().to_string().yellow(),
                );
                eprintln!(
                    "{}  Pass --todo-md <path> to specify the todo.md location.",
                    "💡".bold()
                );
            }
        }

        // Human-readable summary
        let status_icon = if result.is_fully_successful() {
            "✅".to_string()
        } else if result.items_failed > 0 {
            "⚠️ ".to_string()
        } else {
            "ℹ️ ".to_string()
        };

        eprintln!(
            "\n{}  batch {} — {} succeeded / {} failed / {} skipped",
            status_icon,
            result.batch_id.cyan(),
            result.items_succeeded.to_string().green(),
            result.items_failed.to_string().red(),
            result.items_skipped.to_string().yellow(),
        );

        for fc in &result.file_changes {
            eprintln!(
                "   {} {} (+{} / -{})",
                match fc.change_type {
                    rustcode::todo::FileChangeType::Created => "➕".to_string(),
                    rustcode::todo::FileChangeType::Modified => "✏️ ".to_string(),
                    rustcode::todo::FileChangeType::Deleted => "🗑️ ".to_string(),
                },
                fc.file.cyan(),
                fc.lines_added.to_string().green(),
                fc.lines_removed.to_string().red(),
            );
        }

        if !result.errors.is_empty() {
            eprintln!("\n{}  Errors:", "❌".bold());
            for e in &result.errors {
                eprintln!("   • {}", e.red());
            }
        }

        if dry_run {
            eprintln!(
                "\n{}  Dry-run complete — no files were written.",
                "ℹ️ ".bold()
            );
            println!("{result_json}");
        } else if !auto_sync {
            // Only show the manual hint when --auto-sync was NOT used.
            eprintln!(
                "\n{}  Run `rustcode todo sync {} {}` to update todo.md",
                "💡".bold(),
                "todo.md".cyan(),
                result_path_str.green(),
            );
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// todo sync
// ---------------------------------------------------------------------------

async fn handle_todo_sync(
    todo_md: String,
    results: String,
    dry_run: bool,
    append_summary: bool,
) -> anyhow::Result<()> {
    let todo_path = std::path::PathBuf::from(&todo_md);
    let results_path = std::path::PathBuf::from(&results);

    let config = SyncConfig {
        dry_run,
        append_summary,
        ..SyncConfig::default()
    };

    if dry_run {
        eprintln!(
            "{}  [dry-run] Syncing {} ← {}…",
            "🔄".bold(),
            todo_path.display().to_string().cyan(),
            results_path.display().to_string().cyan()
        );
    } else {
        eprintln!(
            "{}  Syncing {} ← {}…",
            "🔄".bold(),
            todo_path.display().to_string().cyan(),
            results_path.display().to_string().cyan()
        );
    }

    let syncer = TodoSyncer::new(config);
    let sync_result = syncer.sync_from_file(&todo_path, &results_path)?;

    sync_result.print_summary();

    if dry_run {
        eprintln!(
            "\n{}  Dry-run complete — todo.md was not modified.",
            "ℹ️ ".bold()
        );
    }

    Ok(())
}

// ============================================================================
// Note Handlers
// ============================================================================

async fn handle_note_action(pool: &sqlx::PgPool, action: NoteAction) -> anyhow::Result<()> {
    match action {
        NoteAction::Add {
            content,
            tags,
            project,
        } => {
            let note = create_note(pool, &content, tags.as_deref(), project.as_deref()).await?;

            println!("{} Note created", "✓".green());
            println!("  {} {}", "ID:".dimmed(), note.id);
            println!("  {} {}", "Content:".dimmed(), note.content);
            if let Some(t) = &note.tags {
                println!("  {} {}", "Tags:".dimmed(), t);
            }
        }

        NoteAction::List {
            limit,
            status,
            project,
            tag,
        } => {
            let notes = list_notes(
                pool,
                limit,
                status.as_deref(),
                project.as_deref(),
                tag.as_deref(),
            )
            .await?;

            if notes.is_empty() {
                println!(
                    "{} No notes found. Add one with: {} note add \"Your note\"",
                    "📝".dimmed(),
                    "rustcode".cyan()
                );
            } else {
                println!("📝 Notes ({}):\n", notes.len());
                for note in notes {
                    print_note(&note);
                }
            }
        }

        NoteAction::Search { query, limit } => {
            let notes = search_notes(pool, &query, limit).await?;

            if notes.is_empty() {
                println!("{} No notes matching \"{}\"", "🔍".dimmed(), query);
            } else {
                println!("🔍 Found {} notes matching \"{}\":\n", notes.len(), query);
                for note in notes {
                    print_note(&note);
                }
            }
        }
    }

    Ok(())
}

fn print_note(note: &db::Note) {
    let status_icon = match note.status.as_str() {
        "inbox" => "📥",
        "processed" => "✅",
        "archived" => "📦",
        _ => "📝",
    };

    println!("  {} [{}] {}", status_icon, note.id.dimmed(), note.content);

    let mut meta = Vec::new();
    if let Some(tags) = &note.tags {
        meta.push(format!("tags: {tags}"));
    }

    if !meta.is_empty() {
        println!("     {}", meta.join(" | ").dimmed());
    }
    println!();
}

// ============================================================================
// Repo Handlers
// ============================================================================

async fn handle_repo_action(pool: &sqlx::PgPool, action: RepoAction) -> anyhow::Result<()> {
    match action {
        RepoAction::Add { path, name } => {
            // Expand and canonicalize path
            let expanded = shellexpand::tilde(&path).to_string();
            let canonical = std::fs::canonicalize(&expanded)?;
            let path_str = canonical.to_string_lossy().to_string();

            // Derive name from path if not provided
            let name = name.unwrap_or_else(|| {
                canonical
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("unnamed")
                    .to_string()
            });

            // Check if path exists
            if !canonical.exists() {
                anyhow::bail!("Path does not exist: {path_str}");
            }

            let repo = db::add_repository(pool, &path_str, &name, None).await?;

            println!("{} Repository added", "✓".green());
            println!("  {} {}", "ID:".dimmed(), repo.id);
            println!("  {} {}", "Name:".dimmed(), repo.name);
            println!("  {} {}", "Path:".dimmed(), repo.path);
        }

        RepoAction::List => {
            let repos = list_repositories(pool).await?;

            if repos.is_empty() {
                println!(
                    "{} No repositories tracked. Add one with: {} repo add <path>",
                    "📂".dimmed(),
                    "rustcode".cyan()
                );
            } else {
                println!("📂 Tracked repositories ({}):\n", repos.len());
                for repo in repos {
                    let analyzed = repo
                        .last_analyzed.map_or_else(|| "never".into(), |ts| {
                            chrono::DateTime::from_timestamp(ts, 0).map_or_else(|| "unknown".into(), |dt| dt.format("%Y-%m-%d %H:%M").to_string())
                        });

                    println!("  📁 {} ({})", repo.name.cyan(), repo.id.dimmed());
                    println!("     {} {}", "Path:".dimmed(), repo.path);
                    println!("     {} {}", "Analyzed:".dimmed(), analyzed);
                    println!();
                }
            }
        }

        RepoAction::Remove { id } => {
            db::remove_repository(pool, &id).await?;
            println!("{} Repository removed: {}", "✓".green(), id);
        }

        RepoAction::EnableAutoScan { repo, interval } => {
            // Resolve repo ID
            let repo_id = if repo.starts_with("gh-") || repo.len() == 36 {
                repo
            } else {
                // Try to find by path or name
                let repos = list_repositories(pool).await?;
                repos
                    .iter()
                    .find(|r| r.path == repo || r.name == repo)
                    .map(|r| r.id.clone())
                    .ok_or_else(|| anyhow::anyhow!("Repository not found: {repo}"))?
            };

            rustcode::auto_scanner::enable_auto_scan(pool, &repo_id, interval).await?;

            let interval_str = interval.unwrap_or(60);
            println!(
                "{} Auto-scan enabled for repository (interval: {} minutes)",
                "✓".green(),
                interval_str
            );
        }

        RepoAction::DisableAutoScan { repo } => {
            // Resolve repo ID
            let repo_id = if repo.starts_with("gh-") || repo.len() == 36 {
                repo
            } else {
                // Try to find by path or name
                let repos = list_repositories(pool).await?;
                repos
                    .iter()
                    .find(|r| r.path == repo || r.name == repo)
                    .map(|r| r.id.clone())
                    .ok_or_else(|| anyhow::anyhow!("Repository not found: {repo}"))?
            };

            rustcode::auto_scanner::disable_auto_scan(pool, &repo_id).await?;
            println!("{} Auto-scan disabled for repository", "✓".green());
        }

        RepoAction::ForceScan { repo } => {
            // Resolve repo ID
            let repo_id = if repo.starts_with("gh-") || repo.len() == 36 {
                repo
            } else {
                // Try to find by path or name
                let repos = list_repositories(pool).await?;
                repos
                    .iter()
                    .find(|r| r.path == repo || r.name == repo)
                    .map(|r| r.id.clone())
                    .ok_or_else(|| anyhow::anyhow!("Repository not found: {repo}"))?
            };

            rustcode::auto_scanner::force_scan(pool, &repo_id).await?;
            println!(
                "{} Forced scan check - will scan on next cycle",
                "✓".green()
            );
        }
    }

    Ok(())
}

// ============================================================================
// Task Handlers
// ============================================================================

async fn handle_task_action(pool: &sqlx::PgPool, action: TaskAction) -> anyhow::Result<()> {
    match action {
        TaskAction::List {
            limit,
            status,
            priority,
        } => {
            let tasks = list_tasks(pool, limit, status.as_deref(), priority, None).await?;

            if tasks.is_empty() {
                println!("{} No tasks found", "📋".dimmed());
            } else {
                println!("📋 Tasks ({}):\n", tasks.len());
                for task in tasks {
                    print_task(&task);
                }
            }
        }

        TaskAction::Done { id } => {
            update_task_status(pool, &id, "done").await?;
            println!("{} Task marked as done: {}", "✓".green(), id);
        }

        TaskAction::Start { id } => {
            update_task_status(pool, &id, "in_progress").await?;
            println!("{} Task started: {}", "▶".blue(), id);
        }
    }

    Ok(())
}

fn print_task(task: &db::Task) {
    let priority_icon = match task.priority {
        1 => "🔴",
        2 => "🟠",
        3 => "🟡",
        4 => "🟢",
        _ => "⚪",
    };

    let priority_label = match task.priority {
        1 => "CRITICAL",
        2 => "HIGH",
        3 => "MEDIUM",
        4 => "LOW",
        _ => "UNKNOWN",
    };

    let status_icon = match task.status.as_str() {
        "pending" => "⏳",
        "in_progress" => "▶️",
        "done" => "✅",
        _ => "❓",
    };

    println!(
        "  {} {} [{}] {}",
        priority_icon,
        status_icon,
        task.id.cyan(),
        task.title
    );
    println!("     {} {}", "Priority:".dimmed(), priority_label);

    if let Some(desc) = &task.description
        && !desc.is_empty() {
            println!("     {}", desc.dimmed());
        }

    if let Some(file) = &task.file_path {
        let line = task
            .line_number
            .map(|n| format!(":{n}"))
            .unwrap_or_default();
        println!("     {} {}{}", "File:".dimmed(), file, line);
    }

    println!();
}

// ============================================================================
// Other Handlers
// ============================================================================

async fn handle_next(pool: &sqlx::PgPool) -> anyhow::Result<()> {
    match get_next_task(pool).await? {
        Some(task) => {
            println!("🎯 Next recommended task:\n");
            print_task(&task);
            println!(
                "Start working on it: {} tasks start {}",
                "rustcode".cyan(),
                task.id
            );
        }
        None => {
            println!("🎉 No pending tasks! Time to relax or add some work.");
        }
    }

    Ok(())
}

async fn handle_stats(pool: &sqlx::PgPool) -> anyhow::Result<()> {
    let stats = get_stats(pool).await?;

    println!("📊 Rustassistant Statistics\n");
    println!("  {} {}", "Total notes:".dimmed(), stats.total_notes);
    println!("  {} {}", "Inbox notes:".dimmed(), stats.inbox_notes);
    println!("  {} {}", "Repositories:".dimmed(), stats.total_repos);
    println!("  {} {}", "Total tasks:".dimmed(), stats.total_tasks);
    println!("  {} {}", "Pending tasks:".dimmed(), stats.pending_tasks);

    Ok(())
}

async fn handle_test_api(pool: &sqlx::PgPool) -> anyhow::Result<()> {
    use rustcode::db::Database;
    use rustcode::grok_client::GrokClient;
    use std::time::Instant;

    println!("🔌 Testing XAI API connection...\n");

    // ── 1. Key presence check ──────────────────────────────────────────────
    let api_key = std::env::var("XAI_API_KEY").or_else(|_| std::env::var("GROK_API_KEY"));

    let key = match api_key {
        Ok(k) if !k.is_empty() => {
            println!("  {} XAI_API_KEY found", "✓".green());
            println!(
                "  {} Key prefix: {}...",
                "🔑".dimmed(),
                &k[..12.min(k.len())]
            );
            k
        }
        _ => {
            println!("  {} XAI_API_KEY not set", "✗".red());
            println!(
                "\n  Set it in your .env file or environment:\n  export XAI_API_KEY=xai-your-key-here"
            );
            return Ok(());
        }
    };

    // ── 2. Model in use ────────────────────────────────────────────────────
    let model =
        std::env::var("XAI_MODEL").unwrap_or_else(|_| "grok-4-1-fast-reasoning".to_string());
    println!("  {} Model: {}", "🤖".dimmed(), model.cyan());

    // ── 3. Build client ────────────────────────────────────────────────────
    println!("\n  Building GrokClient...");
    let db = Database::from_pool(pool.clone());
    let client = GrokClient::new(key, db);

    // ── 4. Ping round-trip ─────────────────────────────────────────────────
    println!("  Sending ping (\"reply with: ok\")...\n");
    let start = Instant::now();

    match client
        .ask_tracked("reply with: ok", None, "test-api-ping")
        .await
    {
        Ok(resp) => {
            let elapsed_ms = start.elapsed().as_millis();

            println!("  {} API responded successfully", "✓".green());
            println!(
                "  {} Reply:      {}",
                "💬".dimmed(),
                resp.content.trim().cyan()
            );
            println!(
                "  {} Latency:    {} ms",
                "⏱".dimmed(),
                elapsed_ms.to_string().yellow()
            );
            println!(
                "  {} Tokens:     {} prompt + {} completion = {} total",
                "📊".dimmed(),
                resp.prompt_tokens,
                resp.completion_tokens,
                resp.total_tokens
            );
            println!("  {} Est. cost:  ${:.6} USD", "💰".dimmed(), resp.cost_usd);
            println!(
                "\n  {} XAI API is reachable and accepting requests.",
                "🎉".green()
            );
        }
        Err(e) => {
            let elapsed_ms = start.elapsed().as_millis();
            println!("  {} API call failed after {} ms", "✗".red(), elapsed_ms);
            println!("  {} Error: {}", "⚠".yellow(), e);

            // Give a hint for the most common failure modes.
            let err_str = e.to_string().to_lowercase();
            if err_str.contains("401")
                || err_str.contains("unauthorized")
                || err_str.contains("invalid")
            {
                println!(
                    "\n  {} Your API key was rejected — check that XAI_API_KEY is correct.",
                    "ℹ".blue()
                );
            } else if err_str.contains("429") || err_str.contains("rate") {
                println!(
                    "\n  {} Rate limit hit — wait a moment and try again.",
                    "ℹ".blue()
                );
            } else if err_str.contains("timeout") || err_str.contains("connect") {
                println!(
                    "\n  {} Network error — check connectivity to api.x.ai.",
                    "ℹ".blue()
                );
            }

            return Err(e);
        }
    }

    Ok(())
}

async fn handle_refactor_action(pool: &sqlx::PgPool, action: RefactorAction) -> anyhow::Result<()> {
    use rustcode::db::Database;
    use rustcode::refactor_assistant::{RefactorAssistant, SmellSeverity};

    let db = Database::from_pool(pool.clone());
    let assistant = RefactorAssistant::new(db).await?;

    match action {
        RefactorAction::Analyze { file } => {
            // Use SQLite cache organized by repo in XDG cache directory
            let repo_path = std::env::current_dir()?;
            let cache = RepoCacheSql::new_for_repo(&repo_path).await?;
            let repo_path_str = repo_path.to_string_lossy().to_string();

            // Read file content for cache checking
            let file_content = std::fs::read_to_string(&file)?;

            // Check cache first
            let analysis = if let Some(cached) = cache
                .get(
                    CacheType::Refactor,
                    &file,
                    &file_content,
                    "xai",
                    "grok-beta",
                    None,
                    None,
                )
                .await?
            {
                println!("📦 Using cached analysis for {file}\n");
                serde_json::from_value(cached)?
            } else {
                println!("🔍 Analyzing {file} for refactoring opportunities...\n");
                let analysis = assistant.analyze_file(&file).await?;

                // Cache the result
                let result_json = serde_json::to_value(&analysis)?;
                cache
                    .set(SqlCacheSetParams {
                        cache_type: CacheType::Refactor,
                        repo_path: &repo_path_str,
                        file_path: &file,
                        content: &file_content,
                        provider: "xai",
                        model: "grok-beta",
                        result: result_json,
                        tokens_used: analysis.tokens_used,
                        prompt_hash: None,
                        schema_version: None,
                    })
                    .await?;

                if let Some(tokens) = analysis.tokens_used {
                    println!("💾 Analysis cached (tokens used: {tokens})\n");
                } else {
                    println!("💾 Analysis cached\n");
                }

                analysis
            };

            println!("📊 Refactoring Analysis:\n");
            println!("  {} {}", "File:".dimmed(), file);
            println!(
                "  {} {}",
                "Code Smells Found:".dimmed(),
                analysis.code_smells.len()
            );
            println!();

            if analysis.code_smells.is_empty() {
                println!("{} No code smells detected! Code looks good.", "✓".green());
            } else {
                for smell in &analysis.code_smells {
                    let severity_icon = match smell.severity {
                        SmellSeverity::Critical => "🔴",
                        SmellSeverity::High => "🟠",
                        SmellSeverity::Medium => "🟡",
                        SmellSeverity::Low => "🟢",
                    };

                    let location = if let Some(ref loc) = smell.location {
                        if let Some(line) = loc.line_start {
                            format!("Line {line}")
                        } else {
                            "Unknown location".to_string()
                        }
                    } else {
                        "Unknown location".to_string()
                    };
                    println!("  {} {:?} ({})", severity_icon, smell.smell_type, location);
                    println!("     {}", smell.description);
                    println!();
                }
            }

            if !analysis.suggestions.is_empty() {
                println!("💡 Refactoring Suggestions:");
                for (i, suggestion) in analysis.suggestions.iter().enumerate() {
                    println!(
                        "  {}. {} ({:?})",
                        i + 1,
                        suggestion.title,
                        suggestion.refactoring_type
                    );
                    println!("     {}", suggestion.description);
                    println!();
                }

                println!(
                    "\nGenerate a detailed plan with: {} refactor plan {}",
                    "rustcode".cyan(),
                    file
                );
            }
        }

        RefactorAction::Plan { file, smell: _ } => {
            println!("📋 Generating refactoring plan for {file}...\n");

            let analysis = assistant.analyze_file(&file).await?;

            if analysis.code_smells.is_empty() {
                println!("{} No code smells found. Nothing to refactor!", "✓".green());
                return Ok(());
            }

            // For now, just use the file path to generate plan
            // The generate_plan method will analyze and create a comprehensive plan
            let plan = assistant.generate_plan(&file, "").await?;

            println!("📋 Refactoring Plan:\n");
            println!("  {} {}", "Title:".dimmed(), plan.title);
            println!("  {} {}", "Goal:".dimmed(), plan.goal);
            println!("  {} {:?}", "Estimated Effort:".dimmed(), plan.total_effort);
            println!("  {} {}", "Files:".dimmed(), plan.files.join(", "));
            println!();

            if !plan.steps.is_empty() {
                println!("Steps:");
                for step in &plan.steps {
                    println!("  {}. {}", step.step_number, step.description);
                    println!("     Effort: {:?}", step.effort);
                    if !step.affected_files.is_empty() {
                        println!("     Files: {}", step.affected_files.join(", "));
                    }
                }
                println!();
            }

            if !plan.risks.is_empty() {
                println!("⚠️  Risks:");
                for risk in &plan.risks {
                    println!("  • {} ({})", risk.description, risk.mitigation);
                }
                println!();
            }

            if !plan.benefits.is_empty() {
                println!("✨ Benefits:");
                for benefit in &plan.benefits {
                    println!("  • {benefit}");
                }
                println!();
            }
        }
    }

    Ok(())
}

async fn handle_docs_action(pool: &sqlx::PgPool, action: DocsAction) -> anyhow::Result<()> {
    use rustcode::db::Database;
    use rustcode::doc_generator::DocGenerator;

    let db = Database::from_pool(pool.clone());
    let generator = DocGenerator::new(db).await?;

    match action {
        DocsAction::Module { file, output } => {
            // Use SQLite cache organized by repo in XDG cache directory
            let repo_path = std::env::current_dir()?;
            let cache = RepoCacheSql::new_for_repo(&repo_path).await?;
            let repo_path_str = repo_path.to_string_lossy().to_string();

            // Read file content for cache checking
            let file_content = std::fs::read_to_string(&file)?;

            // Check cache first
            let doc = if let Some(cached) = cache
                .get(
                    CacheType::Docs,
                    &file,
                    &file_content,
                    "xai",
                    "grok-beta",
                    None,
                    None,
                )
                .await?
            {
                println!("📦 Using cached documentation for {file}\n");
                serde_json::from_value(cached)?
            } else {
                println!("📝 Generating documentation for {file}...\n");
                let doc = generator.generate_module_docs(&file).await?;

                // Cache the result
                let result_json = serde_json::to_value(&doc)?;
                cache
                    .set(SqlCacheSetParams {
                        cache_type: CacheType::Docs,
                        repo_path: &repo_path_str,
                        file_path: &file,
                        content: &file_content,
                        provider: "xai",
                        model: "grok-beta",
                        result: result_json,
                        tokens_used: None,
                        prompt_hash: None,
                        schema_version: None,
                    })
                    .await?;
                println!("💾 Documentation cached\n");

                doc
            };

            let markdown = generator.format_module_doc(&doc);

            if let Some(output_path) = output {
                std::fs::write(&output_path, &markdown)?;
                println!("{} Documentation written to {}", "✓".green(), output_path);
            } else {
                println!("{markdown}");
            }
        }

        DocsAction::Readme { repo, output } => {
            println!("📖 Generating README for {repo}...\n");

            let content = generator.generate_readme(&repo).await?;
            let markdown = generator.format_readme(&content);

            if let Some(output_path) = output {
                std::fs::write(&output_path, &markdown)?;
                println!("{} README written to {}", "✓".green(), output_path);
            } else {
                println!("{markdown}");
            }
        }
    }

    Ok(())
}

// ============================================================================
// Cache Handlers
// ============================================================================

async fn handle_cache_action(action: CacheAction) -> anyhow::Result<()> {
    match action {
        CacheAction::Init { path } => {
            let repo_path = path.unwrap_or_else(|| ".".to_string());
            let cache = RepoCache::new(&repo_path)?;

            println!("{} Cache initialized", "✓".green());
            println!("  {} {}", "Location:".dimmed(), cache.cache_dir().display());
            println!();
            println!("Cache structure created:");
            println!("  - cache/analysis/");
            println!("  - cache/docs/");
            println!("  - cache/refactor/");
            println!("  - cache/todos/");
        }

        CacheAction::Status { path } => {
            // Use SQLite cache for stats
            let repo_path = if let Some(p) = path {
                PathBuf::from(p)
            } else {
                std::env::current_dir()?
            };

            let cache = RepoCacheSql::new_for_repo(&repo_path).await?;
            let stats = cache.stats().await?;

            // Use default budget config ($3/month)
            let budget_config = rustcode::BudgetConfig::default();

            // Compute cache location
            use sha2::{Digest, Sha256};
            let canonical_path = repo_path
                .canonicalize()
                .unwrap_or_else(|_| repo_path.clone());
            let mut hasher = Sha256::new();
            hasher.update(canonical_path.to_string_lossy().as_bytes());
            let hash = hasher.finalize();
            let repo_hash = format!("{hash:x}")[..8].to_string();

            let cache_dir = if let Some(cache_home) = std::env::var_os("XDG_CACHE_HOME") {
                PathBuf::from(cache_home)
            } else if let Some(home) = dirs::home_dir() {
                home.join(".cache")
            } else {
                PathBuf::from(".")
            };
            let cache_location = cache_dir
                .join("rustcode")
                .join("repos")
                .join(&repo_hash)
                .join("cache.db");

            println!("📦 SQLite Cache Summary");
            println!("  Repository: {}", canonical_path.display());
            println!("  Cache Location: {}", cache_location.display());
            println!();

            // Group by cache type
            for type_stats in &stats.by_type {
                println!("  {} cache:", type_stats.cache_type);
                println!("    Entries: {}", type_stats.entries);
                println!("    Tokens: {}", type_stats.tokens);
                println!("    Estimated cost: ${:.4}", type_stats.cost);
            }

            println!();
            println!("  Total entries: {}", stats.total_entries);
            println!("  Total tokens: {}", stats.total_tokens);
            println!("  Total estimated cost: ${:.4}", stats.estimated_cost);
            println!();

            // Budget status
            let remaining = budget_config.monthly_budget - stats.estimated_cost;
            let percentage = (stats.estimated_cost / budget_config.monthly_budget) * 100.0;

            println!("💰 Budget Status:");
            if percentage >= budget_config.alert_threshold * 100.0 {
                println!(
                    "  🔴 Budget Alert: ${:.2} / ${:.2} ({:.1}%)",
                    stats.estimated_cost, budget_config.monthly_budget, percentage
                );
            } else if percentage >= budget_config.warning_threshold * 100.0 {
                println!(
                    "  ⚠️  Budget Warning: ${:.2} / ${:.2} ({:.1}%)",
                    stats.estimated_cost, budget_config.monthly_budget, percentage
                );
            } else {
                println!(
                    "  ✅ Budget OK: ${:.2} / ${:.2} ({:.1}%)",
                    stats.estimated_cost, budget_config.monthly_budget, percentage
                );
            }
            println!("  Remaining: ${remaining:.2}");

            if stats.total_tokens > 0 {
                let tokens_per_dollar =
                    stats.total_tokens as f64 / stats.estimated_cost.max(0.0001);
                let remaining_tokens = (remaining * tokens_per_dollar) as usize;
                println!("  Estimated tokens remaining: ~{remaining_tokens}");
            }
        }

        CacheAction::Clear {
            path,
            cache_type,
            all,
        } => {
            // Use SQLite cache
            let repo_path = if let Some(p) = path {
                PathBuf::from(p)
            } else {
                std::env::current_dir()?
            };

            let cache = RepoCacheSql::new_for_repo(&repo_path).await?;

            if all {
                let removed = cache.clear_all().await?;
                println!("{} Cleared {} cache entries", "✓".green(), removed);
            } else if let Some(type_str) = cache_type {
                let cache_type = match type_str.as_str() {
                    "analysis" => CacheType::Analysis,
                    "docs" => CacheType::Docs,
                    "refactor" => CacheType::Refactor,
                    "todos" => CacheType::Todos,
                    _ => {
                        eprintln!(
                            "{} Invalid cache type. Use: analysis, docs, refactor, or todos",
                            "✗".red()
                        );
                        return Ok(());
                    }
                };

                let removed = cache.clear_type(cache_type).await?;
                println!(
                    "{} Cleared {} {} cache entries",
                    "✓".green(),
                    removed,
                    type_str
                );
            } else {
                eprintln!("{} Specify --all or --cache-type", "✗".red());
            }
        }

        CacheAction::Migrate {
            source,
            destination,
            backup,
            verify,
        } => {
            use rustcode::CacheMigrator;

            // Determine source and destination paths
            let source_path = source.unwrap_or_else(|| {
                let home = dirs::home_dir().expect("Could not find home directory");
                home.join(".rustcode/cache/repos")
                    .to_string_lossy()
                    .to_string()
            });

            let dest_path = destination.unwrap_or_else(|| {
                let home = dirs::home_dir().expect("Could not find home directory");
                home.join(".rustcode/cache.db")
                    .to_string_lossy()
                    .to_string()
            });

            println!("{} Starting cache migration", "🔄".blue());
            println!("  Source: {source_path}");
            println!("  Destination: {dest_path}");
            println!();

            // Create migrator
            let migrator = CacheMigrator::new(&source_path, &dest_path).await?;

            // Create backup if requested
            if backup {
                let backup_path = format!("{source_path}.backup");
                println!("{} Creating backup at {}", "💾".blue(), backup_path);
                migrator.backup(&backup_path)?;
                println!("{} Backup created\n", "✓".green());
            }

            // Run migration with progress
            println!("{} Migrating entries...", "🔄".blue());
            let result = migrator
                .migrate(|progress| {
                    if progress.migrated % 10 == 0 || progress.migrated == progress.total {
                        println!(
                            "  Progress: {}/{} ({} failed)",
                            progress.migrated, progress.total, progress.failed
                        );
                    }
                })
                .await?;

            println!();
            println!("{} Migration complete!", "✓".green());
            println!("  Total entries: {}", result.total_entries);
            println!("  Migrated: {}", result.total_migrated);
            println!("  Failed: {}", result.total_failed);
            println!("  Source size: {} bytes", result.source_size);
            println!("  Destination size: {} bytes", result.destination_size);
            println!(
                "  Space saved: {} bytes ({:.1}%)",
                result.space_saved,
                if result.source_size > 0 {
                    (result.space_saved as f64 / result.source_size as f64) * 100.0
                } else {
                    0.0
                }
            );

            if !result.failures.is_empty() {
                println!();
                println!("{} Failed migrations:", "⚠️".yellow());
                for failure in result.failures.iter().take(5) {
                    println!("  - {}: {}", failure.file_path, failure.error);
                }
                if result.failures.len() > 5 {
                    println!("  ... and {} more", result.failures.len() - 5);
                }
            }

            // Verify if requested
            if verify {
                println!();
                println!("{} Verifying migration...", "🔍".blue());
                let valid = migrator.verify().await?;
                if valid {
                    println!("{} Verification passed!", "✓".green());
                } else {
                    println!("{} Verification failed - entry count mismatch", "✗".red());
                }
            }
        }
    }

    Ok(())
}
