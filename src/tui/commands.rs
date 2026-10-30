//! Slash-command parsing and dispatch for the TUI.
//!
//! [`parse`] is a pure parser: it only recognises the commands implemented in
//! this commit and never touches the app. [`execute`] applies a command and
//! reports the effects the event loop still has to perform (quitting, opening
//! a modal, or work that needs a spawned task in later commits).

use crate::agent::engine::{AgentConfig, AgentMode};
use crate::cli::config::{self, ConfigAction, ConfigKey};
use crate::cli::memory::{self, MemoryView};
use crate::cli::skills;
use crate::tui::app::App;
use crate::tui::event::AgentState;

/// One command's identity, independent of how it is spelled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandId {
    Model,
    Compact,
    Config,
    Plan,
    Clear,
    New,
    Help,
    Init,
    Doctor,
    Memory,
    Skills,
    Undo,
    Sessions,
    Resume,
    Exit,
}

/// One row of the command table: the only place a command's spelling lives.
pub struct CommandSpec {
    pub name: &'static str,
    pub usage: &'static str,
    pub summary: &'static str,
    pub id: CommandId,
}

/// Every command, in menu order. `parse`, the palette, the autocomplete filter
/// and `help_text` all read this one table.
pub const COMMANDS: &[CommandSpec] = &[
    CommandSpec {
        name: "/model",
        usage: "/model [model] [effort]",
        summary: "Switch model and reasoning effort",
        id: CommandId::Model,
    },
    CommandSpec {
        name: "/compact",
        usage: "/compact [focus]",
        summary: "Summarize the session and free context",
        id: CommandId::Compact,
    },
    CommandSpec {
        name: "/config",
        usage: "/config [path|get <key>|set [--global] <key> <value>|unset [--global] <key>]",
        summary: "Show or edit effective configuration",
        id: CommandId::Config,
    },
    CommandSpec {
        name: "/plan",
        usage: "/plan [on|off]",
        summary: "Enter plan mode (read-only)",
        id: CommandId::Plan,
    },
    CommandSpec {
        name: "/clear",
        usage: "/clear",
        summary: "Clear session conversation output",
        id: CommandId::Clear,
    },
    CommandSpec {
        name: "/new",
        usage: "/new",
        summary: "Alias for /clear",
        id: CommandId::New,
    },
    CommandSpec {
        name: "/help",
        usage: "/help",
        summary: "Show help and command palette",
        id: CommandId::Help,
    },
    CommandSpec {
        name: "/init",
        usage: "/init",
        summary: "Initialize workspace configuration",
        id: CommandId::Init,
    },
    CommandSpec {
        name: "/doctor",
        usage: "/doctor [--offline]",
        summary: "Check environment and health",
        id: CommandId::Doctor,
    },
    CommandSpec {
        name: "/memory",
        usage: "/memory [show|project|user|skills|path]",
        summary: "Inspect persistent memory",
        id: CommandId::Memory,
    },
    CommandSpec {
        name: "/skills",
        usage: "/skills [list|show <name>|add <dir>|remove <name> --yes|path]",
        summary: "List, inspect, install or remove skills",
        id: CommandId::Skills,
    },
    CommandSpec {
        name: "/undo",
        usage: "/undo",
        summary: "Restore the files changed by the last agent call",
        id: CommandId::Undo,
    },
    CommandSpec {
        name: "/sessions",
        usage: "/sessions [number]",
        summary: "Pick a stored session, or resume one by its number",
        id: CommandId::Sessions,
    },
    CommandSpec {
        name: "/resume",
        usage: "/resume [number]",
        summary: "Alias for /sessions",
        id: CommandId::Resume,
    },
    CommandSpec {
        name: "/exit",
        usage: "/exit",
        summary: "Quit Seavx agent TUI",
        id: CommandId::Exit,
    },
];

/// The autocomplete palette: every row, in table order.
pub fn palette() -> Vec<(&'static str, &'static str)> {
    COMMANDS
        .iter()
        .map(|spec| (spec.name, spec.summary))
        .collect()
}

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
    Skills {
        action: SkillsAction,
    },
    /// Restore files from the newest checkpoint.
    Undo,
    /// Open the session picker, or resume one by its listed number.
    Sessions {
        index: Option<u32>,
    },
    Unknown(String),
}

