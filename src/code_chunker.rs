//! AST-Aware Code Chunker for Cross-Repo Vector Indexing
//!
//! This module provides semantic code chunking that splits source files at
//! meaningful boundaries (functions, structs, impls, traits) rather than
//! arbitrary line/word counts. Each chunk carries rich metadata for:
//!
//! 1. **Content-addressable deduplication** — same function across repos = one embedding
//! 2. **Cross-repo similarity search** — find near-duplicate patterns via vector similarity
//! 3. **Smart LLM triage** — only send chunks with static issues to the LLM
//!
//! # Architecture
//!
//! ```text
//! Source File → CodeChunker::chunk_file()
//!              ├─ Language detection
//!              ├─ Line-by-line boundary detection (fn, struct, impl, trait, mod, etc.)
//!              ├─ Doc comment + attribute attachment
//!              ├─ Metadata extraction (visibility, imports, complexity)
//!              └─ Content hashing for dedup
//!
//! Result: Vec<CodeChunk> with full metadata per chunk
//! ```
//!
//! # Cross-Repo Deduplication Strategy
//!
//! The key insight: hash the chunk **content**, not the file path. If the same
//! function appears in multiple repos (or as a copy-paste), it gets one embedding
//! and one analysis result.
//!
//! ```text
//! Global Index:
//!   content_hash → (embedding, analysis_result, [(repo_a, path_a), (repo_b, path_b)])
//! ```
//!
//! When scanning repo B and a chunk matches a hash from repo A's cache:
//! - Skip embedding generation (free)
//! - Skip LLM analysis (free)
//! - Link the existing result to the new location
//!
//! # Supported Languages
//!
//! - **Rust**: `fn`, `struct`, `enum`, `trait`, `impl`, `mod`, `const`, `static`, `type`
//! - **Kotlin**: `fun`, `class`, `object`, `interface`, `data class`, `sealed class`
//! - **Python**: `def`, `class`, `async def`
//! - **Go**: `func`, `type`, `struct`
//! - **TypeScript/JavaScript**: `function`, `class`, `interface`, `const`, `export`
//!
//! For unsupported languages, falls back to paragraph-based chunking.

use regex::Regex;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::Path;
use tracing::{debug, warn};

use crate::static_analysis::FileLanguage;

// ============================================================================
// Core Types
// ============================================================================

/// A semantically meaningful chunk of code with rich metadata.
///
/// This is the unit of storage for the cross-repo vector index.
/// Each chunk maps to one embedding vector and can be independently
/// analyzed, cached, and deduplicated.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodeChunk {
    // --- Identity ---
    /// SHA-256 hash of the chunk content (for deduplication)
    pub content_hash: String,

    /// The repo this chunk was extracted from
    pub repo_id: String,

    /// Relative file path within the repo
    pub file_path: String,

    // --- Content ---
    /// The actual chunk content (code text)
    pub content: String,

    /// Word count of the content
    pub word_count: usize,

    // --- Semantic ---
    /// What kind of code entity this chunk represents
    pub entity_type: EntityType,

    /// Name of the entity (e.g., "parse_config", "MyStruct")
    pub entity_name: String,

    /// Programming language
    pub language: FileLanguage,

    // --- Position ---
    /// Start line in the original file (1-based)
    pub start_line: u32,

    /// End line in the original file (1-based, inclusive)
    pub end_line: u32,

    // --- Context ---
    /// Parent module path (e.g., "crate::config", "com.example.app")
    pub parent_module: String,

    /// Import/use statements this chunk depends on
    pub imports_used: Vec<String>,

    /// Whether this entity is public/exported
    pub is_public: bool,

    /// Whether a test exists for this entity (heuristic)
    pub has_tests: bool,

    /// Whether this chunk is itself test code
    pub is_test_code: bool,

    // --- Analysis Metadata (filled in later by static_analysis + LLM) ---
    /// Complexity score from static analysis (0.0 = trivial, 1.0 = very complex)
    pub complexity_score: f32,

    /// Number of issues found (static + LLM combined)
    pub issue_count: u32,

    /// Unix timestamp of last analysis
    pub last_analyzed: i64,

    // --- Embedding (filled in later by indexing pipeline) ---
    /// The embedding vector (e.g., 384-dim from bge-small-en-v1.5)
    /// Empty until the embedding pipeline runs.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub vector: Vec<f32>,
}

impl CodeChunk {
    /// Create a new CodeChunk with the minimum required fields.
    /// Analysis metadata and embedding are left as defaults.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        content: String,
        repo_id: String,
        file_path: String,
        entity_type: EntityType,
        entity_name: String,
        language: FileLanguage,
        start_line: u32,
        end_line: u32,
    ) -> Self {
        let content_hash = compute_content_hash(&content);
        let word_count = content.split_whitespace().count();

        Self {
            content_hash,
            repo_id,
            file_path,
            content,
            word_count,
            entity_type,
            entity_name,
            language,
            start_line,
            end_line,
            parent_module: String::new(),
            imports_used: Vec::new(),
            is_public: false,
            has_tests: false,
            is_test_code: false,
            complexity_score: 0.0,
            issue_count: 0,
            last_analyzed: 0,
            vector: Vec::new(),
        }
    }

    /// Set the parent module path
    pub fn with_parent_module(mut self, module: impl Into<String>) -> Self {
        self.parent_module = module.into();
        self
    }

    /// Set the imports used by this chunk
    pub fn with_imports(mut self, imports: Vec<String>) -> Self {
        self.imports_used = imports;
        self
    }

    /// Set public visibility
    pub fn with_public(mut self, is_public: bool) -> Self {
        self.is_public = is_public;
        self
    }

    /// Mark as test code
    pub fn with_test_code(mut self, is_test: bool) -> Self {
        self.is_test_code = is_test;
        self
    }

    /// Set the complexity score
    pub fn with_complexity(mut self, score: f32) -> Self {
        self.complexity_score = score;
        self
    }

    /// A compact one-line identifier for logging
    pub fn display_id(&self) -> String {
        format!(
            "{}::{}({}) [L{}-{}]",
            self.file_path, self.entity_name, self.entity_type, self.start_line, self.end_line
        )
    }
}

/// The type of code entity a chunk represents
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntityType {
    /// A function or method definition
    Function,
    /// A struct/data class definition
    Struct,
    /// An enum definition
    Enum,
    /// A trait/interface/protocol definition
    Trait,
    /// An impl block (Rust-specific)
    ImplBlock,
    /// A class definition
    Class,
    /// A module declaration
    Module,
    /// Constants and static variables grouped together
    Constants,
    /// Type aliases
    TypeAlias,
    /// Import/use statements grouped together
    Imports,
    /// Test function or test module
    Test,
    /// Top-level code that doesn't fit other categories
    TopLevel,
    /// Doc comment or documentation block
    Documentation,
}

impl std::fmt::Display for EntityType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Function => write!(f, "function"),
            Self::Struct => write!(f, "struct"),
            Self::Enum => write!(f, "enum"),
            Self::Trait => write!(f, "trait"),
            Self::ImplBlock => write!(f, "impl"),
            Self::Class => write!(f, "class"),
            Self::Module => write!(f, "module"),
            Self::Constants => write!(f, "constants"),
            Self::TypeAlias => write!(f, "type_alias"),
            Self::Imports => write!(f, "imports"),
            Self::Test => write!(f, "test"),
            Self::TopLevel => write!(f, "top_level"),
            Self::Documentation => write!(f, "documentation"),
        }
    }
}

// ============================================================================
// Configuration
// ============================================================================

