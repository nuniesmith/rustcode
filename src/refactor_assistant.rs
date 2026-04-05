// # Refactoring Assistant Module
//
// AI-powered code smell detection and refactoring suggestions.
//
// ## Features
//
// - Detect code smells and anti-patterns
// - Suggest specific refactoring strategies
// - Generate refactoring plans
// - Extract function/module suggestions
// - Complexity reduction recommendations
//
// ## Usage
//
// ```rust,no_run
// use rustcode::refactor_assistant::RefactorAssistant;
// use rustcode::db::Database;
//
// #[tokio::main]
// async fn main() -> anyhow::Result<()> {
//     let db = Database::new("data/rustcode.db").await?;
//     let assistant = RefactorAssistant::new(db).await?;
//
//     // Analyze file for refactoring opportunities
//     let analysis = assistant.analyze_file("src/legacy.rs").await?;
//     println!("{}", analysis.format_markdown());
//
//     Ok(())
// }
// ```

use crate::db::Database;
use crate::grok_client::GrokClient;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

// Refactoring assistant with AI-powered analysis
pub struct RefactorAssistant {
    grok_client: GrokClient,
}

// Complete refactoring analysis for a file or directory
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RefactoringAnalysis {
    // File or directory analyzed
    pub path: String,
    // Code smells detected
    pub code_smells: Vec<CodeSmell>,
    // Refactoring suggestions
    pub suggestions: Vec<RefactoringSuggestion>,
    // Overall complexity score (0-100, lower is better)
    pub complexity_score: f64,
    // Maintainability score (0-100, higher is better)
    pub maintainability_score: f64,
    // Priority recommendations
    pub priorities: Vec<String>,
    // Estimated effort
    pub estimated_effort: EffortEstimate,
    // Tokens used in the analysis
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tokens_used: Option<usize>,
}

// Detected code smell
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodeSmell {
    // Smell type
    pub smell_type: CodeSmellType,
    // Severity
    pub severity: SmellSeverity,
    // Description of the issue
    pub description: String,
    // Location in code
    pub location: Option<CodeLocation>,
    // Impact on code quality
    pub impact: String,
}

// Code smell types
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum CodeSmellType {
    // Function too long
    LongFunction,
    // Too many parameters
    LongParameterList,
    // Duplicated code
    DuplicatedCode,
    // Large class/module
    LargeModule,
    // Feature envy (using another module's data heavily)
    FeatureEnvy,
    // Primitive obsession (should use domain types)
    PrimitiveObsession,
    // Deep nesting
    DeepNesting,
    // Complex conditionals
    ComplexConditional,
    // Shotgun surgery (change requires many small edits)
    ShotgunSurgery,
    // Divergent change (module changes for many reasons)
    DivergentChange,
    // Dead code
    DeadCode,
    // Magic numbers
    MagicNumbers,
    // God object (does too much)
    GodObject,
    // Tight coupling
    TightCoupling,
    // Missing error handling
    MissingErrorHandling,
    // Overuse of unwrap/expect
    UnsafeUnwrapping,
}

// Code smell severity
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub enum SmellSeverity {
    // Critical issue affecting correctness
    Critical,
    // High priority refactoring needed
    High,
    // Medium priority
    Medium,
    // Low priority
    Low,
}

// Location in code
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodeLocation {
    // File path
    pub file: String,
    // Start line
    pub line_start: Option<usize>,
    // End line
    pub line_end: Option<usize>,
    // Function or item name
    pub item_name: Option<String>,
}

// Refactoring suggestion
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RefactoringSuggestion {
    // Suggestion type
    pub refactoring_type: RefactoringType,
    // Title/summary
    pub title: String,
    // Detailed description
    pub description: String,
    // Benefits of applying this refactoring
    pub benefits: Vec<String>,
    // Step-by-step instructions
    pub steps: Vec<String>,
    // Code example (before/after)
    pub example: Option<RefactoringExample>,
    // Estimated effort
    pub effort: EffortEstimate,
    // Priority
    pub priority: RefactoringPriority,
}