/// `/skills` subcommands. `add` and `remove` are the two that touch the disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkillsAction {
    List,
    Show(String),
    Add(std::path::PathBuf),
    Remove { name: String, confirmed: bool },
    Path,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandEffect {
    Handled,
    SkillsAdd { dir: std::path::PathBuf },
    SkillsRemove { name: String },
    Quit,
    OpenModelPicker,
    RunDoctor { offline: bool },
    RunCompact { instructions: Option<String> },
    RunUndo,
    RunSessions { number: u32 },
    OpenSessionPicker,
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

    // The table decides what exists; the id decides the argument grammar.
    let Some(spec) = COMMANDS.iter().find(|spec| spec.name == name) else {
        return Some(SlashCommand::Unknown(trimmed.to_string()));
    };

    let command = match spec.id {
        CommandId::Model => {
            // `list` is the historical way to ask for the picker.
            match args.as_slice() {
                ["list"] => SlashCommand::Model {
                    target: None,
                    effort: None,
                },
                _ if args.len() <= 2 => SlashCommand::Model {
                    target: args.first().map(|arg| arg.to_string()),
                    effort: args.get(1).map(|arg| arg.to_string()),
                },
                _ => SlashCommand::Unknown(trimmed.to_string()),
            }
        }
        // One variant, one handler: the alias cannot drift from its target.
        CommandId::Clear | CommandId::New => no_args(&args, trimmed, SlashCommand::Clear),
        CommandId::Plan => match args.as_slice() {
            [] | ["on"] => SlashCommand::Plan {
                mode: AgentMode::Plan,
            },
            ["off"] => SlashCommand::Plan {
                mode: AgentMode::Build,
            },
            _ => SlashCommand::Unknown(trimmed.to_string()),
        },
        CommandId::Help => no_args(&args, trimmed, SlashCommand::Help),
        CommandId::Exit => no_args(&args, trimmed, SlashCommand::Exit),
        CommandId::Init => no_args(&args, trimmed, SlashCommand::Init),
        CommandId::Undo => no_args(&args, trimmed, SlashCommand::Undo),
        CommandId::Compact => SlashCommand::Compact {
            instructions: (!args.is_empty()).then(|| args.join(" ")),
        },
        CommandId::Doctor => match args.as_slice() {
            [] => SlashCommand::Doctor { offline: false },
            ["--offline"] => SlashCommand::Doctor { offline: true },
            _ => SlashCommand::Unknown(trimmed.to_string()),
        },
        // One grammar for both spellings; `/resume` is the alias.
        CommandId::Sessions | CommandId::Resume => match args.as_slice() {
            [] => SlashCommand::Sessions { index: None },
            [number] => match number.parse::<u32>() {
                Ok(number) if number > 0 => SlashCommand::Sessions {
                    index: Some(number),
                },
                _ => SlashCommand::Unknown(trimmed.to_string()),
            },
            _ => SlashCommand::Unknown(trimmed.to_string()),
        },
        CommandId::Config => parse_config(&args, trimmed),
        CommandId::Memory => match args.as_slice() {
            [] | ["show"] => SlashCommand::Memory {
                view: MemoryView::All,
            },
            ["project"] => SlashCommand::Memory {
                view: MemoryView::Project,
            },
            ["user"] => SlashCommand::Memory {
                view: MemoryView::User,
            },
            ["skills"] => SlashCommand::Memory {
                view: MemoryView::Skills,
            },
            ["path"] => SlashCommand::Memory {
                view: MemoryView::Path,
            },
            _ => SlashCommand::Unknown(trimmed.to_string()),
        },
        CommandId::Skills => match args.as_slice() {
            [] | ["list"] => SlashCommand::Skills {
                action: SkillsAction::List,
            },
            ["show", name] => SlashCommand::Skills {
                action: SkillsAction::Show((*name).to_string()),
            },
            ["add", dir] => SlashCommand::Skills {
                action: SkillsAction::Add(std::path::PathBuf::from(dir)),
            },
            // Destructive: the flag is part of the grammar, not a later check.
            ["remove", name, "--yes"] => SlashCommand::Skills {
                action: SkillsAction::Remove {
                    name: (*name).to_string(),
                    confirmed: true,
                },
            },
            ["path"] => SlashCommand::Skills {
                action: SkillsAction::Path,
            },
            _ => SlashCommand::Unknown(trimmed.to_string()),
        },
    };

    Some(command)
}

/// A command whose grammar has no arguments: anything after the name is an
/// error, never silently ignored.
fn no_args(args: &[&str], raw: &str, command: SlashCommand) -> SlashCommand {
    if args.is_empty() {
        command
    } else {
        SlashCommand::Unknown(raw.to_string())
    }
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
            // A new session id; nothing stored is deleted or asked about.
            app.start_new_session();
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
        SlashCommand::Skills { action } => {
            let workspace = std::path::Path::new(".");
            match action {
                SkillsAction::List => {
                    app.push_notice(skills::render_list(&skills::list_report(workspace)));
                    CommandEffect::Handled
                }
                SkillsAction::Path => {
                    app.push_notice(skills::render_paths(workspace));
                    CommandEffect::Handled
                }
                SkillsAction::Show(name) => {
                    match skills::render_show(workspace, &name) {
                        Ok(text) | Err(text) => app.push_notice(text),
                    }
                    CommandEffect::Handled
                }
                // The copy runs off the UI thread; the notice comes back as an action.
                SkillsAction::Add(dir) => CommandEffect::SkillsAdd { dir },
                // The grammar already refuses a removal without `--yes`.
                SkillsAction::Remove {
                    name,
                    confirmed: true,
                } => CommandEffect::SkillsRemove { name },
                SkillsAction::Remove {
                    name,
                    confirmed: false,
                } => {
                    app.push_notice(format!("Removing {name}: --yes is required."));
                    CommandEffect::Handled
                }
            }
        }
        SlashCommand::Doctor { offline } => CommandEffect::RunDoctor { offline },
        SlashCommand::Compact { instructions } => CommandEffect::RunCompact { instructions },
        SlashCommand::Undo => CommandEffect::RunUndo,
        SlashCommand::Sessions { index } => match index {
            // The picker is the list: no number means "let me choose".
            None => CommandEffect::OpenSessionPicker,
            Some(number) => CommandEffect::RunSessions { number },
        },
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
            if let Err(conflict) = config::recover_config(&config::workspace_config_path(workspace))
            {
                app.push_notice(conflict);
            }
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
    let width = COMMANDS
        .iter()
        .map(|spec| spec.usage.chars().count())
        .max()
        .unwrap_or(0);

    let mut text = String::from("Slash commands:");
    for spec in COMMANDS {
        text.push('\n');
        text.push_str(&format!("{:<width$}  {}", spec.usage, spec.summary));
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
