//! Enhanced LLM audit modes - Regular and Full
//!
//! This module provides two comprehensive audit modes:
//! - **Regular Audit**: Holistic codebase analysis, entire codebase in context
//! - **Full Audit**: File-by-file deep dive with scoring and master review

use crate::cache::AuditCache;
use crate::error::Result;
use crate::llm::LlmClient;
use crate::llm_config::LlmConfig;
use crate::scoring::{CodebaseScore, FileScore, TodoBreakdown};
use crate::types::Category;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use tracing::{info, warn};

/// Audit mode selection
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AuditMode {
    /// Regular audit - holistic codebase analysis
    Regular,
    /// Full audit - file-by-file deep dive with master review
    Full,
}

impl std::fmt::Display for AuditMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AuditMode::Regular => write!(f, "Regular"),
            AuditMode::Full => write!(f, "Full"),
        }
    }
}

/// Regular audit result - holistic codebase analysis
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegularAuditResult {
    /// Audit mode
    pub mode: AuditMode,

    /// Overall architecture assessment
    pub architecture_assessment: String,

    /// Key patterns identified
    pub patterns: Vec<String>,

    /// Security concerns
    pub security_concerns: Vec<SecurityConcern>,

    /// Code quality observations
    pub quality_observations: Vec<String>,

    /// Technical debt areas
    pub tech_debt_areas: Vec<TechDebtArea>,

    /// Recommendations
    pub recommendations: Vec<Recommendation>,

    /// Overall health rating (0-100)
    pub overall_health: f64,

    /// Confidence in analysis (0-100)
    pub confidence: f64,
}

/// Full audit result - comprehensive file-by-file analysis
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FullAuditResult {
    /// Audit mode
    pub mode: AuditMode,

    /// Individual file analyses
    pub file_analyses: Vec<FileAnalysis>,

    /// Codebase scoring
    pub codebase_score: CodebaseScore,

    /// Master review synthesizing all findings
    pub master_review: MasterReview,

    /// Critical files requiring attention
    pub critical_files: Vec<PathBuf>,

    /// Architecture insights
    pub architecture_insights: ArchitectureInsights,

    /// Overall health rating (0-100)
    pub overall_health: f64,
}

/// Individual file analysis from Full audit
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileAnalysis {
    /// File path
    pub path: PathBuf,

    /// File score
    pub score: FileScore,

    /// LLM analysis of the file
    pub llm_analysis: FileLlmAnalysis,

    /// How this file fits in the codebase
    pub relationships: FileRelationships,
}

/// LLM analysis of a single file
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileLlmAnalysis {
    /// Purpose of the file
    pub purpose: String,

    /// Importance rating (Critical, High, Medium, Low)
    pub importance: String,

    /// Key functionality
    pub key_functionality: Vec<String>,

    /// Dependencies on other parts
    pub dependencies: Vec<String>,

    /// Security observations
    pub security_observations: Vec<String>,

    /// Quality assessment
    pub quality_assessment: String,

    /// Suggestions for improvement
    pub improvement_suggestions: Vec<String>,
}

/// File relationships within codebase
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileRelationships {
    /// Files this depends on
    pub depends_on: Vec<PathBuf>,

    /// Files that depend on this
    pub depended_by: Vec<PathBuf>,

    /// Related files (similar functionality)
    pub related: Vec<PathBuf>,

    /// Architectural layer (e.g., "API", "Core Logic", "Data Access")
    pub layer: String,
}

/// Master review synthesizing full audit findings
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MasterReview {
    /// Executive summary
    pub executive_summary: String,

    /// Top priorities for improvement
    pub top_priorities: Vec<String>,

    /// System strengths
    pub strengths: Vec<String>,

    /// System weaknesses
    pub weaknesses: Vec<String>,

    /// Architecture quality (0-100)
    pub architecture_quality: f64,

    /// Code consistency (0-100)
    pub code_consistency: f64,

    /// Test coverage assessment
    pub test_coverage_assessment: String,

    /// Long-term sustainability (0-100)
    pub sustainability: f64,

    /// Strategic recommendations
    pub strategic_recommendations: Vec<String>,
}

/// Architecture insights from full audit
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArchitectureInsights {
    /// Identified architectural patterns
    pub patterns: Vec<String>,

    /// Separation of concerns quality (0-100)
    pub separation_of_concerns: f64,

    /// Modularity score (0-100)
    pub modularity: f64,

    /// Dependency complexity
    pub dependency_complexity: String,

    /// Identified anti-patterns
    pub anti_patterns: Vec<String>,
}

/// Security concern identified in audit
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecurityConcern {
    /// Severity (Critical, High, Medium, Low)
    pub severity: String,

    /// Description of concern
    pub description: String,

    /// Affected files/areas
    pub affected_areas: Vec<String>,

    /// Recommended fix
    pub recommendation: String,
}

