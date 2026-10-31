//! Session lifecycle, stored messages, turns and the one write path.
use super::*;
use genai::chat::{ChatMessage, ToolResponse};
use rusqlite::{Connection, params};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

impl SessionStore {
    pub fn path() -> Option<PathBuf> {
        if crate::util::is_debug() {
            return Some(PathBuf::from(".seavx/sessions.db"));
        }
        crate::home::dir().map(|home| home.join(".seavx/sessions.db"))
    }

    pub fn open() -> Result<Self, SessionError> {
        let path = Self::path()
            .ok_or_else(|| SessionError::Unavailable("home directory is not set".to_string()))?;
        Self::open_at(&path)
    }

    /// Open and migrate. A newer `user_version` is refused before any write, so a
    /// build that does not understand the file never touches it.
    pub fn open_at(path: &Path) -> Result<Self, SessionError> {
        if let Some(directory) = path.parent() {
            crate::util::ensure_private_dir(directory, 0o700)
                .map_err(|error| SessionError::Unavailable(error.to_string()))?;
        }

        let connection = Connection::open(path)?;
        let found: u32 = connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .map_err(|error| SessionError::Unavailable(error.to_string()))?;
        if found > SCHEMA_VERSION {
            return Err(SessionError::Newer {
                found,
                supported: SCHEMA_VERSION,
            });
        }

        crate::util::ensure_private_file(path, 0o600)
            .map_err(|error| SessionError::Unavailable(error.to_string()))?;
        connection.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL; PRAGMA foreign_keys=ON; PRAGMA busy_timeout=2000;",
        )?;
        // SQLite creates the WAL and its index itself, with the umask's mode.
        // They carry the same payloads as the database, so an existing one is
        // made owner-only, before and after the migration writes through it.
        tighten_sidecars(path);

