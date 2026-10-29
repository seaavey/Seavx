//! Context budgeting: one estimate, one threshold, one window rule.
//!
//! The estimate is a **serialized-request estimate**: `ceil(chars/4)` over
//! `serde_json::to_string(&ChatRequest)`. It is not the wire payload — genai
//! translates the generic request per provider — but it is deterministic and
//! conservative, because JSON structure inflates the character count above the
//! token cost. It can still under-count CJK or code-dense text by up to ~2x,
//! which is why overflow is handled by one retry rather than promised away.

use genai::chat::{ChatMessage, ChatRequest, ChatRole};

/// `ceil(chars/4)` of the serialized request; a serialization failure falls
/// back to the `Debug` form so this never returns zero for a real request.
pub fn estimate_tokens(request: &ChatRequest) -> usize {
    let serialized = serde_json::to_string(request).unwrap_or_else(|_| format!("{request:?}"));
    serialized.chars().count().div_ceil(4)
}

/// 80 % of the model's window: the point where a request is compacted.
pub fn request_budget(limit: i32) -> usize {
    (limit.max(0) as usize) * 80 / 100
}

/// 60 % of [`request_budget`]: what the recent turns may keep.
pub fn keep_recent_budget(limit: i32) -> usize {
    request_budget(limit) * 60 / 100
}

/// Index of the first message that stays; `0` keeps everything. The cut lands
/// on a user row, so a turn (a user row and everything it produced) is never
/// split, and the newest turn is kept whole even when it alone exceeds the
/// budget.
pub fn window_start(messages: &[ChatMessage], budget: usize) -> usize {
    if messages.is_empty() {
        return 0;
    }

    // Turn boundaries: index 0 opens a turn, and so does every user row.
    let mut starts = vec![0];
    for (index, message) in messages.iter().enumerate().skip(1) {
        if message.role == ChatRole::User {
            starts.push(index);
        }
    }

    let newest = starts.len() - 1;
    let mut used = 0;
    let mut chosen = starts[newest];
    for (position, &start) in starts.iter().enumerate().rev() {
        let end = starts.get(position + 1).copied().unwrap_or(messages.len());
        let size: usize = messages[start..end].iter().map(estimate_message).sum();
        if position != newest && used + size > budget {
            break;
        }
        used += size;
        chosen = start;
    }
    chosen
}

fn estimate_message(message: &ChatMessage) -> usize {
    let serialized = serde_json::to_string(message).unwrap_or_else(|_| format!("{message:?}"));
    serialized.chars().count().div_ceil(4)
}

#[cfg(test)]
#[path = "../tests/core/budget.rs"]
mod tests;
