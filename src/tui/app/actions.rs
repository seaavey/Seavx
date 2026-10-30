use tokio::sync::mpsc;

use super::store::lock_store;
use super::{App, PendingPermission};
use crate::tui::event::{Action, AgentState};

/// Apply one result the loop did not route to a key or mouse handler.
pub(super) fn apply(app: &mut App, action: Action, _tx: &mpsc::UnboundedSender<Action>) {
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
        Action::AgentToolStart(tool) => {
            app.status = AgentState::ExecutingTool(tool.clone());
            // Clean compact Hermes-style tool call indicator without brackets
            app.push_text(&format!("\n⚡ {tool}\n\n"));
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
