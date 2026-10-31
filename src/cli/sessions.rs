//! `seavx sessions list|show|resume|export`.
//!
//! Read commands never write. `resume` is the only arm that appends rows, and
//! only the synthetic interrupted result for a call that never returned.

use clap::Subcommand;

use crate::session::{LoadedSession, SessionError, SessionStore, TurnState};

#[derive(Subcommand, Debug)]
pub enum SessionsCommand {
    /// List this workspace's sessions, newest first
    List {
        /// Include archived sessions
        #[arg(long)]
        all: bool,
    },
    /// Copy a session's completed prefix into a new independent session
    Fork {
        /// Session id, `svx-…` (a bare number still works)
        id: String,
        /// Copy only up to this message sequence (default: the newest turn)
        #[arg(long)]
        at: Option<i64>,
    },
    /// Hide a session from the default list without deleting it
    Archive {
        /// Session id, `svx-…` (a bare number still works)
        id: String,
    },
    /// Bring an archived session back into the default list
    Unarchive {
        /// Session id, `svx-…` (a bare number still works)
        id: String,
    },
    /// Search this workspace's session titles and messages
    Search {
        /// Literal text to look for
        query: String,
        /// Include archived sessions
        #[arg(long)]
        all: bool,
    },
    /// Print one session's transcript
    Show {
        /// Session id, `svx-…` (a bare number still works)
        id: String,
        /// Append the stored execution trace, in order
        #[arg(long)]
        events: bool,
    },
    /// Open one session in the TUI
    Resume {
        /// Session id, `svx-…` (a bare number still works)
        id: String,
    },
    /// Print one session as JSON
    Export {
        /// Session id, `svx-…` (a bare number still works)
        id: String,
    },
}

pub async fn run(command: SessionsCommand) -> anyhow::Result<()> {
    let store = SessionStore::open().map_err(anyhow::Error::msg)?;
    let workspace = crate::session::workspace_key();

    match command {
        SessionsCommand::List { all } => {
            print!(
                "{}",
                list_text(&store, &workspace, all).map_err(anyhow::Error::msg)?
            );
        }
        SessionsCommand::Fork { id, at } => {
            let id = resolve(&store, &id)?;
            let forked = store.fork(&workspace, id, at).map_err(anyhow::Error::msg)?;
            let loaded = store.load(forked).map_err(anyhow::Error::msg)?;
            println!(
                "Forked session {id} into {} ({} messages).",
                loaded.slug,
                loaded.messages.len()
            );
        }
        SessionsCommand::Archive { id } => {
            let id = resolve(&store, &id)?;
            store
                .archive(&workspace, id, true)
                .map_err(anyhow::Error::msg)?;
            println!("Archived session {id}. It is hidden, not deleted.");
        }
        SessionsCommand::Unarchive { id } => {
            let id = resolve(&store, &id)?;
            store
                .archive(&workspace, id, false)
                .map_err(anyhow::Error::msg)?;
            println!("Unarchived session {id}.");
        }
        SessionsCommand::Search { query, all } => {
            print!(
                "{}",
                search_text(&store, &workspace, &query, all).map_err(anyhow::Error::msg)?
            );
        }
        SessionsCommand::Show { id, events } => {
            let id = resolve(&store, &id)?;
            print!(
                "{}",
                show_text(&store, &workspace, id, events).map_err(anyhow::Error::msg)?
            );
        }
        SessionsCommand::Export { id } => {
            let id = resolve(&store, &id)?;
            println!(
                "{}",
                export_text(&store, &workspace, id).map_err(anyhow::Error::msg)?
            );
        }
        SessionsCommand::Resume { id } => {
            let id = resolve(&store, &id)?;
            open_session(&store, &workspace, id).await?
        }
    }
    Ok(())
}

/// Turn what the user typed into a row id: the `svx-…` slug, or a number from
/// a build that had no slugs. The error keeps the text they typed.
fn resolve(store: &SessionStore, key: &str) -> anyhow::Result<i64> {
    store.id_for(key).map_err(|error| match error {
        SessionError::UnknownKey(key) => {
            anyhow::anyhow!("Unknown session {key}. Run 'seavx sessions list'.")
        }
        other => anyhow::Error::msg(other.to_string()),
    })
}

/// `seavx resume`: open a session by id, or — with no id — list this
/// workspace's sessions and ask which one. The listing is the same text
/// `/sessions` prints, so the number a user reads there is the number the
/// prompt accepts.
pub async fn run_resume(key: Option<String>) -> anyhow::Result<()> {
    let store = SessionStore::open().map_err(anyhow::Error::msg)?;
    let workspace = crate::session::workspace_key();

    let Some(key) = key else {
        return pick_and_open(&store, &workspace).await;
    };
    let id = resolve(&store, &key)?;
    open_session(&store, &workspace, id).await
}

async fn pick_and_open(store: &SessionStore, workspace: &str) -> anyhow::Result<()> {
    let summaries = store
        .list(workspace)
        .map_err(|error| anyhow::anyhow!("{error}"))?;
    let listing = numbered_list_text(store, workspace).map_err(anyhow::Error::msg)?;
    println!("{listing}");
    if summaries.is_empty() {
        return Ok(());
    }

    println!();
    let input = crate::cli::prompt::line("Resume which number? (Enter to cancel)", "")?;
    if input.is_empty() {
        return Ok(());
    }

    let number = parse_resume_number(&input, summaries.len()).map_err(anyhow::Error::msg)?;
    open_session(store, workspace, summaries[number as usize - 1].id).await
}

