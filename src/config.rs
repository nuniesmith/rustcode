// Configuration for the audit service

use crate::error::{AuditError, Result};
use crate::task_executor::TaskExecutorOptions;
use crate::task_watcher::TaskWatcherConfig;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

// Audit service configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    // Server configuration
    pub server: ServerConfig,
    // LLM configuration
    pub llm: LlmConfig,
    // Git configuration
    pub git: GitConfig,
    // Scanner configuration
    pub scanner: ScannerConfig,
    // Storage configuration
    pub storage: StorageConfig,
    // Research pipeline configuration
    pub research: Option<ResearchConfig>,
    // Security configuration
    pub security: SecurityConfig,
    // Database configuration
    pub database: DatabaseConfig,
    // Model router configuration (XAI/Ollama inference routing)
    pub model: ModelConfig,
    // Auto-scanner configuration
    pub auto_scan: AutoScanConfig,
    // Task executor configuration
    pub task_executor: TaskExecutorOptions,
    // Task watcher configuration
    pub task_watcher: TaskWatcherConfig,
}

impl Config {
    // Load configuration from environment and config files
    pub fn load() -> Result<Self> {
        // Load from environment variables
        dotenvy::dotenv().ok();

        let server = ServerConfig {
            host: std::env::var("HOST")
                .or_else(|_| std::env::var("AUDIT_HOST"))
                .unwrap_or_else(|_| "0.0.0.0".to_string()),
            port: std::env::var("PORT")
                .or_else(|_| std::env::var("AUDIT_PORT"))
                .ok()
                .and_then(|p| p.parse().ok())
                .unwrap_or(3500),
        };

        let provider = std::env::var("LLM_PROVIDER").unwrap_or_else(|_| "xai".to_string());

        // Select API key based on provider
        let api_key = match provider.as_str() {
            "google" | "gemini" => std::env::var("GOOGLE_API_KEY").ok(),
            "xai" | "grok" => std::env::var("XAI_API_KEY").ok(),
            _ => std::env::var("XAI_API_KEY").ok(), // Default to XAI
        };

        // Select default model based on provider
        let default_model = match provider.as_str() {
            "google" | "gemini" => "gemini-2.0-flash-exp".to_string(),
            "xai" | "grok" => "grok-4-1-fast-reasoning".to_string(),
            _ => "grok-4-1-fast-reasoning".to_string(),
        };

        let llm = LlmConfig {
            provider: provider.clone(),
            api_key,
            model: std::env::var("LLM_MODEL").unwrap_or(default_model),
            max_tokens: std::env::var("LLM_MAX_TOKENS")
                .ok()
                .and_then(|t| t.parse().ok())
                .unwrap_or(4096),
            temperature: std::env::var("LLM_TEMPERATURE")
                .ok()
                .and_then(|t| t.parse().ok())
                .unwrap_or(0.7),
            enabled: std::env::var("LLM_ENABLED")
                .ok()
                .and_then(|e| e.parse().ok())
                .unwrap_or(true),
        };

        let git = GitConfig {
            workspace_dir: std::env::var("GIT_WORKSPACE_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|_| PathBuf::from("./workspace")),
            default_branch: std::env::var("GIT_DEFAULT_BRANCH")
                .unwrap_or_else(|_| "main".to_string()),
            shallow_clone: std::env::var("GIT_SHALLOW_CLONE")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(true),
            repos_dir: std::env::var("REPOS_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|_| PathBuf::from("/app/repos")),
            sync_interval_secs: std::env::var("REPO_SYNC_INTERVAL_SECS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(300),
        };

        let skip_extensions = std::env::var("SCANNER_SKIP_EXTENSIONS")
            .ok()
            .map(|s| {
                s.split(',')
                    .map(|ext| ext.trim().to_string())
                    .filter(|ext| !ext.is_empty())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_else(crate::config::default_skip_extensions);

        let scanner = ScannerConfig {
            max_file_size: std::env::var("SCANNER_MAX_FILE_SIZE")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(1_000_000), // 1MB default
            include_tests: std::env::var("SCANNER_INCLUDE_TESTS")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(true), // Include tests by default for comprehensive analysis
            exclude_patterns: vec![
                "target/".to_string(),
                "node_modules/".to_string(),
                ".git/".to_string(),
                "__pycache__/".to_string(),
                "*.lock".to_string(),
            ],
            skip_extensions,
        };

        let storage = StorageConfig {
            reports_dir: std::env::var("STORAGE_REPORTS_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|_| PathBuf::from("./reports")),
            tasks_dir: std::env::var("STORAGE_TASKS_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|_| PathBuf::from("./tasks")),
        };

        // Security configuration with allowed Git hosts
        let security = SecurityConfig {
            allowed_git_hosts: std::env::var("ALLOWED_GIT_HOSTS")
                .map(|s| s.split(',').map(|h| h.trim().to_string()).collect())
                .unwrap_or_else(|_| SecurityConfig::default().allowed_git_hosts),
            allow_local_paths: std::env::var("ALLOW_LOCAL_PATHS")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(true), // Allow local paths by default for development
            require_https: std::env::var("REQUIRE_HTTPS_GIT")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(true), // Require HTTPS by default
            max_clone_size_mb: std::env::var("MAX_CLONE_SIZE_MB")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(500), // 500MB default
            webhook_secret: std::env::var("GITHUB_WEBHOOK_SECRET").unwrap_or_default(),
        };

        let research = Some(ResearchConfig {
            enabled: std::env::var("RESEARCH_ENABLED")
                .ok()
                .and_then(|e| e.parse().ok())
                .unwrap_or(true),
            output_dir: std::env::var("RESEARCH_OUTPUT_DIR")
                .unwrap_or_else(|_| "docs/research_breakdowns".to_string()),
            file_extensions: vec!["md".to_string(), "txt".to_string()],
            prompts: HashMap::new(), // Prompts are loaded from default or can be overridden
        });

        let database = DatabaseConfig {
            url: std::env::var("DATABASE_URL").unwrap_or_else(|_| {
                "postgresql://rustcode:changeme@localhost:5432/rustcode.db".to_string()
            }),
        };

        let model = ModelConfig {
            xai_api_key: std::env::var("XAI_API_KEY").ok(),
            remote_model: std::env::var("REMOTE_MODEL")
                .unwrap_or_else(|_| "grok-4-1-fast-reasoning".to_string()),
            local_model: std::env::var("LOCAL_MODEL")
                .unwrap_or_else(|_| "qwen2.5-coder:7b".to_string()),
            ollama_base_url: std::env::var("OLLAMA_BASE_URL")
                .unwrap_or_else(|_| "http://localhost:11434".to_string()),
            force_remote: std::env::var("FORCE_REMOTE_MODEL")
                .map(|v| v.eq_ignore_ascii_case("true"))
                .unwrap_or(false),
        };

        let auto_scan = AutoScanConfig {
            enabled: std::env::var("AUTO_SCAN_ENABLED")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(true),
            interval_minutes: std::env::var("AUTO_SCAN_INTERVAL")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(60),
            max_concurrent: std::env::var("AUTO_SCAN_MAX_CONCURRENT")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(2),
            cost_budget: std::env::var("AUTO_SCAN_COST_BUDGET")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(3.00),
        };

        Ok(Self {
            server,
            llm,
            git,
            scanner,
            storage,
            research,
            security,
            database,
            model,
            auto_scan,
        })
    }

    // Get a research prompt by key, falling back to defaults
    pub fn get_research_prompt(&self, key: &str) -> Option<String> {
        self.research.as_ref()?.prompts.get(key).cloned()
    }

    // Validate the configuration
    pub fn validate(&self) -> Result<()> {
        if self.llm.enabled && self.llm.api_key.is_none() {
            let env_var = match self.llm.provider.as_str() {
                "google" | "gemini" => "GOOGLE_API_KEY",
                "xai" | "grok" => "XAI_API_KEY",
                _ => "XAI_API_KEY or GOOGLE_API_KEY",
            };
            return Err(AuditError::config(format!(
                "LLM is enabled but no API key provided. Set {} environment variable.",
                env_var
            )));
        }

        if self.server.port == 0 {
            return Err(AuditError::config("Server port cannot be 0"));
        }

        Ok(())
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            server: ServerConfig::default(),
            llm: LlmConfig::default(),
            git: GitConfig::default(),
            scanner: ScannerConfig::default(),
            storage: StorageConfig::default(),
            research: Some(ResearchConfig::default()),
            security: SecurityConfig::default(),
            database: DatabaseConfig::default(),
            model: ModelConfig::default(),
            auto_scan: AutoScanConfig::default(),
        }
    }
}

// Security configuration for SSRF prevention and access control
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecurityConfig {
    // Whitelist of allowed Git hosts for cloning
    // Examples: ["github.com", "gitlab.com", "bitbucket.org"]
    pub allowed_git_hosts: Vec<String>,
    // Whether to allow local filesystem paths (disable in production)
    pub allow_local_paths: bool,
    // Require HTTPS for Git URLs (prevents MITM attacks)
    pub require_https: bool,
    // Maximum repository size to clone (in MB)
    pub max_clone_size_mb: usize,
    // GitHub webhook secret for validating incoming webhook events — set via GITHUB_WEBHOOK_SECRET
    pub webhook_secret: String,
}

impl Default for SecurityConfig {
    fn default() -> Self {
        Self {
            // Default whitelist of trusted Git hosts
            allowed_git_hosts: vec![
                "github.com".to_string(),
                "gitlab.com".to_string(),
                "bitbucket.org".to_string(),
                "dev.azure.com".to_string(),
                "ssh.dev.azure.com".to_string(),
            ],
            allow_local_paths: true, // Enable for development, disable in production
            require_https: true,
            max_clone_size_mb: 500,
            webhook_secret: String::new(),
        }
    }
}

impl SecurityConfig {
    // Validate a Git URL against the security policy
    pub fn validate_git_url(&self, url: &str) -> Result<()> {
        // Parse the URL to extract the host
        if url.starts_with("git@") {
            // SSH URL format: git@github.com:user/repo.git
            let host = url
                .strip_prefix("git@")
                .and_then(|s| s.split(':').next())
                .ok_or_else(|| AuditError::config("Invalid SSH Git URL format"))?;

            if !self.allowed_git_hosts.iter().any(|h| h == host) {
                return Err(AuditError::config(format!(
                    "Git host '{}' is not in the allowed hosts list. Allowed: {:?}",
                    host, self.allowed_git_hosts
                )));
            }
        } else if url.starts_with("http://") || url.starts_with("https://") {
            // HTTP(S) URL format
            if self.require_https && url.starts_with("http://") {
                return Err(AuditError::config(
                    "HTTP Git URLs are not allowed. Use HTTPS instead.",
                ));
            }

            // Extract host from URL
            let url_parsed = url::Url::parse(url)
                .map_err(|e| AuditError::config(format!("Invalid Git URL: {}", e)))?;

            let host = url_parsed
                .host_str()
                .ok_or_else(|| AuditError::config("Git URL has no host"))?;

            // Check against whitelist
            if !self.allowed_git_hosts.iter().any(|h| h == host) {
                return Err(AuditError::config(format!(
                    "Git host '{}' is not in the allowed hosts list. Allowed: {:?}",
                    host, self.allowed_git_hosts
                )));
            }

            // Block internal/private IPs to prevent SSRF
            if let Some(url::Host::Ipv4(ipv4)) = url_parsed.host() {
                if ipv4.is_private()
                    || ipv4.is_loopback()
                    || ipv4.is_link_local()
                    || ipv4.is_unspecified()
                {
                    return Err(AuditError::config(
                        "Git URLs pointing to private/internal IPs are not allowed",
                    ));
                }
            }
        } else {
            return Err(AuditError::config(format!(
                "Unsupported Git URL scheme. URL must start with 'https://', 'http://', or 'git@'. Got: {}",
                url.chars().take(20).collect::<String>()
            )));
        }

        Ok(())
    }

    // Check if a local path is allowed
    pub fn validate_local_path(&self, path: &str) -> Result<()> {
        if !self.allow_local_paths {
            return Err(AuditError::config(
                "Local filesystem paths are not allowed. Set ALLOW_LOCAL_PATHS=true to enable.",
            ));
        }

        // Prevent path traversal attacks
        let path = std::path::Path::new(path);
        if path
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
        {
            return Err(AuditError::config(
                "Path traversal (../) is not allowed in local paths",
            ));
        }

        Ok(())
    }
}

// Research pipeline configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResearchConfig {
    // Whether research pipeline is enabled
    pub enabled: bool,
    // Directory where research breakdowns are saved
    pub output_dir: String,
    // File extensions to scan for research materials
    pub file_extensions: Vec<String>,
    // Custom prompts for research analysis
    pub prompts: HashMap<String, String>,
}

impl Default for ResearchConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            output_dir: "docs/research_breakdowns".to_string(),
            file_extensions: vec!["md".to_string(), "txt".to_string()],
            prompts: HashMap::new(),
        }
    }
}

// Server configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerConfig {
    // Host to bind to
    pub host: String,
    // Port to bind to
    pub port: u16,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            host: "0.0.0.0".to_string(),
            port: 3500,
        }
    }
}

