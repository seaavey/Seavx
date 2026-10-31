use crate::agent::questions::{QuestionAnswer, parse_request};
use crate::session::QuestionKind;
use serde_json::json;

#[test]
fn a_text_question_parses_without_options() {
    let request = parse_request("call-1", &json!({ "question": "What should the goal be?" }))
        .expect("a text question");
    assert_eq!(request.kind, QuestionKind::Text);
    assert!(request.options.is_empty());
    assert_eq!(request.call_id, "call-1");
}

#[test]
fn a_choice_question_parses_its_options() {
    let request = parse_request(
        "call-2",
        &json!({ "question": "Which one?", "kind": "single", "options": ["a", "b"] }),
    )
    .expect("a single choice");
    assert_eq!(request.kind, QuestionKind::Single);
    assert_eq!(request.options, vec!["a".to_string(), "b".to_string()]);
}

#[test]
fn a_bad_request_is_refused_instead_of_guessed() {
    for args in [
        json!({}),
        json!({ "question": "   " }),
        json!({ "question": "pick", "kind": "single" }),
        json!({ "question": "pick", "kind": "single", "options": [] }),
        json!({ "question": "pick", "kind": "ranked", "options": ["a"] }),
    ] {
        assert!(parse_request("call", &args).is_err(), "{args}");
    }
}

#[test]
fn every_non_answer_tells_the_model_not_to_assume() {
    for answer in [
        QuestionAnswer::Skipped,
        QuestionAnswer::Cancelled,
        QuestionAnswer::Unavailable,
        QuestionAnswer::Answered(String::new()),
    ] {
        let text = answer.tool_result().to_lowercase();
        assert!(
            text.contains("assum"),
            "every non-answer must warn against assuming a value: {answer:?}: {text}"
        );
    }
    assert_eq!(
        QuestionAnswer::Answered("blue".to_string()).tool_result(),
        "The user answered: blue"
    );
}
