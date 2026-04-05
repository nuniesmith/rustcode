// TodoFile — parse, update, and write back `todo.md`
//
// Handles the full lifecycle of a `todo.md` file:
// - Parse sections, items, and status markers
// - Update checkbox states ([ ] / [x]) and emoji markers (✅ / ⚠️ / ❌)
// - Append new items
// - Serialize back to valid Markdown
//
// # Format understood
//
// ```text
// ## 🔴 High Priority
//
// ### Some Section
// - [ ] pending item
// - [x] ~~completed item~~ ✅ Done — note
// - [ ] blocked item ❌ reason
// - [ ] partial item ⚠️ partial note
// ```

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use crate::error::{AuditError, Result};

// ============================================================================
// Status Types
// ============================================================================

// Checkbox state of a TODO item
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CheckboxState {
    // `- [ ]` — unchecked / pending
    Unchecked,
    // `- [x]` — checked / done
    Checked,
}

impl fmt::Display for CheckboxState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CheckboxState::Unchecked => write!(f, "[ ]"),
            CheckboxState::Checked => write!(f, "[x]"),
        }
    }
}

// Emoji status marker on an item
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum StatusMarker {
    // No marker
    #[default]
    None,
    // ✅ completed successfully
    Done,
    // ⚠️ partial / needs attention
    Partial,
    // ❌ blocked / failed
    Blocked,
}

impl StatusMarker {
    // Return the emoji string for this marker (empty string for `None`)
    pub fn emoji(self) -> &'static str {
        match self {
            StatusMarker::None => "",
            StatusMarker::Done => "✅",
            StatusMarker::Partial => "⚠️",
            StatusMarker::Blocked => "❌",
        }
    }

    // Parse a status marker from a line of text
    fn from_line(line: &str) -> Self {
        if line.contains('✅') {
            StatusMarker::Done
        } else if line.contains('⚠') {
            StatusMarker::Partial
        } else if line.contains('❌') {
            StatusMarker::Blocked
        } else {
            StatusMarker::None
        }
    }
}

impl fmt::Display for StatusMarker {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.emoji())
    }
}

// ============================================================================
// Priority
// ============================================================================

// Section-level priority derived from the heading emoji
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Priority {
    High,
    #[default]
    Medium,
    Low,
    Notes,
}

impl Priority {
    fn from_heading(heading: &str) -> Self {
        if heading.contains('🔴') {
            Priority::High
        } else if heading.contains('🟡') {
            Priority::Medium
        } else if heading.contains('🟢') {
            Priority::Low
        } else if heading.contains('📋') {
            Priority::Notes
        } else {
            Priority::Medium
        }
    }

    pub fn emoji(self) -> &'static str {
        match self {
            Priority::High => "🔴",
            Priority::Medium => "🟡",
            Priority::Low => "🟢",
            Priority::Notes => "📋",
        }
    }
}

// ============================================================================
// Core Data Structures
// ============================================================================

// A single item inside a todo section
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TodoItem {
    // Stable ID derived from section + original text hash (hex8)
    pub id: String,
    // Checkbox state
    pub checkbox: CheckboxState,
    // Emoji status marker
    pub marker: StatusMarker,
    // The raw item text (without leading `- [x] ` or `- [ ] `)
    pub text: String,
    // Optional note appended after the marker (e.g. "Done — some explanation")
    pub note: Option<String>,
    // Whether the text is struck-through (`~~…~~`)
    pub strikethrough: bool,
    // Bold title extracted from `**title**` at the start of text, if any
    pub title: Option<String>,
    // Timestamp of the last status change
    pub updated_at: Option<DateTime<Utc>>,
    // Indentation level (0 = top-level list item)
    pub indent: usize,
    // Raw original line (preserved for round-trip fidelity when unchanged)
    pub raw_line: String,
}

impl TodoItem {
    // Render the item back to its Markdown list line
    pub fn to_markdown(&self) -> String {
        let indent = " ".repeat(self.indent * 2);

        // Build the body text
        let body = if self.strikethrough {
            format!("~~{}~~", self.text)
        } else {
            self.text.clone()
        };

        let marker_str = self.marker.emoji();
        let note_str = match &self.note {
            Some(n) if !n.is_empty() => format!(" {}", n),
            _ => String::new(),
        };

        if marker_str.is_empty() {
            format!("{}- {} {}{}", indent, self.checkbox, body, note_str)
        } else {
            format!(
                "{}- {} {} {}{}",
                indent, self.checkbox, body, marker_str, note_str
            )
        }
    }

