//! Host-side Git target resolution for `/review`.
//!
//! This module is never an `AgentTool`: the model cannot call it, and it never
//! runs a shell string. Every command is a fixed argument list, bounded in time
//! and bytes, and it never stages, commits or writes.

use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::agent::security::WorkspaceSecurity;

/// Byte cap for one review's Git output and for one untracked file. Past this
/// the review is incomplete; a truncated diff is never reported as clean.
pub const DIFF_LIMIT: usize = 1024 * 1024;
/// Wall clock for one Git process.
pub const GIT_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReviewTarget {
    WorkingTree,
    Staged,
    Commit(String),
    Base(String),
}

impl ReviewTarget {
    /// How the target is named in a report.
    pub fn label(&self) -> String {
        match self {
            Self::WorkingTree => "working-tree".to_string(),
            Self::Staged => "staged".to_string(),
            Self::Commit(reference) => format!("commit {reference}"),
            Self::Base(reference) => format!("base {reference}"),
        }
    }
}

/// Only the documented forms; anything else is refused before Git runs.
pub fn parse_target(args: &[&str]) -> Result<ReviewTarget, String> {
    match args {
        [] | ["working-tree"] => Ok(ReviewTarget::WorkingTree),
        ["staged"] => Ok(ReviewTarget::Staged),
        ["commit", reference] => Ok(ReviewTarget::Commit((*reference).to_string())),
        ["base", reference] => Ok(ReviewTarget::Base((*reference).to_string())),
        _ => Err("usage: /review [working-tree|staged|commit <ref>|base <ref>]".to_string()),
    }
}

/// Everything one review turn may see. Built only after the paths were filtered
/// and the bodies fit the cap.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewInput {
    pub target: String,
    pub diff: String,
    pub changed_paths: Vec<String>,
    pub withheld_paths: usize,
    pub is_empty: bool,
}

/// Resolve a target into reviewable text without touching the checkout.
pub fn resolve(
    root: &Path,
    target: &ReviewTarget,
    security: &WorkspaceSecurity,
) -> Result<ReviewInput, String> {
    // Fail fast when the directory is not a repository at all.
    run(root, &["rev-parse", "--git-dir"])?;

    let mut withheld = 0;
    let (names, patch_args): (Vec<String>, Vec<String>) = match target {
        ReviewTarget::WorkingTree => (
            split_nul(&run(
                root,
                &[
                    "diff",
                    "--name-only",
                    "-z",
                    "--no-ext-diff",
                    "--no-textconv",
                    "HEAD",
                ],
            )?),
            vec!["diff".to_string(), "HEAD".to_string()],
        ),
        ReviewTarget::Staged => (
            split_nul(&run(
                root,
                &[
                    "diff",
                    "--cached",
                    "--name-only",
                    "-z",
                    "--no-ext-diff",
                    "--no-textconv",
                    "HEAD",
                ],
            )?),
            vec![
                "diff".to_string(),
                "--cached".to_string(),
                "HEAD".to_string(),
            ],
        ),
        ReviewTarget::Commit(reference) => {
            let commit = verify_commit(root, reference)?;
            (
                split_nul(&run(
                    root,
                    &[
                        "diff-tree",
                        "--root",
                        "--no-commit-id",
                        "--name-only",
                        "-r",
                        "-z",
                        &commit,
                    ],
                )?),
                vec![
                    "diff-tree".to_string(),
                    "--root".to_string(),
                    "--no-commit-id".to_string(),
                    "-p".to_string(),
                    "--no-ext-diff".to_string(),
                    "--no-textconv".to_string(),
                    commit,
                ],
            )
        }
        ReviewTarget::Base(reference) => {
            let commit = verify_commit(root, reference)?;
            let merge_base = run_text(root, &["merge-base", "HEAD", &commit])?
                .trim()
                .to_string();
            if merge_base.is_empty() {
                return Err(format!("no merge base between HEAD and {reference}"));
            }
            (
                split_nul(&run(
                    root,
                    &[
                        "diff",
                        "--name-only",
                        "-z",
                        "--no-ext-diff",
                        "--no-textconv",
                        &merge_base,
                        "HEAD",
                    ],
                )?),
                vec![
                    "diff".to_string(),
                    "--no-ext-diff".to_string(),
                    "--no-textconv".to_string(),
                    merge_base,
                    "HEAD".to_string(),
                ],
            )
        }
    };

    // Paths first: nothing below reads a body the security check refused.
    let (mut visible, refused) = filter_paths(&names, security);
    withheld += refused;

    let mut diff = String::new();
    if !visible.is_empty() {
        let mut command = patch_args;
        command.push("--".to_string());
        command.extend(visible.iter().cloned());
        let args: Vec<&str> = command.iter().map(String::as_str).collect();
        diff = String::from_utf8_lossy(&run(root, &args)?).to_string();
    }

    // Untracked files are not in any diff; their body is the whole change.
    if matches!(target, ReviewTarget::WorkingTree) {
        let untracked = split_nul(&run(
            root,
            &["ls-files", "-z", "--others", "--exclude-standard"],
        )?);
        eprintln!("DEBUG untracked={untracked:?} cap={DIFF_LIMIT}");
        let (files, refused) = filter_untracked(root, &untracked, security)?;
        withheld += refused;
        for path in files {
            diff.push_str(&untracked_block(root, &path)?);
            visible.push(path);
        }
    }

    if diff.len() > DIFF_LIMIT {
        return Err(format!(
            "the review input exceeds {DIFF_LIMIT} bytes (1 MiB); review it in smaller pieces"
        ));
    }

    Ok(ReviewInput {
        target: target.label(),
        is_empty: diff.trim().is_empty(),
        diff,
        changed_paths: visible,
        withheld_paths: withheld,
    })
}