/// Configuration for code chunking behavior
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChunkerConfig {
    /// Maximum chunk size in lines before forcing a split (default: 200)
    pub max_chunk_lines: usize,

    /// Minimum chunk size in lines — smaller chunks get merged with neighbors (default: 3)
    pub min_chunk_lines: usize,

    /// Whether to include doc comments as part of their associated entity (default: true)
    pub attach_doc_comments: bool,

    /// Whether to include attributes (#[...]) as part of their associated entity (default: true)
    pub attach_attributes: bool,

    /// Whether to group consecutive constants into a single chunk (default: true)
    pub group_constants: bool,

    /// Whether to group use/import statements into a single chunk (default: true)
    pub group_imports: bool,

    /// Whether to extract test functions as separate chunks (default: true)
    pub separate_tests: bool,

    /// Maximum number of chunks per file (safety limit, default: 500)
    pub max_chunks_per_file: usize,
}

impl Default for ChunkerConfig {
    fn default() -> Self {
        Self {
            max_chunk_lines: 200,
            min_chunk_lines: 3,
            attach_doc_comments: true,
            attach_attributes: true,
            group_constants: true,
            group_imports: true,
            separate_tests: true,
            max_chunks_per_file: 500,
        }
    }
}

// ============================================================================
// Code Chunker
// ============================================================================

/// The main code chunker that splits source files into semantic chunks.
pub struct CodeChunker {
    config: ChunkerConfig,
    /// Pre-compiled patterns for Rust boundary detection
    rust_patterns: RustPatterns,
    /// Pre-compiled patterns for Kotlin boundary detection
    kotlin_patterns: KotlinPatterns,
    /// Pre-compiled patterns for general boundary detection
    general_patterns: GeneralPatterns,
}

/// Pre-compiled regex patterns for Rust code boundaries
struct RustPatterns {
    fn_def: Regex,
    struct_def: Regex,
    enum_def: Regex,
    trait_def: Regex,
    impl_block: Regex,
    mod_def: Regex,
    const_static: Regex,
    type_alias: Regex,
    use_stmt: Regex,
    test_fn: Regex,
    test_mod: Regex,
    doc_comment: Regex,
    attribute: Regex,
}

/// Pre-compiled regex patterns for Kotlin code boundaries
struct KotlinPatterns {
    fun_def: Regex,
    class_def: Regex,
    object_def: Regex,
    interface_def: Regex,
    import_stmt: Regex,
}

/// Pre-compiled patterns for general/fallback boundary detection
struct GeneralPatterns {
    python_def: Regex,
    python_class: Regex,
    go_func: Regex,
    go_type: Regex,
    ts_function: Regex,
    ts_class: Regex,
    ts_interface: Regex,
    ts_export_const: Regex,
}

impl RustPatterns {
    fn new() -> Self {
        Self {
            fn_def: Regex::new(r"^(\s*)(pub(\s*\(crate\))?\s+)?(async\s+)?(unsafe\s+)?fn\s+(\w+)")
                .unwrap(),
            struct_def: Regex::new(r"^(\s*)(pub(\s*\(crate\))?\s+)?struct\s+(\w+)").unwrap(),
            enum_def: Regex::new(r"^(\s*)(pub(\s*\(crate\))?\s+)?enum\s+(\w+)").unwrap(),
            trait_def: Regex::new(r"^(\s*)(pub(\s*\(crate\))?\s+)?(unsafe\s+)?trait\s+(\w+)")
                .unwrap(),
            impl_block: Regex::new(r"^(\s*)(unsafe\s+)?impl(<[^>]*>)?\s+(\w+)").unwrap(),
            mod_def: Regex::new(r"^(\s*)(pub(\s*\(crate\))?\s+)?mod\s+(\w+)").unwrap(),
            const_static: Regex::new(r"^(\s*)(pub(\s*\(crate\))?\s+)?(const|static)\s+(\w+)")
                .unwrap(),
            type_alias: Regex::new(r"^(\s*)(pub(\s*\(crate\))?\s+)?type\s+(\w+)").unwrap(),
            use_stmt: Regex::new(r"^\s*(?:pub\s+)?use\s+").unwrap(),
            test_fn: Regex::new(r"#\[test\]|#\[tokio::test\]|#\[async_std::test\]").unwrap(),
            test_mod: Regex::new(r"#\[cfg\(test\)\]").unwrap(),
            doc_comment: Regex::new(r"^\s*(///|//!)").unwrap(),
            attribute: Regex::new(r"^\s*#\[").unwrap(),
        }
    }
}

impl KotlinPatterns {
    fn new() -> Self {
        Self {
            fun_def: Regex::new(
                r"^\s*(override\s+)?(public|private|protected|internal)?\s*(suspend\s+)?fun\s+(\w+)",
            )
            .unwrap(),
            class_def: Regex::new(
                r"^\s*(public|private|protected|internal)?\s*(data\s+|sealed\s+|abstract\s+|open\s+)?class\s+(\w+)",
            )
            .unwrap(),
            object_def: Regex::new(
                r"^\s*(public|private|protected|internal)?\s*(companion\s+)?object\s+(\w+)?",
            )
            .unwrap(),
            interface_def: Regex::new(
                r"^\s*(public|private|protected|internal)?\s*interface\s+(\w+)",
            )
            .unwrap(),
            import_stmt: Regex::new(r"^\s*import\s+").unwrap(),
        }
    }
}

impl GeneralPatterns {
    fn new() -> Self {
        Self {
            python_def: Regex::new(r"^(\s*)(async\s+)?def\s+(\w+)").unwrap(),
            python_class: Regex::new(r"^(\s*)class\s+(\w+)").unwrap(),
            go_func: Regex::new(r"^func\s+(\(.*?\)\s+)?(\w+)").unwrap(),
            go_type: Regex::new(r"^type\s+(\w+)\s+(struct|interface)").unwrap(),
            ts_function: Regex::new(r"^\s*(export\s+)?(async\s+)?function\s+(\w+)").unwrap(),
            ts_class: Regex::new(r"^\s*(export\s+)?(abstract\s+)?class\s+(\w+)").unwrap(),
            ts_interface: Regex::new(r"^\s*(export\s+)?interface\s+(\w+)").unwrap(),
            ts_export_const: Regex::new(r"^\s*export\s+(const|let|var)\s+(\w+)").unwrap(),
        }
    }
}

impl CodeChunker {
    /// Create a new code chunker with default configuration
    pub fn new() -> Self {
        Self {
            config: ChunkerConfig::default(),
            rust_patterns: RustPatterns::new(),
            kotlin_patterns: KotlinPatterns::new(),
            general_patterns: GeneralPatterns::new(),
        }
    }

    /// Create a new code chunker with custom configuration
    pub fn with_config(config: ChunkerConfig) -> Self {
        Self {
            config,
            rust_patterns: RustPatterns::new(),
            kotlin_patterns: KotlinPatterns::new(),
            general_patterns: GeneralPatterns::new(),
        }
    }