    // Mark this item as done
    pub fn mark_done(&mut self, note: impl Into<String>) {
        self.checkbox = CheckboxState::Checked;
        self.marker = StatusMarker::Done;
        self.strikethrough = true;
        let n = note.into();
        if !n.is_empty() {
            self.note = Some(n);
        }
        self.updated_at = Some(Utc::now());
    }

    // Mark this item as partial
    pub fn mark_partial(&mut self, note: impl Into<String>) {
        self.marker = StatusMarker::Partial;
        let n = note.into();
        if !n.is_empty() {
            self.note = Some(n);
        }
        self.updated_at = Some(Utc::now());
    }

    // Mark this item as blocked
    pub fn mark_blocked(&mut self, reason: impl Into<String>) {
        self.marker = StatusMarker::Blocked;
        let n = reason.into();
        if !n.is_empty() {
            self.note = Some(n);
        }
        self.updated_at = Some(Utc::now());
    }

    // Reset to unchecked / pending
    pub fn reset(&mut self) {
        self.checkbox = CheckboxState::Unchecked;
        self.marker = StatusMarker::None;
        self.strikethrough = false;
        self.note = None;
        self.updated_at = Some(Utc::now());
    }

    // Whether this item is considered complete
    pub fn is_done(&self) -> bool {
        self.checkbox == CheckboxState::Checked || self.marker == StatusMarker::Done
    }
}

// A subsection (`###` heading) within a priority section
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TodoSection {
    // The heading text (without `### `)
    pub heading: String,
    // Items under this section
    pub items: Vec<TodoItem>,
    // Raw lines that appear after the heading but before the first list item
    pub preamble: Vec<String>,
}

impl TodoSection {
    // Count pending (unchecked / non-done) items
    pub fn pending_count(&self) -> usize {
        self.items.iter().filter(|i| !i.is_done()).count()
    }

    // Count completed items
    pub fn done_count(&self) -> usize {
        self.items.iter().filter(|i| i.is_done()).count()
    }
}

// A top-level priority group (`##` heading)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PriorityBlock {
    // The full heading line text (without `## `)
    pub heading: String,
    // Derived priority
    pub priority: Priority,
    // Sections within this block
    pub sections: Vec<TodoSection>,
    // Lines between the heading and first `###` (block-level prose/notes)
    pub preamble: Vec<String>,
}

// ============================================================================
// TodoFile
// ============================================================================

// Parsed representation of a `todo.md` file
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TodoFile {
    // Path on disk
    pub path: PathBuf,
    // Lines before the first `##` heading (title, blockquote intro, etc.)
    pub header: Vec<String>,
    // Ordered list of priority blocks
    pub blocks: Vec<PriorityBlock>,
    // Lines after the last block (footer notes, etc.)
    pub footer: Vec<String>,
}

impl TodoFile {
    // -----------------------------------------------------------------------
    // Construction
    // -----------------------------------------------------------------------

