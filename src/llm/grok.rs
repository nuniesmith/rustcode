//! Grok LLM Analyzer
//!
//! Uses xAI's Grok API to analyze content and files for the processing queue.

use crate::queue::processor::{AnalysisResult, FileAnalysisResult, LlmAnalyzer};
use anyhow::Result;
use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::time::Duration;
use tracing::{debug, error};

// ============================================================================
// Configuration
// ============================================================================

const GROK_API_URL: &str = "https://api.x.ai/v1/chat/completions";
const DEFAULT_MODEL: &str = "grok-3-mini"; // Fast and cheap for analysis
const ANALYSIS_MODEL: &str = "grok-3"; // Better for complex file analysis

// ============================================================================
// Grok Client
// ============================================================================

pub struct GrokAnalyzer {
    client: Client,
    api_key: String,
    /// Track token usage for cost management
    tokens_used: std::sync::atomic::AtomicU64,
}

impl GrokAnalyzer {
    pub fn new(api_key: String) -> Self {
        let client = Client::builder()
            .timeout(Duration::from_secs(120))
            .build()
            .expect("Failed to build HTTP client");

        Self {
            client,
            api_key,
            tokens_used: std::sync::atomic::AtomicU64::new(0),
        }
    }

    pub fn tokens_used(&self) -> u64 {
        self.tokens_used.load(std::sync::atomic::Ordering::Relaxed)
    }

    async fn call_grok(
        &self,
        model: &str,
        system_prompt: &str,
        user_prompt: &str,
        json_mode: bool,
    ) -> Result<(String, Option<usize>)> {
        let mut payload = json!({
            "model": model,
            "messages": [
                {"role": "system", "content": system_prompt},
                {"role": "user", "content": user_prompt}
            ],
            "temperature": 0.3,
            "max_tokens": 2048
        });

        if json_mode {
            payload["response_format"] = json!({"type": "json_object"});
        }

        let response = self
            .client
            .post(GROK_API_URL)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .json(&payload)
            .send()
            .await?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            error!("Grok API error: {} - {}", status, body);
            anyhow::bail!("Grok API error: {}", status);
        }

        let result: GrokResponse = response.json().await?;

        // Track usage
        let tokens = result.usage.as_ref().map(|u| u.total_tokens as usize);
        if let Some(usage) = result.usage {
            self.tokens_used.fetch_add(
                usage.total_tokens as u64,
                std::sync::atomic::Ordering::Relaxed,
            );
            debug!(
                "Tokens used: {} (total: {})",
                usage.total_tokens,
                self.tokens_used()
            );
        }

        let content = result
            .choices
            .first()
            .map(|c| c.message.content.clone())
            .unwrap_or_default();

        Ok((content, tokens))
    }
}

#[derive(Debug, Deserialize)]
struct GrokResponse {
    choices: Vec<GrokChoice>,
    usage: Option<GrokUsage>,
}

#[derive(Debug, Deserialize)]
struct GrokChoice {
    message: GrokMessage,
}

#[derive(Debug, Deserialize)]
struct GrokMessage {
    content: String,
}

#[derive(Debug, Deserialize)]
struct GrokUsage {
    total_tokens: u32,
}

// ============================================================================
// LLM Analyzer Implementation
// ============================================================================

