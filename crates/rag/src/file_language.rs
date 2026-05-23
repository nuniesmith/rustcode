// Detected file language. Used by `code_chunker` (for chunk-language
// tagging and entity recognition) and by `rustcode::static_analysis`
// (for comment-stripping heuristics in `strip_for_prompt`). Lives in
// the `rag` crate so both crates can depend on a single source of
// truth rather than each maintaining a copy.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum FileLanguage {
    Rust,
    Kotlin,
    Python,
    TypeScript,
    JavaScript,
    Go,
    Java,
    Shell,
    Swift,
    Cpp,
    C,
    Unknown,
}

impl FileLanguage {
    // Detect language from file extension
    pub fn from_extension(path: &str) -> Self {
        let ext = path.rsplit('.').next().unwrap_or("");
        match ext {
            "rs" => Self::Rust,
            "kt" | "kts" => Self::Kotlin,
            "py" => Self::Python,
            "ts" | "tsx" => Self::TypeScript,
            "js" | "jsx" => Self::JavaScript,
            "go" => Self::Go,
            "java" => Self::Java,
            "sh" | "bash" | "zsh" => Self::Shell,
            "swift" => Self::Swift,
            "cpp" | "cxx" | "cc" | "hpp" => Self::Cpp,
            "c" | "h" => Self::C,
            _ => Self::Unknown,
        }
    }

    // Get single-line comment prefix for this language
    pub fn comment_prefix(&self) -> &'static str {
        match self {
            Self::Rust
            | Self::Kotlin
            | Self::TypeScript
            | Self::JavaScript
            | Self::Go
            | Self::Java
            | Self::Swift
            | Self::Cpp
            | Self::C => "//",
            Self::Python | Self::Shell => "#",
            Self::Unknown => "//",
        }
    }
}

impl std::fmt::Display for FileLanguage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Rust => write!(f, "rust"),
            Self::Kotlin => write!(f, "kotlin"),
            Self::Python => write!(f, "python"),
            Self::TypeScript => write!(f, "typescript"),
            Self::JavaScript => write!(f, "javascript"),
            Self::Go => write!(f, "go"),
            Self::Java => write!(f, "java"),
            Self::Shell => write!(f, "shell"),
            Self::Swift => write!(f, "swift"),
            Self::Cpp => write!(f, "cpp"),
            Self::C => write!(f, "c"),
            Self::Unknown => write!(f, "unknown"),
        }
    }
}