        let store = Self { connection };
        store.migrate()?;
        tighten_sidecars(path);
        Ok(store)
    }

    /// Create the schema in one transaction and bump `user_version` only after it
    /// commits; a second call is a no-op.
    pub fn migrate(&self) -> Result<(), SessionError> {
        let version: u32 = self
            .connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if version == SCHEMA_VERSION {
            return Ok(());
        }

        let transaction = self.connection.unchecked_transaction()?;
        transaction.execute_batch(SCHEMA)?;

        // A v1 file already has the table, so `CREATE TABLE IF NOT EXISTS`
        // could not add the column: alter it, then name every existing row.
        let mut columns = transaction.prepare("PRAGMA table_info(sessions)")?;
        let names: Vec<String> = columns
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<Result<_, _>>()?;
        drop(columns);
        if !names.iter().any(|name| name == "slug") {
            transaction
                .execute_batch("ALTER TABLE sessions ADD COLUMN slug TEXT NOT NULL DEFAULT ''")?;
        }
        // v6 columns: nullable, so every existing row keeps its meaning.
        for (column, sql) in [
            (
                "parent_session_id",
                "ALTER TABLE sessions ADD COLUMN parent_session_id INTEGER",
            ),
            (
                "fork_seq",
                "ALTER TABLE sessions ADD COLUMN fork_seq INTEGER",
            ),
            (
                "archived_at",
                "ALTER TABLE sessions ADD COLUMN archived_at INTEGER",
            ),
        ] {
            if !names.iter().any(|name| name == column) {
                transaction.execute_batch(sql)?;
            }
        }
        // After the column, never in `SCHEMA`: an index on a column the v1
        // table does not have yet would fail the whole batch.
        transaction.execute_batch(
            "CREATE UNIQUE INDEX IF NOT EXISTS sessions_slug ON sessions(slug) WHERE slug <> ''",
        )?;

        // v3's ledger has no `goal_id` and a `kind` CHECK that refused goal
        // events. SQLite cannot alter either in place, so the table is rebuilt
        // row for row inside this transaction, ids and sequence intact.
        let mut ledger_columns = transaction.prepare("PRAGMA table_info(agent_events)")?;
        let ledger_names: Vec<String> = ledger_columns
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<Result<_, _>>()?;
        drop(ledger_columns);
        // v5 admits a `question` event; the CHECK is part of the table, so the
        // rebuild runs again for a v4 file.
        let ledger_ddl: String = transaction.query_row(
            "SELECT COALESCE((SELECT sql FROM sqlite_master WHERE name = 'agent_events'), '')",
            [],
            |row| row.get(0),
        )?;
        let needs_rebuild = !ledger_names.iter().any(|name| name == "goal_id")
            || !ledger_ddl.contains("'question'");
        if needs_rebuild {
            transaction.execute_batch(
                "ALTER TABLE agent_events RENAME TO agent_events_v3;
                 CREATE TABLE agent_events(
                     id INTEGER PRIMARY KEY AUTOINCREMENT,
                     session_id INTEGER REFERENCES sessions(id) ON DELETE CASCADE,
                     goal_id INTEGER REFERENCES goals(id) ON DELETE CASCADE,
                     seq INTEGER NOT NULL,
                     turn_id INTEGER NOT NULL,
                     kind TEXT NOT NULL
                         CHECK(kind IN ('tool','turn','goal','task','question')),
                     status TEXT NOT NULL,
                     failure TEXT,
                     tool_call_id TEXT,
                     summary TEXT NOT NULL DEFAULT '',
                     pid INTEGER,
                     created_at INTEGER NOT NULL,
                     UNIQUE(session_id, seq)
                 );
                 INSERT INTO agent_events(id, session_id, goal_id, seq, turn_id, kind,
                                          status, failure, tool_call_id, summary, pid, created_at)
                     SELECT id, session_id, NULL, seq, turn_id, kind, status, failure,
                            tool_call_id, summary, pid, created_at
                     FROM agent_events_v3;
                 DROP TABLE agent_events_v3;
                 CREATE INDEX IF NOT EXISTS agent_events_session_seq
                     ON agent_events(session_id, seq);
                 CREATE UNIQUE INDEX IF NOT EXISTS agent_events_goal_seq
                     ON agent_events(goal_id, seq) WHERE goal_id IS NOT NULL;",
            )?;
        }
        // After the rebuild, never in `SCHEMA`: on a v3 file the column does not
        // exist yet, and the whole batch would fail on the index alone.
        transaction.execute_batch(
            "CREATE UNIQUE INDEX IF NOT EXISTS agent_events_goal_seq
                 ON agent_events(goal_id, seq) WHERE goal_id IS NOT NULL;",
        )?;

        let mut unnamed = transaction.prepare("SELECT id FROM sessions WHERE slug = ''")?;
        let pending: Vec<i64> = unnamed
            .query_map([], |row| row.get(0))?
            .collect::<Result<_, _>>()?;
        drop(unnamed);
        for id in pending {
            transaction.execute(
                "UPDATE sessions SET slug = ?1 WHERE id = ?2",
                params![new_slug(), id],
            )?;
        }

        transaction.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        transaction.commit()?;
        Ok(())
    }

    /// The connection behind every later read and write.
    pub fn connection(&self) -> &Connection {
        &self.connection
    }

    /// Insert a session row and return its id.
    pub fn create(
        &self,
        workspace: &str,
        model: Option<&str>,
        title: Option<&str>,
    ) -> Result<i64, SessionError> {
        let now = crate::util::epoch_secs() as i64;
        self.write(|connection| {
            // The unique index turns a collision into an error instead of two
            // sessions sharing a name; one retry with a fresh slug is enough.
            let mut slug = new_slug();
            let mut inserted = insert_session(connection, &slug, workspace, title, model, now);
            if let Err(rusqlite::Error::SqliteFailure(error, _)) = &inserted
                && error.code == rusqlite::ErrorCode::ConstraintViolation
            {
                slug = new_slug();
                inserted = insert_session(connection, &slug, workspace, title, model, now);
            }
            inserted?;
            Ok(connection.last_insert_rowid())
        })
    }

    // ---- fork, archive, search --------------------------------------------

    /// Copy the source session's prefix into a new independent session.
    ///
    /// `boundary` is the last message `seq` to copy; `None` means the newest
    /// complete turn. The source is only read: its rows, slug and timestamps are
    /// untouched, and a refusal happens before anything is written.
    pub fn fork(
        &self,
        workspace: &str,
        source: i64,
        boundary: Option<i64>,
    ) -> Result<i64, SessionError> {
        let now = crate::util::epoch_secs() as i64;
        self.write(|connection| {
            connection.execute_batch("BEGIN IMMEDIATE")?;
            let forked = (|| {
                let (owner, title, model): (String, Option<String>, Option<String>) = connection
                    .query_row(
                        "SELECT workspace, title, model FROM sessions WHERE id = ?1",
                        [source],
                        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                    )
                    .optional()?
                    .ok_or(SessionError::Unknown(source))?;
                if owner != workspace {
                    return Err(SessionError::Unknown(source));
                }

                let boundary = match boundary {
                    Some(seq) => seq,
                    // Default: the newest finished turn. A session whose last
                    // row is an unanswered prompt still has one to fork.
                    None => connection
                        .query_row(
                            "SELECT MAX(seq) FROM messages
                             WHERE session_id = ?1 AND role = 'assistant' AND status = 'complete'",
                            [source],
                            |row| row.get::<_, Option<i64>>(0),
                        )
                        .optional()?
                        .flatten()
                        .ok_or_else(|| {
                            SessionError::Refused(format!(
                                "session {source} has no finished turn to fork"
                            ))
                        })?,
                };
                let (role, status): (String, String) = connection
                    .query_row(
                        "SELECT role, status FROM messages WHERE session_id = ?1 AND seq = ?2",
                        params![source, boundary],
                        |row| Ok((row.get(0)?, row.get(1)?)),
                    )
                    .optional()?
                    .ok_or_else(|| {
                        SessionError::Refused(format!(
                            "session {source} has no message {boundary}"
                        ))
                    })?;
                if role != "assistant" || status != "complete" {
                    return Err(SessionError::Refused(format!(
                        "message {boundary} is not a finished turn; fork only at the end of a completed turn"
                    )));
                }
                // The row after the boundary may only be the next turn's prompt.
                // Anything else means the fork would end inside a turn.
                let next_role: Option<String> = connection
                    .query_row(
                        "SELECT role FROM messages WHERE session_id = ?1 AND seq = ?2",
                        params![source, boundary + 1],
                        |row| row.get(0),
                    )
                    .optional()?;
                if next_role.as_deref().is_some_and(|role| role != "user") {
                    return Err(SessionError::Refused(format!(
                        "message {boundary} is inside a turn; fork only at the end of a completed turn"
                    )));
                }

                let mut slug = new_slug();
                let mut row = connection.execute(
                    "INSERT INTO sessions(slug, workspace, title, model, payload_format, status,
                                         created_at, updated_at, parent_session_id, fork_seq)
                     VALUES (?1, ?2, ?3, ?4, ?5, 'active', ?6, ?6, ?7, ?8)",
                    params![slug, workspace, title, model, PAYLOAD_FORMAT, now, source, boundary],
                );
                if let Err(rusqlite::Error::SqliteFailure(error, _)) = &row
                    && error.code == rusqlite::ErrorCode::ConstraintViolation
                {
                    slug = new_slug();
                    row = connection.execute(
                        "INSERT INTO sessions(slug, workspace, title, model, payload_format, status,
                                             created_at, updated_at, parent_session_id, fork_seq)
                         VALUES (?1, ?2, ?3, ?4, ?5, 'active', ?6, ?6, ?7, ?8)",
                        params![slug, workspace, title, model, PAYLOAD_FORMAT, now, source, boundary],
                    );
                }
                row?;
                let copy = connection.last_insert_rowid();
                connection.execute(
                    "INSERT INTO messages(session_id, seq, role, payload, status, created_at)
                     SELECT ?1, seq, role, payload, status, created_at
                     FROM messages WHERE session_id = ?2 AND seq <= ?3 ORDER BY seq",
                    params![copy, source, boundary],
                )?;
                Ok(copy)
            })();
            finish(connection, forked)
        })?
    }

    /// Hide a session from the default list without deleting a single row.
    pub fn archive(&self, workspace: &str, id: i64, archived: bool) -> Result<(), SessionError> {
        let now = crate::util::epoch_secs() as i64;
        self.write(|connection| {
            connection.execute_batch("BEGIN IMMEDIATE")?;
            let written = (|| {
                let owner: String = connection
                    .query_row(
                        "SELECT workspace FROM sessions WHERE id = ?1",
                        [id],
                        |row| row.get(0),
                    )
                    .optional()?
                    .ok_or(SessionError::Unknown(id))?;
                if owner != workspace {
                    return Err(SessionError::Unknown(id));
                }
                connection.execute(
                    "UPDATE sessions SET archived_at = ?2, updated_at = ?3 WHERE id = ?1",
                    params![id, archived.then_some(now), now],
                )?;
                Ok(())
            })();
            finish(connection, written)
        })?
    }

    /// The session rows this workspace has, newest first. Archived sessions are
    /// left out unless `include_archived` asks for them.
    pub fn list_with_archived(
        &self,
        workspace: &str,
        include_archived: bool,
    ) -> Result<Vec<SessionSummary>, SessionError> {
        let mut statement = self.connection.prepare(
            "SELECT s.id, s.slug, s.title, s.updated_at, COUNT(m.id)
             FROM sessions s JOIN messages m ON m.session_id = s.id
             WHERE s.workspace = ?1 AND (?2 OR s.archived_at IS NULL)
             GROUP BY s.id
             HAVING COUNT(m.id) > 0
             ORDER BY s.updated_at DESC, s.id DESC",
        )?;
        let rows: Vec<(i64, String, Option<String>, i64, i64)> = statement
            .query_map(params![workspace, include_archived], |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            })?
            .collect::<Result<_, _>>()?;
        drop(statement);
        Ok(rows
            .into_iter()
            .map(|(id, slug, title, updated_at, messages)| SessionSummary {
                id,
                slug,
                title: title.unwrap_or_else(|| format!("session {id}")),
                updated_at,
                messages: messages as usize,
                state: TurnState::Idle,
            })
            .collect())
    }

    /// Literal title and message-content search inside one workspace. The query
    /// is bound as a value, never spliced into SQL, and the result is capped.
    pub fn search(
        &self,
        workspace: &str,
        query: &str,
        include_archived: bool,
        limit: usize,
    ) -> Result<Vec<SessionSummary>, SessionError> {
        let query = query.trim();
        if query.is_empty() {
            return Ok(Vec::new());
        }
        let pattern = format!("%{}%", escape_like(query));
        let mut statement = self.connection.prepare(
            "SELECT s.id, s.slug, s.title, s.updated_at, COUNT(m.id)
             FROM sessions s JOIN messages m ON m.session_id = s.id
             WHERE s.workspace = ?1 AND (?2 OR s.archived_at IS NULL)
               AND (COALESCE(s.title, '') LIKE ?3 ESCAPE '\\'
                    OR m.payload LIKE ?3 ESCAPE '\\')
             GROUP BY s.id
             ORDER BY s.updated_at DESC, s.id DESC
             LIMIT ?4",
        )?;
        let rows: Vec<(i64, String, Option<String>, i64, i64)> = statement
            .query_map(
                params![workspace, include_archived, pattern, limit as i64],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )?
            .collect::<Result<_, _>>()?;
        Ok(rows
            .into_iter()
            .map(|(id, slug, title, updated_at, messages)| SessionSummary {
                id,
                slug,
                title: title.unwrap_or_else(|| format!("session {id}")),
                updated_at,
                messages: messages as usize,
                state: TurnState::Idle,
            })
            .collect())
    }

    /// Resolve the id a user typed: the `svx-…` slug, or a bare number from a
    /// build that had no slugs.
    pub fn id_for(&self, key: &str) -> Result<i64, SessionError> {
        let key = key.trim();
        let by_slug = self
            .connection
            .query_row("SELECT id FROM sessions WHERE slug = ?1", [key], |row| {
                row.get::<_, i64>(0)
            })
            .optional()?;
        if let Some(id) = by_slug {
            return Ok(id);
        }

        if let Ok(id) = key.parse::<i64>() {
            let exists = self
                .connection
                .query_row("SELECT 1 FROM sessions WHERE id = ?1", [id], |_| Ok(()))
                .optional()?;
            if exists.is_some() {
                return Ok(id);
            }
        }

        Err(SessionError::UnknownKey(key.to_string()))
    }

    /// The one row a submitted prompt writes; never rewritten afterwards.
    pub fn record_user(&self, session: i64, prompt: &str) -> Result<i64, SessionError> {
        self.record_message(session, &ChatMessage::user(prompt), MessageStatus::Complete)
    }

    /// One transaction per message: validate first, then allocate `seq` and
    /// insert under `BEGIN IMMEDIATE`.
    pub fn record_message(
        &self,
        session: i64,
        message: &ChatMessage,
        status: MessageStatus,
    ) -> Result<i64, SessionError> {
        self.ensure_writable(session)?;
        self.check_tool_pairs(session, message)?;
        let payload = serde_json::to_string(message)
            .map_err(|error| SessionError::Refused(error.to_string()))?;
        self.insert_row(session, role_name(&message.role), status, &payload)
    }

    pub fn record_title(&self, session: i64, title: &str) -> Result<(), SessionError> {
        let now = crate::util::epoch_secs() as i64;
        self.write(|connection| {
            connection
                .execute(
                    "UPDATE sessions SET title = ?2, updated_at = ?3 WHERE id = ?1",
                    params![session, title, now],
                )
                .map(|_| ())
        })
    }

    pub fn set_usage(&self, session: i64, total: i64, prompt: i64) -> Result<(), SessionError> {
        self.write(|connection| {
            connection
                .execute(
                    "UPDATE sessions SET total_tokens = ?2, prompt_tokens = ?3 WHERE id = ?1",
                    params![session, total, prompt],
                )
                .map(|_| ())
        })
    }

    /// Sessions of one workspace with at least one message, newest activity
    /// first, each with its derived turn state.
    pub fn list(&self, workspace: &str) -> Result<Vec<SessionSummary>, SessionError> {
        let mut statement = self.connection.prepare(
            "SELECT s.id, s.slug, s.title, s.updated_at, COUNT(m.id)
             FROM sessions s JOIN messages m ON m.session_id = s.id
             WHERE s.workspace = ?1 AND s.archived_at IS NULL
             GROUP BY s.id
             HAVING COUNT(m.id) > 0
             ORDER BY s.updated_at DESC, s.id DESC",
        )?;
        let rows: Vec<(i64, String, Option<String>, i64, i64)> = statement
            .query_map([workspace], |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            })?
            .collect::<Result<_, _>>()?;
        drop(statement);

        let mut summaries = Vec::with_capacity(rows.len());
        for (id, slug, title, updated_at, messages) in rows {
            summaries.push(SessionSummary {
                id,
                slug,
                title: title.unwrap_or_default(),
                updated_at,
                messages: messages as usize,
                state: self.turn_state(id)?,
            });
        }
        Ok(summaries)
    }

    /// Read one session back, rows in `seq` order, marking it read-only when the
    /// payload format or one payload cannot be replayed by this build.
    pub fn load(&self, id: i64) -> Result<LoadedSession, SessionError> {
        let header = self
            .connection
            .query_row(
                "SELECT slug, workspace, title, model, payload_format, total_tokens, prompt_tokens
                 FROM sessions WHERE id = ?1",
                [id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, Option<String>>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, i64>(5)?,
                        row.get::<_, i64>(6)?,
                    ))
                },
            )
            .optional()?
            .ok_or(SessionError::Unknown(id))?;

        let messages = self.rows(id)?;
        let mut read_only = (header.4 != PAYLOAD_FORMAT).then(|| {
            format!(
                "payload format {} does not match {}",
                header.4, PAYLOAD_FORMAT
            )
        });
        if read_only.is_none()
            && let Some(unreadable) = messages
                .iter()
                .find(|message| serde_json::from_str::<ChatMessage>(&message.payload).is_err())
        {
            read_only = Some(format!(
                "message #{} cannot be read as a chat message",
                unreadable.seq
            ));
        }

        Ok(LoadedSession {
            id,
            slug: header.0,
            workspace: header.1,
            title: header.2,
            model: header.3,
            payload_format: header.4,
            read_only,
            messages,
            total_tokens: header.5,
            prompt_tokens: header.6,
            pending_calls: self.pending_call_ids(id).unwrap_or_default(),
        })
    }

    /// Write the synthetic interrupted result for every tool call of the last
    /// assistant row that never got a result. Idempotent, never rewrites a row.
    pub fn record_recovery(&self, session: i64) -> Result<usize, SessionError> {
        let loaded = self.load(session)?;
        if let Some(reason) = loaded.read_only {
            return Err(SessionError::ReadOnly(reason));
        }
        let Some(last) = loaded
            .messages
            .iter()
            .rposition(|row| row.role == "assistant")
        else {
            return Ok(0);
        };
        let answered: Vec<String> = loaded.messages[last + 1..]
            .iter()
            .filter(|row| row.role == "tool")
            .filter_map(|row| response_call_id(&row.payload))
            .collect();

        let mut written = 0;
        for call_id in call_ids(&loaded.messages[last].payload) {
            if answered.contains(&call_id) {
                continue;
            }
            self.record_message(
                session,
                &ChatMessage::from(ToolResponse::new(call_id, INTERRUPTED_RESULT)),
                MessageStatus::Interrupted,
            )?;
            written += 1;
        }
        Ok(written)
    }

    fn rows(&self, session: i64) -> Result<Vec<StoredMessage>, SessionError> {
        let mut statement = self.connection.prepare(
            "SELECT seq, role, status, payload, created_at FROM messages
             WHERE session_id = ?1 ORDER BY seq",
        )?;
        let rows = statement.query_map([session], |row| {
            Ok(StoredMessage {
                seq: row.get(0)?,
                role: row.get(1)?,
                status: MessageStatus::parse(&row.get::<_, String>(2)?),
                payload: row.get(3)?,
                created_at: row.get(4)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    fn turn_state(&self, session: i64) -> Result<TurnState, SessionError> {
        let rows = self.rows(session)?;
        Ok(derive_turn_state(&rows))
    }

    /// A session written by another payload format, or one that does not exist.
    fn ensure_writable(&self, session: i64) -> Result<(), SessionError> {
        let format: Option<String> = self
            .connection
            .query_row(
                "SELECT payload_format FROM sessions WHERE id = ?1",
                [session],
                |row| row.get(0),
            )
            .optional()?;
        match format {
            None => Err(SessionError::Unknown(session)),
            Some(format) if format != PAYLOAD_FORMAT => Err(SessionError::ReadOnly(format!(
                "payload format {format} does not match {PAYLOAD_FORMAT}"
            ))),
            Some(_) => Ok(()),
        }
    }

    /// An assistant row carrying tool calls is committed first; a result whose
    /// `call_id` is not in the newest assistant row is a programming error.
    fn check_tool_pairs(&self, session: i64, message: &ChatMessage) -> Result<(), SessionError> {
        let responses = message.content.tool_responses();
        if responses.is_empty() {
            return Ok(());
        }
        let last_call_row: Option<String> = self
            .connection
            .query_row(
                "SELECT payload FROM messages WHERE session_id = ?1 AND role = 'assistant'
                 ORDER BY seq DESC LIMIT 1",
                [session],
                |row| row.get(0),
            )
            .optional()?;
        let Some(payload) = last_call_row else {
            return Err(SessionError::Refused(
                "a tool result was written before any assistant row".to_string(),
            ));
        };
        let calls: Vec<String> = match serde_json::from_str::<ChatMessage>(&payload) {
            Ok(message) => message
                .content
                .tool_calls()
                .iter()
                .map(|call| call.call_id.clone())
                .collect(),
            Err(_) => Vec::new(),
        };
        for response in responses {
            if !calls.contains(&response.call_id) {
                tracing::error!(
                    call_id = %response.call_id,
                    surface = "session-store",
                    "tool result has no committed assistant call"
                );
                return Err(SessionError::Refused(format!(
                    "tool result for unknown call_id {}",
                    response.call_id
                )));
            }
        }
        Ok(())
    }

    fn insert_row(
        &self,
        session: i64,
        role: &str,
        status: MessageStatus,
        payload: &str,
    ) -> Result<i64, SessionError> {
        let now = crate::util::epoch_secs() as i64;
        self.write(|connection| {
            connection.execute_batch("BEGIN IMMEDIATE")?;
            let inserted = (|| {
                // `seq` is allocated inside the same transaction as the insert,
                // so two writers cannot receive the same number.
                connection.execute(
                    "INSERT INTO messages(session_id, seq, role, status, payload, created_at)
                     SELECT ?1, COALESCE(MAX(seq), 0) + 1, ?2, ?3, ?4, ?5
                     FROM messages WHERE session_id = ?1",
                    params![session, role, status.as_str(), payload, now],
                )?;
                let seq: i64 = connection.query_row(
                    "SELECT seq FROM messages WHERE id = last_insert_rowid()",
                    [],
                    |row| row.get(0),
                )?;
                connection.execute(
                    "UPDATE sessions SET updated_at = ?2, status = 'active' WHERE id = ?1",
                    params![session, now],
                )?;
                Ok(seq)
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

    /// The single retry table: 3 attempts, 100/200 ms waits, one deadline
    /// measured from `Instant::now()`, `busy_timeout` capped by what is left.
    pub(super) fn write<T>(
        &self,
        mut operation: impl FnMut(&Connection) -> rusqlite::Result<T>,
    ) -> Result<T, SessionError> {
        let deadline = Instant::now() + WRITE_BUDGET;
        for attempt in 0..ATTEMPTS {
            if attempt > 0 {
                std::thread::sleep(Duration::from_millis(100 << (attempt - 1)));
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            let busy_timeout = remaining.as_millis().min(BUSY_TIMEOUT_CAP_MS as u128) as u64;
            self.connection
                .pragma_update(None, "busy_timeout", busy_timeout)?;
            match operation(&self.connection) {
                Ok(value) => return Ok(value),
                Err(error) if is_busy(&error) => {
                    tracing::warn!(attempt, %error, "session store is busy");
                }
                Err(error) => return Err(SessionError::from(error)),
            }
        }
        Err(SessionError::Busy)
    }
}