// Refactoring types
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum RefactoringType {
    // Extract function from code block
    ExtractFunction,
    // Extract module from large file
    ExtractModule,
    // Rename for clarity
    Rename,
    // Inline unnecessary abstraction
    Inline,
    // Replace conditional with polymorphism
    ReplaceConditional,
    // Introduce parameter object
    IntroduceParameterObject,
    // Replace magic number with constant
    ReplaceMagicNumber,
    // Decompose conditional
    DecomposeConditional,
    // Consolidate duplicate code
    ConsolidateDuplicate,
    // Simplify complex expression
    SimplifyExpression,
    // Remove dead code
    RemoveDeadCode,
    // Improve error handling
    ImproveErrorHandling,
    // Reduce coupling
    ReduceCoupling,
    // Split large function
    SplitFunction,
}

// Refactoring priority
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub enum RefactoringPriority {
    // Must do (correctness/security)
    Critical,
    // Should do soon (maintainability)
    High,
    // Nice to have (code quality)
    Medium,
    // Optional (minor improvement)
    Low,
}

// Effort estimate
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum EffortEstimate {
    // Less than 30 minutes
    Trivial,
    // 30 minutes to 2 hours
    Small,
    // 2 hours to 1 day
    Medium,
    // 1-3 days
    Large,
    // More than 3 days
    VeryLarge,
}

// Refactoring example (before/after)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RefactoringExample {
    // Code before refactoring
    pub before: String,
    // Code after refactoring
    pub after: String,
    // Explanation of changes
    pub explanation: String,
}

// Refactoring plan for multiple files
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RefactoringPlan {
    // Plan title
    pub title: String,
    // Overall goal
    pub goal: String,
    // Files to refactor
    pub files: Vec<String>,
    // Ordered steps
    pub steps: Vec<PlanStep>,
    // Total estimated effort
    pub total_effort: EffortEstimate,
    // Expected benefits
    pub benefits: Vec<String>,
    // Risks and mitigation
    pub risks: Vec<Risk>,
}

// Step in refactoring plan
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanStep {
    // Step number
    pub step_number: usize,
    // Step description
    pub description: String,
    // Files affected
    pub affected_files: Vec<String>,
    // Effort estimate
    pub effort: EffortEstimate,
    // Dependencies (previous steps required)
    pub dependencies: Vec<usize>,
    // Validation criteria
    pub validation: Vec<String>,
}

// Risk in refactoring plan
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Risk {
    // Risk description
    pub description: String,
    // Mitigation strategy
    pub mitigation: String,
    // Severity
    pub severity: SmellSeverity,
}

impl RefactorAssistant {
    // Create a new refactoring assistant
    pub async fn new(db: Database) -> Result<Self> {
        let grok_client = GrokClient::from_env(db).await?;
        Ok(Self { grok_client })
    }

    // Analyze a file for refactoring opportunities
    pub async fn analyze_file(&self, file_path: impl AsRef<Path>) -> Result<RefactoringAnalysis> {
        let file_path = file_path.as_ref();
        let content = std::fs::read_to_string(file_path)
            .with_context(|| format!("Failed to read file: {}", file_path.display()))?;

        self.analyze_content(file_path.to_string_lossy().to_string(), &content)
            .await
    }

    // Analyze directory for refactoring opportunities
    pub async fn analyze_directory(
        &self,
        dir_path: impl AsRef<Path>,
    ) -> Result<Vec<RefactoringAnalysis>> {
        let dir_path = dir_path.as_ref();
        let mut analyses = Vec::new();

        for entry in walkdir::WalkDir::new(dir_path)
            .follow_links(true)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_file())
        {
            let path = entry.path();
            if self.is_source_file(path) {
                if let Ok(analysis) = self.analyze_file(path).await {
                    analyses.push(analysis);
                }
            }
        }

