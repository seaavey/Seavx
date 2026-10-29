use crate::agent::prompt::{PLAN_POLICY, load_system_prompt};
use crate::agent::security::WorkspaceSecurity;
use crate::agent::tools::ToolRegistry;
use genai::Client;
use genai::adapter::AdapterKind;
use genai::chat::{ChatMessage, ChatOptions, ChatRequest, ReasoningEffort, ToolResponse};
use genai::resolver::{AuthData, Endpoint};

use serde::{Deserialize, Serialize};
use std::path::Path;

/// Minimum evaluator confidence before an auto-distilled workflow is persisted as a skill.
pub const SKILL_CONFIDENCE_THRESHOLD: u8 = 75;

/// Runtime operating mode. Ephemeral: never persisted in `AgentConfig`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AgentMode {
    #[default]
    Build,
    Plan,
}

impl AgentMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::Build => "build",
            Self::Plan => "plan",
        }
    }
}

/// A completed task is only a skill candidate in Build Mode.
pub(crate) fn should_auto_distill(mode: AgentMode, step_count: usize) -> bool {
    mode == AgentMode::Build && step_count >= 2
}

/// Append the read-only planning policy after the base prompt has been resolved.
pub(crate) fn with_mode_policy(mut prompt: String, mode: AgentMode) -> String {
    if mode == AgentMode::Plan {
        prompt.push_str("\n\n");
        prompt.push_str(PLAN_POLICY);
    }
    prompt
}

/// Canonical reasoning-effort options, in menu order. Single source for the
/// TUI modal, the CLI prompts and `parse_reasoning_effort`.
pub const EFFORT_LEVELS: &[&str] = &["none", "minimal", "medium", "high", "highx", "max"];

/// Endpoint used when the config does not set `base_url`.
pub const DEFAULT_BASE_URL: &str = "https://api.openai.com/v1/";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentConfig {
    pub model: String,
    pub max_turns: usize,
    pub base_url: Option<String>,
    pub api_key: Option<String>,
    pub reasoning_effort: Option<String>,
    pub system_prompt: Option<String>,
}

impl Default for AgentConfig {
    fn default() -> Self {
        let model =
            std::env::var("SEAVX_MODEL").unwrap_or_else(|_| "openai/gpt-4o-mini".to_string());
        let base_url = std::env::var("SEAVX_BASE_URL").ok();
        let api_key = std::env::var("SEAVX_API_KEY")
            .or_else(|_| std::env::var("OPENAI_API_KEY"))
            .or_else(|_| std::env::var("ANTHROPIC_API_KEY"))
            .ok();
        let reasoning_effort = std::env::var("SEAVX_REASONING_EFFORT").ok();
        Self {
            model,
            max_turns: 1000,
            base_url,
            api_key,
            reasoning_effort,
            system_prompt: None,
        }
    }
}

pub fn parse_reasoning_effort(effort: &str) -> Option<ReasoningEffort> {
    match effort.to_lowercase().trim() {
        "none" | "zero" | "0" => Some(ReasoningEffort::Zero),
        "minimal" | "low" => Some(ReasoningEffort::Low),
        "medium" | "med" => Some(ReasoningEffort::Medium),
        "high" => Some(ReasoningEffort::High),
        "highx" | "xhigh" => Some(ReasoningEffort::XHigh),
        "max" => Some(ReasoningEffort::Max),
        _ => None,
    }
}

pub fn normalize_base_url(url: &str) -> (String, Vec<AdapterKind>) {
    let mut trimmed = url.trim().to_string();
    if !trimmed.starts_with("http://") && !trimmed.starts_with("https://") {
        trimmed = format!("https://{trimmed}");
    }

    // Ensure URL ends with /v1/
    if !trimmed.ends_with("/v1") && !trimmed.ends_with("/v1/") {
        if trimmed.ends_with('/') {
            trimmed.push_str("v1/");
        } else {
            trimmed.push_str("/v1/");
        }
    } else if !trimmed.ends_with('/') {
        trimmed.push('/');
    }

    // Infer supported protocols based on host domain
    let lower = trimmed.to_lowercase();
    let mut protocols = Vec::new();

    if lower.contains("anthropic") {
        protocols.push(AdapterKind::Anthropic);
        protocols.push(AdapterKind::OpenAI);
    } else if lower.contains("openai") {
        protocols.push(AdapterKind::OpenAI);
    } else {
        // Universal proxy or custom router: register both Anthropic and OpenAI
        protocols.push(AdapterKind::Anthropic);
        protocols.push(AdapterKind::OpenAI);
    }

    (trimmed, protocols)
}

