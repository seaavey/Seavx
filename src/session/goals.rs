//! Goals, their criteria and tasks, and the events they log.
use super::*;

impl SessionStore {
    // ---- goals -------------------------------------------------------------

    /// A new goal is a `draft`: nothing runs until the user approves it.
    pub fn create_goal(
        &self,
        workspace: &str,
        session: Option<i64>,
        objective: &str,
    ) -> Result<i64, SessionError> {
        let now = crate::util::epoch_secs() as i64;
        let objective = crate::agent::events::bounded(
            &crate::agent::security::redact(objective),
            crate::agent::events::SUMMARY_LIMIT,
        );
        if objective.trim().is_empty() {
            return Err(SessionError::Refused(
                "a goal needs an objective".to_string(),
            ));
        }
        self.write(|connection| {
            connection.execute_batch("BEGIN IMMEDIATE")?;
            let created = (|| {
                connection.execute(
                    "INSERT INTO goals(workspace, session_id, objective, status, created_at, updated_at)
                     VALUES (?1, ?2, ?3, 'draft', ?4, ?4)",
                    params![workspace, session, objective, now],
                )?;
                let id = connection.last_insert_rowid();
                log_goal_event(connection, id, session, "goal", "draft", &objective, now)?;
                Ok(id)
            })();
            finish(connection, created)
        })?
    }

