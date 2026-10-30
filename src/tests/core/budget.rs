#[test]
fn the_limit_table_is_one_table() {
    use crate::agent::engine::model_context_limit;

    for family in ["claude-opus-4", "gemini-2.0-pro", "nemotron-4"] {
        assert_eq!(model_context_limit(family).tokens, 1_000_000, "{family}");
        assert!(!model_context_limit(family).assumed, "{family}");
    }
    for family in [
        "deepseek-chat",
        "glm-4",
        "gpt-4o",
        "gpt-4.1",
        "o3",
        "o4-mini",
    ] {
        assert_eq!(model_context_limit(family).tokens, 128_000, "{family}");
        assert!(!model_context_limit(family).assumed, "{family}");
    }
    let unknown = model_context_limit("vendor/mystery-1");
    assert_eq!(unknown.tokens, 64_000);
    assert!(unknown.assumed, "an unknown id is a conservative guess");
}

#[test]
fn the_estimate_counts_the_serialized_request() {
    use crate::agent::budget::estimate_tokens;
    use genai::chat::{ChatMessage, ChatRequest};

    let request = ChatRequest::default()
        .append_message(ChatMessage::system("s"))
        .append_message(ChatMessage::user("hello"));
    let expected = serde_json::to_string(&request)
        .unwrap()
        .chars()
        .count()
        .div_ceil(4);
    assert_eq!(estimate_tokens(&request), expected);

    let empty = ChatRequest::default();
    assert_eq!(
        estimate_tokens(&empty),
        serde_json::to_string(&empty)
            .unwrap()
            .chars()
            .count()
            .div_ceil(4)
    );
}

#[test]
fn multibyte_text_is_counted_by_chars_not_bytes() {
    use crate::agent::budget::estimate_tokens;
    use genai::chat::ChatRequest;

    let request = ChatRequest::from_user("日本語".repeat(100));
    let json = serde_json::to_string(&request).unwrap();
    assert_eq!(estimate_tokens(&request), json.chars().count().div_ceil(4));
    assert_ne!(
        estimate_tokens(&request),
        json.len().div_ceil(4),
        "bytes would over-count a CJK request by 3x"
    );
}

#[test]
fn the_budgets_are_eighty_and_sixty_percent() {
    use crate::agent::budget::{keep_recent_budget, request_budget};

    assert_eq!(request_budget(128_000), 102_400);
    assert_eq!(keep_recent_budget(128_000), 61_440);
    assert_eq!(request_budget(64_000), 51_200);
    assert_eq!(request_budget(1_000_000), 800_000);
}

fn turn(user: &str, assistant_chars: usize) -> Vec<genai::chat::ChatMessage> {
    vec![
        genai::chat::ChatMessage::user(user),
        genai::chat::ChatMessage::assistant("x".repeat(assistant_chars)),
    ]
}

#[test]
fn the_window_cuts_at_a_user_row_and_keeps_the_newest_turn() {
    use crate::agent::budget::window_start;

    // ~30 000 estimated tokens per turn: 120 000 chars / 4.
    let mut messages = Vec::new();
    for index in 0..4 {
        messages.extend(turn(&format!("question {index}"), 120_000));
    }

    let start = window_start(&messages, 80_000);
    assert_eq!(start, 4, "the newest two turns stay");
    assert_eq!(
        messages[start].role,
        genai::chat::ChatRole::User,
        "the window starts at a user row"
    );
    assert_eq!(messages.len() - start, 4);
}

#[test]
fn an_oversized_newest_turn_is_kept_whole() {
    use crate::agent::budget::window_start;

    let mut messages = turn("small", 100);
    messages.extend(turn("huge", 480_000));

    let start = window_start(&messages, 80_000);
    assert_eq!(start, 2, "the oversized newest turn is never cut");
    assert_eq!(
        messages[start].content.first_text(),
        Some("huge"),
        "its user row leads it"
    );
}

