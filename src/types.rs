//! Core types for the audit service

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use uuid::Uuid;

// Re-export types from other modules
pub use crate::context::GlobalContextBundle;
pub use crate::tests_runner::TestResults;

/// File category based on location and purpose
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Category {
    /// Janus - Core trading system, neuromorphic components, and decision-making
    Janus,
    /// Execution - Lightweight external communication service (receives signals from Janus)
    Execution,
    /// Clients - KMP applications (Android, iOS, Web, Desktop for Linux/Windows/macOS)
    Clients,
    /// Audit - Audit service and tooling
    Audit,
    /// Infrastructure (Docker, CI/CD)
    Infra,
    /// Configuration files
    Config,
    /// Documentation
    Documentation,
    /// Tests
    Tests,
    /// Other/Unknown
    Other,
}

impl Category {
    /// Get the category from a file path
    pub fn from_path(path: &str) -> Self {
        // Audit service itself
        if path.contains("src/audit/") {
            return Category::Audit;
        }

        // Execution - lightweight external communication service
        if path.contains("/execution/")
            || path.contains("/services/execution/")
            || path.contains("execution-service")
        {
            return Category::Execution;
        }

        // Clients - KMP applications for Android, iOS, Web, Desktop
        if path.contains("/clients/")
            || path.contains("/android/")
            || path.contains("/ios/")
            || path.contains("/web/")
            || path.contains("/desktop/")
            || path.contains("/kmp/")
            || path.contains("/shared/")
            || path.ends_with(".kt")
            || path.ends_with(".kts")
            || path.ends_with(".swift")
            || path.contains("commonMain")
            || path.contains("androidMain")
            || path.contains("iosMain")
            || path.contains("desktopMain")
            || path.contains("webMain")
        {
            return Category::Clients;
        }

        // Janus - core trading system, neuromorphic components, decision-making, risk management
        if path.contains("/janus/")
            || path.contains("/neuromorphic/")
            || path.contains("/cerebellum/")
            || path.contains("/hippocampus/")
            || path.contains("/thalamus/")
            || path.contains("/cortex/")
            || path.contains("/vision/")
            || path.contains("/dsp/")
            || path.contains("/training/")
            || path.contains("/risk/")
            || path.contains("/amygdala/")
            || path.contains("/conscience/")
            || path.contains("/kill-switch/")
            || path.contains("/circuit-breaker/")
            || path.contains("/orders/")
            || path.contains("/trading/")
            || path.contains("/connectors/")
            || path.contains("/adapters/")
            || path.contains("/binance/")
            || path.contains("/coinbase/")
            || path.contains("/kraken/")
        {
            return Category::Janus;
        }

        // Infrastructure files and directories
        if path.contains("docker/")
            || path.contains("Dockerfile")
            || path.contains(".github/")
            || path.contains(".githooks/")
            || path.contains("scripts/")
            || path.ends_with(".dockerignore")
            || path.ends_with(".gitattributes")
            || path.ends_with(".gitignore")
            || path.ends_with("compose")
            || path.ends_with("compose.yml")
            || path.ends_with("compose.yaml")
            || path.contains("compose.prod")
        {
            Category::Infra
        } else if path.ends_with(".toml")
            || path.ends_with(".yaml")
            || path.ends_with(".yml")
            || path.ends_with(".env")
            || path.contains("config/")
        {
            Category::Config
        } else if path.ends_with(".md") || path.contains("docs/") {
            Category::Documentation
        } else if path.contains("test") || path.contains("tests/") || path.contains("benches/") {
            Category::Tests
        } else {
            Category::Other
        }
    }

    /// Get the main category group (for high-level organization)
    pub fn main_group(&self) -> MainCategory {
        match self {
            Category::Janus => MainCategory::Janus,
            Category::Execution => MainCategory::Execution,
            Category::Clients => MainCategory::Clients,
            Category::Audit => MainCategory::Audit,
            _ => MainCategory::Other,
        }
    }
}

