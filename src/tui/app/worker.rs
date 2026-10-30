use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use tokio::sync::mpsc;

use super::store::lock_store;
use super::{App, PendingQuestion, TuiConfirmer, TurnRequest};
use crate::agent::engine::AgentMode;
use crate::agent::events::AgentEvent;
use crate::tui::event::{Action, AgentState};

/// Ends the turn however the worker exits, panic included, so the UI never
/// stays stuck on `thinking`.
// The field is `pub(super)` so `src/tests/tui/app.rs` can still construct the
// guard through the root re-export; it is private to the app module otherwise.
pub(super) struct DoneGuard(pub(super) mpsc::UnboundedSender<Action>);

impl Drop for DoneGuard {
    fn drop(&mut self) {
        let _ = self.0.send(Action::AgentDone);
    }
}

impl App<'_> {
    /// The turn ended: the spinner stops, and a cancel says so once.
    pub(super) fn finish_turn(
        &mut self,
        status: crate::agent::cancel::TurnStatus,
        cleanup: Option<crate::agent::cancel::Cleanup>,
        pid: Option<u32>,
    ) {
        self.status = AgentState::Idle;
        if status != crate::agent::cancel::TurnStatus::Cancelled {
            return;
        }
        let mut line = String::from("System: cancelled");
        if cleanup == Some(crate::agent::cancel::Cleanup::Unconfirmed) {
            line.push('\n');
            line.push_str("System: ");
            line.push_str(&crate::agent::cancel::cleanup_notice(
                pid.unwrap_or_default(),
            ));
        }
        self.push_notice(line);
    }

    /// Answer the pending permission question with one key: `y` once, `a` this
    /// category, `u` unrestricted, `n`/`Esc` deny. Returns the answer sent, or
    /// `None` when the key is not an answer.
    pub(crate) fn answer_permission(
        &mut self,
        code: KeyCode,
    ) -> Option<crate::agent::permissions::Answer> {
        use crate::agent::permissions::Answer;
        let answer = match code {
            KeyCode::Char('y') => Answer::Once,
            KeyCode::Char('a') => Answer::Category,
            KeyCode::Char('u') => Answer::Unrestricted,
            KeyCode::Char('n') | KeyCode::Esc => Answer::Deny,
            _ => return None,
        };
        let pending = self.pending_permission.take()?;
        let _ = pending.reply.send(answer);
        Some(answer)
    }

    /// Feed one key to the clarification on screen. Returns the notice to show,
    /// or `None` when the key did nothing (the question stays up).
    pub(crate) fn answer_question(&mut self, key: &KeyEvent) -> Option<String> {
        use crate::session::{QuestionKind, QuestionState};
        use KeyCode::*;

        let mut pending = self.pending_question.take()?;
        let submit = |app: &mut App<'_>, pending: PendingQuestion, state, text: String| {
            if let Some(reply) = pending.reply {
                let _ = reply.send((state, text.clone()));
            } else {
                // Restored from the store: no turn is waiting, so the answer is
                // written down and appended as the one result for that call.
                app.resolve_restored_question(pending.id, state, &text);
            }
            app.revision += 1;
            match state {
                QuestionState::Answered => format!("Answered: {text}"),
                QuestionState::Skipped => "Question skipped.".to_string(),
                _ => "Question cancelled.".to_string(),
            }
        };

        match key.code {
            Esc => Some(submit(
                self,
                pending,
                QuestionState::Cancelled,
                String::new(),
            )),
            Char('s') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                Some(submit(self, pending, QuestionState::Skipped, String::new()))
            }
            Enter => {
                let (state, text) = pending.outcome();
                Some(submit(self, pending, state, text))
            }
            Up => {
                pending.selected = pending.selected.saturating_sub(1);
                self.pending_question = Some(pending);
                None
            }
            Down => {
                if pending.selected + 1 < pending.options.len() {
                    pending.selected += 1;
                }
                self.pending_question = Some(pending);
                None
            }
            Char(' ') if pending.kind == QuestionKind::Multi => {
                if !pending.chosen.remove(&pending.selected) {
                    pending.chosen.insert(pending.selected);
                }
                self.pending_question = Some(pending);
                None
            }
            Backspace if pending.kind == QuestionKind::Text => {
                pending.text.pop();
                self.pending_question = Some(pending);
                None
            }
            Char(c) if pending.kind == QuestionKind::Text => {
                if !key.modifiers.contains(KeyModifiers::CONTROL)
                    && !key.modifiers.contains(KeyModifiers::ALT)
                {
                    pending.text.push(c);
                }
                self.pending_question = Some(pending);
                None
            }
            _ => {
                self.pending_question = Some(pending);
                None
            }
        }
    }

    /// Answer a question restored from disk: resolve it once and append exactly
    /// one tool result, so a later replay does not invent an interruption.
    #[allow(clippy::needless_pass_by_value)]
    fn resolve_restored_question(
        &mut self,
        id: i64,
        state: crate::session::QuestionState,
        text: &str,
    ) {
        let Some(store) = self.store.as_ref() else {
            return;
        };
        let workspace = crate::session::workspace_key();
        let call = {
            let guard = lock_store(store);
            let Ok(_) = guard.resolve_question(&workspace, id, state, text) else {
                return;
            };
            guard.question(&workspace, id).ok().map(|q| q.tool_call_id)
        };
        let (Some(call), Some(session)) = (call, self.session_id) else {
            return;
        };
        use crate::session::QuestionState;
        let result = match state {
            QuestionState::Answered => format!("The user answered: {text}"),
            QuestionState::Skipped => "The user skipped the question.".to_string(),
            _ => "The user cancelled the question.".to_string(),
        };
        let message = genai::chat::ChatMessage::from(genai::chat::ToolResponse::new(call, result));
        let guard = lock_store(store);
        let _ = guard.record_message(session, &message, crate::session::MessageStatus::Complete);
    }
}

