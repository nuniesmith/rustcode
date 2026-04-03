//! Document Chunking Module
//!
//! This module provides intelligent text chunking for the RAG system.
//! It splits documents into semantically meaningful chunks while preserving
//! structure and adding overlap for better context retrieval.
//!
//! # Features
//!
//! - **Fixed-size chunking**: Split by word count with configurable size
//! - **Markdown-aware**: Preserve code blocks, headings, and formatting
//! - **Overlap**: Configurable overlap between chunks for context
//! - **Smart splitting**: Prefer splitting at paragraph boundaries
//!
//! # Example
//!
//! ```rust,no_run
//! use rustcode::chunking::{ChunkConfig, chunk_document};
//!
//! let config = ChunkConfig::default();
//! let content = "# Introduction\n\nThis is a long document...";
//! let chunks = chunk_document(content, &config).unwrap();
//! ```

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

// ============================================================================
// Configuration
// ============================================================================

/// Configuration for document chunking
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChunkConfig {
    /// Target number of words per chunk (default: 512)
    pub target_words: usize,

    /// Number of words to overlap between chunks (default: 100)
    pub overlap_words: usize,

    /// Minimum chunk size in words (default: 50)
    pub min_chunk_size: usize,

    /// Maximum chunk size in words before forcing split (default: 768)
    pub max_chunk_size: usize,

    /// Whether to preserve markdown structure (default: true)
    pub markdown_aware: bool,

    /// Whether to preserve code blocks (default: true)
    pub preserve_code_blocks: bool,

    /// Whether to include headings in context (default: true)
    pub include_headings: bool,
}

impl Default for ChunkConfig {
    fn default() -> Self {
        Self {
            target_words: 512,
            overlap_words: 100,
            min_chunk_size: 50,
            max_chunk_size: 768,
            markdown_aware: true,
            preserve_code_blocks: true,
            include_headings: true,
        }
    }
}

impl ChunkConfig {
    /// Create a small chunk configuration (256 words target)
    pub fn small() -> Self {
        Self {
            target_words: 256,
            overlap_words: 50,
            min_chunk_size: 25,
            max_chunk_size: 384,
            ..Default::default()
        }
    }

    /// Create a large chunk configuration (1024 words target)
    pub fn large() -> Self {
        Self {
            target_words: 1024,
            overlap_words: 200,
            min_chunk_size: 100,
            max_chunk_size: 1536,
            ..Default::default()
        }
    }

    /// Validate configuration values
    pub fn validate(&self) -> Result<()> {
        if self.target_words == 0 {
            anyhow::bail!("target_words must be greater than 0");
        }

        if self.min_chunk_size > self.target_words {
            anyhow::bail!("min_chunk_size must be <= target_words");
        }

        if self.max_chunk_size < self.target_words {
            anyhow::bail!("max_chunk_size must be >= target_words");
        }

        if self.overlap_words >= self.target_words {
            anyhow::bail!("overlap_words must be < target_words");
        }

        Ok(())
    }
}

// ============================================================================
// Data Structures
// ============================================================================

/// A chunk of document content with metadata
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ChunkData {
    /// The text content of the chunk
    pub content: String,

    /// Character offset where chunk starts in original document
    pub char_start: usize,

    /// Character offset where chunk ends in original document
    pub char_end: usize,

    /// Word count of this chunk
    pub word_count: usize,

    /// Optional heading context (nearest preceding heading)
    pub heading: Option<String>,

    /// Index of this chunk in the sequence (0-based)
    pub index: usize,
}

impl ChunkData {
    pub fn new(
        content: String,
        char_start: usize,
        char_end: usize,
        word_count: usize,
        heading: Option<String>,
        index: usize,
    ) -> Self {
        Self {
            content,
            char_start,
            char_end,
            word_count,
            heading,
            index,
        }
    }
}

/// Internal representation of a text segment
#[derive(Debug, Clone)]
struct TextSegment {
    content: String,
    char_start: usize,
    char_end: usize,
    segment_type: SegmentType,
}

#[derive(Debug, Clone, PartialEq)]
enum SegmentType {
    Text,
    CodeBlock,
    Heading,
}

// ============================================================================
// Public API
// ============================================================================

/// Chunk a document according to the given configuration
///
/// This is the main entry point for chunking.
pub fn chunk_document(content: &str, config: &ChunkConfig) -> Result<Vec<ChunkData>> {
    config.validate().context("Invalid chunk configuration")?;

    if content.is_empty() {
        return Ok(Vec::new());
    }

    // Parse content into segments (text, code blocks, headings)
    let segments = if config.markdown_aware {
        parse_markdown_segments(content, config)
    } else {
        vec![TextSegment {
            content: content.to_string(),
            char_start: 0,
            char_end: content.len(),
            segment_type: SegmentType::Text,
        }]
    };

    // Chunk the segments
    let chunks = chunk_segments(&segments, config)?;

    Ok(chunks)
}