    // Load and parse a `todo.md` from disk
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        let content = fs::read_to_string(&path).map_err(AuditError::Io)?;
        let mut file = Self::parse(&content);
        file.path = path;
        Ok(file)
    }

    // Parse a `todo.md` from a string
    pub fn parse(content: &str) -> Self {
        let lines: Vec<&str> = content.lines().collect();
        let mut file = TodoFile {
            path: PathBuf::new(),
            header: Vec::new(),
            blocks: Vec::new(),
            footer: Vec::new(),
        };

        let mut i = 0;

        // Collect header lines (before first `##`)
        while i < lines.len() {
            if lines[i].starts_with("## ") {
                break;
            }
            file.header.push(lines[i].to_string());
            i += 1;
        }

        // Parse priority blocks
        while i < lines.len() {
            if lines[i].starts_with("## ") {
                let heading = lines[i][3..].to_string();
                let priority = Priority::from_heading(lines[i]);
                let mut block = PriorityBlock {
                    heading,
                    priority,
                    sections: Vec::new(),
                    preamble: Vec::new(),
                };
                i += 1;

                // Collect preamble between `##` and first `###`
                while i < lines.len()
                    && !lines[i].starts_with("## ")
                    && !lines[i].starts_with("### ")
                {
                    block.preamble.push(lines[i].to_string());
                    i += 1;
                }

                // Parse subsections
                while i < lines.len() && !lines[i].starts_with("## ") {
                    if lines[i].starts_with("### ") {
                        let sec_heading = lines[i][4..].to_string();
                        let mut section = TodoSection {
                            heading: sec_heading,
                            items: Vec::new(),
                            preamble: Vec::new(),
                        };
                        i += 1;

                        // Collect lines until next `###`, `##`, or EOF
                        while i < lines.len()
                            && !lines[i].starts_with("## ")
                            && !lines[i].starts_with("### ")
                        {
                            let line = lines[i];
                            if let Some(item) = Self::parse_item(line) {
                                section.items.push(item);
                            } else {
                                section.preamble.push(line.to_string());
                            }
                            i += 1;
                        }

                        block.sections.push(section);
                    } else {
                        // Lines at block level that aren't items or sub-headings
                        block.preamble.push(lines[i].to_string());
                        i += 1;
                    }
                }

                file.blocks.push(block);
            } else {
                file.footer.push(lines[i].to_string());
                i += 1;
            }
        }

        file
    }

    // -----------------------------------------------------------------------
    // Parsing helpers
    // -----------------------------------------------------------------------

    // Try to parse a single list-item line into a `TodoItem`
    fn parse_item(line: &str) -> Option<TodoItem> {
        // Detect indentation
        let trimmed = line.trim_start();
        let indent = (line.len() - trimmed.len()) / 2;

        // Must start with `- [` or `* [`
        if !(trimmed.starts_with("- [") || trimmed.starts_with("* [")) {
            return None;
        }

        let after_bullet = &trimmed[2..]; // `[x] rest…` or `[ ] rest…`
        if after_bullet.len() < 3 {
            return None;
        }

        let checkbox = match &after_bullet[..3] {
            "[x]" | "[X]" => CheckboxState::Checked,
            "[ ]" => CheckboxState::Unchecked,
            _ => return None,
        };

        // Everything after `[x] ` (or `[ ] `)
        let rest = after_bullet[3..].trim_start().to_string();
        let marker = StatusMarker::from_line(&rest);

        // Detect strikethrough `~~text~~`
        let (text_raw, strikethrough) = if rest.starts_with("~~") {
            // strip surrounding ~~
            let inner_end = rest.rfind("~~").unwrap_or(rest.len() - 2);
            let inner = if inner_end > 2 {
                rest[2..inner_end].to_string()
            } else {
                rest.trim_matches('~').to_string()
            };
            (inner, true)
        } else {
            (rest.clone(), false)
        };

        // Extract optional title from `**title** — …`
        let (title, body_text) = if let Some(stripped) = text_raw.strip_prefix("**") {
            if let Some(end) = stripped.find("**") {
                let t = stripped[..end].to_string();
                let after = stripped[end + 2..]
                    .trim_start_matches(" —")
                    .trim()
                    .to_string();
                (Some(t), after)
            } else {
                (None, text_raw.clone())
            }
        } else {
            (None, text_raw.clone())
        };

        // Split note after marker emoji
        let (text_clean, note) = Self::split_note(&body_text, marker);

        // Build a stable ID: 8-char hex hash of the original line
        let id = format!("{:08x}", crc32_simple(line.as_bytes()));

        Some(TodoItem {
            id,
            checkbox,
            marker,
            text: text_clean,
            note,
            strikethrough,
            title,
            updated_at: None,
            indent,
            raw_line: line.to_string(),
        })
    }

    // Split the note out of the text after a status emoji
    fn split_note(text: &str, marker: StatusMarker) -> (String, Option<String>) {
        let emoji = marker.emoji();
        if emoji.is_empty() {
            return (text.to_string(), None);
        }

        if let Some(pos) = text.find(emoji) {
            let before = text[..pos].trim_end().to_string();
            let after = text[pos + emoji.len()..].trim().to_string();
            let note = if after.is_empty() { None } else { Some(after) };
            (before, note)
        } else {
            (text.to_string(), None)
        }
    }

    // -----------------------------------------------------------------------
    // Querying
    // -----------------------------------------------------------------------

    // Iterate over every item across all blocks and sections
    pub fn all_items(&self) -> impl Iterator<Item = &TodoItem> {
        self.blocks
            .iter()
            .flat_map(|b| b.sections.iter())
            .flat_map(|s| s.items.iter())
    }

    // Iterate mutably over every item
    pub fn all_items_mut(&mut self) -> impl Iterator<Item = &mut TodoItem> {
        self.blocks
            .iter_mut()
            .flat_map(|b| b.sections.iter_mut())
            .flat_map(|s| s.items.iter_mut())
    }

    // Find an item by its stable ID
    pub fn find_by_id(&self, id: &str) -> Option<&TodoItem> {
        self.all_items().find(|i| i.id == id)
    }

    // Find an item mutably by its stable ID
    pub fn find_by_id_mut(&mut self, id: &str) -> Option<&mut TodoItem> {
        self.all_items_mut().find(|i| i.id == id)
    }

    // Find items whose text contains `substr` (case-insensitive)
    pub fn find_by_text(&self, substr: &str) -> Vec<&TodoItem> {
        let lower = substr.to_lowercase();
        self.all_items()
            .filter(|i| i.text.to_lowercase().contains(&lower))
            .collect()
    }

    // Total item counts
    pub fn counts(&self) -> TodoCounts {
        let mut counts = TodoCounts::default();
        for item in self.all_items() {
            counts.total += 1;
            match item.marker {
                StatusMarker::Done => counts.done += 1,
                StatusMarker::Partial => counts.partial += 1,
                StatusMarker::Blocked => counts.blocked += 1,
                StatusMarker::None => {
                    if item.checkbox == CheckboxState::Checked {
                        counts.done += 1;
                    } else {
                        counts.pending += 1;
                    }
                }
            }
        }
        counts
    }

    // -----------------------------------------------------------------------
    // Mutation
    // -----------------------------------------------------------------------

    // Mark an item done by ID
    pub fn mark_done(&mut self, id: &str, note: impl Into<String>) -> bool {
        if let Some(item) = self.find_by_id_mut(id) {
            item.mark_done(note);
            true
        } else {
            false
        }
    }

    // Mark an item partial by ID
    pub fn mark_partial(&mut self, id: &str, note: impl Into<String>) -> bool {
        if let Some(item) = self.find_by_id_mut(id) {
            item.mark_partial(note);
            true
        } else {
            false
        }
    }

    // Mark an item blocked by ID
    pub fn mark_blocked(&mut self, id: &str, reason: impl Into<String>) -> bool {
        if let Some(item) = self.find_by_id_mut(id) {
            item.mark_blocked(reason);
            true
        } else {
            false
        }
    }

    // Append a new item to a named section (creates section if needed)
    //
    // `block_heading` should match the text after `## ` (e.g. `"🔴 High Priority"`)
    // `section_heading` should match the text after `### ` (e.g. `"API & Data Layer"`)
    pub fn append_item(
        &mut self,
        block_heading: &str,
        section_heading: &str,
        text: impl Into<String>,
    ) {
        let text = text.into();
        // Find or create the block
        let block = if let Some(b) = self.blocks.iter_mut().find(|b| b.heading == block_heading) {
            b
        } else {
            self.blocks.push(PriorityBlock {
                heading: block_heading.to_string(),
                priority: Priority::from_heading(block_heading),
                sections: Vec::new(),
                preamble: Vec::new(),
            });
            self.blocks.last_mut().unwrap()
        };

        // Find or create the section
        let section = if let Some(s) = block
            .sections
            .iter_mut()
            .find(|s| s.heading == section_heading)
        {
            s
        } else {
            block.sections.push(TodoSection {
                heading: section_heading.to_string(),
                items: Vec::new(),
                preamble: Vec::new(),
            });
            block.sections.last_mut().unwrap()
        };

        let raw_line = format!("- [ ] {}", text);
        let id = format!("{:08x}", crc32_simple(raw_line.as_bytes()));

        section.items.push(TodoItem {
            id,
            checkbox: CheckboxState::Unchecked,
            marker: StatusMarker::None,
            text,
            note: None,
            strikethrough: false,
            title: None,
            updated_at: Some(Utc::now()),
            indent: 0,
            raw_line,
        });
    }

    // -----------------------------------------------------------------------
    // Serialisation
    // -----------------------------------------------------------------------

    // Render the entire file back to Markdown
    pub fn to_markdown(&self) -> String {
        let mut out = String::new();

        // Header
        for line in &self.header {
            out.push_str(line);
            out.push('\n');
        }

        for block in &self.blocks {
            out.push_str(&format!("## {}\n", block.heading));
            for line in &block.preamble {
                out.push_str(line);
                out.push('\n');
            }
            for section in &block.sections {
                out.push_str(&format!("### {}\n", section.heading));
                for line in &section.preamble {
                    out.push_str(line);
                    out.push('\n');
                }
                for item in &section.items {
                    out.push_str(&item.to_markdown());
                    out.push('\n');
                }
            }
        }

        // Footer
        for line in &self.footer {
            out.push_str(line);
            out.push('\n');
        }

        out
    }

    // Write the file back to disk (atomic: write to `.tmp` then rename)
    pub fn save(&self) -> Result<()> {
        let content = self.to_markdown();
        let tmp = self.path.with_extension("md.tmp");
        fs::write(&tmp, &content).map_err(AuditError::Io)?;
        fs::rename(&tmp, &self.path).map_err(AuditError::Io)?;
        Ok(())
    }

    // Save to an explicit path (useful for tests / alternate targets)
    pub fn save_to(&self, path: impl AsRef<Path>) -> Result<()> {
        let content = self.to_markdown();
        let path = path.as_ref();
        let tmp = path.with_extension("md.tmp");
        fs::write(&tmp, &content).map_err(AuditError::Io)?;
        fs::rename(&tmp, path).map_err(AuditError::Io)?;
        Ok(())
    }
}