// LLM configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmConfig {
    // LLM provider (grok, openai, etc.)
    pub provider: String,
    // API key
    pub api_key: Option<String>,
    // Model name
    pub model: String,
    // Max tokens for completion
    pub max_tokens: usize,
    // Temperature for sampling
    pub temperature: f64,
    // Whether LLM analysis is enabled
    pub enabled: bool,
}

impl Default for LlmConfig {
    fn default() -> Self {
        Self {
            provider: "xai".to_string(),
            api_key: None,
            model: "grok-4-1-fast-reasoning".to_string(),
            max_tokens: 4096,
            temperature: 0.7,
            enabled: false,
        }
    }
}

// Git configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitConfig {
    // Directory where repositories are cloned
    pub workspace_dir: PathBuf,
    // Default branch to checkout
    pub default_branch: String,
    // Whether to do shallow clones (depth=1)
    pub shallow_clone: bool,
    // Directory where repositories are stored for auto-scanning — set via REPOS_DIR
    pub repos_dir: PathBuf,
    // Interval in seconds between repository syncs — set via REPO_SYNC_INTERVAL_SECS
    pub sync_interval_secs: u64,
}

impl Default for GitConfig {
    fn default() -> Self {
        Self {
            workspace_dir: PathBuf::from("./workspace"),
            default_branch: "main".to_string(),
            shallow_clone: true,
            repos_dir: PathBuf::from("/app/repos"),
            sync_interval_secs: 300,
        }
    }
}

