use crate::agent::engine::AgentMode;
use crate::agent::permissions::ActionKind;
use crate::agent::security::WorkspaceSecurity;
use crate::atomic_file::{ModePolicy, write_atomic};
use crate::checkpoint::{CheckpointInput, CheckpointStore, CheckpointTarget, EntryState};
use async_trait::async_trait;
use schemars::{JsonSchema, schema_for};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[async_trait]
pub trait AgentTool: Send + Sync {
    fn name(&self) -> &'static str;
    fn description(&self) -> &'static str;
    fn parameters_schema(&self) -> Value;

    /// Tools default to mutation. Only explicitly classified tools are safe in Plan Mode.
    fn is_read_only(&self) -> bool {
        false
    }

    /// What this call may do. Defaults to a workspace write; tools that never
    /// mutate, and `run_command`, override it.
    fn classify(&self, _args: &Value, _security: &WorkspaceSecurity) -> ActionKind {
        ActionKind::Write
    }

    /// Where this call writes when the destination is chosen by the tool rather
    /// than by `validate_path`, so the engine can classify a global write.
    fn destination(&self, _args: &Value, _security: &WorkspaceSecurity) -> Option<PathBuf> {
        None
    }

    async fn execute(&self, args: Value, security: &WorkspaceSecurity) -> Result<String, String>;
}

// ---- Shared cores ----------------------------------------------------------

fn parse_args<T: DeserializeOwned>(args: Value) -> Result<T, String> {
    serde_json::from_value(args).map_err(|e| format!("Invalid args: {e}"))
}

fn schema<T: JsonSchema>() -> Value {
    serde_json::to_value(schema_for!(T)).unwrap_or_default()
}

/// Create parent directories when needed, then write content atomically. Shared
/// core of `write_file` and `skill_write`; `ctx` labels the target in errors.
fn write_with_parents(path: &Path, content: String, ctx: &str) -> Result<(), String> {
    crate::agent::security::check_write_target(path)?;
    write_atomic(path, content.as_bytes(), ModePolicy::Preserve)
        .map_err(|e| format!("Failed to write {ctx}: {e}"))
}

/// Capture the pre-write bytes of `path` so `/undo` can restore them. A target
/// outside the workspace is accepted only inside an allowed global root.
fn capture_checkpoint(security: &WorkspaceSecurity, path: &Path) -> Result<(), String> {
    // Existence decides the class, never a successful read: recording an
    // existing file as `created` would make `/undo` delete it.
    let exists = std::fs::symlink_metadata(path).is_ok();
    let original = if exists {
        Some(std::fs::read(path).map_err(|error| {
            format!(
                "refusing to write {}: it exists but cannot be read, so it cannot be checkpointed ({error})",
                path.display()
            )
        })?)
    } else {
        None
    };
    let state = if exists {
        EntryState::Modified
    } else {
        EntryState::Created
    };
    let target = match path.strip_prefix(security.root()) {
        Ok(_) => CheckpointTarget::Workspace(path.to_path_buf()),
        Err(_) => {
            let allowed_root =
                crate::agent::permissions::global_root_for(crate::home::dir().as_deref(), path)
                    .ok_or_else(|| {
                        format!(
                            "Refusing to write outside the workspace: {}",
                            path.display()
                        )
                    })?;
            CheckpointTarget::Global {
                path: path.to_path_buf(),
                allowed_root,
            }
        }
    };

    CheckpointStore::create(
        security.root(),
        &[CheckpointInput {
            target,
            state,
            original,
        }],
    )
    .map(|_| ())
    .map_err(|error| format!("Checkpoint failed: {error}"))
}

