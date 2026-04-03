//! Simplified Task Model
//!
//! Consolidates QueueItem, FileAnalysis, and TodoItem into a single Task type.
//! Designed for easy LLM processing and IDE export.

use serde::{Deserialize, Serialize};
use sqlx::{FromRow, PgPool};

use uuid::Uuid;

// ============================================================================
// Task Status & Types
// ============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type, Default)]
#[sqlx(type_name = "TEXT", rename_all = "snake_case")]
pub enum TaskStatus {
    #[default]
    Pending, // Not yet processed by LLM
    Processing, // LLM currently working on it
    Review,     // Needs human review before IDE handoff
    Ready,      // Ready to send to IDE agent
    Done,       // Completed
    Failed,     // Processing failed, may retry
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type, Default)]
#[sqlx(type_name = "TEXT", rename_all = "snake_case")]
pub enum TaskSource {
    #[default]
    Manual, // Manually added task
    Todo, // TODO/FIXME comment from code
    Scan, // Found during repo scan
    Idea, // Random thought/idea capture
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type)]
#[sqlx(type_name = "TEXT", rename_all = "snake_case")]
pub enum TaskCategory {
    Bug,
    Refactor,
    Feature,
    Docs,
    Test,
    Cleanup,
    Performance,
    Security,
    Other,
}

// ============================================================================
// Core Task Model
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct Task {
    pub id: String,

    // Content
    pub content: String,
    pub context: Option<String>,
    pub llm_suggestion: Option<String>,

    // Source tracking
    pub source_type: String,
    pub source_repo: Option<String>,
    pub source_file: Option<String>,
    pub source_line: Option<i32>,
    pub content_hash: Option<String>,

    // Status & Priority
    pub status: String,
    pub priority: i32,
    pub category: Option<String>,

    // Grouping
    pub group_id: Option<String>,
    pub group_reason: Option<String>,

    // Processing metadata
    pub retry_count: i32,
    pub last_error: Option<String>,
    pub tokens_used: Option<i32>,

    // Timestamps
    pub created_at: i64,
    pub updated_at: i64,
    pub processed_at: Option<i64>,
    pub completed_at: Option<i64>,
}

impl Task {
    pub fn new(content: impl Into<String>, source: TaskSource) -> Self {
        let content = content.into();
        let hash = Self::hash_content(&content);

        Self {
            id: Uuid::new_v4().to_string(),
            content,
            context: None,
            llm_suggestion: None,
            source_type: format!("{:?}", source).to_lowercase(),
            source_repo: None,
            source_file: None,
            source_line: None,
            content_hash: Some(hash),
            status: "pending".to_string(),
            priority: 5,
            category: None,
            group_id: None,
            group_reason: None,
            retry_count: 0,
            last_error: None,
            tokens_used: None,
            created_at: chrono::Utc::now().timestamp(),
            updated_at: chrono::Utc::now().timestamp(),
            processed_at: None,
            completed_at: None,
        }
    }

    pub fn with_context(mut self, context: impl Into<String>) -> Self {
        self.context = Some(context.into());
        self
    }

    pub fn with_source_file(
        mut self,
        repo: impl Into<String>,
        file: impl Into<String>,
        line: Option<i32>,
    ) -> Self {
        self.source_repo = Some(repo.into());
        self.source_file = Some(file.into());
        self.source_line = line;
        self
    }

    pub fn with_priority(mut self, priority: i32) -> Self {
        self.priority = priority.clamp(1, 10);
        self
    }

    pub fn with_category(mut self, category: TaskCategory) -> Self {
        self.category = Some(format!("{:?}", category).to_lowercase());
        self
    }

    fn hash_content(content: &str) -> String {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        content.hash(&mut hasher);
        format!("{:016x}", hasher.finish())
    }

    pub fn status_enum(&self) -> TaskStatus {
        match self.status.as_str() {
            "pending" => TaskStatus::Pending,
            "processing" => TaskStatus::Processing,
            "review" => TaskStatus::Review,
            "ready" => TaskStatus::Ready,
            "done" => TaskStatus::Done,
            "failed" => TaskStatus::Failed,
            _ => TaskStatus::Pending,
        }
    }
}

// ============================================================================
// Task Group (for batch IDE export)
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskGroup {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    pub tasks: Vec<Task>,
    pub combined_priority: i32,
    pub group_key: String, // What they're grouped by (file, category, etc.)
}

