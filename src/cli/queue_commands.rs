//! Queue CLI Commands
//!
//! Commands for managing the processing queue, scanning repos, and viewing status.

use crate::db::queue::{
    create_queue_tables, QueuePriority, QueueSource, QueueStage, GITHUB_USERNAME,
};
use crate::llm::grok::GrokAnalyzer;
use crate::queue::processor::{
    capture_note, capture_thought, get_pending_items, get_queue_stats, LlmAnalyzer,
    ProcessorConfig, QueueProcessor,
};
use crate::scanner::github::{
    build_dir_tree, get_unanalyzed_files, save_dir_tree, scan_repo_for_todos, sync_repos_to_db,
};
use anyhow::Result;
use clap::Subcommand;
use colored::Colorize;
use sqlx::PgPool;
use std::path::PathBuf;

// ============================================================================
// CLI Structure
// ============================================================================

#[derive(Subcommand)]
pub enum QueueCommands {
    /// Add content to the processing queue
    Add {
        /// Content to add
        content: String,

        /// Source type: note, thought, research
        #[arg(short, long, default_value = "note")]
        source: String,

        /// Priority: critical, high, normal, low, background
        #[arg(short, long, default_value = "normal")]
        priority: String,

        /// Associated project name
        #[arg(long)]
        project: Option<String>,
    },

    /// View queue status and statistics
    Status,

    /// List items in a specific stage
    List {
        /// Stage to list: inbox, pending, analyzing, ready, failed
        #[arg(default_value = "inbox")]
        stage: String,

        /// Maximum items to show
        #[arg(short, long, default_value = "20")]
        limit: i32,
    },

    /// Process the queue (run in foreground)
    Process {
        /// Number of items per batch
        #[arg(short, long, default_value = "10")]
        batch_size: i32,

        /// Run once and exit (don't loop)
        #[arg(long)]
        once: bool,
    },

    /// Retry failed items
    Retry {
        /// Specific item ID to retry (or all if not specified)
        id: Option<String>,
    },
}

#[derive(Subcommand)]
pub enum ScanCommands {
    /// Sync repositories from GitHub
    Repos {
        /// GitHub API token (optional, for higher rate limits)
        #[arg(short, long, env = "GITHUB_TOKEN")]
        token: Option<String>,
    },

    /// Scan a repository for TODOs
    Todos {
        /// Repository path or ID
        repo: String,
    },

    /// Build/update directory tree for a repository
    Tree {
        /// Repository path or ID
        repo: String,

        /// Maximum depth
        #[arg(short, long, default_value = "5")]
        depth: usize,
    },

    /// Find unanalyzed files in a repository
    Unanalyzed {
        /// Repository path or ID
        repo: String,

        /// Maximum files to show
        #[arg(short, long, default_value = "20")]
        limit: i32,
    },

    /// Analyze files in a repository
    Analyze {
        /// Repository path or ID
        repo: String,

        /// Maximum files to analyze
        #[arg(short, long, default_value = "10")]
        limit: i32,
    },

    /// Run full scan on all repos
    All {
        /// GitHub API token
        #[arg(short, long, env = "GITHUB_TOKEN")]
        token: Option<String>,

        /// Skip TODO scanning
        #[arg(long)]
        skip_todos: bool,

        /// Skip tree building
        #[arg(long)]
        skip_tree: bool,
    },
}

#[derive(Subcommand)]
pub enum ReportCommands {
    /// Show TODO summary across all repos
    Todos {
        /// Filter by priority (1-4)
        #[arg(short, long)]
        priority: Option<i32>,

        /// Filter by repository
        #[arg(short, long)]
        repo: Option<String>,
    },

    /// Show file analysis summary
    Files {
        /// Filter by repository
        #[arg(short, long)]
        repo: Option<String>,

        /// Show only files needing attention
        #[arg(long)]
        attention_only: bool,
    },

    /// Show repository health report
    Health {
        /// Repository path or ID
        repo: Option<String>,
    },

