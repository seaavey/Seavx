use crate::agent::engine::AgentMode;
use crate::agent::security::WorkspaceSecurity;
use async_trait::async_trait;
use schemars::{JsonSchema, schema_for};
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashMap;
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

    async fn execute(&self, args: Value, security: &WorkspaceSecurity) -> Result<String, String>;
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

    fn description(&self) -> &'static str {
        "Read file contents within workspace, with optional line offset (1-based) and limit"
    }

    fn is_read_only(&self) -> bool {
        true
    }

    fn parameters_schema(&self) -> Value {
        serde_json::to_value(schema_for!(ReadFileArgs)).unwrap_or_default()
    }

    async fn execute(&self, args: Value, security: &WorkspaceSecurity) -> Result<String, String> {
        let parsed: ReadFileArgs =
            serde_json::from_value(args).map_err(|e| format!("Invalid args: {e}"))?;
        let path = security.validate_path(&parsed.path)?;
        let content = tokio::fs::read_to_string(path)
            .await
            .map_err(|e| format!("Failed to read file: {e}"))?;

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
        serde_json::to_value(schema_for!(WriteFileArgs)).unwrap_or_default()
    }

    async fn execute(&self, args: Value, security: &WorkspaceSecurity) -> Result<String, String> {
        let parsed: WriteFileArgs =
            serde_json::from_value(args).map_err(|e| format!("Invalid args: {e}"))?;
        let path = security.validate_path(&parsed.path)?;
        if let Some(parent) = path.parent() {
            let _ = tokio::fs::create_dir_all(parent).await;
        }
        tokio::fs::write(&path, parsed.content)
            .await
            .map_err(|e| format!("Failed to write file: {e}"))?;
        Ok(format!("Successfully wrote {}", parsed.path))
    }
}

