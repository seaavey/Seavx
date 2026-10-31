//! The runtime's event vocabulary.
//!
//! Presentation events stream to whoever is drawing the turn; the tool and turn
//! variants carry the lifecycle the ledger persists. Nothing here decides what
//! is durable: `NewEvent::from_event` in the session store does.

use std::time::Duration;

use crate::agent::cancel::{Cleanup, TurnStatus};

/// The longest summary that may reach a database row. The ellipsis `bounded`
/// appends sits outside this count, so a stored summary is 513 characters at
/// most.
pub const SUMMARY_LIMIT: usize = 512;

/// Truncates to `limit` characters on a character boundary and marks the cut.
/// Byte slicing would panic on multibyte text, and a silent cut would read as
/// the whole value.
pub fn bounded(text: &str, limit: usize) -> String {
    match text.char_indices().nth(limit) {
        None => text.to_string(),
        Some((cut, _)) => {
            let mut capped = String::with_capacity(cut + 4);
            capped.push_str(&text[..cut]);
            capped.push('…');
            capped
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentEvent {
    /// A streamed answer fragment.
    Token(String),
    /// A streamed reasoning fragment, rendered apart from the answer.
    Thinking(String),
    /// Engine news for the user, without the old `System: ` prefix.
    Notice(String),
    /// Text the user sent into a running turn, delivered at its next boundary.
    /// Its own variant because it is transcript content, not model output: the
    /// worker stores it as a user row and the turn must not be replayed without it.
    Steer(String),
    Usage {
        total_tokens: i32,
        prompt_tokens: i32,
    },
    /// One transition of one tool call.
    Tool(ToolEvent),
    /// The turn's terminal status, once, after the answer or the failure.
    Turn {
        status: TurnStatus,
        failure: Option<FailureKind>,
        cleanup: Option<Cleanup>,
        pid: Option<u32>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolEvent {
    /// The provider's tool-call id: the only key that pairs a call with its result.
    pub call_id: String,
    pub name: String,
    /// Redacted and bounded argument summary: the only tool text that is stored.
    pub summary: String,
    pub status: ToolStatus,
    pub failure: Option<FailureKind>,
    /// Captured once when the call is queued, so every transition carries the
    /// same start and the TUI can show it.
    pub started_at: i64,
    /// Set on the terminal transition only.
    pub elapsed: Option<Duration>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolStatus {
    Queued,
    Running,
    Completed,
    Failed,
    Cancelled,
}

impl ToolStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureKind {
    Provider,
    PermissionDenied,
    Tool,
    Timeout,
    /// The kill was sent but the OS never confirmed the process tree is gone.
    CleanupUnconfirmed,
    Internal,
}

impl FailureKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Provider => "provider",
            Self::PermissionDenied => "permission_denied",
            Self::Tool => "tool",
            Self::Timeout => "timeout",
            Self::CleanupUnconfirmed => "cleanup_unconfirmed",
            Self::Internal => "internal",
        }
    }

    /// Message-based, like `classify_failure`: only an error that names the
    /// condition is classified; anything else is a plain tool failure.
    pub fn for_tool_error(message: &str) -> Self {
        let lower = message.to_ascii_lowercase();
        if lower.contains("timed out") || lower.contains("timeout") {
            Self::Timeout
        } else {
            Self::Tool
        }
    }
}

#[cfg(test)]
#[path = "../tests/core/events.rs"]
mod tests;