    /// Chunk a source file into semantic code chunks.
    ///
    /// This is the main entry point. It detects the language from the file path,
    /// then uses language-specific boundary detection to split the file.
    pub fn chunk_file(&self, file_path: &str, content: &str, repo_id: &str) -> Vec<CodeChunk> {
        let language = FileLanguage::from_extension(file_path);
        let lines: Vec<&str> = content.lines().collect();

        if lines.is_empty() {
            return Vec::new();
        }

        // Detect boundaries based on language
        let boundaries = match language {
            FileLanguage::Rust => self.detect_rust_boundaries(&lines),
            FileLanguage::Kotlin => self.detect_kotlin_boundaries(&lines),
            FileLanguage::Python => self.detect_python_boundaries(&lines),
            FileLanguage::Go => self.detect_go_boundaries(&lines),
            FileLanguage::TypeScript | FileLanguage::JavaScript => {
                self.detect_ts_boundaries(&lines)
            }
            _ => self.detect_generic_boundaries(&lines),
        };

        // Convert boundaries into chunks
        let mut chunks =
            self.boundaries_to_chunks(&boundaries, &lines, file_path, repo_id, language);

        // Extract file-level imports
        let imports = self.extract_imports(&lines, language);

        // Determine parent module
        let parent_module = self.detect_parent_module(file_path, &lines, language);

        // Check for test presence
        let test_fn_names = self.extract_test_names(&lines, language);

        // Enrich each chunk with context
        for chunk in &mut chunks {
            chunk.parent_module = parent_module.clone();
            chunk.imports_used = self.find_relevant_imports(&chunk.content, &imports);

            // Check if this entity has a corresponding test
            if !chunk.is_test_code {
                chunk.has_tests = test_fn_names
                    .iter()
                    .any(|test_name| test_name.contains(&chunk.entity_name));
            }

            // Compute simple complexity score
            chunk.complexity_score = self.compute_chunk_complexity(&chunk.content);
        }

        // Enforce max chunks limit
        if chunks.len() > self.config.max_chunks_per_file {
            warn!(
                "File {} produced {} chunks (limit {}), truncating",
                file_path,
                chunks.len(),
                self.config.max_chunks_per_file
            );
            chunks.truncate(self.config.max_chunks_per_file);
        }

        debug!(
            "Chunked {} into {} chunks ({})",
            file_path,
            chunks.len(),
            language
        );

        chunks
    }

    /// Chunk a file by reading it from disk.
    pub fn chunk_file_from_path(
        &self,
        file_path: &Path,
        repo_id: &str,
    ) -> std::io::Result<Vec<CodeChunk>> {
        let content = std::fs::read_to_string(file_path)?;
        let rel_path = file_path.to_string_lossy();
        Ok(self.chunk_file(&rel_path, &content, repo_id))
    }

    /// Get the chunker configuration
    pub fn config(&self) -> &ChunkerConfig {
        &self.config
    }

    // ========================================================================
    // Rust Boundary Detection
    // ========================================================================

    fn detect_rust_boundaries(&self, lines: &[&str]) -> Vec<Boundary> {
        let mut boundaries: Vec<Boundary> = Vec::new();
        let mut i = 0;
        let mut in_test_module = false;
        let mut pending_doc_start: Option<usize> = None;
        let mut pending_attr_start: Option<usize> = None;
        let mut is_next_test_fn = false;

        while i < lines.len() {
            let line = lines[i];
            let trimmed = line.trim();

            // Track doc comments
            if self.rust_patterns.doc_comment.is_match(line) {
                if pending_doc_start.is_none() {
                    pending_doc_start = Some(i);
                }
                i += 1;
                continue;
            }

            // Track attributes
            if self.rust_patterns.attribute.is_match(line) {
                if pending_attr_start.is_none() && pending_doc_start.is_none() {
                    pending_attr_start = Some(i);
                }

                // Check for test markers
                if self.rust_patterns.test_fn.is_match(trimmed) {
                    is_next_test_fn = true;
                }
                if self.rust_patterns.test_mod.is_match(trimmed) {
                    in_test_module = true;
                }

                i += 1;
                continue;
            }

            // Reset doc/attr tracking on blank lines (they break the chain)
            if trimmed.is_empty() {
                // Only reset if we haven't started collecting for an entity
                pending_doc_start = None;
                pending_attr_start = None;
                is_next_test_fn = false;
                i += 1;
                continue;
            }

            // Detect entity boundaries
            let entity_start = pending_doc_start.or(pending_attr_start).unwrap_or(i);

            // use statements
            if self.config.group_imports && self.rust_patterns.use_stmt.is_match(line) {
                // Collect consecutive use statements
                let start = entity_start;
                let mut end = i;
                while end + 1 < lines.len() {
                    let next = lines[end + 1].trim();
                    if self.rust_patterns.use_stmt.is_match(lines[end + 1]) || next.is_empty() {
                        end += 1;
                        if next.is_empty() && end + 1 < lines.len() {
                            // Allow one blank line between use groups
                            if !self.rust_patterns.use_stmt.is_match(lines[end + 1]) {
                                break;
                            }
                        }
                    } else {
                        break;
                    }
                }

                boundaries.push(Boundary {
                    start_line: start,
                    entity_start_line: i,
                    entity_type: EntityType::Imports,
                    entity_name: "imports".to_string(),
                    is_public: false,
                    is_test: false,
                });

                pending_doc_start = None;
                pending_attr_start = None;
                i = end + 1;
                continue;
            }

            // fn definitions
            if let Some(caps) = self.rust_patterns.fn_def.captures(line) {
                let name = caps.get(6).map(|m| m.as_str()).unwrap_or("anonymous");
                let is_pub = line.contains("pub ");
                let is_test = is_next_test_fn || in_test_module;

                boundaries.push(Boundary {
                    start_line: entity_start,
                    entity_start_line: i,
                    entity_type: if is_test {
                        EntityType::Test
                    } else {
                        EntityType::Function
                    },
                    entity_name: name.to_string(),
                    is_public: is_pub,
                    is_test,
                });

                pending_doc_start = None;
                pending_attr_start = None;
                is_next_test_fn = false;
                i += 1;
                continue;
            }

            // struct definitions
            if let Some(caps) = self.rust_patterns.struct_def.captures(line) {
                let name = caps.get(4).map(|m| m.as_str()).unwrap_or("Unknown");
                boundaries.push(Boundary {
                    start_line: entity_start,
                    entity_start_line: i,
                    entity_type: EntityType::Struct,
                    entity_name: name.to_string(),
                    is_public: line.contains("pub "),
                    is_test: in_test_module,
                });
                pending_doc_start = None;
                pending_attr_start = None;
                i += 1;
                continue;
            }

            // enum definitions
            if let Some(caps) = self.rust_patterns.enum_def.captures(line) {
                let name = caps.get(4).map(|m| m.as_str()).unwrap_or("Unknown");
                boundaries.push(Boundary {
                    start_line: entity_start,
                    entity_start_line: i,
                    entity_type: EntityType::Enum,
                    entity_name: name.to_string(),
                    is_public: line.contains("pub "),
                    is_test: in_test_module,
                });
                pending_doc_start = None;
                pending_attr_start = None;
                i += 1;
                continue;
            }

            // trait definitions
            if let Some(caps) = self.rust_patterns.trait_def.captures(line) {
                let name = caps.get(5).map(|m| m.as_str()).unwrap_or("Unknown");
                boundaries.push(Boundary {
                    start_line: entity_start,
                    entity_start_line: i,
                    entity_type: EntityType::Trait,
                    entity_name: name.to_string(),
                    is_public: line.contains("pub "),
                    is_test: in_test_module,
                });
                pending_doc_start = None;
                pending_attr_start = None;
                i += 1;
                continue;
            }

            // impl blocks
            if let Some(caps) = self.rust_patterns.impl_block.captures(line) {
                let name = caps.get(4).map(|m| m.as_str()).unwrap_or("Unknown");
                // Try to capture "impl Trait for Type" pattern
                let full_name = if line.contains(" for ") {
                    let parts: Vec<&str> = line.split(" for ").collect();
                    if parts.len() >= 2 {
                        let impl_part = parts[0].trim().trim_start_matches("impl").trim();
                        let type_part = parts[1].split_whitespace().next().unwrap_or("");
                        let type_part = type_part.trim_end_matches('{').trim();
                        format!("{} for {}", impl_part, type_part)
                    } else {
                        name.to_string()
                    }
                } else {
                    name.to_string()
                };

                boundaries.push(Boundary {
                    start_line: entity_start,
                    entity_start_line: i,
                    entity_type: EntityType::ImplBlock,
                    entity_name: full_name,
                    is_public: false,
                    is_test: in_test_module,
                });
                pending_doc_start = None;
                pending_attr_start = None;
                i += 1;
                continue;
            }

            // mod definitions
            if let Some(caps) = self.rust_patterns.mod_def.captures(line) {
                let name = caps.get(4).map(|m| m.as_str()).unwrap_or("unknown");
                if name == "tests" {
                    in_test_module = true;
                }
                boundaries.push(Boundary {
                    start_line: entity_start,
                    entity_start_line: i,
                    entity_type: EntityType::Module,
                    entity_name: name.to_string(),
                    is_public: line.contains("pub "),
                    is_test: name == "tests" || in_test_module,
                });
                pending_doc_start = None;
                pending_attr_start = None;
                i += 1;
                continue;
            }

            // const/static
            if self.rust_patterns.const_static.is_match(line) {
                if let Some(caps) = self.rust_patterns.const_static.captures(line) {
                    let name = caps.get(5).map(|m| m.as_str()).unwrap_or("UNKNOWN");
                    boundaries.push(Boundary {
                        start_line: entity_start,
                        entity_start_line: i,
                        entity_type: EntityType::Constants,
                        entity_name: name.to_string(),
                        is_public: line.contains("pub "),
                        is_test: in_test_module,
                    });
                }
                pending_doc_start = None;
                pending_attr_start = None;
                i += 1;
                continue;
            }

            // type alias
            if let Some(caps) = self.rust_patterns.type_alias.captures(line) {
                let name = caps.get(4).map(|m| m.as_str()).unwrap_or("Unknown");
                boundaries.push(Boundary {
                    start_line: entity_start,
                    entity_start_line: i,
                    entity_type: EntityType::TypeAlias,
                    entity_name: name.to_string(),
                    is_public: line.contains("pub "),
                    is_test: in_test_module,
                });
                pending_doc_start = None;
                pending_attr_start = None;
                i += 1;
                continue;
            }

            // Non-boundary line — reset pending state
            pending_doc_start = None;
            pending_attr_start = None;
            is_next_test_fn = false;
            i += 1;
        }

        boundaries
    }

