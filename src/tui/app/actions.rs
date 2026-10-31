use tokio::sync::mpsc;

use super::store::lock_store;
use super::{App, ModalSelection, PendingPermission, PendingQuestion};
use crate::agent::events::ToolStatus;
use crate::tui::event::{Action, AgentState};

/// The matrix answer for the command `/test` proposes: the same `decide` the
/// agent loop uses, so `tool_permissions` governs both. Kept out of the spawn so
/// the policy is testable without a terminal.
pub(super) fn test_verdict(
    config: &crate::agent::engine::AgentConfig,
    kind: crate::agent::permissions::ActionKind,
) -> crate::agent::permissions::Verdict {
    use crate::agent::permissions::{PermissionMode, decide, parse_mode, parse_permitted};
    let mode = parse_mode(&config.tool_permissions).unwrap_or(PermissionMode::Ask);
    let permitted = parse_permitted(&config.permitted_actions).unwrap_or_default();
    decide(mode, &permitted, kind)
}

/// Apply one result the loop did not route to a key or mouse handler.
pub(super) fn apply(app: &mut App, action: Action, tx: &mpsc::UnboundedSender<Action>) {
    match action {
        // Routed by `run`: the loop needs the terminal for these three.
        Action::Tick => {}
        Action::Key(_) => {}
        Action::Mouse(_) => {}
        Action::AgentToken(token) => {
            app.status = AgentState::Streaming;
            app.push_text(&token);
            if app.auto_scroll {
                let lines = app.line_count();
                // Keep pinned to latest lines
                app.scroll = lines;
            }
        }
        Action::AgentThinking(thoughts) => {
            app.push_thinking(&thoughts);
            if app.auto_scroll {
                app.scroll = app.line_count();
            }
        }
        Action::AgentTitle { title, session } => {
            // The title is stored against the session that asked for it,
            // and only shown when that session is still the open one.
            if let (Some(store), Some(id)) = (app.store.clone(), session) {
                let stored = title.clone();
                tokio::task::spawn_blocking(move || {
                    let guard = lock_store(&store);
                    if let Err(error) = guard.record_title(id, &stored) {
                        tracing::warn!(%error, "could not store the session title");
                    }
                });
            }
            app.apply_title(session, title);
        }
        Action::AgentUsage {
            total_tokens,
            prompt_tokens,
        } => {
            app.total_tokens = total_tokens;
            app.prompt_tokens = prompt_tokens;
            if let (Some(store), Some(id)) = (app.store.clone(), app.session_id) {
                tokio::task::spawn_blocking(move || {
                    let guard = lock_store(&store);
                    if let Err(error) =
                        guard.set_usage(id, total_tokens as i64, prompt_tokens as i64)
                    {
                        tracing::warn!(%error, "could not store session usage");
                    }
                });
            }
        }
        Action::SessionStarted(id) => {
            app.session_id = Some(id);
        }
        Action::SessionFailed(message) => {
            app.store = None;
            app.persistence_error = Some(message.clone());
            app.push_notice(message);
        }
        Action::GoalDone {
            result,
            goal,
            start,
        } => {
            app.goal = goal;
            match result {
                Ok(text) => app.push_notice(text),
                Err(error) => app.push_notice(error),
            }
            if let Some(task) = start
                && let Some(request) = app.submit_turn(&task)
            {
                let is_first_turn = app.session_title.is_none();
                let context = app.session_context.clone();
                let tx = tx.clone();
                tokio::spawn(async move {
                    crate::tui::app::worker::run_agent_worker(
                        request,
                        tx,
                        is_first_turn,
                        crate::agent::engine::AgentMode::Build,
                        context,
                    )
                    .await;
                });
            }
        }
        Action::ReviewResolved(result) => {
            app.status = AgentState::Idle;
            match result {
                Ok(input) => {
                    if input.withheld_paths > 0 {
                        app.push_notice(format!(
                            "{} path(s) withheld from this review for security.",
                            input.withheld_paths
                        ));
                    }
                    let task = crate::agent::git_review::task_text(&input);
                    if let Some(request) = app.submit_turn(&task) {
                        let is_first_turn = app.session_title.is_none();
                        let context = app.session_context.clone();
                        let tx = tx.clone();
                        tokio::spawn(async move {
                            crate::tui::app::worker::run_agent_worker(
                                request,
                                tx,
                                is_first_turn,
                                crate::agent::engine::AgentMode::Review,
                                context,
                            )
                            .await;
                        });
                    }
                }
                Err(error) => app.push_notice(format!("Review could not start: {error}")),
            }
        }
        Action::AgentTool(tool) => {
            let label = format!("{}({})", tool.name, tool.summary);
            match tool.status {
                ToolStatus::Queued => {
                    app.status = AgentState::ExecutingTool(label.clone());
                    // Clean compact Hermes-style tool call indicator without brackets
                    app.push_text(&format!("\n⚡ {label}\n\n"));
                }
                ToolStatus::Running => app.status = AgentState::ExecutingTool(label),
                // The outcome line: the status word is the ledger's, so a denied
                // or cancelled call can never read as completed here.
                ToolStatus::Completed | ToolStatus::Failed | ToolStatus::Cancelled => {
                    let elapsed = tool
                        .elapsed
                        .map(|elapsed| format!(" {:.1}s", elapsed.as_secs_f32()))
                        .unwrap_or_default();
                    let failure = tool
                        .failure
                        .map(|failure| format!(" · {}", failure.as_str()))
                        .unwrap_or_default();
                    app.push_text(&format!("  └ {}{elapsed}{failure}\n", tool.status.as_str()));
                }
            }
            if app.auto_scroll {
                let lines = app.line_count();
                app.scroll = lines;
            }
        }
        Action::TurnFinished {
            status,
            cleanup,
            pid,
        } => {
            app.finish_turn(status, cleanup, pid);
            // A queued prompt runs only after a turn that succeeded. A cancel or
            // a failure drops it: replaying it would repeat work the user just
            // stopped, and the queue lives in memory only.
            if matches!(status, crate::agent::cancel::TurnStatus::Completed)
                && let Some(prompt) = app.queued_prompt.take()
                && let Some(request) = app.submit_turn(&prompt)
            {
                let is_first_turn = app.session_title.is_none();
                let context = app.session_context.clone();
                let tx = tx.clone();
                tokio::spawn(async move {
                    crate::tui::app::worker::run_agent_worker(
                        request,
                        tx,
                        is_first_turn,
                        crate::agent::engine::AgentMode::Build,
                        context,
                    )
                    .await;
                });
            } else if !matches!(status, crate::agent::cancel::TurnStatus::Completed)
                && app.queued_prompt.take().is_some()
            {
                app.push_notice("Queued prompt dropped: the turn did not succeed.");
            }
            // A goal turn is done when its turn is: the tasks it ran are
            // recorded as completed with what actually happened, never with a
            // claim the turn did not make.
            if let Some(goal) = app
                .goal
                .as_deref()
                .and_then(|line| line.split_whitespace().next())
                && let Ok(id) = goal.parse::<i64>()
            {
                let session = app.session_id;
                let evidence = format!(
                    "turn {}; session {}",
                    status.as_str(),
                    session
                        .map(|id| id.to_string())
                        .unwrap_or_else(|| "none".to_string())
                );
                let store = app.store.clone();
                if let Some(store) = store {
                    tokio::task::spawn_blocking(move || {
                        let guard = lock_store(&store);
                        let Ok(goal) = guard.goal(&crate::session::workspace_key(), id) else {
                            return;
                        };
                        for task in goal.tasks {
                            if task.state == crate::session::TaskState::Running {
                                let _ = guard.set_task_state(
                                    &crate::session::workspace_key(),
                                    id,
                                    task.position,
                                    crate::session::TaskState::Completed,
                                    &evidence,
                                );
                            }
                        }
                    });
                }
            }
        }
        Action::AgentDone => {
            app.status = AgentState::Idle;
        }
        Action::PermissionAsk { request, reply } => {
            app.push_notice(format!(
                "Permission required: {} wants to {}.\n  y = once  ·  a = this category  ·  u = unrestricted  ·  n = deny",
                request.tool, request.effect
            ));
            app.pending_permission = Some(PendingPermission { request, reply });
        }
        Action::QuestionAsked {
            id,
            question,
            kind,
            options,
            reply,
        } => {
            app.pending_question = Some(PendingQuestion {
                id,
                question,
                kind,
                options,
                selected: 0,
                chosen: std::collections::BTreeSet::new(),
                text: String::new(),
                reply: Some(reply),
            });
            app.revision += 1;
        }
        Action::ModelsRefreshed(result) => app.models_refreshed(result),
        Action::DoctorDone(report) => {
            app.status = AgentState::Idle;
            app.push_notice(crate::cli::doctor::render(&report));
        }
        Action::SessionResume(result) => {
            app.status = AgentState::Idle;
            match result {
                Ok(session) => app.load_session(session),
                Err(error) => app.push_notice(error),
            }
        }
        Action::SessionPickerOpened(result) => {
            app.status = AgentState::Idle;
            match result {
                Ok(rows) => {
                    app.show_session_picker(rows);
                }
                Err(error) => app.push_notice(format!("Sessions unavailable: {error}")),
            }
        }
        Action::TestProposed {
            command,
            evidence,
            changed,
        } => {
            app.status = AgentState::Idle;
            app.push_notice(format!(
                "Proposed: {command}\n{evidence} ({changed} changed file(s))."
            ));
            let tx = tx.clone();
            tokio::spawn(async move {
                use crate::agent::permissions::Verdict;

                let security = crate::agent::security::WorkspaceSecurity::new(".");
                // The command is classified as what it is, and the matrix — not
                // this handler — decides whether it may run at all.
                let kind = crate::agent::shell::classify(&command, &security).kind;
                let config = crate::agent::engine::AgentConfig::load(security.root());
                match test_verdict(&config, kind) {
                    Verdict::Deny => {
                        let _ = tx.send(Action::TestDone(format!(
                            "`{command}` not run: tool_permissions denies {}.",
                            kind.name()
                        )));
                        return;
                    }
                    Verdict::Allow => {}
                    Verdict::Ask => {
                        let path = crate::cli::config::workspace_config_path(security.root());
                        let confirm = crate::tui::app::TuiConfirmer::new(tx.clone());
                        if let Err(denied) = crate::agent::permissions::approve(
                            &confirm,
                            kind,
                            "run_command",
                            &command,
                            &path,
                        )
                        .await
                        {
                            let _ =
                                tx.send(Action::TestDone(format!("`{command}` not run: {denied}")));
                            return;
                        }
                    }
                }

                let cancel = crate::agent::cancel::CancelToken::new();
                use crate::agent::tools::AgentTool;
                let run = crate::agent::tools::RunCommandTool
                    .execute(
                        serde_json::json!({ "command": command }),
                        &security,
                        &cancel,
                    )
                    .await;
                let text = match run {
                    Ok(text) => {
                        let status = text
                            .lines()
                            .next()
                            .and_then(|line| line.strip_prefix("exit: "))
                            .and_then(|value| value.trim().parse::<i32>().ok());
                        let truncated = text.contains(" chars omitted]");
                        crate::tui::commands::test_report(&command, status, &text, truncated)
                    }
                    Err(error) => format!("`{command}` not run: {error}"),
                };
                let _ = tx.send(Action::TestDone(text));
            });
        }
        Action::TestDone(report) => {
            app.status = AgentState::Idle;
            app.push_notice(report);
        }
        Action::SessionsOpDone(report) => {
            app.status = AgentState::Idle;
            app.push_notice(report);
        }
        Action::UndoDone(result) => {
            app.status = AgentState::Idle;
            match result {
                Ok(paths) if paths.is_empty() => app.push_notice("Nothing to undo."),
                Ok(paths) => {
                    let mut text = format!("Restored {} file(s):", paths.len());
                    for path in paths {
                        text.push_str(&format!("\n{}", path.display()));
                    }
                    app.push_notice(text);
                }
                Err(error) => app.push_notice(format!("Undo failed: {error}")),
            }
        }
        Action::SkillsInstalled(result) => {
            app.status = AgentState::Idle;
            match result {
                Ok(path) => app.push_notice(format!("Installed skill at {}", path.display())),
                Err(error) => app.push_notice(format!("Install failed: {error}")),
            }
        }
        Action::SkillsRemoved(result) => {
            app.status = AgentState::Idle;
            match result {
                Ok(path) => app.push_notice(format!("Removed skill at {}", path.display())),
                Err(error) => app.push_notice(format!("Remove failed: {error}")),
            }
        }
        Action::SkillsInstallMarked(indices) => {
            let Some(fetched) = app.pending_skills.take() else {
                app.push_notice("The fetched skills are gone; run /skills install again.");
                return;
            };
            app.status = AgentState::Checking;
            let tx = tx.clone();
            let root = app.root.clone();
            // The copies walk a checkout: they stay off the UI thread.
            tokio::task::spawn_blocking(move || {
                let results = indices
                    .into_iter()
                    .map(|index| {
                        let name = fetched
                            .candidates
                            .get(index)
                            .map(|candidate| candidate.name.clone())
                            .unwrap_or_else(|| format!("#{}", index + 1));
                        (name, fetched.install(&root, index))
                    })
                    .collect();
                let _ = tx.send(Action::SkillsInstalledBatch(results));
            });
        }
        Action::SkillsFetched(result) => {
            app.status = AgentState::Idle;
            match result {
                Ok(fetched) if fetched.candidates.is_empty() => {
                    app.push_notice("No skills with a SKILL.md in that repository.")
                }
                Ok(fetched) => {
                    let names: Vec<String> =
                        fetched.candidates.iter().map(|c| c.name.clone()).collect();
                    let descriptions: Vec<String> = fetched
                        .candidates
                        .iter()
                        .map(|c| c.description.clone())
                        .collect();
                    let marked = vec![false; names.len()];
                    app.pending_skills = Some(fetched);
                    app.modal = Some(ModalSelection::Skills {
                        names,
                        descriptions,
                        selected: 0,
                        marked,
                    });
                }
                Err(error) => app.push_notice(format!("Fetch failed: {error}")),
            }
        }
        Action::SkillsInstalledBatch(results) => {
            app.status = AgentState::Idle;
            let mut text = String::new();
            let mut installed = 0;
            for (name, result) in &results {
                match result {
                    Ok(path) => {
                        installed += 1;
                        text.push_str(&format!("installed {name} at {}\n", path.display()));
                    }
                    Err(reason) => text.push_str(&format!("refused {name}: {reason}\n")),
                }
            }
            text.push_str(&format!(
                "{installed} of {} skill(s) installed.",
                results.len()
            ));
            app.push_notice(text);
        }
        Action::CompactDone { result } => {
            app.status = AgentState::Idle;
            match result {
                Ok(result) => app.apply_compaction(result),
                Err(error) => {
                    app.push_notice(format!("Compaction failed: {error}"));
                }
            }
        }
    }
}