        Ok(analyses)
    }

    // Analyze code content
    async fn analyze_content(
        &self,
        file_path: String,
        content: &str,
    ) -> Result<RefactoringAnalysis> {
        let prompt = format!(
            r#"Analyze this code for refactoring opportunities. Return ONLY valid JSON with this structure:
{{
  "code_smells": [
    {{
      "smell_type": "long_function|long_parameter_list|duplicated_code|large_module|feature_envy|primitive_obsession|deep_nesting|complex_conditional|shotgun_surgery|divergent_change|dead_code|magic_numbers|god_object|tight_coupling|missing_error_handling|unsafe_unwrapping",
      "severity": "critical|high|medium|low",
      "description": "detailed description",
      "location": {{
        "file": "file.rs",
        "line_start": 42,
        "line_end": 67,
        "item_name": "function_name"
      }},
      "impact": "impact on code quality"
    }}
  ],
  "suggestions": [
    {{
      "refactoring_type": "extract_function|extract_module|rename|inline|replace_conditional|introduce_parameter_object|replace_magic_number|decompose_conditional|consolidate_duplicate|simplify_expression|remove_dead_code|improve_error_handling|reduce_coupling|split_function",
      "title": "short title",
      "description": "detailed description",
      "benefits": ["benefit1", "benefit2"],
      "steps": ["step1", "step2"],
      "example": {{
        "before": "code before",
        "after": "code after",
        "explanation": "what changed"
      }},
      "effort": "trivial|small|medium|large|very_large",
      "priority": "critical|high|medium|low"
    }}
  ],
  "complexity_score": 0-100,
  "maintainability_score": 0-100,
  "priorities": ["most important refactoring", "second priority"],
  "estimated_effort": "trivial|small|medium|large|very_large"
}}

Code to analyze:
```
{}
```

Focus on detecting:
1. Functions longer than 50 lines
2. Functions with >4 parameters
3. Duplicated code blocks
4. Deep nesting (>4 levels)
5. Complex conditionals
6. Magic numbers
7. Missing error handling
8. Excessive use of unwrap()
9. Tight coupling between modules
10. Dead/unused code

For each smell, provide:
- Specific refactoring suggestion
- Step-by-step instructions
- Before/after code example
- Effort estimate
- Priority level"#,
            content
        );

        let tracked = self
            .grok_client
            .ask_tracked(&prompt, Some(content), "refactor_analysis")
            .await
            .context("Failed to analyze code for refactoring")?;

        let mut analysis = self.parse_refactoring_response(&tracked.content, file_path)?;
        analysis.tokens_used = Some(tracked.total_tokens as usize);
        Ok(analysis)
    }

    // Generate a refactoring plan for multiple files
    pub async fn generate_plan(
        &self,
        path: impl AsRef<Path>,
        goal: &str,
    ) -> Result<RefactoringPlan> {
        let path = path.as_ref();

        // Collect files to analyze
        let files: Vec<String> = if path.is_file() {
            vec![path.to_string_lossy().to_string()]
        } else {
            walkdir::WalkDir::new(path)
                .follow_links(true)
                .into_iter()
                .filter_map(|e| e.ok())
                .filter(|e| e.file_type().is_file())
                .filter(|e| self.is_source_file(e.path()))
                .map(|e| e.path().to_string_lossy().to_string())
                .collect()
        };

        let prompt = format!(
            r#"Create a comprehensive refactoring plan for this codebase. Return ONLY valid JSON:
{{
  "title": "plan title",
  "goal": "overall goal",
  "files": ["file1.rs", "file2.rs"],
  "steps": [
    {{
      "step_number": 1,
      "description": "step description",
      "affected_files": ["file.rs"],
      "effort": "trivial|small|medium|large|very_large",
      "dependencies": [0],
      "validation": ["how to verify this step"]
    }}
  ],
  "total_effort": "trivial|small|medium|large|very_large",
  "benefits": ["benefit1", "benefit2"],
  "risks": [
    {{
      "description": "risk description",
      "mitigation": "mitigation strategy",
      "severity": "critical|high|medium|low"
    }}
  ]
}}

Goal: {}
Files: {}

Analyze the codebase and create a step-by-step plan that:
1. Identifies current issues
2. Proposes specific refactorings
3. Orders steps logically (dependencies)
4. Estimates effort per step
5. Identifies risks and mitigations
6. Provides validation criteria

Make the plan:
- Actionable (specific, not vague)
- Incremental (small, testable steps)
- Safe (low-risk changes first)
- Valuable (high-impact improvements prioritized)"#,
            goal,
            files.join(", ")
        );

        let response = self
            .grok_client
            .ask(&prompt, None)
            .await
            .context("Failed to generate refactoring plan")?;

        self.parse_plan_response(&response, goal)
    }

    // Suggest extract function refactoring
    pub async fn suggest_extract_function(
        &self,
        file_path: impl AsRef<Path>,
        start_line: usize,
        end_line: usize,
    ) -> Result<RefactoringSuggestion> {
        let file_path = file_path.as_ref();
        let content = std::fs::read_to_string(file_path)
            .with_context(|| format!("Failed to read file: {}", file_path.display()))?;

        let lines: Vec<&str> = content.lines().collect();
        let code_block = lines[start_line.saturating_sub(1)..end_line.min(lines.len())].join("\n");

        let prompt = format!(
            r#"Suggest a function extraction refactoring for this code block. Return JSON:
{{
  "refactoring_type": "extract_function",
  "title": "Extract ... into separate function",
  "description": "detailed description",
  "benefits": ["benefit1", "benefit2"],
  "steps": ["step1", "step2"],
  "example": {{
    "before": "original code",
    "after": "refactored code with new function",
    "explanation": "what changed"
  }},
  "effort": "trivial|small|medium",
  "priority": "high|medium|low"
}}

Code block (lines {}-{}):
```
{}
```

Suggest:
1. Appropriate function name
2. Parameters needed
3. Return type
4. Where to place the new function
5. How to call it from original location"#,
            start_line, end_line, code_block
        );

        let response = self
            .grok_client
            .ask(&prompt, Some(&content))
            .await
            .context("Failed to suggest extract function")?;

        self.parse_suggestion_response(&response)
    }

    // Parse refactoring analysis response
    fn parse_refactoring_response(
        &self,
        response: &str,
        file_path: String,
    ) -> Result<RefactoringAnalysis> {
        let json_str = self.extract_json(response);

        match serde_json::from_str::<serde_json::Value>(&json_str) {
            Ok(json) => {
                let code_smells = json["code_smells"]
                    .as_array()
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|cs| {
                                Some(CodeSmell {
                                    smell_type: self.parse_smell_type(cs["smell_type"].as_str()?),
                                    severity: self.parse_severity(cs["severity"].as_str()?),
                                    description: cs["description"].as_str()?.to_string(),
                                    location: cs["location"].as_object().map(|loc| CodeLocation {
                                        file: loc
                                            .get("file")
                                            .and_then(|v| v.as_str())
                                            .unwrap_or("")
                                            .to_string(),
                                        line_start: loc
                                            .get("line_start")
                                            .and_then(|v| v.as_u64())
                                            .map(|n| n as usize),
                                        line_end: loc
                                            .get("line_end")
                                            .and_then(|v| v.as_u64())
                                            .map(|n| n as usize),
                                        item_name: loc
                                            .get("item_name")
                                            .and_then(|v| v.as_str())
                                            .map(String::from),
                                    }),
                                    impact: cs["impact"].as_str()?.to_string(),
                                })
                            })
                            .collect()
                    })
                    .unwrap_or_default();

                let suggestions = json["suggestions"]
                    .as_array()
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|s| self.parse_suggestion_from_json(s).ok())
                            .collect()
                    })
                    .unwrap_or_default();

                let complexity_score = json["complexity_score"].as_f64().unwrap_or(50.0);
                let maintainability_score = json["maintainability_score"].as_f64().unwrap_or(50.0);

                let priorities = json["priorities"]
                    .as_array()
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|p| p.as_str().map(String::from))
                            .collect()
                    })
                    .unwrap_or_default();

                let estimated_effort = json["estimated_effort"]
                    .as_str()
                    .map(|s| self.parse_effort(s))
                    .unwrap_or(EffortEstimate::Medium);

                Ok(RefactoringAnalysis {
                    path: file_path,
                    code_smells,
                    suggestions,
                    complexity_score,
                    maintainability_score,
                    priorities,
                    estimated_effort,
                    tokens_used: None,
                })
            }
            Err(_) => {
                // Fallback to basic analysis
                Ok(RefactoringAnalysis {
                    path: file_path,
                    code_smells: vec![],
                    suggestions: vec![],
                    complexity_score: 50.0,
                    maintainability_score: 50.0,
                    priorities: vec!["Review AI response for details".to_string()],
                    estimated_effort: EffortEstimate::Medium,
                    tokens_used: None,
                })
            }
        }
    }

    // Parse suggestion response
    fn parse_suggestion_response(&self, response: &str) -> Result<RefactoringSuggestion> {
        let json_str = self.extract_json(response);

        match serde_json::from_str::<serde_json::Value>(&json_str) {
            Ok(json) => self.parse_suggestion_from_json(&json),
            Err(_) => Ok(RefactoringSuggestion {
                refactoring_type: RefactoringType::ExtractFunction,
                title: "See AI response".to_string(),
                description: response.to_string(),
                benefits: vec![],
                steps: vec![],
                example: None,
                effort: EffortEstimate::Medium,
                priority: RefactoringPriority::Medium,
            }),
        }
    }

    // Parse suggestion from JSON value
    fn parse_suggestion_from_json(
        &self,
        json: &serde_json::Value,
    ) -> Result<RefactoringSuggestion> {
        Ok(RefactoringSuggestion {
            refactoring_type: self.parse_refactoring_type(
                json["refactoring_type"]
                    .as_str()
                    .unwrap_or("extract_function"),
            ),
            title: json["title"].as_str().unwrap_or("").to_string(),
            description: json["description"].as_str().unwrap_or("").to_string(),
            benefits: json["benefits"]
                .as_array()
                .map(|arr| {
                    arr.iter()
                        .filter_map(|b| b.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default(),
            steps: json["steps"]
                .as_array()
                .map(|arr| {
                    arr.iter()
                        .filter_map(|s| s.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default(),
            example: json["example"].as_object().map(|ex| RefactoringExample {
                before: ex["before"].as_str().unwrap_or("").to_string(),
                after: ex["after"].as_str().unwrap_or("").to_string(),
                explanation: ex["explanation"].as_str().unwrap_or("").to_string(),
            }),
            effort: self.parse_effort(json["effort"].as_str().unwrap_or("medium")),
            priority: self.parse_priority(json["priority"].as_str().unwrap_or("medium")),
        })
    }

    // Parse plan response
    fn parse_plan_response(&self, response: &str, goal: &str) -> Result<RefactoringPlan> {
        let json_str = self.extract_json(response);

        match serde_json::from_str::<serde_json::Value>(&json_str) {
            Ok(json) => {
                let steps = json["steps"]
                    .as_array()
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|s| {
                                Some(PlanStep {
                                    step_number: s["step_number"].as_u64()? as usize,
                                    description: s["description"].as_str()?.to_string(),
                                    affected_files: s["affected_files"]
                                        .as_array()?
                                        .iter()
                                        .filter_map(|f| f.as_str().map(String::from))
                                        .collect(),
                                    effort: self.parse_effort(s["effort"].as_str()?),
                                    dependencies: s["dependencies"]
                                        .as_array()
                                        .map(|arr| {
                                            arr.iter()
                                                .filter_map(|d| d.as_u64().map(|n| n as usize))
                                                .collect()
                                        })
                                        .unwrap_or_default(),
                                    validation: s["validation"]
                                        .as_array()
                                        .map(|arr| {
                                            arr.iter()
                                                .filter_map(|v| v.as_str().map(String::from))
                                                .collect()
                                        })
                                        .unwrap_or_default(),
                                })
                            })
                            .collect()
                    })
                    .unwrap_or_default();

                let risks = json["risks"]
                    .as_array()
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|r| {
                                Some(Risk {
                                    description: r["description"].as_str()?.to_string(),
                                    mitigation: r["mitigation"].as_str()?.to_string(),
                                    severity: self.parse_severity(r["severity"].as_str()?),
                                })
                            })
                            .collect()
                    })
                    .unwrap_or_default();

                Ok(RefactoringPlan {
                    title: json["title"]
                        .as_str()
                        .unwrap_or("Refactoring Plan")
                        .to_string(),
                    goal: goal.to_string(),
                    files: json["files"]
                        .as_array()
                        .map(|arr| {
                            arr.iter()
                                .filter_map(|f| f.as_str().map(String::from))
                                .collect()
                        })
                        .unwrap_or_default(),
                    steps,
                    total_effort: self
                        .parse_effort(json["total_effort"].as_str().unwrap_or("medium")),
                    benefits: json["benefits"]
                        .as_array()
                        .map(|arr| {
                            arr.iter()
                                .filter_map(|b| b.as_str().map(String::from))
                                .collect()
                        })
                        .unwrap_or_default(),
                    risks,
                })
            }
            Err(_) => Ok(RefactoringPlan {
                title: "Refactoring Plan".to_string(),
                goal: goal.to_string(),
                files: vec![],
                steps: vec![],
                total_effort: EffortEstimate::Medium,
                benefits: vec![],
                risks: vec![],
            }),
        }
    }

    // Extract JSON from response
    fn extract_json(&self, response: &str) -> String {
        if let Some(start) = response.find("```json") {
            if let Some(end) = response[start..].find("```\n") {
                let json_start = start + 7;
                return response[json_start..start + end].trim().to_string();
            }
        }

        if let Some(start) = response.find('{') {
            if let Some(end) = response.rfind('}') {
                return response[start..=end].to_string();
            }
        }

        response.to_string()
    }

    // Check if file is a source file
    fn is_source_file(&self, path: &Path) -> bool {
        if let Some(ext) = path.extension() {
            matches!(
                ext.to_str().unwrap_or(""),
                "rs" | "py" | "js" | "ts" | "java" | "kt" | "go"
            )
        } else {
            false
        }
    }

    // Parse code smell type
    fn parse_smell_type(&self, s: &str) -> CodeSmellType {
        match s.to_lowercase().replace("_", "").as_str() {
            "longfunction" => CodeSmellType::LongFunction,
            "longparameterlist" => CodeSmellType::LongParameterList,
            "duplicatedcode" => CodeSmellType::DuplicatedCode,
            "largemodule" => CodeSmellType::LargeModule,
            "featureenvy" => CodeSmellType::FeatureEnvy,
            "primitiveobsession" => CodeSmellType::PrimitiveObsession,
            "deepnesting" => CodeSmellType::DeepNesting,
            "complexconditional" => CodeSmellType::ComplexConditional,
            "shotgunsurgery" => CodeSmellType::ShotgunSurgery,
            "divergentchange" => CodeSmellType::DivergentChange,
            "deadcode" => CodeSmellType::DeadCode,
            "magicnumbers" => CodeSmellType::MagicNumbers,
            "godobject" => CodeSmellType::GodObject,
            "tightcoupling" => CodeSmellType::TightCoupling,
            "missingerrorhandling" => CodeSmellType::MissingErrorHandling,
            "unsafeunwrapping" => CodeSmellType::UnsafeUnwrapping,
            _ => CodeSmellType::LongFunction,
        }
    }

    // Parse refactoring type
    fn parse_refactoring_type(&self, s: &str) -> RefactoringType {
        match s.to_lowercase().replace("_", "").as_str() {
            "extractfunction" => RefactoringType::ExtractFunction,
            "extractmodule" => RefactoringType::ExtractModule,
            "rename" => RefactoringType::Rename,
            "inline" => RefactoringType::Inline,
            "replaceconditional" => RefactoringType::ReplaceConditional,
            "introduceparameterobject" => RefactoringType::IntroduceParameterObject,
            "replacemagicnumber" => RefactoringType::ReplaceMagicNumber,
            "decomposeconditional" => RefactoringType::DecomposeConditional,
            "consolidateduplicate" => RefactoringType::ConsolidateDuplicate,
            "simplifyexpression" => RefactoringType::SimplifyExpression,
            "removedeadcode" => RefactoringType::RemoveDeadCode,
            "improveerrorhandling" => RefactoringType::ImproveErrorHandling,
            "reducecoupling" => RefactoringType::ReduceCoupling,
            "splitfunction" => RefactoringType::SplitFunction,
            _ => RefactoringType::ExtractFunction,
        }
    }

    // Parse severity
    fn parse_severity(&self, s: &str) -> SmellSeverity {
        match s.to_lowercase().as_str() {
            "critical" => SmellSeverity::Critical,
            "high" => SmellSeverity::High,
            "medium" => SmellSeverity::Medium,
            "low" => SmellSeverity::Low,
            _ => SmellSeverity::Medium,
        }
    }

    // Parse effort
    fn parse_effort(&self, s: &str) -> EffortEstimate {
        match s.to_lowercase().replace("_", "").as_str() {
            "trivial" => EffortEstimate::Trivial,
            "small" => EffortEstimate::Small,
            "medium" => EffortEstimate::Medium,
            "large" => EffortEstimate::Large,
            "verylarge" => EffortEstimate::VeryLarge,
            _ => EffortEstimate::Medium,
        }
    }

    // Parse priority
    fn parse_priority(&self, s: &str) -> RefactoringPriority {
        match s.to_lowercase().as_str() {
            "critical" => RefactoringPriority::Critical,
            "high" => RefactoringPriority::High,
            "medium" => RefactoringPriority::Medium,
            "low" => RefactoringPriority::Low,
            _ => RefactoringPriority::Medium,
        }
    }
}

