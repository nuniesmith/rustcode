//! Research Aggregator
//!
//! Synthesizes findings from multiple workers into a coherent report.

use super::{ResearchRequest, WorkerResult};
use crate::llm::GrokClient;
use anyhow::Result;
use serde::{Deserialize, Serialize};

// ============================================================================
// Aggregated Report
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResearchReport {
    pub research_id: String,
    pub topic: String,
    pub summary: String,
    pub sections: Vec<ReportSection>,
    pub key_findings: Vec<String>,
    pub recommendations: Vec<String>,
    pub confidence_score: i32,
    pub total_tokens: i64,
    pub worker_count: i32,
    pub successful_workers: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReportSection {
    pub title: String,
    pub content: String,
    pub sources: Vec<String>,
    pub confidence: i32,
}

// ============================================================================
// Aggregator
// ============================================================================

pub struct Aggregator {
    llm: GrokClient,
    max_tokens: usize,
}

impl Aggregator {
    pub fn new(llm: GrokClient) -> Self {
        Self {
            llm,
            max_tokens: 8192, // Larger for synthesis
        }
    }

    /// Aggregate worker results into a final report
    pub async fn aggregate(
        &self,
        request: &ResearchRequest,
        results: &[WorkerResult],
    ) -> Result<ResearchReport> {
        let successful: Vec<_> = results.iter().filter(|r| r.status == "completed").collect();

        if successful.is_empty() {
            return Err(anyhow::anyhow!("No successful worker results to aggregate"));
        }

        // Build sections from worker results
        let sections: Vec<ReportSection> = successful
            .iter()
            .map(|r| ReportSection {
                title: r.subtopic.clone(),
                content: r.findings.clone(),
                sources: r
                    .sources
                    .as_ref()
                    .and_then(|s| serde_json::from_str(s).ok())
                    .unwrap_or_default(),
                confidence: r.confidence,
            })
            .collect();

        // Use LLM to synthesize
        let (summary, key_findings, recommendations) = self.synthesize(request, &sections).await?;

        let total_tokens: i64 = results.iter().map(|r| r.tokens_used).sum();
        let avg_confidence =
            successful.iter().map(|r| r.confidence).sum::<i32>() / successful.len() as i32;

        Ok(ResearchReport {
            research_id: request.id.clone(),
            topic: request.topic.clone(),
            summary,
            sections,
            key_findings,
            recommendations,
            confidence_score: avg_confidence,
            total_tokens,
            worker_count: results.len() as i32,
            successful_workers: successful.len() as i32,
        })
    }

    /// Use LLM to synthesize findings
    async fn synthesize(
        &self,
        request: &ResearchRequest,
        sections: &[ReportSection],
    ) -> Result<(String, Vec<String>, Vec<String>)> {
        let sections_text: String = sections
            .iter()
            .map(|s| format!("## {}\n\n{}", s.title, s.content))
            .collect::<Vec<_>>()
            .join("\n\n---\n\n");

        let prompt = format!(
            r#"Synthesize these research findings into a coherent report.

Original Topic: {topic}
Research Type: {research_type}

WORKER FINDINGS:
{sections}

---

Provide your synthesis in this exact JSON format:
{{
    "summary": "A 2-3 paragraph executive summary of all findings",
    "key_findings": ["finding 1", "finding 2", "finding 3", "..."],
    "recommendations": ["recommendation 1", "recommendation 2", "..."]
}}

The summary should:
- Integrate insights from all sections
- Highlight the most important discoveries
- Be coherent and well-structured

Key findings should be specific, actionable insights.
Recommendations should be practical next steps based on the research."#,
            topic = request.topic,
            research_type = request.research_type,
            sections = sections_text,
        );

        let response = self.llm.generate(&prompt, self.max_tokens).await?;

        // Parse JSON response
        #[derive(Deserialize)]
        struct SynthesisResponse {
            summary: String,
            key_findings: Vec<String>,
            recommendations: Vec<String>,
        }

        let parsed: SynthesisResponse = serde_json::from_str(&response)
            .or_else(|_| {
                // Try to extract JSON
                let start = response.find('{').unwrap_or(0);
                let end = response.rfind('}').map(|i| i + 1).unwrap_or(response.len());
                serde_json::from_str(&response[start..end])
            })
            .unwrap_or_else(|_| SynthesisResponse {
                summary: response.clone(),
                key_findings: vec!["See full report".to_string()],
                recommendations: vec!["Review findings in detail".to_string()],
            });

        Ok((parsed.summary, parsed.key_findings, parsed.recommendations))
    }
}

// ============================================================================
// Report Formatting
// ============================================================================

impl ResearchReport {
    /// Format as markdown
    pub fn to_markdown(&self) -> String {
        let mut md = String::new();

        md.push_str(&format!("# Research Report: {}\n\n", self.topic));
        md.push_str(&format!("**Confidence:** {}/10 | ", self.confidence_score));
        md.push_str(&format!(
            "**Workers:** {}/{} succeeded | ",
            self.successful_workers, self.worker_count
        ));
        md.push_str(&format!("**Tokens:** {}\n\n", self.total_tokens));

        md.push_str("## Executive Summary\n\n");
        md.push_str(&self.summary);
        md.push_str("\n\n");

        md.push_str("## Key Findings\n\n");
        for finding in &self.key_findings {
            md.push_str(&format!("- {}\n", finding));
        }
        md.push('\n');

        md.push_str("## Recommendations\n\n");
        for rec in &self.recommendations {
            md.push_str(&format!("- {}\n", rec));
        }
        md.push('\n');

        md.push_str("## Detailed Sections\n\n");
        for section in &self.sections {
            md.push_str(&format!("### {}\n\n", section.title));
            md.push_str(&format!("*Confidence: {}/10*\n\n", section.confidence));
            md.push_str(&section.content);
            md.push_str("\n\n");
        }

        md
    }

    /// Format as JSON
    pub fn to_json(&self) -> Result<String> {
        Ok(serde_json::to_string_pretty(self)?)
    }

    /// Format for Zed IDE paste
    pub fn to_zed_format(&self) -> String {
        let mut output = String::new();

        output.push_str(&format!("=== Research: {} ===\n\n", self.topic));
        output.push_str(&format!(
            "Confidence: {}/10 | {} workers\n\n",
            self.confidence_score, self.successful_workers
        ));

        output.push_str("Summary:\n");
        output.push_str(&self.summary);
        output.push_str("\n\n");

        output.push_str("Key Findings:\n");
        for (i, finding) in self.key_findings.iter().enumerate() {
            output.push_str(&format!("{}. {}\n", i + 1, finding));
        }
        output.push('\n');

        output.push_str("Next Steps:\n");
        for rec in &self.recommendations {
            output.push_str(&format!("• {}\n", rec));
        }

        output
    }
}
