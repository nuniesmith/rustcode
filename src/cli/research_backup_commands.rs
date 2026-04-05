// Research and Backup CLI Commands

use crate::backup::{print_rclone_setup_instructions, BackupConfig, BackupManager};
use crate::llm::GrokClient;
use crate::research::aggregator::Aggregator;
use crate::research::worker::{ResearchOrchestrator, WorkerConfig};
use crate::research::{
    get_research_with_results, list_research, save_research_request, ResearchDepth, ResearchRequest,
};
use anyhow::Result;
use clap::Subcommand;
use colored::Colorize;
use sqlx::PgPool;

// ============================================================================
// Research Commands
// ============================================================================

#[derive(Subcommand)]
pub enum ResearchCommands {
    // Start a new research project
    Start {
        // Topic to research
        topic: String,

        // Research type: general, code, idea, comparison
        #[arg(short = 't', long, default_value = "general")]
        research_type: String,

        // Depth: quick, standard, deep
        #[arg(short, long, default_value = "standard")]
        depth: String,

        // Description or specific questions
        #[arg(short = 'q', long)]
        description: Option<String>,

        // Repository context (for code research)
        #[arg(short, long)]
        repo: Option<String>,

        // File context (for code research)
        #[arg(short, long)]
        files: Option<String>,
    },

    // List research projects
    List {
        // Max number to show
        #[arg(short, long, default_value = "10")]
        limit: i32,
    },

    // View a research report
    View {
        // Research ID
        id: String,

        // Output format: markdown, json, zed
        #[arg(short, long, default_value = "markdown")]
        format: String,
    },

    // Quick research (single worker, fast)
    Quick {
        // Question to research
        question: String,
    },
}

pub async fn handle_research_command(pool: &PgPool, cmd: ResearchCommands) -> Result<()> {
    match cmd {
        ResearchCommands::Start {
            topic,
            research_type,
            depth,
            description,
            repo,
            files,
        } => {
            let depth_enum = match depth.to_lowercase().as_str() {
                "quick" => ResearchDepth::Quick,
                "deep" => ResearchDepth::Deep,
                _ => ResearchDepth::Standard,
            };

            let mut request = ResearchRequest::new(&topic, &research_type)
                .with_depth(depth_enum)
                .with_context(repo, files);

            if let Some(desc) = description {
                request = request.with_description(desc);
            }

            println!("\n{} Starting research: {}\n", "🔬".bold(), topic.cyan());
            println!(
                "Type: {} | Depth: {:?} | Workers: {}",
                research_type, depth_enum, request.worker_count
            );

            // Save request
            save_research_request(pool, &request).await?;

            // Initialize LLM client
            let llm = GrokClient::from_env()?;

            // Create orchestrator and execute
            let orchestrator =
                ResearchOrchestrator::new(pool.clone(), llm.clone(), WorkerConfig::default());

            println!("\n{}", "Spawning research workers...".dimmed());
            let results = orchestrator.execute(&request).await?;

            let successful = results.iter().filter(|r| r.status == "completed").count();
            println!(
                "\n{} {}/{} workers completed",
                if successful == results.len() {
                    "✓".green()
                } else {
                    "⚠".yellow()
                },
                successful,
                results.len()
            );

            // Aggregate results
            println!("\n{}", "Aggregating findings...".dimmed());
            let aggregator = Aggregator::new(llm);
            let report = aggregator.aggregate(&request, &results).await?;

            // Output report
            println!("\n{}", "═".repeat(60));
            println!("{}", report.to_markdown());

            println!(
                "\n{} Research saved: {}",
                "✓".green(),
                request.id[..8].dimmed()
            );
            println!(
                "View anytime with: rustcode research view {}",
                &request.id[..8]
            );
        }

        ResearchCommands::List { limit } => {
            let research = list_research(pool, limit).await?;

            if research.is_empty() {
                println!("{}", "No research projects found".yellow());
                return Ok(());
            }

            println!("\n{} Research Projects:\n", "🔬".bold());

            for r in research {
                let status_icon = match r.status.as_str() {
                    "completed" => "✓".green(),
                    "in_progress" => "⟳".blue(),
                    "failed" => "✗".red(),
                    _ => "○".white(),
                };

                println!(
                    "  {} {} [{}] {}",
                    r.id[..8].dimmed(),
                    status_icon,
                    r.research_type.cyan(),
                    r.topic
                );
            }
            println!();
        }

        ResearchCommands::View { id, format } => {
            // Find research by partial ID
            let (request, results) = get_research_with_results(pool, &id).await?;

            if results.is_empty() {
                println!("{}", "No results found for this research".yellow());
                return Ok(());
            }

            // Regenerate report
            let llm = GrokClient::from_env()?;
            let aggregator = Aggregator::new(llm);
            let report = aggregator.aggregate(&request, &results).await?;

            match format.as_str() {
                "json" => println!("{}", report.to_json()?),
                "zed" => println!("{}", report.to_zed_format()),
                _ => println!("{}", report.to_markdown()),
            }
        }

        ResearchCommands::Quick { question } => {
            println!("\n{} Quick research: {}\n", "⚡".bold(), question.cyan());

            let request =
                ResearchRequest::new(&question, "general").with_depth(ResearchDepth::Quick);

            save_research_request(pool, &request).await?;

            let llm = GrokClient::from_env()?;
            let orchestrator = ResearchOrchestrator::new(
                pool.clone(),
                llm.clone(),
                WorkerConfig {
                    max_concurrent: 2,
                    timeout_secs: 60,
                    max_tokens: 2048,
                    retry_failed: false,
                },
            );

            let results = orchestrator.execute(&request).await?;
            let aggregator = Aggregator::new(llm);
            let report = aggregator.aggregate(&request, &results).await?;

            println!("{}", report.to_zed_format());
        }
    }

    Ok(())
}