/// Resolve a ref to a full commit id, or refuse. `--end-of-options` keeps a ref
/// from being read as a flag.
fn verify_commit(root: &Path, reference: &str) -> Result<String, String> {
    let spec = format!("{reference}^{{commit}}");
    let commit = run_text(
        root,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            "--end-of-options",
            &spec,
        ],
    )?;
    let commit = commit.trim().to_string();
    if commit.is_empty() {
        return Err(format!("'{reference}' does not name a commit"));
    }
    Ok(commit)
}

/// Keep the paths a review may show, and count the ones it may not.
fn filter_paths(names: &[String], security: &WorkspaceSecurity) -> (Vec<String>, usize) {
    let mut visible = Vec::new();
    let mut refused = 0;
    for name in names {
        if name.is_empty() {
            continue;
        }
        if security.validate_path(name).is_ok() {
            visible.push(name.clone());
        } else {
            refused += 1;
        }
    }
    (visible, refused)
}

/// Untracked files must be regular, inside the workspace, and policy-clean.
fn filter_untracked(
    root: &Path,
    names: &[String],
    security: &WorkspaceSecurity,
) -> Result<(Vec<String>, usize), String> {
    let mut visible = Vec::new();
    let mut refused = 0;
    for name in names {
        if name.is_empty() {
            continue;
        }
        let Ok(path) = security.validate_path(name) else {
            refused += 1;
            continue;
        };
        // Never follow a link: it can leave the workspace after validation.
        let Ok(metadata) = std::fs::symlink_metadata(&path) else {
            refused += 1;
            continue;
        };
        if !metadata.file_type().is_file() || !path.starts_with(root) {
            refused += 1;
            continue;
        }
        // A file too large to review fails the review: silently dropping it
        // would report a change set the reviewer never saw.
        if metadata.len() as usize > DIFF_LIMIT {
            return Err(format!(
                "{name} exceeds {DIFF_LIMIT} bytes (1 MiB); the review is incomplete"
            ));
        }
        visible.push(name.clone());
    }
    Ok((visible, refused))
}

/// Untracked content, shaped like the diff hunk a reviewer would see.
fn untracked_block(root: &Path, name: &str) -> Result<String, String> {
    let path = root.join(name);
    let bytes = std::fs::read(&path).map_err(|error| format!("cannot read {name}: {error}"))?;
    if bytes.len() > DIFF_LIMIT {
        return Err(format!("{name} exceeds {DIFF_LIMIT} bytes"));
    }
    let body = String::from_utf8_lossy(&bytes);
    let mut block = format!("--- /dev/null\n+++ b/{name}\n");
    for line in body.lines() {
        block.push('+');
        block.push_str(line);
        block.push('\n');
    }
    Ok(block)
}

/// Git's `-z` output: NUL-separated, one path per entry.
fn split_nul(bytes: &[u8]) -> Vec<String> {
    // shortcut: non-UTF-8 paths are lossily decoded; upgrade if a repository
    // with such a path ever needs an exact round-trip.
    String::from_utf8_lossy(bytes)
        .split('\0')
        .filter(|part| !part.is_empty())
        .map(str::to_string)
        .collect()
}

fn run_text(root: &Path, args: &[&str]) -> Result<String, String> {
    Ok(String::from_utf8_lossy(&run(root, args)?).to_string())
}

/// One Git process: fixed arguments, no shell, no lock writes, no external diff,
/// no text conversion, bounded output and bounded wall clock.
fn run(root: &Path, args: &[&str]) -> Result<Vec<u8>, String> {
    let mut command = Command::new("git");
    command
        .current_dir(root)
        .args(args)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", null_device())
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command
        .spawn()
        .map_err(|error| format!("git {}: {error}", args.first().unwrap_or(&"")))?;

    let stdout = child.stdout.take().expect("piped stdout");
    let stderr = child.stderr.take().expect("piped stderr");
    let out = std::thread::spawn(move || read_capped(stdout));
    let err = std::thread::spawn(move || read_capped(stderr));

    // Poll rather than wait, so a wedged process cannot outlive the budget.
    let deadline = Instant::now() + GIT_TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!(
                    "git {} timed out after {}s",
                    args.first().unwrap_or(&""),
                    GIT_TIMEOUT.as_secs()
                ));
            }
            Err(error) => return Err(format!("git {}: {error}", args.first().unwrap_or(&""))),
        }
    };

    let (out, out_overflow) = out.join().unwrap_or((Vec::new(), false));
    let (err, err_overflow) = err.join().unwrap_or((Vec::new(), false));
    if out_overflow || err_overflow {
        return Err(format!(
            "git {} produced more than {DIFF_LIMIT} bytes; the review is incomplete",
            args.first().unwrap_or(&"")
        ));
    }
    if !status.success() {
        let message = String::from_utf8_lossy(&err).trim().to_string();
        return Err(if message.is_empty() {
            format!("git {} failed", args.first().unwrap_or(&""))
        } else {
            message
        });
    }
    Ok(out)
}

/// Read at most `DIFF_LIMIT` bytes, and say whether more arrived.
fn read_capped(mut stream: impl Read) -> (Vec<u8>, bool) {
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 16 * 1024];
    loop {
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => return (buffer, false),
            Ok(read) => {
                let room = DIFF_LIMIT.saturating_sub(buffer.len());
                if read > room {
                    buffer.extend_from_slice(&chunk[..room]);
                    return (buffer, true);
                }
                buffer.extend_from_slice(&chunk[..read]);
            }
        }
    }
}

fn null_device() -> &'static str {
    if cfg!(windows) { "NUL" } else { "/dev/null" }
}

#[cfg(test)]
#[path = "../tests/agent/git_review.rs"]
mod tests;
