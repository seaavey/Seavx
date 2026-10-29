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