// ============================================================================
// Backup Commands
// ============================================================================

#[derive(Subcommand)]
pub enum BackupCommands {
    // Create a new backup
    Create,

    // List available backups
    List,

    // Restore from a backup
    Restore {
        // Backup name (e.g., backup_20240101_120000)
        name: String,
    },

    // Show rclone setup instructions
    Setup,

    // Check backup configuration
    Check,
}

pub async fn handle_backup_command(cmd: BackupCommands) -> Result<()> {
    let config = BackupConfig::from_env();
    let manager = BackupManager::new(config.clone());

    match cmd {
        BackupCommands::Create => {
            // Check rclone first
            if !manager.check_rclone()? {
                println!(
                    "{} rclone not configured. Run: rustcode backup setup",
                    "✗".red()
                );
                return Ok(());
            }

            println!("\n{} Creating backup...\n", "📦".bold());

            match manager.create_backup() {
                Ok(result) => {
                    println!("{} Backup created successfully!", "✓".green());
                    println!("  Name: {}", result.name.cyan());
                    println!("  Size: {} bytes", result.size_bytes);
                    println!("  Path: {}", result.remote_path.dimmed());
                }
                Err(e) => {
                    println!("{} Backup failed: {}", "✗".red(), e);
                }
            }
        }

        BackupCommands::List => {
            if !manager.check_rclone()? {
                println!(
                    "{} rclone not configured. Run: rustcode backup setup",
                    "✗".red()
                );
                return Ok(());
            }

            let backups = manager.list_backups()?;

            if backups.is_empty() {
                println!("{}", "No backups found".yellow());
                return Ok(());
            }

            println!("\n{} Available Backups:\n", "📦".bold());

            for backup in backups {
                println!("  {} ({})", backup.name.cyan(), backup.created_at.dimmed());
            }

            println!("\nRestore with: rustcode backup restore <name>");
        }

        BackupCommands::Restore { name } => {
            if !manager.check_rclone()? {
                println!(
                    "{} rclone not configured. Run: rustcode backup setup",
                    "✗".red()
                );
                return Ok(());
            }

            println!("\n{} Restoring from: {}\n", "📦".bold(), name.cyan());
            println!(
                "{} Make sure rustcode service is stopped!",
                "⚠".yellow()
            );
            println!("Press Enter to continue or Ctrl+C to cancel...");

            let mut input = String::new();
            std::io::stdin().read_line(&mut input)?;

            match manager.restore(&name) {
                Ok(()) => {
                    println!("{} Restore complete!", "✓".green());
                    println!("Restart rustcode service to use restored data.");
                }
                Err(e) => {
                    println!("{} Restore failed: {}", "✗".red(), e);
                }
            }
        }

        BackupCommands::Setup => {
            print_rclone_setup_instructions();
        }

        BackupCommands::Check => {
            println!("\n{} Backup Configuration:\n", "🔧".bold());
            println!("  Data directory: {}", config.data_dir.display());
            println!("  Remote name: {}", config.remote_name);
            println!("  Remote path: {}", config.remote_path);
            println!("  Retention: {} backups", config.retention_count);

            println!("\n{} Checking rclone...", "🔍".bold());

            match manager.check_rclone() {
                Ok(true) => {
                    println!("  {} rclone configured correctly", "✓".green());

                    // Try listing backups
                    if let Ok(backups) = manager.list_backups() {
                        println!("  {} {} existing backups", "✓".green(), backups.len());
                    }
                }
                Ok(false) => {
                    println!("  {} rclone remote not configured", "✗".red());
                    println!("\n  Run: rustcode backup setup");
                }
                Err(e) => {
                    println!("  {} rclone error: {}", "✗".red(), e);
                }
            }
        }
    }

    Ok(())
}
