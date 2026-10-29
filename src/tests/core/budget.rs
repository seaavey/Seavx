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