impl RefactoringAnalysis {
    // Format analysis as markdown
    pub fn format_markdown(&self) -> String {
        let mut output = String::new();

        output.push_str(&format!("# Refactoring Analysis: {}\n\n", self.path));

        // Scores
        output.push_str("## Metrics\n\n");
        output.push_str(&format!(
            "- **Complexity Score:** {:.1}/100 ({})\n",
            self.complexity_score,
            if self.complexity_score > 70.0 {
                "Needs simplification"
            } else if self.complexity_score > 40.0 {
                "Moderate complexity"
            } else {
                "Good"
            }
        ));
        output.push_str(&format!(
            "- **Maintainability Score:** {:.1}/100 ({})\n",
            self.maintainability_score,
            if self.maintainability_score > 75.0 {
                "Good"
            } else if self.maintainability_score > 50.0 {
                "Acceptable"
            } else {
                "Needs improvement"
            }
        ));
        output.push_str(&format!(
            "- **Estimated Effort:** {:?}\n\n",
            self.estimated_effort
        ));

        // Code smells
        if !self.code_smells.is_empty() {
            output.push_str(&format!(
                "## Code Smells Found ({})\n\n",
                self.code_smells.len()
            ));

            for smell in &self.code_smells {
                let icon = match smell.severity {
                    SmellSeverity::Critical => "🔴",
                    SmellSeverity::High => "🟠",
                    SmellSeverity::Medium => "🟡",
                    SmellSeverity::Low => "🔵",
                };

                output.push_str(&format!(
                    "### {} {:?} - {:?}\n\n",
                    icon, smell.severity, smell.smell_type
                ));
                output.push_str(&format!("**Description:** {}\n\n", smell.description));

                if let Some(loc) = &smell.location {
                    if let (Some(start), Some(end)) = (loc.line_start, loc.line_end) {
                        output.push_str(&format!("**Location:** Lines {}-{}", start, end));
                        if let Some(name) = &loc.item_name {
                            output.push_str(&format!(" ({})", name));
                        }
                        output.push_str("\n\n");
                    }
                }

                output.push_str(&format!("**Impact:** {}\n\n", smell.impact));
            }
        }

        // Suggestions
        if !self.suggestions.is_empty() {
            output.push_str(&format!(
                "## Refactoring Suggestions ({})\n\n",
                self.suggestions.len()
            ));

            for (i, suggestion) in self.suggestions.iter().enumerate() {
                output.push_str(&format!(
                    "### {}. {} ({:?})\n\n",
                    i + 1,
                    suggestion.title,
                    suggestion.priority
                ));
                output.push_str(&format!("**Type:** {:?}\n", suggestion.refactoring_type));
                output.push_str(&format!("**Effort:** {:?}\n\n", suggestion.effort));
                output.push_str(&format!("{}\n\n", suggestion.description));

                if !suggestion.benefits.is_empty() {
                    output.push_str("**Benefits:**\n");
                    for benefit in &suggestion.benefits {
                        output.push_str(&format!("- {}\n", benefit));
                    }
                    output.push('\n');
                }

                if !suggestion.steps.is_empty() {
                    output.push_str("**Steps:**\n");
                    for (j, step) in suggestion.steps.iter().enumerate() {
                        output.push_str(&format!("{}. {}\n", j + 1, step));
                    }
                    output.push('\n');
                }

                if let Some(example) = &suggestion.example {
                    output.push_str("**Example:**\n\n");
                    output.push_str("Before:\n```rust\n");
                    output.push_str(&example.before);
                    output.push_str("\n```\n\n");
                    output.push_str("After:\n```rust\n");
                    output.push_str(&example.after);
                    output.push_str("\n```\n\n");
                    output.push_str(&format!("*{}*\n\n", example.explanation));
                }
            }
        }

        // Priorities
        if !self.priorities.is_empty() {
            output.push_str("## Priority Recommendations\n\n");
            for (i, priority) in self.priorities.iter().enumerate() {
                output.push_str(&format!("{}. {}\n", i + 1, priority));
            }
            output.push('\n');
        }

        output
    }
}

