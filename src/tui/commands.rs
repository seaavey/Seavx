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

/// What `/goal` was asked to do. `id` defaults to the workspace's live goal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GoalAction {
    New(String),
    /// Replace the draft's objective, or show it when no text is given.
    Edit(Option<String>),
    CriteriaAdd(String),
    CriteriaRemove(usize),
    TaskAdd(String),
    TaskRemove(usize),
    Approve,
    Show(Option<i64>),
    Pause(Option<i64>),
    Resume(Option<i64>),
    Cancel(Option<i64>),
}

/// The session verbs that act on a stored session rather than open it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionsOp {
    Fork { id: String, at: Option<i64> },
    Archive { id: String, archived: bool },
    Search { query: String, all: bool },
}

/// One command's identity, independent of how it is spelled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandId {
    Model,
    Compact,
    Config,
    Plan,
    Ask,
    Review,
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
    Goal,
    Tasks,
    Test,
    Preset,
    Steer,
    Queue,
    Status,
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
        name: "/ask",
        usage: "/ask <question>",
        summary: "Ask a question about the code, read-only, one turn",
        id: CommandId::Ask,
    },
    CommandSpec {
        name: "/review",
        usage: "/review [working-tree|staged|commit <ref>|base <ref>]",
        summary: "Review a git target and report findings, read-only",
        id: CommandId::Review,
    },
    CommandSpec {
        name: "/preset",
        usage: "/preset [name]",
        summary: "List prompt presets, or put one in the next prompt",
        id: CommandId::Preset,
    },
    CommandSpec {
        name: "/test",
        usage: "/test",
        summary: "Propose the test command for the changed files, then run it",
        id: CommandId::Test,
    },
    CommandSpec {
        name: "/steer",
        usage: "/steer <text>",
        summary: "Tell the running turn something; applied at its next step",
        id: CommandId::Steer,
    },
    CommandSpec {
        name: "/queue",
        usage: "/queue <prompt>",
        summary: "Run this prompt after the current turn succeeds",
        id: CommandId::Queue,
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
        usage: "/memory [show|project|user|path]",
        summary: "Inspect persistent memory",
        id: CommandId::Memory,
    },
    CommandSpec {
        name: "/skills",
        usage: "/skills [list|show <name>|add <dir>|install <url> [name...]|remove <name> --yes|path]",
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
        usage: "/sessions [number] | fork <id> [--at <n>] | archive <id> | unarchive <id> | find <text> [--all]",
        summary: "Pick a stored session, resume one, or fork, archive and search them",
        id: CommandId::Sessions,
    },
    CommandSpec {
        name: "/resume",
        usage: "/resume [number]",
        summary: "Alias for /sessions",
        id: CommandId::Resume,
    },
    CommandSpec {
        name: "/goal",
        usage: "/goal <objective> | edit [text] | criteria add|remove <n|text> | task add|remove <n|text> | approve | show | pause | resume | cancel [id]",
        summary: "Draft, approve and track a durable goal",
        id: CommandId::Goal,
    },
    CommandSpec {
        name: "/tasks",
        usage: "/tasks",
        summary: "Show the current goal's ordered tasks",
        id: CommandId::Tasks,
    },
    CommandSpec {
        name: "/status",
        usage: "/status",
        summary: "Show what the agent is doing right now",
        id: CommandId::Status,
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
    /// List presets, or preview and apply one to the next prompt.
    Preset {
        name: Option<String>,
    },
    /// Propose and, once permitted, run the workspace's test command.
    Test,
    /// Fork, archive or search a stored session.
    SessionsOp {
        op: SessionsOp,
    },
    /// Talk to the turn that is running; delivered at its next safe boundary.
    Steer {
        text: String,
    },
    /// Hold a prompt until the running turn finishes successfully.
    Queue {
        prompt: String,
    },
    /// Switch the runtime mode; the mode marker already shows the result.
    Plan {
        mode: AgentMode,
    },
    /// One read-only question about the code. The standing mode is untouched.
    Ask {
        question: String,
    },
    /// One read-only review of a resolved Git target.
    Review {
        target: crate::agent::git_review::ReviewTarget,
    },
    /// Create, edit, approve, inspect or move a goal.
    Goal {
        action: crate::tui::commands::GoalAction,
    },
    /// Inspect the current goal's tasks.
    Tasks,
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
    /// Compact, read-only view of the current state.
    Status,
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
    Install { url: String, names: Vec<String> },
    Remove { name: String, confirmed: bool },
    Path,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandEffect {
    Handled,
    SkillsAdd {
        dir: std::path::PathBuf,
    },
    SkillsRemove {
        name: String,
    },
    SkillsFetch {
        url: String,
        names: Vec<String>,
    },
    Quit,
    OpenModelPicker,
    RunDoctor {
        offline: bool,
    },
    RunCompact {
        instructions: Option<String>,
    },
    RunUndo,
    RunSessions {
        number: u32,
    },
    /// Submit one Ask turn; the worker runs it in `AgentMode::Ask`.
    RunAsk {
        question: String,
    },
    /// Resolve the target off the UI thread, then run one Review turn.
    RunReview {
        target: crate::agent::git_review::ReviewTarget,
    },
    /// One goal operation, run against the store off the UI thread.
    RunGoal {
        action: GoalAction,
    },
    RunSessionsOp {
        op: SessionsOp,
    },
    RunTest,
    /// Read the current goal's tasks.
    RunTasks,
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
        CommandId::Status => no_args(&args, trimmed, SlashCommand::Status),
        // Empty arguments mean `working-tree`; a bad form is a typo, not a run.
        CommandId::Review => match crate::agent::git_review::parse_target(&args) {
            Ok(target) => SlashCommand::Review { target },
            Err(_) => SlashCommand::Unknown(trimmed.to_string()),
        },
        // The whole remainder is the question. A bare `/ask` still parses: it
        // answers with its usage instead of reading as a typo.
        CommandId::Tasks => no_args(&args, trimmed, SlashCommand::Tasks),
        CommandId::Goal => match args.as_slice() {
            // A bare `/goal` shows the current one, so the registry stays
            // parseable without arguments like every other command.
            [] => SlashCommand::Goal {
                action: GoalAction::Show(None),
            },
            ["edit"] => SlashCommand::Goal {
                action: GoalAction::Edit(None),
            },
            ["edit", objective @ ..] => SlashCommand::Goal {
                action: GoalAction::Edit(Some(objective.join(" "))),
            },
            ["criteria", "add", text @ ..] if !text.is_empty() => SlashCommand::Goal {
                action: GoalAction::CriteriaAdd(text.join(" ")),
            },
            ["task", "add", text @ ..] if !text.is_empty() => SlashCommand::Goal {
                action: GoalAction::TaskAdd(text.join(" ")),
            },
            [kind @ ("criteria" | "task"), "remove", number] => match number.parse::<usize>() {
                Ok(index) if index > 0 => SlashCommand::Goal {
                    action: if *kind == "criteria" {
                        GoalAction::CriteriaRemove(index - 1)
                    } else {
                        GoalAction::TaskRemove(index - 1)
                    },
                },
                _ => SlashCommand::Unknown(trimmed.to_string()),
            },
            ["approve"] => SlashCommand::Goal {
                action: GoalAction::Approve,
            },
            [verb @ ("show" | "pause" | "resume" | "cancel"), rest @ ..] => {
                let id = match rest {
                    [] => None,
                    [number] => match number.parse::<i64>() {
                        Ok(id) if id > 0 => Some(id),
                        _ => return Some(SlashCommand::Unknown(trimmed.to_string())),
                    },
                    _ => return Some(SlashCommand::Unknown(trimmed.to_string())),
                };
                let action = match *verb {
                    "show" => GoalAction::Show(id),
                    "pause" => GoalAction::Pause(id),
                    "resume" => GoalAction::Resume(id),
                    _ => GoalAction::Cancel(id),
                };
                SlashCommand::Goal { action }
            }
            objective => SlashCommand::Goal {
                action: GoalAction::New(objective.join(" ")),
            },
        },
        CommandId::Ask => SlashCommand::Ask {
            question: args.join(" "),
        },
        CommandId::Preset => match args.as_slice() {
            [] => SlashCommand::Preset { name: None },
            [name] => SlashCommand::Preset {
                name: Some((*name).to_string()),
            },
            _ => SlashCommand::Unknown(trimmed.to_string()),
        },
        CommandId::Test => SlashCommand::Test,
        CommandId::Steer => SlashCommand::Steer {
            text: args.join(" "),
        },
        CommandId::Queue => SlashCommand::Queue {
            prompt: args.join(" "),
        },
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
            ["fork", id] => SlashCommand::SessionsOp {
                op: SessionsOp::Fork {
                    id: (*id).to_string(),
                    at: None,
                },
            },
            ["fork", id, "--at", at] => match at.parse::<i64>() {
                Ok(at) if at > 0 => SlashCommand::SessionsOp {
                    op: SessionsOp::Fork {
                        id: (*id).to_string(),
                        at: Some(at),
                    },
                },
                _ => SlashCommand::Unknown(trimmed.to_string()),
            },
            [verb @ ("archive" | "unarchive"), id] => SlashCommand::SessionsOp {
                op: SessionsOp::Archive {
                    id: (*id).to_string(),
                    archived: *verb == "archive",
                },
            },
            ["find", rest @ ..] if !rest.is_empty() => {
                let query = rest.join(" ");
                let (query, all) = match query.strip_suffix(" --all") {
                    Some(query) => (query.to_string(), true),
                    None => (query, false),
                };
                SlashCommand::SessionsOp {
                    op: SessionsOp::Search { query, all },
                }
            }
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
            ["install", url, names @ ..] => SlashCommand::Skills {
                action: SkillsAction::Install {
                    url: (*url).to_string(),
                    names: names.iter().map(|name| (*name).to_string()).collect(),
                },
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

/// One compact line per fact: what the agent is doing, which session it is
/// doing it in, and whether persistence degraded. `/status` is not a trace
/// viewer, so it never lists stored events, arguments or output.
pub fn status_text(app: &App) -> String {
    let state = match &app.status {
        AgentState::Idle => "idle".to_string(),
        AgentState::Thinking => "thinking".to_string(),
        AgentState::Streaming => "streaming".to_string(),
        AgentState::ExecutingTool(tool) => format!("executing {tool}"),
        AgentState::Checking => "checking the model".to_string(),
        AgentState::Compacting => "compacting".to_string(),
    };
    let mut lines = vec![format!("state: {state}")];
    if let Some(pending) = &app.pending_permission {
        lines.push(format!(
            "waiting for permission: {} wants to {}",
            pending.request.tool, pending.request.effect
        ));
    }
    let session = match (app.session_id, app.session_title.as_deref()) {
        (Some(id), Some(title)) => format!("{id} ({title})"),
        (Some(id), None) => id.to_string(),
        (None, _) => "none".to_string(),
    };
    lines.push(format!("session: {session}"));
    if let Some(goal) = &app.goal {
        lines.push(format!("goal: {goal}"));
    }
    if let Ok(slot) = app.steer.lock() {
        if let Some(text) = &slot.waiting {
            lines.push(format!("steer: waiting ({text})"));
        } else if slot.applied > 0 {
            lines.push(format!("steer: {} applied", slot.applied));
        }
    }
    if let Some(prompt) = &app.queued_prompt {
        lines.push(format!("queued: {prompt}"));
    }
    if let Some(error) = &app.persistence_error {
        lines.push(format!("persistence: degraded ({error})"));
    }
    lines.join("\n")
}

// ---- /test: propose, then verify ---------------------------------------

/// The command a test run would use, with the evidence that justifies it.
///
/// A guess is not a proposal: the command is only offered when a manifest in
/// the changed set (or at the root) actually declares tests.
pub(crate) fn propose_test(changed: &[String], root_has_cargo: bool) -> Option<(String, String)> {
    let cargo = root_has_cargo
        || changed
            .iter()
            .any(|path| path == "Cargo.toml" || path.ends_with("/Cargo.toml"));
    if cargo {
        return Some((
            "cargo test".to_string(),
            "Cargo.toml declares the Rust test targets".to_string(),
        ));
    }
    let package = changed
        .iter()
        .any(|path| path == "package.json" || path.ends_with("/package.json"));
    if package {
        return Some((
            "npm test --silent".to_string(),
            "package.json declares the test script".to_string(),
        ));
    }
    None
}

/// Cargo's own summary, when it is there to read: passed, failed, ignored,
/// not-run. Anything unrecognised or truncated answers `None` for every count;
/// success is never inferred from text.
pub(crate) fn summarize_test_output(
    output: &str,
    truncated: bool,
) -> (Option<usize>, Option<usize>, Option<usize>, Option<usize>) {
    if truncated {
        return (None, None, None, None);
    }
    let Some(line) = output
        .lines()
        .rev()
        .find(|line| line.trim_start().starts_with("test result:"))
    else {
        return (None, None, None, None);
    };
    let number = |label: &str| -> Option<usize> {
        let at = line.find(label)?;
        line[..at]
            .split_whitespace()
            .last()
            .and_then(|value| value.parse().ok())
    };
    let passed = number(" passed");
    let failed = number(" failed");
    let ignored = number(" ignored");
    let not_run = number(" filtered out");
    if passed.is_none() && failed.is_none() {
        return (None, None, None, None);
    }
    (passed, failed, ignored, not_run)
}

/// The line the user reads after a `/test`.
pub(crate) fn test_report(
    command: &str,
    status: Option<i32>,
    output: &str,
    truncated: bool,
) -> String {
    let (passed, failed, ignored, not_run) = summarize_test_output(output, truncated);
    let verdict = match status {
        Some(0) => "passed",
        Some(_) => "failed",
        None => "did not finish",
    };
    let counts = if passed.is_none() && failed.is_none() {
        "counts unavailable (output truncated or unrecognised)".to_string()
    } else {
        format!(
            "passed {}{} ignored {}{}",
            passed.map(|n| n.to_string()).unwrap_or_else(|| "?".into()),
            failed.map(|n| format!(", failed {n}")).unwrap_or_default(),
            ignored.map(|n| n.to_string()).unwrap_or_else(|| "?".into()),
            not_run
                .map(|n| format!(", filtered out {n}"))
                .unwrap_or_default()
        )
    };
    format!(
        "`{command}` {verdict} (exit {})\n{counts}",
        status
            .map(|s| s.to_string())
            .unwrap_or_else(|| "none".into())
    )
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
        // A running turn is exactly what the user is asking about.
        SlashCommand::Status => {
            app.push_notice(status_text(app));
            return CommandEffect::Handled;
        }
        // Steering and queuing exist for a running turn, so they skip the gate
        // that refuses everything else while one is busy.
        SlashCommand::Steer { text } => {
            let text = text.trim().to_string();
            if text.is_empty() {
                app.push_notice("Usage: /steer <text>".to_string());
            } else if app.status == AgentState::Idle {
                app.push_notice("No turn is running; send it as a prompt instead.".to_string());
            } else if app.steer_waiting() {
                app.push_notice("A steer is already waiting for the next step.".to_string());
            } else {
                app.queue_steer(text);
                app.push_notice("Steer queued: delivered at the next safe boundary.");
            }
            return CommandEffect::Handled;
        }
        SlashCommand::Queue { prompt } => {
            let prompt = prompt.trim().to_string();
            if prompt.is_empty() {
                app.push_notice("Usage: /queue <prompt>".to_string());
            } else if app.status == AgentState::Idle {
                app.push_notice("No turn is running; nothing to queue behind.".to_string());
            } else if app.queued_prompt.is_some() {
                app.push_notice("A prompt is already queued.".to_string());
            } else {
                app.queued_prompt = Some(prompt);
                app.push_notice("Prompt queued: it runs only after this turn succeeds.");
            }
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
                SkillsAction::Install { url, names } => CommandEffect::SkillsFetch { url, names },
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
        SlashCommand::Review { target } => CommandEffect::RunReview { target },
        SlashCommand::Goal { action } => CommandEffect::RunGoal { action },
        SlashCommand::Tasks => CommandEffect::RunTasks,
        SlashCommand::Ask { question } => {
            if question.trim().is_empty() {
                app.push_notice("Usage: /ask <question>".to_string());
                CommandEffect::Handled
            } else {
                CommandEffect::RunAsk { question }
            }
        }
        // Handled above the busy gate; unreachable here, but the match must
        // still say so.
        SlashCommand::Steer { .. } | SlashCommand::Queue { .. } => CommandEffect::Handled,
        SlashCommand::Preset { name } => {
            // The workspace the session belongs to, not the process cwd.
            let root = app.root.clone();
            let root = root.as_path();
            match name {
                None => {
                    let found = crate::agent::presets::list(root);
                    if found.is_empty() {
                        app.push_notice(
                            "No presets yet. Add .seavx/prompts/<name>.md (project) or ~/.seavx/prompts/<name>.md (everywhere)."
                                .to_string(),
                        );
                    } else {
                        let names: Vec<String> = found
                            .into_iter()
                            .map(|(name, path)| format!("{name}  ({})", path.display()))
                            .collect();
                        app.push_notice(format!("Presets:\n{}", names.join("\n")));
                    }
                }
                Some(name) => match crate::agent::presets::load(root, &name) {
                    Ok(text) => {
                        let preview: Vec<&str> = text.lines().take(3).collect();
                        // The preset becomes the next prompt; it never reaches
                        // the system prompt or the permission policy.
                        app.set_input(&text);
                        app.push_notice(format!(
                            "Preset '{name}' loaded into the prompt:\n{}",
                            preview.join("\n")
                        ));
                    }
                    Err(error) => app.push_notice(error),
                },
            }
            CommandEffect::Handled
        }
        SlashCommand::SessionsOp { op } => CommandEffect::RunSessionsOp { op },
        SlashCommand::Test => CommandEffect::RunTest,
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
        SlashCommand::Help
        | SlashCommand::Exit
        | SlashCommand::Plan { .. }
        | SlashCommand::Status => CommandEffect::Handled,
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
