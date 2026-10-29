//! Versioned SQLite session store.
//!
//! One database per workspace: `<cwd>/.seavx/sessions.db` in debug mode, else
//! `<home>/.seavx/sessions.db` — the same rule `MEMORY.md` follows. The file is
//! `0600`, its directory `0700`, WAL and `synchronous=NORMAL` give the write
//! budget without holding a lock across a turn.

use std::path::{Path, PathBuf};

use rusqlite::Connection;

/// Serde format of a message payload. Bumped only if `genai` changes its
/// `ChatMessage` shape; a mismatch opens the session read-only.
pub const PAYLOAD_FORMAT: &str = "genai-chat-v1";
/// Schema version this build understands.
pub const SCHEMA_VERSION: u32 = 1;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS sessions(
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    workspace TEXT NOT NULL,
    title TEXT,
    model TEXT,
    payload_format TEXT NOT NULL,
    status TEXT NOT NULL CHECK(status IN ('active','ended')),
    total_tokens INTEGER NOT NULL DEFAULT 0,
    prompt_tokens INTEGER NOT NULL DEFAULT 0,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS messages(
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id INTEGER NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    seq INTEGER NOT NULL,
    role TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'complete'
        CHECK(status IN ('complete','partial','cancelled','interrupted')),
    payload TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    UNIQUE(session_id, seq)
);
CREATE INDEX IF NOT EXISTS messages_session_seq ON messages(session_id, seq);
CREATE INDEX IF NOT EXISTS sessions_workspace_updated ON sessions(workspace, updated_at DESC);
";

#[derive(Debug)]
pub enum SessionError {
    /// The file was written by a newer build: refuse rather than downgrade.
    Newer { found: u32, supported: u32 },
    /// Cannot be opened, read or migrated; the run continues unpersisted.
    Unavailable(String),
}

impl std::fmt::Display for SessionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SessionError::Newer { found, supported } => write!(
                f,
                "session database version {found} is newer than this build ({supported}); refusing to open it"
            ),
            SessionError::Unavailable(detail) => write!(f, "session store unavailable: {detail}"),
        }
    }
}

impl std::error::Error for SessionError {}

impl From<rusqlite::Error> for SessionError {
    fn from(error: rusqlite::Error) -> Self {
        SessionError::Unavailable(error.to_string())
    }
}

pub struct SessionStore {
    connection: Connection,
}

impl SessionStore {
    /// The default location for this run.
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
            create_owner_only(directory, 0o700)
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

        create_owner_only(path, 0o600)
            .map_err(|error| SessionError::Unavailable(error.to_string()))?;
        connection.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL; PRAGMA foreign_keys=ON; PRAGMA busy_timeout=2000;",
        )?;

        let store = Self { connection };
        store.migrate()?;
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
        transaction.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        transaction.commit()?;
        Ok(())
    }

    /// The connection behind every later read and write.
    pub fn connection(&self) -> &Connection {
        &self.connection
    }
}

fn create_owner_only(path: &Path, mode: u32) -> std::io::Result<()> {
    if path.is_dir() {
        std::fs::create_dir_all(path)?;
    } else if !path.exists() {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // Tighten, never refuse: a legacy 0755 directory is repaired.
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))?;
    }
    Ok(())
}

#[cfg(test)]
#[path = "tests/core/session.rs"]
mod tests;
