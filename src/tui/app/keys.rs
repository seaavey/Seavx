use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use tokio::sync::mpsc;

use super::modal::{next_index, prev_index};
use super::{
    App, EnterAction, ModalSelection, apply_command_effect, edit_query, new_input,
    run_agent_worker, spawn_session_resume, visible_models,
};
use crate::agent::engine::EFFORT_LEVELS;
use crate::tui::event::Action;

pub(super) fn is_mode_shortcut(key: &KeyEvent) -> bool {
    key.code == KeyCode::BackTab
        || (key.code == KeyCode::Tab && key.modifiers.contains(KeyModifiers::CONTROL))
}

/// Whether a key edits the prompt text (as opposed to only moving the cursor).
/// Only these regrow the ghost suggestion after the raw key is applied.
pub(super) fn edits_text(key: &KeyEvent) -> bool {
    match key.code {
        KeyCode::Char(_) => {
            !key.modifiers.contains(KeyModifiers::CONTROL)
                && !key.modifiers.contains(KeyModifiers::ALT)
        }
        KeyCode::Backspace | KeyCode::Enter | KeyCode::Tab => true,
        _ => false,
    }
}

pub(super) async fn handle(app: &mut App<'_>, key: KeyEvent, tx: &mpsc::UnboundedSender<Action>) {
    // Quitting is never blocked. A pending question is answered by
    // dropping its reply, which the confirmer treats as a denial, so
    // Ctrl+D cannot approve anything on the way out.
    if quit_shortcut(app, &key) {
        return;
    }

    // A pending question owns the next key: y/a/u answer it, n/Esc
    // denies. Nothing else runs until it is resolved.
    if app.pending_permission.is_some() {
        match app.answer_permission(key.code) {
            Some(answer) => {
                use crate::agent::permissions::Answer;
                let label = match answer {
                    Answer::Once => "approved once",
                    Answer::Category => "approved for this category",
                    Answer::Unrestricted => "switched to allow",
                    Answer::Deny => "denied",
                };
                app.push_notice(format!("Permission {label}."));
            }
            None => app.push_notice("Answer the permission prompt: y / a / u / n"),
        }
        return;
    }

    // Modal Key Interceptor (Arrow selection + Enter)
    if let Some(modal) = app.modal.clone() {
        match modal {
            ModalSelection::Model {
                models,
                query,
                selected,
            } => {
                // Typing is the search box, so 'j'/'k' no longer
                // move: arrows do.
                if let Some(query) = edit_query(&query, &key) {
                    app.modal = Some(ModalSelection::Model {
                        models,
                        query,
                        selected: 0,
                    });
                    return;
                }
                match key.code {
                    KeyCode::Esc => {
                        app.modal = None;
                    }
                    KeyCode::Up => app.step_model_picker(false),
                    KeyCode::Down => app.step_model_picker(true),
                    KeyCode::Char('r')
                        if key.modifiers.contains(KeyModifiers::CONTROL)
                            && !app.refreshing_models =>
                    {
                        // The fetch runs off the UI thread: awaiting it
                        // here froze the terminal for its whole timeout.
                        app.refreshing_models = true;
                        let base_url =
                            app.config.base_url.clone().unwrap_or_else(|| {
                                crate::agent::engine::DEFAULT_BASE_URL.to_string()
                            });
                        let api_key = app.config.api_key.clone();
                        let tx = tx.clone();
                        tokio::spawn(async move {
                            let model_mgr =
                                crate::agent::models::ModelManager::new(std::path::Path::new("."));
                            let outcome = model_mgr
                                .refresh_remote_models(&base_url, api_key.as_deref())
                                .await;
                            let _ = tx.send(Action::ModelsRefreshed(outcome));
                        });
                    }
                    KeyCode::Enter => {
                        let chosen = visible_models(&models, &query)
                            .get(selected)
                            .map(|model| (*model).clone());
                        let options = EFFORT_LEVELS.to_vec();
                        let current_eff = app.config.reasoning_effort.as_deref().unwrap_or("none");
                        let eff_sel = options.iter().position(|o| *o == current_eff).unwrap_or(0);
                        app.modal = chosen.map(|model| ModalSelection::Effort {
                            model,
                            options,
                            selected: eff_sel,
                        });
                    }
                    _ => {}
                }
            }
            ModalSelection::Effort {
                model,
                options,
                selected,
            } => match key.code {
                KeyCode::Esc => {
                    app.modal = None;
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    let new_sel = prev_index(selected, options.len());
                    app.modal = Some(ModalSelection::Effort {
                        model,
                        options,
                        selected: new_sel,
                    });
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    let new_sel = next_index(selected, options.len());
                    app.modal = Some(ModalSelection::Effort {
                        model,
                        options,
                        selected: new_sel,
                    });
                }
                KeyCode::Enter => {
                    let chosen_effort = options[selected].to_string();
                    app.config.model = model.clone();
                    app.config.reasoning_effort = Some(chosen_effort.clone());

                    // Persist through the one config writer.
                    let saved = app.config.save(std::path::Path::new("."));

                    let saved_line = match &saved {
                        Ok(()) => "Saved to .seavx/config.toml".to_string(),
                        Err(error) => format!("Failed to save config: {error}"),
                    };
                    app.set_text(format!(
                        "Switched model to: {}\nReasoning effort: {}\n{saved_line}",
                        app.config.model,
                        app.config.reasoning_effort.as_deref().unwrap_or("none")
                    ));
                    app.scroll = 0;
                    app.auto_scroll = true;
                    app.modal = None;
                }
                _ => {}
            },
            ModalSelection::Session { rows, selected } => match key.code {
                KeyCode::Esc => {
                    app.modal = None;
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    let new_sel = prev_index(selected, rows.len());
                    app.modal = Some(ModalSelection::Session {
                        rows,
                        selected: new_sel,
                    });
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    let new_sel = next_index(selected, rows.len());
                    app.modal = Some(ModalSelection::Session {
                        rows,
                        selected: new_sel,
                    });
                }
                KeyCode::Enter => {
                    app.modal = None;
                    // Rows are the `/sessions` order, so the index is the number.
                    if selected < rows.len() {
                        spawn_session_resume(app, tx, (selected + 1) as u32);
                    }
                }
                _ => {}
            },
            ModalSelection::Skills {
                names,
                descriptions,
                selected,
                mut marked,
            } => match key.code {
                // Cancelling drops the checkout: nothing is copied.
                KeyCode::Esc => {
                    app.modal = None;
                    app.pending_skills = None;
                }
                KeyCode::Up => {
                    let selected = prev_index(selected, names.len());
                    app.modal = Some(ModalSelection::Skills {
                        names,
                        descriptions,
                        selected,
                        marked,
                    });
                }
                KeyCode::Down => {
                    let selected = next_index(selected, names.len());
                    app.modal = Some(ModalSelection::Skills {
                        names,
                        descriptions,
                        selected,
                        marked,
                    });
                }
                KeyCode::Char(' ') => {
                    if let Some(mark) = marked.get_mut(selected) {
                        *mark = !*mark;
                    }
                    app.modal = Some(ModalSelection::Skills {
                        names,
                        descriptions,
                        selected,
                        marked,
                    });
                }
                KeyCode::Char('a') => {
                    marked.fill(true);
                    app.modal = Some(ModalSelection::Skills {
                        names,
                        descriptions,
                        selected,
                        marked,
                    });
                }
                KeyCode::Enter => {
                    app.modal = None;
                    let mut chosen: Vec<usize> = marked
                        .iter()
                        .enumerate()
                        .filter(|(_, mark)| **mark)
                        .map(|(index, _)| index)
                        .collect();
                    // Nothing marked installs the row under the cursor.
                    if chosen.is_empty() && selected < names.len() {
                        chosen.push(selected);
                    }
                    let _ = tx.send(Action::SkillsInstallMarked(chosen));
                }
                _ => {}
            },
        }
        return;
    }

    if is_mode_shortcut(&key) {
        app.toggle_mode();
        return;
    }

    let at_matches = app.at_matches();

    if !app.popups_dismissed() && !at_matches.is_empty() {
        match key.code {
            KeyCode::Up => {
                app.at_selected = prev_index(app.at_selected, at_matches.len());
                return;
            }
            KeyCode::Down => {
                app.at_selected = next_index(app.at_selected, at_matches.len());
                return;
            }
            KeyCode::Tab | KeyCode::Enter => {
                let (is_dir, path) = &at_matches[app.at_selected.min(at_matches.len() - 1)];
                app.insert_at_tag(*is_dir, path);
                app.at_selected = 0;
                return;
            }
            _ => {}
        }
    }

    let filtered_cmds = app.slash_matches();

    if !app.popups_dismissed() && !filtered_cmds.is_empty() {
        match key.code {
            KeyCode::Up => {
                app.slash_selected = prev_index(app.slash_selected, filtered_cmds.len());
                return;
            }
            KeyCode::Down => {
                app.slash_selected = next_index(app.slash_selected, filtered_cmds.len());
                return;
            }
            KeyCode::Tab => {
                let (selected_cmd, _) =
                    filtered_cmds[app.slash_selected.min(filtered_cmds.len() - 1)];
                app.input = new_input();
                app.input.insert_str(format!("{selected_cmd} "));
                app.slash_selected = 0;
                app.refresh_ghost();
                return;
            }
            KeyCode::Enter => {
                let (selected_cmd, _) =
                    filtered_cmds[app.slash_selected.min(filtered_cmds.len() - 1)];
                app.input = new_input();
                if let Some(command) = crate::tui::commands::parse(selected_cmd) {
                    let effect = crate::tui::commands::execute(app, command);
                    apply_command_effect(app, effect, tx);
                }
                return;
            }
            _ => {}
        }
    }

    match key.code {
        KeyCode::Esc => app.escape(),
        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            app.should_quit = true;
        }
        KeyCode::PageUp => app.scroll_up(5),
        KeyCode::PageDown => app.scroll_down(5),
        KeyCode::Up if key.modifiers.contains(KeyModifiers::SHIFT) => {
            app.scroll_up(2);
        }
        KeyCode::Down if key.modifiers.contains(KeyModifiers::SHIFT) => {
            app.scroll_down(2);
        }
        // Single-line prompt: arrows recall history, filtered by the typed prefix.
        KeyCode::Up if app.input.lines().len() == 1 => app.history_previous(),
        KeyCode::Down if app.input.lines().len() == 1 => app.history_next(),
        // Accept or eat the ghost suggestion at the end of the line.
        KeyCode::Tab => {
            if app.ghost_active() {
                // Accept: the tail becomes part of the prompt.
                app.set_input(&app.input.lines()[0].clone());
                app.refresh_ghost();
            } else {
                app.reset_history_nav();
                app.input.input(key);
            }
        }
        KeyCode::Right => {
            if app.ghost_active() {
                app.set_input(&app.input.lines()[0].clone());
                app.refresh_ghost();
            } else {
                app.clear_ghost();
                app.input.input(key);
            }
        }
        KeyCode::Delete => {
            if app.ghost_active() {
                // First Delete swallows the suggestion, like fish.
                app.clear_ghost();
            } else {
                app.reset_history_nav();
                app.input.input(key);
            }
        }
        KeyCode::Home => {
            app.auto_scroll = false;
            app.scroll = 0;
        }
        KeyCode::End => {
            app.auto_scroll = true;
        }
        KeyCode::Enter if !key.modifiers.contains(KeyModifiers::SHIFT) => {
            let lines = app.input.lines().to_vec();
            let prompt = lines.join("\n");
            match app.enter_action(&prompt) {
                EnterAction::Command(command) => {
                    let effect = crate::tui::commands::execute(app, command);
                    apply_command_effect(app, effect, tx);
                }
                EnterAction::Nothing => {}
                // One Enter: one request, one new user row. The
                // worker writes the row before it sends.
                EnterAction::Turn(prompt) => {
                    if let Some(request) = app.submit_turn(&prompt) {
                        let is_first_turn = app.session_title.is_none();
                        run_agent_worker(
                            request,
                            tx.clone(),
                            is_first_turn,
                            app.mode,
                            app.session_context.clone(),
                        )
                        .await;
                    }
                }
            }
        }
        _ => {
            app.reset_history_nav();
            // Cursor moves dismiss the ghost; edits regrow it.
            app.clear_ghost();
            app.input.input(key);
            if edits_text(&key) {
                app.refresh_ghost();
            }
        }
    }
}

/// The three ways out of the TUI, checked before anything that could swallow a
/// key: `Ctrl+D`/`Ctrl+Q` quit at once, `Ctrl+C` needs a second press within
/// 1.5 s. Returns `true` when the key was consumed.
pub(super) fn quit_shortcut(app: &mut App, key: &KeyEvent) -> bool {
    if !key.modifiers.contains(KeyModifiers::CONTROL) {
        return false;
    }
    match key.code {
        KeyCode::Char('d') | KeyCode::Char('q') => {
            app.should_quit = true;
            true
        }
        KeyCode::Char('c') => {
            if let Some(prev) = app.last_ctrl_c
                && prev.elapsed() < std::time::Duration::from_millis(1500)
            {
                app.should_quit = true;
                return true;
            }
            app.last_ctrl_c = Some(std::time::Instant::now());
            app.toast = Some((
                "Press Ctrl+C again to exit".to_string(),
                std::time::Instant::now(),
            ));
            true
        }
        _ => false,
    }
}