/// Replace `old_string` and write the file back, returning how many occurrences
/// changed. A snippet that occurs more than once is refused unless `replace_all`
/// is set: silently editing the first match edits the wrong place.
async fn replace_unique(
    path: &Path,
    old_string: &str,
    new_string: &str,
    replace_all: bool,
    ctx: &str,
) -> Result<usize, String> {
    if old_string.is_empty() {
        return Err("old_string must not be empty".to_string());
    }

    let content = crate::agent::security::read_verified(path)
        .await
        .map_err(|e| format!("Failed to read {ctx} for editing: {e}"))?;

    let occurrences = content.matches(old_string).count();
    if occurrences == 0 {
        return Err(format!("Target old_string not found in {ctx}"));
    }
    if occurrences > 1 && !replace_all {
        let samples: Vec<String> = content
            .lines()
            .enumerate()
            .filter(|(_, line)| line.contains(old_string))
            .take(3)
            .map(|(index, line)| format!("line {}: {}", index + 1, line.trim()))
            .collect();
        return Err(format!(
            "old_string occurs {occurrences} times in {ctx}; add context or pass replace_all\n{}",
            samples.join("\n")
        ));
    }

    // An identical replacement is a noted no-op, not a rewrite of the same bytes.
    if old_string == new_string {
        return Ok(0);
    }

    let count = if replace_all { occurrences } else { 1 };
    let updated = content.replacen(old_string, new_string, count);
    crate::agent::security::check_write_target(path)?;
    write_atomic(path, updated.as_bytes(), ModePolicy::Preserve)
        .map_err(|e| format!("Failed to save {ctx} edit: {e}"))?;
    Ok(count)
}

// 1. read_file
#[derive(Deserialize, JsonSchema)]
struct ReadFileArgs {
    path: String,
    offset: Option<usize>,
    limit: Option<usize>,
}

pub struct ReadFileTool;

#[async_trait]
impl AgentTool for ReadFileTool {
    fn name(&self) -> &'static str {
        "read_file"
    }

    fn classify(&self, _args: &Value, _security: &WorkspaceSecurity) -> ActionKind {
        ActionKind::Read
    }

    fn description(&self) -> &'static str {
        "Read file contents within workspace, with optional line offset (1-based) and limit"
    }

    fn is_read_only(&self) -> bool {
        true
    }

    fn parameters_schema(&self) -> Value {
        schema::<ReadFileArgs>()
    }

    async fn execute(&self, args: Value, security: &WorkspaceSecurity) -> Result<String, String> {
        let parsed = parse_args::<ReadFileArgs>(args)?;
        let path = security.validate_path(&parsed.path)?;
        let content = crate::agent::security::read_verified(&path).await?;

        let lines: Vec<&str> = content.lines().collect();
        let total_lines = lines.len();
        let offset = parsed.offset.unwrap_or(1).max(1);
        let start_idx = (offset - 1).min(total_lines);
        let max_limit = parsed.limit.unwrap_or(2000).min(5000);
        let end_idx = (start_idx + max_limit).min(total_lines);

        let mut output = String::new();
        for (i, line) in lines[start_idx..end_idx].iter().enumerate() {
            output.push_str(&format!("{}|{}\n", start_idx + i + 1, line));
        }

        if end_idx < total_lines {
            output.push_str(&format!(
                "[Truncated: showing lines {}-{} of {} lines total]\n",
                start_idx + 1,
                end_idx,
                total_lines
            ));
        }

        Ok(output)
    }
}

// 2. write_file
#[derive(Deserialize, JsonSchema)]
struct WriteFileArgs {
    path: String,
    content: String,
}

pub struct WriteFileTool;

#[async_trait]
impl AgentTool for WriteFileTool {
    fn name(&self) -> &'static str {
        "write_file"
    }

    fn description(&self) -> &'static str {
        "Write content to a file, creating parent directories if needed"
    }

    fn parameters_schema(&self) -> Value {
        schema::<WriteFileArgs>()
    }

    async fn execute(&self, args: Value, security: &WorkspaceSecurity) -> Result<String, String> {
        let parsed = parse_args::<WriteFileArgs>(args)?;
        let path = security.validate_path(&parsed.path)?;
        capture_checkpoint(security, &path)?;
        write_with_parents(&path, parsed.content, "file")?;
        Ok(format!("Successfully wrote {}", parsed.path))
    }
}

// 3. edit_file
#[derive(Deserialize, JsonSchema)]
struct EditFileArgs {
    path: String,
    old_string: String,
    new_string: String,
    /// Replace every occurrence instead of refusing an ambiguous snippet.
    #[serde(default)]
    replace_all: bool,
}