impl AgentConfig {
    pub fn load(workspace_root: &Path) -> Self {
        let mut config = Self::default();

        // 1. Check workspace .seavx/config.toml
        let local_config = workspace_root.join(".seavx/config.toml");
        let path = if local_config.exists() {
            Some(local_config)
        } else {
            crate::util::global_config_path().filter(|global| global.exists())
        };

        if let Some(cfg_path) = path
            && let Ok(content) = std::fs::read_to_string(cfg_path)
            && let Ok(parsed) = toml::from_str::<AgentConfig>(&content)
        {
            config = parsed;
        }

        // Environment override takes highest precedence if provided
        if let Ok(m) = std::env::var("SEAVX_MODEL") {
            config.model = m;
        }

        if let Ok(b) = std::env::var("SEAVX_BASE_URL") {
            config.base_url = Some(b);
        }

        if let Ok(k) = std::env::var("SEAVX_API_KEY") {
            config.api_key = Some(k);
        }

        if let Ok(r) = std::env::var("SEAVX_REASONING_EFFORT") {
            config.reasoning_effort = Some(r);
        }

        config
    }

    /// Persist as pretty TOML under the workspace's `.seavx/config.toml`,
    /// creating the directory when missing. TUI call-sites ignore errors.
    pub fn save(&self, workspace_root: &Path) -> std::io::Result<()> {
        self.write_to(&workspace_root.join(".seavx/config.toml"))
    }

    /// Write the config as pretty TOML, creating parent directories if needed.
    pub fn write_to(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let toml_str =
            toml::to_string_pretty(self).map_err(|e| std::io::Error::other(e.to_string()))?;
        std::fs::write(path, toml_str)
    }
}

pub struct AgentRuntime {
    client: Client,
    registry: ToolRegistry,
    security: WorkspaceSecurity,
    config: AgentConfig,
}

impl AgentRuntime {
    pub fn new(config: AgentConfig, security: WorkspaceSecurity) -> Self {
        let mut builder = Client::builder();

        let auth_data = config
            .api_key
            .as_ref()
            .map(|k| AuthData::from_single(k.clone()));

        if let Some(ref base_url) = config.base_url {
            let (normalized, adapters) = normalize_base_url(base_url);
            tracing::info!(base_url = %normalized, "Configured custom API endpoint");

            let endpoint = Endpoint::from_owned(normalized);
            for adapter in adapters {
                if let Some(ref auth) = auth_data {
                    builder =
                        builder.append_provider_config(adapter, (endpoint.clone(), auth.clone()));
                } else {
                    builder = builder.append_provider_config(adapter, endpoint.clone());
                }
            }

            // Custom gateway mapping: ensure unknown models map to OpenAI adapter instead of Ollama
            builder = builder.with_model_mapper_fn(|model_iden: genai::ModelIden| {
                if model_iden.adapter_kind == AdapterKind::Ollama {
                    Ok(genai::ModelIden::new(
                        AdapterKind::OpenAI,
                        model_iden.model_name,
                    ))
                } else {
                    Ok(model_iden)
                }
            });
        } else if let Some(ref auth) = auth_data {
            builder = builder.append_provider_config(AdapterKind::OpenAI, auth.clone());
            builder = builder.append_provider_config(AdapterKind::Anthropic, auth.clone());
        }

        let client = builder
            .build()
            .unwrap_or_else(|_| Client::new().expect("Failed to initialize genai Client"));
        Self {
            client,
            registry: ToolRegistry::with_default_tools(),
            security,
            config,
        }
    }

    pub fn client(&self) -> &Client {
        &self.client
    }

