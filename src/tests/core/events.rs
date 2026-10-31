use super::*;

#[test]
fn bounded_keeps_short_text_and_caps_long_text_on_a_char_boundary() {
    assert_eq!(bounded("short", SUMMARY_LIMIT), "short");
    let long = "é".repeat(900);
    let capped = bounded(&long, SUMMARY_LIMIT);
    assert_eq!(capped.chars().count(), SUMMARY_LIMIT + 1); // 512 + the ellipsis
    assert!(capped.ends_with('…'));
    assert!(capped.starts_with('é'), "no partial character at the cut");
}

#[test]
fn statuses_and_failures_serialize_to_the_ledger_spelling() {
    assert_eq!(ToolStatus::Queued.as_str(), "queued");
    assert_eq!(ToolStatus::Running.as_str(), "running");
    assert_eq!(ToolStatus::Completed.as_str(), "completed");
    assert_eq!(ToolStatus::Failed.as_str(), "failed");
    assert_eq!(ToolStatus::Cancelled.as_str(), "cancelled");
    assert_eq!(FailureKind::PermissionDenied.as_str(), "permission_denied");
    assert_eq!(
        FailureKind::CleanupUnconfirmed.as_str(),
        "cleanup_unconfirmed"
    );
}

#[test]
fn a_tool_error_naming_a_timeout_is_classified_as_timeout() {
    assert_eq!(
        FailureKind::for_tool_error("command timed out after 30s"),
        FailureKind::Timeout
    );
    assert_eq!(
        FailureKind::for_tool_error("no such file or directory"),
        FailureKind::Tool
    );
}
