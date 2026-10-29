//! Command classification: the whole command, its paths, and its destinations.
//!
//! The shell is outside the jail, so this module gates rather than contains. It
//! splits a command into segments, classifies every one of them plus every
//! substitution and redirection, and returns the most restrictive kind. A
//! construct it cannot prove read-only is `Execute` and `unanalysable`, which
//! the matrix turns into an ask — never an allow.

use std::path::{Path, PathBuf};

use crate::agent::permissions::{ActionKind, global_root_for};
use crate::agent::security::{WorkspaceSecurity, check_file};

pub struct Segment {
    pub program: String,
    pub args: Vec<String>,
}

pub struct CommandClass {
    pub kind: ActionKind,
    pub segments: Vec<Segment>,
    pub unanalysable: bool,
}

pub const READ_ONLY: &[&str] = &[
    "ls", "cat", "head", "tail", "less", "more", "rg", "grep", "find", "pwd", "which", "wc",
    "sort", "uniq", "xxd", "strings", "file", "stat", "echo",
];
pub const PATH_ARGS: &[&str] = &[
    "cat", "head", "tail", "less", "more", "grep", "rg", "sort", "uniq", "wc", "xxd", "strings",
    "cp", "mv", "tee", "dd", "sed", "awk",
];
pub const RECURSIVE_READERS: &[&str] = &["rg", "grep", "find"];
pub const DESTRUCTIVE: &[&str] = &[
    "rm -rf /",
    "mkfs",
    "dd of=/dev/",
    "shutdown",
    "reboot",
    "chmod -R 777 /",
    ":(){ :|:& };:",
];

/// Programs that write a file named by their last argument.
const WRITERS: &[&str] = &["cp", "mv", "install", "ln", "tee", "dd"];
/// Programs whose whole point is leaving the machine.
const NETWORK_PROGRAMS: &[&str] = &[
    "curl", "wget", "ssh", "scp", "sftp", "rsync", "nc", "ncat", "netcat", "telnet", "ftp",
];
/// Git subcommands that are read-only; everything else asks.
const GIT_READ_ONLY: &[&str] = &[
    "status",
    "diff",
    "log",
    "show",
    "blame",
    "grep",
    "rev-parse",
    "ls-files",
];
const GIT_NETWORK: &[&str] = &["push", "pull", "fetch", "clone"];
/// Shells that can hide an arbitrary command behind `-c`.
const SHELLS: &[&str] = &["sh", "bash", "zsh", "dash", "ksh", "fish"];

/// Restrictiveness order used to fold segment verdicts.
fn rank(kind: ActionKind) -> u8 {
    match kind {
        ActionKind::Read => 0,
        ActionKind::Write => 1,
        ActionKind::Execute => 2,
        ActionKind::Network => 3,
        ActionKind::GlobalWrite => 4,
        ActionKind::Destructive => 5,
    }
}

fn worst(current: ActionKind, candidate: ActionKind) -> ActionKind {
    if rank(candidate) > rank(current) {
        candidate
    } else {
        current
    }
}

pub fn classify(command: &str, security: &WorkspaceSecurity) -> CommandClass {
    classify_with_home(crate::home::dir().as_deref(), command, security)
}

/// Injected-home form, so tests never read the process `HOME`.
pub fn classify_with_home(
    home: Option<&Path>,
    command: &str,
    security: &WorkspaceSecurity,
) -> CommandClass {
    let mut class = CommandClass {
        kind: ActionKind::Read,
        segments: Vec::new(),
        unanalysable: false,
    };

    // A construct with no safe reading is an ask, and it inherits nothing.
    if has_heredoc(command) || command.contains("<<") {
        return unanalysable(class, ActionKind::Execute);
    }

    let mut substitutions: Vec<String> = Vec::new();
    let stripped = match lex(command, &mut substitutions) {
        Ok(tokens) => tokens,
        Err(()) => return unanalysable(class, ActionKind::Execute),
    };

    for body in &substitutions {
        let nested = classify_with_home(home, body, security);
        class.kind = worst(class.kind, nested.kind);
        class.unanalysable |= nested.unanalysable;
    }

    let mut group: Vec<String> = Vec::new();
    let mut groups: Vec<Vec<String>> = Vec::new();
    for token in stripped {
        match token.as_str() {
            // A separator closes the simple command it follows.
            "|" | "||" | "&&" | ";" | "&" | "\n" => {
                if !group.is_empty() {
                    groups.push(std::mem::take(&mut group));
                }
            }
            token if token.starts_with('>') => {
                // The target is the next word; the marker keeps it out of a
                // program position so it is only ever read as a destination.
                if !group.is_empty() {
                    groups.push(std::mem::take(&mut group));
                }
                group.push("__redirect__".to_string());
            }
            _ => group.push(token),
        }
    }
    if !group.is_empty() {
        groups.push(group);
    }

    for tokens in groups {
        let Some(segment) = classify_segment(home, &tokens, security, &mut class) else {
            continue;
        };
        class.segments.push(segment);
    }

    class
}

