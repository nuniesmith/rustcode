// Task CLI Commands
//
// Commands for managing tasks, grouping, and exporting to IDE.

use crate::task::grouping::{
    filter_by_priority, filter_ready_groups, get_next_group, group_tasks, GroupingStrategy,
};
use crate::task::{
    create_task, get_pending_tasks, get_task_stats, get_tasks_by_status, update_task_status, Task,
    TaskSource, TaskStatus,
};
use anyhow::Result;
use clap::Subcommand;
use colored::Colorize;
use sqlx::PgPool;

#[derive(Subcommand)]
pub enum TaskCommands {
    // Add a new task
    Add {
        // Task description
        content: String,

        // Source: manual, todo, scan, idea
        #[arg(short, long, default_value = "manual")]
        source: String,

        // Priority (1-10)
        #[arg(short, long, default_value = "5")]
        priority: i32,

        // Category: bug, refactor, feature, docs, test
        #[arg(short, long)]
        category: Option<String>,

        // Associated file path
        #[arg(short, long)]
        file: Option<String>,

        // Repository name
        #[arg(short, long)]
        repo: Option<String>,
    },

    // List tasks by status
    List {
        // Status: pending, review, ready, done, failed, all
        #[arg(default_value = "pending")]
        status: String,

        // Max number of tasks
        #[arg(short, long, default_value = "20")]
        limit: i32,
    },

    // Show task statistics
    Stats,

    // Get the next highest-priority task group for IDE
    #[command(name = "next")]
    NextGroup {
        // Output format: zed, markdown, json
        #[arg(short, long, default_value = "zed")]
        format: String,

        // Minimum priority to include
        #[arg(short, long, default_value = "1")]
        min_priority: i32,

        // Grouping: file, category, repo, smart
        #[arg(short, long, default_value = "smart")]
        group_by: String,

        // Copy to clipboard
        #[arg(long)]
        copy: bool,
    },

    // List all task groups
    Groups {
        // Grouping strategy: file, category, repo, smart
        #[arg(short, long, default_value = "smart")]
        group_by: String,

        // Minimum priority
        #[arg(short, long, default_value = "1")]
        min_priority: i32,
    },

    // Mark a task as done
    Done {
        // Task ID (or "next" for the next pending task)
        id: String,
    },

    // Mark a task as ready for IDE
    Ready {
        // Task ID
        id: String,
    },
}

