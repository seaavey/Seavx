//! `seavx sessions list|show|resume|export`.
//!
//! Read commands never write. `resume` is the only arm that appends rows, and
//! only the synthetic interrupted result for a call that never returned.

use clap::Subcommand;

use crate::session::{LoadedSession, SessionError, SessionStore, TurnState};

#[derive(Subcommand, Debug)]
pub enum SessionsCommand {
    /// List this workspace's sessions, newest first
    List,
    /// Print one session's transcript
    Show {
        /// Exact session id
        id: i64,
    },
    /// Open one session in the TUI
    Resume {
        /// Exact session id
        id: i64,
    },
    /// Print one session as JSON
    Export {
        /// Exact session id
        id: i64,
    },
}

pub async fn run(command: SessionsCommand) -> anyhow::Result<()> {
    let store = SessionStore::open().map_err(anyhow::Error::msg)?;
    let workspace = crate::session::workspace_key();

    match command {
        SessionsCommand::List => {
            print!(
                "{}",
                list_text(&store, &workspace).map_err(anyhow::Error::msg)?
            );
        }
        SessionsCommand::Show { id } => {
            print!(
                "{}",
                show_text(&store, &workspace, id).map_err(anyhow::Error::msg)?
            );
        }
        SessionsCommand::Export { id } => {
            println!(
                "{}",
                export_text(&store, &workspace, id).map_err(anyhow::Error::msg)?
            );
        }
        SessionsCommand::Resume { id } => {
            let session = resume_target(&store, &workspace, id).map_err(anyhow::Error::msg)?;
            let state = session_state(&store, &workspace, id);
            if state == TurnState::Pending {
                // rc.3 has no lease, so this is the honest word.
                println!("note: another process may still be writing to this session");
            }
            if session.read_only.is_none() {
                store
                    .record_recovery(id)
                    .map_err(|error| anyhow::anyhow!("{error}"))?;
            }
            let loaded = store.load(id).map_err(|error| anyhow::anyhow!("{error}"))?;
            crate::tui::app::start_tui(None, Some(loaded)).await?;
        }
    }
    Ok(())
}

/// `id / age / msgs / state / title`, newest first; an empty workspace says so.
pub fn list_text(store: &SessionStore, workspace: &str) -> Result<String, String> {
    let summaries = store.list(workspace).map_err(|error| error.to_string())?;
    if summaries.is_empty() {
        return Ok(format!("No sessions yet for {workspace}.\n"));
    }

    let now = crate::util::epoch_secs() as i64;
    let rows: Vec<[String; 5]> = summaries
        .iter()
        .map(|summary| {
            [
                summary.id.to_string(),
                relative_age(summary.updated_at, now),
                summary.messages.to_string(),
                summary.state.as_str().to_string(),
                summary.title.clone(),
            ]
        })
        .collect();
    let header = ["id", "age", "msgs", "state", "title"];

    let mut widths = [0usize; 5];
    for column in 0..4 {
        widths[column] = rows
            .iter()
            .map(|row| row[column].chars().count())
            .chain(std::iter::once(header[column].chars().count()))
            .max()
            .unwrap_or(0);
    }

    let mut out = String::new();
    let mut line = |cells: [&str; 5]| {
        for column in 0..4 {
            out.push_str(&format!(
                "{:<width$}  ",
                cells[column],
                width = widths[column]
            ));
        }
        out.push_str(cells[4]);
        out.push('\n');
    };
    line(header);
    for row in &rows {
        line([&row[0], &row[1], &row[2], &row[3], row[4].as_str()]);
    }
    Ok(out)
}

