use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::{App, ModalSelection};

/// Wrap-around previous index; no-op-safe when the list is empty.
pub(super) fn prev_index(sel: usize, len: usize) -> usize {
    if sel == 0 {
        len.saturating_sub(1)
    } else {
        sel - 1
    }
}

/// Wrap-around next index; no-op-safe when the list is empty.
pub(super) fn next_index(sel: usize, len: usize) -> usize {
    if sel + 1 >= len { 0 } else { sel + 1 }
}

impl App<'_> {
    /// Open the cached-model picker. Returns `false` when the cache is empty.
    pub fn open_model_picker(&mut self) -> bool {
        let models = cached_models(self);
        if models.is_empty() {
            self.set_text("No cached models found. Run `seavx model` once to populate cache.");
            self.scroll = 0;
            self.auto_scroll = true;
            return false;
        }

        let current_idx = models
            .iter()
            .position(|m| *m == self.config.model)
            .unwrap_or(0);
        self.modal = Some(ModalSelection::Model {
            models,
            query: String::new(),
            selected: current_idx,
        });
        true
    }

    /// Move the open model picker's cursor by one row, wrapping. Arrows and the
    /// wheel both land here, so the cursor never leaves the filtered rows.
    /// List every preset with its first lines, so the picker can preview one
    /// before it is applied. Reading happens once, here.
    pub fn open_preset_picker(&mut self) -> bool {
        let found = crate::agent::presets::list(&self.root);
        if found.is_empty() {
            self.push_notice(
                "No presets yet. Add .seavx/prompts/<name>.md (project) or ~/.seavx/prompts/<name>.md (everywhere)."
                    .to_string(),
            );
            return false;
        }
        let mut names = Vec::with_capacity(found.len());
        let mut previews = Vec::with_capacity(found.len());
        for (name, _path) in found {
            let preview = match crate::agent::presets::load(&self.root, &name) {
                Ok(text) => preset_preview(&text),
                // A preset that cannot be read is still listed: the reason is
                // what the user needs to fix, and hiding the row loses it.
                Err(error) => error,
            };
            names.push(name);
            previews.push(preview);
        }
        self.modal = Some(ModalSelection::Preset {
            names,
            previews,
            query: String::new(),
            selected: 0,
        });
        true
    }

    /// Put one preset in the next prompt. Shared by `/preset <name>` and the
    /// picker, so both say the same thing and neither touches the policy.
    pub fn apply_preset(&mut self, name: &str) {
        match crate::agent::presets::load(&self.root, name) {
            Ok(text) => {
                let preview: Vec<&str> = text.lines().take(3).collect();
                self.set_input(&text);
                self.push_notice(format!(
                    "Preset '{name}' loaded into the prompt:\n{}",
                    preview.join("\n")
                ));
            }
            Err(error) => self.push_notice(error),
        }
    }

    /// Copy one Markdown file into the project presets, so `/preset <name>`
    /// can use it. Nothing is applied: the user decides when.
    pub fn add_preset(&mut self, path: &str) {
        match crate::agent::presets::add(&self.root, path) {
            Ok((name, target)) => self.push_notice(format!(
                "Added preset '{name}' at {}.\n/preset {name} puts it in the next prompt.",
                target.display()
            )),
            Err(error) => self.push_notice(error),
        }
    }

    /// Move inside the preset picker, wrapping over the visible rows.
    pub fn step_preset_picker(&mut self, down: bool) {
        let Some(ModalSelection::Preset {
            names,
            previews,
            query,
            selected,
        }) = self.modal.take()
        else {
            return;
        };
        let len = visible_presets(&names, &query).len();
        let selected = if down {
            next_index(selected, len)
        } else {
            prev_index(selected, len)
        };
        self.modal = Some(ModalSelection::Preset {
            names,
            previews,
            query,
            selected,
        });
    }

    pub fn step_model_picker(&mut self, down: bool) {
        let Some(ModalSelection::Model {
            models,
            query,
            selected,
        }) = self.modal.take()
        else {
            return;
        };
        let len = visible_models(&models, &query).len();
        let selected = if down {
            next_index(selected, len)
        } else {
            prev_index(selected, len)
        };
        self.modal = Some(ModalSelection::Model {
            models,
            query,
            selected,
        });
    }

    /// Land a finished `/models` refresh: the spinner stops and the picker shows
    /// the new list. A picker closed while the fetch ran stays closed, and only
    /// the toast reports the outcome.
    pub fn models_refreshed(&mut self, result: Result<Vec<String>, String>) {
        self.refreshing_models = false;
        match result {
            Ok(models) => {
                let count = models.len();
                // A refresh keeps the filter the user typed; the cursor follows
                // the active model through the same filtered list. Only a model
                // picker is replaced: an effort picker opened meanwhile stays.
                let query = match &self.modal {
                    Some(ModalSelection::Model { query, .. }) => Some(query.clone()),
                    _ => None,
                };
                if let Some(query) = query {
                    let selected = visible_models(&models, &query)
                        .iter()
                        .position(|model| **model == self.config.model)
                        .unwrap_or(0);
                    self.modal = Some(ModalSelection::Model {
                        models,
                        query,
                        selected,
                    });
                }
                self.toast = Some((
                    format!("✓ Refreshed {count} models from gateway"),
                    std::time::Instant::now(),
                ));
            }
            Err(error) => {
                self.toast = Some((
                    format!("✗ Refresh failed: {error}"),
                    std::time::Instant::now(),
                ));
            }
        }
    }

    /// Open the session picker on rows already read off the event loop.
    /// Returns `false` when there is nothing to pick, so the caller can say so
    /// instead of opening an empty modal.
    pub fn show_session_picker(&mut self, rows: Vec<String>) -> bool {
        if rows.is_empty() {
            self.push_notice("No sessions yet. Start one by sending a prompt.");
            return false;
        }
        self.modal = Some(ModalSelection::Session { rows, selected: 0 });
        true
    }

    /// Switch the model, optionally with a reasoning effort, and persist it.
    pub fn set_model(&mut self, target: &str, effort: Option<&str>) {
        let models = cached_models(self);
        let resolved_model = match resolve_model_target(&models, target) {
            Ok(model) => model,
            Err(error) => {
                self.push_notice(error);
                return;
            }
        };

        let config_path = crate::cli::config::workspace_config_path(std::path::Path::new("."));
        // One write: the model and the effort change together or not at all.
        let mut errors = Vec::new();
        if let Err(error) =
            crate::cli::config::set_model_and_effort(&config_path, &resolved_model, effort)
        {
            errors.push(error);
        }

        self.config = crate::agent::engine::AgentConfig::load(std::path::Path::new("."));

        let mut message = format!(
            "Switched model to: {}\nEffort: {}\nSaved to .seavx/config.toml",
            self.config.model,
            self.config.reasoning_effort.as_deref().unwrap_or("none")
        );
        for error in errors {
            message.push_str(&format!("\n{error}"));
        }

        self.set_text(message);
        self.scroll = 0;
        self.auto_scroll = true;
    }
}