// ============================================================================
// Counts helper
// ============================================================================

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TodoCounts {
    pub total: usize,
    pub pending: usize,
    pub done: usize,
    pub partial: usize,
    pub blocked: usize,
}

// ============================================================================
// CRC32 helper (no extra dep — simple Castagnoli-ish impl for stable IDs)
// ============================================================================

fn crc32_simple(data: &[u8]) -> u32 {
    let mut crc: u32 = 0xFFFF_FFFF;
    for &byte in data {
        crc ^= (byte as u32) << 24;
        for _ in 0..8 {
            if crc & 0x8000_0000 != 0 {
                crc = (crc << 1) ^ 0x04C1_1DB7;
            } else {
                crc <<= 1;
            }
        }
    }
    !crc
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"# RustCode — TODO Backlog

> A living document.

---

## 🔴 High Priority

### API & Data Layer
- [ ] Fix admin module — accessing non-existent `ApiState` fields (`src/api/mod.rs`)
- [x] ~~Publish Docker image~~ ✅ Done — ci-cd.yml pushes to Docker Hub

### Search & RAG
- [ ] Integrate RAG context search with LanceDB vector search — currently returns empty results

---

## 🟡 Medium Priority

### CLI & Developer Experience
- [ ] Actually test the XAI API connection in `test-api` command

---

## 📋 Notes

- Redis is configured in `docker-compose.yml` for caching.
"#;

    #[test]
    fn test_parse_round_trip() {
        let file = TodoFile::parse(SAMPLE);
        assert_eq!(file.blocks.len(), 3);

        let high = &file.blocks[0];
        assert_eq!(high.priority, Priority::High);
        assert_eq!(high.sections.len(), 2);

        let api = &high.sections[0];
        assert_eq!(api.items.len(), 2);
        assert_eq!(api.items[0].checkbox, CheckboxState::Unchecked);
        assert_eq!(api.items[1].checkbox, CheckboxState::Checked);
        assert_eq!(api.items[1].marker, StatusMarker::Done);
        assert!(api.items[1].strikethrough);
    }

    #[test]
    fn test_mark_done() {
        let mut file = TodoFile::parse(SAMPLE);
        let id = file.all_items().next().unwrap().id.clone();
        let changed = file.mark_done(&id, "Fixed in PR #42");
        assert!(changed);
        let item = file.find_by_id(&id).unwrap();
        assert!(item.is_done());
        assert_eq!(item.marker, StatusMarker::Done);
        assert_eq!(item.note.as_deref(), Some("Fixed in PR #42"));
    }

    #[test]
    fn test_counts() {
        let file = TodoFile::parse(SAMPLE);
        let counts = file.counts();
        assert_eq!(counts.total, 4);
        assert_eq!(counts.done, 1);
        assert_eq!(counts.pending, 3);
    }

    #[test]
    fn test_append_item() {
        let mut file = TodoFile::parse(SAMPLE);
        file.append_item(
            "🔴 High Priority",
            "API & Data Layer",
            "New task added by test",
        );
        let items = file.find_by_text("New task added by test");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].checkbox, CheckboxState::Unchecked);
    }

    #[test]
    fn test_to_markdown_preserves_structure() {
        let file = TodoFile::parse(SAMPLE);
        let rendered = file.to_markdown();
        // Must still have all headings
        assert!(rendered.contains("## 🔴 High Priority"));
        assert!(rendered.contains("### API & Data Layer"));
        assert!(rendered.contains("## 🟡 Medium Priority"));
    }

    #[test]
    fn test_find_by_text() {
        let file = TodoFile::parse(SAMPLE);
        let results = file.find_by_text("LanceDB");
        assert_eq!(results.len(), 1);
    }
}