// Scanner configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScannerConfig {
    // Maximum file size to scan (in bytes)
    pub max_file_size: usize,
    // Whether to include test files
    pub include_tests: bool,
    // Patterns to exclude from scanning
    pub exclude_patterns: Vec<String>,
    // File extensions to skip entirely (e.g. ["min.js", "map", "lock"]).
    // Loaded from `SCANNER_SKIP_EXTENSIONS` (comma-separated).
    pub skip_extensions: Vec<String>,
}

impl Default for ScannerConfig {
    fn default() -> Self {
        Self {
            max_file_size: 1_000_000,
            include_tests: true, // Include test files by default for comprehensive analysis
            exclude_patterns: vec![
                "target/".to_string(),
                "node_modules/".to_string(),
                ".git/".to_string(),
                "__pycache__/".to_string(),
                "*.lock".to_string(),
            ],
            skip_extensions: default_skip_extensions(),
        }
    }
}

// Default file extensions that are always skipped by the scanner.
pub fn default_skip_extensions() -> Vec<String> {
    vec![
        "min.js".to_string(),
        "min.css".to_string(),
        "map".to_string(),
        "lock".to_string(),
        "snap".to_string(),
        "pb".to_string(),
        "wasm".to_string(),
        "ico".to_string(),
        "png".to_string(),
        "jpg".to_string(),
        "jpeg".to_string(),
        "gif".to_string(),
        "svg".to_string(),
        "ttf".to_string(),
        "woff".to_string(),
        "woff2".to_string(),
    ]
}