/// Main category groupings for high-level organization
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MainCategory {
    /// Janus - Core trading system, neuromorphic components, decision-making, and risk management
    Janus,
    /// Execution - Lightweight external communication service (receives signals from Janus)
    Execution,
    /// Clients - KMP applications (Android, iOS, Web, Desktop for Linux/Windows/macOS)
    Clients,
    /// Audit - Audit service and tooling
    Audit,
    /// Other - Infrastructure, config, docs, tests
    Other,
}

impl MainCategory {
    /// Get a human-readable description
    pub fn description(&self) -> &'static str {
        match self {
            MainCategory::Janus => {
                "Core trading system, neuromorphic components, decision-making, and risk management"
            }
            MainCategory::Execution => {
                "Lightweight external communication service (receives signals from Janus)"
            }
            MainCategory::Clients => {
                "KMP applications (Android, iOS, Web, Desktop for Linux/Windows/macOS)"
            }
            MainCategory::Audit => "Audit service and tooling",
            MainCategory::Other => "Infrastructure, configuration, documentation, tests",
        }
    }
}

/// File priority for audit
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FilePriority {
    Critical = 4,
    High = 3,
    Medium = 2,
    Low = 1,
    Exclude = 0,
}

impl FilePriority {
    /// Determine priority from file path
    pub fn from_path(path: &str) -> Self {
        // Critical files
        if path.contains("kill_switch")
            || path.contains("circuit_breaker")
            || path.contains("conscience")
            || path.ends_with("main.rs")
            || path.ends_with("main.py")
        {
            return FilePriority::Critical;
        }

        // High priority
        if path.contains("amygdala/")
            || path.contains("risk")
            || path.contains("execution")
            || path.contains("cerebellum")
        {
            return FilePriority::High;
        }

        // Exclude
        if path.contains("target/")
            || path.contains("node_modules/")
            || path.contains(".git/")
            || path.contains("__pycache__")
            || path.contains(".pytest_cache")
        {
            return FilePriority::Exclude;
        }

        // Medium priority by default
        FilePriority::Medium
    }
}

/// Security rating for code
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum SecurityRating {
    A,
    B,
    C,
    D,
    F,
}

impl SecurityRating {
    /// Convert from importance score
    pub fn from_importance(score: f64) -> Self {
        if score >= 0.9 {
            SecurityRating::A
        } else if score >= 0.7 {
            SecurityRating::B
        } else if score >= 0.5 {
            SecurityRating::C
        } else if score >= 0.3 {
            SecurityRating::D
        } else {
            SecurityRating::F
        }
    }
}

/// Audit tag found in code
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditTag {
    /// Tag type
    pub tag_type: AuditTagType,
    /// File path
    pub file: PathBuf,
    /// Line number
    pub line: usize,
    /// Tag value/description
    pub value: String,
    /// Additional context
    pub context: Option<String>,
}

/// Type of audit tag
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AuditTagType {
    /// @audit-tag: [new | old | experimental | deprecated]
    Tag,
    /// @audit-todo: [task description]
    Todo,
    /// @audit-freeze (never modify)
    Freeze,
    /// @audit-review: [review notes]
    Review,
    /// @audit-security: [security concern]
    Security,
}

impl AuditTagType {
    /// Get the tag prefix
    pub fn prefix(&self) -> &'static str {
        match self {
            AuditTagType::Tag => "@audit-tag:",
            AuditTagType::Todo => "@audit-todo:",
            AuditTagType::Freeze => "@audit-freeze",
            AuditTagType::Review => "@audit-review:",
            AuditTagType::Security => "@audit-security:",
        }
    }
}

/// Generated task from audit
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    /// Unique task ID
    pub id: String,
    /// Task title
    pub title: String,
    /// Task description
    pub description: String,
    /// Source file
    pub file: PathBuf,
    /// Line number
    pub line: Option<usize>,
    /// Task priority
    pub priority: TaskPriority,
    /// Task category
    pub category: Category,
    /// Created timestamp
    pub created_at: DateTime<Utc>,
    /// Tags
    pub tags: Vec<String>,
}

