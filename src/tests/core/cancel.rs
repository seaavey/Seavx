//! Cancelling a turn: while waiting, mid-stream and between turns.

use crate::agent::backend::{BackendError, ChatBackend, StreamDelta};
use crate::agent::cancel::{CancelToken, Cleanup, TurnStatus, cleanup_notice};
use crate::agent::engine::{AgentConfig, AgentMode, AgentRuntime};
use crate::agent::permissions::DenyingConfirmer;
use crate::agent::security::WorkspaceSecurity;
#[cfg(unix)]
use crate::agent::tools::run_shell;
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

/// Polls until `probe` says the pid is gone, failing after two seconds.
#[cfg(unix)]
async fn wait_until_gone(pid: i32) -> bool {
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while unsafe { libc::kill(pid, 0) } == 0 {
        if std::time::Instant::now() > deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    true
}

fn runtime_with(backend: &SlowBackend, workspace: &std::path::Path, stream: bool) -> AgentRuntime {
    let mut config = AgentConfig::load(workspace);
    config.model = Some("fake-model".to_string());
    config.max_turns = 4;
    config.stream = stream;
    config.base_url = Some("https://gateway.test/v1/".to_string());
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
                    genai::chat::ChatMessage::user("do it"),
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
        let mut events: Vec<crate::agent::events::AgentEvent> = Vec::new();
        let outcome = runtime
            .execute_task(
                genai::chat::ChatMessage::user("do it"),
                AgentMode::Build,
                Vec::new(),
                &DenyingConfirmer,
                &clone,
                |event| events.push(event),
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
        events
            .iter()
            .any(|event| matches!(event, crate::agent::events::AgentEvent::Token(token) if token == "three")),
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
            genai::chat::ChatMessage::user("do it"),
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

#[cfg(unix)]
#[tokio::test]
async fn cancelling_a_command_kills_the_whole_tree_confirmed() {
    let workspace = tempfile::tempdir().unwrap();
    let cancel = CancelToken::new();
    let path = workspace.path().to_path_buf();
    let clone = cancel.clone();

    // A shell that starts a grandchild and waits for it: killing only the shell
    // would leave `sleep 30` behind.
    let handle = tokio::spawn(async move {
        let security = WorkspaceSecurity::new(path);
        run_shell(
            "sleep 30 & echo $! > pid; wait",
            &security,
            Duration::from_secs(30),
            &clone,
        )
        .await
    });

    let pid: i32 = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Ok(text) = std::fs::read_to_string(workspace.path().join("pid"))
                && let Ok(pid) = text.trim().parse()
            {
                return pid;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the command must write its grandchild pid");

    cancel.cancel();
    let outcome = tokio::time::timeout(Duration::from_secs(5), handle)
        .await
        .expect("the kill must finish inside the window")
        .unwrap()
        .expect("a cancelled command still returns a result");

    assert_eq!(outcome.cleanup, Some(Cleanup::Confirmed));
    assert_eq!(outcome.status, -1, "a killed command has no exit code");
    assert!(
        wait_until_gone(pid).await,
        "the grandchild {pid} survived the group kill"
    );
}

#[tokio::test]
async fn an_unconfirmed_reap_is_reported_not_claimed_clean() {
    // The kernel confirms every kill it delivers, so the timeout branch is
    // forced through the same decision function the tool uses.
    let cleanup = crate::agent::cancel::reap_or_report(
        std::future::pending::<()>(),
        Duration::from_millis(50),
    )
    .await;
    assert_eq!(cleanup, Cleanup::Unconfirmed);

    let pid = 4242;
    let notice = cleanup_notice(pid);
    assert!(
        notice.contains(&pid.to_string()),
        "the notice names the pid: {notice}"
    );
    assert!(
        !notice.to_lowercase().contains("clean"),
        "an unconfirmed cleanup is never called clean: {notice}"
    );
}