/// Case-insensitive substring match for the model picker's filter.
fn model_matches(name: &str, query: &str) -> bool {
    query.is_empty() || name.to_lowercase().contains(&query.to_lowercase())
}

/// The rows the model picker shows: the names containing the query. Rendering
/// and Enter both read this, so the cursor always lands on the row on screen.
pub fn visible_models<'a>(models: &'a [String], query: &str) -> Vec<&'a String> {
    models
        .iter()
        .filter(|model| model_matches(model, query))
        .collect()
}

/// Preset rows a query keeps, in list order. The search box filters the names
/// the same way every site that draws or selects reads them.
pub fn visible_presets(names: &[String], query: &str) -> Vec<usize> {
    let wanted = query.trim().to_ascii_lowercase();
    names
        .iter()
        .enumerate()
        .filter(|(_, name)| wanted.is_empty() || name.to_ascii_lowercase().contains(&wanted))
        .map(|(index, _)| index)
        .collect()
}

/// The lines a picker shows for one preset: enough to recognise it, never the
/// whole file, and one line per row.
fn preset_preview(text: &str) -> String {
    const LINES: usize = 12;
    let mut preview: Vec<&str> = text.lines().take(LINES).collect();
    let more = text.lines().count() > LINES;
    let mut out = preview.join("\n");
    preview.clear();
    if more {
        out.push_str("\n...");
    }
    out
}

/// The picker's search box: a printable key appends, Backspace deletes one
/// character. `None` means the key is not text, so the caller keeps it.
pub(super) fn edit_query(query: &str, key: &KeyEvent) -> Option<String> {
    if key.modifiers.contains(KeyModifiers::CONTROL) || key.modifiers.contains(KeyModifiers::ALT) {
        return None;
    }
    match key.code {
        KeyCode::Backspace => {
            let mut trimmed = query.to_string();
            trimmed.pop();
            Some(trimmed)
        }
        KeyCode::Char(c) => Some(format!("{query}{c}")),
        _ => None,
    }
}

/// Cached models for the configured gateway; empty when the cache is cold.
/// A cache past its TTL still answers: an old list beats an empty picker, and
/// `seavx model` is what refreshes it.
fn cached_models(app: &App) -> Vec<String> {
    let base_url = app
        .config
        .base_url
        .as_deref()
        .unwrap_or(crate::agent::engine::DEFAULT_BASE_URL);
    let model_mgr = crate::agent::models::ModelManager::new(std::path::Path::new("."));
    model_mgr
        .get_cached_models_allow_stale(&crate::agent::models::ModelManager::domain_slug(base_url))
        .unwrap_or_default()
}

/// Resolve a `/model` argument: a 1-based index into the cached list, or a
/// literal model name. An index outside the list is an error, never a name —
/// `/model 9` against three cached models must not save `model = "9"`.
pub(super) fn resolve_model_target(models: &[String], target: &str) -> Result<String, String> {
    let Ok(number) = target.parse::<usize>() else {
        return Ok(target.to_string());
    };
    number
        .checked_sub(1)
        .and_then(|index| models.get(index))
        .cloned()
        .ok_or_else(|| {
            format!(
                "No model number {number} in the cached list ({} known). Run `seavx model` to refresh it.",
                models.len()
            )
        })
}