// ============================================================================
// Markdown Parsing
// ============================================================================

fn parse_markdown_segments(content: &str, config: &ChunkConfig) -> Vec<TextSegment> {
    let mut segments = Vec::new();
    let mut current_pos = 0;
    let mut in_code_block = false;
    let mut code_block_start = 0;
    let mut code_block_content = String::new();

    for line in content.lines() {
        let line_start = current_pos;
        let line_end = current_pos + line.len();

        // Check for code block fence
        if config.preserve_code_blocks && line.trim_start().starts_with("```") {
            if in_code_block {
                // End of code block
                code_block_content.push_str(line);
                code_block_content.push('\n');

                segments.push(TextSegment {
                    content: code_block_content.clone(),
                    char_start: code_block_start,
                    char_end: line_end + 1,
                    segment_type: SegmentType::CodeBlock,
                });

                code_block_content.clear();
                in_code_block = false;
            } else {
                // Start of code block
                in_code_block = true;
                code_block_start = line_start;
                code_block_content.push_str(line);
                code_block_content.push('\n');
            }
        } else if in_code_block {
            // Inside code block
            code_block_content.push_str(line);
            code_block_content.push('\n');
        } else if config.include_headings && line.trim_start().starts_with('#') {
            // Heading
            segments.push(TextSegment {
                content: line.to_string(),
                char_start: line_start,
                char_end: line_end,
                segment_type: SegmentType::Heading,
            });
        } else {
            // Regular text - accumulate with previous text segment if possible
            if let Some(last) = segments.last_mut() {
                if last.segment_type == SegmentType::Text && last.char_end == current_pos {
                    last.content.push('\n');
                    last.content.push_str(line);
                    last.char_end = line_end;
                } else {
                    segments.push(TextSegment {
                        content: line.to_string(),
                        char_start: line_start,
                        char_end: line_end,
                        segment_type: SegmentType::Text,
                    });
                }
            } else {
                segments.push(TextSegment {
                    content: line.to_string(),
                    char_start: line_start,
                    char_end: line_end,
                    segment_type: SegmentType::Text,
                });
            }
        }

        current_pos = line_end + 1; // +1 for newline
    }

    // Handle unclosed code block
    if in_code_block {
        segments.push(TextSegment {
            content: code_block_content,
            char_start: code_block_start,
            char_end: current_pos,
            segment_type: SegmentType::CodeBlock,
        });
    }

    segments
}

// ============================================================================
// Chunking Logic
// ============================================================================

