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

/// Apply one `/goal` operation. Every refusal names what to do next; nothing
/// here switches a goal behind the user's back.
fn run_goal(
    workspace: &str,
    action: crate::tui::commands::GoalAction,
    start: &mut Option<String>,
) -> Result<String, String> {
    use crate::tui::commands::GoalAction;
    let store = crate::session::SessionStore::open().map_err(|error| error.to_string())?;

    // `id` defaults to the one goal that holds the turn, then to the newest.
    let resolve = |id: Option<i64>| -> Result<i64, String> {
        if let Some(id) = id {
            return Ok(id);
        }
        store
            .goals(workspace)
            .map_err(|error| error.to_string())?
            .into_iter()
            .find(|goal| matches!(goal.state.as_str(), "active" | "blocked" | "paused"))
            .map(|goal| goal.id)
            .ok_or_else(|| "no active goal; start one with /goal <objective>".to_string())
    };

    match action {
        GoalAction::New(objective) => {
            let id = store
                .create_goal(workspace, None, &objective)
                .map_err(|error| error.to_string())?;
            Ok(format!(
                "Goal {id} drafted: {objective}\nUse /goal edit to add criteria and tasks, then /goal approve."
            ))
        }
        GoalAction::Edit => {
            let goal = store
                .goal(workspace, resolve(None)?)
                .map_err(|error| error.to_string())?;
            Ok(format!(
                "Goal {}: {}\nEdit is not interactive yet; the draft holds {} criteria and {} tasks.",
                goal.id,
                goal.objective,
                goal.criteria.len(),
                goal.tasks.len()
            ))
        }
        GoalAction::Approve => {
            let id = resolve(None)?;
            store
                .set_goal_state(workspace, id, crate::session::GoalState::Active)
                .map_err(|error| error.to_string())?;
            // The approved plan runs as one Build turn: same permissions, same
            // checkpoint, same cancellation as a typed prompt.
            let goal = store
                .goal(workspace, id)
                .map_err(|error| error.to_string())?;
            // The tasks are marked running before the turn starts, so a finish
            // that arrives first cannot leave them pending for ever.
            for task in &goal.tasks {
                if task.state == crate::session::TaskState::Pending {
                    store
                        .set_task_state(
                            workspace,
                            id,
                            task.position,
                            crate::session::TaskState::Running,
                            "",
                        )
                        .map_err(|error| error.to_string())?;
                }
            }
            *start = Some(crate::session::goal_task_text(&goal));
            Ok(format!(
                "Goal {id} approved; running {} task(s) under Build permissions.",
                goal.tasks.len()
            ))
        }
        GoalAction::Show(id) => {
            let goal = store
                .goal(workspace, resolve(id)?)
                .map_err(|error| error.to_string())?;
            Ok(render_goal(&goal))
        }
        GoalAction::Pause(id) => {
            let id = resolve(id)?;
            store
                .set_goal_state(workspace, id, crate::session::GoalState::Paused)
                .map_err(|error| error.to_string())?;
            Ok(format!("Goal {id} paused."))
        }
        GoalAction::Resume(id) => {
            let id = resolve(id)?;
            store
                .set_goal_state(workspace, id, crate::session::GoalState::Active)
                .map_err(|error| error.to_string())?;
            Ok(format!("Goal {id} resumed."))
        }
        GoalAction::Cancel(id) => {
            let id = resolve(id)?;
            store
                .set_goal_state(workspace, id, crate::session::GoalState::Cancelled)
                .map_err(|error| error.to_string())?;
            Ok(format!("Goal {id} cancelled."))
        }
    }
}

/// The one line `/status` shows: the goal that holds this workspace, if any.
fn live_goal_line(workspace: &str) -> Option<String> {
    let store = crate::session::SessionStore::open().ok()?;
    let goals = store.goals(workspace).ok()?;
    let goal = goals
        .into_iter()
        .find(|goal| matches!(goal.state.as_str(), "active" | "blocked" | "paused"))?;
    Some(format!("{} {}", goal.id, goal.state.as_str()))
}

fn read_tasks(workspace: &str) -> Result<String, String> {
    let store = crate::session::SessionStore::open().map_err(|error| error.to_string())?;
    let goals = store.goals(workspace).map_err(|error| error.to_string())?;
    let Some(goal) = goals
        .into_iter()
        .find(|goal| matches!(goal.state.as_str(), "active" | "blocked" | "paused"))
    else {
        return Ok("No active goal.".to_string());
    };
    if goal.tasks.is_empty() {
        return Ok(format!("Goal {} has no tasks.", goal.id));
    }
    let mut out = format!("Goal {} tasks:\n", goal.id);
    for task in &goal.tasks {
        out.push_str(&format!(
            "  {}. [{}] {}\n",
            task.position + 1,
            task.state.as_str(),
            task.title
        ));
    }
    Ok(out)
}

fn render_goal(goal: &crate::session::Goal) -> String {
    let mut out = format!(
        "Goal {} [{}] {}\n",
        goal.id,
        goal.state.as_str(),
        goal.objective
    );
    for criterion in &goal.criteria {
        out.push_str(&format!(
            "  criterion {}: [{}] {}\n",
            criterion.position + 1,
            criterion.state.as_str(),
            criterion.text
        ));
        if !criterion.evidence.is_empty() {
            out.push_str(&format!("    evidence: {}\n", criterion.evidence));
        }
        if let Some(reason) = &criterion.waiver_reason {
            out.push_str(&format!("    waived: {reason}\n"));
        }
    }
    for task in &goal.tasks {
        out.push_str(&format!(
            "  task {}: [{}] {}\n",
            task.position + 1,
            task.state.as_str(),
            task.title
        ));
    }
    out
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
        CommandEffect::RunGoal { action } => {
            let tx = tx.clone();
            let workspace = crate::session::workspace_key();
            tokio::task::spawn_blocking(move || {
                let mut start = None;
                let result = run_goal(&workspace, action, &mut start);
                let goal = live_goal_line(&workspace);
                let _ = tx.send(Action::GoalDone {
                    result,
                    goal,
                    start,
                });
            });
        }
        CommandEffect::RunTasks => {
            let tx = tx.clone();
            let workspace = crate::session::workspace_key();
            tokio::task::spawn_blocking(move || {
                let result = read_tasks(&workspace);
                let goal = live_goal_line(&workspace);
                let _ = tx.send(Action::GoalDone {
                    result,
                    goal,
                    start: None,
                });
            });
        }
        CommandEffect::RunReview { target } => {
            // Git work never runs on the event loop; the resolved input comes
            // back as an action, and only then is a turn submitted.
            app.status = AgentState::Checking;
            let tx = tx.clone();
            tokio::task::spawn_blocking(move || {
                let security = crate::agent::security::WorkspaceSecurity::new(".");
                let result = crate::agent::git_review::resolve(security.root(), &target, &security);
                let _ = tx.send(Action::ReviewResolved(result));
            });
        }
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
