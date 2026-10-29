//! The one seam between the engine and a provider.
//!
//! `GenaiBackend` is the only production implementation; the fake lives in the
//! test tree, so cancel, mid-stream failure and fallback are testable without a
//! network or credentials.

use genai::Client;
use genai::chat::{ChatOptions, ChatRequest, ChatResponse};

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
}

/// The production backend: genai over the configured endpoint.
pub struct GenaiBackend {
    client: Client,
}

impl GenaiBackend {
    pub fn new(client: Client) -> Self {
        Self { client }
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
        Ok(self.client.exec_chat(model, request, options).await?)
    }
}

#[cfg(test)]
#[path = "../tests/core/backend.rs"]
mod tests;
