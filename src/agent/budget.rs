//! Context budgeting: one estimate, one threshold, one window rule.
//!
//! The estimate is a **serialized-request estimate**: `ceil(chars/4)` over
//! `serde_json::to_string(&ChatRequest)`. It is not the wire payload — genai
//! translates the generic request per provider — but it is deterministic and
//! conservative, because JSON structure inflates the character count above the
//! token cost. It can still under-count CJK or code-dense text by up to ~2x,
//! which is why overflow is handled by one retry rather than promised away.

use genai::chat::ChatRequest;

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

#[cfg(test)]
#[path = "../tests/core/budget.rs"]
mod tests;
