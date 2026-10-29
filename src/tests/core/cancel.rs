//! Cancelling a turn: while waiting, mid-stream and between turns.

use crate::agent::backend::{BackendError, ChatBackend, StreamDelta};
use crate::agent::cancel::{CancelToken, TurnStatus};
use crate::agent::engine::{AgentConfig, AgentMode, AgentRuntime};
use crate::agent::permissions::DenyingConfirmer;
use crate::agent::security::WorkspaceSecurity;
use crate::session::MessageStatus;
use genai::adapter::AdapterKind;
use genai::chat::{ChatOptions, ChatRequest, ChatResponse, MessageContent};
use std::sync::{Arc, Mutex};
use std::time::Duration;

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

/// A provider that never answers on its own: `send` and `stream` both wait for
/// the cancel to drop them.
#[derive(Clone, Default)]
struct SlowBackend {
    calls: Arc<Mutex<Vec<&'static str>>>,
}

impl SlowBackend {
    fn calls(&self) -> Vec<&'static str> {
        self.calls.lock().unwrap().clone()
    }
}

#[async_trait::async_trait]
impl ChatBackend for SlowBackend {
    async fn send(
        &self,
        _model: &str,
        _request: ChatRequest,
        _options: Option<&ChatOptions>,
    ) -> Result<ChatResponse, BackendError> {
        self.calls.lock().unwrap().push("send");
        tokio::time::sleep(Duration::from_secs(30)).await;
        Ok(response("never"))
    }

    async fn stream(
        &self,
        _model: &str,
        _request: ChatRequest,
        _options: Option<&ChatOptions>,
        on_delta: &mut (dyn FnMut(StreamDelta) + Send),
    ) -> Result<ChatResponse, BackendError> {
        self.calls.lock().unwrap().push("stream");
        on_delta(StreamDelta::Token("one ".into()));
        on_delta(StreamDelta::Token("two ".into()));
        on_delta(StreamDelta::Token("three".into()));
        // Blocks with the answer in hand: only a cancel ends this.
        tokio::time::sleep(Duration::from_secs(30)).await;
        Ok(response("one two three"))
    }
}

fn runtime_with(backend: &SlowBackend, workspace: &std::path::Path, stream: bool) -> AgentRuntime {
    let mut config = AgentConfig::load(workspace);
    config.model = "fake-model".to_string();
    config.max_turns = 4;
    config.stream = stream;
    AgentRuntime::with_backend(
        config,
        WorkspaceSecurity::new(workspace),
        Box::new(backend.clone()),
    )
}

#[tokio::test]
async fn cancelling_while_awaiting_the_provider_drops_the_future() {
    let workspace = tempfile::tempdir().unwrap();
    let backend = SlowBackend::default();
    let runtime = runtime_with(&backend, workspace.path(), false);
    let cancel = CancelToken::new();

    let turn = {
        let cancel = cancel.clone();
        tokio::spawn(async move {
            let mut rows = Vec::new();
            let outcome = runtime
                .execute_task(
                    "do it",
                    AgentMode::Build,
                    Vec::new(),
                    &DenyingConfirmer,
                    &cancel,
                    |_| {},
                    |message, status| {
                        rows.push((message.content.texts().join(""), status));
                    },
                )
                .await;
            (outcome, rows)
        })
    };
    tokio::time::sleep(Duration::from_millis(50)).await;
    cancel.cancel();

    let (outcome, rows) = tokio::time::timeout(Duration::from_secs(2), turn)
        .await
        .expect("the turn must return within the grace window")
        .unwrap();

    assert_eq!(outcome.status, TurnStatus::Cancelled);
    assert!(outcome.result.is_err(), "a cancelled turn is not an answer");
    assert_eq!(
        rows,
        vec![(String::new(), MessageStatus::Cancelled)],
        "one assistant row, cancelled, no text"
    );
}

#[tokio::test]
async fn cancelling_mid_stream_keeps_what_arrived() {
    let workspace = tempfile::tempdir().unwrap();
    let backend = SlowBackend::default();
    let runtime = runtime_with(&backend, workspace.path(), true);
    let cancel = CancelToken::new();
    let clone = cancel.clone();

    let turn = tokio::spawn(async move {
        let mut rows = Vec::new();
        let mut events = Vec::new();
        let outcome = runtime
            .execute_task(
                "do it",
                AgentMode::Build,
                Vec::new(),
                &DenyingConfirmer,
                &clone,
                |event| events.push(event.to_string()),
                |message, status| rows.push((message.content.texts().join(""), status)),
            )
            .await;
        (outcome, rows, events)
    });

    // Wait for the three deltas to land before cancelling.
    tokio::time::sleep(Duration::from_millis(100)).await;
    cancel.cancel();

    let (outcome, rows, events) = tokio::time::timeout(Duration::from_secs(2), turn)
        .await
        .expect("the turn must return within the grace window")
        .unwrap();

    assert_eq!(outcome.status, TurnStatus::Cancelled);
    assert_eq!(
        rows,
        vec![("one two three".to_string(), MessageStatus::Partial)],
        "the received prefix is persisted"
    );
    assert_eq!(
        backend.calls(),
        vec!["stream"],
        "a cancelled stream is never resumed"
    );
    assert!(
        events.iter().any(|event| event == "TOKEN:three"),
        "the deltas reached the sink: {events:?}"
    );
}

#[tokio::test]
async fn the_state_machine_has_no_reverse_edges() {
    let mut turn = crate::agent::cancel::TurnState::new();
    assert_eq!(turn.status(), TurnStatus::Running);
    assert!(turn.transition(TurnStatus::Completed));
    assert!(
        !turn.transition(TurnStatus::Cancelling),
        "a completed turn cannot be cancelled"
    );
    assert_eq!(turn.status(), TurnStatus::Completed);

    let mut cancelled = crate::agent::cancel::TurnState::new();
    assert!(cancelled.transition(TurnStatus::Cancelling));
    assert!(cancelled.transition(TurnStatus::Cancelled));
    assert!(!cancelled.transition(TurnStatus::Completed));
    assert!(!cancelled.transition(TurnStatus::Running));
    assert_eq!(cancelled.status(), TurnStatus::Cancelled);
    assert!(cancelled.status().is_terminal());
}

#[tokio::test]
async fn cancelling_between_turns_is_immediate() {
    let workspace = tempfile::tempdir().unwrap();
    let backend = SlowBackend::default();
    let runtime = runtime_with(&backend, workspace.path(), true);
    let cancel = CancelToken::new();
    cancel.cancel();

    let outcome = runtime
        .execute_task(
            "do it",
            AgentMode::Build,
            Vec::new(),
            &DenyingConfirmer,
            &cancel,
            |_| {},
            |_, _| {},
        )
        .await;

    assert_eq!(outcome.status, TurnStatus::Cancelled);
    assert!(
        backend.calls().is_empty(),
        "a turn cancelled before it starts calls no provider"
    );
}