// 3. edit_file
#[derive(Deserialize, JsonSchema)]
struct EditFileArgs {
    path: String,
    old_string: String,
    new_string: String,
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
        serde_json::to_value(schema_for!(EditFileArgs)).unwrap_or_default()
    }

    async fn execute(&self, args: Value, security: &WorkspaceSecurity) -> Result<String, String> {
        let parsed: EditFileArgs =
            serde_json::from_value(args).map_err(|e| format!("Invalid args: {e}"))?;
        let path = security.validate_path(&parsed.path)?;
        let content = tokio::fs::read_to_string(&path)
            .await
            .map_err(|e| format!("Failed to read file for editing: {e}"))?;

        if !content.contains(&parsed.old_string) {
            return Err("Target old_string not found in file".to_string());
        }

        let updated = content.replacen(&parsed.old_string, &parsed.new_string, 1);
        tokio::fs::write(&path, updated)
            .await
            .map_err(|e| format!("Failed to save edit: {e}"))?;
        Ok(format!("Successfully updated {}", parsed.path))
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

    fn description(&self) -> &'static str {
        "List files and subdirectories in a workspace path"
    }

    fn is_read_only(&self) -> bool {
        true
    }

    fn parameters_schema(&self) -> Value {
        serde_json::to_value(schema_for!(ListDirArgs)).unwrap_or_default()
    }

    async fn execute(&self, args: Value, security: &WorkspaceSecurity) -> Result<String, String> {
        let parsed: ListDirArgs =
            serde_json::from_value(args).map_err(|e| format!("Invalid args: {e}"))?;
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

    fn description(&self) -> &'static str {
        "Run shell command safely in workspace root with 30s timeout"
    }

    fn parameters_schema(&self) -> Value {
        serde_json::to_value(schema_for!(RunCommandArgs)).unwrap_or_default()
    }

    async fn execute(&self, args: Value, security: &WorkspaceSecurity) -> Result<String, String> {
        let parsed: RunCommandArgs =
            serde_json::from_value(args).map_err(|e| format!("Invalid args: {e}"))?;

        let blocked = ["rm -rf /", ":(){ :|:& };:"];
        for b in &blocked {
            if parsed.command.contains(b) {
                return Err("Command refused by security policy".to_string());
            }
        }

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

        let cmd = shell
            .arg(&parsed.command)
            .current_dir(security.root())
            .output();

        let output = tokio::time::timeout(std::time::Duration::from_secs(30), cmd)
            .await
            .map_err(|_| "Command execution timed out (30s limit)".to_string())?
            .map_err(|e| format!("Process execution failed: {e}"))?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let status = output.status.code().unwrap_or(-1);

        Ok(format!(
            "exit: {status}\nstdout:\n{stdout}\nstderr:\n{stderr}"
        ))
    }
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

    fn description(&self) -> &'static str {
        "Search file contents (target='content', default) or match filenames (target='files') across the workspace"
    }

    fn is_read_only(&self) -> bool {
        true
    }

    fn parameters_schema(&self) -> Value {
        serde_json::to_value(schema_for!(SearchFilesArgs)).unwrap_or_default()
    }

    async fn execute(&self, args: Value, security: &WorkspaceSecurity) -> Result<String, String> {
        let parsed: SearchFilesArgs =
            serde_json::from_value(args).map_err(|e| format!("Invalid args: {e}"))?;
        let target_mode = parsed.target.unwrap_or_else(|| "content".to_string());
        let search_path = parsed.path.unwrap_or_else(|| ".".to_string());
        let root = security.validate_path(&search_path)?;

        let pattern = parsed.pattern.clone();
        let results = tokio::task::spawn_blocking(move || -> Result<Vec<String>, String> {
            let mut matches = Vec::new();
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

                    // Skip hidden git and target build artifacts
                    if file_name == ".git" || file_name == "target" || file_name == "node_modules" {
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

            Ok(matches)
        })
        .await
        .map_err(|e| format!("Search task failed: {e}"))??;

        if results.is_empty() {
            Ok(format!("No matches found for pattern '{}'", parsed.pattern))
        } else {
            let count = results.len();
            let mut output = results.join("\n");
            if count >= 100 {
                output.push_str("\n[Truncated to 100 results]");
            }
            Ok(output)
        }
    }
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

    fn description(&self) -> &'static str {
        "Save a durable lesson/preference with confidence score (0-100%). Rejects if confidence < 70%."
    }

    fn parameters_schema(&self) -> Value {
        serde_json::to_value(schema_for!(SaveMemoryArgs)).unwrap_or_default()
    }

    async fn execute(&self, args: Value, security: &WorkspaceSecurity) -> Result<String, String> {
        let parsed: SaveMemoryArgs =
            serde_json::from_value(args).map_err(|e| format!("Invalid args: {e}"))?;

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

        let is_debug = std::env::var("SEAVX_DEBUG")
            .map(|v| v == "1" || v == "true")
            .unwrap_or(false)
            || cfg!(debug_assertions);

        let target = parsed.target.unwrap_or_else(|| "project".to_string());
        let target_file = if target == "user" {
            if is_debug {
                // In debug mode, save USER.md locally inside repo .seavx/USER.md
                let seavx_dir = security.root().join(".seavx");
                let _ = tokio::fs::create_dir_all(&seavx_dir).await;
                seavx_dir.join("USER.md")
            } else {
                let home =
                    crate::home::dir().ok_or_else(|| "home directory not set".to_string())?;
                let dir = home.join(".seavx");
                let _ = tokio::fs::create_dir_all(&dir).await;
                dir.join("USER.md")
            }
        } else if is_debug {
            // In debug mode, save project memory under .seavx/MEMORY.md
            let seavx_dir = security.root().join(".seavx");
            let _ = tokio::fs::create_dir_all(&seavx_dir).await;
            seavx_dir.join("MEMORY.md")
        } else {
            security.root().join("MEMORY.md")
        };

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

        tokio::fs::write(&target_file, updated)
            .await
            .map_err(|e| format!("Failed to write memory: {e}"))?;

        Ok(format!(
            "Saved [{confidence}%] to {}: {}",
            target_file.display(),
            lesson
        ))
    }
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

    fn description(&self) -> &'static str {
        "Read full instructions of an available skill by name"
    }

    fn is_read_only(&self) -> bool {
        true
    }

    fn parameters_schema(&self) -> Value {
        serde_json::to_value(schema_for!(SkillViewArgs)).unwrap_or_default()
    }

    async fn execute(&self, args: Value, security: &WorkspaceSecurity) -> Result<String, String> {
        let parsed: SkillViewArgs =
            serde_json::from_value(args).map_err(|e| format!("Invalid args: {e}"))?;
        let path = find_skill_file(security, &parsed.name)?;
        tokio::fs::read_to_string(path)
            .await
            .map_err(|e| format!("Failed to read skill: {e}"))
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

    fn description(&self) -> &'static str {
        "Create a new skill or overwrite an existing skill's SKILL.md with full content"
    }

    fn parameters_schema(&self) -> Value {
        serde_json::to_value(schema_for!(SkillWriteArgs)).unwrap_or_default()
    }

    async fn execute(&self, args: Value, security: &WorkspaceSecurity) -> Result<String, String> {
        let parsed: SkillWriteArgs =
            serde_json::from_value(args).map_err(|e| format!("Invalid args: {e}"))?;
        if parsed.content.trim().is_empty() {
            return Err("Skill content cannot be empty".to_string());
        }

        // Overwrite in place when the skill already exists (could live in a shared skills root).
        if let Ok(path) = find_skill_file(security, &parsed.name) {
            tokio::fs::write(&path, parsed.content)
                .await
                .map_err(|e| format!("Failed to write skill: {e}"))?;
            return Ok(format!("Successfully updated {}", path.display()));
        }

        let path = crate::agent::skills::SkillManager::new(security.root())
            .save_skill(&parsed.name, &parsed.content)?;
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

    fn description(&self) -> &'static str {
        "Replace an exact snippet with new text inside an existing skill's SKILL.md"
    }

    fn parameters_schema(&self) -> Value {
        serde_json::to_value(schema_for!(SkillPatchArgs)).unwrap_or_default()
    }

    async fn execute(&self, args: Value, security: &WorkspaceSecurity) -> Result<String, String> {
        let parsed: SkillPatchArgs =
            serde_json::from_value(args).map_err(|e| format!("Invalid args: {e}"))?;
        let path = find_skill_file(security, &parsed.name)?;
        let content = tokio::fs::read_to_string(&path)
            .await
            .map_err(|e| format!("Failed to read skill: {e}"))?;

        if !content.contains(&parsed.old_string) {
            return Err("Target old_string not found in skill".to_string());
        }

        let updated = content.replacen(&parsed.old_string, &parsed.new_string, 1);
        tokio::fs::write(&path, updated)
            .await
            .map_err(|e| format!("Failed to save skill patch: {e}"))?;
        Ok(format!("Successfully patched {}", parsed.name))
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
        self.tools
            .values()
            .filter(|tool| mode == AgentMode::Build || tool.is_read_only())
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
