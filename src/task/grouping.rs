//! Task Grouping Logic
//!
//! Groups related tasks for efficient batch processing and IDE handoff.
//! Tasks can be grouped by: file, category, repository, or similarity.

use crate::task::{Task, TaskGroup};
use std::collections::HashMap;

// ============================================================================
// Grouping Strategies
// ============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroupingStrategy {
    /// Group by source file (default for code tasks)
    ByFile,
    /// Group by category (bug, refactor, etc.)
    ByCategory,
    /// Group by repository
    ByRepo,
    /// Smart grouping: file first, then category
    Smart,
}

// ============================================================================
// Core Grouping Functions
// ============================================================================

/// Group tasks using the specified strategy
pub fn group_tasks(tasks: Vec<Task>, strategy: GroupingStrategy) -> Vec<TaskGroup> {
    match strategy {
        GroupingStrategy::ByFile => group_by_file(tasks),
        GroupingStrategy::ByCategory => group_by_category(tasks),
        GroupingStrategy::ByRepo => group_by_repo(tasks),
        GroupingStrategy::Smart => smart_grouping(tasks),
    }
}

/// Group tasks by source file
pub fn group_by_file(tasks: Vec<Task>) -> Vec<TaskGroup> {
    let mut groups: HashMap<String, Vec<Task>> = HashMap::new();

    for task in tasks {
        let key = task
            .source_file
            .clone()
            .unwrap_or_else(|| "no-file".to_string());
        groups.entry(key).or_default().push(task);
    }

    let mut result: Vec<TaskGroup> = groups
        .into_iter()
        .map(|(key, tasks)| TaskGroup::new(key, tasks))
        .collect();

    // Sort by combined priority (highest first)
    result.sort_by(|a, b| b.combined_priority.cmp(&a.combined_priority));
    result
}

/// Group tasks by category
pub fn group_by_category(tasks: Vec<Task>) -> Vec<TaskGroup> {
    let mut groups: HashMap<String, Vec<Task>> = HashMap::new();

    for task in tasks {
        let key = task
            .category
            .clone()
            .unwrap_or_else(|| "uncategorized".to_string());
        groups.entry(key).or_default().push(task);
    }

    let mut result: Vec<TaskGroup> = groups
        .into_iter()
        .map(|(key, tasks)| TaskGroup::new(key, tasks))
        .collect();

    result.sort_by(|a, b| b.combined_priority.cmp(&a.combined_priority));
    result
}

/// Group tasks by repository
pub fn group_by_repo(tasks: Vec<Task>) -> Vec<TaskGroup> {
    let mut groups: HashMap<String, Vec<Task>> = HashMap::new();

    for task in tasks {
        let key = task
            .source_repo
            .clone()
            .unwrap_or_else(|| "no-repo".to_string());
        groups.entry(key).or_default().push(task);
    }

    let mut result: Vec<TaskGroup> = groups
        .into_iter()
        .map(|(key, tasks)| TaskGroup::new(key, tasks))
        .collect();

    result.sort_by(|a, b| b.combined_priority.cmp(&a.combined_priority));
    result
}

/// Smart grouping: prioritizes file-based groups, then falls back to category
/// Also identifies cross-cutting concerns that span multiple files
pub fn smart_grouping(tasks: Vec<Task>) -> Vec<TaskGroup> {
    let mut file_groups: HashMap<String, Vec<Task>> = HashMap::new();
    let mut no_file_tasks: Vec<Task> = Vec::new();

    // First pass: separate by file
    for task in tasks {
        if let Some(file) = &task.source_file {
            file_groups.entry(file.clone()).or_default().push(task);
        } else {
            no_file_tasks.push(task);
        }
    }

    let mut result: Vec<TaskGroup> = Vec::new();

    // Create file-based groups
    for (file, tasks) in file_groups {
        // If multiple tasks in same file, group them
        if tasks.len() > 1 {
            result.push(TaskGroup::new(file, tasks));
        } else {
            // Single task in file - check if it can join a category group
            no_file_tasks.extend(tasks);
        }
    }

    // Group remaining by category
    if !no_file_tasks.is_empty() {
        let category_groups = group_by_category(no_file_tasks);
        result.extend(category_groups);
    }

    // Sort by priority
    result.sort_by(|a, b| b.combined_priority.cmp(&a.combined_priority));
    result
}

// ============================================================================
// Filtering Helpers
// ============================================================================

/// Filter groups by minimum priority
pub fn filter_by_priority(groups: Vec<TaskGroup>, min_priority: i32) -> Vec<TaskGroup> {
    groups
        .into_iter()
        .filter(|g| g.combined_priority >= min_priority)
        .collect()
}

