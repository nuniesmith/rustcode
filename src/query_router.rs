// # Query Router Module
//
// Routes user queries based on intent classification to optimize cost and performance.
//
// ## Features
//
// - Intent classification (greeting, search, analysis, etc.)
// - Semantic cache integration
// - Cost-aware routing decisions
// - Metrics tracking
//
// ## Usage
//
// ```rust,no_run
// use rustcode::query_router::{QueryRouter, Action, UserContext};
//
// #[tokio::main]
// async fn main() -> anyhow::Result<()> {
//     # let pool = todo!();
//     # let cache_path = "cache.db";
//     let mut router = QueryRouter::new(pool, cache_path).await?;
//     let user_context = UserContext::default();
//
//     let action = router.route("What should I work on next?", &user_context).await?;
//     match action {
//         Action::CallGrok(context) => {
//             // Make API call with context
//         }
//         Action::SearchDatabase(query) => {
//             // Search local database
//         }
//         Action::DirectResponse(response) => {
//             // Return canned response
//         }
//         _ => {}
//     }
//
//     Ok(())
// }
// ```

use crate::context_rag::{Context, ContextBuilder};
use crate::response_cache::ResponseCache;
use anyhow::{Context as AnyhowContext, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tracing::{debug, info};

// Query intent classification
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum QueryIntent {
    // Simple greeting or acknowledgment
    Greeting,

    // Search for notes or tasks in database
    NoteSearch,

    // Repository or file analysis request
    RepoAnalysis,

    // Task generation or recommendation
    TaskGeneration,

    // Code explanation or question
    CodeQuestion,

    // Direct factual question (can be answered without LLM)
    DirectAnswer,

    // GitHub issues query
    GitHubIssues,

    // GitHub pull requests query
    GitHubPRs,

    // GitHub repositories query
    GitHubRepos,

    // GitHub commits query
    GitHubCommits,

    // GitHub search query
    GitHubSearch,

    // Generic query requiring LLM
    Generic,
}

// Action to take based on routing decision
#[derive(Debug)]
pub enum Action {
    // Return cached response
    CachedResponse(String),

    // Return direct response without LLM
    DirectResponse(String),

    // Search database only
    SearchDatabase(String),

    // Call Grok with full context
    CallGrok(Context),

    // Call Grok with minimal context (for simple questions)
    CallGrokMinimal(String),
}

// User context for routing decisions
#[derive(Debug, Clone, Default)]
pub struct UserContext {
    // Recent queries (for conversation context)
    pub recent_queries: Vec<String>,

    // Current repository being worked on
    pub current_repo: Option<String>,

    // Active project
    pub current_project: Option<String>,

    // User preferences
    pub preferences: HashMap<String, String>,
}

// Query routing statistics
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoutingStats {
    pub total_queries: u64,
    pub cache_hits: u64,
    pub direct_responses: u64,
    pub database_searches: u64,
    pub grok_calls: u64,
    pub grok_minimal_calls: u64,
    pub cost_saved_usd: f64,
}

// Query router with intelligent routing
pub struct QueryRouter {
    // Database connection pool
    #[allow(dead_code)]
    pool: Option<sqlx::PgPool>,

    // Response cache
    cache: Option<ResponseCache>,

    // Context builder
    context_builder: Option<ContextBuilder>,

    // Routing statistics
    stats: RoutingStats,
}

impl QueryRouter {
    // Create a new query router
    pub async fn new(pool: sqlx::PgPool, cache_path: &str) -> Result<Self> {
        let cache = ResponseCache::new(cache_path)
            .await
            .context("Failed to initialize response cache")?;

        let context_builder =
            ContextBuilder::new(crate::db::core::Database::from_pool(pool.clone()));

        Ok(Self {
            pool: Some(pool),
            cache: Some(cache),
            context_builder: Some(context_builder),
            stats: RoutingStats::default(),
        })
    }

    // Route a query to the appropriate action
    pub async fn route(&mut self, query: &str, user_context: &UserContext) -> Result<Action> {
        self.stats.total_queries += 1;

        info!("Routing query: {}", query);

        // Step 1: Check response cache (exact match)
        if let Some(cached) = self.check_cache(query).await? {
            self.stats.cache_hits += 1;
            self.stats.cost_saved_usd += Self::estimate_grok_cost(query);
            info!("Cache HIT - returning cached response");
            return Ok(Action::CachedResponse(cached));
        }

        // Step 2: Classify intent
        let intent = self.classify_intent(query, user_context);
        debug!("Classified intent: {:?}", intent);

        // Step 3: Route based on intent
        match intent {
            QueryIntent::Greeting => {
                self.stats.direct_responses += 1;
                self.stats.cost_saved_usd += Self::estimate_grok_cost(query);
                Ok(Action::DirectResponse(self.generate_greeting(query)))
            }

            QueryIntent::NoteSearch => {
                self.stats.database_searches += 1;
                self.stats.cost_saved_usd += Self::estimate_grok_cost(query);
                Ok(Action::SearchDatabase(query.to_string()))
            }

            QueryIntent::DirectAnswer => {
                self.stats.direct_responses += 1;
                self.stats.cost_saved_usd += Self::estimate_grok_cost(query);
                Ok(Action::DirectResponse(self.generate_direct_answer(query)))
            }

            QueryIntent::RepoAnalysis => {
                self.stats.grok_calls += 1;
                let context = self.build_analysis_context(query, user_context).await?;
                Ok(Action::CallGrok(context))
            }

            QueryIntent::TaskGeneration => {
                self.stats.grok_calls += 1;
                let context = self.build_task_context(query, user_context).await?;
                Ok(Action::CallGrok(context))
            }

            QueryIntent::CodeQuestion => {
                self.stats.grok_minimal_calls += 1;
                Ok(Action::CallGrokMinimal(query.to_string()))
            }

            QueryIntent::GitHubIssues => {
                self.stats.database_searches += 1;
                self.stats.cost_saved_usd += 0.005; // Estimated LLM cost saved
                Ok(Action::SearchDatabase(query.to_string()))
            }

            QueryIntent::GitHubPRs => {
                self.stats.database_searches += 1;
                self.stats.cost_saved_usd += 0.005;
                Ok(Action::SearchDatabase(query.to_string()))
            }

            QueryIntent::GitHubRepos => {
                self.stats.database_searches += 1;
                self.stats.cost_saved_usd += 0.005;
                Ok(Action::SearchDatabase(query.to_string()))
            }

            QueryIntent::GitHubCommits => {
                self.stats.database_searches += 1;
                self.stats.cost_saved_usd += 0.005;
                Ok(Action::SearchDatabase(query.to_string()))
            }

            QueryIntent::GitHubSearch => {
                self.stats.database_searches += 1;
                self.stats.cost_saved_usd += 0.005;
                Ok(Action::SearchDatabase(query.to_string()))
            }

            QueryIntent::Generic => {
                self.stats.grok_calls += 1;
                let context = self.build_generic_context(query, user_context).await?;
                Ok(Action::CallGrok(context))
            }
        }
    }

    // Classify query intent
    fn classify_intent(&self, query: &str, _context: &UserContext) -> QueryIntent {
        let lower = query.to_lowercase();
        let words: Vec<&str> = lower.split_whitespace().collect();

        // Greeting patterns
        if self.is_greeting(&lower, &words) {
            return QueryIntent::Greeting;
        }

        // GitHub patterns (check before note search)
        if self.is_github_issues(&lower, &words) {
            return QueryIntent::GitHubIssues;
        }

        if self.is_github_prs(&lower, &words) {
            return QueryIntent::GitHubPRs;
        }

        if self.is_github_repos(&lower, &words) {
            return QueryIntent::GitHubRepos;
        }

        if self.is_github_commits(&lower, &words) {
            return QueryIntent::GitHubCommits;
        }

        if self.is_github_search(&lower, &words) {
            return QueryIntent::GitHubSearch;
        }

        // Note search patterns
        if self.is_note_search(&lower, &words) {
            return QueryIntent::NoteSearch;
        }

        // Repository analysis patterns
        if self.is_repo_analysis(&lower, &words) {
            return QueryIntent::RepoAnalysis;
        }

        // Task generation patterns
        if self.is_task_generation(&lower, &words) {
            return QueryIntent::TaskGeneration;
        }

        // Code question patterns
        if self.is_code_question(&lower, &words) {
            return QueryIntent::CodeQuestion;
        }

        // Direct answer patterns
        if self.is_direct_answer(&lower, &words) {
            return QueryIntent::DirectAnswer;
        }

        // Default to generic
        QueryIntent::Generic
    }

    // Intent detection helpers

    fn is_greeting(&self, query: &str, _words: &[&str]) -> bool {
        matches!(
            query,
            "hi" | "hello" | "hey" | "thanks" | "thank you" | "bye" | "goodbye"
        ) || query.starts_with("thanks ")
            || query.starts_with("thank you ")
    }

    fn is_note_search(&self, _query: &str, words: &[&str]) -> bool {
        let search_keywords = ["find", "search", "show", "list", "get"];
        let note_keywords = ["note", "notes", "idea", "ideas", "thought", "thoughts"];

        let has_search = words.iter().any(|w| search_keywords.contains(w));
        let has_note = words.iter().any(|w| note_keywords.contains(w));

        has_search && has_note
    }

    fn is_repo_analysis(&self, query: &str, words: &[&str]) -> bool {
        let analysis_keywords = ["analyze", "review", "score", "check", "audit", "inspect"];
        let repo_keywords = ["file", "code", "repository", "repo", "project"];

        let has_analysis = words.iter().any(|w| analysis_keywords.contains(w));
        let has_repo = words.iter().any(|w| repo_keywords.contains(w));

        // Also check for file extensions
        let has_file_ext = query.contains(".rs")
            || query.contains(".js")
            || query.contains(".py")
            || query.contains(".go");

        (has_analysis && has_repo) || has_file_ext
    }

    fn is_task_generation(&self, query: &str, words: &[&str]) -> bool {
        let task_keywords = [
            "task",
            "tasks",
            "todo",
            "work",
            "next",
            "recommend",
            "should",
        ];
        let generation_keywords = ["what", "generate", "create", "suggest"];

        let has_task = words.iter().any(|w| task_keywords.contains(w));
        let has_generation = words.iter().any(|w| generation_keywords.contains(w));

        has_task || (has_generation && query.contains("should"))
    }

    fn is_code_question(&self, _query: &str, words: &[&str]) -> bool {
        let question_words = ["how", "why", "what", "when", "where", "explain"];
        let code_words = ["function", "class", "method", "variable", "work", "works"];

        let has_question = words.iter().any(|w| question_words.contains(w));
        let has_code = words.iter().any(|w| code_words.contains(w));

        has_question && has_code
    }

    fn is_direct_answer(&self, query: &str, _words: &[&str]) -> bool {
        // FAQ-style questions
        let faq_patterns = [
            "how do i",
            "how to",
            "what is rustcode",
            "what does rustcode do",
            "help",
            "usage",
        ];

        faq_patterns.iter().any(|pattern| query.contains(pattern))
    }

    fn is_github_issues(&self, _query: &str, words: &[&str]) -> bool {
        let issue_keywords = ["issue", "issues", "bug", "bugs"];

        words.iter().any(|w| issue_keywords.contains(w))
    }

    fn is_github_prs(&self, _query: &str, words: &[&str]) -> bool {
        let pr_keywords = ["pr", "prs", "pull", "merge"];

        words.iter().any(|w| pr_keywords.contains(w))
    }

    fn is_github_repos(&self, query: &str, words: &[&str]) -> bool {
        let repo_keywords = ["repository", "repositories", "repos"];
        let github_keywords = ["github", "gh"];

        let has_repo = words.iter().any(|w| repo_keywords.contains(w));
        let has_github = words.iter().any(|w| github_keywords.contains(w));

        (has_repo && has_github) || query.contains("github repo")
    }

    fn is_github_commits(&self, _query: &str, words: &[&str]) -> bool {
        let commit_keywords = ["commit", "commits", "committed"];
        let github_keywords = ["github", "gh"];

        let has_commit = words.iter().any(|w| commit_keywords.contains(w));
        let has_github = words.iter().any(|w| github_keywords.contains(w));

        has_commit && has_github
    }

    fn is_github_search(&self, query: &str, words: &[&str]) -> bool {
        let search_keywords = ["search", "find"];
        let github_keywords = ["github", "gh"];

        let has_search = words.iter().any(|w| search_keywords.contains(w));
        let has_github = words.iter().any(|w| github_keywords.contains(w));

        (has_search && has_github) || query.contains("search github")
    }

    // Cache operations

    async fn check_cache(&self, query: &str) -> Result<Option<String>> {
        match &self.cache {
            Some(cache) => cache
                .get(query, "query_response")
                .await
                .context("Failed to check cache"),
            None => Ok(None),
        }
    }

    pub async fn cache_response(&self, query: &str, response: &str) -> Result<()> {
        match &self.cache {
            Some(cache) => cache
                .set(query, "query_response", response, Some(24))
                .await
                .context("Failed to cache response"),
            None => Ok(()),
        }
    }

    // Context builders

    async fn build_analysis_context(
        &self,
        _query: &str,
        user_context: &UserContext,
    ) -> Result<Context> {
        let cb = self
            .context_builder
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("No context builder configured"))?;
        let mut builder = cb.clone();

        // Add repository context if specified
        if let Some(repo) = &user_context.current_repo {
            builder = builder.with_repository(repo);
        } else {
            builder = builder.with_all_repositories();
        }

        // Build context
        builder
            .build()
            .await
            .context("Failed to build analysis context")
    }

    async fn build_task_context(
        &self,
        _query: &str,
        user_context: &UserContext,
    ) -> Result<Context> {
        let cb = self
            .context_builder
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("No context builder configured"))?;
        let mut builder = cb
            .clone()
            .with_notes()
            .with_all_repositories()
            .max_files(50);

        // Add project filter if specified
        if let Some(project) = &user_context.current_project {
            builder = builder.with_path(project);
        }

        builder
            .build()
            .await
            .context("Failed to build task context")
    }

    async fn build_generic_context(
        &self,
        _query: &str,
        _user_context: &UserContext,
    ) -> Result<Context> {
        let cb = self
            .context_builder
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("No context builder configured"))?;
        cb.clone()
            .with_notes()
            .with_recent_files(10)
            .max_files(20)
            .build()
            .await
            .context("Failed to build generic context")
    }

    // Response generators

    fn generate_greeting(&self, query: &str) -> String {
        let lower = query.to_lowercase();

        if lower.contains("thank") {
            "You're welcome! Let me know if you need anything else.".to_string()
        } else if lower.contains("bye") || lower.contains("goodbye") {
            "Goodbye! Happy coding!".to_string()
        } else {
            "Hello! I'm Rustassistant. I can help you manage notes, analyze code, and recommend tasks. What would you like to do?".to_string()
        }
    }

    fn generate_direct_answer(&self, query: &str) -> String {
        let lower = query.to_lowercase();

        if lower.contains("what is rustcode") || lower.contains("what does rustcode do") {
            return "Rustassistant is a developer workflow management tool. I can:\n\
                    • Manage notes and ideas with tags\n\
                    • Track repositories and analyze code\n\
                    • Generate and prioritize tasks\n\
                    • Provide code recommendations using AI\n\n\
                    Try: `rustcode note add \"Your idea\" --tags feature`"
                .to_string();
        }

        if lower.contains("help") || lower.contains("usage") {
            return "Common commands:\n\
                    • rustcode note add \"text\" --tags tag1,tag2\n\
                    • rustcode note list\n\
                    • rustcode repo add /path/to/repo\n\
                    • rustcode tasks list\n\
                    • rustcode next\n\
                    • rustcode stats\n\n\
                    Use --help on any command for more details."
                .to_string();
        }

        "I don't have a direct answer for that. Let me search for relevant information.".to_string()
    }

    // Cost estimation

    fn estimate_grok_cost(_query: &str) -> f64 {
        // Rough estimate: avg 100k tokens per full query
        let avg_input_tokens = 100_000.0;
        let avg_output_tokens = 50_000.0;

        let input_cost = (avg_input_tokens / 1_000_000.0) * 0.20;
        let output_cost = (avg_output_tokens / 1_000_000.0) * 0.50;

        input_cost + output_cost
    }

    // Get routing statistics
    pub fn get_stats(&self) -> &RoutingStats {
        &self.stats
    }

    // Reset statistics
    pub fn reset_stats(&mut self) {
        self.stats = RoutingStats::default();
    }

    // Get cache hit rate
    pub fn cache_hit_rate(&self) -> f64 {
        if self.stats.total_queries == 0 {
            return 0.0;
        }
        (self.stats.cache_hits as f64 / self.stats.total_queries as f64) * 100.0
    }

    // Get percentage of queries that avoided Grok
    pub fn cost_avoidance_rate(&self) -> f64 {
        if self.stats.total_queries == 0 {
            return 0.0;
        }
        let avoided =
            self.stats.cache_hits + self.stats.direct_responses + self.stats.database_searches;
        (avoided as f64 / self.stats.total_queries as f64) * 100.0
    }
}