pub struct EditFileTool;

#[async_trait]
impl AgentTool for EditFileTool {
    fn name(&self) -> &'static str {
        "edit_file"
    }

    fn description(&self) -> &'static str {
        "Replace an exact snippet with new text inside an existing file"
    }

    fn parameters_schema(&self) -> Value {
        schema::<EditFileArgs>()
    }

    async fn execute(&self, args: Value, security: &WorkspaceSecurity) -> Result<String, String> {
        let parsed = parse_args::<EditFileArgs>(args)?;
        let path = security.validate_path(&parsed.path)?;
        capture_checkpoint(security, &path)?;
        let count = replace_unique(
            &path,
            &parsed.old_string,
            &parsed.new_string,
            parsed.replace_all,
            "file",
        )
        .await?;
        Ok(if count == 0 {
            format!("No change to {}: old_string equals new_string", parsed.path)
        } else {
            format!(
                "Successfully updated {} ({count} replacement(s))",
                parsed.path
            )
        })
    }
}

// 4. list_directory
#[derive(Deserialize, JsonSchema)]
struct ListDirArgs {
    path: Option<String>,
}

pub struct ListDirectoryTool;

#[async_trait]
impl AgentTool for ListDirectoryTool {
    fn name(&self) -> &'static str {
        "list_directory"
    }

    fn classify(&self, _args: &Value, _security: &WorkspaceSecurity) -> ActionKind {
        ActionKind::Read
    }

    fn description(&self) -> &'static str {
        "List files and subdirectories in a workspace path"
    }

    fn is_read_only(&self) -> bool {
        true
    }

    fn parameters_schema(&self) -> Value {
        schema::<ListDirArgs>()
    }

    async fn execute(&self, args: Value, security: &WorkspaceSecurity) -> Result<String, String> {
        let parsed = parse_args::<ListDirArgs>(args)?;
        let req_path = match parsed.path {
            Some(ref p) if p != "None" && !p.trim().is_empty() => p.as_str(),
            _ => ".",
        };
        let path = security.validate_path(req_path)?;

        let mut entries = tokio::fs::read_dir(path)
            .await
            .map_err(|e| format!("Failed to read directory: {e}"))?;
        let mut list = Vec::new();

        while let Ok(Some(entry)) = entries.next_entry().await {
            let name = entry.file_name().to_string_lossy().to_string();
            let is_dir = entry.file_type().await.map(|t| t.is_dir()).unwrap_or(false);
            if is_dir {
                list.push(format!("{name}/"));
            } else {
                list.push(name);
            }
        }
        list.sort();
        Ok(list.join("\n"))
    }
}

// 5. run_command
#[derive(Deserialize, JsonSchema)]
struct RunCommandArgs {
    command: String,
}

pub struct RunCommandTool;

#[async_trait]
impl AgentTool for RunCommandTool {
    fn name(&self) -> &'static str {
        "run_command"
    }

    fn classify(&self, args: &Value, security: &WorkspaceSecurity) -> ActionKind {
        let command = args
            .get("command")
            .and_then(Value::as_str)
            .unwrap_or_default();
        crate::agent::shell::classify(command, security).kind
    }

    fn description(&self) -> &'static str {
        "Run shell command safely in workspace root with 30s timeout"
    }

    fn parameters_schema(&self) -> Value {
        schema::<RunCommandArgs>()
    }

    async fn execute(&self, args: Value, security: &WorkspaceSecurity) -> Result<String, String> {
        let parsed = parse_args::<RunCommandArgs>(args)?;
        let class = crate::agent::shell::classify(&parsed.command, security);

        // The engine already refused this through the matrix; the belt exists so a
        // caller that skips the engine still cannot run it.
        if class.kind == ActionKind::Destructive {
            return Err("Command refused by security policy".to_string());
        }

        let output = run_shell(&parsed.command, security, COMMAND_TIMEOUT).await?;
        let stdout = cap_stream(&String::from_utf8_lossy(&output.stdout));
        let stderr = cap_stream(&String::from_utf8_lossy(&output.stderr));

        // A recursive read names a directory, so its lines are filtered: the
        // policy cannot see the files it walked.
        let recursive = class.segments.iter().any(|segment| {
            crate::agent::shell::RECURSIVE_READERS.contains(&segment.program.as_str())
        });
        let (stdout, withheld) = if recursive {
            crate::agent::shell::filter_output(&stdout, security)
        } else {
            (stdout, 0)
        };
        let _ = withheld;

        let status = output.status.code().unwrap_or(-1);
        let text = format!("exit: {status}\nstdout:\n{stdout}\nstderr:\n{stderr}");
        Ok(crate::agent::security::redact(&text))
    }
}