fn unanalysable(mut class: CommandClass, kind: ActionKind) -> CommandClass {
    class.kind = worst(class.kind, kind);
    class.unanalysable = true;
    class
}

fn has_heredoc(command: &str) -> bool {
    command.contains("<<")
}

/// Split into words, keeping `|`, `>`, `>>`, `&&` and friends as their own
/// tokens, and collecting `$(…)`/`${…}`/backtick bodies for classification.
fn lex(command: &str, substitutions: &mut Vec<String>) -> Result<Vec<String>, ()> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut chars = command.chars().peekable();
    let mut quote: Option<char> = None;

    while let Some(ch) = chars.next() {
        match quote {
            Some(q) => {
                if ch == q {
                    quote = None;
                } else {
                    current.push(ch);
                }
            }
            None => match ch {
                '\'' | '"' => quote = Some(ch),
                '`' => {
                    let mut body = String::new();
                    loop {
                        match chars.next() {
                            Some('`') | None => break,
                            Some(c) => body.push(c),
                        }
                    }
                    substitutions.push(body);
                }
                '$' if chars.peek() == Some(&'(') => {
                    chars.next();
                    let mut depth = 1;
                    let mut body = String::new();
                    for c in chars.by_ref() {
                        match c {
                            '(' => {
                                depth += 1;
                                body.push(c);
                            }
                            ')' => {
                                depth -= 1;
                                if depth == 0 {
                                    break;
                                }
                                body.push(c);
                            }
                            _ => body.push(c),
                        }
                    }
                    substitutions.push(body);
                }
                '$' if chars.peek() == Some(&'{') => {
                    chars.next();
                    let mut body = String::new();
                    for c in chars.by_ref() {
                        if c == '}' {
                            break;
                        }
                        body.push(c);
                    }
                    substitutions.push(body);
                }
                ';' | '|' | '&' | '\n' => {
                    push_word(&mut tokens, &mut current);
                    let mut token = ch.to_string();
                    if (ch == '|' || ch == '&') && chars.peek() == Some(&ch) {
                        chars.next();
                        token.push(ch);
                    }
                    tokens.push(token);
                }
                '>' => {
                    push_word(&mut tokens, &mut current);
                    let mut token = ch.to_string();
                    if chars.peek() == Some(&'>') {
                        chars.next();
                        token.push('>');
                    }
                    tokens.push(token);
                }
                '<' => {
                    push_word(&mut tokens, &mut current);
                    tokens.push("<".to_string());
                }
                c if c.is_whitespace() => push_word(&mut tokens, &mut current),
                c => current.push(c),
            },
        }
    }

    if quote.is_some() {
        return Err(());
    }
    push_word(&mut tokens, &mut current);
    Ok(tokens)
}

fn push_word(tokens: &mut Vec<String>, current: &mut String) {
    if !current.is_empty() {
        tokens.push(std::mem::take(current));
    }
}