    // ========================================================================
    // Kotlin Boundary Detection
    // ========================================================================

    fn detect_kotlin_boundaries(&self, lines: &[&str]) -> Vec<Boundary> {
        let mut boundaries: Vec<Boundary> = Vec::new();
        let mut i = 0;

        while i < lines.len() {
            let line = lines[i];
            let trimmed = line.trim();

            // Skip blank lines
            if trimmed.is_empty() {
                i += 1;
                continue;
            }

            // import statements
            if self.config.group_imports && self.kotlin_patterns.import_stmt.is_match(line) {
                let start = i;
                while i + 1 < lines.len()
                    && (self.kotlin_patterns.import_stmt.is_match(lines[i + 1])
                        || lines[i + 1].trim().is_empty())
                {
                    i += 1;
                    if lines[i].trim().is_empty()
                        && i + 1 < lines.len()
                        && !self.kotlin_patterns.import_stmt.is_match(lines[i + 1])
                    {
                        break;
                    }
                }
                boundaries.push(Boundary {
                    start_line: start,
                    entity_start_line: start,
                    entity_type: EntityType::Imports,
                    entity_name: "imports".to_string(),
                    is_public: false,
                    is_test: false,
                });
                i += 1;
                continue;
            }

            // fun definitions
            if let Some(caps) = self.kotlin_patterns.fun_def.captures(line) {
                let name = caps.get(4).map(|m| m.as_str()).unwrap_or("anonymous");
                let is_pub = !line.contains("private") && !line.contains("protected");
                let is_test = trimmed.starts_with("@Test") || {
                    i > 0 && lines[i - 1].trim().starts_with("@Test")
                };

                boundaries.push(Boundary {
                    start_line: i,
                    entity_start_line: i,
                    entity_type: if is_test {
                        EntityType::Test
                    } else {
                        EntityType::Function
                    },
                    entity_name: name.to_string(),
                    is_public: is_pub,
                    is_test,
                });
                i += 1;
                continue;
            }

            // class definitions
            if let Some(caps) = self.kotlin_patterns.class_def.captures(line) {
                let name = caps.get(3).map(|m| m.as_str()).unwrap_or("Unknown");
                boundaries.push(Boundary {
                    start_line: i,
                    entity_start_line: i,
                    entity_type: EntityType::Class,
                    entity_name: name.to_string(),
                    is_public: !line.contains("private") && !line.contains("protected"),
                    is_test: false,
                });
                i += 1;
                continue;
            }

            // object definitions
            if let Some(caps) = self.kotlin_patterns.object_def.captures(line) {
                let name = caps.get(3).map(|m| m.as_str()).unwrap_or("companion");
                boundaries.push(Boundary {
                    start_line: i,
                    entity_start_line: i,
                    entity_type: EntityType::Class,
                    entity_name: name.to_string(),
                    is_public: !line.contains("private"),
                    is_test: false,
                });
                i += 1;
                continue;
            }

            // interface definitions
            if let Some(caps) = self.kotlin_patterns.interface_def.captures(line) {
                let name = caps.get(2).map(|m| m.as_str()).unwrap_or("Unknown");
                boundaries.push(Boundary {
                    start_line: i,
                    entity_start_line: i,
                    entity_type: EntityType::Trait,
                    entity_name: name.to_string(),
                    is_public: !line.contains("private"),
                    is_test: false,
                });
                i += 1;
                continue;
            }

            i += 1;
        }

        boundaries
    }

    // ========================================================================
    // Python Boundary Detection
    // ========================================================================

    fn detect_python_boundaries(&self, lines: &[&str]) -> Vec<Boundary> {
        let mut boundaries: Vec<Boundary> = Vec::new();

        for (i, line) in lines.iter().enumerate() {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }

            // def / async def
            if let Some(caps) = self.general_patterns.python_def.captures(line) {
                let name = caps.get(3).map(|m| m.as_str()).unwrap_or("anonymous");
                let is_test = name.starts_with("test_") || name.starts_with("test");
                boundaries.push(Boundary {
                    start_line: i,
                    entity_start_line: i,
                    entity_type: if is_test {
                        EntityType::Test
                    } else {
                        EntityType::Function
                    },
                    entity_name: name.to_string(),
                    is_public: !name.starts_with('_'),
                    is_test,
                });
                continue;
            }

            // class
            if let Some(caps) = self.general_patterns.python_class.captures(line) {
                let name = caps.get(2).map(|m| m.as_str()).unwrap_or("Unknown");
                boundaries.push(Boundary {
                    start_line: i,
                    entity_start_line: i,
                    entity_type: EntityType::Class,
                    entity_name: name.to_string(),
                    is_public: !name.starts_with('_'),
                    is_test: name.starts_with("Test"),
                });
            }
        }