/// Wall-clock ceiling for one command; the child is killed when it expires.
pub(crate) const COMMAND_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
/// Per-stream ceiling, split evenly between the head and the tail.
const STREAM_CAP: usize = 32 * 1024;
const STREAM_HALF: usize = 16 * 1024;

/// Run one command with a cleared environment: a tool call cannot read the
/// agent's own `SEAVX_*` variables, and the child dies with the future.
pub(crate) async fn run_shell(
    command: &str,
    security: &WorkspaceSecurity,
    limit: std::time::Duration,
) -> Result<std::process::Output, String> {
    // Windows has no `sh`; `cmd /C` runs the same command through the system shell.
    #[cfg(windows)]
    let mut shell = {
        let mut cmd = tokio::process::Command::new("cmd");
        cmd.arg("/C");
        cmd
    };
    #[cfg(not(windows))]
    let mut shell = {
        let mut cmd = tokio::process::Command::new("sh");
        cmd.arg("-c");
        cmd
    };

    shell
        .arg(command)
        .current_dir(security.root())
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .env_clear()
        .kill_on_drop(true);
    for name in ["PATH", "LANG", "TERM"] {
        if let Ok(value) = std::env::var(name) {
            shell.env(name, value);
        }
    }
    // Most tooling breaks without `HOME`; the agent's own variables never pass.
    if let Some(home) = crate::home::dir() {
        shell.env("HOME", home);
    }

    let child = shell
        .spawn()
        .map_err(|e| format!("Process execution failed: {e}"))?;
    match tokio::time::timeout(limit, child.wait_with_output()).await {
        Err(_) => Err(format!(
            "Command execution timed out ({}s limit)",
            limit.as_secs()
        )),
        Ok(Err(error)) => Err(format!("Process execution failed: {error}")),
        Ok(Ok(output)) => Ok(output),
    }
}

/// Keep a bounded head and tail so a chatty command cannot flood the context.
fn cap_stream(text: &str) -> String {
    if text.len() <= STREAM_CAP {
        return text.to_string();
    }
    let head: String = text.chars().take(STREAM_HALF).collect();
    let tail: String = text
        .chars()
        .rev()
        .take(STREAM_HALF)
        .collect::<Vec<char>>()
        .into_iter()
        .rev()
        .collect();
    let omitted = text.len().saturating_sub(head.len() + tail.len());
    format!("{head}\n[{omitted} bytes omitted]\n{tail}")
}

// 6. search_files
#[derive(Deserialize, JsonSchema)]
struct SearchFilesArgs {
    pattern: String,
    target: Option<String>, // "content" or "files", default "content"
    path: Option<String>,
}

pub struct SearchFilesTool;