/// The picker accepts only a number it printed: no ids, no titles, no range.
pub fn parse_resume_number(input: &str, count: usize) -> Result<u32, String> {
    if count == 0 {
        return Err("no sessions to resume".to_string());
    }
    match input.trim().parse::<usize>() {
        Ok(number) if (1..=count).contains(&number) => Ok(number as u32),
        _ => Err(format!("Enter a number from 1 to {count}.")),
    }
}

/// Load one session and hand it to the TUI. Both `sessions resume <id>` and
/// `resume` end here, so the recovery note and the read-only rule cannot drift.
async fn open_session(store: &SessionStore, workspace: &str, id: i64) -> anyhow::Result<()> {
    let session = resume_target(store, workspace, id).map_err(anyhow::Error::msg)?;
    let state = session_state(store, workspace, id);
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
    crate::tui::app::start_tui(None, Some(loaded)).await
}

/// One compact row per session, newest first: the `/sessions` picker's rows.
pub fn session_rows(store: &SessionStore, workspace: &str) -> Result<Vec<String>, String> {
    let summaries = store.list(workspace).map_err(|error| error.to_string())?;
    let now = crate::util::epoch_secs() as i64;
    Ok(summaries
        .iter()
        .enumerate()
        .map(|(index, summary)| {
            format!(
                "{}  {}  {}  {} msgs  {}  {}",
                index + 1,
                summary.slug,
                relative_age(summary.updated_at, now),
                summary.messages,
                summary.state.as_str(),
                if summary.title.is_empty() {
                    "(untitled)"
                } else {
                    summary.title.as_str()
                }
            )
        })
        .collect())
}

/// `slug / age / msgs / state / title`, newest first; an empty workspace says so.
pub fn list_text(store: &SessionStore, workspace: &str, all: bool) -> Result<String, String> {
    let summaries = store
        .list_with_archived(workspace, all)
        .map_err(|error| error.to_string())?;
    if summaries.is_empty() {
        return Ok(format!("No sessions yet for {workspace}.\n"));
    }
    Ok(summary_table(&summaries))
}

/// The one table both `/sessions` and the search print.
fn summary_table(summaries: &[crate::session::SessionSummary]) -> String {
    let now = crate::util::epoch_secs() as i64;
    let rows: Vec<[String; 5]> = summaries
        .iter()
        .map(|summary| {
            [
                summary.slug.clone(),
                relative_age(summary.updated_at, now),
                summary.messages.to_string(),
                summary.state.as_str().to_string(),
                summary.title.clone(),
            ]
        })
        .collect();
    let header = ["slug", "age", "msgs", "state", "title"];

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
    out
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
            "\n  {}  {}  {}  {} msgs  {}\n     {}",
            index + 1,
            summary.slug,
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
    out.push_str("\nResume one with /sessions <number> or /resume.");
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
    // The picker must recover a crash the same way `seavx resume` does.
    store
        .record_recovery(summary.id)
        .map_err(|error| error.to_string())?;
    store.load(summary.id).map_err(|error| error.to_string())
}

/// Full transcript with the header the spec asks for: id, title, model,
/// payload format and usage.
/// Search hits, in the same table the list uses.
pub fn search_text(
    store: &SessionStore,
    workspace: &str,
    query: &str,
    all: bool,
) -> Result<String, String> {
    const LIMIT: usize = 50;
    let summaries = store
        .search(workspace, query, all, LIMIT)
        .map_err(|error| error.to_string())?;
    if summaries.is_empty() {
        return Ok(format!("No session matches '{query}' in {workspace}.\n"));
    }
    Ok(summary_table(&summaries))
}

pub fn show_text(
    store: &SessionStore,
    workspace: &str,
    id: i64,
    events: bool,
) -> Result<String, String> {
    let session = resume_target(store, workspace, id)?;
    let mut out = format!(
        "Session {} · {}\n",
        session.slug,
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

    if events {
        // The stored trace, in database order: safe metadata only, never the
        // transcript bodies the rows already hold.
        out.push_str("\nTrace:\n");
        let stored = store.events(id).map_err(|error| error.to_string())?;
        if stored.is_empty() {
            out.push_str("(no events recorded)\n");
        }
        for event in stored {
            out.push_str(&format!(
                "{:>3} {} {}{}{}{}\n",
                event.seq,
                event.kind,
                event.status,
                event
                    .failure
                    .map(|failure| format!(" · {failure}"))
                    .unwrap_or_default(),
                event
                    .tool_call_id
                    .map(|call| format!(" {call}"))
                    .unwrap_or_default(),
                if event.summary.is_empty() {
                    String::new()
                } else {
                    format!(" {}", event.summary)
                },
            ));
        }
    }
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
        SessionError::UnknownKey(key) => {
            format!("Unknown session {key}. Run 'seavx sessions list'.")
        }
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