/// The TUI's `/sessions` list: numbered in the order `resume <n>` reads.
pub fn numbered_list_text(store: &SessionStore, workspace: &str) -> Result<String, String> {
    let summaries = store.list(workspace).map_err(|error| error.to_string())?;
    if summaries.is_empty() {
        return Ok(format!(
            "No sessions yet for {workspace}. Start one by sending a prompt."
        ));
    }

    let now = crate::util::epoch_secs() as i64;
    let mut out = String::from("Sessions (newest first):");
    for (index, summary) in summaries.iter().enumerate() {
        out.push_str(&format!(
            "\n  {}  id {}  {}  {} msgs  {}\n     {}",
            index + 1,
            summary.id,
            relative_age(summary.updated_at, now),
            summary.messages,
            summary.state.as_str(),
            if summary.title.is_empty() {
                "(untitled)"
            } else {
                summary.title.as_str()
            }
        ));
    }
    out.push_str("\nResume one with /sessions <number>.");
    Ok(out)
}

/// Load the session at `number` in the listed order, refusing a read-only one
/// with its reason instead of opening it.
pub fn resume_by_number(
    store: &SessionStore,
    workspace: &str,
    number: u32,
) -> Result<LoadedSession, String> {
    let summaries = store.list(workspace).map_err(|error| error.to_string())?;
    let summary = summaries
        .get(number.saturating_sub(1) as usize)
        .filter(|_| number > 0)
        .ok_or_else(|| {
            format!("No session {number}. Run `/sessions` to list this workspace's sessions.")
        })?;
    let session = store.load(summary.id).map_err(|error| error.to_string())?;
    if let Some(reason) = &session.read_only {
        return Err(format!(
            "Session {} is read-only: {reason}. Use /clear to start a new one.",
            session.id
        ));
    }
    Ok(session)
}

/// Full transcript with the header the spec asks for: id, title, model,
/// payload format and usage.
pub fn show_text(store: &SessionStore, workspace: &str, id: i64) -> Result<String, String> {
    let session = resume_target(store, workspace, id)?;
    let mut out = format!(
        "Session {} · {}\n",
        session.id,
        session.title.as_deref().unwrap_or("(untitled)")
    );
    out.push_str(&format!(
        "model: {} · payload format: {} · tokens: {} (prompt {})\n",
        session.model.as_deref().unwrap_or("unknown"),
        session.payload_format,
        session.total_tokens,
        session.prompt_tokens,
    ));
    if let Some(reason) = &session.read_only {
        out.push_str(&format!("read-only: {reason}\n"));
    }
    out.push('\n');
    out.push_str(&crate::session::render_text(&session));
    Ok(out)
}

pub fn export_text(store: &SessionStore, workspace: &str, id: i64) -> Result<String, String> {
    Ok(crate::session::export_json(&resume_target(
        store, workspace, id,
    )?))
}

/// Another workspace's session is refused with that path, for resume, show and
/// export alike: a transcript is not readable just because its id was guessed.
pub fn resume_target(
    store: &SessionStore,
    workspace: &str,
    id: i64,
) -> Result<LoadedSession, String> {
    let session = load_named(store, id)?;
    if session.workspace != workspace {
        return Err(format!(
            "Session {id} belongs to another workspace: {}",
            session.workspace
        ));
    }
    Ok(session)
}

fn load_named(store: &SessionStore, id: i64) -> Result<LoadedSession, String> {
    store.load(id).map_err(|error| match error {
        SessionError::Unknown(id) => format!("Unknown session {id}. Run 'seavx sessions list'."),
        other => other.to_string(),
    })
}

fn session_state(store: &SessionStore, workspace: &str, id: i64) -> TurnState {
    store
        .list(workspace)
        .ok()
        .and_then(|summaries| {
            summaries
                .into_iter()
                .find(|summary| summary.id == id)
                .map(|summary| summary.state)
        })
        .unwrap_or(TurnState::Idle)
}

/// Relative age from epoch seconds; no date dependency, no `chrono`.
pub fn relative_age(updated_at: i64, now: i64) -> String {
    let seconds = now.saturating_sub(updated_at).max(0);
    match seconds {
        0..60 => "now".to_string(),
        60..3_600 => format!("{}m", seconds / 60),
        3_600..86_400 => format!("{}h", seconds / 3_600),
        _ => format!("{}d", seconds / 86_400),
    }
}

#[cfg(test)]
#[path = "../tests/cli/sessions.rs"]
mod tests;