impl RefactoringPlan {
    // Format plan as markdown
    pub fn format_markdown(&self) -> String {
        let mut output = String::new();

        output.push_str(&format!("# Refactoring Plan: {}\n\n", self.title));
        output.push_str(&format!("**Goal:** {}\n\n", self.goal));
        output.push_str(&format!("**Total Effort:** {:?}\n", self.total_effort));
        output.push_str(&format!("**Files Affected:** {}\n\n", self.files.len()));

        // Benefits
        if !self.benefits.is_empty() {
            output.push_str("## Expected Benefits\n\n");
            for benefit in &self.benefits {
                output.push_str(&format!("- {}\n", benefit));
            }
            output.push('\n');
        }

        // Steps
        if !self.steps.is_empty() {
            output.push_str(&format!("## Steps ({} total)\n\n", self.steps.len()));
            for step in &self.steps {
                output.push_str(&format!(
                    "### Step {}: {}\n\n",
                    step.step_number, step.description
                ));
                output.push_str(&format!("**Effort:** {:?}\n", step.effort));

                if !step.affected_files.is_empty() {
                    output.push_str(&format!("**Files:** {}\n", step.affected_files.join(", ")));
                }

                if !step.dependencies.is_empty() {
                    output.push_str(&format!(
                        "**Dependencies:** Steps {:?}\n",
                        step.dependencies
                    ));
                }

                if !step.validation.is_empty() {
                    output.push_str("\n**Validation:**\n");
                    for validation in &step.validation {
                        output.push_str(&format!("- {}\n", validation));
                    }
                }
                output.push('\n');
            }
        }

        // Risks
        if !self.risks.is_empty() {
            output.push_str("## Risks & Mitigation\n\n");
            for risk in &self.risks {
                let icon = match risk.severity {
                    SmellSeverity::Critical => "🔴",
                    SmellSeverity::High => "🟠",
                    SmellSeverity::Medium => "🟡",
                    SmellSeverity::Low => "🔵",
                };
                output.push_str(&format!(
                    "{} **{:?}:** {}\n",
                    icon, risk.severity, risk.description
                ));
                output.push_str(&format!("   *Mitigation:* {}\n\n", risk.mitigation));
            }
        }

        output
    }
}