#[async_trait]
impl AgentTool for SearchFilesTool {
    fn name(&self) -> &'static str {
        "search_files"
    }

    fn classify(&self, _args: &Value, _security: &WorkspaceSecurity) -> ActionKind {
        ActionKind::Read
    }

    fn description(&self) -> &'static str {
        "Search file contents (target='content', default) or match filenames (target='files') across the workspace"
    }

    fn is_read_only(&self) -> bool {
        true
    }

    fn parameters_schema(&self) -> Value {
        schema::<SearchFilesArgs>()
    }

    async fn execute(&self, args: Value, security: &WorkspaceSecurity) -> Result<String, String> {
        let parsed = parse_args::<SearchFilesArgs>(args)?;
        let target_mode = parsed.target.unwrap_or_else(|| "content".to_string());
        let search_path = parsed.path.unwrap_or_else(|| ".".to_string());
        let root = security.validate_path(&search_path)?;

        let pattern = parsed.pattern.clone();
        let results =
            tokio::task::spawn_blocking(move || -> Result<(Vec<String>, usize), String> {
                let mut matches = Vec::new();
                let mut skipped = 0usize;
                let mut stack = vec![root];

                while let Some(current_dir) = stack.pop() {
                    let read_dir = match std::fs::read_dir(&current_dir) {
                        Ok(rd) => rd,
                        Err(_) => continue,
                    };

                    for entry in read_dir.flatten() {
                        let path = entry.path();
                        let file_name = path
                            .file_name()
                            .map(|s| s.to_string_lossy())
                            .unwrap_or_default();

                        // Policy before the skip list: a protected file is counted even
                        // when it is hidden, while a protected directory is dropped
                        // without a count because it is not a file result.
                        if crate::agent::security::check_file(&path).is_err() {
                            if path.is_file() {
                                skipped += 1;
                            }
                            continue;
                        }

                        // Skip hidden dirs, build artifacts and dependencies like every other walk.
                        if crate::util::is_skipped(&file_name) {
                            continue;
                        }

                        if path.is_dir() {
                            stack.push(path);
                        } else if path.is_file() {
                            if target_mode == "files" {
                                if file_name.contains(&pattern) {
                                    matches.push(path.display().to_string());
                                }
                            } else {
                                if let Ok(content) = std::fs::read_to_string(&path) {
                                    for (idx, line) in content.lines().enumerate() {
                                        if line.contains(&pattern) {
                                            matches.push(format!(
                                                "{}:{}: {}",
                                                path.display(),
                                                idx + 1,
                                                line.trim()
                                            ));
                                            if matches.len() >= 100 {
                                                break;
                                            }
                                        }
                                    }
                                }
                            }
                        }

                        if matches.len() >= 100 {
                            break;
                        }
                    }

                    if matches.len() >= 100 {
                        break;
                    }
                }

                Ok((matches, skipped))
            })
            .await
            .map_err(|e| format!("Search task failed: {e}"))??;

        let (results, skipped) = results;
        let mut output = if results.is_empty() {
            format!("No matches found for pattern '{}'", parsed.pattern)
        } else {
            let count = results.len();
            let mut output = results.join("\n");
            if count >= 100 {
                output.push_str("\n[Truncated to 100 results]");
            }
            output
        };
        if skipped > 0 {
            output.push_str(&format!("\n[{skipped} files skipped by policy]"));
        }
        Ok(crate::agent::security::redact(&output))
    }
}

/// The file `save_memory` writes, shared by `execute` and the permission check.
fn memory_target(
    security: &WorkspaceSecurity,
    target: Option<&str>,
) -> Result<std::path::PathBuf, String> {
    let user_target = target == Some("user");
    let is_debug = crate::util::is_debug();
    let path = match (user_target, is_debug) {
        (true, true) => security.root().join(".seavx").join("USER.md"),
        (true, false) => crate::home::dir()
            .ok_or_else(|| "home directory not set".to_string())?
            .join(".seavx")
            .join("USER.md"),
        (false, true) => security.root().join(".seavx").join("MEMORY.md"),
        (false, false) => security.root().join("MEMORY.md"),
    };
    Ok(path)
}

// 7. save_memory
#[derive(Deserialize, JsonSchema)]
struct SaveMemoryArgs {
    lesson: String,
    #[serde(default, deserialize_with = "deserialize_confidence")]
    confidence: Option<u8>, // Confidence percentage 0-100% (default 80%)
    target: Option<String>, // "project" (MEMORY.md) or "user" (USER.md), default "project"
}

fn deserialize_confidence<'de, D>(deserializer: D) -> Result<Option<u8>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::Error;
    let value = Option::<serde_json::Value>::deserialize(deserializer)?;
    match value {
        Some(serde_json::Value::Number(n)) => {
            if let Some(u) = n.as_u64() {
                Ok(Some(u as u8))
            } else {
                Err(D::Error::custom("Invalid number for confidence"))
            }
        }
        Some(serde_json::Value::String(s)) => {
            let trimmed = s.trim().trim_end_matches('%');
            trimmed
                .parse::<u8>()
                .map(Some)
                .map_err(|e| D::Error::custom(format!("Invalid string number: {e}")))
        }
        Some(_) => Err(D::Error::custom("Expected number or string for confidence")),
        None => Ok(None),
    }
}

