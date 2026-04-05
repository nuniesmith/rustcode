// Documentation Generator
//
// Automatically generates documentation using LLM analysis.
//
// # Examples
//
// ```no_run
// use rustcode::doc_generator::DocGenerator;
// use rustcode::db::Database;
//
// #[tokio::main]
// async fn main() -> anyhow::Result<()> {
//     let db = Database::new("sqlite:data/rustcode.db").await?;
//     let generator = DocGenerator::new(db).await?;
//
//     let docs = generator.generate_module_docs("src/db.rs").await?;
//     println!("{}", generator.format_module_doc(&docs));
//
//     Ok(())
// }
// ```

use crate::db::Database;
use crate::GrokClient;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::Path;

// ============================================================================
// Data Types
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModuleDoc {
    pub module_name: String,
    pub summary: String,
    pub functions: Vec<FunctionDoc>,
    pub examples: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionDoc {
    pub name: String,
    pub signature: String,
    pub description: String,
    pub parameters: Vec<ParameterDoc>,
    pub returns: String,
    pub examples: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParameterDoc {
    pub name: String,
    pub param_type: String,
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReadmeContent {
    pub title: String,
    pub description: String,
    pub features: Vec<String>,
    pub installation: String,
    pub usage_examples: Vec<String>,
    pub architecture: String,
    pub contributing: String,
}

// ============================================================================
// DocGenerator
// ============================================================================

pub struct DocGenerator {
    grok_client: GrokClient,
}

impl DocGenerator {
    // Create a new documentation generator
    pub async fn new(db: Database) -> Result<Self> {
        let grok_client = GrokClient::from_env(db).await?;
        Ok(Self { grok_client })
    }

    // Generate documentation for a Rust module/file
    pub async fn generate_module_docs(&self, file_path: impl AsRef<Path>) -> Result<ModuleDoc> {
        let file_path = file_path.as_ref();
        let content = std::fs::read_to_string(file_path)?;

        let module_name = file_path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown")
            .to_string();

        let prompt = self.build_module_doc_prompt(&module_name, &content);

        let response = self.grok_client.ask(&prompt, None).await?;

        // Try to parse JSON response
        let doc: ModuleDoc = serde_json::from_str(&response).map_err(|e| {
            anyhow::anyhow!(
                "Failed to parse module doc JSON: {}.\nResponse preview: {}",
                e,
                &response.chars().take(200).collect::<String>()
            )
        })?;

        Ok(doc)
    }

    // Generate README content from repository analysis
    pub async fn generate_readme(&self, repo_path: impl AsRef<Path>) -> Result<ReadmeContent> {
        let repo_path = repo_path.as_ref();

        let context = self.build_repo_context(repo_path)?;
        let prompt = self.build_readme_prompt(&context);

        let response = self.grok_client.ask(&prompt, None).await?;

        let readme: ReadmeContent = serde_json::from_str(&response).map_err(|e| {
            anyhow::anyhow!(
                "Failed to parse README JSON: {}.\nResponse preview: {}",
                e,
                &response.chars().take(200).collect::<String>()
            )
        })?;

        Ok(readme)
    }

    // Format module documentation as Markdown
    pub fn format_module_doc(&self, doc: &ModuleDoc) -> String {
        let mut md = String::new();

        md.push_str(&format!("# Module: {}\n\n", doc.module_name));
        md.push_str(&format!("{}\n\n", doc.summary));

        if !doc.functions.is_empty() {
            md.push_str("## Functions\n\n");

            for func in &doc.functions {
                md.push_str(&format!("### `{}`\n\n", func.name));
                md.push_str(&format!("```rust\n{}\n```\n\n", func.signature));
                md.push_str(&format!("{}\n\n", func.description));

                if !func.parameters.is_empty() {
                    md.push_str("**Parameters:**\n\n");
                    for param in &func.parameters {
                        md.push_str(&format!(
                            "- `{}` (`{}`): {}\n",
                            param.name, param.param_type, param.description
                        ));
                    }
                    md.push('\n');
                }

                md.push_str(&format!("**Returns:** {}\n\n", func.returns));

                if !func.examples.is_empty() {
                    md.push_str("**Examples:**\n\n");
                    for example in &func.examples {
                        md.push_str(&format!("```rust\n{}\n```\n\n", example));
                    }
                }
            }
        }

        if !doc.examples.is_empty() {
            md.push_str("## Module Examples\n\n");
            for example in &doc.examples {
                md.push_str(&format!("```rust\n{}\n```\n\n", example));
            }
        }

        md
    }

    // Format README content as Markdown
    pub fn format_readme(&self, content: &ReadmeContent) -> String {
        let mut md = String::new();

        md.push_str(&format!("# {}\n\n", content.title));
        md.push_str(&format!("{}\n\n", content.description));

        md.push_str("## Features\n\n");
        for feature in &content.features {
            md.push_str(&format!("- {}\n", feature));
        }
        md.push('\n');

        md.push_str("## Installation\n\n");
        md.push_str(&content.installation);
        md.push_str("\n\n");

        md.push_str("## Usage\n\n");
        for example in &content.usage_examples {
            md.push_str(&format!("```rust\n{}\n```\n\n", example));
        }

        md.push_str("## Architecture\n\n");
        md.push_str(&content.architecture);
        md.push_str("\n\n");

        md.push_str("## Contributing\n\n");
        md.push_str(&content.contributing);
        md.push('\n');

        md
    }

    // ========================================================================
    // Private Helper Methods
    // ========================================================================

    fn build_module_doc_prompt(&self, module_name: &str, content: &str) -> String {
        format!(
            r#"You are a documentation expert. Analyze this Rust code and generate comprehensive documentation.

File: {module_name}.rs

Code:
```rust
{content}
```

Generate a JSON response with:
1. Module summary (2-3 clear sentences explaining what this module does)
2. List of public functions with detailed documentation:
   - Function name
   - Full signature
   - Clear description of what it does
   - Parameter documentation (name, type, description)
   - Return value description
   - Usage examples
3. Overall module usage examples

Focus on public APIs. Be accurate and concise.

Respond ONLY with valid JSON matching this structure:
{{
  "module_name": "{module_name}",
  "summary": "Brief summary of the module",
  "functions": [
    {{
      "name": "function_name",
      "signature": "pub async fn name(param: Type) -> Result<T>",
      "description": "What this function does",
      "parameters": [
        {{"name": "param", "param_type": "Type", "description": "What it means"}}
      ],
      "returns": "Description of return value",
      "examples": ["example_code()"]
    }}
  ],
  "examples": ["module_level_example()"]
}}"#,
            module_name = module_name,
            content = content
        )
    }

    fn build_repo_context(&self, repo_path: &Path) -> Result<String> {
        let mut context = String::new();

        // Read Cargo.toml
        let cargo_toml = repo_path.join("Cargo.toml");
        if cargo_toml.exists() {
            context.push_str("=== Cargo.toml ===\n");
            context.push_str(&std::fs::read_to_string(cargo_toml)?);
            context.push_str("\n\n");
        }

        // Read src/lib.rs or src/main.rs (first 200 lines)
        let lib_rs = repo_path.join("src/lib.rs");
        let main_rs = repo_path.join("src/main.rs");

        if lib_rs.exists() {
            context.push_str("=== src/lib.rs (first 200 lines) ===\n");
            let content = std::fs::read_to_string(lib_rs)?;
            let lines: Vec<&str> = content.lines().take(200).collect();
            context.push_str(&lines.join("\n"));
            context.push_str("\n\n");
        } else if main_rs.exists() {
            context.push_str("=== src/main.rs (first 200 lines) ===\n");
            let content = std::fs::read_to_string(main_rs)?;
            let lines: Vec<&str> = content.lines().take(200).collect();
            context.push_str(&lines.join("\n"));
            context.push_str("\n\n");
        }

        // Read existing README if it exists
        let readme = repo_path.join("README.md");
        if readme.exists() {
            context.push_str("=== Existing README.md (first 50 lines) ===\n");
            let content = std::fs::read_to_string(readme)?;
            let lines: Vec<&str> = content.lines().take(50).collect();
            context.push_str(&lines.join("\n"));
            context.push_str("\n\n");
        }

        Ok(context)
    }

    fn build_readme_prompt(&self, context: &str) -> String {
        format!(
            r#"You are a technical writer. Generate a professional README.md for this Rust project.

Project Context:
{context}

Create a comprehensive README with:
1. Project title and one-line tagline
2. Clear description (2-3 paragraphs: what it does, why it exists, who it's for)
3. Key features (5-8 bullet points)
4. Installation instructions (cargo install, git clone, etc.)
5. Usage examples with actual code
6. Architecture overview
7. Contributing guidelines

Be professional, clear, and accurate.

Respond ONLY with valid JSON:
{{
  "title": "Project Name",
  "description": "Detailed description...",
  "features": [
    "Feature 1",
    "Feature 2"
  ],
  "installation": "Installation instructions as markdown",
  "usage_examples": [
    "code_example_1",
    "code_example_2"
  ],
  "architecture": "Architecture overview as markdown",
  "contributing": "Contributing guidelines as markdown"
}}"#,
            context = context
        )
    }
}
