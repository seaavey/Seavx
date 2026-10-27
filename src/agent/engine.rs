use crate::agent::prompt::load_system_prompt;
use crate::agent::protocol::Message;
use crate::agent::security::WorkspaceSecurity;
use crate::agent::tools::ToolRegistry;
use genai::Client;
use genai::adapter::AdapterKind;
use genai::chat::{ChatMessage, ChatOptions, ChatRequest, ReasoningEffort, ToolResponse};
use genai::resolver::{AuthData, Endpoint};

use serde::{Deserialize, Serialize};
use std::path::Path;

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
            max_turns: 20,
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
        } else if let Ok(home) = std::env::var("HOME") {
            let global = std::path::PathBuf::from(home).join(".seavx/config.toml");
            if global.exists() { Some(global) } else { None }
        } else {
            None
        };

        if let Some(cfg_path) = path {
            if let Ok(content) = std::fs::read_to_string(cfg_path) {
                if let Ok(parsed) = toml::from_str::<AgentConfig>(&content) {
                    config = parsed;
                }
            }
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

    pub fn tool_registry(&mut self) -> &mut ToolRegistry {
        &mut self.registry
    }

    pub async fn execute_task(
        &self,
        task: &str,
        mut event_sink: impl FnMut(&str) + Send,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        let system_prompt = self
            .config
            .system_prompt
            .clone()
            .unwrap_or_else(|| load_system_prompt(self.security.root()));

        let mut chat_req = ChatRequest::default()
            .with_tools(self.registry.to_genai_tools())
            .append_message(ChatMessage::system(&system_prompt))
            .append_message(ChatMessage::user(task));

        let mut history = vec![Message::system(&system_prompt), Message::user(task)];

        tracing::info!(model = %self.config.model, "Starting Seavx agent task");
        event_sink("Starting agent reasoning...");

        let mut chat_options = ChatOptions::default()
            .with_capture_reasoning_content(true)
            .with_normalize_reasoning_content(true);

        if let Some(ref effort_str) = self.config.reasoning_effort {
            if let Some(effort) = parse_reasoning_effort(effort_str) {
                chat_options = chat_options.with_reasoning_effort(effort);
            }
        }

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

            // Emit reasoning thought stream if available from model
            if let Some(ref thoughts) = response.reasoning_content {
                let trimmed = thoughts.trim();
                if !trimmed.is_empty() {
                    event_sink(&format!("💭 Thought:\n{trimmed}"));
                }
            }

            let tool_calls = response.tool_calls();
            if tool_calls.is_empty() {
                let final_text = response.clone().into_first_text().unwrap_or_default();

                let return_text = if !final_text.trim().is_empty() {
                    final_text
                } else if let Some(ref thoughts) = response.reasoning_content {
                    format!("💭 Reasoning:\n{thoughts}")
                } else {
                    "Task completed.".to_string()
                };

                history.push(Message::assistant_text(&return_text));
                tracing::info!(turns = turn + 1, final_output = %return_text, "Agent completed task");

                // Auto-Skill Evaluation: If task was non-trivial (used tools across turns), distill into a skill
                if turn >= 2 {
                    self.auto_distill_skill(task, &history, &mut event_sink)
                        .await;
                }

                return Ok(return_text);
            }

            let assistant_msg = response.clone().into_assistant_message_for_tool_use();
            if let Some(msg) = assistant_msg {
                chat_req = chat_req.append_message(msg);
            }

            let owned_calls = response.into_tool_calls();

            let mut internal_calls = Vec::new();
            for call in &owned_calls {
                internal_calls.push(crate::agent::protocol::ToolCallRequest {
                    id: call.call_id.clone(),
                    name: call.fn_name.clone(),
                    arguments: call.fn_arguments.clone(),
                });
            }
            history.push(Message::assistant_tool_calls(internal_calls));

            for call in owned_calls {
                let tool_name = &call.fn_name;
                let args_summary = match &call.fn_arguments {
                    serde_json::Value::Object(map) => {
                        let parts: Vec<String> = map
                            .iter()
                            .map(|(k, v)| {
                                let val_str = match v {
                                    serde_json::Value::String(s) => {
                                        if s.len() > 60 {
                                            format!("\"{}...\"", &s[..57])
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

                let (result_str, is_err) = if let Some(tool) = self.registry.get(tool_name) {
                    let args = call.fn_arguments.clone();
                    match tool.execute(args, &self.security).await {
                        Ok(res) => (res, false),
                        Err(err) => (format!("Error executing {tool_name}: {err}"), true),
                    }
                } else {
                    (format!("Tool '{tool_name}' not found"), true)
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

                history.push(Message::tool_result(&call.call_id, &result_str, is_err));
                chat_req = chat_req.append_message(ChatMessage::from(ToolResponse::new(
                    call.call_id.clone(),
                    result_str,
                )));
            }
        }

        Err("Maximum turn budget reached without resolution".into())
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
            let timestamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();

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
        history: &[Message],
        event_sink: &mut (impl FnMut(&str) + Send),
    ) {
        event_sink("Evaluating workflow singularity for automatic skill synthesis...");
        let prompt = format!(
            "Analyze this completed task execution:\nTask: {}\nTotal steps: {}\n\n\
            Task Criteria for Skill Singularity:\n\
            1. Rate repeatability & engineering value as a confidence percentage (0-100%).\n\
            2. If confidence >= 75%, output a valid SKILL.md with frontmatter (name, description, confidence: <percentage>%) and concise steps.\n\
            3. If confidence < 75%, output ONLY: NONE",
            task,
            history.len()
        );

        let req = ChatRequest::from_user(prompt);
        if let Ok(res) = self.client.exec_chat(&self.config.model, req, None).await {
            let text = res.into_first_text().unwrap_or_default();
            let trimmed = text.trim();
            if trimmed.starts_with("---") && trimmed.contains("name:") {
                let mgr = crate::agent::skills::SkillManager::new(self.security.root());
                // Extract skill name
                let name = trimmed
                    .lines()
                    .find(|l| l.trim().starts_with("name:"))
                    .map(|l| {
                        l.trim()
                            .trim_start_matches("name:")
                            .trim()
                            .trim_matches('"')
                            .trim_matches('\'')
                    })
                    .unwrap_or("auto-workflow");

                if let Ok(saved_path) = mgr.save_skill(name, trimmed) {
                    event_sink(&format!(
                        "✨ High Singularity Skill Synthesized: {}",
                        saved_path.display()
                    ));
                    tracing::info!(skill = %name, "Automatically created high-confidence skill from workflow");
                }
            } else {
                tracing::info!("Skill synthesis skipped: confidence below singularity threshold");
            }
        }
    }
}
