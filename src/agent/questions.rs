//! The clarification seam.
//!
//! Deliberately separate from [`crate::agent::permissions::Confirmer`]: a
//! permission answer means "may I do this", a question answer is content. The
//! two never share a type, so a permission hotkey cannot answer a question.

use crate::session::QuestionKind;

/// One structured question the model asked, tied to the call that asked it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuestionRequest {
    pub call_id: String,
    pub question: String,
    pub kind: QuestionKind,
    pub options: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QuestionAnswer {
    Answered(String),
    /// The human moved on without a value: the agent must not assume one.
    Skipped,
    Cancelled,
    /// Nobody can answer here, e.g. headless `exec`.
    Unavailable,
}

impl QuestionAnswer {
    /// The tool result the model sees. Every non-answer says plainly that no
    /// value may be assumed, so a dependent step cannot silently guess.
    pub fn tool_result(&self) -> String {
        match self {
            Self::Answered(answer) if answer.trim().is_empty() => {
                "The user answered with nothing. Proceed without assuming a value and say what you left open."
                    .to_string()
            }
            Self::Answered(answer) => format!("The user answered: {answer}"),
            Self::Skipped => "The user skipped the question. Proceed without assuming an answer and state the consequence."
                .to_string(),
            Self::Cancelled => "The user cancelled the question. Do not act on an assumed answer; stop and report what you needed."
                .to_string(),
            Self::Unavailable => "No human is available to answer here. Do not assume an answer; report what you could not determine."
                .to_string(),
        }
    }
}

#[async_trait::async_trait]
pub trait Questioner: Send + Sync {
    async fn ask(&self, request: QuestionRequest) -> QuestionAnswer;
}

/// The default: headless runs never wait on a human.
pub struct UnavailableQuestioner;

#[async_trait::async_trait]
impl Questioner for UnavailableQuestioner {
    async fn ask(&self, _request: QuestionRequest) -> QuestionAnswer {
        QuestionAnswer::Unavailable
    }
}

/// Read the model's `ask_user` arguments into the seam's shape.
pub fn parse_request(call_id: &str, args: &serde_json::Value) -> Result<QuestionRequest, String> {
    let question = args
        .get("question")
        .and_then(|value| value.as_str())
        .unwrap_or_default()
        .trim()
        .to_string();
    if question.is_empty() {
        return Err("ask_user needs a question".to_string());
    }
    let kind = match args.get("kind").and_then(|value| value.as_str()) {
        Some("multi") => QuestionKind::Multi,
        Some("text") | None => QuestionKind::Text,
        Some("single") => QuestionKind::Single,
        Some(other) => return Err(format!("unknown question kind '{other}'")),
    };
    let options: Vec<String> = args
        .get("options")
        .and_then(|value| value.as_array())
        .map(|options| {
            options
                .iter()
                .filter_map(|option| option.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    if kind != QuestionKind::Text && options.is_empty() {
        return Err("a choice question needs at least one option".to_string());
    }
    Ok(QuestionRequest {
        call_id: call_id.to_string(),
        question,
        kind,
        options,
    })
}

#[cfg(test)]
#[path = "../tests/agent/questions.rs"]
mod tests;