pub struct SaveMemoryTool;

#[async_trait]
impl AgentTool for SaveMemoryTool {
    fn name(&self) -> &'static str {
        "save_memory"
    }

    fn destination(
        &self,
        args: &Value,
        security: &WorkspaceSecurity,
    ) -> Option<std::path::PathBuf> {
        memory_target(security, args.get("target").and_then(Value::as_str)).ok()
    }

    fn description(&self) -> &'static str {
        "Save a durable lesson/preference with confidence score (0-100%). Rejects if confidence < 70%."
    }

    fn parameters_schema(&self) -> Value {
        schema::<SaveMemoryArgs>()
    }

    async fn execute(&self, args: Value, security: &WorkspaceSecurity) -> Result<String, String> {
        let parsed = parse_args::<SaveMemoryArgs>(args)?;

        let lesson = parsed.lesson.trim();
        if lesson.is_empty() {
            return Err("Lesson cannot be empty".to_string());
        }

        // Singularity / Threshold Gate: Confidence percentage threshold
        let confidence = parsed.confidence.unwrap_or(80);
        if confidence < 70 {
            return Err(format!(
                "Memory rejected: confidence score {confidence}% is below threshold (minimum 70% required)"
            ));
        }

        // Filter potential credentials/secrets
        let sensitive = ["api_key", "secret", "password", "token", "sk-", "ghp_"];
        let lower = lesson.to_lowercase();
        for s in &sensitive {
            if lower.contains(s) {
                return Err(format!(
                    "Memory rejected: potential credential detected ({s})"
                ));
            }
        }

        let target = parsed.target.unwrap_or_else(|| "project".to_string());
        let target_file = memory_target(security, Some(&target))?;
        if let Some(dir) = target_file.parent() {
            let _ = tokio::fs::create_dir_all(dir).await;
        }

        let existing = tokio::fs::read_to_string(&target_file)
            .await
            .unwrap_or_default();

        if existing.contains(lesson) {
            return Ok(format!(
                "Lesson already recorded in {}",
                target_file.display()
            ));
        }

        let mut updated = existing;
        if updated.trim().is_empty() {
            let header = if target == "user" {
                "# User Preferences\n\n"
            } else {
                "# Project Memory\n\n"
            };
            updated.push_str(header);
        } else if !updated.ends_with('\n') {
            updated.push('\n');
        }

        updated.push_str(&format!("- [{confidence}%] {lesson}\n"));

        capture_checkpoint(security, &target_file)?;
        write_atomic(&target_file, updated.as_bytes(), ModePolicy::Preserve)
            .map_err(|e| format!("Failed to write memory: {e}"))?;

        Ok(format!(
            "Saved [{confidence}%] to {}: {}",
            target_file.display(),
            lesson
        ))
    }
}

/// Where a skill write lands: the existing file when it exists, else the path the
/// manager would create.
fn skill_destination(args: &Value, security: &WorkspaceSecurity) -> Option<std::path::PathBuf> {
    let name = args.get("name").and_then(Value::as_str)?;
    find_skill_file(security, name).ok().or_else(|| {
        crate::agent::skills::SkillManager::new(security.root())
            .skill_path(name)
            .ok()
    })
}

fn find_skill_file(security: &WorkspaceSecurity, name: &str) -> Result<std::path::PathBuf, String> {
    crate::agent::skills::SkillManager::new(security.root())
        .list_skills()
        .into_iter()
        .find(|s| s.name == name)
        .map(|s| s.path)
        .ok_or_else(|| format!("Skill '{name}' not found"))
}

// 8. skill_view
#[derive(Deserialize, JsonSchema)]
struct SkillViewArgs {
    name: String,
}

pub struct SkillViewTool;