#[test]
fn a_tool_result_never_leads_the_window() {
    use crate::agent::budget::window_start;

    let mut messages = turn("old", 300_000);
    messages.push(genai::chat::ChatMessage::user("read it"));
    messages.push(genai::chat::ChatMessage::assistant(
        genai::chat::MessageContent::from_tool_calls(vec![
            serde_json::from_value(serde_json::json!({
                "call_id": "call_a",
                "fn_name": "read_file",
                "fn_arguments": { "path": "x" },
            }))
            .unwrap(),
        ]),
    ));
    messages.push(genai::chat::ChatMessage::from(
        genai::chat::ToolResponse::new("call_a", "body"),
    ));

    let start = window_start(&messages, 80_000);
    assert_eq!(
        messages[start].role,
        genai::chat::ChatRole::User,
        "the cut moves back to the turn boundary instead of splitting the pair"
    );
    assert!(start < 3, "the tool result is inside the window");
}

#[test]
fn a_short_or_headless_summary_is_rejected() {
    use crate::agent::budget::validate_summary;

    assert!(validate_summary("too short").is_err());
    let long = "x".repeat(200);
    assert!(validate_summary(&long).is_err(), "no headings");
    let good = format!(
        "# Goals\n{}\n# Decisions\nx\n# Files\nx\n# Pending\nx",
        "x".repeat(100)
    );
    assert!(validate_summary(&good).is_ok(), "{good}");
}

struct ScriptedSummarizer {
    calls: std::sync::atomic::AtomicUsize,
    response: Result<String, String>,
}

impl ScriptedSummarizer {
    fn ok() -> Self {
        Self {
            calls: std::sync::atomic::AtomicUsize::new(0),
            response: Ok(format!(
                "# Goals\n{}\n# Decisions\nkeep the plan\n# Files\nsrc/agent/budget.rs\n# Pending\nnone",
                "work on the compaction contract".repeat(5)
            )),
        }
    }

    fn failing() -> Self {
        Self {
            calls: std::sync::atomic::AtomicUsize::new(0),
            response: Err("provider unavailable".to_string()),
        }
    }

    fn calls(&self) -> usize {
        self.calls.load(std::sync::atomic::Ordering::SeqCst)
    }
}

#[async_trait::async_trait]
impl crate::tui::compact::Summarizer for ScriptedSummarizer {
    async fn summarize(&self, _prompt: String) -> Result<String, String> {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.response.clone()
    }
}

fn transcript_history(turns: usize) -> Vec<genai::chat::ChatMessage> {
    let mut messages = Vec::new();
    for index in 0..turns {
        messages.extend(turn(&format!("question {index}"), 1_000));
    }
    messages
}

#[tokio::test]
async fn compaction_chunks_at_twenty_four_thousand_characters_and_merges() {
    use crate::agent::budget::compact;

    let summarizer = ScriptedSummarizer::ok();
    let history = transcript_history(60);
    let result = compact(&history, 4_000, &summarizer).await.unwrap();
    assert_eq!(
        summarizer.calls(),
        3,
        "two chunks plus one merge call: {}",
        summarizer.calls()
    );
    assert!(!result.history.is_empty(), "the recent window is kept");
    assert!(result.before > result.after, "the summary is smaller");
    assert!(result.summary.starts_with("# Goals"));

    let summarizer = ScriptedSummarizer::ok();
    let history = transcript_history(12);
    let result = compact(&history, 1_000, &summarizer).await.unwrap();
    assert_eq!(
        summarizer.calls(),
        1,
        "a transcript under 24 000 chars is one call"
    );
    assert!(result.after > 0);
}

#[tokio::test]
async fn a_failing_summarizer_returns_the_reason() {
    use crate::agent::budget::compact;

    let summarizer = ScriptedSummarizer::failing();
    let error = compact(&transcript_history(60), 4_000, &summarizer)
        .await
        .unwrap_err();
    assert!(error.contains("provider unavailable"), "{error}");
}
