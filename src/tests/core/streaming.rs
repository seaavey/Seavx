//! Streaming, the single fallback and partial turns.

use crate::agent::backend::{BackendError, ChatBackend, StreamDelta};
use crate::agent::engine::{AgentConfig, AgentMode, AgentRuntime};
use crate::agent::permissions::DenyingConfirmer;
use crate::agent::security::WorkspaceSecurity;
use crate::session::MessageStatus;
use genai::adapter::AdapterKind;
use genai::chat::{ChatOptions, ChatRequest, ChatResponse, MessageContent};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

fn response(text: &str) -> ChatResponse {
    let model = genai::ModelIden::new(AdapterKind::OpenAI, "fake-model");
    ChatResponse {
        content: MessageContent::from_text(text),
        reasoning_content: None,
        model_iden: model.clone(),
        provider_model_iden: model,
        stop_reason: None,
        usage: Default::default(),
        captured_raw_body: None,
        response_id: None,
    }
}

/// One scripted stream: the deltas it delivers, in order, and the error it dies
/// with (`Ok(delta)` forever if it never fails).
type StreamScript = Vec<Result<StreamDelta, String>>;

#[derive(Clone, Default)]
struct FakeBackend {
    inner: Arc<FakeInner>,
}

#[derive(Default)]
struct FakeInner {
    streams: Mutex<VecDeque<StreamScript>>,
    sends: Mutex<VecDeque<Result<String, String>>>,
    calls: Mutex<Vec<&'static str>>,
    reasoning: Mutex<Option<String>>,
}

impl FakeBackend {
    fn stream_then(&self, script: StreamScript) {
        self.inner.streams.lock().unwrap().push_back(script);
    }

    /// Reasoning the provider hands back whole once the stream ends, on top of
    /// whatever the deltas already carried.
    fn reasoning_on_end(&self, text: &str) {
        *self.inner.reasoning.lock().unwrap() = Some(text.to_string());
    }

    fn answer(&self, text: &str) {
        self.inner
            .sends
            .lock()
            .unwrap()
            .push_back(Ok(text.to_string()));
    }

    fn send_fails(&self, message: &str) {
        self.inner
            .sends
            .lock()
            .unwrap()
            .push_back(Err(message.to_string()));
    }

    fn calls(&self) -> Vec<&'static str> {
        self.inner.calls.lock().unwrap().clone()
    }
}

#[async_trait::async_trait]
impl ChatBackend for FakeBackend {
    async fn send(
        &self,
        _model: &str,
        _request: ChatRequest,
        _options: Option<&ChatOptions>,
    ) -> Result<ChatResponse, BackendError> {
        self.inner.calls.lock().unwrap().push("send");
        match self.inner.sends.lock().unwrap().pop_front() {
            Some(Ok(text)) => Ok(response(&text)),
            Some(Err(message)) => Err(std::io::Error::other(message).into()),
            None => Err(std::io::Error::other("fake backend has no scripted answer").into()),
        }
    }

    async fn stream(
        &self,
        _model: &str,
        _request: ChatRequest,
        _options: Option<&ChatOptions>,
        on_delta: &mut (dyn FnMut(StreamDelta) + Send),
    ) -> Result<ChatResponse, BackendError> {
        self.inner.calls.lock().unwrap().push("stream");
        let script = self
            .inner
            .streams
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| vec![Err("fake backend has no scripted stream".to_string())]);

        let mut text = String::new();
        for step in script {
            match step {
                Ok(delta) => {
                    if let StreamDelta::Token(token) = &delta {
                        text.push_str(token);
                    }
                    on_delta(delta);
                }
                Err(message) => return Err(std::io::Error::other(message).into()),
            }
        }
        Ok(ChatResponse {
            reasoning_content: self.inner.reasoning.lock().unwrap().clone(),
            ..response(&text)
        })
    }
}

fn runtime_with(backend: &FakeBackend, workspace: &std::path::Path, stream: bool) -> AgentRuntime {
    let mut config = AgentConfig::load(workspace);
    config.model = "fake-model".to_string();
    config.max_turns = 4;
    config.stream = stream;
    let security = WorkspaceSecurity::new(workspace);
    AgentRuntime::with_backend(config, security, Box::new(backend.clone()))
}

