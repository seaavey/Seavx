use crossterm::event::KeyCode;
use tokio::sync::mpsc;

use super::store::lock_store;
use super::{App, TuiConfirmer, TurnRequest};
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
        // Set by the first streamed token: the answer is already on screen, so
        // the turn must not append it a second time.
        let streamed = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let streamed_evt = streamed.clone();
        let task = crate::tui::compact::build_agent_task(&prompt, session_context.as_deref());
        let result = runtime
            .execute_task(
                &task,
                mode,
                history,
                &TuiConfirmer::new(tx.clone()),
                &cancel,
                move |event| {
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