fn chunk_segments(segments: &[TextSegment], config: &ChunkConfig) -> Result<Vec<ChunkData>> {
    let mut chunks = Vec::new();
    let mut current_content = String::new();
    let mut current_start = 0;
    let mut current_word_count = 0;
    let mut current_heading: Option<String> = None;
    let mut chunk_index = 0;

    // Helper to flush current chunk
    let flush_chunk = |content: &mut String,
                       start: usize,
                       end: usize,
                       word_count: usize,
                       heading: &Option<String>,
                       index: &mut usize,
                       chunks: &mut Vec<ChunkData>,
                       min_size: usize| {
        if word_count > 0 {
            // Create chunk even if below min_size for very short documents
            // But only if we have actual content
            let should_create = word_count >= min_size || chunks.is_empty();

            if should_create {
                chunks.push(ChunkData::new(
                    content.trim().to_string(),
                    start,
                    end,
                    word_count,
                    heading.clone(),
                    *index,
                ));
                *index += 1;
            }
        }
    };

    for segment in segments {
        match segment.segment_type {
            SegmentType::Heading => {
                // Save heading as context
                current_heading = Some(segment.content.trim().to_string());

                // Add heading to current chunk if include_headings is true
                if config.include_headings {
                    if !current_content.is_empty() {
                        current_content.push('\n');
                    }
                    current_content.push_str(&segment.content);
                    current_word_count += count_words(&segment.content);
                }
            }

            SegmentType::CodeBlock => {
                let code_words = count_words(&segment.content);

                // If current chunk + code block would exceed max, flush first
                if current_word_count > 0 && current_word_count + code_words > config.max_chunk_size
                {
                    flush_chunk(
                        &mut current_content,
                        current_start,
                        segment.char_start,
                        current_word_count,
                        &current_heading,
                        &mut chunk_index,
                        &mut chunks,
                        config.min_chunk_size,
                    );

                    // Start new chunk with overlap
                    let overlap = get_overlap_content(&current_content, config.overlap_words);
                    current_content = overlap;
                    current_start = segment.char_start;
                    current_word_count = count_words(&current_content);
                }

                // Add code block to current chunk
                if !current_content.is_empty() {
                    current_content.push('\n');
                }
                current_content.push_str(&segment.content);
                current_word_count += code_words;

                // If code block itself exceeds max, flush immediately
                if current_word_count > config.max_chunk_size {
                    flush_chunk(
                        &mut current_content,
                        current_start,
                        segment.char_end,
                        current_word_count,
                        &current_heading,
                        &mut chunk_index,
                        &mut chunks,
                        0, // Allow oversized code blocks
                    );

                    current_content.clear();
                    current_word_count = 0;
                    current_start = segment.char_end;
                }
            }

            SegmentType::Text => {
                // Process text paragraphs
                let paragraphs = split_into_paragraphs(&segment.content);
                let mut para_offset = segment.char_start;

                for paragraph in paragraphs {
                    if paragraph.trim().is_empty() {
                        para_offset += paragraph.len() + 2;
                        continue;
                    }

                    let para_words = count_words(&paragraph);

                    // If this paragraph alone exceeds max_chunk_size, split it by words
                    if para_words > config.max_chunk_size {
                        let words: Vec<&str> = paragraph.split_whitespace().collect();
                        let mut word_idx = 0;

                        while word_idx < words.len() {
                            // Flush current chunk if it has content
                            if current_word_count >= config.min_chunk_size {
                                flush_chunk(
                                    &mut current_content,
                                    current_start,
                                    para_offset,
                                    current_word_count,
                                    &current_heading,
                                    &mut chunk_index,
                                    &mut chunks,
                                    config.min_chunk_size,
                                );

                                let overlap =
                                    get_overlap_content(&current_content, config.overlap_words);
                                current_content = overlap;
                                current_start = para_offset;
                                current_word_count = count_words(&current_content);
                            }

                            // Add words up to target_words
                            let words_to_add =
                                config.target_words.saturating_sub(current_word_count);
                            let end_idx = (word_idx + words_to_add).min(words.len());

                            if !current_content.is_empty() && !current_content.ends_with('\n') {
                                current_content.push(' ');
                            }
                            current_content.push_str(&words[word_idx..end_idx].join(" "));
                            current_word_count += end_idx - word_idx;
                            word_idx = end_idx;
                        }

                        para_offset += paragraph.len() + 2;
                        continue;
                    }

                    // Check if we need to flush before adding this paragraph
                    if current_word_count > 0 {
                        let would_exceed_target =
                            current_word_count + para_words > config.target_words;
                        let would_exceed_max =
                            current_word_count + para_words > config.max_chunk_size;

                        if would_exceed_target || would_exceed_max {
                            flush_chunk(
                                &mut current_content,
                                current_start,
                                para_offset,
                                current_word_count,
                                &current_heading,
                                &mut chunk_index,
                                &mut chunks,
                                config.min_chunk_size,
                            );

                            // Start new chunk with overlap
                            let overlap =
                                get_overlap_content(&current_content, config.overlap_words);
                            current_content = overlap;
                            current_start = para_offset;
                            current_word_count = count_words(&current_content);
                        }
                    }

                    // Add paragraph to current chunk
                    if !current_content.is_empty() && !current_content.ends_with('\n') {
                        current_content.push_str("\n\n");
                    }
                    current_content.push_str(&paragraph);
                    current_word_count += para_words;

                    para_offset += paragraph.len() + 2; // +2 for \n\n separator
                }
            }
        }
    }

    // ALWAYS flush remaining content if we have any
    // This ensures short documents still produce a chunk
    if !current_content.trim().is_empty() {
        chunks.push(ChunkData::new(
            current_content.trim().to_string(),
            current_start,
            current_start + current_content.len(),
            current_word_count,
            current_heading,
            chunk_index,
        ));
    }

    Ok(chunks)
}

// ============================================================================
// Helper Functions
// ============================================================================

fn count_words(text: &str) -> usize {
    text.split_whitespace().count()
}

