//! The one seam between the engine and a provider.
//!
//! `GenaiBackend` is the only production implementation; the fake lives in the
//! test tree, so cancel, mid-stream failure and fallback are testable without a
//! network or credentials.

use futures::StreamExt;
use genai::Client;
use genai::chat::{
    ChatOptions, ChatRequest, ChatResponse, ChatStreamEvent, MessageContent, ToolCall, Usage,
};

/// What one backend call may fail with. Boxed rather than `genai::Error` so a
/// test fake can produce its own error and the classifier can read any message.
pub type BackendError = Box<dyn std::error::Error + Send + Sync>;

/// One piece of a streamed answer, as it arrives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamDelta {
    Token(String),
    Thinking(String),
}

/// One request, one response — and the same request streamed.
#[async_trait::async_trait]
pub trait ChatBackend: Send + Sync {
    async fn send(
        &self,
        model: &str,
        request: ChatRequest,
        options: Option<&ChatOptions>,
    ) -> Result<ChatResponse, BackendError>;

    /// Streams the answer, one delta at a time, and returns it assembled.
    ///
    /// A backend that cannot stream falls back to `send`: the caller sees no
    /// deltas, but a turn still completes.
    async fn stream(
        &self,
        model: &str,
        request: ChatRequest,
        options: Option<&ChatOptions>,
        _on_delta: &mut (dyn FnMut(StreamDelta) + Send),
    ) -> Result<ChatResponse, BackendError> {
        self.send(model, request, options).await
    }
}

/// The production backend: genai over the configured endpoint.
pub struct GenaiBackend {
    /// `None` when the config named no provider: every call reports that instead
    /// of reaching a host the user never configured.
    client: Option<Client>,
}

impl GenaiBackend {
    pub fn new(client: Client) -> Self {
        Self {
            client: Some(client),
        }
    }

    pub(crate) fn unavailable() -> Self {
        Self { client: None }
    }

    fn client(&self) -> Result<&Client, BackendError> {
        self.client.as_ref().ok_or_else(|| {
            std::io::Error::other("no provider configured: run `seavx setup`").into()
        })
    }
}

#[async_trait::async_trait]
impl ChatBackend for GenaiBackend {
    async fn send(
        &self,
        model: &str,
        request: ChatRequest,
        options: Option<&ChatOptions>,
    ) -> Result<ChatResponse, BackendError> {
        Ok(self.client()?.exec_chat(model, request, options).await?)
    }

    async fn stream(
        &self,
        model: &str,
        request: ChatRequest,
        options: Option<&ChatOptions>,
        on_delta: &mut (dyn FnMut(StreamDelta) + Send),
    ) -> Result<ChatResponse, BackendError> {
        // Usage, reasoning and the final content only travel on the end event
        // when the adapter was asked to capture them.
        let options = options
            .cloned()
            .unwrap_or_default()
            .with_capture_usage(true)
            .with_capture_content(true)
            .with_capture_tool_calls(true);

        let response = self
            .client()?
            .exec_chat_stream(model, request, Some(&options))
            .await?;
        let model_iden = response.model_iden.clone();
        let mut stream = response.stream;

        let mut text = String::new();
        let mut thinking = String::new();
        let mut calls: Vec<ToolCall> = Vec::new();
        let mut captured: Option<MessageContent> = None;
        let mut usage = Usage::default();
        let mut stop_reason = None;
        let mut response_id = None;
        let mut ended = false;

        while let Some(event) = stream.next().await {
            let event = match event {
                Ok(event) => event,
                // The transport broke mid-answer: keep what the sink already
                // got, hand the prefix back so the caller can persist it.
                Err(error) => return Err(prefix_error(error, &text)),
            };
            match event {
                ChatStreamEvent::Chunk(chunk) if !chunk.content.is_empty() => {
                    text.push_str(&chunk.content);
                    on_delta(StreamDelta::Token(chunk.content));
                }
                ChatStreamEvent::ReasoningChunk(chunk) if !chunk.content.is_empty() => {
                    thinking.push_str(&chunk.content);
                    on_delta(StreamDelta::Thinking(chunk.content));
                }
                // Each tool-call chunk carries the call accumulated so far, so
                // the newest one for a call id replaces the previous.
                ChatStreamEvent::ToolCallChunk(chunk) => {
                    match calls
                        .iter_mut()
                        .find(|call| call.call_id == chunk.tool_call.call_id)
                    {
                        Some(call) => *call = chunk.tool_call,
                        None => calls.push(chunk.tool_call),
                    }
                }
                ChatStreamEvent::End(end) => {
                    ended = true;
                    usage = end.captured_usage.unwrap_or_default();
                    stop_reason = end.captured_stop_reason;
                    response_id = end.captured_response_id;
                    thinking = end.captured_reasoning_content.unwrap_or(thinking);
                    captured = end.captured_content;
                }
                _ => {}
            }
        }

        if !ended {
            return Err(prefix_error(
                std::io::Error::other("stream ended before completion"),
                &text,
            ));
        }

        let content = match captured {
            Some(content) if !content.is_empty() => content,
            _ if calls.is_empty() => MessageContent::from_text(text),
            _ => MessageContent::from_tool_calls(calls.into_iter().map(parse_arguments).collect()),
        };

        Ok(ChatResponse {
            content,
            reasoning_content: (!thinking.is_empty()).then_some(thinking),
            provider_model_iden: model_iden.clone(),
            model_iden,
            stop_reason,
            usage,
            captured_raw_body: None,
            response_id,
        })
    }
}

/// A failed stream still delivered text: name that prefix in the error, which
/// is the only place the caller can learn how much of the answer it holds.
fn prefix_error(error: impl Into<BackendError>, text: &str) -> BackendError {
    let error = error.into();
    if text.is_empty() {
        return error;
    }
    let prefix: String = text.chars().take(200).collect();
    std::io::Error::other(format!("{error} (stream ended after {prefix:?})")).into()
}

/// Streamed arguments arrive as a JSON string; the engine expects a value.
fn parse_arguments(mut call: ToolCall) -> ToolCall {
    if let serde_json::Value::String(raw) = &call.fn_arguments
        && let Ok(parsed) = serde_json::from_str(raw)
    {
        call.fn_arguments = parsed;
    }
    call
}

#[cfg(test)]
#[path = "../tests/core/backend.rs"]
mod tests;