        boundaries
    }

    // ========================================================================
    // Go Boundary Detection
    // ========================================================================

    fn detect_go_boundaries(&self, lines: &[&str]) -> Vec<Boundary> {
        let mut boundaries: Vec<Boundary> = Vec::new();

        for (i, line) in lines.iter().enumerate() {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }

            // func
            if let Some(caps) = self.general_patterns.go_func.captures(trimmed) {
                let name = caps.get(2).map(|m| m.as_str()).unwrap_or("anonymous");
                let is_test = name.starts_with("Test") || name.starts_with("Benchmark");
                boundaries.push(Boundary {
                    start_line: i,
                    entity_start_line: i,
                    entity_type: if is_test {
                        EntityType::Test
                    } else {
                        EntityType::Function
                    },
                    entity_name: name.to_string(),
                    is_public: name.chars().next().is_some_and(|c| c.is_uppercase()),
                    is_test,
                });
                continue;
            }

            // type struct/interface
            if let Some(caps) = self.general_patterns.go_type.captures(trimmed) {
                let name = caps.get(1).map(|m| m.as_str()).unwrap_or("Unknown");
                let kind = caps.get(2).map(|m| m.as_str()).unwrap_or("struct");
                boundaries.push(Boundary {
                    start_line: i,
                    entity_start_line: i,
                    entity_type: if kind == "interface" {
                        EntityType::Trait
                    } else {
                        EntityType::Struct
                    },
                    entity_name: name.to_string(),
                    is_public: name.chars().next().is_some_and(|c| c.is_uppercase()),
                    is_test: false,
                });
            }
        }

        boundaries
    }

    // ========================================================================
    // TypeScript/JavaScript Boundary Detection
    // ========================================================================

    fn detect_ts_boundaries(&self, lines: &[&str]) -> Vec<Boundary> {
        let mut boundaries: Vec<Boundary> = Vec::new();

        for (i, line) in lines.iter().enumerate() {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }

            // function
            if let Some(caps) = self.general_patterns.ts_function.captures(line) {
                let name = caps.get(3).map(|m| m.as_str()).unwrap_or("anonymous");
                let is_test = name.starts_with("test")
                    || name.starts_with("it")
                    || trimmed.starts_with("it(")
                    || trimmed.starts_with("describe(")
                    || trimmed.starts_with("test(");
                boundaries.push(Boundary {
                    start_line: i,
                    entity_start_line: i,
                    entity_type: if is_test {
                        EntityType::Test
                    } else {
                        EntityType::Function
                    },
                    entity_name: name.to_string(),
                    is_public: line.contains("export"),
                    is_test,
                });
                continue;
            }

            // class
            if let Some(caps) = self.general_patterns.ts_class.captures(line) {
                let name = caps.get(3).map(|m| m.as_str()).unwrap_or("Unknown");
                boundaries.push(Boundary {
                    start_line: i,
                    entity_start_line: i,
                    entity_type: EntityType::Class,
                    entity_name: name.to_string(),
                    is_public: line.contains("export"),
                    is_test: false,
                });
                continue;
            }

            // interface
            if let Some(caps) = self.general_patterns.ts_interface.captures(line) {
                let name = caps.get(2).map(|m| m.as_str()).unwrap_or("Unknown");
                boundaries.push(Boundary {
                    start_line: i,
                    entity_start_line: i,
                    entity_type: EntityType::Trait,
                    entity_name: name.to_string(),
                    is_public: line.contains("export"),
                    is_test: false,
                });
                continue;
            }

            // export const
            if let Some(caps) = self.general_patterns.ts_export_const.captures(line) {
                let name = caps.get(2).map(|m| m.as_str()).unwrap_or("UNKNOWN");
                boundaries.push(Boundary {
                    start_line: i,
                    entity_start_line: i,
                    entity_type: EntityType::Constants,
                    entity_name: name.to_string(),
                    is_public: true,
                    is_test: false,
                });
            }
        }

        boundaries
    }

    // ========================================================================
    // Generic/Fallback Boundary Detection
    // ========================================================================

    fn detect_generic_boundaries(&self, lines: &[&str]) -> Vec<Boundary> {
        // For unknown languages, chunk at blank-line paragraph boundaries
        let mut boundaries: Vec<Boundary> = Vec::new();
        let mut chunk_start: Option<usize> = None;

        for (i, line) in lines.iter().enumerate() {
            if line.trim().is_empty() {
                if chunk_start.is_some() {
                    chunk_start = None;
                }
            } else if chunk_start.is_none() {
                chunk_start = Some(i);
                boundaries.push(Boundary {
                    start_line: i,
                    entity_start_line: i,
                    entity_type: EntityType::TopLevel,
                    entity_name: format!("block_{}", boundaries.len()),
                    is_public: false,
                    is_test: false,
                });
            }
        }

        boundaries
    }

    // ========================================================================
    // Boundary → Chunk Conversion
    // ========================================================================

    /// Convert detected boundaries into actual CodeChunk objects by extracting
    /// the content between consecutive boundaries and tracking brace depth.
    fn boundaries_to_chunks(
        &self,
        boundaries: &[Boundary],
        lines: &[&str],
        file_path: &str,
        repo_id: &str,
        language: FileLanguage,
    ) -> Vec<CodeChunk> {
        if boundaries.is_empty() {
            // No boundaries found — return the whole file as a single chunk
            if lines.is_empty() {
                return Vec::new();
            }
            let content = lines.join("\n");
            let chunk = CodeChunk::new(
                content,
                repo_id.to_string(),
                file_path.to_string(),
                EntityType::TopLevel,
                "file".to_string(),
                language,
                1,
                lines.len() as u32,
            );
            return vec![chunk];
        }

        let mut chunks: Vec<CodeChunk> = Vec::new();
        let uses_braces = matches!(
            language,
            FileLanguage::Rust
                | FileLanguage::Kotlin
                | FileLanguage::Go
                | FileLanguage::Java
                | FileLanguage::TypeScript
                | FileLanguage::JavaScript
                | FileLanguage::Cpp
                | FileLanguage::C
                | FileLanguage::Swift
        );

        for (idx, boundary) in boundaries.iter().enumerate() {
            let start = boundary.start_line;

            // Determine end of this chunk
            let end = if uses_braces {
                // For brace-based languages, find the closing brace
                let brace_end = self.find_block_end(lines, boundary.entity_start_line);

                // But don't extend past the next boundary's start
                let next_start = if idx + 1 < boundaries.len() {
                    boundaries[idx + 1].start_line
                } else {
                    lines.len()
                };

                brace_end
                    .min(next_start)
                    .max(boundary.entity_start_line + 1)
            } else {
                // For indentation-based languages (Python), use next boundary
                if idx + 1 < boundaries.len() {
                    // Walk backward from next boundary to skip trailing blank lines
                    let mut end = boundaries[idx + 1].start_line;
                    while end > start + 1 && lines.get(end - 1).is_none_or(|l| l.trim().is_empty())
                    {
                        end -= 1;
                    }
                    end
                } else {
                    lines.len()
                }
            };

            // Ensure end > start and within bounds
            let end = end.min(lines.len()).max(start + 1);

            // Extract content
            let chunk_lines = &lines[start..end];
            let content = chunk_lines.join("\n");

            // Skip chunks that are too small (unless they're constants/imports)
            if chunk_lines.len() < self.config.min_chunk_lines
                && !matches!(
                    boundary.entity_type,
                    EntityType::Constants | EntityType::Imports | EntityType::TypeAlias
                )
            {
                continue;
            }

            // Split oversized chunks at function boundaries within impl blocks
            if chunk_lines.len() > self.config.max_chunk_lines
                && boundary.entity_type == EntityType::ImplBlock
            {
                // For large impl blocks, try to split at inner fn boundaries
                let sub_chunks = self.split_large_impl_block(
                    chunk_lines,
                    start,
                    file_path,
                    repo_id,
                    language,
                    &boundary.entity_name,
                );
                if !sub_chunks.is_empty() {
                    chunks.extend(sub_chunks);
                    continue;
                }
            }

            let chunk = CodeChunk::new(
                content,
                repo_id.to_string(),
                file_path.to_string(),
                boundary.entity_type,
                boundary.entity_name.clone(),
                language,
                (start + 1) as u32,
                end as u32,
            )
            .with_public(boundary.is_public)
            .with_test_code(boundary.is_test);

            chunks.push(chunk);
        }

        chunks
    }

    /// Find the end of a brace-delimited block starting at a given line.
    /// Tracks brace depth to handle nested blocks correctly.
    fn find_block_end(&self, lines: &[&str], start: usize) -> usize {
        let mut depth: i32 = 0;
        let mut found_open = false;

        for (i, line) in lines.iter().enumerate().skip(start) {
            let line = *line;

            // Skip string literals (simplified — doesn't handle all edge cases)
            let mut in_string = false;
            let mut prev_char = ' ';
            for ch in line.chars() {
                if ch == '"' && prev_char != '\\' {
                    in_string = !in_string;
                }
                if !in_string {
                    if ch == '{' {
                        depth += 1;
                        found_open = true;
                    } else if ch == '}' {
                        depth -= 1;
                    }
                }
                prev_char = ch;
            }

            // Block ends when we return to depth 0 after opening
            if found_open && depth <= 0 {
                return i + 1; // Include the closing brace line
            }
        }

        // If no matching brace found, return to end of file
        lines.len()
    }

    /// Split a large impl block into individual method chunks.
    fn split_large_impl_block(
        &self,
        block_lines: &[&str],
        global_offset: usize,
        file_path: &str,
        repo_id: &str,
        language: FileLanguage,
        impl_name: &str,
    ) -> Vec<CodeChunk> {
        let mut chunks: Vec<CodeChunk> = Vec::new();

        // Find fn boundaries within the impl block
        let mut fn_starts: Vec<(usize, String, bool)> = Vec::new(); // (line, name, is_pub)

        for (i, line) in block_lines.iter().enumerate() {
            if let Some(caps) = self.rust_patterns.fn_def.captures(line) {
                let name = caps.get(6).map(|m| m.as_str()).unwrap_or("anonymous");
                let is_pub = line.contains("pub ");
                fn_starts.push((i, name.to_string(), is_pub));
            }
        }

        if fn_starts.is_empty() {
            return Vec::new(); // Can't split — return empty to fall back to single chunk
        }

        // Include the impl header as its own small chunk
        if let Some(&(first_fn_line, _, _)) = fn_starts.first() {
            if first_fn_line > 0 {
                let header = block_lines[..first_fn_line].join("\n");
                if !header.trim().is_empty() {
                    chunks.push(CodeChunk::new(
                        header,
                        repo_id.to_string(),
                        file_path.to_string(),
                        EntityType::ImplBlock,
                        format!("{} (header)", impl_name),
                        language,
                        (global_offset + 1) as u32,
                        (global_offset + first_fn_line) as u32,
                    ));
                }
            }
        }

        // Create a chunk for each method
        for (idx, (start, name, is_pub)) in fn_starts.iter().enumerate() {
            let end = if idx + 1 < fn_starts.len() {
                // Look back from next fn to find actual end of this fn
                let next_start = fn_starts[idx + 1].0;
                // Walk backwards to skip doc comments/attributes of next fn
                let mut actual_end = next_start;
                while actual_end > *start + 1 {
                    let prev = block_lines[actual_end - 1].trim();
                    if prev.is_empty() || prev.starts_with("///") || prev.starts_with("#[") {
                        actual_end -= 1;
                    } else {
                        break;
                    }
                }
                actual_end
            } else {
                block_lines.len()
            };

            let content = block_lines[*start..end].join("\n");
            chunks.push(
                CodeChunk::new(
                    content,
                    repo_id.to_string(),
                    file_path.to_string(),
                    EntityType::Function,
                    format!("{}::{}", impl_name, name),
                    language,
                    (global_offset + start + 1) as u32,
                    (global_offset + end) as u32,
                )
                .with_public(*is_pub),
            );
        }

        chunks
    }

    // ========================================================================
    // Context Extraction Helpers
    // ========================================================================

    /// Extract all import/use statements from the file
    fn extract_imports(&self, lines: &[&str], language: FileLanguage) -> Vec<String> {
        let pattern = match language {
            FileLanguage::Rust => &self.rust_patterns.use_stmt,
            FileLanguage::Kotlin => &self.kotlin_patterns.import_stmt,
            _ => return Vec::new(),
        };

        lines
            .iter()
            .filter(|line| pattern.is_match(line))
            .map(|line| line.trim().to_string())
            .collect()
    }

    /// Find which imports are relevant to a chunk by checking if the imported
    /// name appears in the chunk content.
    fn find_relevant_imports(&self, chunk_content: &str, all_imports: &[String]) -> Vec<String> {
        all_imports
            .iter()
            .filter(|import| {
                // Extract the final segment of the import path
                let last_segment = import
                    .rsplit("::")
                    .next()
                    .or_else(|| import.rsplit('.').next())
                    .unwrap_or("")
                    .trim_end_matches(';')
                    .trim_end_matches('{')
                    .trim();

                // Check if this import's name appears in the chunk
                !last_segment.is_empty() && chunk_content.contains(last_segment)
            })
            .cloned()
            .collect()
    }

    /// Detect the parent module path from the file structure and content
    fn detect_parent_module(
        &self,
        file_path: &str,
        lines: &[&str],
        language: FileLanguage,
    ) -> String {
        match language {
            FileLanguage::Rust => {
                // Convert file path to module path
                // e.g., "src/scanner/mod.rs" → "crate::scanner"
                // e.g., "src/auto_scanner.rs" → "crate::auto_scanner"
                let path = file_path
                    .trim_start_matches("src/")
                    .trim_end_matches("/mod.rs")
                    .trim_end_matches(".rs")
                    .replace('/', "::");
                format!("crate::{}", path)
            }
            FileLanguage::Kotlin | FileLanguage::Java => {
                // Look for package declaration
                for line in lines.iter().take(10) {
                    let trimmed = line.trim();
                    if trimmed.starts_with("package ") {
                        return trimmed
                            .trim_start_matches("package ")
                            .trim_end_matches(';')
                            .trim()
                            .to_string();
                    }
                }
                String::new()
            }
            FileLanguage::Python => {
                // Convert file path to Python module path
                file_path
                    .trim_end_matches("/__init__.py")
                    .trim_end_matches(".py")
                    .replace('/', ".")
            }
            FileLanguage::Go => {
                // Look for package declaration
                for line in lines.iter().take(10) {
                    let trimmed = line.trim();
                    if trimmed.starts_with("package ") {
                        return trimmed.trim_start_matches("package ").trim().to_string();
                    }
                }
                String::new()
            }
            _ => String::new(),
        }
    }

    /// Extract test function names (for cross-referencing with entity names)
    fn extract_test_names(&self, lines: &[&str], language: FileLanguage) -> Vec<String> {
        let mut names = Vec::new();

        match language {
            FileLanguage::Rust => {
                let mut next_is_test = false;
                for line in lines {
                    let trimmed = line.trim();
                    if self.rust_patterns.test_fn.is_match(trimmed) {
                        next_is_test = true;
                        continue;
                    }
                    if next_is_test {
                        if let Some(caps) = self.rust_patterns.fn_def.captures(line) {
                            if let Some(name) = caps.get(6) {
                                names.push(name.as_str().to_string());
                            }
                        }
                        next_is_test = false;
                    }
                }
            }
            FileLanguage::Python => {
                for line in lines {
                    if let Some(caps) = self.general_patterns.python_def.captures(line) {
                        if let Some(name) = caps.get(3) {
                            let n = name.as_str();
                            if n.starts_with("test_") || n.starts_with("test") {
                                names.push(n.to_string());
                            }
                        }
                    }
                }
            }
            FileLanguage::Go => {
                for line in lines {
                    if let Some(caps) = self.general_patterns.go_func.captures(line.trim()) {
                        if let Some(name) = caps.get(2) {
                            let n = name.as_str();
                            if n.starts_with("Test") {
                                names.push(n.to_string());
                            }
                        }
                    }
                }
            }
            _ => {}
        }

        names
    }

    /// Compute a simple complexity score for a chunk (0.0–1.0)
    fn compute_chunk_complexity(&self, content: &str) -> f32 {
        let lines: Vec<&str> = content.lines().collect();
        let line_count = lines.len().max(1);

        let mut score: f32 = 0.0;

        // Line count contributes to complexity (normalized to ~200 lines = 0.3)
        score += (line_count as f32 / 200.0).min(0.3);

        // Count decision points
        let decision_points: usize = lines
            .iter()
            .filter(|l| {
                let t = l.trim();
                !t.starts_with("//")
                    && (t.starts_with("if ")
                        || t.contains(" if ")
                        || t.starts_with("match ")
                        || t.contains(" match ")
                        || t.starts_with("while ")
                        || t.starts_with("for ")
                        || t.starts_with("loop")
                        || t.contains("&&")
                        || t.contains("||"))
            })
            .count();
        score += (decision_points as f32 / 20.0).min(0.3);

        // Nesting depth
        let max_nesting = lines
            .iter()
            .map(|l| (l.len() - l.trim_start().len()) / 4)
            .max()
            .unwrap_or(0);
        score += (max_nesting as f32 / 8.0).min(0.2);

        // Unwrap/panic usage
        let unsafe_patterns: usize = lines
            .iter()
            .filter(|l| l.contains(".unwrap()") || l.contains("panic!(") || l.contains("unsafe "))
            .count();
        score += (unsafe_patterns as f32 / 10.0).min(0.2);

        score.min(1.0)
    }
}

