use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

struct FakeSummarizer {
    calls: AtomicUsize,
    response: String,
}

#[async_trait::async_trait]
impl Summarizer for FakeSummarizer {
    async fn summarize(&self, _prompt: String) -> Result<String, String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(self.response.clone())
    }
}

#[tokio::test]
async fn short_transcript_is_rejected_without_calling_the_model() {
    let summarizer = FakeSummarizer {
        calls: AtomicUsize::new(0),
        response: "summary".into(),
    };
    let err = run_compaction("one line", None, &summarizer)
        .await
        .unwrap_err();
    assert!(err.contains("too short"));
    assert_eq!(summarizer.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn large_transcript_is_chunked_and_merged() {
    let summarizer = FakeSummarizer {
        calls: AtomicUsize::new(0),
        response: "summary".into(),
    };
    let transcript = "line\n\n".repeat(10_000);
    let result = run_compaction(&transcript, Some("files"), &summarizer)
        .await
        .unwrap();
    assert_eq!(result.summary, "summary");
    assert!(
        summarizer.calls.load(Ordering::SeqCst) >= 3,
        "chunk calls plus merge call"
    );
}

#[test]
fn chunks_never_exceed_the_limit_and_mark_truncation() {
    let long = "x".repeat(100);
    let chunks = chunk_transcript(&long, 30);
    assert!(chunks.iter().all(|chunk| chunk.chars().count() <= 30));
    assert!(chunks.iter().any(|chunk| chunk.contains("[truncated]")));
}

#[test]
fn a_limit_smaller_than_the_marker_still_returns_a_bounded_chunk() {
    let chunks = chunk_transcript(&"x".repeat(100), 5);
    assert_eq!(chunks.len(), 1);
    assert!(chunks[0].chars().count() <= 5);
}

#[tokio::test]
async fn empty_summary_is_rejected() {
    let summarizer = FakeSummarizer {
        calls: AtomicUsize::new(0),
        response: "   ".into(),
    };
    let transcript = "line\n\n".repeat(2_000);
    let err = run_compaction(&transcript, None, &summarizer)
        .await
        .unwrap_err();
    assert!(err.contains("empty"));
}

#[test]
fn agent_task_includes_only_a_present_context() {
    assert_eq!(build_agent_task("do it", None), "do it");
    let task = build_agent_task("do it", Some("prior work"));
    assert!(task.contains("# SESSION CONTEXT (compacted)"));
    assert!(task.contains("prior work"));
    assert!(task.contains("# CURRENT REQUEST\ndo it"));
}
