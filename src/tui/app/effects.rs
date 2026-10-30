use tokio::sync::mpsc;

use super::App;
use super::worker::run_agent_worker;
use crate::agent::engine::AgentMode;
use crate::tui::event::{Action, AgentState};

/// Load one session off the event loop and hand it back as an action. Both
/// `/sessions <n>` and its picker go through here, so a number means the same
/// thing in both.
pub(super) fn spawn_session_resume(app: &mut App, tx: &mpsc::UnboundedSender<Action>, number: u32) {
    app.status = AgentState::Checking;
    let tx = tx.clone();
    tokio::task::spawn_blocking(move || {
        let result = crate::session::SessionStore::open()
            .map_err(|error| error.to_string())
            .and_then(|store| {
                crate::cli::sessions::resume_by_number(
                    &store,
                    &crate::session::workspace_key(),
                    number,
                )
            });
        let _ = tx.send(Action::SessionResume(result));
    });
}

/// Apply the event-loop effects a command asked for.
pub(super) fn apply_command_effect(
    app: &mut App,
    effect: crate::tui::commands::CommandEffect,
    tx: &mpsc::UnboundedSender<Action>,
) {
    use crate::tui::commands::CommandEffect;

    match effect {
        CommandEffect::Handled => {}
        CommandEffect::Quit => app.should_quit = true,
        CommandEffect::OpenModelPicker => {
            app.open_model_picker();
        }
        CommandEffect::RunDoctor { offline } => {
            app.status = AgentState::Checking;
            let tx = tx.clone();
            let options = crate::cli::doctor::DoctorOptions {
                offline,
                workspace_root: std::path::PathBuf::from("."),
                home: crate::home::dir(),
            };
            tokio::spawn(async move {
                let report = crate::cli::doctor::run(options).await;
                let _ = tx.send(Action::DoctorDone(report));
            });
        }
        CommandEffect::RunSessions { number } => spawn_session_resume(app, tx, number),
        CommandEffect::RunAsk { question } => {
            // The same submit path a typed prompt takes; only the mode differs,
            // and it never touches `app.mode`.
            if let Some(request) = app.submit_turn(&question) {
                let is_first_turn = app.session_title.is_none();
                let context = app.session_context.clone();
                let tx = tx.clone();
                tokio::spawn(async move {
                    run_agent_worker(request, tx, is_first_turn, AgentMode::Ask, context).await;
                });
            }
        }
        CommandEffect::OpenSessionPicker => match crate::session::SessionStore::open() {
            Ok(store) => {
                app.open_session_picker(&store);
            }
            Err(error) => app.push_notice(format!("Sessions unavailable: {error}")),
        },
        CommandEffect::RunUndo => {
            app.status = AgentState::Checking;
            let tx = tx.clone();
            let root = app.root.clone();
            tokio::task::spawn_blocking(move || {
                let result =
                    crate::checkpoint::CheckpointStore::undo_latest(&root).map_err(|error| {
                        // A half-done undo must say what it already changed.
                        let restored = error.restored();
                        if restored.is_empty() {
                            return error.to_string();
                        }
                        let paths: Vec<String> = restored
                            .iter()
                            .map(|path| path.display().to_string())
                            .collect();
                        format!(
                            "{error}\nRestored {} file(s) before the failure:\n{}",
                            paths.len(),
                            paths.join("\n")
                        )
                    });
                let _ = tx.send(Action::UndoDone(result));
            });
        }
        CommandEffect::SkillsAdd { dir } => {
            app.status = AgentState::Checking;
            let tx = tx.clone();
            let root = app.root.clone();
            tokio::task::spawn_blocking(move || {
                let result = crate::cli::skills::install(&root, &dir);
                let _ = tx.send(Action::SkillsInstalled(result));
            });
        }
        CommandEffect::SkillsRemove { name } => {
            app.status = AgentState::Checking;
            let tx = tx.clone();
            let root = app.root.clone();
            tokio::task::spawn_blocking(move || {
                let result = crate::cli::skills::remove(&root, &name);
                let _ = tx.send(Action::SkillsRemoved(result));
            });
        }
        CommandEffect::SkillsFetch { url, names } => {
            app.status = AgentState::Checking;
            let tx = tx.clone();
            let root = app.root.clone();
            tokio::task::spawn_blocking(move || {
                let fetched = match crate::cli::skills::fetch(&url) {
                    Ok(fetched) => fetched,
                    Err(error) => {
                        let _ = tx.send(Action::SkillsFetched(Err(error)));
                        return;
                    }
                };
                if names.is_empty() {
                    let _ = tx.send(Action::SkillsFetched(Ok(fetched)));
                    return;
                }
                // A named install skips the picker, so it runs to completion here.
                let results = names
                    .iter()
                    .map(
                        |name| match fetched.candidates.iter().position(|c| &c.name == name) {
                            Some(index) => (name.clone(), fetched.install(&root, index)),
                            None => (
                                name.clone(),
                                Err(format!("{name} is not in this repository.")),
                            ),
                        },
                    )
                    .collect();
                let _ = tx.send(Action::SkillsInstalledBatch(results));
            });
        }
        CommandEffect::RunCompact { instructions } => {
            let transcript = crate::tui::compact::transcript_from_segments(&app.segments);
            if crate::tui::compact::is_too_short(&transcript) {
                app.push_notice(format!(
                    "Transcript is too short to compact ({} lines).",
                    transcript.lines().count()
                ));
                return;
            }

            app.status = AgentState::Compacting;
            let tx = tx.clone();
            let config = app.config.clone();
            tokio::spawn(async move {
                let summarizer = crate::tui::compact::ModelSummarizer::from_config(&config);
                let result = crate::tui::compact::run_compaction(
                    &transcript,
                    instructions.as_deref(),
                    &summarizer,
                )
                .await;
                let _ = tx.send(Action::CompactDone { result });
            });
        }
    }
}
