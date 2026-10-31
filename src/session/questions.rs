//! Clarifying questions: asked, resolved, listed.
use super::*;

impl SessionStore {
    // ---- clarifying questions ---------------------------------------------

    /// Persist a pending question and its `question_asked` event together, before
    /// anyone is shown it.
    // The argument list is the question's own shape; a struct with one caller
    // would only rename the same fields.
    #[allow(clippy::too_many_arguments)]
    pub fn ask_question(
        &self,
        workspace: &str,
        session: Option<i64>,
        turn_id: u64,
        tool_call_id: &str,
        question: &str,
        kind: QuestionKind,
        options: &[String],
    ) -> Result<i64, SessionError> {
        let text = crate::agent::security::redact(question);
        let labels: Vec<String> = options
            .iter()
            .map(|option| crate::agent::security::redact(option))
            .collect();
        let total = text.len() + labels.iter().map(String::len).sum::<usize>();
        if total > QUESTION_LIMIT {
            return Err(SessionError::Refused(format!(
                "the question and its options exceed {QUESTION_LIMIT} bytes"
            )));
        }
        let options_json = serde_json::to_string(&labels).unwrap_or_else(|_| "[]".to_string());
        let now = crate::util::epoch_secs() as i64;
        self.write(|connection| {
            connection.execute_batch("BEGIN IMMEDIATE")?;
            let created = (|| {
                connection.execute(
                    "INSERT INTO pending_questions
                     (workspace, session_id, turn_id, tool_call_id, question, kind, options, state, created_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'pending', ?8)",
                    params![
                        workspace,
                        session,
                        turn_id as i64,
                        tool_call_id,
                        text,
                        kind.as_str(),
                        options_json,
                        now
                    ],
                )?;
                let id = connection.last_insert_rowid();
                log_question_event(connection, session, id, "pending", tool_call_id, &text, now)?;
                Ok(id)
            })();
            finish(connection, created)
        })?
    }

    pub fn question(&self, workspace: &str, id: i64) -> Result<PendingQuestion, SessionError> {
        let question = self
            .connection
            .query_row(
                "SELECT id, workspace, session_id, turn_id, tool_call_id, question, kind,
                        options, state, answer
                 FROM pending_questions WHERE id = ?1 AND workspace = ?2",
                params![id, workspace],
                question_row,
            )
            .optional()?
            .ok_or(SessionError::Unknown(id))?;
        Ok(question)
    }

    /// Every unanswered question of this workspace, oldest first.
    pub fn open_questions(&self, workspace: &str) -> Result<Vec<PendingQuestion>, SessionError> {
        let mut statement = self.connection.prepare(
            "SELECT id, workspace, session_id, turn_id, tool_call_id, question, kind,
                    options, state, answer
             FROM pending_questions WHERE workspace = ?1 AND state = 'pending'
             ORDER BY id",
        )?;
        let rows = statement.query_map([workspace], question_row)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// Resolve once. A second call returns the stored outcome and writes nothing,
    /// so a retried turn cannot append a second decision.
    pub fn resolve_question(
        &self,
        workspace: &str,
        id: i64,
        state: QuestionState,
        answer: &str,
    ) -> Result<PendingQuestion, SessionError> {
        if state == QuestionState::Pending {
            return Err(SessionError::Refused(
                "a question cannot resolve to pending".to_string(),
            ));
        }
        let answer = crate::agent::events::bounded(
            &crate::agent::security::redact(answer),
            crate::agent::events::SUMMARY_LIMIT,
        );
        let now = crate::util::epoch_secs() as i64;
        self.write(|connection| {
            connection.execute_batch("BEGIN IMMEDIATE")?;
            let resolved = (|| {
                let current: Option<String> = connection
                    .query_row(
                        "SELECT state FROM pending_questions WHERE id = ?1 AND workspace = ?2",
                        params![id, workspace],
                        |row| row.get(0),
                    )
                    .optional()?;
                let Some(current) = current else {
                    return Err(SessionError::Unknown(id));
                };
                if current != "pending" {
                    // Already resolved: the stored outcome stands, no new event.
                    return Ok(());
                }
                connection.execute(
                    "UPDATE pending_questions SET state = ?3, answer = ?4, resolved_at = ?5
                     WHERE id = ?1 AND workspace = ?2",
                    params![id, workspace, state.as_str(), answer, now],
                )?;
                let session: Option<i64> = connection.query_row(
                    "SELECT session_id FROM pending_questions WHERE id = ?1",
                    [id],
                    |row| row.get(0),
                )?;
                let call: String = connection.query_row(
                    "SELECT tool_call_id FROM pending_questions WHERE id = ?1",
                    [id],
                    |row| row.get(0),
                )?;
                log_question_event(connection, session, id, state.as_str(), &call, &answer, now)?;
                Ok(())
            })();
            finish(connection, resolved)
        })??;
        self.question(workspace, id)
    }

    /// Calls of this session whose question is still open: replay must not
    /// invent an interrupted result for them.
    pub fn pending_call_ids(&self, session: i64) -> Result<Vec<String>, SessionError> {
        let mut statement = self.connection.prepare(
            "SELECT tool_call_id FROM pending_questions
             WHERE session_id = ?1 AND state = 'pending'",
        )?;
        let rows = statement.query_map([session], |row| row.get(0))?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }
}