/// Get only groups that are ready for IDE export
pub fn filter_ready_groups(groups: Vec<TaskGroup>) -> Vec<TaskGroup> {
    groups
        .into_iter()
        .map(|mut g| {
            g.tasks
                .retain(|t| t.status == "ready" || t.status == "review");
            g
        })
        .filter(|g| !g.tasks.is_empty())
        .collect()
}

/// Get the next group to work on (highest priority)
pub fn get_next_group(groups: &[TaskGroup]) -> Option<&TaskGroup> {
    groups.first()
}

/// Get top N groups by priority
pub fn get_top_groups(groups: &[TaskGroup], n: usize) -> Vec<&TaskGroup> {
    groups.iter().take(n).collect()
}

// ============================================================================
// Similarity Detection (for smarter grouping)
// ============================================================================

/// Check if two tasks are likely related based on content similarity
pub fn tasks_are_similar(task1: &Task, task2: &Task) -> bool {
    // Same file = definitely related
    if task1.source_file.is_some() && task1.source_file == task2.source_file {
        return true;
    }

    // Same category + same repo = likely related
    if task1.category == task2.category && task1.source_repo == task2.source_repo {
        // Check for keyword overlap
        let content1_lower = task1.content.to_lowercase();
        let words1: std::collections::HashSet<String> = content1_lower
            .split_whitespace()
            .filter(|w| w.len() > 3)
            .map(|s| s.to_string())
            .collect();

        let content2_lower = task2.content.to_lowercase();
        let words2: std::collections::HashSet<String> = content2_lower
            .split_whitespace()
            .filter(|w| w.len() > 3)
            .map(|s| s.to_string())
            .collect();

        let overlap = words1.intersection(&words2).count();
        let min_size = words1.len().min(words2.len());

        // More than 30% word overlap = similar
        if min_size > 0 && overlap as f32 / min_size as f32 > 0.3 {
            return true;
        }
    }

    false
}

/// Find all tasks similar to a given task
pub fn find_similar_tasks<'a>(target: &Task, candidates: &'a [Task]) -> Vec<&'a Task> {
    candidates
        .iter()
        .filter(|t| t.id != target.id && tasks_are_similar(target, t))
        .collect()
}

// ============================================================================
// Group Enhancement
// ============================================================================

/// Add LLM-generated description to a group
pub fn enhance_group_description(group: &mut TaskGroup, description: String) {
    group.description = Some(description);
}

/// Merge two groups if they're related
pub fn merge_groups(group1: TaskGroup, group2: TaskGroup) -> TaskGroup {
    let combined_tasks: Vec<Task> = group1.tasks.into_iter().chain(group2.tasks).collect();

    let new_key = format!("{} + {}", group1.group_key, group2.group_key);
    TaskGroup::new(new_key, combined_tasks)
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::task::TaskSource;

    fn make_task(content: &str, file: Option<&str>, category: Option<&str>, priority: i32) -> Task {
        let mut task = Task::new(content, TaskSource::Manual).with_priority(priority);

        if let Some(f) = file {
            task.source_file = Some(f.to_string());
        }
        if let Some(c) = category {
            task.category = Some(c.to_string());
        }
        task
    }

    #[test]
    fn test_group_by_file() {
        let tasks = vec![
            make_task("Fix bug 1", Some("src/main.rs"), None, 8),
            make_task("Fix bug 2", Some("src/main.rs"), None, 5),
            make_task("Fix bug 3", Some("src/lib.rs"), None, 3),
        ];

        let groups = group_by_file(tasks);

        assert_eq!(groups.len(), 2);
        // Highest priority group first (main.rs has max priority 8)
        assert!(groups[0].group_key.contains("main.rs"));
        assert_eq!(groups[0].tasks.len(), 2);
    }

    #[test]
    fn test_smart_grouping() {
        let tasks = vec![
            make_task(
                "Refactor function A",
                Some("src/main.rs"),
                Some("refactor"),
                5,
            ),
            make_task(
                "Refactor function B",
                Some("src/main.rs"),
                Some("refactor"),
                6,
            ),
            make_task("Add docs", None, Some("docs"), 3),
            make_task("More docs", None, Some("docs"), 4),
            make_task("Single file task", Some("src/other.rs"), Some("bug"), 2),
        ];

        let groups = smart_grouping(tasks);

        // Should have: main.rs group, docs group, and single tasks merged into category
        assert!(groups.len() >= 2);
        // main.rs group should have highest priority
        assert_eq!(groups[0].combined_priority, 6);
    }

    #[test]
    fn test_filter_by_priority() {
        let tasks = vec![
            make_task("High priority", Some("src/a.rs"), None, 9),
            make_task("Low priority", Some("src/b.rs"), None, 2),
        ];

        let groups = group_by_file(tasks);
        let filtered = filter_by_priority(groups, 5);

        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].combined_priority, 9);
    }
}