fn classify_segment(
    home: Option<&Path>,
    tokens: &[String],
    security: &WorkspaceSecurity,
    class: &mut CommandClass,
) -> Option<Segment> {
    // `K=V cmd` runs cmd with an environment prefix.
    let mut words = tokens.iter().peekable();
    while let Some(word) = words.peek() {
        let assignment = word.split_once('=').is_some_and(|(name, _)| {
            !name.is_empty() && name.chars().all(|c| c.is_alphanumeric() || c == '_')
        });
        if assignment {
            words.next();
        } else {
            break;
        }
    }

    let mut words: Vec<String> = words.cloned().collect();
    // A redirection marker and its target are checked as a write, then dropped.
    if let Some(position) = words.iter().position(|word| word == "__redirect__") {
        let target = words.get(position + 1).cloned().unwrap_or_default();
        words.truncate(position);
        if let Some(kind) = destination_kind(home, &target, security, class) {
            class.kind = worst(class.kind, kind);
        }
    }

    let program = words.first()?.clone();
    let args: Vec<String> = words[1..].to_vec();
    let segment = Segment {
        program: program.clone(),
        args: args.clone(),
    };

    // A program named by a variable cannot be classified.
    if program.contains('$') {
        class.unanalysable = true;
        class.kind = worst(class.kind, ActionKind::Execute);
        return Some(segment);
    }

    // The raw deny list is an accident guard, matched on the whole segment.
    let joined = std::iter::once(program.as_str())
        .chain(args.iter().map(String::as_str))
        .collect::<Vec<_>>()
        .join(" ");
    let kind = segment_kind(home, &program, &args, security, class, &joined);
    class.kind = worst(class.kind, kind);
    Some(segment)
}

fn segment_kind(
    home: Option<&Path>,
    program: &str,
    args: &[String],
    security: &WorkspaceSecurity,
    class: &mut CommandClass,
    joined: &str,
) -> ActionKind {
    if DESTRUCTIVE.iter().any(|pattern| joined.contains(pattern)) {
        return ActionKind::Destructive;
    }
    if program == "rm" && args.iter().any(|arg| is_recursive_force(arg)) {
        return ActionKind::Destructive;
    }
    if SHELLS.contains(&program)
        && args
            .iter()
            .any(|arg| arg.starts_with('-') && arg.contains('c'))
    {
        class.unanalysable = true;
        return ActionKind::Execute;
    }
    if program == "eval" || program == "exec" {
        class.unanalysable = true;
        return ActionKind::Execute;
    }
    if NETWORK_PROGRAMS.contains(&program) {
        return ActionKind::Network;
    }

    if program == "git" {
        return match args.first().map(String::as_str) {
            Some(sub) if GIT_READ_ONLY.contains(&sub) => ActionKind::Read,
            Some("branch") if args.iter().any(|arg| arg == "--list") => ActionKind::Read,
            Some("remote") if args.iter().any(|arg| arg == "-v") => ActionKind::Read,
            Some("remote") if args.iter().any(|arg| arg == "update") => ActionKind::Network,
            Some(sub) if GIT_NETWORK.contains(&sub) => ActionKind::Network,
            _ => ActionKind::Execute,
        };
    }
    if program == "cargo" {
        return match args.first().map(String::as_str) {
            Some("test" | "build" | "check" | "fmt" | "clippy") => ActionKind::Read,
            _ => ActionKind::Execute,
        };
    }

    if WRITERS.contains(&program) {
        return writer_kind(home, program, args, security, class);
    }

    if PATH_ARGS.contains(&program) {
        for arg in args.iter().filter(|arg| !arg.starts_with('-')) {
            let Some(resolved) = resolve(home, arg, security) else {
                // A glob, a variable or a substitution names a file the policy
                // cannot see, so the segment asks instead of allowing.
                class.unanalysable = true;
                return ActionKind::Execute;
            };
            if check_file(&resolved).is_err() {
                return ActionKind::Destructive;
            }
        }
    }

    if RECURSIVE_READERS.contains(&program) && recursive_read(args, home, security) {
        return ActionKind::Execute;
    }
    if READ_ONLY.contains(&program) {
        return ActionKind::Read;
    }
    ActionKind::Execute
}

fn is_recursive_force(arg: &str) -> bool {
    arg == "-rf" || arg == "-fr" || arg == "-r" || arg == "-R" || arg == "--recursive"
}