impl std::fmt::Display for CodeSmellType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CodeSmellType::LongFunction => write!(f, "Long Function"),
            CodeSmellType::LongParameterList => write!(f, "Long Parameter List"),
            CodeSmellType::DuplicatedCode => write!(f, "Duplicated Code"),
            CodeSmellType::LargeModule => write!(f, "Large Module"),
            CodeSmellType::FeatureEnvy => write!(f, "Feature Envy"),
            CodeSmellType::PrimitiveObsession => write!(f, "Primitive Obsession"),
            CodeSmellType::DeepNesting => write!(f, "Deep Nesting"),
            CodeSmellType::ComplexConditional => write!(f, "Complex Conditional"),
            CodeSmellType::ShotgunSurgery => write!(f, "Shotgun Surgery"),
            CodeSmellType::DivergentChange => write!(f, "Divergent Change"),
            CodeSmellType::DeadCode => write!(f, "Dead Code"),
            CodeSmellType::MagicNumbers => write!(f, "Magic Numbers"),
            CodeSmellType::GodObject => write!(f, "God Object"),
            CodeSmellType::TightCoupling => write!(f, "Tight Coupling"),
            CodeSmellType::MissingErrorHandling => write!(f, "Missing Error Handling"),
            CodeSmellType::UnsafeUnwrapping => write!(f, "Unsafe Unwrapping"),
        }
    }
}

impl std::fmt::Display for RefactoringType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RefactoringType::ExtractFunction => write!(f, "Extract Function"),
            RefactoringType::ExtractModule => write!(f, "Extract Module"),
            RefactoringType::Rename => write!(f, "Rename"),
            RefactoringType::Inline => write!(f, "Inline"),
            RefactoringType::ReplaceConditional => write!(f, "Replace Conditional"),
            RefactoringType::IntroduceParameterObject => write!(f, "Introduce Parameter Object"),
            RefactoringType::ReplaceMagicNumber => write!(f, "Replace Magic Number"),
            RefactoringType::DecomposeConditional => write!(f, "Decompose Conditional"),
            RefactoringType::ConsolidateDuplicate => write!(f, "Consolidate Duplicate"),
            RefactoringType::SimplifyExpression => write!(f, "Simplify Expression"),
            RefactoringType::RemoveDeadCode => write!(f, "Remove Dead Code"),
            RefactoringType::ImproveErrorHandling => write!(f, "Improve Error Handling"),
            RefactoringType::ReduceCoupling => write!(f, "Reduce Coupling"),
            RefactoringType::SplitFunction => write!(f, "Split Function"),
        }
    }
}
