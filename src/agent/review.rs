//! Structured review findings.
//!
//! The model answers a review turn with one JSON block; this module is the only
//! place that decides whether that answer is a report at all. A payload that
//! does not parse is a review error, never a clean review with no findings.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Critical,
    High,
    Medium,
    Low,
    Info,
}

impl Severity {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Critical => "critical",
            Self::High => "high",
            Self::Medium => "medium",
            Self::Low => "low",
            Self::Info => "info",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Confidence {
    /// The code shows it.
    Confirmed,
    /// It needs a run or a reading the reviewer did not do.
    Hypothesis,
}

impl Confidence {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Confirmed => "confirmed",
            Self::Hypothesis => "hypothesis",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Finding {
    pub severity: Severity,
    pub title: String,
    pub impact: String,
    pub remediation: String,
    pub path: String,
    #[serde(default)]
    pub line_start: Option<u32>,
    #[serde(default)]
    pub line_end: Option<u32>,
    pub confidence: Confidence,
    /// True when the finding sits outside the reviewed diff.
    #[serde(default)]
    pub outside_diff: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewReport {
    pub target: String,
    #[serde(default)]
    pub findings: Vec<Finding>,
    #[serde(default)]
    pub limitations: Vec<String>,
}

/// Extract the report from a model answer: the first fenced `json` block, or the
/// whole answer when it is already the object.
pub fn parse_report(answer: &str) -> Result<ReviewReport, String> {
    let text = fence_body(answer).unwrap_or(answer).trim();
    let report: ReviewReport = serde_json::from_str(text)
        .map_err(|error| format!("the review response was not a valid report: {error}"))?;
    for finding in &report.findings {
        for (field, value) in [
            ("title", &finding.title),
            ("impact", &finding.impact),
            ("remediation", &finding.remediation),
            ("path", &finding.path),
        ] {
            if value.trim().is_empty() {
                return Err(format!("a finding is missing its {field}"));
            }
        }
        if let (Some(start), Some(end)) = (finding.line_start, finding.line_end)
            && end < start
        {
            return Err(format!(
                "a finding cites a range that ends before it starts: {start}-{end}"
            ));
        }
    }
    Ok(report)
}

/// The body of the first ```json fence, if the answer has one.
fn fence_body(answer: &str) -> Option<&str> {
    let start = answer.find("```json")? + "```json".len();
    let rest = &answer[start..];
    let end = rest.find("```")?;
    Some(&rest[..end])
}

/// Plain text, one section per finding. Only what the report carries is shown.
/// The withheld-path count is announced by the caller that resolved the target,
/// so no path name or content ever reaches this function.
pub fn render_report(report: &ReviewReport) -> String {
    let mut out = format!("Review of {}:\n", report.target);
    if report.findings.is_empty() {
        out.push_str("\nNo findings.\n");
    }
    for finding in &report.findings {
        let location = match (finding.line_start, finding.line_end) {
            (Some(start), Some(end)) if end != start => format!("{}:{start}-{end}", finding.path),
            (Some(start), _) => format!("{}:{start}", finding.path),
            _ => finding.path.clone(),
        };
        let context = if finding.outside_diff {
            " · outside the diff"
        } else {
            ""
        };
        out.push_str(&format!(
            "\n[{}] {} ({}){context}\n  at {location}\n  impact: {}\n  fix: {}\n",
            finding.severity.as_str(),
            finding.title,
            finding.confidence.as_str(),
            finding.impact,
            finding.remediation,
        ));
    }
    out.push_str("\nLimits:");
    if report.limitations.is_empty() {
        out.push_str(" only the reviewed diff and the files the reviewer read. No tests were run.");
    } else {
        for limit in &report.limitations {
            out.push_str("\n- ");
            out.push_str(limit);
        }
        out.push_str("\nNo tests were run.");
    }
    out.push('\n');
    out
}

#[cfg(test)]
#[path = "../tests/agent/review.rs"]
mod tests;