impl Default for CodeChunker {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// Internal Types
// ============================================================================

/// A detected boundary in source code (internal representation)
#[derive(Debug)]
struct Boundary {
    /// Line index where this entity's chunk starts (including doc comments/attributes)
    start_line: usize,
    /// Line index where the actual entity definition starts
    entity_start_line: usize,
    /// What kind of entity this is
    entity_type: EntityType,
    /// Name of the entity
    entity_name: String,
    /// Whether the entity is public
    is_public: bool,
    /// Whether this is test code
    is_test: bool,
}

// ============================================================================
// Utility Functions
// ============================================================================

/// Compute a SHA-256 content hash for deduplication
pub fn compute_content_hash(content: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(content.as_bytes());
    hex::encode(hasher.finalize())
}

/// Summary statistics for a batch of chunks
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ChunkingStats {
    /// Total chunks produced
    pub total_chunks: usize,
    /// Chunks by entity type
    pub by_type: std::collections::HashMap<String, usize>,
    /// Number of unique content hashes (for dedup measurement)
    pub unique_hashes: usize,
    /// Number of duplicate chunks detected
    pub duplicate_count: usize,
    /// Total word count across all chunks
    pub total_words: usize,
    /// Average chunk size in lines
    pub avg_chunk_lines: f64,
}

/// Compute statistics for a batch of chunks
pub fn compute_chunking_stats(chunks: &[CodeChunk]) -> ChunkingStats {
    use std::collections::{HashMap, HashSet};

    let mut by_type: HashMap<String, usize> = HashMap::new();
    let mut hashes: HashSet<String> = HashSet::new();
    let mut total_words = 0usize;
    let mut total_lines = 0usize;

    for chunk in chunks {
        *by_type.entry(chunk.entity_type.to_string()).or_insert(0) += 1;
        hashes.insert(chunk.content_hash.clone());
        total_words += chunk.word_count;
        total_lines += (chunk.end_line - chunk.start_line + 1) as usize;
    }

    let unique = hashes.len();
    let total = chunks.len();

    ChunkingStats {
        total_chunks: total,
        by_type,
        unique_hashes: unique,
        duplicate_count: total.saturating_sub(unique),
        total_words,
        avg_chunk_lines: if total > 0 {
            total_lines as f64 / total as f64
        } else {
            0.0
        },
    }
}

// ============================================================================
// Cross-Repo Deduplication Index
// ============================================================================

/// An entry in the global deduplication index.
///
/// Maps a content hash to its embedding and the set of locations where this
/// exact code appears across repos.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DedupEntry {
    /// Content hash (SHA-256)
    pub content_hash: String,
    /// The embedding vector (shared across all locations)
    pub vector: Vec<f32>,
    /// All locations where this exact code appears
    pub locations: Vec<ChunkLocation>,
    /// Analysis result (shared)
    pub issue_count: u32,
    /// Last analyzed timestamp
    pub last_analyzed: i64,
}