    pub fn goal(&self, workspace: &str, id: i64) -> Result<Goal, SessionError> {
        let row = self
            .connection
            .query_row(
                "SELECT id, workspace, session_id, objective, status FROM goals
                 WHERE id = ?1 AND workspace = ?2",
                params![id, workspace],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<i64>>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                    ))
                },
            )
            .optional()?
            .ok_or(SessionError::Unknown(id))?;

        let mut criteria_stmt = self.connection.prepare(
            "SELECT position, text, status, evidence, waiver_reason FROM goal_criteria
             WHERE goal_id = ?1 ORDER BY position",
        )?;
        let criteria = criteria_stmt
            .query_map([id], |row| {
                Ok(Criterion {
                    position: row.get(0)?,
                    text: row.get(1)?,
                    state: CriterionState::parse(&row.get::<_, String>(2)?),
                    evidence: row.get(3)?,
                    waiver_reason: row.get(4)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;

        let mut task_stmt = self.connection.prepare(
            "SELECT position, title, status, evidence FROM agent_tasks
             WHERE goal_id = ?1 ORDER BY position",
        )?;
        let tasks = task_stmt
            .query_map([id], |row| {
                Ok(GoalTask {
                    position: row.get(0)?,
                    title: row.get(1)?,
                    state: TaskState::parse(&row.get::<_, String>(2)?),
                    evidence: row.get(3)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;

        Ok(Goal {
            id: row.0,
            workspace: row.1,
            session_id: row.2,
            objective: row.3,
            state: GoalState::parse(&row.4),
            criteria,
            tasks,
        })
    }

    /// Every goal in one workspace, newest first.
    pub fn goals(&self, workspace: &str) -> Result<Vec<Goal>, SessionError> {
        let mut statement = self.connection.prepare(
            "SELECT id FROM goals WHERE workspace = ?1 ORDER BY updated_at DESC, id DESC",
        )?;
        let ids: Vec<i64> = statement
            .query_map([workspace], |row| row.get(0))?
            .collect::<Result<_, _>>()?;
        drop(statement);
        ids.into_iter().map(|id| self.goal(workspace, id)).collect()
    }

    /// Replace a draft's objective. Only a draft is editable.
    pub fn set_goal_objective(
        &self,
        workspace: &str,
        id: i64,
        objective: &str,
    ) -> Result<(), SessionError> {
        let now = crate::util::epoch_secs() as i64;
        let objective = crate::agent::events::bounded(
            &crate::agent::security::redact(objective),
            crate::agent::events::SUMMARY_LIMIT,
        );
        if objective.trim().is_empty() {
            return Err(SessionError::Refused(
                "a goal needs an objective".to_string(),
            ));
        }
        self.write(|connection| {
            connection.execute_batch("BEGIN IMMEDIATE")?;
            let written = (|| {
                let state = goal_state(connection, workspace, id)?;
                if state != GoalState::Draft {
                    return Err(SessionError::Refused(format!(
                        "goal {id} is {}; only a draft can be edited",
                        state.as_str()
                    )));
                }
                connection.execute(
                    "UPDATE goals SET objective = ?2, updated_at = ?3 WHERE id = ?1",
                    params![id, objective, now],
                )?;
                log_goal_event(
                    connection,
                    id,
                    None,
                    "goal",
                    "draft",
                    "objective edited",
                    now,
                )?;
                Ok(())
            })();
            finish(connection, written)
        })?
    }

    /// Replace the draft's criteria and ordered tasks. Only a draft is editable.
    pub fn set_goal_plan(
        &self,
        workspace: &str,
        id: i64,
        criteria: &[String],
        tasks: &[String],
    ) -> Result<(), SessionError> {
        let now = crate::util::epoch_secs() as i64;
        self.write(|connection| {
            connection.execute_batch("BEGIN IMMEDIATE")?;
            let written = (|| {
                let state = goal_state(connection, workspace, id)?;
                if state != GoalState::Draft {
                    return Err(SessionError::Refused(format!(
                        "goal {id} is {}; only a draft can be edited",
                        state.as_str()
                    )));
                }
                connection.execute("DELETE FROM goal_criteria WHERE goal_id = ?1", [id])?;
                connection.execute("DELETE FROM agent_tasks WHERE goal_id = ?1", [id])?;
                for (position, text) in criteria.iter().enumerate() {
                    connection.execute(
                        "INSERT INTO goal_criteria(goal_id, position, text, status)
                         VALUES (?1, ?2, ?3, 'pending')",
                        params![id, position as i64, text.trim()],
                    )?;
                }
                for (position, title) in tasks.iter().enumerate() {
                    connection.execute(
                        "INSERT INTO agent_tasks(goal_id, position, title, status, updated_at)
                         VALUES (?1, ?2, ?3, 'pending', ?4)",
                        params![id, position as i64, title.trim(), now],
                    )?;
                }
                connection.execute(
                    "UPDATE goals SET updated_at = ?2 WHERE id = ?1",
                    params![id, now],
                )?;
                log_goal_event(
                    connection,
                    id,
                    None,
                    "goal",
                    "draft",
                    &format!("{} criteria, {} tasks", criteria.len(), tasks.len()),
                    now,
                )?;
                Ok(())
            })();
            finish(connection, written)
        })?
    }

    /// Move a goal. The user is the only one who can approve it, and completion
    /// is refused while a criterion or task is still open.
    pub fn set_goal_state(
        &self,
        workspace: &str,
        id: i64,
        next: GoalState,
    ) -> Result<(), SessionError> {
        let now = crate::util::epoch_secs() as i64;
        self.write(|connection| {
            connection.execute_batch("BEGIN IMMEDIATE")?;
            let moved = (|| {
                let state = goal_state(connection, workspace, id)?;
                if !state.allows(next) {
                    return Err(SessionError::Refused(format!(
                        "goal {id} cannot go from {} to {}",
                        state.as_str(),
                        next.as_str()
                    )));
                }
                if next.is_live()
                    && let Some(other) = conflicting_goal(connection, workspace, id)?
                {
                    return Err(SessionError::Refused(format!(
                        "goal {other} is already active in this workspace"
                    )));
                }
                if next == GoalState::Completed {
                    let goal = self.goal(workspace, id)?;
                    if let Some(blocker) = goal.completion_blocker() {
                        return Err(SessionError::Refused(format!(
                            "goal {id} cannot be completed: {blocker}"
                        )));
                    }
                }
                connection.execute(
                    "UPDATE goals SET status = ?2, updated_at = ?3 WHERE id = ?1",
                    params![id, next.as_str(), now],
                )?;
                log_goal_event(
                    connection,
                    id,
                    None,
                    "goal",
                    next.as_str(),
                    &format!("goal {}", next.as_str()),
                    now,
                )?;
                Ok(())
            })();
            finish(connection, moved)
        })?
    }

    /// A task is complete only with evidence attached.
    pub fn set_task_state(
        &self,
        workspace: &str,
        id: i64,
        position: i64,
        next: TaskState,
        evidence: &str,
    ) -> Result<(), SessionError> {
        let now = crate::util::epoch_secs() as i64;
        let evidence = crate::agent::events::bounded(
            &crate::agent::security::redact(evidence),
            crate::agent::events::SUMMARY_LIMIT,
        );
        self.write(|connection| {
            connection.execute_batch("BEGIN IMMEDIATE")?;
            let moved = (|| {
                goal_state(connection, workspace, id)?;
                if next == TaskState::Completed && evidence.trim().is_empty() {
                    return Err(SessionError::Refused(format!(
                        "task {} cannot be completed without evidence",
                        position + 1
                    )));
                }
                let changed = connection.execute(
                    "UPDATE agent_tasks SET status = ?3, evidence = ?4, updated_at = ?5
                     WHERE goal_id = ?1 AND position = ?2",
                    params![id, position, next.as_str(), evidence, now],
                )?;
                if changed == 0 {
                    return Err(SessionError::Unknown(id));
                }
                log_goal_event(
                    connection,
                    id,
                    None,
                    "task",
                    next.as_str(),
                    &format!("task {} {}", position + 1, next.as_str()),
                    now,
                )?;
                Ok(())
            })();
            finish(connection, moved)
        })?
    }

    /// Satisfy a criterion with evidence, or waive it with the user's reason.
    pub fn set_criterion_state(
        &self,
        workspace: &str,
        id: i64,
        position: i64,
        next: CriterionState,
        evidence: &str,
        waiver: Option<&str>,
    ) -> Result<(), SessionError> {
        let now = crate::util::epoch_secs() as i64;
        let evidence = crate::agent::events::bounded(
            &crate::agent::security::redact(evidence),
            crate::agent::events::SUMMARY_LIMIT,
        );
        let waiver = waiver.map(|reason| {
            crate::agent::events::bounded(
                &crate::agent::security::redact(reason),
                crate::agent::events::SUMMARY_LIMIT,
            )
        });
        self.write(|connection| {
            connection.execute_batch("BEGIN IMMEDIATE")?;
            let moved = (|| {
                goal_state(connection, workspace, id)?;
                match next {
                    CriterionState::Satisfied if evidence.trim().is_empty() => {
                        return Err(SessionError::Refused(format!(
                            "criterion {} cannot be satisfied without evidence",
                            position + 1
                        )));
                    }
                    CriterionState::Waived
                        if waiver
                            .as_deref()
                            .is_none_or(|reason| reason.trim().is_empty()) =>
                    {
                        return Err(SessionError::Refused(format!(
                            "criterion {} needs a reason to be waived",
                            position + 1
                        )));
                    }
                    _ => {}
                }
                let changed = connection.execute(
                    "UPDATE goal_criteria
                     SET status = ?3, evidence = ?4, waiver_reason = ?5
                     WHERE goal_id = ?1 AND position = ?2",
                    params![id, position, next.as_str(), evidence, waiver],
                )?;
                if changed == 0 {
                    return Err(SessionError::Unknown(id));
                }
                log_goal_event(
                    connection,
                    id,
                    None,
                    "goal",
                    next.as_str(),
                    &format!("criterion {} {}", position + 1, next.as_str()),
                    now,
                )?;
                Ok(())
            })();
            finish(connection, moved)
        })?
    }

    /// Append one trace row and return its per-session `seq`. `seq` is allocated
    /// inside the insert's transaction, so two writers cannot share a number.
    pub fn record_event(&self, session: i64, event: NewEvent<'_>) -> Result<i64, SessionError> {
        let now = crate::util::epoch_secs() as i64;
        // The last gate before the database: a caller that forgot to redact
        // cannot store a credential or an unbounded payload.
        let summary = crate::agent::events::bounded(
            &crate::agent::security::redact(event.summary),
            crate::agent::events::SUMMARY_LIMIT,
        );
        self.write(|connection| {
            connection.execute_batch("BEGIN IMMEDIATE")?;
            let inserted = (|| {
                connection.execute(
                    "INSERT INTO agent_events(session_id, seq, turn_id, kind, status, failure, tool_call_id, summary, pid, created_at)
                     SELECT ?1, COALESCE(MAX(seq), 0) + 1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9
                     FROM agent_events WHERE session_id = ?1",
                    params![
                        session,
                        event.turn_id,
                        event.kind,
                        event.status,
                        event.failure,
                        event.tool_call_id,
                        summary,
                        event.pid,
                        now
                    ],
                )?;
                connection.query_row(
                    "SELECT seq FROM agent_events WHERE id = last_insert_rowid()",
                    [],
                    |row| row.get(0),
                )
            })();
            match inserted {
                Ok(seq) => {
                    connection.execute_batch("COMMIT")?;
                    Ok(seq)
                }
                Err(error) => {
                    let _ = connection.execute_batch("ROLLBACK");
                    Err(error)
                }
            }
        })
    }

    /// The stored trace for one session, in database order.
    pub fn events(&self, session: i64) -> Result<Vec<StoredEvent>, SessionError> {
        let mut statement = self.connection.prepare(
            "SELECT seq, turn_id, kind, status, failure, tool_call_id, summary, pid, created_at
             FROM agent_events WHERE session_id = ?1 ORDER BY seq",
        )?;
        let rows = statement.query_map([session], |row| {
            Ok(StoredEvent {
                seq: row.get(0)?,
                turn_id: row.get(1)?,
                kind: row.get(2)?,
                status: row.get(3)?,
                failure: row.get(4)?,
                tool_call_id: row.get(5)?,
                summary: row.get(6)?,
                pid: row.get(7)?,
                created_at: row.get(8)?,
            })
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }
}