impl Default for RoutingStats {
    fn default() -> Self {
        Self {
            total_queries: 0,
            cache_hits: 0,
            direct_responses: 0,
            database_searches: 0,
            grok_calls: 0,
            grok_minimal_calls: 0,
            cost_saved_usd: 0.0,
        }
    }
}

#[cfg(test)]
#[allow(unreachable_code, unused_variables)]
mod tests {
    use super::*;

    fn create_test_router() -> QueryRouter {
        QueryRouter {
            pool: None,
            cache: None,
            context_builder: None,
            stats: RoutingStats::default(),
        }
    }

    fn create_test_context() -> UserContext {
        UserContext::default()
    }

    #[test]
    fn test_greeting_detection() {
        let router = create_test_router();

        assert_eq!(
            router.classify_intent("hi", &create_test_context()),
            QueryIntent::Greeting
        );
        assert_eq!(
            router.classify_intent("thanks!", &create_test_context()),
            QueryIntent::Greeting
        );
        assert_eq!(
            router.classify_intent("thank you so much", &create_test_context()),
            QueryIntent::Greeting
        );
    }

    #[test]
    fn test_note_search_detection() {
        let router = create_test_router();

        assert_eq!(
            router.classify_intent("find my notes about refactoring", &create_test_context()),
            QueryIntent::NoteSearch
        );
        assert_eq!(
            router.classify_intent("search for ideas tagged feature", &create_test_context()),
            QueryIntent::NoteSearch
        );
    }

    #[test]
    fn test_repo_analysis_detection() {
        let router = create_test_router();

        assert_eq!(
            router.classify_intent("analyze auth.rs", &create_test_context()),
            QueryIntent::RepoAnalysis
        );
        assert_eq!(
            router.classify_intent("review the code in src/main.rs", &create_test_context()),
            QueryIntent::RepoAnalysis
        );
    }

    #[test]
    fn test_task_generation_detection() {
        let router = create_test_router();

        assert_eq!(
            router.classify_intent("what should I work on next?", &create_test_context()),
            QueryIntent::TaskGeneration
        );
        assert_eq!(
            router.classify_intent("recommend a task", &create_test_context()),
            QueryIntent::TaskGeneration
        );
    }

    #[test]
    fn test_cost_estimation() {
        let cost = QueryRouter::estimate_grok_cost("test query");
        assert!(cost > 0.0);
        assert!(cost < 1.0); // Should be a few cents
    }
}