fn recursive_read(args: &[String], home: Option<&Path>, security: &WorkspaceSecurity) -> bool {
    let mut recursive_flag = false;
    for arg in args {
        match arg.as_str() {
            "-r" | "-R" | "--recursive" | "-uu" | "--hidden" | "--no-ignore" | "-a" | "--text" => {
                recursive_flag = true;
            }
            value
                if !value.starts_with('-')
                    && resolve(home, value, security).is_some_and(|path| path.is_dir()) =>
            {
                recursive_flag = true;
            }
            _ => {}
        }
    }
    recursive_flag
}

fn writer_kind(
    home: Option<&Path>,
    program: &str,
    args: &[String],
    security: &WorkspaceSecurity,
    class: &mut CommandClass,
) -> ActionKind {
    let mut paths: Vec<&String> = args.iter().filter(|arg| !arg.starts_with('-')).collect();
    if program == "dd" {
        // `dd of=<path>` names its destination explicitly.
        let Some(target) = args.iter().find_map(|arg| arg.strip_prefix("of=")) else {
            class.unanalysable = true;
            return ActionKind::Execute;
        };
        return destination_kind(home, target, security, class).unwrap_or(ActionKind::Write);
    }
    // `cp`/`mv`/`ln`/`install`/`tee` all write their last path argument.
    let Some(destination) = paths.pop() else {
        class.unanalysable = true;
        return ActionKind::Execute;
    };
    // Sources are reads, so a protected source still refuses the command.
    for source in paths {
        let protected =
            resolve(home, source, security).is_some_and(|resolved| check_file(&resolved).is_err());
        if protected {
            return ActionKind::Destructive;
        }
    }
    destination_kind(home, destination, security, class).unwrap_or(ActionKind::Write)
}

/// Classify a write destination: inside the workspace, inside a global root, or
/// a refusal. `None` means the destination could not be analysed.
fn destination_kind(
    home: Option<&Path>,
    target: &str,
    security: &WorkspaceSecurity,
    class: &mut CommandClass,
) -> Option<ActionKind> {
    if target.is_empty() || target.contains(['$', '*', '?', '`', '(', ')']) {
        class.unanalysable = true;
        return Some(ActionKind::Execute);
    }
    let resolved = resolve(home, target, security)?;
    if check_file(&resolved).is_err() {
        return Some(ActionKind::Destructive);
    }
    if resolved.starts_with(security.root()) {
        return Some(ActionKind::Write);
    }
    if global_root_for(home, &resolved).is_some() {
        return Some(ActionKind::GlobalWrite);
    }
    class.unanalysable = true;
    Some(ActionKind::Execute)
}

fn resolve(home: Option<&Path>, token: &str, security: &WorkspaceSecurity) -> Option<PathBuf> {
    let token = token.trim_matches(|c| c == '\'' || c == '"');
    if token.contains(['$', '*', '?', '`', '(', ')']) {
        return None;
    }
    if let Some(rest) = token.strip_prefix("~/") {
        return home.map(|home| home.join(rest));
    }
    let path = Path::new(token);
    if path.is_absolute() {
        Some(path.to_path_buf())
    } else {
        Some(security.root().join(path))
    }
}

/// Drop lines whose leading `path:` prefix the policy refuses, and lines that
/// cannot be read as a path prefix at all. Measured per line, so the shell may
/// still read those bytes — their content never reaches the conversation.
pub fn filter_output(text: &str, security: &WorkspaceSecurity) -> (String, usize) {
    let mut kept: Vec<&str> = Vec::new();
    let mut withheld = 0usize;
    for line in text.lines() {
        let prefix = line.split(':').next().unwrap_or_default().trim();
        let allowed = !prefix.is_empty() && {
            let path = if Path::new(prefix).is_absolute() {
                PathBuf::from(prefix)
            } else {
                security.root().join(prefix)
            };
            check_file(&path).is_ok()
        };
        if allowed {
            kept.push(line);
        } else {
            withheld += 1;
        }
    }

    let mut out = kept.join("\n");
    if withheld > 0 {
        out.push_str(&format!("\n[{withheld} lines withheld by policy]"));
    }
    (out, withheld)
}