    /// Generate standardization report
    Standardization {
        /// Repository path or ID
        repo: String,
    },
}

// ============================================================================
// Command Handlers
// ============================================================================

pub async fn handle_queue_command(pool: &PgPool, cmd: QueueCommands) -> Result<()> {
    // Ensure tables exist
    create_queue_tables(pool).await?;

    match cmd {
        QueueCommands::Add {
            content,
            source,
            priority,
            project,
        } => {
            let source = parse_source(&source);
            let _priority = parse_priority(&priority);

            let item = match source {
                QueueSource::RawThought => capture_thought(pool, &content).await?,
                _ => capture_note(pool, &content, project.as_deref()).await?,
            };

            println!("{} Added to queue", "✓".green());
            println!("  {} {}", "ID:".dimmed(), item.id);
            println!("  {} {}", "Stage:".dimmed(), item.stage);
            println!("  {} {:?}", "Source:".dimmed(), source);
        }

        QueueCommands::Status => {
            let stats = get_queue_stats(pool).await?;

            println!("📊 Queue Status\n");
            println!("  {} {}", "Inbox:".dimmed(), stats.inbox);
            println!(
                "  {} {}",
                "Pending Analysis:".dimmed(),
                stats.pending_analysis
            );
            println!("  {} {}", "Analyzing:".dimmed(), stats.analyzing);
            println!(
                "  {} {}",
                "Pending Tagging:".dimmed(),
                stats.pending_tagging
            );
            println!("  {} {}", "Ready:".dimmed(), stats.ready);
            println!(
                "  {} {}",
                "Failed:".dimmed(),
                format!("{}", stats.failed).red()
            );
            println!("  {} {}", "Archived:".dimmed(), stats.archived);
            println!();
            println!("  {} {}", "Total Pending:".cyan(), stats.total_pending());
        }

        QueueCommands::List { stage, limit } => {
            let stage_enum = parse_stage(&stage);
            let items = get_pending_items(pool, stage_enum, limit).await?;

            if items.is_empty() {
                println!("{} No items in {} stage", "📭".dimmed(), stage);
            } else {
                println!("📋 Items in {} stage ({}):\n", stage, items.len());

                for item in items {
                    let preview: String = item.content.chars().take(60).collect();
                    let preview = if item.content.len() > 60 {
                        format!("{}...", preview)
                    } else {
                        preview
                    };

                    println!("  [{}] {}", item.id.dimmed(), preview);
                    println!(
                        "     {} {} | {} P{}",
                        "Source:".dimmed(),
                        item.source,
                        "Priority:".dimmed(),
                        item.priority
                    );
                    if item.retry_count > 0 {
                        println!("     {} {}", "Retries:".dimmed(), item.retry_count);
                    }
                    println!();
                }
            }
        }

        QueueCommands::Process { batch_size, once } => {
            let api_key =
                std::env::var("XAI_API_KEY").expect("XAI_API_KEY must be set for processing");

            let analyzer = Box::new(GrokAnalyzer::new(api_key));
            let config = ProcessorConfig {
                batch_size,
                ..Default::default()
            };

            let processor = QueueProcessor::new(pool.clone(), config, analyzer);

            println!("🔄 Starting queue processor...");

            if once {
                // Process one cycle and exit
                // (Would need to expose individual methods for this)
                println!(
                    "{} Single-pass processing not yet implemented",
                    "⚠".yellow()
                );
            } else {
                processor.run().await?;
            }
        }

        QueueCommands::Retry { id } => {
            if let Some(_id) = id {
                println!("{} Retry by ID not yet implemented", "⚠".yellow());
            } else {
                let items = crate::queue::processor::get_retriable_items(pool, 3).await?;
                println!("🔄 Found {} retriable items", items.len());

                for item in items {
                    crate::queue::processor::advance_stage(pool, &item.id).await?;
                    println!("  {} {}", "✓".green(), item.id);
                }
            }
        }
    }

    Ok(())
}