impl TaskGroup {
    pub fn new(key: impl Into<String>, tasks: Vec<Task>) -> Self {
        let key = key.into();
        let combined_priority = tasks.iter().map(|t| t.priority).max().unwrap_or(5);
        let name = if tasks.iter().any(|t| t.source_file.is_some()) {
            tasks
                .iter()
                .find_map(|t| t.source_file.clone())
                .unwrap_or_else(|| key.clone())
        } else {
            key.clone()
        };

        Self {
            id: Uuid::new_v4().to_string(),
            name: format!("{} ({} tasks)", name, tasks.len()),
            description: None,
            combined_priority,
            tasks,
            group_key: key,
        }
    }

    /// Format for Zed IDE chat paste
    pub fn format_for_zed(&self) -> String {
        let mut output = String::new();

        output.push_str(&format!(
            "=== {} | Priority: {} ===\n\n",
            self.name, self.combined_priority
        ));

        if let Some(desc) = &self.description {
            output.push_str(&format!("Summary: {}\n\n", desc));
        }

        output.push_str("Tasks:\n");
        for (i, task) in self.tasks.iter().enumerate() {
            let location = match (&task.source_file, task.source_line) {
                (Some(file), Some(line)) => format!(" [{}:{}]", file, line),
                (Some(file), None) => format!(" [{}]", file),
                _ => String::new(),
            };

            let category = task.category.as_deref().unwrap_or("task");
            output.push_str(&format!(
                "{}. [{}] {}{}\n",
                i + 1,
                category.to_uppercase(),
                task.content,
                location
            ));

            if let Some(suggestion) = &task.llm_suggestion {
                output.push_str(&format!("   Suggestion: {}\n", suggestion));
            }
        }

        // Add context files
        let files: Vec<String> = self
            .tasks
            .iter()
            .filter_map(|t| t.source_file.as_ref())
            .cloned()
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();

        if !files.is_empty() {
            output.push_str(&format!("\nRelevant files: {}\n", files.join(", ")));
        }

        output
    }

    /// Format as markdown for documentation
    pub fn format_as_markdown(&self) -> String {
        let mut output = String::new();

        output.push_str(&format!("## {}\n\n", self.name));
        output.push_str(&format!("**Priority:** {}/10\n\n", self.combined_priority));

        if let Some(desc) = &self.description {
            output.push_str(&format!("{}\n\n", desc));
        }

        output.push_str("### Tasks\n\n");
        for task in &self.tasks {
            let checkbox = if task.status == "done" { "[x]" } else { "[ ]" };
            let location = task
                .source_file
                .as_ref()
                .map(|f| format!(" (`{}`)", f))
                .unwrap_or_default();

            output.push_str(&format!("- {} {}{}\n", checkbox, task.content, location));
        }

        output
    }
}

// ============================================================================
// Database Operations
// ============================================================================

pub async fn create_task(pool: &PgPool, task: &Task) -> anyhow::Result<()> {
    sqlx::query(
        r#"
        INSERT INTO tasks (
            id, content, context, llm_suggestion,
            source_type, source_repo, source_file, source_line, content_hash,
            status, priority, category,
            group_id, group_reason,
            retry_count, last_error, tokens_used,
            created_at, updated_at
        ) VALUES (
            $1, $2, $3, $4,
            $5, $6, $7, $8, $9,
            $10, $11, $12,
            $13, $14,
            $15, $16, $17,
            $18, $19
        )
    "#,
    )
    .bind(&task.id)
    .bind(&task.content)
    .bind(&task.context)
    .bind(&task.llm_suggestion)
    .bind(&task.source_type)
    .bind(&task.source_repo)
    .bind(&task.source_file)
    .bind(task.source_line)
    .bind(&task.content_hash)
    .bind(&task.status)
    .bind(task.priority)
    .bind(&task.category)
    .bind(&task.group_id)
    .bind(&task.group_reason)
    .bind(task.retry_count)
    .bind(&task.last_error)
    .bind(task.tokens_used)
    .bind(task.created_at)
    .bind(task.updated_at)
    .execute(pool)
    .await?;

    Ok(())
}

pub async fn get_task(pool: &PgPool, id: &str) -> anyhow::Result<Option<Task>> {
    let task = sqlx::query_as::<_, Task>("SELECT * FROM tasks WHERE id = $1")
        .bind(id)
        .fetch_optional(pool)
        .await?;
    Ok(task)
}

pub async fn get_pending_tasks(pool: &PgPool, limit: i32) -> anyhow::Result<Vec<Task>> {
    let tasks = sqlx::query_as::<_, Task>(
        "SELECT * FROM tasks WHERE status IN ('pending', 'review', 'ready') ORDER BY priority DESC, created_at ASC LIMIT $1"
    )
    .bind(limit)
    .fetch_all(pool)
    .await?;
    Ok(tasks)
}