fn token(text: &str) -> Result<StreamDelta, String> {
    Ok(StreamDelta::Token(text.to_string()))
}

fn thinking(text: &str) -> Result<StreamDelta, String> {
    Ok(StreamDelta::Thinking(text.to_string()))
}

#[tokio::test]
async fn a_stream_that_fails_before_the_first_chunk_falls_back_once() {
    let workspace = tempfile::tempdir().unwrap();
    let fake = FakeBackend::default();
    fake.stream_then(vec![Err("connection reset by peer".into())]);
    fake.answer("fallback answer");
    let runtime = runtime_with(&fake, workspace.path(), true);

    let answer = runtime
        .execute_task(
            "do it",
            AgentMode::Build,
            Vec::new(),
            &DenyingConfirmer,
            &crate::agent::cancel::CancelToken::new(),
            |_| {},
            |_, _| {},
        )
        .await
        .result
        .unwrap();

    assert_eq!(answer, "fallback answer");
    assert_eq!(fake.calls(), vec!["stream", "send"]);
}

#[tokio::test]
async fn a_stream_that_fails_after_one_chunk_is_not_retried() {
    let workspace = tempfile::tempdir().unwrap();
    let fake = FakeBackend::default();
    fake.stream_then(vec![token("half an ans"), Err("broken pipe".into())]);
    let runtime = runtime_with(&fake, workspace.path(), true);

    let mut events = Vec::new();
    let mut reported = Vec::new();
    let error = runtime
        .execute_task(
            "do it",
            AgentMode::Build,
            Vec::new(),
            &DenyingConfirmer,
            &crate::agent::cancel::CancelToken::new(),
            |event| events.push(event.to_string()),
            |message, status| reported.push((message.clone(), status)),
        )
        .await
        .result
        .unwrap_err();

    assert!(error.to_string().contains("broken pipe"), "{error}");
    assert_eq!(fake.calls(), vec!["stream"], "no resend after output");

    assert!(
        events.iter().any(|event| event == "TOKEN:half an ans"),
        "the delta reaches the sink: {events:?}"
    );
    let (partial, status) = reported.last().expect("a partial row is reported");
    assert_eq!(*status, MessageStatus::Partial);
    assert_eq!(partial.content.texts().join(""), "half an ans");
}

#[tokio::test]
async fn streamed_reasoning_is_not_repeated_by_the_captured_copy() {
    let workspace = tempfile::tempdir().unwrap();
    let fake = FakeBackend::default();
    // The provider replays the whole reasoning once the stream ends.
    fake.reasoning_on_end("weighing it");
    fake.stream_then(vec![thinking("weighing "), thinking("it"), token("hi")]);
    let runtime = runtime_with(&fake, workspace.path(), true);

    let mut events = Vec::new();
    let answer = runtime
        .execute_task(
            "do it",
            AgentMode::Build,
            Vec::new(),
            &DenyingConfirmer,
            &crate::agent::cancel::CancelToken::new(),
            |event| events.push(event.to_string()),
            |_, _| {},
        )
        .await
        .result
        .unwrap();

    assert_eq!(answer, "hi");
    let thinking_events: Vec<&String> = events
        .iter()
        .filter(|event| event.starts_with("THINKING:"))
        .collect();
    assert_eq!(
        thinking_events,
        ["THINKING:weighing ", "THINKING:it"],
        "the deltas are the reasoning; the captured copy would draw it twice: {events:?}"
    );
}

#[tokio::test]
async fn the_fallback_consumes_the_single_retry() {
    let workspace = tempfile::tempdir().unwrap();
    let fake = FakeBackend::default();
    fake.stream_then(vec![Err("connection reset by peer".into())]);
    fake.send_fails("400: unknown tool_call_id in the replayed history");
    let runtime = runtime_with(&fake, workspace.path(), true);

    let error = runtime
        .execute_task(
            "do it",
            AgentMode::Build,
            Vec::new(),
            &DenyingConfirmer,
            &crate::agent::cancel::CancelToken::new(),
            |_| {},
            |_, _| {},
        )
        .await
        .result
        .unwrap_err();

    assert!(error.to_string().contains("tool_call_id"), "{error}");
    assert_eq!(
        fake.calls(),
        vec!["stream", "send"],
        "the fallback is the only retry"
    );
}