pub async fn handle_scan_command(pool: &PgPool, cmd: ScanCommands) -> Result<()> {
    create_queue_tables(pool).await?;

    match cmd {
        ScanCommands::Repos { token } => {
            println!("🔄 Syncing repositories for {}...", GITHUB_USERNAME.cyan());

            let repo_ids = sync_repos_to_db(pool, token.as_deref()).await?;

            println!("{} Synced {} repositories", "✓".green(), repo_ids.len());

            // List repos
            let repos: Vec<(String, String)> = sqlx::query_as(
                "SELECT id, name FROM repositories WHERE status = 'active' ORDER BY name",
            )
            .fetch_all(pool)
            .await?;

            for (_id, name) in repos {
                println!("  📁 {}", name);
            }
        }

        ScanCommands::Todos { repo } => {
            let (repo_id, repo_path) = resolve_repo(pool, &repo).await?;

            println!("🔍 Scanning {} for TODOs...", repo_path.display());

            let result = scan_repo_for_todos(pool, &repo_id, &repo_path).await?;

            println!("{} Scan complete", "✓".green());
            println!("  {} {}", "Total found:".dimmed(), result.total_found);
            println!("  {} {}", "New:".dimmed(), result.new_todos);
            println!("  {} {}", "Updated:".dimmed(), result.updated_todos);
            println!("  {} {}", "Removed:".dimmed(), result.removed_todos);
        }

        ScanCommands::Tree { repo, depth } => {
            let (repo_id, repo_path) = resolve_repo(pool, &repo).await?;

            println!("🌳 Building directory tree for {}...", repo_path.display());

            let tree = build_dir_tree(&repo_path, depth)?;
            save_dir_tree(pool, &repo_id, &tree).await?;

            // Print tree summary
            fn count_items(node: &crate::scanner::github::TreeNode) -> (usize, usize) {
                if node.is_dir {
                    let mut dirs = 1;
                    let mut files = 0;
                    for child in &node.children {
                        let (d, f) = count_items(child);
                        dirs += d;
                        files += f;
                    }
                    (dirs, files)
                } else {
                    (0, 1)
                }
            }

            let (dirs, files) = count_items(&tree);

            println!("{} Tree saved", "✓".green());
            println!("  {} {}", "Directories:".dimmed(), dirs);
            println!("  {} {}", "Files:".dimmed(), files);
        }

        ScanCommands::Unanalyzed { repo, limit } => {
            let (repo_id, repo_path) = resolve_repo(pool, &repo).await?;

            let files = get_unanalyzed_files(pool, &repo_id, &repo_path, limit).await?;

            if files.is_empty() {
                println!("{} All files have been analyzed!", "✓".green());
            } else {
                println!("📄 Unanalyzed files ({}):\n", files.len());

                for file in files {
                    let rel_path = file.strip_prefix(&repo_path).unwrap_or(&file);
                    println!("  {}", rel_path.display());
                }
            }
        }

        ScanCommands::Analyze { repo, limit } => {
            let api_key =
                std::env::var("XAI_API_KEY").expect("XAI_API_KEY must be set for analysis");

            let (repo_id, repo_path) = resolve_repo(pool, &repo).await?;
            let analyzer = GrokAnalyzer::new(api_key);

            let files = get_unanalyzed_files(pool, &repo_id, &repo_path, limit).await?;

            if files.is_empty() {
                println!("{} All files have been analyzed!", "✓".green());
                return Ok(());
            }

            println!("🔬 Analyzing {} files...\n", files.len());

            for file in files {
                let rel_path = file.strip_prefix(&repo_path).unwrap_or(&file);
                let rel_path_str = rel_path.to_string_lossy().to_string();

                print!("  {} {}...", "→".dimmed(), rel_path.display());

                // Read file
                let content = match std::fs::read(&file) {
                    Ok(c) => c,
                    Err(e) => {
                        println!(" {} ({})", "skipped".yellow(), e);
                        continue;
                    }
                };

                let content_str = match String::from_utf8(content.clone()) {
                    Ok(s) => s,
                    Err(_) => {
                        println!(" {} (binary)", "skipped".yellow());
                        continue;
                    }
                };

                // Detect language from extension
                let lang = file.extension().and_then(|e| e.to_str()).unwrap_or("text");

                // Analyze with LLM
                match analyzer
                    .analyze_file(&content_str, &rel_path_str, lang)
                    .await
                {
                    Ok(analysis) => {
                        crate::scanner::github::save_file_analysis(
                            pool,
                            &repo_id,
                            &rel_path_str,
                            &content,
                            &analysis,
                        )
                        .await?;

                        let score_color = if analysis.quality_score >= 7 {
                            "green"
                        } else if analysis.quality_score >= 4 {
                            "yellow"
                        } else {
                            "red"
                        };

                        println!(
                            " {} Q:{} C:{}",
                            "✓".green(),
                            format!("{}", analysis.quality_score).color(score_color),
                            analysis.complexity_score
                        );
                    }
                    Err(e) => {
                        println!(" {} ({})", "failed".red(), e);
                    }
                }
            }

            println!("\n📊 Tokens used: {}", analyzer.tokens_used());
        }

        ScanCommands::All {
            token,
            skip_todos,
            skip_tree,
        } => {
            println!("🚀 Running full scan for {}...\n", GITHUB_USERNAME.cyan());

            // Sync repos
            println!("1️⃣ Syncing repositories...");
            let repo_ids = sync_repos_to_db(pool, token.as_deref()).await?;
            println!("   {} {} repos synced\n", "✓".green(), repo_ids.len());

            // For each repo, scan TODOs and build tree
            for repo_id in &repo_ids {
                let repo: Option<(String, String)> =
                    sqlx::query_as("SELECT name, path FROM repositories WHERE id = $1")
                        .bind(repo_id)
                        .fetch_optional(pool)
                        .await?;

                let (name, _path) = match repo {
                    Some(r) => r,
                    None => continue,
                };

                println!("📁 Processing {}...", name.cyan());

                // Try to find local clone
                let local_path = find_local_repo(&name);

                if let Some(repo_path) = local_path {
                    if !skip_todos {
                        print!("   {} Scanning TODOs...", "→".dimmed());
                        match scan_repo_for_todos(pool, repo_id, &repo_path).await {
                            Ok(result) => println!(" {} found {}", "✓".green(), result.total_found),
                            Err(e) => println!(" {} ({})", "failed".red(), e),
                        }
                    }

                    if !skip_tree {
                        print!("   {} Building tree...", "→".dimmed());
                        match build_dir_tree(&repo_path, 5) {
                            Ok(tree) => {
                                save_dir_tree(pool, repo_id, &tree).await?;
                                println!(" {}", "✓".green());
                            }
                            Err(e) => println!(" {} ({})", "failed".red(), e),
                        }
                    }
                } else {
                    println!("   {} Local clone not found, skipping scan", "⚠".yellow());
                }
            }

            println!("\n{} Full scan complete!", "✓".green());
        }
    }

    Ok(())
}

