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
/// here switches a goal behind the user's back. The store is passed in so the
/// verbs are testable against a temporary database.
pub(super) fn run_goal(
    store: &crate::session::SessionStore,
    workspace: &str,
    action: crate::tui::commands::GoalAction,
    start: &mut Option<String>,
) -> Result<String, String> {
    use crate::tui::commands::GoalAction;

    // `id` defaults to the goal that holds the turn, else the newest draft: the
    // editing verbs only accept a draft, and a draft is the only thing
    // `/goal approve` can promote.
    let resolve = |id: Option<i64>| -> Result<i64, String> {
        if let Some(id) = id {
            return Ok(id);
        }
        let goals = store.goals(workspace).map_err(|error| error.to_string())?;
        let live = goals
            .iter()
            .find(|goal| matches!(goal.state.as_str(), "active" | "blocked" | "paused"));
        let draft = || goals.iter().find(|goal| goal.state.as_str() == "draft");
        live.or_else(draft)
            .map(|goal| goal.id)
            .ok_or_else(|| "no goal yet; start one with /goal <objective>".to_string())
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
        GoalAction::Edit(objective) => {
            let id = resolve(None)?;
            let goal = store
                .goal(workspace, id)
                .map_err(|error| error.to_string())?;
            let Some(objective) = objective else {
                return Ok(format!(
                    "Goal {}: {}\n{} criteria, {} tasks. Edit with /goal edit <text>, /goal criteria add <text>, /goal task add <text>.",
                    goal.id,
                    goal.objective,
                    goal.criteria.len(),
                    goal.tasks.len()
                ));
            };
            let criteria: Vec<String> = goal.criteria.iter().map(|c| c.text.clone()).collect();
            let tasks: Vec<String> = goal.tasks.iter().map(|t| t.title.clone()).collect();
            store
                .set_goal_objective(workspace, id, &objective)
                .map_err(|error| error.to_string())?;
            store
                .set_goal_plan(workspace, id, &criteria, &tasks)
                .map_err(|error| error.to_string())?;
            Ok(format!("Goal {id} objective updated."))
        }
        GoalAction::CriteriaAdd(text) => {
            let id = resolve(None)?;
            let goal = store
                .goal(workspace, id)
                .map_err(|error| error.to_string())?;
            let mut criteria: Vec<String> = goal.criteria.iter().map(|c| c.text.clone()).collect();
            criteria.push(text);
            let tasks: Vec<String> = goal.tasks.iter().map(|t| t.title.clone()).collect();
            store
                .set_goal_plan(workspace, id, &criteria, &tasks)
                .map_err(|error| error.to_string())?;
            Ok(format!("Goal {id}: {} criteria.", criteria.len()))
        }
        GoalAction::CriteriaWaive { index, reason } => {
            let id = resolve(None)?;
            // The index is checked here, like the sibling add/remove verbs, so a
            // typo names the criterion instead of surfacing a store error.
            let goal = store
                .goal(workspace, id)
                .map_err(|error| error.to_string())?;
            if index >= goal.criteria.len() {
                return Err(format!("there is no criterion {}", index + 1));
            }
            store
                .set_criterion_state(
                    workspace,
                    id,
                    index as i64,
                    crate::session::CriterionState::Waived,
                    "",
                    Some(&reason),
                )
                .map_err(|error| error.to_string())?;
            Ok(format!("Goal {id}: criterion {} waived.", index + 1))
        }
        GoalAction::CriteriaSatisfy { index, evidence } => {
            let id = resolve(None)?;
            let goal = store
                .goal(workspace, id)
                .map_err(|error| error.to_string())?;
            if index >= goal.criteria.len() {
                return Err(format!("there is no criterion {}", index + 1));
            }
            store
                .set_criterion_state(
                    workspace,
                    id,
                    index as i64,
                    crate::session::CriterionState::Satisfied,
                    &evidence,
                    None,
                )
                .map_err(|error| error.to_string())?;
            Ok(format!("Goal {id}: criterion {} satisfied.", index + 1))
        }
        GoalAction::TaskAdd(text) => {
            let id = resolve(None)?;
            let goal = store
                .goal(workspace, id)
                .map_err(|error| error.to_string())?;
            let criteria: Vec<String> = goal.criteria.iter().map(|c| c.text.clone()).collect();
            let mut tasks: Vec<String> = goal.tasks.iter().map(|t| t.title.clone()).collect();
            tasks.push(text);
            store
                .set_goal_plan(workspace, id, &criteria, &tasks)
                .map_err(|error| error.to_string())?;
            Ok(format!("Goal {id}: {} tasks.", tasks.len()))
        }
        GoalAction::CriteriaRemove(index) => {
            let id = resolve(None)?;
            let goal = store
                .goal(workspace, id)
                .map_err(|error| error.to_string())?;
            let mut criteria: Vec<String> = goal.criteria.iter().map(|c| c.text.clone()).collect();
            let tasks: Vec<String> = goal.tasks.iter().map(|t| t.title.clone()).collect();
            if index >= criteria.len() {
                return Err(format!("there is no criterion {}", index + 1));
            }
            criteria.remove(index);
            store
                .set_goal_plan(workspace, id, &criteria, &tasks)
                .map_err(|error| error.to_string())?;
            Ok(format!("Goal {id}: {} criteria.", criteria.len()))
        }
        GoalAction::TaskRemove(index) => {
            let id = resolve(None)?;
            let goal = store
                .goal(workspace, id)
                .map_err(|error| error.to_string())?;
            let criteria: Vec<String> = goal.criteria.iter().map(|c| c.text.clone()).collect();
            let mut tasks: Vec<String> = goal.tasks.iter().map(|t| t.title.clone()).collect();
            if index >= tasks.len() {
                return Err(format!("there is no task {}", index + 1));
            }
            tasks.remove(index);
            store
                .set_goal_plan(workspace, id, &criteria, &tasks)
                .map_err(|error| error.to_string())?;
            Ok(format!("Goal {id}: {} tasks.", tasks.len()))
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
        GoalAction::Complete(id) => {
            let id = resolve(id)?;
            // The store is the gate: it refuses while a criterion is pending or
            // missing its evidence, so this cannot be a model-asserted claim.
            store
                .set_goal_state(workspace, id, crate::session::GoalState::Completed)
                .map_err(|error| error.to_string())?;
            Ok(format!("Goal {id} completed."))
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

/// Apply one `/sessions` verb that acts on a stored session instead of opening
/// it. Every id is resolved once, and the store checks workspace ownership
/// before it reads or writes anything.
pub(super) fn run_sessions_op(
    store: &crate::session::SessionStore,
    workspace: &str,
    op: crate::tui::commands::SessionsOp,
) -> Result<String, String> {
    use crate::tui::commands::SessionsOp;
    let resolve = |key: &str| {
        store.id_for(key).map_err(|error| match error {
            crate::session::SessionError::UnknownKey(key) => {
                format!("Unknown session {key}. Run /sessions to list them.")
            }
            other => other.to_string(),
        })
    };

    match op {
        SessionsOp::Fork { id, at } => {
            let source = resolve(&id)?;
            let fork = store
                .fork(workspace, source, at)
                .map_err(|error| error.to_string())?;
            let forked = store.load(fork).map_err(|error| error.to_string())?;
            Ok(format!(
                "Forked session {source} into {} ({} messages).",
                forked.slug,
                forked.messages.len()
            ))
        }
        SessionsOp::Archive { id, archived } => {
            let id = resolve(&id)?;
            store
                .archive(workspace, id, archived)
                .map_err(|error| error.to_string())?;
            Ok(if archived {
                format!("Archived session {id}. It is hidden, not deleted.")
            } else {
                format!("Unarchived session {id}.")
            })
        }
        // The same bounded, literal, workspace-scoped search the CLI prints.
        SessionsOp::Search { query, all } => {
            crate::cli::sessions::search_text(store, workspace, &query, all)
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
        CommandEffect::RunSessionsOp { op } => {
            // Store work never runs on the event loop. A refusal is reported by
            // the same notice as a success: the user needs the reason.
            app.status = AgentState::Checking;
            let tx = tx.clone();
            let workspace = crate::session::workspace_key();
            tokio::task::spawn_blocking(move || {
                let report = crate::session::SessionStore::open()
                    .map_err(|error| error.to_string())
                    .and_then(|store| run_sessions_op(&store, &workspace, op))
                    .unwrap_or_else(|error| error);
                let _ = tx.send(Action::SessionsOpDone(report));
            });
        }
        CommandEffect::RunGoal { action } => {
            let tx = tx.clone();
            let workspace = crate::session::workspace_key();
            tokio::task::spawn_blocking(move || {
                let mut start = None;
                let result = crate::session::SessionStore::open()
                    .map_err(|error| error.to_string())
                    .and_then(|store| run_goal(&store, &workspace, action, &mut start));
                let goal = live_goal_line(&workspace);
                let _ = tx.send(Action::GoalDone {
                    result,
                    goal,
                    start,
                });
            });
        }
        CommandEffect::RunTest => {
            // Discovery is Git + filesystem work: off the event loop. The
            // command is shown and permitted before anything runs.
            app.status = AgentState::Checking;
            let tx = tx.clone();
            tokio::task::spawn_blocking(move || {
                let security = crate::agent::security::WorkspaceSecurity::new(".");
                let changed = match crate::agent::git_review::resolve(
                    security.root(),
                    &crate::agent::git_review::ReviewTarget::WorkingTree,
                    &security,
                ) {
                    Ok(input) => input.changed_paths,
                    Err(error) => {
                        let _ = tx.send(Action::TestDone(format!(
                            "Could not read the working tree: {error}"
                        )));
                        return;
                    }
                };
                let root_has_cargo = security.root().join("Cargo.toml").is_file();
                let Some((command, evidence)) =
                    crate::tui::commands::propose_test(&changed, root_has_cargo)
                else {
                    let _ = tx.send(Action::TestDone(format!(
                        "No manifest in the {} changed file(s) declares tests, so no command is proposed. Run one explicitly with the agent.",
                        changed.len()
                    )));
                    return;
                };
                let _ = tx.send(Action::TestProposed {
                    command,
                    evidence,
                    changed: changed.len(),
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
        CommandEffect::OpenSessionPicker => {
            // The listing is a SQLite read: same off-thread path as every other
            // store call, so opening the picker cannot freeze the input thread.
            app.status = AgentState::Checking;
            let tx = tx.clone();
            let workspace = crate::session::workspace_key();
            tokio::task::spawn_blocking(move || {
                let rows = crate::session::SessionStore::open()
                    .map_err(|error| error.to_string())
                    .and_then(|store| crate::cli::sessions::session_rows(&store, &workspace));
                let _ = tx.send(Action::SessionPickerOpened(rows));
            });
        }
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