pub async fn get_tasks_by_status(
    pool: &PgPool,
    status: &str,
    limit: i32,
) -> anyhow::Result<Vec<Task>> {
    let tasks = sqlx::query_as::<_, Task>(
        "SELECT * FROM tasks WHERE status = $1 ORDER BY priority DESC, created_at ASC LIMIT $2",
    )
    .bind(status)
    .bind(limit)
    .fetch_all(pool)
    .await?;
    Ok(tasks)
}

pub async fn update_task_status(pool: &PgPool, id: &str, status: TaskStatus) -> anyhow::Result<()> {
    let status_str = format!("{:?}", status).to_lowercase();
    let now = chrono::Utc::now().timestamp();

    if matches!(status, TaskStatus::Done) {
        sqlx::query(
            "UPDATE tasks SET status = $1, updated_at = $2, completed_at = $3 WHERE id = $4",
        )
        .bind(&status_str)
        .bind(now)
        .bind(now)
        .bind(id)
        .execute(pool)
        .await?;
    } else {
        sqlx::query("UPDATE tasks SET status = $1, updated_at = $2 WHERE id = $3")
            .bind(&status_str)
            .bind(now)
            .bind(id)
            .execute(pool)
            .await?;
    }

    Ok(())
}

pub async fn update_task_analysis(
    pool: &PgPool,
    id: &str,
    priority: i32,
    category: &str,
    suggestion: Option<&str>,
    tokens: Option<i32>,
) -> anyhow::Result<()> {
    let now = chrono::Utc::now().timestamp();

    sqlx::query(
        "UPDATE tasks SET priority = $1, category = $2, llm_suggestion = $3, tokens_used = $4, status = 'review', processed_at = $5, updated_at = $6 WHERE id = $7"
    )
    .bind(priority)
    .bind(category)
    .bind(suggestion)
    .bind(tokens)
    .bind(now)
    .bind(now)
    .bind(id)
    .execute(pool)
    .await?;

    Ok(())
}

pub async fn mark_task_failed(pool: &PgPool, id: &str, error: &str) -> anyhow::Result<()> {
    let now = chrono::Utc::now().timestamp();

    sqlx::query(
        "UPDATE tasks SET status = 'failed', last_error = $1, retry_count = retry_count + 1, updated_at = $2 WHERE id = $3"
    )
    .bind(error)
    .bind(now)
    .bind(id)
    .execute(pool)
    .await?;

    Ok(())
}

pub async fn check_duplicate(pool: &PgPool, content_hash: &str) -> anyhow::Result<bool> {
    let count: (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM tasks WHERE content_hash = $1 AND status != 'done'")
            .bind(content_hash)
            .fetch_one(pool)
            .await?;

    Ok(count.0 > 0)
}

pub async fn assign_group(
    pool: &PgPool,
    task_id: &str,
    group_id: &str,
    reason: &str,
) -> anyhow::Result<()> {
    sqlx::query("UPDATE tasks SET group_id = $1, group_reason = $2, updated_at = $3 WHERE id = $4")
        .bind(group_id)
        .bind(reason)
        .bind(chrono::Utc::now().timestamp())
        .bind(task_id)
        .execute(pool)
        .await?;

    Ok(())
}

// ============================================================================
// Statistics
// ============================================================================

#[derive(Debug, Default, Serialize)]
pub struct TaskStats {
    pub pending: i64,
    pub processing: i64,
    pub review: i64,
    pub ready: i64,
    pub done: i64,
    pub failed: i64,
    pub total_tokens: i64,
}

pub async fn get_task_stats(pool: &PgPool) -> anyhow::Result<TaskStats> {
    let stats =
        sqlx::query_as::<_, (String, i64)>("SELECT status, COUNT(*) FROM tasks GROUP BY status")
            .fetch_all(pool)
            .await?;

    let mut result = TaskStats::default();
    for (status, count) in stats {
        match status.as_str() {
            "pending" => result.pending = count,
            "processing" => result.processing = count,
            "review" => result.review = count,
            "ready" => result.ready = count,
            "done" => result.done = count,
            "failed" => result.failed = count,
            _ => {}
        }
    }

    let tokens: (i64,) = sqlx::query_as("SELECT COALESCE(SUM(tokens_used), 0) FROM tasks")
        .fetch_one(pool)
        .await?;
    result.total_tokens = tokens.0;

    Ok(result)
}