    /// Options for every request. Effort is always sent explicitly: with no effort in the
    /// options genai strips a `-high`/`-low` suffix off the model name, and gateways reject
    /// the trimmed id (`model_not_allowed`).
    pub fn chat_options(&self) -> ChatOptions {
        let effort = self
            .config
            .reasoning_effort
            .as_deref()
            .and_then(parse_reasoning_effort)
            .or_else(|| ReasoningEffort::from_model_name(&self.config.model).0);

        let options = ChatOptions::default()
            .with_capture_reasoning_content(true)
            .with_normalize_reasoning_content(true);

        match effort {
            Some(effort) => options.with_reasoning_effort(effort),
            None => options,
        }
    }

    pub async fn execute_task(
        &self,
        task: &str,
        mode: AgentMode,
        mut event_sink: impl FnMut(&str) + Send,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        let system_prompt = self
            .config
            .system_prompt
            .clone()
            .unwrap_or_else(|| load_system_prompt(self.security.root()));

        let system_prompt = with_mode_policy(system_prompt, mode);

        let mut chat_req = ChatRequest::default()
            .with_tools(self.registry.to_genai_tools(mode))
            .append_message(ChatMessage::system(&system_prompt))
            .append_message(ChatMessage::user(task));

        tracing::info!(model = %self.config.model, "Starting Seavx agent task");

        let chat_options = self.chat_options();

        // Tool steps of this run, fed to the post-task skill evaluator: the model
        // cannot judge repeatability from the task text alone.
        let mut steps: Vec<String> = Vec::new();

        for turn in 0..self.config.max_turns {
            tracing::debug!(turn = turn, "Executing chat request");

            // Log full request payload to .seavx/request.log
            Self::log_request_payload(
                &self.config.model,
                turn,
                &chat_req,
                &chat_options,
                self.security.root(),
            );

            let response = self
                .client
                .exec_chat(&self.config.model, chat_req.clone(), Some(&chat_options))
                .await?;

            // Track real usage tokens from provider response
            if let Some(total) = response.usage.total_tokens {
                let prompt = response.usage.prompt_tokens.unwrap_or(0);
                event_sink(&format!("USAGE:{total}:{prompt}"));
            }

            // Surface the model's reasoning so the TUI can render it as a gray block
            if let Some(ref thoughts) = response.reasoning_content {
                let trimmed = thoughts.trim();
                if !trimmed.is_empty() {
                    event_sink(&format!("THINKING:{trimmed}"));
                }
            }

            let tool_calls = response.tool_calls();
            if tool_calls.is_empty() {
                let final_text = response.clone().into_first_text().unwrap_or_default();

                let return_text = if !final_text.trim().is_empty() {
                    final_text
                } else {
                    "Task completed.".to_string()
                };

                tracing::info!(turns = turn + 1, final_output = %return_text, "Agent completed task");

                // Auto-Skill Evaluation: a completed task that ran at least two tools is a
                // candidate workflow (a single call is not), regardless of how many turns it took.
                if should_auto_distill(mode, steps.len()) {
                    self.auto_distill_skill(task, &steps, &mut event_sink).await;
                }

                return Ok(return_text);
            }

            let assistant_msg = response.clone().into_assistant_message_for_tool_use();
            if let Some(msg) = assistant_msg {
                chat_req = chat_req.append_message(msg);
            }

            let owned_calls = response.into_tool_calls();

            for call in owned_calls {
                let tool_name = &call.fn_name;
                let args_summary = match &call.fn_arguments {
                    serde_json::Value::Object(map) => {
                        let parts: Vec<String> = map
                            .iter()
                            .map(|(k, v)| {
                                let val_str = match v {
                                    serde_json::Value::String(s) => {
                                        // char-safe: byte slicing panics on multibyte args
                                        if s.chars().count() > 60 {
                                            let head: String = s.chars().take(57).collect();
                                            format!("\"{head}...\"")
                                        } else {
                                            format!("\"{s}\"")
                                        }
                                    }
                                    other => other.to_string(),
                                };
                                format!("{k}={val_str}")
                            })
                            .collect();
                        parts.join(", ")
                    }
                    other => other.to_string(),
                };

                event_sink(&format!("Tool: {tool_name}({args_summary})"));
                tracing::info!(tool = %tool_name, args = %args_summary, id = %call.call_id, "Executing agent tool");
                steps.push(format!("{tool_name}({args_summary})"));

                let (result_str, is_err) = match self.registry.get_for_mode(tool_name, mode) {
                    Ok(tool) => {
                        let args = call.fn_arguments.clone();
                        match tool.execute(args, &self.security).await {
                            Ok(res) => (res, false),
                            Err(err) => (format!("Error executing {tool_name}: {err}"), true),
                        }
                    }
                    Err(err) => (format!("Error executing {tool_name}: {err}"), true),
                };

                tracing::info!(tool = %tool_name, is_error = is_err, output = %result_str, "Agent tool completed");

                // Emit preview snippet of tool output (first 3 lines or 150 chars)
                let preview = result_str.lines().take(3).collect::<Vec<_>>().join("\n");
                let truncated_preview = if result_str.lines().count() > 3 {
                    format!("{preview}\n  ... (truncated)")
                } else {
                    preview
                };

                event_sink(&format!("Output:\n{truncated_preview}"));

                chat_req = chat_req.append_message(ChatMessage::from(ToolResponse::new(
                    call.call_id.clone(),
                    result_str,
                )));
            }
        }

        // Final turn: force the model to provide a response without calling tools
        tracing::warn!("Turn budget exhausted, requesting final summary response");
        let final_prompt = "You have reached the maximum allowed tool iterations. Please summarize what you have accomplished, what is left, and provide your final response to the user now without calling any more tools.";
        let forced_req = chat_req
            .with_tools(Vec::<genai::chat::Tool>::new())
            .append_message(ChatMessage::user(final_prompt));

        let response = self
            .client
            .exec_chat(&self.config.model, forced_req, Some(&chat_options))
            .await?;

        let final_text = response.into_first_text().unwrap_or_default();
        if !final_text.trim().is_empty() {
            Ok(final_text)
        } else {
            Ok(
                "Turn budget reached. Work partially completed without a final response."
                    .to_string(),
            )
        }
    }

