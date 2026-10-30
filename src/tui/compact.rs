//! Session compaction: summarize the visible transcript and keep the summary
//! as context for later requests in the same TUI run.

use crate::agent::engine::{AgentConfig, AgentRuntime};
use crate::agent::security::WorkspaceSecurity;
use crate::tui::app::Segment;

pub const MIN_TRANSCRIPT_CHARS: usize = 4_000;
pub const MIN_TRANSCRIPT_LINES: usize = 40;
pub const CHUNK_CHARS: usize = 24_000;
/// The banner a compaction summary enters the request under.
pub const SESSION_CONTEXT_BANNER: &str = "# SESSION CONTEXT (compacted)";

const TRUNCATION_MARKER: &str = "\n...[truncated]...\n";

const COMPACTION_PROMPT: &str = "\
You are compacting an AI coding session so work can continue with less context.
Preserve, in this order:
1. The user's goals and constraints.
2. Decisions made and why.
3. Files and paths read, created, or modified.
4. Commands and tools run, with results that still matter.
5. Errors, failed attempts, and their fixes.
6. Pending tasks and open questions.

Do not invent facts. Do not include secrets or long code blocks.
Keep exact paths, identifiers, and command names.
Output Markdown with short headings and bullets, at most 800 words.";

#[derive(Debug, Clone)]
pub struct CompactResult {
    pub summary: String,
    pub before_chars: usize,
    pub after_chars: usize,
    pub before_lines: usize,
    pub after_lines: usize,
}

#[async_trait::async_trait]
pub trait Summarizer: Send + Sync {
    async fn summarize(&self, prompt: String) -> Result<String, String>;
}

pub struct ModelSummarizer {
    client: genai::Client,
    model: String,
    options: genai::chat::ChatOptions,
}

impl ModelSummarizer {
    pub fn from_config(config: &AgentConfig) -> Self {
        let runtime = AgentRuntime::new(config.clone(), WorkspaceSecurity::new("."));
        Self {
            client: runtime.client().clone(),
            model: config.model.clone(),
            options: runtime.chat_options(),
        }
    }
}

#[async_trait::async_trait]
impl Summarizer for ModelSummarizer {
    async fn summarize(&self, prompt: String) -> Result<String, String> {
        let request = genai::chat::ChatRequest::from_user(prompt);
        let response = self
            .client
            .exec_chat(&self.model, request, Some(&self.options))
            .await
            .map_err(|error| error.to_string())?;

        response
            .into_first_text()
            .map(|text| text.trim().to_string())
            .filter(|text| !text.is_empty())
            .ok_or_else(|| "compaction returned an empty summary".to_string())
    }
}

/// Plain text for the summarizer: text verbatim, reasoning clearly labelled.
pub fn transcript_from_segments(segments: &[Segment]) -> String {
    segments
        .iter()
        .map(|segment| match segment {
            Segment::Text(text) => text.clone(),
            Segment::Notice(text) => text.clone(),
            Segment::Thinking(text) => format!("[reasoning]\n{text}"),
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// A session needs both fewer than [`MIN_TRANSCRIPT_LINES`] lines and fewer
/// than [`MIN_TRANSCRIPT_CHARS`] characters to count as too short.
pub fn is_too_short(transcript: &str) -> bool {
    transcript.lines().count() < MIN_TRANSCRIPT_LINES
        && transcript.chars().count() < MIN_TRANSCRIPT_CHARS
}

pub fn build_summary_prompt(transcript: &str, focus: Option<&str>) -> String {
    let mut prompt = String::from(COMPACTION_PROMPT);

    if let Some(focus) = focus.map(str::trim).filter(|focus| !focus.is_empty()) {
        prompt.push_str("\n\nThe user wants this compaction to emphasize: ");
        prompt.push_str(focus);
    }

    prompt.push_str("\n\nSession transcript:\n");
    prompt.push_str(transcript);
    prompt
}

/// Split on blank lines and pack blocks up to `max_chars`. A single block
/// larger than the limit keeps its start and end around a truncation marker,
/// so the returned pieces never exceed the limit.
pub fn chunk_transcript(transcript: &str, max_chars: usize) -> Vec<String> {
    if max_chars == 0 {
        return Vec::new();
    }

    let mut chunks = Vec::new();
    let mut current = String::new();

    for block in transcript.split("\n\n") {
        let separator = if current.is_empty() { 0 } else { 2 };
        if current.chars().count() + separator + block.chars().count() <= max_chars {
            if separator > 0 {
                current.push_str("\n\n");
            }
            current.push_str(block);
            continue;
        }

        if !current.is_empty() {
            chunks.push(std::mem::take(&mut current));
        }

        if block.chars().count() <= max_chars {
            current.push_str(block);
            continue;
        }

        let marker_chars = TRUNCATION_MARKER.chars().count();
        if marker_chars >= max_chars {
            chunks.push(TRUNCATION_MARKER.chars().take(max_chars).collect());
            continue;
        }

        let keep = max_chars - marker_chars;
        let head = keep.div_ceil(2);
        let tail = keep - head;
        let characters: Vec<char> = block.chars().collect();

        let mut piece: String = characters.iter().take(head).collect();
        piece.push_str(TRUNCATION_MARKER);
        if tail > 0 {
            piece.extend(characters.iter().skip(characters.len() - tail));
        }
        chunks.push(piece);
    }

    if !current.is_empty() {
        chunks.push(current);
    }

    chunks
}

pub async fn run_compaction(
    transcript: &str,
    focus: Option<&str>,
    summarizer: &dyn Summarizer,
) -> Result<CompactResult, String> {
    if is_too_short(transcript) {
        return Err(format!(
            "Transcript is too short to compact ({} lines).",
            transcript.lines().count()
        ));
    }

    let before_chars = transcript.chars().count();
    let before_lines = transcript.lines().count();
    let chunks = chunk_transcript(transcript, CHUNK_CHARS);

    let summary = if chunks.len() <= 1 {
        summarize(summarizer, build_summary_prompt(transcript, focus)).await?
    } else {
        let mut summaries = Vec::with_capacity(chunks.len());
        for chunk in &chunks {
            summaries.push(summarize(summarizer, build_summary_prompt(chunk, focus)).await?);
        }
        summarize(
            summarizer,
            build_summary_prompt(&summaries.join("\n\n"), focus),
        )
        .await?
    };

    let summary = summary.trim().to_string();
    if summary.is_empty() {
        return Err("compaction returned an empty summary".to_string());
    }
    // One gate: a summary that cannot carry the plan is rejected here too.
    crate::agent::budget::validate_summary(&summary)?;

    Ok(CompactResult {
        after_chars: summary.chars().count(),
        after_lines: summary.lines().count(),
        summary,
        before_chars,
        before_lines,
    })
}

async fn summarize(summarizer: &dyn Summarizer, prompt: String) -> Result<String, String> {
    let summary = summarizer.summarize(prompt).await?;
    let summary = summary.trim().to_string();
    if summary.is_empty() {
        Err("compaction returned an empty summary".to_string())
    } else {
        Ok(summary)
    }
}

pub fn build_agent_task(prompt: &str, session_context: Option<&str>) -> String {
    match session_context {
        Some(context) => {
            format!("{SESSION_CONTEXT_BANNER}\n{context}\n\n# CURRENT REQUEST\n{prompt}")
        }
        None => prompt.to_string(),
    }
}

#[cfg(test)]
#[path = "../tests/tui/compact.rs"]
mod tests;