#[tokio::test]
async fn stream_false_skips_the_stream_attempt() {
    let workspace = tempfile::tempdir().unwrap();
    let fake = FakeBackend::default();
    fake.answer("plain answer");
    let runtime = runtime_with(&fake, workspace.path(), false);

    let answer = runtime
        .execute_task(
            "do it",
            AgentMode::Build,
            Vec::new(),
            &DenyingConfirmer,
            &crate::agent::cancel::CancelToken::new(),
            |_| {},
            |_, _| {},
        )
        .await
        .result
        .unwrap();

    assert_eq!(answer, "plain answer");
    assert_eq!(fake.calls(), vec!["send"]);
}

#[cfg(test)]
mod history_halving {
    use crate::agent::engine::halve_turns;
    use genai::chat::{ChatMessage, ChatRequest, ChatRole};

    fn history(turns: usize) -> ChatRequest {
        let mut request = ChatRequest::default().append_message(ChatMessage::system("system"));
        for index in 0..turns {
            request = request
                .append_message(ChatMessage::user(format!("u{index}")))
                .append_message(ChatMessage::assistant(format!("a{index}")));
        }
        request.append_message(ChatMessage::user("newest"))
    }

    #[test]
    fn a_body_without_a_user_row_is_not_halved() {
        // The replay retry used to index past the single boundary this history
        // has, which panicked instead of retrying.
        let request = ChatRequest::default()
            .append_message(ChatMessage::system("system"))
            .append_message(ChatMessage::assistant("a"))
            .append_message(ChatMessage::assistant("b"))
            .append_message(ChatMessage::user("newest"));
        let halved = halve_turns(&request);
        assert_eq!(
            halved.messages.len(),
            request.messages.len(),
            "nothing to halve keeps the whole history"
        );
    }

    #[test]
    fn halving_keeps_the_newest_turns_and_starts_at_a_user_row() {
        let request = history(6);
        let halved = halve_turns(&request);
        assert!(halved.messages.len() < request.messages.len());
        assert_eq!(halved.messages[0].role, ChatRole::System);
        assert_eq!(
            halved.messages[1].role,
            ChatRole::User,
            "a turn is cut at its user row, so no tool pair is split"
        );
        assert_eq!(halved.messages.last().unwrap().role, ChatRole::User);
    }

    #[test]
    fn a_short_history_is_returned_unchanged() {
        let request = history(1);
        assert_eq!(halve_turns(&request).messages.len(), request.messages.len());
    }
}

#[cfg(test)]
mod destination_kind {
    use crate::agent::engine::effective_kind;
    use crate::agent::permissions::ActionKind;
    use crate::agent::security::WorkspaceSecurity;

    #[test]
    fn a_global_destination_is_a_global_write_whatever_the_tool_says() {
        let workspace = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let security = WorkspaceSecurity::new(workspace.path());
        let memory = home.path().join(".seavx/MEMORY.md");
        let skill = home.path().join(".seavx/skills/review/SKILL.md");

        for destination in [memory, skill] {
            assert_eq!(
                effective_kind(
                    Some(home.path()),
                    &security,
                    ActionKind::Write,
                    Some(&destination),
                ),
                ActionKind::GlobalWrite,
                "{} must ask: it leaves the workspace",
                destination.display()
            );
        }

        assert_eq!(
            effective_kind(
                Some(home.path()),
                &security,
                ActionKind::Write,
                Some(&workspace.path().join("notes.txt")),
            ),
            ActionKind::Write,
            "a workspace destination keeps the tool's own kind"
        );
        assert_eq!(
            effective_kind(Some(home.path()), &security, ActionKind::Read, None),
            ActionKind::Read,
            "a tool with no destination is judged by what it declares"
        );
    }
}