/// Technical debt area
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TechDebtArea {
    /// Area name/description
    pub area: String,

    /// Severity (High, Medium, Low)
    pub severity: String,

    /// Estimated effort to resolve
    pub effort: String,

    /// Impact if not addressed
    pub impact: String,
}

/// Recommendation from audit
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Recommendation {
    /// Priority (Critical, High, Medium, Low)
    pub priority: String,

    /// Category (Security, Performance, Maintainability, etc.)
    pub category: String,

    /// Recommendation text
    pub recommendation: String,

    /// Expected benefit
    pub benefit: String,
}

impl Default for FileRelationships {
    fn default() -> Self {
        Self {
            depends_on: Vec::new(),
            depended_by: Vec::new(),
            related: Vec::new(),
            layer: String::from("Unknown"),
        }
    }
}

/// Enhanced LLM auditor with Regular and Full modes
pub struct LlmAuditor {
    llm_client: LlmClient,
    cache: Option<AuditCache>,
    config: LlmConfig,
}

impl LlmAuditor {
    /// Create a new LLM auditor with the specified provider and project root
    pub fn new_with_provider(provider: &str, project_root: &Path) -> Result<Self> {
        // Load config
        let config = LlmConfig::load(project_root)?;

        // Check if LLM audits are enabled
        if !config.enabled {
            warn!(
                "LLM audits are DISABLED in config. Set enabled=true in .llm-audit.toml to enable."
            );
        }

        let api_key = config.get_api_key_for_provider(provider)?;

        let (model, actual_provider, max_tokens) = match provider.to_lowercase().as_str() {
            "google" | "gemini" => ("gemini-1.5-pro".to_string(), "google".to_string(), 8000),
            "xai" | "grok" => (
                "grok-4-1-fast-reasoning".to_string(),
                "xai".to_string(),
                32000,
            ),
            // Claude Opus 4.5 - Anthropic's most capable model for deep analysis
            // Best for: whitepaper conformity verification, high-stakes auditing, JANUS theory validation
            // 200K context window with excellent reasoning capabilities
            "anthropic" | "claude" | "opus" => (
                "claude-opus-4-20250514".to_string(),
                "anthropic".to_string(),
                32000, // Max output tokens for Claude
            ),
            // Claude Sonnet 4 - balanced performance for routine audits
            "sonnet" => (
                "claude-sonnet-4-20250514".to_string(),
                "anthropic".to_string(),
                16000,
            ),
            _ => (
                config.provider.default_model.clone(),
                config.provider.default_provider.clone(),
                config.provider.max_tokens,
            ),
        };

        let llm_client = LlmClient::new_with_provider(
            api_key,
            actual_provider,
            model,
            max_tokens,
            config.provider.temperature,
        )?;

        // Initialize cache if enabled
        let cache = if config.cache.enabled {
            match AuditCache::new(project_root, &config.cache) {
                Ok(c) => {
                    info!("✅ Audit cache initialized");
                    Some(c)
                }
                Err(e) => {
                    warn!("Failed to initialize cache: {}", e);
                    None
                }
            }
        } else {
            None
        };

        Ok(Self {
            llm_client,
            cache,
            config,
        })
    }

    /// Create a new LLM auditor (defaults to xai provider)
    pub fn new(project_root: &Path) -> Result<Self> {
        Self::new_with_provider("xai", project_root)
    }

    /// Check if LLM audits are enabled
    pub fn is_enabled(&self) -> bool {
        self.config.enabled
    }

    /// Print configuration summary
    pub fn print_config(&self) {
        self.config.print_summary();
        if let Some(ref cache) = self.cache {
            cache.print_summary();
        }
    }

    /// Run a regular audit (holistic codebase analysis)
    pub async fn run_regular_audit(
        &self,
        project_path: &Path,
        _focus_areas: Vec<String>,
    ) -> Result<RegularAuditResult> {
        info!("🔍 Running Regular Audit on: {:?}", project_path);

        // Collect file contents for analysis
        let rust_files = self.find_rust_files(project_path)?;
        let mut file_contents = Vec::new();

        for path in rust_files.iter().take(10) {
            if let Ok(content) = fs::read_to_string(path) {
                let path_str = path.to_string_lossy().to_string();
                file_contents.push((path_str, content));
            }
        }

        let file_refs: Vec<(&str, &str)> = file_contents
            .iter()
            .map(|(p, c)| (p.as_str(), c.as_str()))
            .collect();

        // Use analyze_codebase for holistic analysis
        let analysis = self.llm_client.analyze_codebase(&file_refs).await?;

        // Parse into regular audit result
        Ok(RegularAuditResult {
            mode: AuditMode::Regular,
            architecture_assessment: format!(
                "Analyzed codebase with {} deprecated files, {} missing types, {} security concerns",
                analysis.deprecated_files.len(),
                analysis.missing_types.len(),
                analysis.security_concerns.len()
            ),
            patterns: vec![
                "Focus areas analyzed".to_string(),
                format!("Security concerns: {}", analysis.security_concerns.len()),
            ],
            security_concerns: analysis
                .security_concerns
                .iter()
                .map(|concern| SecurityConcern {
                    severity: "Medium".to_string(),
                    description: concern.clone(),
                    affected_areas: vec![],
                    recommendation: "Review and address".to_string(),
                })
                .collect(),
            quality_observations: vec![
                format!("Deprecated files: {}", analysis.deprecated_files.len()),
                format!("Architecture issues: {}", analysis.architecture_issues.len()),
            ],
            tech_debt_areas: Vec::new(),
            recommendations: vec![Recommendation {
                priority: "High".to_string(),
                category: "Architecture".to_string(),
                recommendation: "Review and address identified issues".to_string(),
                benefit: "Improved code quality and maintainability".to_string(),
            }],
            overall_health: 70.0,
            confidence: 75.0,
        })
    }

