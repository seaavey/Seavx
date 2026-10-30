use crate::agent::review::{Confidence, Severity, parse_report, render_report};

const VALID: &str = r#"Here is the review:

```json
{
  "target": "working-tree",
  "findings": [
    {
      "severity": "high",
      "title": "The path is joined without validation",
      "impact": "A crafted argument reads outside the workspace.",
      "remediation": "Validate with WorkspaceSecurity before reading.",
      "path": "src/agent/tools.rs",
      "line_start": 250,
      "line_end": 252,
      "confidence": "confirmed"
    },
    {
      "severity": "low",
      "title": "The error message drops the command",
      "impact": "Harder to debug a refusal.",
      "remediation": "Include the command in the message.",
      "path": "src/agent/shell.rs",
      "line_start": 40,
      "confidence": "hypothesis",
      "outside_diff": true
    }
  ],
  "limitations": ["Only the working tree was reviewed."]
}
```
"#;

#[test]
fn a_valid_report_keeps_every_field() {
    let report = parse_report(VALID).expect("the fenced report parses");
    assert_eq!(report.target, "working-tree");
    assert_eq!(report.findings.len(), 2);

    let first = &report.findings[0];
    assert_eq!(first.severity, Severity::High);
    assert_eq!(first.confidence, Confidence::Confirmed);
    assert_eq!(first.path, "src/agent/tools.rs");
    assert_eq!((first.line_start, first.line_end), (Some(250), Some(252)));
    assert!(!first.outside_diff);

    let second = &report.findings[1];
    assert_eq!(second.confidence, Confidence::Hypothesis);
    assert!(second.outside_diff);
}

#[test]
fn a_report_without_fences_still_parses() {
    let bare = r#"{"target":"staged","findings":[],"limitations":[]}"#;
    let report = parse_report(bare).expect("a bare object parses");
    assert!(report.findings.is_empty());
}

#[test]
fn a_malformed_or_incomplete_report_is_an_error_not_a_clean_review() {
    for bad in [
        "I found nothing wrong.",
        r#"{"target":"staged","findings":[{"severity":"high"}]}"#,
        r#"{"target":"staged","findings":[{"severity":"urgent","title":"t","impact":"i","remediation":"r","path":"p","confidence":"confirmed"}]}"#,
        r#"{"target":"staged","findings":[{"severity":"high","title":"  ","impact":"i","remediation":"r","path":"p","confidence":"confirmed"}]}"#,
        r#"{"target":"staged","findings":[{"severity":"high","title":"t","impact":"i","remediation":"r","path":"p","line_start":9,"line_end":3,"confidence":"confirmed"}]}"#,
    ] {
        assert!(
            parse_report(bad).is_err(),
            "{bad} must not pass as a report"
        );
    }
}

#[test]
fn the_rendering_names_the_target_and_never_claims_a_test() {
    let report = parse_report(VALID).unwrap();
    let text = render_report(&report);

    assert!(text.contains("working-tree"), "{text}");
    assert!(text.contains("[high]"), "{text}");
    assert!(text.contains("src/agent/tools.rs:250-252"), "{text}");
    assert!(text.contains("(confirmed)"), "{text}");
    assert!(text.contains("outside the diff"), "{text}");
    assert!(text.contains("No tests were run"), "{text}");
}

#[test]
fn an_empty_review_states_its_scope_instead_of_nothing() {
    let report = parse_report(r#"{"target":"commit abc123","findings":[]}"#).unwrap();
    let text = render_report(&report);
    assert!(text.contains("commit abc123"), "{text}");
    assert!(text.contains("No findings"), "{text}");
    assert!(text.contains("No tests were run"), "{text}");
    assert!(!text.contains("withheld"), "{text}");
}