// Storage configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StorageConfig {
    // Directory where audit reports are saved
    pub reports_dir: PathBuf,
    // Directory where generated tasks are saved
    pub tasks_dir: PathBuf,
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            reports_dir: PathBuf::from("./reports"),
            tasks_dir: PathBuf::from("./tasks"),
        }
    }
}

// Database configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DatabaseConfig {
    // PostgreSQL connection URL — set via DATABASE_URL
    pub url: String,
}

impl Default for DatabaseConfig {
    fn default() -> Self {
        Self {
            url: "postgresql://rustcode:changeme@localhost:5432/rustcode.db".to_string(),
        }
    }
}

// Model router configuration for XAI/Ollama inference routing
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelConfig {
    // XAI (Grok) API key — set via XAI_API_KEY
    pub xai_api_key: Option<String>,
    // Remote model name (e.g. grok-4-1-fast-reasoning) — set via REMOTE_MODEL
    pub remote_model: String,
    // Local Ollama model name (e.g. qwen2.5-coder:7b) — set via LOCAL_MODEL
    pub local_model: String,
    // Ollama base URL — set via OLLAMA_BASE_URL
    pub ollama_base_url: String,
    // Force all requests through the remote model, skip local — set via FORCE_REMOTE_MODEL
    pub force_remote: bool,
}

impl Default for ModelConfig {
    fn default() -> Self {
        Self {
            xai_api_key: None,
            remote_model: "grok-4-1-fast-reasoning".to_string(),
            local_model: "qwen2.5-coder:7b".to_string(),
            ollama_base_url: "http://localhost:11434".to_string(),
            force_remote: false,
        }
    }
}