pub async fn handle_task_command(pool: &PgPool, cmd: TaskCommands) -> Result<()> {
    match cmd {
        TaskCommands::Add {
            content,
            source,
            priority,
            category,
            file,
            repo,
        } => {
            let source_type = match source.to_lowercase().as_str() {
                "todo" => TaskSource::Todo,
                "scan" => TaskSource::Scan,
                "idea" => TaskSource::Idea,
                _ => TaskSource::Manual,
            };

            let mut task = Task::new(&content, source_type).with_priority(priority);

            if let Some(cat) = category {
                task.category = Some(cat);
            }
            if let Some(f) = file {
                task.source_file = Some(f);
            }
            if let Some(r) = repo {
                task.source_repo = Some(r);
            }

            create_task(pool, &task).await?;
            println!(
                "{} Task added: {} (priority: {})",
                "✓".green(),
                task.id,
                task.priority
            );
        }

        TaskCommands::List { status, limit } => {
            let tasks = if status == "all" {
                get_pending_tasks(pool, limit).await?
            } else {
                get_tasks_by_status(pool, &status, limit).await?
            };

            if tasks.is_empty() {
                println!("{}", "No tasks found".yellow());
                return Ok(());
            }

            println!(
                "\n{} {} tasks:\n",
                "📋".bold(),
                status.to_uppercase().bold()
            );

            for task in tasks {
                let priority_color = match task.priority {
                    8..=10 => task.priority.to_string().red(),
                    5..=7 => task.priority.to_string().yellow(),
                    _ => task.priority.to_string().white(),
                };

                let category = task.category.as_deref().unwrap_or("task");
                let location = task
                    .source_file
                    .as_ref()
                    .map(|f| format!(" [{}]", f))
                    .unwrap_or_default();

                println!(
                    "  {} [P{}] [{}] {}{}",
                    task.id[..8].dimmed(),
                    priority_color,
                    category.cyan(),
                    task.content,
                    location.dimmed()
                );
            }
            println!();
        }

        TaskCommands::Stats => {
            let stats = get_task_stats(pool).await?;

            println!("\n{}\n", "📊 Task Statistics".bold());
            println!("  Pending:    {}", stats.pending.to_string().yellow());
            println!("  Processing: {}", stats.processing.to_string().blue());
            println!("  Review:     {}", stats.review.to_string().cyan());
            println!("  Ready:      {}", stats.ready.to_string().green());
            println!("  Done:       {}", stats.done.to_string().white());
            println!("  Failed:     {}", stats.failed.to_string().red());
            println!(
                "\n  Total tokens used: {}",
                stats.total_tokens.to_string().dimmed()
            );
            println!();
        }

        TaskCommands::NextGroup {
            format,
            min_priority,
            group_by,
            copy,
        } => {
            let tasks = get_pending_tasks(pool, 100).await?;

            if tasks.is_empty() {
                println!("{}", "No pending tasks".yellow());
                return Ok(());
            }

            let strategy = match group_by.as_str() {
                "file" => GroupingStrategy::ByFile,
                "category" => GroupingStrategy::ByCategory,
                "repo" => GroupingStrategy::ByRepo,
                _ => GroupingStrategy::Smart,
            };

            let groups = group_tasks(tasks, strategy);
            let filtered = filter_by_priority(groups, min_priority);
            let ready = filter_ready_groups(filtered);

            let groups_to_use = if ready.is_empty() {
                // Fall back to all filtered groups if none are "ready"
                filter_by_priority(
                    group_tasks(get_pending_tasks(pool, 100).await?, strategy),
                    min_priority,
                )
            } else {
                ready
            };

            if let Some(group) = get_next_group(&groups_to_use) {
                let output = match format.as_str() {
                    "markdown" | "md" => group.format_as_markdown(),
                    "json" => serde_json::to_string_pretty(group)?,
                    _ => group.format_for_zed(),
                };

                if copy {
                    #[cfg(feature = "clipboard")]
                    {
                        use clipboard::{ClipboardContext, ClipboardProvider};
                        let mut ctx: ClipboardContext = ClipboardProvider::new()?;
                        ctx.set_contents(output.clone())?;
                        println!("{}", "Copied to clipboard!".green());
                    }
                    #[cfg(not(feature = "clipboard"))]
                    {
                        println!(
                            "{}",
                            "Clipboard feature not enabled. Add --features clipboard".yellow()
                        );
                    }
                }

                println!("{}", output);
            } else {
                println!("{}", "No task groups found".yellow());
            }
        }

        TaskCommands::Groups {
            group_by,
            min_priority,
        } => {
            let tasks = get_pending_tasks(pool, 200).await?;

            if tasks.is_empty() {
                println!("{}", "No pending tasks".yellow());
                return Ok(());
            }

            let strategy = match group_by.as_str() {
                "file" => GroupingStrategy::ByFile,
                "category" => GroupingStrategy::ByCategory,
                "repo" => GroupingStrategy::ByRepo,
                _ => GroupingStrategy::Smart,
            };

            let groups = group_tasks(tasks, strategy);
            let filtered = filter_by_priority(groups, min_priority);

            println!(
                "\n{} {} task groups:\n",
                "📁".bold(),
                filtered.len().to_string().bold()
            );

            for (i, group) in filtered.iter().enumerate() {
                let priority_color = match group.combined_priority {
                    8..=10 => group.combined_priority.to_string().red(),
                    5..=7 => group.combined_priority.to_string().yellow(),
                    _ => group.combined_priority.to_string().white(),
                };

                println!(
                    "  {}. {} [P{}] ({} tasks)",
                    (i + 1).to_string().dimmed(),
                    group.name.cyan(),
                    priority_color,
                    group.tasks.len()
                );
            }

            println!(
                "\nRun {} to get the top group formatted for Zed",
                "rustcode task next".green()
            );
            println!();
        }

        TaskCommands::Done { id } => {
            let task_id = if id == "next" {
                let tasks = get_tasks_by_status(pool, "ready", 1).await?;
                tasks
                    .first()
                    .map(|t| t.id.clone())
                    .ok_or_else(|| anyhow::anyhow!("No ready tasks"))?
            } else {
                id
            };

            update_task_status(pool, &task_id, TaskStatus::Done).await?;
            println!(
                "{} Task {} marked as done",
                "✓".green(),
                task_id[..8].dimmed()
            );
        }

        TaskCommands::Ready { id } => {
            update_task_status(pool, &id, TaskStatus::Ready).await?;
            println!(
                "{} Task {} marked as ready for IDE",
                "✓".green(),
                id[..8].dimmed()
            );
        }
    }

    Ok(())
}