#[async_trait]
impl AgentTool for SkillViewTool {
    fn name(&self) -> &'static str {
        "skill_view"
    }

    fn classify(&self, _args: &Value, _security: &WorkspaceSecurity) -> ActionKind {
        ActionKind::Read
    }

    fn description(&self) -> &'static str {
        "Read full instructions of an available skill by name"
    }

    fn is_read_only(&self) -> bool {
        true
    }

    fn parameters_schema(&self) -> Value {
        schema::<SkillViewArgs>()
    }

    async fn execute(&self, args: Value, security: &WorkspaceSecurity) -> Result<String, String> {
        let parsed = parse_args::<SkillViewArgs>(args)?;
        let path = find_skill_file(security, &parsed.name)?;
        tokio::fs::read_to_string(path)
            .await
            .map_err(|e| format!("Failed to read skill: {e}"))
    }
}

// 8b. apply_patch
#[derive(Deserialize, JsonSchema)]
struct ApplyPatchArgs {
    /// A unified diff. Every hunk must match exactly; nothing is written unless
    /// every file applies.
    patch: String,
}

pub struct ApplyPatchTool;

#[async_trait]
impl AgentTool for ApplyPatchTool {
    fn name(&self) -> &'static str {
        "apply_patch"
    }

    fn description(&self) -> &'static str {
        "Apply a unified diff to one or more files as a single all-or-nothing, undoable change"
    }

    fn parameters_schema(&self) -> Value {
        schema::<ApplyPatchArgs>()
    }

    async fn execute(&self, args: Value, security: &WorkspaceSecurity) -> Result<String, String> {
        let parsed = parse_args::<ApplyPatchArgs>(args)?;
        let files = crate::agent::patch::parse(&parsed.patch)
            .map_err(|error| format!("Patch refused: {error}"))?;

        // Every file applies in memory first, so a later failure writes nothing.
        let mut edits = Vec::new();
        let mut summary = Vec::new();
        let mut hunks = 0usize;
        for file in &files {
            let requested = file
                .path
                .to_str()
                .ok_or_else(|| format!("{}: path is not valid UTF-8", file.path.display()))?;
            let path = security.validate_path(requested)?;
            let current = match crate::agent::security::read_verified(&path).await {
                Ok(text) => text,
                Err(_) if file.create => String::new(),
                Err(error) => return Err(format!("{requested}: {error}")),
            };
            let (updated, stats) = crate::agent::patch::apply_to(&current, file)?;
            hunks += stats.hunks;
            summary.push(format!(
                "+{} -{} {}",
                stats.added,
                stats.removed,
                file.path.display()
            ));
            edits.push(crate::agent::batch::FileEdit {
                path,
                content: updated.into_bytes(),
            });
        }

        let report = crate::agent::batch::commit(security, edits)
            .map_err(|error| format!("Patch not applied: {error}"))?;
        Ok(format!(
            "Applied {hunks} hunk(s) in {} file(s):\n{}\ncheckpoint: {}",
            report.written.len(),
            summary.join("\n"),
            report.checkpoint.display()
        ))
    }
}

// 9. skill_write
#[derive(Deserialize, JsonSchema)]
struct SkillWriteArgs {
    name: String,
    content: String,
}

pub struct SkillWriteTool;

#[async_trait]
impl AgentTool for SkillWriteTool {
    fn name(&self) -> &'static str {
        "skill_write"
    }

    fn destination(
        &self,
        args: &Value,
        security: &WorkspaceSecurity,
    ) -> Option<std::path::PathBuf> {
        skill_destination(args, security)
    }

    fn description(&self) -> &'static str {
        "Create a new skill or overwrite an existing skill's SKILL.md with full content"
    }

    fn parameters_schema(&self) -> Value {
        schema::<SkillWriteArgs>()
    }

    async fn execute(&self, args: Value, security: &WorkspaceSecurity) -> Result<String, String> {
        let parsed = parse_args::<SkillWriteArgs>(args)?;
        if parsed.content.trim().is_empty() {
            return Err("Skill content cannot be empty".to_string());
        }

        // Overwrite in place when the skill already exists (could live in a shared skills root).
        if let Ok(path) = find_skill_file(security, &parsed.name) {
            capture_checkpoint(security, &path)?;
            write_with_parents(&path, parsed.content, "skill")?;
            return Ok(format!("Successfully updated {}", path.display()));
        }

        let manager = crate::agent::skills::SkillManager::new(security.root());
        capture_checkpoint(security, &manager.skill_path(&parsed.name)?)?;
        let path = manager.save_skill(&parsed.name, &parsed.content)?;
        Ok(format!("Successfully created {}", path.display()))
    }
}