// Auto-scanner configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AutoScanConfig {
    // Whether the auto-scanner is enabled — set via AUTO_SCAN_ENABLED
    pub enabled: bool,
    // Scan interval in minutes — set via AUTO_SCAN_INTERVAL
    pub interval_minutes: u64,
    // Maximum number of concurrent scans — set via AUTO_SCAN_MAX_CONCURRENT
    pub max_concurrent: usize,
    // Per-scan cost budget in USD (0.0 = unlimited) — set via AUTO_SCAN_COST_BUDGET
    pub cost_budget: f64,
}

impl Default for AutoScanConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            interval_minutes: 60,
            max_concurrent: 2,
            cost_budget: 3.00,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = Config::default();
        assert_eq!(config.server.port, 3500);
        assert_eq!(config.llm.provider, "xai");
        assert!(!config.llm.enabled);
        assert!(config.research.is_some());
        assert!(config.research.unwrap().enabled);
        assert!(!config.security.allowed_git_hosts.is_empty());
        assert!(config.security.require_https);
    }

    #[test]
    fn test_security_validate_git_url_allowed() {
        let security = SecurityConfig::default();

        // Valid HTTPS URLs
        assert!(
            security
                .validate_git_url("https://github.com/user/repo.git")
                .is_ok()
        );
        assert!(
            security
                .validate_git_url("https://gitlab.com/user/repo")
                .is_ok()
        );

        // Valid SSH URLs
        assert!(
            security
                .validate_git_url("git@github.com:user/repo.git")
                .is_ok()
        );
    }

    #[test]
    fn test_security_validate_git_url_blocked() {
        let security = SecurityConfig::default();

        // Unknown host
        assert!(
            security
                .validate_git_url("https://evil.com/user/repo.git")
                .is_err()
        );

        // HTTP when HTTPS required
        assert!(
            security
                .validate_git_url("http://github.com/user/repo.git")
                .is_err()
        );

        // Private IP (SSRF prevention)
        let mut security_with_host = security.clone();
        security_with_host
            .allowed_git_hosts
            .push("192.168.1.1".to_string());
        // Even if host is whitelisted, private IPs should be blocked
        assert!(
            security_with_host
                .validate_git_url("https://192.168.1.1/repo.git")
                .is_err()
        );
    }

    #[test]
    fn test_security_validate_local_path() {
        let mut security = SecurityConfig {
            allow_local_paths: true,
            ..Default::default()
        };

        // Valid local paths
        assert!(security.validate_local_path("/home/user/repo").is_ok());
        assert!(security.validate_local_path("./repo").is_ok());

        // Path traversal blocked
        assert!(security.validate_local_path("../../../etc/passwd").is_err());
        assert!(security.validate_local_path("/home/../etc/passwd").is_err());

        // Local paths disabled
        security.allow_local_paths = false;
        assert!(security.validate_local_path("/home/user/repo").is_err());
    }

    #[test]
    fn test_validate_missing_api_key() {
        let mut config = Config::default();
        config.llm.enabled = true;
        config.llm.api_key = None;

        let result = config.validate();
        assert!(result.is_err());
    }

    #[test]
    fn test_validate_with_api_key() {
        let mut config = Config::default();
        config.llm.enabled = true;
        config.llm.api_key = Some("test-key".to_string());

        let result = config.validate();
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_invalid_port() {
        let mut config = Config::default();
        config.server.port = 0;

        let result = config.validate();
        assert!(result.is_err());
    }

    #[test]
    fn test_default_database_config() {
        let config = Config::default();
        assert!(config.database.url.contains("rustcode"));
    }

    #[test]
    fn test_default_model_config() {
        let config = Config::default();
        assert!(config.model.xai_api_key.is_none());
        assert_eq!(config.model.remote_model, "grok-4-1-fast-reasoning");
        assert_eq!(config.model.local_model, "qwen2.5-coder:7b");
        assert_eq!(config.model.ollama_base_url, "http://localhost:11434");
        assert!(!config.model.force_remote);
    }

    #[test]
    fn test_default_auto_scan_config() {
        let config = Config::default();
        assert!(config.auto_scan.enabled);
        assert_eq!(config.auto_scan.interval_minutes, 60);
        assert_eq!(config.auto_scan.max_concurrent, 2);
        assert!((config.auto_scan.cost_budget - 3.00).abs() < f64::EPSILON);
    }

    #[test]
    fn test_default_git_config_new_fields() {
        let config = Config::default();
        assert_eq!(config.git.sync_interval_secs, 300);
        assert_eq!(config.git.repos_dir, std::path::PathBuf::from("/app/repos"));
    }

    #[test]
    fn test_default_security_webhook_secret() {
        let config = Config::default();
        assert!(config.security.webhook_secret.is_empty());
    }
}