pub async fn run_agent_worker(
    request: TurnRequest,
    tx: mpsc::UnboundedSender<Action>,
    is_first_turn: bool,
    mode: AgentMode,
    session_context: Option<String>,
) {
    tokio::spawn(async move {
        let _done = DoneGuard(tx.clone());
        let security = crate::agent::security::WorkspaceSecurity::new(".");
        let config = crate::agent::engine::AgentConfig::load(security.root());
        let runtime = crate::agent::engine::AgentRuntime::new(config.clone(), security);

        let TurnRequest {
            prompt,
            session_id,
            turn_id,
            mut store,
            cancel,
        } = request;
        let mut session_id = session_id;

        // The user row and the history for this request, off the UI thread.
        let mut history = Vec::new();
        if let Some(handle) = store.clone() {
            let prepared = {
                let guard = lock_store(&handle);
                crate::session::prepare_turn(
                    &guard,
                    &crate::session::workspace_key(),
                    &config.model,
                    session_id,
                    &prompt,
                )
            };
            match prepared {
                Ok((id, replayed)) => {
                    session_id = Some(id);
                    history = replayed;
                    let _ = tx.send(Action::SessionStarted(id));
                }
                Err(error) => {
                    let _ = tx.send(Action::SessionFailed(format!(
                        "Session history disabled: {error}"
                    )));
                    store = None;
                }
            }
        }

        // Generate quick concise title only on the first turn of the session
        if is_first_turn {
            let title_tx = tx.clone();
            let title_session = session_id;
            let title_prompt = prompt.clone();
            let client_clone = runtime.client().clone();
            let model_name = config.model.clone();
            let title_options = runtime.chat_options();
            tokio::spawn(async move {
                let req = genai::chat::ChatRequest::from_user(format!(
                    "Generate a very short 3-5 word concise title for this task without punctuation or quotes:\n{}",
                    title_prompt
                ));
                if let Ok(res) = client_clone
                    .exec_chat(&model_name, req, Some(&title_options))
                    .await
                    && let Some(text) = res.into_first_text()
                {
                    let clean = crate::util::unquote(text.trim()).to_string();
                    if !clean.is_empty() {
                        let _ = title_tx.send(Action::AgentTitle {
                            title: clean,
                            session: title_session,
                        });
                    }
                }
            });
        }

        let tx_clone = tx.clone();
        let row_store = store.clone();
        let row_tx = tx.clone();
        let event_store = store.clone();
        let event_tx = tx.clone();
        // Set by the first streamed token: the answer is already on screen, so
        // the turn must not append it a second time.
        let streamed = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let streamed_evt = streamed.clone();
        let runtime = runtime.with_questioner(std::sync::Arc::new(super::TuiQuestioner::new(
            tx.clone(),
            store.clone(),
            session_id,
            turn_id,
        )));
        let task = crate::tui::compact::build_agent_task(&prompt, session_context.as_deref());
        let result = runtime
            .execute_task(
                &task,
                mode,
                history,
                &TuiConfirmer::new(tx.clone()),
                &cancel,
                move |event| {
                    // The durable subset is written here, on the worker task:
                    // the database is never touched by the render thread.
                    if let (Some(handle), Some(id)) = (&event_store, session_id)
                        && let Some(row) = crate::session::NewEvent::from_event(turn_id, &event)
                    {
                        let guard = lock_store(handle);
                        if let Err(error) = guard.record_event(id, row) {
                            let _ = event_tx.send(Action::SessionFailed(format!(
                                "Session history disabled: {error}"
                            )));
                        }
                    }
                    let action = match event {
                        AgentEvent::Token(token) => {
                            let first =
                                !streamed_evt.swap(true, std::sync::atomic::Ordering::Relaxed);
                            Some(Action::AgentToken(if first {
                                format!("\n\n{token}")
                            } else {
                                token
                            }))
                        }
                        AgentEvent::Thinking(thoughts) => Some(Action::AgentThinking(thoughts)),
                        AgentEvent::Usage {
                            total_tokens,
                            prompt_tokens,
                        } => Some(Action::AgentUsage {
                            total_tokens,
                            prompt_tokens,
                        }),
                        AgentEvent::Tool(tool) => Some(Action::AgentTool(tool)),
                        AgentEvent::Notice(note) => {
                            Some(Action::AgentToken(format!("\n  {note}\n")))
                        }
                        // The turn's terminal status is sent from the outcome
                        // right after this call returns, once, with its cleanup.
                        AgentEvent::Turn { .. } => None,
                    };
                    if let Some(action) = action {
                        let _ = tx_clone.send(action);
                    }
                },
                // The single persistence channel: every appended message, once.
                move |message, status| {
                    let (Some(handle), Some(id)) = (&row_store, session_id) else {
                        return;
                    };
                    let guard = lock_store(handle);
                    if let Err(error) = guard.record_message(id, message, status) {
                        let _ = row_tx.send(Action::SessionFailed(format!(
                            "Session history disabled: {error}"
                        )));
                    }
                },
            )
            .await;

        let _ = tx.send(Action::TurnFinished {
            status: result.status,
            cleanup: result.cleanup,
            pid: result.pid,
        });

        match result.result {
            Ok(answer) => {
                if streamed.load(std::sync::atomic::Ordering::Relaxed) {
                    let _ = tx.send(Action::AgentToken("\n".to_string()));
                } else {
                    let clean_answer = answer.trim();
                    if !clean_answer.is_empty() {
                        let _ = tx.send(Action::AgentToken(format!("\n\n{clean_answer}\n")));
                    }
                }
            }
            // A cancelled turn already said so through its status; the error it
            // carries is the same news, and printing both reads as a failure.
            Err(_) if result.status == crate::agent::cancel::TurnStatus::Cancelled => {}
            Err(e) => {
                let _ = tx.send(Action::AgentToken(format!("\n\nError: {e}\n")));
            }
        }
    });
}