impl Task {
    /// Create a new task
    pub fn new(
        title: impl Into<String>,
        description: impl Into<String>,
        file: PathBuf,
        line: Option<usize>,
        priority: TaskPriority,
        category: Category,
    ) -> Self {
        Self {
            id: format!("TASK-{}", Uuid::new_v4().to_string()[..8].to_uppercase()),
            title: title.into(),
            description: description.into(),
            file,
            line,
            priority,
            category,
            created_at: Utc::now(),
            tags: Vec::new(),
        }
    }

    /// Add a tag
    pub fn with_tag(mut self, tag: impl Into<String>) -> Self {
        self.tags.push(tag.into());
        self
    }
}

/// Task priority
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TaskPriority {
    Critical,
    High,
    Medium,
    Low,
}

/// File analysis result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileAnalysis {
    /// File path
    pub path: PathBuf,
    /// Category
    pub category: Category,
    /// Priority
    pub priority: FilePriority,
    /// Lines of code
    pub lines: usize,
    /// Documentation blocks
    pub doc_blocks: usize,
    /// Security rating
    pub security_rating: Option<SecurityRating>,
    /// Issues found
    pub issues: Vec<Issue>,
    /// LLM analysis (if available)
    pub llm_analysis: Option<String>,
    /// Tags found
    pub tags: Vec<AuditTag>,
}

/// Code issue
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Issue {
    /// Issue severity
    pub severity: IssueSeverity,
    /// Issue category
    pub category: IssueCategory,
    /// File path
    pub file: PathBuf,
    /// Line number
    pub line: usize,
    /// Issue message
    pub message: String,
    /// Suggested fix
    pub suggestion: Option<String>,
}

/// Issue severity
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum IssueSeverity {
    Critical,
    High,
    Medium,
    Low,
    Info,
}

/// Issue category
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum IssueCategory {
    Security,
    Performance,
    TypeSafety,
    AsyncSafety,
    RiskManagement,
    CodeQuality,
    Documentation,
    Testing,
}

/// System architecture map
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemMap {
    /// Total files analyzed
    pub total_files: usize,
    /// Files by category
    pub files_by_category: HashMap<Category, usize>,
    /// Lines by category
    pub lines_by_category: HashMap<Category, usize>,
    /// Service dependencies
    pub dependencies: Vec<ServiceDependency>,
    /// Mermaid diagram
    pub mermaid_diagram: Option<String>,
}

/// Service dependency
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceDependency {
    /// Source service
    pub from: String,
    /// Target service
    pub to: String,
    /// Dependency type
    pub dep_type: DependencyType,
}

/// Dependency type
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DependencyType {
    Grpc,
    Http,
    Internal,
}

/// Audit request
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditRequest {
    /// Repository URL or local path
    pub repository: String,
    /// Branch to audit (default: main)
    pub branch: Option<String>,
    /// Enable LLM analysis
    pub enable_llm: bool,
    /// Focus areas
    pub focus: Vec<String>,
    /// Include tests
    pub include_tests: bool,
}

/// Audit report
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditReport {
    /// Report ID
    pub id: String,
    /// Repository audited
    pub repository: String,
    /// Branch audited
    pub branch: String,
    /// Timestamp
    pub created_at: DateTime<Utc>,
    /// System map
    pub system_map: SystemMap,
    /// File analyses
    pub files: Vec<FileAnalysis>,
    /// Generated tasks
    pub tasks: Vec<Task>,
    /// Total issues by severity
    pub issues_by_severity: HashMap<IssueSeverity, usize>,
    /// Summary
    pub summary: AuditSummary,
    /// Test results (if tests were run)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub test_results: Option<Vec<TestResults>>,
    /// Global context bundle (if deep analysis was performed)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_bundle: Option<GlobalContextBundle>,
}

/// Audit summary
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AuditSummary {
    /// Total files analyzed
    pub total_files: usize,
    /// Total lines of code
    pub total_lines: usize,
    /// Total issues found
    pub total_issues: usize,
    /// Total tasks generated
    pub total_tasks: usize,
    /// Files with critical issues
    pub critical_files: usize,
    /// Average security rating
    pub avg_security_rating: Option<f64>,
    /// Total tests run
    pub total_tests: Option<usize>,
    /// Test pass rate
    pub test_pass_rate: Option<f64>,
    /// Code coverage percentage
    pub code_coverage: Option<f64>,
}