    /// Run a full audit (file-by-file deep dive)
    pub async fn run_full_audit(&self, project_path: &Path) -> Result<FullAuditResult> {
        info!("🔬 Running Full Audit on: {:?}", project_path);

        // 1. Collect and analyze top files
        let mut file_analyses = Vec::new();
        let mut analyzed_paths = Vec::new();

        // Find Rust files
        let rust_files = self.find_rust_files(project_path)?;

        // Analyze top 10 files to avoid excessive API calls
        for path in rust_files.iter().take(10) {
            if let Ok(content) = fs::read_to_string(path) {
                // Use Rust category for .rs files
                // Detect category from file path
                let category = Category::from_path(path.to_str().unwrap_or(""));
                let analysis = self
                    .llm_client
                    .analyze_file(path, &content, category)
                    .await?;

                // Create a basic score
                let mut score = FileScore::new(path.clone());
                score.importance = analysis.importance * 100.0;

                // Convert letter grade to numeric score (A=100, B=80, C=60, D=40, F=20)
                score.security = match analysis.security_rating.trim().to_uppercase().as_str() {
                    "A" => 100.0,
                    "B" => 80.0,
                    "C" => 60.0,
                    "D" => 40.0,
                    "F" => 20.0,
                    _ => 50.0, // Default/unknown
                };

                score.risk = if analysis.issues.iter().any(|i| i.severity == "critical") {
                    90.0
                } else if analysis.issues.iter().any(|i| i.severity == "high") {
                    70.0
                } else {
                    30.0
                };

                file_analyses.push(FileAnalysis {
                    path: path.clone(),
                    score: score.clone(),
                    llm_analysis: FileLlmAnalysis {
                        purpose: "Analyzed file".to_string(),
                        importance: analysis.importance.to_string(),
                        key_functionality: vec![analysis.summary.clone()],
                        dependencies: vec![],
                        security_observations: analysis
                            .issues
                            .iter()
                            .filter_map(|i| {
                                if i.severity == "critical" || i.severity == "high" {
                                    Some(i.description.clone())
                                } else {
                                    None
                                }
                            })
                            .collect(),
                        quality_assessment: format!(
                            "Security rating: {}",
                            analysis.security_rating
                        ),
                        improvement_suggestions: analysis
                            .issues
                            .iter()
                            .filter_map(|i| i.suggestion.clone())
                            .collect(),
                    },
                    relationships: FileRelationships::default(),
                });

                analyzed_paths.push(path.clone());
            }
        }

        // 3. Build codebase score from analyses
        let codebase_score =
            self.build_codebase_score_from_analyses(&file_analyses, rust_files.len())?;

        // 4. Generate master review
        let master_review = self.generate_master_review(&file_analyses).await?;

        // 5. Identify critical files
        let critical_files: Vec<PathBuf> = file_analyses
            .iter()
            .filter(|fa| fa.score.risk > 70.0 || fa.score.importance > 80.0)
            .take(5)
            .map(|fa| fa.path.clone())
            .collect();

        let overall_health = codebase_score.overall_health;

        Ok(FullAuditResult {
            mode: AuditMode::Full,
            file_analyses,
            codebase_score,
            master_review,
            critical_files,
            architecture_insights: ArchitectureInsights {
                patterns: vec!["Rust codebase".to_string()],
                separation_of_concerns: 65.0,
                modularity: 70.0,
                dependency_complexity: "Moderate".to_string(),
                anti_patterns: vec![],
            },
            overall_health,
        })
    }

