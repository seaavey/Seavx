use super::*;

fn listed() -> Vec<String> {
    vec!["model-a".to_string(), "model-b".to_string()]
}

#[test]
fn a_listed_name_is_writable() {
    assert_eq!(
        model_gate(false, "gateway.test", &listed(), "model-a"),
        ModelGate::Writable
    );
}

#[test]
fn an_unlisted_name_is_refused_when_piped() {
    match model_gate(false, "gateway.test", &listed(), "ghost-model") {
        ModelGate::Refused(reason) => {
            assert!(reason.contains("ghost-model"), "{reason}");
            assert!(
                reason.contains("gateway.test"),
                "the host is named: {reason}"
            );
            assert!(reason.contains('2'), "the list size is named: {reason}");
        }
        other => panic!("a pipeline cannot say yes: {other:?}"),
    }
}

#[test]
fn an_unlisted_name_asks_when_interactive() {
    match model_gate(true, "gateway.test", &listed(), "ghost-model") {
        ModelGate::Ask(question) => {
            assert!(question.contains("use it anyway?"), "{question}");
            assert!(question.contains("gateway.test"), "{question}");
        }
        other => panic!("a terminal is asked, not refused: {other:?}"),
    }
}

#[test]
fn model_without_an_endpoint_prints_not_set() {
    let mut config = AgentConfig {
        base_url: None,
        ..AgentConfig::default()
    };
    assert_eq!(
        require_endpoint(&config).unwrap_err(),
        "not set (run seavx setup)",
        "there is no built-in endpoint to fall back on"
    );

    config.base_url = Some("https://gateway.test/v1/".to_string());
    assert_eq!(
        require_endpoint(&config).unwrap(),
        "https://gateway.test/v1/"
    );
}

#[test]
fn nothing_listed_cannot_refuse_anything() {
    assert_eq!(
        model_gate(false, "gateway.test", &[], "ghost-model"),
        ModelGate::Writable,
        "a list that could not be fetched proves nothing about the name"
    );
}