/// A specific location where a chunk appears
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChunkLocation {
    pub repo_id: String,
    pub file_path: String,
    pub start_line: u32,
    pub end_line: u32,
    pub entity_name: String,
}

/// A simple in-memory dedup index for tracking cross-repo duplicates.
///
/// In production, this would be backed by SQLite/LanceDB, but this provides
/// the interface and logic for the dedup strategy.
#[derive(Debug, Default)]
pub struct DedupIndex {
    entries: std::collections::HashMap<String, DedupEntry>,
}

impl DedupIndex {
    pub fn new() -> Self {
        Self {
            entries: std::collections::HashMap::new(),
        }
    }

    /// Check if a content hash already exists in the index
    pub fn contains(&self, content_hash: &str) -> bool {
        self.entries.contains_key(content_hash)
    }

    /// Get an existing entry by content hash
    pub fn get(&self, content_hash: &str) -> Option<&DedupEntry> {
        self.entries.get(content_hash)
    }

    /// Insert or update a chunk in the index.
    /// If the hash already exists, adds the new location. Returns true if this
    /// was a new entry (needs embedding), false if it was a duplicate (free).
    pub fn insert_or_link(&mut self, chunk: &CodeChunk) -> bool {
        let location = ChunkLocation {
            repo_id: chunk.repo_id.clone(),
            file_path: chunk.file_path.clone(),
            start_line: chunk.start_line,
            end_line: chunk.end_line,
            entity_name: chunk.entity_name.clone(),
        };

        if let Some(entry) = self.entries.get_mut(&chunk.content_hash) {
            // Duplicate — just add the new location
            let already_linked = entry
                .locations
                .iter()
                .any(|loc| loc.repo_id == location.repo_id && loc.file_path == location.file_path);
            if !already_linked {
                entry.locations.push(location);
            }
            false // Was duplicate — skip embedding
        } else {
            // New entry — needs embedding
            self.entries.insert(
                chunk.content_hash.clone(),
                DedupEntry {
                    content_hash: chunk.content_hash.clone(),
                    vector: chunk.vector.clone(),
                    locations: vec![location],
                    issue_count: chunk.issue_count,
                    last_analyzed: chunk.last_analyzed,
                },
            );
            true // New — needs embedding
        }
    }

    /// Get all entries that appear in multiple repos (cross-repo duplicates)
    pub fn cross_repo_duplicates(&self) -> Vec<&DedupEntry> {
        self.entries
            .values()
            .filter(|entry| {
                let unique_repos: std::collections::HashSet<&str> = entry
                    .locations
                    .iter()
                    .map(|loc| loc.repo_id.as_str())
                    .collect();
                unique_repos.len() > 1
            })
            .collect()
    }

    /// Get total number of unique chunks
    pub fn unique_count(&self) -> usize {
        self.entries.len()
    }

