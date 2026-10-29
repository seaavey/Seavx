//! Slash-command parsing and dispatch for the TUI.
//!
//! [`parse`] is a pure parser: it only recognises the commands implemented in
//! this commit and never touches the app. [`execute`] applies a command and
//! reports the effects the event loop still has to perform (quitting, opening
//! a modal, or work that needs a spawned task in later commits).

use crate::agent::engine::{AgentConfig, AgentMode};
use crate::cli::config::{self, ConfigAction, ConfigKey};
use crate::cli::memory::{self, MemoryView};
use crate::tui::app::{App, SLASH_COMMANDS};
use crate::tui::event::AgentState;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SlashCommand {
    Model {
        target: Option<String>,
        effort: Option<String>,
    },
    Clear,
    /// Switch the runtime mode; the mode marker already shows the result.
    Plan {
        mode: AgentMode,
    },
    Help,
    Exit,
    Init,
    Doctor {
        offline: bool,
    },
    Compact {
        instructions: Option<String>,
    },
    Config {
        action: ConfigAction,
    },
    Memory {
        view: MemoryView,
    },
    /// Restore files from the newest checkpoint.
    Undo,
    Unknown(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandEffect {
    Handled,
    Quit,
    OpenModelPicker,
    RunDoctor { offline: bool },
    RunCompact { instructions: Option<String> },
    RunUndo,
}

/// Parse one line of input. Returns `None` when the line is not a slash
/// command at all; malformed slash input becomes [`SlashCommand::Unknown`] so
/// the caller can report it instead of sending it to the agent.
pub fn parse(input: &str) -> Option<SlashCommand> {
    let trimmed = input.trim();
    let mut parts = trimmed.split_whitespace();
    let name = parts.next()?;
    let args: Vec<&str> = parts.collect();

    if !name.starts_with('/') || name == "/" {
        return None;
    }

    let command = match name {
        "/help" if args.is_empty() => SlashCommand::Help,
        "/clear" if args.is_empty() => SlashCommand::Clear,
        "/plan" => match args.as_slice() {
            [] | ["on"] => SlashCommand::Plan {
                mode: AgentMode::Plan,
            },
            ["off"] => SlashCommand::Plan {
                mode: AgentMode::Build,
            },
            _ => SlashCommand::Unknown(trimmed.to_string()),
        },
        "/exit" if args.is_empty() => SlashCommand::Exit,
        "/init" if args.is_empty() => SlashCommand::Init,
        "/compact" => SlashCommand::Compact {
            instructions: (!args.is_empty()).then(|| args.join(" ")),
        },
        "/doctor" => match args.as_slice() {
            [] => SlashCommand::Doctor { offline: false },
            ["--offline"] => SlashCommand::Doctor { offline: true },
            _ => SlashCommand::Unknown(trimmed.to_string()),
        },
        "/config" => parse_config(&args, trimmed),
        "/undo" if args.is_empty() => SlashCommand::Undo,
        "/memory" if args.len() <= 1 => match args.first().copied() {
            None | Some("show") => SlashCommand::Memory {
                view: MemoryView::All,
            },
            Some("project") => SlashCommand::Memory {
                view: MemoryView::Project,
            },
            Some("user") => SlashCommand::Memory {
                view: MemoryView::User,
            },
            Some("skills") => SlashCommand::Memory {
                view: MemoryView::Skills,
            },
            Some("path") => SlashCommand::Memory {
                view: MemoryView::Path,
            },
            Some(_) => SlashCommand::Unknown(trimmed.to_string()),
        },
        "/model" if args.len() <= 2 => {
            // `list` is the historical way to ask for the picker.
            if args.first() == Some(&"list") && args.len() == 1 {
                SlashCommand::Model {
                    target: None,
                    effort: None,
                }
            } else {
                SlashCommand::Model {
                    target: args.first().map(|arg| arg.to_string()),
                    effort: args.get(1).map(|arg| arg.to_string()),
                }
            }
        }
        _ => SlashCommand::Unknown(trimmed.to_string()),
    };

    Some(command)
}

/// `/config` has its own grammar: only `set`/`unset` accept `--global`, and
/// only immediately after the verb, so a value may contain the word itself.
fn parse_config(args: &[&str], raw: &str) -> SlashCommand {
    let unknown = || SlashCommand::Unknown(raw.to_string());

    match args {
        [] => SlashCommand::Config {
            action: ConfigAction::Show,
        },
        ["path"] => SlashCommand::Config {
            action: ConfigAction::Path,
        },
        ["get", key] => SlashCommand::Config {
            action: ConfigAction::Get {
                key: (*key).to_string(),
            },
        },
        ["set", rest @ ..] => {
            let (global, rest) = match rest {
                ["--global", rest @ ..] => (true, rest),
                _ => (false, rest),
            };
            match rest {
                [key, values @ ..] if !values.is_empty() => SlashCommand::Config {
                    action: ConfigAction::Set {
                        key: (*key).to_string(),
                        value: values.join(" "),
                        global,
                    },
                },
                _ => unknown(),
            }
        }
        ["unset", rest @ ..] => {
            let (global, rest) = match rest {
                ["--global", rest @ ..] => (true, rest),
                _ => (false, rest),
            };
            match rest {
                [key] => SlashCommand::Config {
                    action: ConfigAction::Unset {
                        key: (*key).to_string(),
                        global,
                    },
                },
                _ => unknown(),
            }
        }
        _ => unknown(),
    }
}

/// Apply a parsed command. Anything that needs the async event loop is
/// returned as an effect instead of being performed here.
pub fn execute(app: &mut App, command: SlashCommand) -> CommandEffect {
    // Help and exit stay available while the agent is working.
    match command {
        SlashCommand::Help => {
            app.push_notice(help_text());
            return CommandEffect::Handled;
        }
        SlashCommand::Exit => return CommandEffect::Quit,
        // Mode switching reports its own refusal, so it skips the busy gate.
        SlashCommand::Plan { mode } => {
            app.request_mode(mode);
            return CommandEffect::Handled;
        }
        _ => {}
    }

    if app.status != AgentState::Idle {
        app.push_notice("Agent is busy, try again when it is idle.");
        return CommandEffect::Handled;
    }

    match command {
        SlashCommand::Clear => {
            app.clear_transcript();
            app.session_title = None;
            app.scroll = 0;
            app.auto_scroll = true;
            CommandEffect::Handled
        }
        SlashCommand::Model { target, effort } => match target {
            Some(target) => {
                app.set_model(&target, effort.as_deref());
                CommandEffect::Handled
            }
            None => CommandEffect::OpenModelPicker,
        },
        SlashCommand::Memory { view } => {
            let report = memory::build_report(std::path::Path::new("."));
            app.push_notice(memory::render_text(&report, view));
            CommandEffect::Handled
        }
        SlashCommand::Config { action } => {
            execute_config(app, action);
            CommandEffect::Handled
        }
        SlashCommand::Doctor { offline } => CommandEffect::RunDoctor { offline },
        SlashCommand::Compact { instructions } => CommandEffect::RunCompact { instructions },
        SlashCommand::Undo => CommandEffect::RunUndo,
        SlashCommand::Init => {
            match config::init_workspace(std::path::Path::new(".")) {
                Ok((path, true)) => {
                    app.config = AgentConfig::load(std::path::Path::new("."));
                    app.push_notice(format!("Created default config at {}", path.display()));
                }
                Ok((path, false)) => {
                    app.push_notice(format!("Config already exists at {}", path.display()));
                }
                Err(error) => app.push_notice(format!("Failed to create config: {error}")),
            }
            CommandEffect::Handled
        }
        SlashCommand::Unknown(input) => {
            app.push_notice(format!(
                "Unknown command: {input}. Type /help for the list."
            ));
            CommandEffect::Handled
        }
        // Handled before the busy gate above.
        SlashCommand::Help | SlashCommand::Exit | SlashCommand::Plan { .. } => {
            CommandEffect::Handled
        }
    }
}

fn execute_config(app: &mut App, action: ConfigAction) {
    let workspace = std::path::Path::new(".");

    match action {
        ConfigAction::Show => {
            let (config, report) = AgentConfig::load_with_report(workspace);
            app.push_notice(config::render_report(&config, &report));
        }
        ConfigAction::Path => {
            let (_, report) = AgentConfig::load_with_report(workspace);
            app.push_notice(config::render_paths(&report));
        }
        ConfigAction::Get { key } => {
            let raw_key = key.clone();
            let Some(key) = ConfigKey::parse(&key) else {
                app.push_notice(unknown_key_message(&raw_key));
                return;
            };
            let (config, report) = AgentConfig::load_with_report(workspace);
            app.push_notice(config::render_value(&config, &report, key));
        }
        ConfigAction::Set { key, value, global } => {
            let raw_key = key.clone();
            let Some(key) = ConfigKey::parse(&key) else {
                app.push_notice(unknown_key_message(&raw_key));
                return;
            };
            if !key.is_writable() {
                app.push_notice(config::API_KEY_HINT);
                return;
            }
            let path = match target_config_path(global) {
                Ok(path) => path,
                Err(error) => {
                    app.push_notice(error);
                    return;
                }
            };
            match config::set_value(&path, key, &value) {
                Ok(value) => {
                    app.config = AgentConfig::load(workspace);
                    app.push_notice(format!(
                        "Set {} = {}\nSaved to {}{}",
                        key.name(),
                        value,
                        path.display(),
                        shadow_warning(global, workspace)
                    ));
                }
                Err(error) => app.push_notice(format!("Failed to set {}: {error}", key.name())),
            }
        }
        ConfigAction::Unset { key, global } => {
            let raw_key = key.clone();
            let Some(key) = ConfigKey::parse(&key) else {
                app.push_notice(unknown_key_message(&raw_key));
                return;
            };
            if !key.is_writable() {
                app.push_notice(config::API_KEY_HINT);
                return;
            }
            let path = match target_config_path(global) {
                Ok(path) => path,
                Err(error) => {
                    app.push_notice(error);
                    return;
                }
            };
            match config::unset_value(&path, key) {
                Ok(true) => {
                    app.config = AgentConfig::load(workspace);
                    app.push_notice(format!(
                        "Unset {}\nSaved to {}{}",
                        key.name(),
                        path.display(),
                        shadow_warning(global, workspace)
                    ));
                }
                Ok(false) => {
                    app.push_notice(format!("{} was not set in {}", key.name(), path.display()))
                }
                Err(error) => {
                    app.push_notice(format!("Failed to unset {}: {error}", key.name()));
                }
            }
        }
    }
}

fn target_config_path(global: bool) -> Result<std::path::PathBuf, String> {
    if global {
        config::global_config_path()
            .ok_or_else(|| "Could not resolve the home directory for --global.".to_string())
    } else {
        Ok(config::workspace_config_path(std::path::Path::new(".")))
    }
}

fn shadow_warning(global: bool, workspace: &std::path::Path) -> &'static str {
    if global && config::workspace_config_path(workspace).exists() {
        "\nWorkspace config shadows this file; the new value will not take effect here."
    } else {
        ""
    }
}

fn unknown_key_message(key: &str) -> String {
    format!(
        "Unknown config key: {key}\nSupported keys: {}, api_key (read-only)",
        config::WRITABLE_KEYS.join(", ")
    )
}

/// One aligned line per palette entry, so the help output and the autocomplete
/// menu can never disagree about which commands exist.
pub fn help_text() -> String {
    let width = SLASH_COMMANDS
        .iter()
        .map(|(command, _)| command.chars().count())
        .max()
        .unwrap_or(0);

    let mut text = String::from("Slash commands:");
    for (command, description) in SLASH_COMMANDS {
        text.push('\n');
        text.push_str(&format!("{command:<width$}  {description}"));
    }
    text
}

/// Strip terminal control characters but keep the whitespace that makes
/// multi-line reports readable.
pub fn sanitize(text: &str) -> String {
    text.chars()
        .filter(|c| *c == '\n' || *c == '\t' || !c.is_control())
        .collect()
}

#[cfg(test)]
#[path = "../tests/tui/commands.rs"]
mod tests;