// 10. skill_patch
#[derive(Deserialize, JsonSchema)]
struct SkillPatchArgs {
    name: String,
    old_string: String,
    new_string: String,
}

pub struct SkillPatchTool;

#[async_trait]
impl AgentTool for SkillPatchTool {
    fn name(&self) -> &'static str {
        "skill_patch"
    }

    fn destination(
        &self,
        args: &Value,
        security: &WorkspaceSecurity,
    ) -> Option<std::path::PathBuf> {
        skill_destination(args, security)
    }

    fn description(&self) -> &'static str {
        "Replace an exact snippet with new text inside an existing skill's SKILL.md"
    }

    fn parameters_schema(&self) -> Value {
        schema::<SkillPatchArgs>()
    }

    async fn execute(&self, args: Value, security: &WorkspaceSecurity) -> Result<String, String> {
        let parsed = parse_args::<SkillPatchArgs>(args)?;
        let path = find_skill_file(security, &parsed.name)?;
        capture_checkpoint(security, &path)?;
        let count = replace_unique(
            &path,
            &parsed.old_string,
            &parsed.new_string,
            false,
            "skill",
        )
        .await?;
        Ok(if count == 0 {
            format!(
                "No change to skill {}: old_string equals new_string",
                parsed.name
            )
        } else {
            format!(
                "Successfully patched {} ({count} replacement(s))",
                parsed.name
            )
        })
    }
}

#[derive(Default)]
pub struct ToolRegistry {
    tools: HashMap<String, Arc<dyn AgentTool>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_default_tools() -> Self {
        let mut registry = Self::new();
        registry.register(Arc::new(ReadFileTool));
        registry.register(Arc::new(WriteFileTool));
        registry.register(Arc::new(EditFileTool));
        registry.register(Arc::new(ListDirectoryTool));
        registry.register(Arc::new(RunCommandTool));
        registry.register(Arc::new(SearchFilesTool));
        registry.register(Arc::new(SaveMemoryTool));
        registry.register(Arc::new(ApplyPatchTool));
        registry.register(Arc::new(SkillViewTool));
        registry.register(Arc::new(SkillWriteTool));
        registry.register(Arc::new(SkillPatchTool));
        registry
    }

    pub fn register(&mut self, tool: Arc<dyn AgentTool>) {
        self.tools.insert(tool.name().to_string(), tool);
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn AgentTool>> {
        self.tools.get(name).cloned()
    }

    pub fn to_genai_tools(&self, mode: AgentMode) -> Vec<genai::chat::Tool> {
        let mut tools: Vec<Arc<dyn AgentTool>> = self
            .tools
            .values()
            .filter(|tool| mode == AgentMode::Build || tool.is_read_only())
            .cloned()
            .collect();
        // HashMap order varies per run; a stable list keeps the request prefix
        // identical so prompt caching and log comparisons stay meaningful.
        tools.sort_by_key(|tool| tool.name());
        tools
            .into_iter()
            .map(|tool| {
                genai::chat::Tool::new(tool.name())
                    .with_description(tool.description())
                    .with_schema(tool.parameters_schema())
            })
            .collect()
    }

    pub fn get_for_mode(&self, name: &str, mode: AgentMode) -> Result<Arc<dyn AgentTool>, String> {
        let tool = self
            .get(name)
            .ok_or_else(|| format!("Tool '{name}' not found."))?;

        if mode == AgentMode::Plan && !tool.is_read_only() {
            return Err(format!("Tool '{name}' is disabled in Plan Mode."));
        }

        Ok(tool)
    }
}

#[cfg(test)]
#[path = "../tests/core/tools.rs"]
mod tests;