fn split_into_paragraphs(text: &str) -> Vec<String> {
    text.split("\n\n")
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

fn get_overlap_content(content: &str, overlap_words: usize) -> String {
    let words: Vec<&str> = content.split_whitespace().collect();
    if words.len() <= overlap_words {
        content.to_string()
    } else {
        words[words.len() - overlap_words..].join(" ")
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_chunk_config_default() {
        let config = ChunkConfig::default();
        assert_eq!(config.target_words, 512);
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_chunk_config_validation() {
        let mut config = ChunkConfig::default();
        config.overlap_words = config.target_words; // Invalid
        assert!(config.validate().is_err());

        config.overlap_words = 50;
        config.max_chunk_size = config.target_words / 2; // Invalid
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_count_words() {
        assert_eq!(count_words("hello world"), 2);
        assert_eq!(count_words("  spaces   everywhere  "), 2);
        assert_eq!(count_words(""), 0);
    }

    #[test]
    fn test_split_paragraphs() {
        let text = "Para 1\n\nPara 2\n\nPara 3";
        let paras = split_into_paragraphs(text);
        assert_eq!(paras.len(), 3);
    }

    #[test]
    fn test_get_overlap_content() {
        let content = "word1 word2 word3 word4 word5";
        let overlap = get_overlap_content(content, 2);
        assert_eq!(overlap, "word4 word5");
    }

    #[test]
    fn test_chunk_empty_document() {
        let config = ChunkConfig::default();
        let chunks = chunk_document("", &config).unwrap();
        assert_eq!(chunks.len(), 0);
    }

    #[test]
    fn test_chunk_short_document() {
        let config = ChunkConfig::default();
        let content = "This is a short document with only a few words.";
        let chunks = chunk_document(content, &config).unwrap();

        // Should create one chunk even though it's below min_chunk_size
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].word_count, 10);
        assert_eq!(chunks[0].index, 0);
    }

    #[test]
    fn test_chunk_with_code_blocks() {
        let config = ChunkConfig::default();
        let content = r#"# Example

Some text before code.

```rust
fn main() {
    println!("Hello");
}
```

Some text after code."#;

        let chunks = chunk_document(content, &config).unwrap();

        // Should preserve code block
        assert!(!chunks.is_empty());
        let full_content = chunks
            .iter()
            .map(|c| c.content.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(full_content.contains("```rust"));
        assert!(full_content.contains("fn main()"));
    }

    #[test]
    fn test_chunk_with_headings() {
        let config = ChunkConfig::default();
        let content = r#"# Main Heading

Content under main heading.

## Subheading

Content under subheading."#;

        let chunks = chunk_document(content, &config).unwrap();

        assert!(!chunks.is_empty());
        // First chunk should have heading context
        assert!(chunks[0].heading.is_some());
    }

    #[test]
    fn test_chunk_long_document() {
        let config = ChunkConfig {
            target_words: 50,
            overlap_words: 10,
            min_chunk_size: 10,
            max_chunk_size: 80,
            markdown_aware: false,
            preserve_code_blocks: false,
            include_headings: false,
        };

        // Create a document with 200 words (4x target)
        let words: Vec<String> = (0..200).map(|i| format!("word{}", i)).collect();
        let content = words.join(" ");

        let chunks = chunk_document(&content, &config).unwrap();

        // Should create multiple chunks
        assert!(
            chunks.len() >= 3,
            "Expected at least 3 chunks, got {}",
            chunks.len()
        );

        // Each chunk should respect size constraints
        for (i, chunk) in chunks.iter().enumerate() {
            assert!(
                chunk.word_count <= config.max_chunk_size,
                "Chunk {} has {} words, exceeds max {}",
                i,
                chunk.word_count,
                config.max_chunk_size
            );
        }
    }

    #[test]
    fn test_chunk_overlap() {
        let config = ChunkConfig {
            target_words: 20,
            overlap_words: 5,
            min_chunk_size: 5,
            max_chunk_size: 30,
            markdown_aware: false,
            preserve_code_blocks: false,
            include_headings: false,
        };

        let content = (0..50)
            .map(|i| format!("word{}", i))
            .collect::<Vec<_>>()
            .join(" ");
        let chunks = chunk_document(&content, &config).unwrap();

        // Should have multiple chunks with overlap
        assert!(chunks.len() >= 2);

        // Verify chunks have sequential indices
        for (i, chunk) in chunks.iter().enumerate() {
            assert_eq!(chunk.index, i);
        }
    }

    #[test]
    fn test_very_short_document() {
        let config = ChunkConfig {
            target_words: 100,
            overlap_words: 20,
            min_chunk_size: 50,
            max_chunk_size: 150,
            markdown_aware: false,
            preserve_code_blocks: false,
            include_headings: false,
        };

        // Only 5 words - below min_chunk_size
        let content = "Just five words here now";
        let chunks = chunk_document(content, &config).unwrap();

        // Should still create one chunk
        assert_eq!(chunks.len(), 1, "Very short doc should produce 1 chunk");
        assert_eq!(chunks[0].word_count, 5);
    }
}