pub async fn handle_report_command(pool: &PgPool, cmd: ReportCommands) -> Result<()> {
    match cmd {
        ReportCommands::Todos { priority, repo } => {
            // Read from the canonical `tasks` table (todo_items was dropped in
            // migration 017). Filter by source = 'github_scanner' or
            // source = 'queue_processor' to get TODO-derived tasks.
            let mut conditions = vec![
                "t.status != 'done'".to_string(),
                "(t.source = 'github_scanner' OR t.source = 'queue_processor')".to_string(),
            ];
            let mut param_idx = 1usize;

            if priority.is_some() {
                conditions.push(format!("t.priority <= ${}", param_idx));
                param_idx += 1;
            }
            if repo.is_some() {
                conditions.push(format!("r.name = ${}", param_idx));
                param_idx += 1;
            }
            let _ = param_idx; // silence unused warning after last branch

            let where_sql = format!("WHERE {}", conditions.join(" AND "));
            let query = format!(
                "SELECT t.id, t.title, t.priority, t.file_path, t.line_number,
                        t.source, COALESCE(r.name, 'unknown') AS repo_name
                 FROM tasks t
                 LEFT JOIN repositories r ON t.repo_id = r.id
                 {}
                 ORDER BY t.priority ASC, t.created_at DESC
                 LIMIT 50",
                where_sql
            );

            #[derive(sqlx::FromRow)]
            struct TaskRow {
                #[allow(dead_code)]
                id: String,
                title: String,
                priority: i32,
                file_path: Option<String>,
                line_number: Option<i32>,
                source: String,
                repo_name: String,
            }

            let mut q = sqlx::query_as::<_, TaskRow>(&query);
            if let Some(p) = priority {
                q = q.bind(p);
            }
            if let Some(r) = &repo {
                q = q.bind(r);
            }

            let tasks: Vec<TaskRow> = q.fetch_all(pool).await?;

            if tasks.is_empty() {
                println!("{} No TODOs found", "✓".green());
            } else {
                println!("📋 TODOs ({}):\n", tasks.len());

                for task in tasks {
                    let priority_icon = match task.priority {
                        1 => "🔴",
                        2 => "🟠",
                        3 => "🟡",
                        _ => "🟢",
                    };

                    println!(
                        "  {} [{}] {}",
                        priority_icon,
                        task.source.cyan(),
                        task.title
                    );
                    if let Some(fp) = &task.file_path {
                        println!(
                            "     {} {}{}",
                            task.repo_name.dimmed(),
                            fp,
                            task.line_number
                                .map(|l| format!(":{}", l))
                                .unwrap_or_default()
                        );
                    }
                    println!();
                }
            }
        }

        ReportCommands::Files {
            repo,
            attention_only,
        } => {
            let mut query = String::from(
                "SELECT f.*, r.name as repo_name FROM file_analysis f
                 JOIN repositories r ON f.repo_id = r.id WHERE 1=1",
            );

            if attention_only {
                query.push_str(" AND f.needs_attention = 1");
            }
            if repo.is_some() {
                query.push_str(" AND r.name = ?");
            }

            query.push_str(" ORDER BY f.quality_score ASC LIMIT 30");

            #[derive(sqlx::FromRow)]
            struct FileWithRepo {
                file_path: String,
                summary: Option<String>,
                quality_score: Option<i32>,
                complexity_score: Option<i32>,
                needs_attention: bool,
                repo_name: String,
            }

            let mut q = sqlx::query_as::<_, FileWithRepo>(&query);
            if let Some(r) = &repo {
                q = q.bind(r);
            }

            let files: Vec<FileWithRepo> = q.fetch_all(pool).await?;

            if files.is_empty() {
                println!("{} No analyzed files found", "📭".dimmed());
            } else {
                println!("📄 Analyzed Files ({}):\n", files.len());

                for file in files {
                    let attention_icon = if file.needs_attention { "⚠️ " } else { "" };
                    let quality = file.quality_score.unwrap_or(0);
                    let complexity = file.complexity_score.unwrap_or(0);

                    println!(
                        "  {}{} [Q:{} C:{}]",
                        attention_icon, file.file_path, quality, complexity
                    );
                    println!(
                        "     {} {}",
                        file.repo_name.dimmed(),
                        file.summary.as_deref().unwrap_or("No summary")
                    );
                    println!();
                }
            }
        }

        ReportCommands::Health { repo } => {
            if let Some(repo_name) = repo {
                // Single repo health
                let cache: Option<(i32, i32, i32, i32, Option<i32>)> = sqlx::query_as(
                    "SELECT c.total_files, c.analyzed_files, c.total_todos, c.active_todos, c.health_score
                     FROM repo_cache c JOIN repositories r ON c.repo_id = r.id
                     WHERE r.name = ?"
                )
                .bind(&repo_name)
                .fetch_optional(pool)
                .await?;

                if let Some((total, analyzed, total_todos, active_todos, health)) = cache {
                    println!("📊 Health Report: {}\n", repo_name.cyan());
                    println!(
                        "  {} {}",
                        "Health Score:".dimmed(),
                        health
                            .map(|h| format!("{}/10", h))
                            .unwrap_or("Not rated".to_string())
                    );
                    println!("  {} {}/{}", "Files Analyzed:".dimmed(), analyzed, total);
                    println!(
                        "  {} {} ({} active)",
                        "TODOs:".dimmed(),
                        total_todos,
                        active_todos
                    );
                } else {
                    println!("{} No data for {}", "⚠".yellow(), repo_name);
                }
            } else {
                // All repos summary
                let repos: Vec<(String, i32, i32, i32, Option<i32>)> = sqlx::query_as(
                    "SELECT r.name, c.total_files, c.analyzed_files, c.active_todos, c.health_score
                     FROM repo_cache c JOIN repositories r ON c.repo_id = r.id
                     WHERE r.status = 'active'
                     ORDER BY c.health_score ASC NULLS LAST",
                )
                .fetch_all(pool)
                .await?;

                println!("📊 Repository Health Summary\n");

                for (name, total, analyzed, todos, health) in repos {
                    let health_str = health
                        .map(|h| format!("{}/10", h))
                        .unwrap_or("--".to_string());
                    let progress = if total > 0 {
                        format!("{}%", (analyzed * 100) / total)
                    } else {
                        "--".to_string()
                    };

                    println!(
                        "  📁 {} | Health: {} | Analyzed: {} | TODOs: {}",
                        name, health_str, progress, todos
                    );
                }
            }
        }

        ReportCommands::Standardization { repo } => {
            let api_key = std::env::var("XAI_API_KEY").expect("XAI_API_KEY must be set");

            let (repo_id, _repo_path) = resolve_repo(pool, &repo).await?;
            let analyzer = GrokAnalyzer::new(api_key);

            println!("🔬 Generating standardization report for {}...\n", repo);

            // Get tree structure
            let tree = crate::scanner::github::get_dir_tree(pool, &repo_id)
                .await?
                .ok_or_else(|| {
                    anyhow::anyhow!("No directory tree cached. Run 'scan tree' first.")
                })?;

            // Build tree text representation
            fn tree_to_text(node: &crate::scanner::github::TreeNode, indent: usize) -> String {
                let mut s = format!(
                    "{}{}{}\n",
                    "  ".repeat(indent),
                    if node.is_dir { "📁 " } else { "📄 " },
                    node.name
                );
                for child in &node.children {
                    s.push_str(&tree_to_text(child, indent + 1));
                }
                s
            }

            let tree_text = tree_to_text(&tree, 0);

            // Get sample files (first 5 analyzed files)
            let samples: Vec<(String, Option<String>)> = sqlx::query_as(
                "SELECT file_path, summary FROM file_analysis WHERE repo_id = $1 LIMIT 5",
            )
            .bind(&repo_id)
            .fetch_all(pool)
            .await?;

            let sample_files: Vec<(&str, &str)> = samples
                .iter()
                .map(|(p, s)| (p.as_str(), s.as_deref().unwrap_or("")))
                .collect();

            let report = analyzer
                .analyze_repo_standardization(&repo, &tree_text, &sample_files)
                .await?;

            println!("📋 Standardization Report\n");
            println!("  {} {}/10\n", "Health Score:".cyan(), report.health_score);

            if !report.issues.is_empty() {
                println!("  {}:", "Issues Found".red());
                for issue in &report.issues {
                    let severity_icon = match issue.severity.as_str() {
                        "high" => "🔴",
                        "medium" => "🟠",
                        _ => "🟡",
                    };
                    println!(
                        "    {} [{}] {}",
                        severity_icon, issue.category, issue.description
                    );
                    println!("       → {}", issue.recommendation.dimmed());
                }
                println!();
            }

            if !report.strengths.is_empty() {
                println!("  {}:", "Strengths".green());
                for s in &report.strengths {
                    println!("    ✓ {}", s);
                }
                println!();
            }

            if !report.missing_files.is_empty() {
                println!("  {}:", "Missing Files".yellow());
                for f in &report.missing_files {
                    println!("    ⚠ {}", f);
                }
            }
        }
    }

    Ok(())
}

