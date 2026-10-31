use super::{App, PendingQuestion, TurnRequest, new_input};
use crate::tui::event::AgentState;

impl App<'_> {
    /// Attach the store, or record the one notice and keep running unpersisted.
    /// A corrupt or newer database never fails the TUI.
    pub fn open_store(
        &mut self,
        store: Result<crate::session::SessionStore, crate::session::SessionError>,
    ) {
        match store {
            Ok(store) => self.attach_store(std::sync::Arc::new(std::sync::Mutex::new(store))),
            Err(error) => {
                let notice = format!("Session history disabled: {error}");
                self.persistence_error = Some(notice.clone());
                self.push_notice(notice);
            }
        }
    }

    /// Hand the app the store it writes through, or the run is unpersisted.
    pub fn attach_store(
        &mut self,
        store: std::sync::Arc<std::sync::Mutex<crate::session::SessionStore>>,
    ) {
        self.store = Some(store);
    }

    /// Rebuild the transcript from stored rows, prefill the pending prompt and
    /// report what the resume budget left out.
    pub fn load_session(&mut self, session: crate::session::LoadedSession) {
        // The load replaces the screen: the rows on it and the compaction
        // summary belong to whatever session was open before this one.
        self.clear_transcript();
        self.read_only = None;
        self.session_id = Some(session.id);
        self.session_title = session.title.clone();
        self.total_tokens = session.total_tokens as i32;
        self.prompt_tokens = session.prompt_tokens as i32;
        if let Some(reason) = session.read_only.clone() {
            self.read_only = Some(reason.clone());
            self.push_notice(format!("Session {} is read-only: {reason}", session.id));
        }

        let outcome = crate::session::replay(&session);
        let body = crate::session::render_messages(&outcome.history);
        if !body.is_empty() {
            self.push_text(body.trim_end());
            self.push_text("\n\n");
        }
        if outcome.omitted > 0 {
            self.push_notice(format!(
                "System: {} older messages omitted (resume budget).",
                outcome.omitted
            ));
        }
        if let Some(draft) = outcome.draft {
            self.push_text(&format!("User: {draft} (pending, not sent)\n\n"));
            self.set_input(&draft);
        } else {
            // A draft from the session that was open would otherwise ride along.
            self.input = new_input();
        }
        self.scroll = self.max_scroll();
        self.auto_scroll = true;

        // An open question comes back with the session. It is not an
        // interruption: answering it appends the one result for its call.
        self.pending_question = None;
        if !session.pending_calls.is_empty()
            && let Some(handle) = self.store.clone()
        {
            let workspace = crate::session::workspace_key();
            let open = {
                let guard = lock_store(&handle);
                guard
                    .open_questions(&workspace)
                    .unwrap_or_default()
                    .into_iter()
                    .find(|question| question.session_id == Some(session.id))
            };
            if let Some(question) = open {
                self.pending_question = Some(PendingQuestion {
                    id: question.id,
                    question: question.question.clone(),
                    kind: question.kind,
                    options: question.options.clone(),
                    selected: 0,
                    chosen: std::collections::BTreeSet::new(),
                    text: String::new(),
                    reply: None,
                });
                self.push_notice(format!(
                    "Restored question {}: {}",
                    question.id, question.question
                ));
            }
        }
    }

    /// `/clear` and `/new`: a fresh session id, transcript and compaction
    /// context; nothing stored is deleted.
    pub fn start_new_session(&mut self) {
        self.clear_transcript();
        self.session_title = None;
        self.session_id = None;
        self.read_only = None;
        // The compaction context belongs to the session that produced it.
        self.session_context = None;
        // Usage is per session too, or the footer keeps the old count.
        self.total_tokens = 0;
        self.prompt_tokens = 0;
    }

    /// Show the prompt and hand the worker everything it needs. The user row is
    /// written by the worker, so the UI thread never waits on SQLite.
    pub fn submit_turn(&mut self, prompt: &str) -> Option<TurnRequest> {
        let prompt = prompt.trim().to_string();
        // An image alone is a turn: only a turn with neither half is a no-op.
        if prompt.is_empty() && self.pending_attachments.is_empty() {
            return None;
        }
        if let Some(reason) = self.read_only.clone() {
            self.push_notice(format!(
                "This session is read-only ({reason}). Use /clear to start a new one."
            ));
            return None;
        }
        // One turn at a time: a second request would replace `self.cancel` and
        // leave the running turn with a token nobody can reach.
        if self.status != AgentState::Idle {
            self.push_notice("A task is already running. Press Esc to cancel it first.");
            return None;
        }

        if !prompt.is_empty() {
            self.remember_prompt(&prompt);
        }
        let attachments = std::mem::take(&mut self.pending_attachments);
        if !self.segments.is_empty() {
            self.push_text("\n\n");
        }
        let mut row = format!("User: {prompt}");
        for (index, attachment) in attachments.iter().enumerate() {
            row.push_str(&format!("\n[{}]", attachment.summary(index + 1)));
        }
        self.push_text(&format!("{row}\n\n"));
        self.scroll = self.max_scroll();
        self.auto_scroll = true;
        self.status = AgentState::Thinking;
        self.input = new_input();
        self.reset_history_nav();
        // A cancelled turn must not poison the next one.
        self.cancel = crate::agent::cancel::CancelToken::new();
        // Only an accepted turn takes a number, so a refusal above cannot leave
        // a gap between the transcript and the stored trace.
        self.turn_id += 1;

        Some(TurnRequest {
            steer: self.steer.clone(),
            prompt,
            session_id: self.session_id,
            turn_id: self.turn_id,
            store: self.store.clone(),
            cancel: self.cancel.clone(),
            attachments,
        })
    }

    /// Show a title only for the session that asked for it. A title request is
    /// spawned, so its answer can arrive after the user started another session;
    /// renaming that one would be wrong.
    pub fn apply_title(&mut self, session: Option<i64>, title: String) -> bool {
        if session != self.session_id {
            return false;
        }
        self.session_title = Some(title);
        true
    }
}

/// The store is behind a mutex: the connection is `Send` but not `Sync`, and
/// every write holds the lock for one insert.
pub(super) fn lock_store(
    store: &std::sync::Arc<std::sync::Mutex<crate::session::SessionStore>>,
) -> std::sync::MutexGuard<'_, crate::session::SessionStore> {
    store
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}