    fn log_request_payload(
        model: &str,
        turn: usize,
        chat_req: &ChatRequest,
        chat_options: &ChatOptions,
        workspace_root: &Path,
    ) {
        use std::io::Write;
        let log_dir = workspace_root.join(".seavx");
        let _ = std::fs::create_dir_all(&log_dir);
        let log_file = log_dir.join("request.log");

        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_file)
        {
            let timestamp = crate::util::epoch_secs();

            let effort = chat_options
                .reasoning_effort
                .as_ref()
                .map(|e| format!("{e:?}"))
                .unwrap_or_else(|| "default".to_string());

            let mut summary = format!(
                "--------------------------------------------------------------------------------\n\
                [{timestamp}] POST /v1/chat/completions (turn: {turn}, model: {model}, effort: {effort})\n"
            );

            // Log messages compactly without dumping raw giant schemas
            for (idx, msg) in chat_req.messages.iter().enumerate() {
                let role = format!("{:?}", msg.role);
                let content_summary = match msg.content.first_text() {
                    Some(txt) => {
                        let lines: Vec<&str> = txt.lines().collect();
                        if lines.len() > 3 {
                            format!("{}... ({} lines)", lines[0].trim(), lines.len())
                        } else {
                            txt.trim().to_string()
                        }
                    }
                    None => {
                        let calls = msg.content.tool_calls();
                        if !calls.is_empty() {
                            let names: Vec<String> =
                                calls.iter().map(|c| c.fn_name.clone()).collect();
                            format!("[ToolCalls: {}]", names.join(", "))
                        } else {
                            "[Multipart/Binary/ToolResponse]".to_string()
                        }
                    }
                };
                summary.push_str(&format!("  msg[{}][{role}]: {content_summary}\n", idx));
            }

            // List tool names concisely instead of thousands of JSON schema lines
            if let Some(ref tools) = chat_req.tools {
                let tool_names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
                summary.push_str(&format!(
                    "  tools({}): {}\n",
                    tools.len(),
                    tool_names.join(", ")
                ));
            }

            summary.push('\n');
            let _ = file.write_all(summary.as_bytes());
        }
    }

    async fn auto_distill_skill(
        &self,
        task: &str,
        steps: &[String],
        event_sink: &mut (impl FnMut(&str) + Send),
    ) {
        event_sink("System: evaluating workflow for automatic skill synthesis...");
        let trace = steps
            .iter()
            .enumerate()
            .map(|(i, s)| format!("{}. {s}", i + 1))
            .collect::<Vec<_>>()
            .join("\n");
        let prompt = format!(
            "Analyze this completed task execution:\nTask: {}\nSteps:\n{}\n\n\
            Task Criteria for Skill Singularity:\n\
            1. Rate repeatability & engineering value as a confidence percentage (0-100%).\n\
            2. If confidence >= {SKILL_CONFIDENCE_THRESHOLD}%, output a valid SKILL.md with frontmatter (name, description, confidence: <percentage>%) and concise steps.\n\
            3. If confidence < {SKILL_CONFIDENCE_THRESHOLD}%, output ONLY: NONE",
            task, trace
        );

        let req = ChatRequest::from_user(prompt);
        let options = self.chat_options();
        let res = match self
            .client
            .exec_chat(&self.config.model, req, Some(&options))
            .await
        {
            Ok(res) => res,
            Err(err) => {
                tracing::warn!(error = %err, "Skill synthesis skipped: evaluator request failed");
                event_sink(&format!("System: skill evaluator request failed: {err}"));
                return;
            }
        };

        let text = res.into_first_text().unwrap_or_default();
        let trimmed = text.trim();
        if !(trimmed.starts_with("---") && trimmed.contains("name:")) {
            tracing::info!("Skill synthesis skipped: evaluator returned no skill");
            event_sink("System: no repeatable workflow found, skill not saved");
            return;
        }

        let confidence = parse_confidence(trimmed);
        if !matches!(confidence, Some(c) if c >= SKILL_CONFIDENCE_THRESHOLD) {
            tracing::info!(
                ?confidence,
                "Skill synthesis skipped: confidence below threshold"
            );
            event_sink("System: workflow rated below singularity threshold, skill not saved");
            return;
        }

        let name = crate::util::frontmatter_value(trimmed, "name")
            .unwrap_or_else(|| "auto-workflow".to_string());

        let mgr = crate::agent::skills::SkillManager::new(self.security.root());
        match mgr.save_skill(&name, trimmed) {
            Ok(saved_path) => {
                tracing::info!(skill = %name, "Automatically created high-confidence skill from workflow");
                event_sink(&format!(
                    "System: skill synthesized -> {}",
                    saved_path.display()
                ));
            }
            Err(err) => {
                tracing::warn!(error = %err, "Skill synthesis failed: cannot persist skill");
                event_sink(&format!("System: skill synthesis failed: {err}"));
            }
        }
    }
}

/// Confidence percentage declared in a synthesized skill's frontmatter.
pub(crate) fn parse_confidence(skill_md: &str) -> Option<u8> {
    skill_md.lines().find_map(|line| {
        line.trim()
            .strip_prefix("confidence:")?
            .trim()
            .trim_matches('"')
            .trim_end_matches('%')
            .trim()
            .parse()
            .ok()
    })
}

#[cfg(test)]
mod mode_policy_tests {
    use super::*;
    use crate::agent::prompt::PLAN_POLICY;

    #[test]
    fn build_mode_leaves_the_prompt_unchanged() {
        let prompt = "base prompt".to_string();
        assert_eq!(with_mode_policy(prompt.clone(), AgentMode::Build), prompt);
    }

    #[test]
    fn plan_mode_appends_the_policy() {
        let prompt = with_mode_policy("base prompt".to_string(), AgentMode::Plan);
        assert!(prompt.starts_with("base prompt"));
        assert!(prompt.contains("# PLAN MODE ACTIVE"));
        assert!(prompt.contains(PLAN_POLICY));
    }
}