#[async_trait]
impl LlmAnalyzer for GrokAnalyzer {
    async fn analyze_content(&self, content: &str, source: &str) -> Result<AnalysisResult> {
        let system_prompt = r#"You are an expert content analyzer for a developer workflow system.
Analyze the provided content and extract structured information.

You must respond with valid JSON in this exact format:
{
    "summary": "A 1-2 sentence summary of the content",
    "tags": ["tag1", "tag2", "tag3"],
    "category": "one of: docs, code, idea, task, research, note, todo, bug, feature",
    "score": 1-10 importance/quality score,
    "action_items": ["any actionable items extracted"],
    "related_topics": ["related concepts or topics"],
    "suggested_project": "project name if one is mentioned or implied, otherwise null"
}

Guidelines:
- Tags should be lowercase, single words or hyphenated
- Score 1-3: low importance, 4-6: medium, 7-9: high, 10: critical
- Extract ALL action items, even implicit ones
- Be concise but comprehensive"#;

        let user_prompt = format!(
            "Content source: {}\n\nContent to analyze:\n{}",
            source, content
        );

        let (response, _tokens) = self
            .call_grok(DEFAULT_MODEL, system_prompt, &user_prompt, false)
            .await?;

        // Parse JSON response
        let parsed: serde_json::Value = serde_json::from_str(&response)
            .map_err(|e| anyhow::anyhow!("Failed to parse Grok response: {} - {}", e, response))?;

        Ok(AnalysisResult {
            summary: parsed["summary"].as_str().unwrap_or("").to_string(),
            tags: parsed["tags"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default(),
            category: parsed["category"].as_str().unwrap_or("note").to_string(),
            score: parsed["score"].as_i64().unwrap_or(5) as i32,
            action_items: parsed["action_items"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default(),
            related_topics: parsed["related_topics"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default(),
            suggested_project: parsed["suggested_project"]
                .as_str()
                .filter(|s| !s.is_empty())
                .map(String::from),
        })
    }

    async fn analyze_file(
        &self,
        content: &str,
        file_path: &str,
        language: &str,
    ) -> Result<FileAnalysisResult> {
        let system_prompt = r#"You are an expert code reviewer and analyzer.
Analyze the provided source code file and extract structured information.

You must respond with valid JSON in this exact format:
{
    "summary": "A 2-3 sentence description of what this file does",
    "purpose": "The primary role/purpose of this file (e.g., 'API endpoint handlers', 'Database models', 'Utility functions')",
    "language": "The programming language",
    "complexity_score": 1-10 complexity rating,
    "quality_score": 1-10 code quality rating,
    "security_notes": ["any security concerns found"],
    "improvements": ["suggested improvements"],
    "dependencies": ["external dependencies or imports used"],
    "exports": ["public API / exported items"],
    "tags": ["relevant tags for categorization"],
    "needs_attention": true/false if this file needs immediate attention
}

Guidelines:
- complexity_score: 1-3 simple, 4-6 moderate, 7-9 complex, 10 very complex
- quality_score: 1-3 poor, 4-6 acceptable, 7-9 good, 10 excellent
- needs_attention: true if there are security issues, bugs, or critical improvements needed
- Be specific about security notes and improvements
- List actual dependencies and exports, not generic descriptions"#;

        let user_prompt = format!(
            "File: {}\nLanguage: {}\n\nSource code:\n```{}\n{}\n```",
            file_path, language, language, content
        );

        // Use better model for file analysis
        let (response, tokens) = self
            .call_grok(ANALYSIS_MODEL, system_prompt, &user_prompt, true)
            .await?;

        let parsed: serde_json::Value = serde_json::from_str(&response)
            .map_err(|e| anyhow::anyhow!("Failed to parse Grok response: {} - {}", e, response))?;

        Ok(FileAnalysisResult {
            summary: parsed["summary"].as_str().unwrap_or("").to_string(),
            purpose: parsed["purpose"].as_str().unwrap_or("").to_string(),
            language: parsed["language"].as_str().unwrap_or(language).to_string(),
            complexity_score: parsed["complexity_score"].as_i64().unwrap_or(5) as i32,
            quality_score: parsed["quality_score"].as_i64().unwrap_or(5) as i32,
            security_notes: parsed["security_notes"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default(),
            improvements: parsed["improvements"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default(),
            dependencies: parsed["dependencies"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default(),
            exports: parsed["exports"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default(),
            tags: parsed["tags"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default(),
            needs_attention: parsed["needs_attention"].as_bool().unwrap_or(false),
            tokens_used: tokens,
        })
    }
}

// ============================================================================
// Specialized Analysis Functions
// ============================================================================

impl GrokAnalyzer {
    /// Analyze a TODO comment with context
    pub async fn analyze_todo(
        &self,
        todo_content: &str,
        file_path: &str,
        surrounding_code: Option<&str>,
    ) -> Result<TodoAnalysis> {
        let system_prompt = r#"You are analyzing a TODO/FIXME comment found in code.
Determine its priority and provide context.

Respond with JSON:
{
    "priority": 1-4 (1=critical, 2=high, 3=medium, 4=low),
    "context": "Explanation of what this TODO is about and why it matters",
    "estimated_effort": hours as float (e.g., 0.5, 2.0, 8.0),
    "category": "one of: bug, feature, refactor, optimization, documentation, security, testing",
    "suggested_task_title": "A clear task title for this TODO"
}

Priority guidelines:
- 1 (critical): Security issues, data loss risks, blocking bugs
- 2 (high): Important functionality, user-facing bugs
- 3 (medium): Nice to have improvements, code quality
- 4 (low): Minor cleanup, cosmetic issues"#;

        let user_prompt = if let Some(code) = surrounding_code {
            format!(
                "File: {}\nTODO: {}\n\nSurrounding code:\n```\n{}\n```",
                file_path, todo_content, code
            )
        } else {
            format!("File: {}\nTODO: {}", file_path, todo_content)
        };

        let (response, _tokens) = self
            .call_grok(DEFAULT_MODEL, system_prompt, &user_prompt, true)
            .await?;
        let parsed: serde_json::Value = serde_json::from_str(&response)?;

        Ok(TodoAnalysis {
            priority: parsed["priority"].as_i64().unwrap_or(3) as i32,
            context: parsed["context"].as_str().unwrap_or("").to_string(),
            estimated_effort: parsed["estimated_effort"].as_f64().unwrap_or(1.0) as f32,
            category: parsed["category"].as_str().unwrap_or("feature").to_string(),
            suggested_task_title: parsed["suggested_task_title"]
                .as_str()
                .unwrap_or("")
                .to_string(),
        })
    }

    /// Analyze repository for standardization issues
    pub async fn analyze_repo_standardization(
        &self,
        repo_name: &str,
        file_structure: &str,
        sample_files: &[(&str, &str)], // (path, content snippet)
    ) -> Result<StandardizationReport> {
        let system_prompt = r#"You are a code standardization expert.
Analyze the repository structure and sample files for consistency and best practices.

Respond with JSON:
{
    "health_score": 1-10 overall health score,
    "issues": [
        {
            "severity": "high/medium/low",
            "category": "naming/structure/documentation/testing/security/dependencies",
            "description": "Description of the issue",
            "recommendation": "How to fix it"
        }
    ],
    "strengths": ["What the repo does well"],
    "patterns": ["Detected patterns or conventions"],
    "missing_files": ["Expected files that are missing, e.g., README.md, .gitignore, tests/"]
}

Be specific and actionable in recommendations."#;

        let samples_text: String = sample_files
            .iter()
            .map(|(path, content)| format!("--- {} ---\n{}\n", path, content))
            .collect();

        let user_prompt = format!(
            "Repository: {}\n\nFile structure:\n{}\n\nSample files:\n{}",
            repo_name, file_structure, samples_text
        );

        let (response, _tokens) = self
            .call_grok(DEFAULT_MODEL, system_prompt, &user_prompt, true)
            .await?;
        let parsed: serde_json::Value = serde_json::from_str(&response)?;

        Ok(StandardizationReport {
            health_score: parsed["health_score"].as_i64().unwrap_or(5) as i32,
            issues: parsed["issues"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|v| {
                            Some(StandardizationIssue {
                                severity: v["severity"].as_str()?.to_string(),
                                category: v["category"].as_str()?.to_string(),
                                description: v["description"].as_str()?.to_string(),
                                recommendation: v["recommendation"].as_str()?.to_string(),
                            })
                        })
                        .collect()
                })
                .unwrap_or_default(),
            strengths: parsed["strengths"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default(),
            patterns: parsed["patterns"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default(),
            missing_files: parsed["missing_files"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default(),
        })
    }

    /// Generate a project plan from analyzed content
    pub async fn generate_project_plan(
        &self,
        project_name: &str,
        notes: &[&str],
        existing_tasks: &[&str],
    ) -> Result<ProjectPlan> {
        let system_prompt = r#"You are a project planning assistant.
Based on the notes and existing tasks, create a structured project plan.

Respond with JSON:
{
    "summary": "Brief project summary",
    "goals": ["Main project goals"],
    "phases": [
        {
            "name": "Phase name",
            "description": "What this phase accomplishes",
            "tasks": ["Task descriptions"],
            "estimated_days": number
        }
    ],
    "risks": ["Potential risks or blockers"],
    "dependencies": ["External dependencies"],
    "success_criteria": ["How to measure success"]
}"#;

        let notes_text = notes.join("\n\n");
        let tasks_text = existing_tasks.join("\n");

        let user_prompt = format!(
            "Project: {}\n\nNotes and ideas:\n{}\n\nExisting tasks:\n{}",
            project_name, notes_text, tasks_text
        );

        let (response, _tokens) = self
            .call_grok(DEFAULT_MODEL, system_prompt, &user_prompt, true)
            .await?;
        let parsed: serde_json::Value = serde_json::from_str(&response)?;

        Ok(ProjectPlan {
            summary: parsed["summary"].as_str().unwrap_or("").to_string(),
            goals: parsed["goals"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default(),
            phases: parsed["phases"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|v| {
                            Some(ProjectPhase {
                                name: v["name"].as_str()?.to_string(),
                                description: v["description"].as_str()?.to_string(),
                                tasks: v["tasks"]
                                    .as_array()
                                    .map(|t| {
                                        t.iter()
                                            .filter_map(|x| x.as_str().map(String::from))
                                            .collect()
                                    })
                                    .unwrap_or_default(),
                                estimated_days: v["estimated_days"].as_i64().unwrap_or(7) as i32,
                            })
                        })
                        .collect()
                })
                .unwrap_or_default(),
            risks: parsed["risks"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default(),
            dependencies: parsed["dependencies"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default(),
            success_criteria: parsed["success_criteria"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default(),
        })
    }
}

// ============================================================================
// Analysis Result Types
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TodoAnalysis {
    pub priority: i32,
    pub context: String,
    pub estimated_effort: f32,
    pub category: String,
    pub suggested_task_title: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StandardizationReport {
    pub health_score: i32,
    pub issues: Vec<StandardizationIssue>,
    pub strengths: Vec<String>,
    pub patterns: Vec<String>,
    pub missing_files: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StandardizationIssue {
    pub severity: String,
    pub category: String,
    pub description: String,
    pub recommendation: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectPlan {
    pub summary: String,
    pub goals: Vec<String>,
    pub phases: Vec<ProjectPhase>,
    pub risks: Vec<String>,
    pub dependencies: Vec<String>,
    pub success_criteria: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectPhase {
    pub name: String,
    pub description: String,
    pub tasks: Vec<String>,
    pub estimated_days: i32,
}