// ============================================================================
// Helper Functions
// ============================================================================

fn parse_source(s: &str) -> QueueSource {
    match s.to_lowercase().as_str() {
        "thought" | "idea" => QueueSource::RawThought,
        "research" => QueueSource::Research,
        "doc" | "document" => QueueSource::Document,
        _ => QueueSource::Note,
    }
}

fn parse_priority(s: &str) -> QueuePriority {
    match s.to_lowercase().as_str() {
        "critical" | "1" => QueuePriority::Critical,
        "high" | "2" => QueuePriority::High,
        "low" | "4" => QueuePriority::Low,
        "background" | "5" => QueuePriority::Background,
        _ => QueuePriority::Normal,
    }
}

fn parse_stage(s: &str) -> QueueStage {
    match s.to_lowercase().as_str() {
        "pending" | "pending_analysis" => QueueStage::PendingAnalysis,
        "analyzing" => QueueStage::Analyzing,
        "tagging" | "pending_tagging" => QueueStage::PendingTagging,
        "ready" | "done" => QueueStage::Ready,
        "failed" | "error" => QueueStage::Failed,
        "archived" => QueueStage::Archived,
        _ => QueueStage::Inbox,
    }
}

async fn resolve_repo(pool: &PgPool, input: &str) -> Result<(String, PathBuf)> {
    // Try as path first
    let path = PathBuf::from(input);
    if path.exists() && path.is_dir() {
        // Find or create repo entry
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown");

        let existing: Option<(String,)> =
            sqlx::query_as("SELECT id FROM repositories WHERE local_path = $1")
                .bind(path.to_string_lossy().as_ref())
                .fetch_optional(pool)
                .await?;

        let id = if let Some((id,)) = existing {
            id
        } else {
            let id = uuid::Uuid::new_v4().to_string();
            let now = chrono::Utc::now().timestamp();

            sqlx::query(
                "INSERT INTO repositories (id, local_path, name, created_at, updated_at) VALUES ($1, $2, $3, $4, $5)"
            )
            .bind(&id)
            .bind(path.to_string_lossy().as_ref())
            .bind(name)
            .bind(now)
            .bind(now)
            .execute(pool)
            .await?;

            id
        };

        return Ok((id, path));
    }

    // Try as repo name or ID
    let repo: Option<(String, String)> =
        sqlx::query_as("SELECT id, local_path FROM repositories WHERE id = $1 OR name = $2")
            .bind(input)
            .bind(input)
            .fetch_optional(pool)
            .await?;

    if let Some((id, path)) = repo {
        // Path might be a URL for GitHub repos
        if let Some(local) = find_local_repo(&path) {
            return Ok((id, local));
        }
        return Ok((id, PathBuf::from(path)));
    }

    anyhow::bail!("Repository not found: {}", input)
}

fn find_local_repo(name_or_path: &str) -> Option<PathBuf> {
    // Common locations to check
    let home = dirs::home_dir()?;
    let candidates = [
        PathBuf::from(name_or_path),
        home.join("github").join(name_or_path),
        home.join("code").join(name_or_path),
        home.join("projects").join(name_or_path),
        home.join("dev").join(name_or_path),
        home.join("repos").join(name_or_path),
        home.join(name_or_path),
    ];

    candidates
        .into_iter()
        .find(|path| path.exists() && path.is_dir())
}