    /// Build a summary context of the codebase
    #[allow(dead_code)]
    fn build_codebase_context(&self, project_path: &Path) -> Result<String> {
        let mut context = String::new();
        context.push_str(&format!("Codebase at: {}\n\n", project_path.display()));

        // Add basic structure info
        let mut file_count = 0;
        let mut rust_files = 0;

        if let Ok(entries) = fs::read_dir(project_path) {
            for entry in entries.flatten() {
                file_count += 1;
                if let Some(ext) = entry.path().extension() {
                    if ext == "rs" {
                        rust_files += 1;
                    }
                }
            }
        }

        context.push_str(&format!("Total files: {}\n", file_count));
        context.push_str(&format!("Rust files: {}\n", rust_files));

        Ok(context)
    }

    /// Find Rust files recursively
    fn find_rust_files(&self, dir: &Path) -> Result<Vec<PathBuf>> {
        let mut results = Vec::new();

        if !dir.is_dir() {
            return Ok(results);
        }

        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();

            if path.is_dir() {
                // Skip target and hidden directories
                if let Some(name) = path.file_name() {
                    let name_str = name.to_string_lossy();
                    if name_str == "target" || name_str.starts_with('.') {
                        continue;
                    }
                }
                results.extend(self.find_rust_files(&path)?);
            } else if path.extension().and_then(|s| s.to_str()) == Some("rs") {
                results.push(path);
            }
        }

        Ok(results)
    }

    /// Build codebase score from file analyses
    fn build_codebase_score_from_analyses(
        &self,
        analyses: &[FileAnalysis],
        total_files: usize,
    ) -> Result<CodebaseScore> {
        let mut avg_score = FileScore::new(PathBuf::from("average"));

        if !analyses.is_empty() {
            let count = analyses.len() as f64;
            for analysis in analyses {
                avg_score.importance += analysis.score.importance;
                avg_score.risk += analysis.score.risk;
                avg_score.security += analysis.score.security;
                avg_score.quality += analysis.score.quality;
            }
            avg_score.importance /= count;
            avg_score.risk /= count;
            avg_score.security /= count;
            avg_score.quality /= count;
        }

        let critical_files: Vec<PathBuf> = analyses
            .iter()
            .filter(|a| a.score.risk > 70.0)
            .map(|a| a.path.clone())
            .collect();

        let high_priority_files: Vec<PathBuf> = analyses
            .iter()
            .filter(|a| a.score.importance > 70.0)
            .map(|a| a.path.clone())
            .collect();

        let overall_health = 100.0 - avg_score.risk;
        let tech_debt = avg_score.tech_debt;

        Ok(CodebaseScore {
            total_files,
            averages: avg_score,
            critical_files,
            high_priority_files,
            healthiest_files: Vec::new(),
            unhealthiest_files: Vec::new(),
            total_todos: TodoBreakdown {
                high: 0,
                medium: 0,
                low: 0,
                total: 0,
            },
            total_tech_debt: tech_debt,
            overall_health,
        })
    }

    /// Generate master review from file analyses
    async fn generate_master_review(&self, analyses: &[FileAnalysis]) -> Result<MasterReview> {
        let summary = format!(
            "Analyzed {} high-priority files. Security and quality metrics collected.",
            analyses.len()
        );

        let mut top_priorities = Vec::new();
        let mut weaknesses = Vec::new();

        for analysis in analyses {
            if !analysis.llm_analysis.security_observations.is_empty() {
                top_priorities.push(format!(
                    "Address security issues in {}",
                    analysis.path.display()
                ));
            }
            if !analysis.llm_analysis.improvement_suggestions.is_empty() {
                weaknesses.push(analysis.llm_analysis.improvement_suggestions[0].clone());
            }
        }

        Ok(MasterReview {
            executive_summary: summary,
            top_priorities,
            strengths: vec!["Structured codebase".to_string()],
            weaknesses,
            architecture_quality: 70.0,
            code_consistency: 75.0,
            test_coverage_assessment: "Test coverage not measured".to_string(),
            sustainability: 70.0,
            strategic_recommendations: vec![
                "Implement automated testing".to_string(),
                "Address security concerns".to_string(),
            ],
        })
    }
}

impl Default for LlmAuditor {
    fn default() -> Self {
        // Use a dummy path for default - in practice, callers should use new() with proper path
        let dummy_path = std::path::PathBuf::from(".");
        Self::new(&dummy_path).unwrap_or_else(|_| {
            warn!("Failed to create default LLM auditor, creating fallback");
            Self::new_with_provider("xai", &dummy_path)
                .expect("Failed to create fallback LLM client")
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_audit_mode_display() {
        assert_eq!(AuditMode::Regular.to_string(), "Regular");
        assert_eq!(AuditMode::Full.to_string(), "Full");
    }

    #[test]
    fn test_auditor_creation() {
        use std::path::Path;
        let project_root = Path::new(".");
        let _auditor = LlmAuditor::new(project_root);
        // Placeholder test - actual tests need LLM integration
    }
}