    /// Get total number of duplicate links saved
    pub fn duplicates_saved(&self) -> usize {
        self.entries
            .values()
            .map(|e| e.locations.len().saturating_sub(1))
            .sum()
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn chunker() -> CodeChunker {
        CodeChunker::new()
    }

    #[test]
    fn test_rust_function_chunking() {
        let content = r#"use std::fs;
use std::path::Path;

/// Read a config file from disk
pub fn read_config(path: &str) -> Result<String, std::io::Error> {
    let content = fs::read_to_string(path)?;
    Ok(content)
}

/// Write data to a file
fn write_data(path: &str, data: &str) -> Result<(), std::io::Error> {
    fs::write(path, data)?;
    Ok(())
}
"#;

        let chunks = chunker().chunk_file("src/config.rs", content, "test-repo");
        assert!(!chunks.is_empty());

        // Should have imports + 2 functions
        let fn_chunks: Vec<_> = chunks
            .iter()
            .filter(|c| c.entity_type == EntityType::Function)
            .collect();
        assert_eq!(fn_chunks.len(), 2);
        assert_eq!(fn_chunks[0].entity_name, "read_config");
        assert_eq!(fn_chunks[1].entity_name, "write_data");

        // read_config should be public
        assert!(fn_chunks[0].is_public);
        assert!(!fn_chunks[1].is_public);

        // Both should have a parent module
        assert_eq!(fn_chunks[0].parent_module, "crate::config");
    }

    #[test]
    fn test_rust_struct_and_impl() {
        let content = r#"/// A user in the system
pub struct User {
    pub name: String,
    pub email: String,
    age: u32,
}

impl User {
    pub fn new(name: String, email: String, age: u32) -> Self {
        Self { name, email, age }
    }

    pub fn display_name(&self) -> &str {
        &self.name
    }
}
"#;

        let chunks = chunker().chunk_file("src/models.rs", content, "repo");

        let struct_chunks: Vec<_> = chunks
            .iter()
            .filter(|c| c.entity_type == EntityType::Struct)
            .collect();
        assert_eq!(struct_chunks.len(), 1);
        assert_eq!(struct_chunks[0].entity_name, "User");
        assert!(struct_chunks[0].is_public);

        let impl_or_fn_chunks: Vec<_> = chunks
            .iter()
            .filter(|c| {
                c.entity_type == EntityType::ImplBlock || c.entity_type == EntityType::Function
            })
            .collect();
        assert!(!impl_or_fn_chunks.is_empty());
    }

    #[test]
    fn test_rust_test_detection() {
        let content = r#"pub fn add(a: i32, b: i32) -> i32 {
    a + b
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_add() {
        assert_eq!(add(1, 2), 3);
    }

    #[test]
    fn test_add_negative() {
        assert_eq!(add(-1, 1), 0);
    }
}
"#;

        let chunks = chunker().chunk_file("src/math.rs", content, "repo");

        let test_chunks: Vec<_> = chunks.iter().filter(|c| c.is_test_code).collect();
        assert!(!test_chunks.is_empty());

        let fn_chunks: Vec<_> = chunks
            .iter()
            .filter(|c| c.entity_type == EntityType::Function && !c.is_test_code)
            .collect();
        assert_eq!(fn_chunks.len(), 1);
        assert_eq!(fn_chunks[0].entity_name, "add");
    }

    #[test]
    fn test_content_hash_dedup() {
        let content = "pub fn helper() -> bool { true }";
        let hash1 = compute_content_hash(content);
        let hash2 = compute_content_hash(content);
        assert_eq!(hash1, hash2);

        let different = "pub fn helper() -> bool { false }";
        let hash3 = compute_content_hash(different);
        assert_ne!(hash1, hash3);
    }

    #[test]
    fn test_dedup_index() {
        let mut index = DedupIndex::new();

        let chunk1 = CodeChunk::new(
            "pub fn shared() -> i32 { 42 }".to_string(),
            "repo_a".to_string(),
            "src/utils.rs".to_string(),
            EntityType::Function,
            "shared".to_string(),
            FileLanguage::Rust,
            1,
            1,
        );

        // First insertion — should be new
        assert!(index.insert_or_link(&chunk1));

        // Same content, different repo — should be duplicate
        let mut chunk2 = chunk1.clone();
        chunk2.repo_id = "repo_b".to_string();
        chunk2.file_path = "src/helpers.rs".to_string();
        assert!(!index.insert_or_link(&chunk2));

        assert_eq!(index.unique_count(), 1);
        assert_eq!(index.duplicates_saved(), 1);

        let cross = index.cross_repo_duplicates();
        assert_eq!(cross.len(), 1);
        assert_eq!(cross[0].locations.len(), 2);
    }

    #[test]
    fn test_empty_file() {
        let chunks = chunker().chunk_file("empty.rs", "", "repo");
        assert!(chunks.is_empty());
    }

    #[test]
    fn test_kotlin_chunking() {
        let content = r#"package com.example.app

import kotlinx.coroutines.flow.Flow

class UserRepository(private val dao: UserDao) {
    fun getUser(id: String): User {
        return dao.findById(id)
    }

    suspend fun saveUser(user: User) {
        dao.insert(user)
    }
}

interface UserDao {
    fun findById(id: String): User
    fun insert(user: User)
}
"#;

        let chunks = chunker().chunk_file("UserRepository.kt", content, "repo");
        assert!(!chunks.is_empty());

        // Check for class or function chunks (class body may be split into functions)
        let class_or_fn_chunks: Vec<_> = chunks
            .iter()
            .filter(|c| {
                c.entity_type == EntityType::Class
                    || c.entity_type == EntityType::Function
                    || c.entity_type == EntityType::Trait
            })
            .collect();
        assert!(!class_or_fn_chunks.is_empty());

        // Check parent module detected from package
        for chunk in &chunks {
            if !chunk.parent_module.is_empty() {
                assert_eq!(chunk.parent_module, "com.example.app");
            }
        }
    }

    #[test]
    fn test_python_chunking() {
        let content = r#"import os
from pathlib import Path

class FileManager:
    def __init__(self, root: str):
        self.root = Path(root)

    def read_file(self, name: str) -> str:
        return (self.root / name).read_text()

def test_file_manager():
    fm = FileManager("/tmp")
    assert fm.root == Path("/tmp")
"#;

        let chunks = chunker().chunk_file("file_manager.py", content, "repo");
        assert!(!chunks.is_empty());

        let test_chunks: Vec<_> = chunks
            .iter()
            .filter(|c| c.entity_type == EntityType::Test)
            .collect();
        assert!(!test_chunks.is_empty());
    }

    #[test]
    fn test_chunking_stats() {
        // Use multi-line functions so each chunk exceeds min_chunk_lines
        let content = r#"pub fn alpha(x: i32) -> i32 {
    let result = x * 2;
    result + 1
}

pub fn beta(x: i32) -> i32 {
    let result = x * 3;
    result + 2
}

pub fn gamma(x: i32) -> i32 {
    let result = x * 4;
    result + 3
}
"#;
        let chunks = chunker().chunk_file("src/fns.rs", content, "repo");
        let stats = compute_chunking_stats(&chunks);
        assert!(stats.total_chunks > 0);
        assert!(stats.avg_chunk_lines > 0.0);
    }

    #[test]
    fn test_chunk_complexity_score() {
        let c = chunker();

        let simple = "pub fn add(a: i32, b: i32) -> i32 { a + b }";
        assert!(c.compute_chunk_complexity(simple) < 0.3);

        let complex = r#"
pub fn process(items: &[Item]) -> Result<Vec<Output>, Error> {
    let mut results = Vec::new();
    for item in items {
        if item.is_valid() {
            match item.kind() {
                Kind::A => {
                    if item.value > 100 && item.enabled {
                        for sub in item.children() {
                            if sub.active || sub.forced {
                                results.push(sub.process()?);
                            }
                        }
                    }
                }
                Kind::B => {
                    while let Some(next) = item.next()? {
                        results.push(next);
                    }
                }
                _ => {}
            }
        }
    }
    Ok(results)
}
"#;
        assert!(c.compute_chunk_complexity(complex) > 0.4);
    }

    #[test]
    fn test_code_chunk_display_id() {
        let chunk = CodeChunk::new(
            "fn test() {}".to_string(),
            "repo".to_string(),
            "src/lib.rs".to_string(),
            EntityType::Function,
            "test".to_string(),
            FileLanguage::Rust,
            10,
            12,
        );
        assert_eq!(chunk.display_id(), "src/lib.rs::test(function) [L10-12]");
    }

    #[test]
    fn test_parent_module_detection() {
        let c = chunker();

        // Rust
        assert_eq!(
            c.detect_parent_module("src/scanner/mod.rs", &[], FileLanguage::Rust),
            "crate::scanner"
        );
        assert_eq!(
            c.detect_parent_module("src/auto_scanner.rs", &[], FileLanguage::Rust),
            "crate::auto_scanner"
        );

        // Go
        let go_lines = vec!["package main", "", "func init() {}"];
        assert_eq!(
            c.detect_parent_module("cmd/server.go", &go_lines, FileLanguage::Go),
            "main"
        );
    }
}
