use tokio::sync::mpsc;

use super::store::lock_store;
use super::{App, ModalSelection, PendingPermission};
use crate::agent::events::ToolStatus;
use crate::tui::event::{Action, AgentState};

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
        Action::GoalDone { result, goal } => {
            app.goal = goal;
            match result {
                Ok(text) => app.push_notice(text),
                Err(error) => app.push_notice(error),
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
        } => app.finish_turn(status, cleanup, pid),
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
