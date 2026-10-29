use crate::agent::backend::{ChatBackend, GenaiBackend, StreamDelta};
use crate::agent::permissions::{
    self, ActionKind, Answer, Confirmer, PermissionMode, PermissionRequest, Verdict,
};
use crate::agent::prompt::{PLAN_POLICY, load_system_prompt};
use crate::agent::security::{self, WorkspaceSecurity};
use crate::agent::tools::ToolRegistry;
use genai::Client;
use genai::adapter::AdapterKind;
use genai::chat::{ChatMessage, ChatOptions, ChatRequest, ReasoningEffort, ToolResponse};
use genai::resolver::{AuthData, Endpoint};

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

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

/// Turn budget a new config starts with. An existing file keeps its own value.
pub const DEFAULT_MAX_TURNS: usize = 100;

/// Endpoint used when the config does not set `base_url`.
pub const DEFAULT_BASE_URL: &str = "https://api.openai.com/v1/";

/// Config keys that can appear in a config file, in display order.
pub const CONFIG_KEYS: [&str; 9] = [
    "model",
    "base_url",
    "api_key",
    "reasoning_effort",
    "stream",
    "max_turns",
    "system_prompt",
    "tool_permissions",
    "permitted_actions",
];

/// Environment overrides that win over the file, paired with the key they set.
const ENV_OVERRIDES: [(&str, &str); 4] = [
    ("model", "SEAVX_MODEL"),
    ("base_url", "SEAVX_BASE_URL"),
    ("api_key", "SEAVX_API_KEY"),
    ("reasoning_effort", "SEAVX_REASONING_EFFORT"),
];

/// Where an effective configuration value came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigOrigin {
    Default,
    Workspace,
    Global,
    Env(&'static str),
}

/// Paths and provenance for the configuration loaded alongside it.
#[derive(Debug, Clone)]
pub struct ConfigReport {
    pub workspace_path: PathBuf,
    pub global_path: Option<PathBuf>,
    pub active_path: Option<PathBuf>,
    pub active_origin: ConfigOrigin,
    pub parse_error: Option<String>,
    pub origins: BTreeMap<&'static str, ConfigOrigin>,
}

impl ConfigReport {
    pub fn origin(&self, key: &str) -> ConfigOrigin {
        self.origins
            .get(key)
            .copied()
            .unwrap_or(ConfigOrigin::Default)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentConfig {
    pub model: String,
    pub max_turns: usize,
    pub base_url: Option<String>,
    pub api_key: Option<String>,
    pub reasoning_effort: Option<String>,
    pub system_prompt: Option<String>,
    /// `deny` | `ask` | `allow`, kept raw so an invalid value is reported with
    /// the file that holds it instead of being coerced to a silent default.
    #[serde(default = "default_tool_permissions")]
    pub tool_permissions: String,
    /// Action kinds lifted from `ask` to `allow`; never `global-write`.
    #[serde(default)]
    pub permitted_actions: Vec<String>,
    /// Stream the answer instead of waiting for the whole response.
    #[serde(default = "default_stream")]
    pub stream: bool,
}

fn default_tool_permissions() -> String {
    "ask".to_string()
}

fn default_stream() -> bool {
    true
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self::from_env(|name| std::env::var(name).ok())
    }
}

impl AgentConfig {
    /// Build the built-in defaults from an injected environment, so tests
    /// never depend on the caller's real variables.
    fn from_env(env: impl Fn(&str) -> Option<String>) -> Self {
        let model = env("SEAVX_MODEL").unwrap_or_else(|| "openai/gpt-4o-mini".to_string());
        let base_url = env("SEAVX_BASE_URL");
        let api_key = env("SEAVX_API_KEY")
            .or_else(|| env("OPENAI_API_KEY"))
            .or_else(|| env("ANTHROPIC_API_KEY"));
        let reasoning_effort = env("SEAVX_REASONING_EFFORT");
        Self {
            model,
            max_turns: DEFAULT_MAX_TURNS,
            base_url,
            api_key,
            reasoning_effort,
            system_prompt: None,
            tool_permissions: default_tool_permissions(),
            permitted_actions: Vec::new(),
            stream: default_stream(),
        }
    }
}

/// The context window a model is assumed to have.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContextLimit {
    pub tokens: i32,
    /// `true` when the id was not recognised and the conservative default was
    /// used, so the footer and `doctor` can say so.
    pub assumed: bool,
}

/// Model families known to have a 1M window.
const MILLION_TOKEN_FAMILIES: [&str; 3] = ["claude", "gemini", "nemotron"];
/// Model families known to have a 128k window.
const HUNDRED_TWENTY_EIGHT_K_FAMILIES: [&str; 6] =
    ["deepseek", "glm", "gpt-4o", "gpt-4.1", "o3", "o4"];
/// Anything unrecognised gets this instead of the largest guess.
pub const ASSUMED_CONTEXT_LIMIT: i32 = 64_000;

/// One table, shared by the footer, the context budget and `doctor`. An
/// unrecognised id is never assumed to be 128k.
pub fn model_context_limit(model: &str) -> ContextLimit {
    let id = model.to_lowercase();
    if MILLION_TOKEN_FAMILIES
        .iter()
        .any(|family| id.contains(family))
    {
        return ContextLimit {
            tokens: 1_000_000,
            assumed: false,
        };
    }
    if HUNDRED_TWENTY_EIGHT_K_FAMILIES
        .iter()
        .any(|family| id.contains(family))
    {
        return ContextLimit {
            tokens: 128_000,
            assumed: false,
        };
    }
    ContextLimit {
        tokens: ASSUMED_CONTEXT_LIMIT,
        assumed: true,
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
        Self::load_with_report(workspace_root).0
    }

    /// Load the effective config together with the paths and provenance that
    /// `/config` and `seavx doctor` report.
    pub fn load_with_report(workspace_root: &Path) -> (Self, ConfigReport) {
        Self::load_with_report_env(workspace_root, |name| std::env::var(name).ok())
    }

    /// Testable core of [`Self::load_with_report`]. Mirrors `load` exactly:
    /// a workspace file shadows the global file entirely, and `SEAVX_*`
    /// overrides win over both.
    pub(crate) fn load_with_report_env(
        workspace_root: &Path,
        env: impl Fn(&str) -> Option<String>,
    ) -> (Self, ConfigReport) {
        Self::load_with_report_home(workspace_root, crate::home::dir().as_deref(), env)
    }

    /// Like [`Self::load_with_report_env`], but the home directory is injected
    /// so callers such as `doctor` can keep tests away from the real home.
    pub(crate) fn load_with_report_home(
        workspace_root: &Path,
        home: Option<&Path>,
        env: impl Fn(&str) -> Option<String>,
    ) -> (Self, ConfigReport) {
        let workspace_path = workspace_root.join(".seavx/config.toml");
        let global_path = home.map(|home| home.join(".seavx/config.toml"));

        // A replace interrupted by a crash is resolved before the file is read.
        for path in [Some(&workspace_path), global_path.as_ref()]
            .into_iter()
            .flatten()
        {
            if let Err(error) = crate::cli::config::recover_config(path) {
                tracing::warn!(%error, "config recovery");
            }
        }

        let workspace_exists = workspace_path.exists();
        let global_exists = global_path.as_ref().is_some_and(|path| path.exists());
        let active_path = if workspace_exists {
            Some(workspace_path.clone())
        } else if global_exists {
            global_path.clone()
        } else {
            None
        };
        let active_origin = if workspace_exists {
            ConfigOrigin::Workspace
        } else if active_path.is_some() {
            ConfigOrigin::Global
        } else {
            ConfigOrigin::Default
        };

        let mut config = Self::from_env(&env);
        let mut origins = BTreeMap::new();
        for (key, variable) in ENV_OVERRIDES {
            if env(variable).is_some() {
                origins.insert(key, ConfigOrigin::Env(variable));
            }
        }

        let mut parse_error = None;

        if let Some(path) = &active_path {
            match std::fs::read_to_string(path) {
                Ok(content) => match toml::from_str::<Self>(&content) {
                    Ok(parsed) => {
                        if let Ok(value) = content.parse::<toml::Value>()
                            && let Some(table) = value.as_table()
                        {
                            for key in CONFIG_KEYS {
                                if table.contains_key(key) {
                                    origins.insert(key, active_origin);
                                }
                            }
                        }
                        // The file replaces the whole default config, matching
                        // `load`: keys it omits do not inherit fallback envs.
                        config = parsed;
                    }
                    Err(error) => parse_error = Some(first_line(&error.to_string())),
                },
                Err(error) => parse_error = Some(first_line(&error.to_string())),
            }
        }

        apply_env_overrides(&mut config, &mut origins, &env);

        (
            config,
            ConfigReport {
                workspace_path,
                global_path,
                active_path,
                active_origin,
                parse_error,
                origins,
            },
        )
    }

    /// Persist under the workspace's `.seavx/config.toml`. Call-sites that
    /// cannot report an error ignore it.
    pub fn save(&self, workspace_root: &Path) -> std::io::Result<()> {
        self.write_to(&workspace_root.join(".seavx/config.toml"))
    }

    /// Write through the one validated config writer in `config.rs`.
    pub fn write_to(&self, path: &Path) -> std::io::Result<()> {
        crate::cli::config::write_agent_config(path, self).map_err(std::io::Error::other)
    }
}

/// Only the first line of a parser error is safe to show: later lines echo the
/// offending source, which may be an `api_key`.
fn first_line(text: &str) -> String {
    text.lines().next().unwrap_or_default().trim().to_string()
}

fn apply_env_overrides(
    config: &mut AgentConfig,
    origins: &mut BTreeMap<&'static str, ConfigOrigin>,
    env: impl Fn(&str) -> Option<String>,
) {
    for (key, variable) in ENV_OVERRIDES {
        let Some(value) = env(variable) else {
            continue;
        };
        match key {
            "model" => config.model = value,
            "base_url" => config.base_url = Some(value),
            "api_key" => config.api_key = Some(value),
            "reasoning_effort" => config.reasoning_effort = Some(value),
            _ => {}
        }
        origins.insert(key, ConfigOrigin::Env(variable));
    }
}

/// What a provider failure is allowed to buy: at most one retry per user turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetryTrigger {
    /// The provider said the request is too large for the model.
    CapacityError,
    /// The provider refused the replayed tool-call structure.
    ReplayRejection,
    /// The stream produced no chunk at all: the endpoint may not stream.
    StreamUnavailable,
}

/// One budget per user turn, never per tool call.
#[derive(Debug, Default)]
pub struct RetryBudget {
    used: bool,
    output_seen: bool,
}

impl RetryBudget {
    /// Any received output closes the budget: partial output is persisted, never
    /// resent. Phase 5's streaming path is the caller that sets this.
    pub fn note_output(&mut self) {
        self.output_seen = true;
    }

    /// `true` when this trigger may be retried; a second call is always refused,
    /// so two triggers can never stack.
    pub fn spend(&mut self, _trigger: RetryTrigger) -> bool {
        if self.used || self.output_seen {
            return false;
        }
        self.used = true;
        true
    }
}

/// Phrases that identify a capacity error. Anything else is not retried: an
/// unidentified failure may be a bad model, a bad key or a bug.
const CAPACITY_MARKERS: [&str; 6] = [
    "maximum context length",
    "context length",
    "context_length_exceeded",
    "token limit",
    "prompt is too long",
    "prompt too long",
];

/// Phrases that identify a replay rejection: the provider refused the structure
/// of the replayed tool calls.
const REPLAY_MARKERS: [&str; 4] = ["tool_call_id", "tool_calls", "tool result", "tool_use"];

fn message_of(error: &(dyn std::error::Error + 'static)) -> String {
    let mut text = error.to_string();
    let mut source = error.source();
    while let Some(error) = source {
        text.push(' ');
        text.push_str(&error.to_string());
        source = error.source();
    }
    text.to_lowercase()
}

/// The one classifier: only a message that names the condition counts.
pub fn classify_failure(error: &(dyn std::error::Error + 'static)) -> Option<RetryTrigger> {
    let text = message_of(error);
    if CAPACITY_MARKERS.iter().any(|marker| text.contains(marker)) {
        return Some(RetryTrigger::CapacityError);
    }
    if REPLAY_MARKERS.iter().any(|marker| text.contains(marker)) {
        return Some(RetryTrigger::ReplayRejection);
    }
    None
}

/// The replay-rejection retry: keep the newest half of the turns, cut at a user
/// row, so a tool-call pair is never split.
fn halve_turns(request: &ChatRequest) -> ChatRequest {
    let messages = &request.messages;
    if messages.len() < 4 {
        return request.clone();
    }

    let body = &messages[1..messages.len() - 1];
    let mut starts = vec![0];
    for (index, message) in body.iter().enumerate().skip(1) {
        if message.role == genai::chat::ChatRole::User {
            starts.push(index);
        }
    }
    let keep_from = starts[starts.len().div_ceil(2)];

    let mut rebuilt = ChatRequest::default();
    if let Some(tools) = &request.tools {
        rebuilt = rebuilt.with_tools(tools.clone());
    }
    rebuilt = rebuilt.append_message(messages[0].clone());
    for message in &body[keep_from..] {
        rebuilt = rebuilt.append_message(message.clone());
    }
    rebuilt.append_message(messages[messages.len() - 1].clone())
}

pub struct AgentRuntime {
    client: Client,
    backend: Box<dyn ChatBackend>,
    summarizer: Option<std::sync::Arc<dyn crate::tui::compact::Summarizer>>,
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
        let backend = Box::new(GenaiBackend::new(client.clone()));
        Self {
            client,
            backend,
            summarizer: None,
            registry: ToolRegistry::with_default_tools(),
            security,
            config,
        }
    }

    /// Swap the provider seam; only tests use it, production always talks genai.
    #[cfg(test)]
    pub(crate) fn with_backend(
        config: AgentConfig,
        security: WorkspaceSecurity,
        backend: Box<dyn ChatBackend>,
    ) -> Self {
        let mut runtime = Self::new(config, security);
        runtime.backend = backend;
        runtime
    }

    /// Swap the summarizer the compaction step uses, so no test reaches the
    /// network.
    #[cfg(test)]
    pub(crate) fn with_summarizer(
        mut self,
        summarizer: std::sync::Arc<dyn crate::tui::compact::Summarizer>,
    ) -> Self {
        self.summarizer = Some(summarizer);
        self
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
        history: Vec<ChatMessage>,
        confirm: &dyn Confirmer,
        mut event_sink: impl FnMut(&str) + Send,
        mut on_message: impl FnMut(&ChatMessage, crate::session::MessageStatus),
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        let summarizer: std::sync::Arc<dyn crate::tui::compact::Summarizer> = match &self.summarizer
        {
            Some(summarizer) => summarizer.clone(),
            None => std::sync::Arc::new(crate::tui::compact::ModelSummarizer::from_config(
                &self.config,
            )),
        };
        let retry_history = history.clone();
        let mut chat_req = self
            .prepare_request(history, task, mode, &*summarizer, &mut event_sink)
            .await;
        let mut retry = RetryBudget::default();

        tracing::info!(model = %self.config.model, "Starting Seavx agent task");

        let chat_options = self.chat_options();

        // Tool steps of this run, fed to the post-task skill evaluator: the model
        // cannot judge repeatability from the task text alone.
        let mut steps: Vec<String> = Vec::new();
        // Repeated-action detection: the same call or the same error twice ends
        // the loop instead of burning the turn budget.
        let mut last_action: Option<String> = None;
        let mut last_failure: Option<String> = None;

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

            // First attempt: a stream when the config allows it, one plain
            // request otherwise. Both share the turn's single retry.
            let mut got_delta = false;
            let mut received = String::new();
            let first = if self.config.stream {
                let mut on_delta = |delta: StreamDelta| {
                    got_delta = true;
                    retry.note_output();
                    match delta {
                        StreamDelta::Token(token) => {
                            received.push_str(&token);
                            event_sink(&format!("TOKEN:{token}"));
                        }
                        StreamDelta::Thinking(thoughts) => {
                            event_sink(&format!("THINKING:{thoughts}"));
                        }
                    }
                };
                self.backend
                    .stream(
                        &self.config.model,
                        chat_req.clone(),
                        Some(&chat_options),
                        &mut on_delta,
                    )
                    .await
            } else {
                self.backend
                    .send(&self.config.model, chat_req.clone(), Some(&chat_options))
                    .await
            };

            let response = match first {
                Ok(response) => response,
                Err(error) if got_delta => {
                    // Output already left the sink: persisted, never resent.
                    if !received.trim().is_empty() {
                        on_message(
                            &ChatMessage::assistant(&received),
                            crate::session::MessageStatus::Partial,
                        );
                    }
                    return Err(error);
                }
                Err(error) => {
                    // One retry per turn, on the first identified trigger only.
                    let trigger = classify_failure(error.as_ref());
                    let spent = match trigger {
                        Some(trigger) => retry.spend(trigger),
                        // A stream that produced nothing may still answer
                        // unstreamed; that attempt is the same single retry.
                        None => self.config.stream && retry.spend(RetryTrigger::StreamUnavailable),
                    };
                    if !spent {
                        return Err(error);
                    }
                    if let Some(trigger) = trigger {
                        chat_req = match trigger {
                            RetryTrigger::ReplayRejection => halve_turns(&chat_req),
                            RetryTrigger::CapacityError => {
                                self.aggressive_request(&retry_history, task, mode, &*summarizer)
                                    .await
                            }
                            RetryTrigger::StreamUnavailable => chat_req.clone(),
                        };
                    }
                    // A failure here is the turn's failure: it never buys a
                    // second retry.
                    self.backend
                        .send(&self.config.model, chat_req.clone(), Some(&chat_options))
                        .await?
                }
            };

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

                // The filler is not model output: only real text is reported.
                if !final_text.trim().is_empty() {
                    on_message(
                        &ChatMessage::assistant(&final_text),
                        crate::session::MessageStatus::Complete,
                    );
                }

                let return_text = if !final_text.trim().is_empty() {
                    final_text
                } else {
                    "Task completed.".to_string()
                };

                tracing::info!(
                    turns = turn + 1,
                    final_output = %security::redact(&return_text),
                    "Agent completed task"
                );

                // Auto-Skill Evaluation: a completed task that ran at least two tools is a
                // candidate workflow (a single call is not), regardless of how many turns it took.
                if should_auto_distill(mode, steps.len()) {
                    self.auto_distill_skill(task, &steps, &mut event_sink).await;
                }

                return Ok(return_text);
            }

            let assistant_msg = response.clone().into_assistant_message_for_tool_use();
            if let Some(msg) = assistant_msg {
                on_message(&msg, crate::session::MessageStatus::Complete);
                chat_req = chat_req.append_message(msg);
            }

            let owned_calls = response.into_tool_calls();

            for call in owned_calls {
                let tool_name = &call.fn_name;
                let signature = format!("{tool_name}({})", call.fn_arguments);
                if last_action.as_deref() == Some(signature.as_str()) {
                    tracing::warn!(action = %security::redact(&signature), "repeated action stopped the loop");
                    return Ok(format!("stopped: {signature} repeated twice"));
                }
                last_action = Some(signature);

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

                let args_summary = security::redact(&args_summary);
                event_sink(&format!("Tool: {tool_name}({args_summary})"));
                Self::log_tool_call(tool_name, &args_summary, &call.call_id);
                steps.push(format!("{tool_name}({args_summary})"));

                let (result_str, is_err) = match self.registry.get_for_mode(tool_name, mode) {
                    Ok(tool) => {
                        let args = call.fn_arguments.clone();
                        let kind = tool.classify(&args, &self.security);
                        let detail = tool
                            .destination(&args, &self.security)
                            .map(|path| path.display().to_string())
                            .unwrap_or_else(|| args_summary.clone());
                        match self.gate(confirm, kind, tool_name, &detail).await {
                            Ok(()) => match tool.execute(args, &self.security).await {
                                Ok(res) => (res, false),
                                Err(err) => (format!("Error executing {tool_name}: {err}"), true),
                            },
                            Err(denied) => (denied, true),
                        }
                    }
                    Err(err) => (format!("Error executing {tool_name}: {err}"), true),
                };

                let result_str = security::redact(&result_str);
                Self::log_tool_result(tool_name, is_err, &result_str);

                if is_err {
                    if last_failure.as_deref() == Some(result_str.as_str()) {
                        return Ok(format!(
                            "stopped: the same error repeated twice: {result_str}"
                        ));
                    }
                    last_failure = Some(result_str.clone());
                } else {
                    last_failure = None;
                }

                // Emit preview snippet of tool output (first 3 lines or 150 chars)
                let preview = result_str.lines().take(3).collect::<Vec<_>>().join("\n");
                let truncated_preview = if result_str.lines().count() > 3 {
                    format!("{preview}\n  ... (truncated)")
                } else {
                    preview
                };

                event_sink(&format!("Output:\n{truncated_preview}"));

                let tool_message =
                    ChatMessage::from(ToolResponse::new(call.call_id.clone(), result_str));
                on_message(&tool_message, crate::session::MessageStatus::Complete);
                chat_req = chat_req.append_message(tool_message);
            }
        }

        // Final turn: force the model to provide a response without calling tools
        tracing::warn!("Turn budget exhausted, requesting final summary response");
        let final_prompt = "You have reached the maximum allowed tool iterations. Please summarize what you have accomplished, what is left, and provide your final response to the user now without calling any more tools.";
        let forced_req = chat_req
            .with_tools(Vec::<genai::chat::Tool>::new())
            .append_message(ChatMessage::user(final_prompt));

        let response = self
            .backend
            .send(&self.config.model, forced_req, Some(&chat_options))
            .await?;

        let final_text = response.into_first_text().unwrap_or_default();
        if !final_text.trim().is_empty() {
            on_message(
                &ChatMessage::assistant(&final_text),
                crate::session::MessageStatus::Complete,
            );
            Ok(final_text)
        } else {
            Ok(
                "Turn budget reached. Work partially completed without a final response."
                    .to_string(),
            )
        }
    }

    /// Assemble the request for one turn and compact it before it is sent.
    /// `mode` belongs here because the estimate must cover the exported tool
    /// schemas: the budget is spent on the request that actually goes out.
    pub(crate) async fn prepare_request(
        &self,
        history: Vec<ChatMessage>,
        task: &str,
        mode: AgentMode,
        summarizer: &dyn crate::tui::compact::Summarizer,
        event_sink: &mut impl FnMut(&str),
    ) -> ChatRequest {
        use crate::agent::budget::{
            compact, estimate_tokens, keep_recent_budget, request_budget, window_start,
        };

        let limit = model_context_limit(&self.config.model);
        let budget = request_budget(limit.tokens);
        let request = self.chat_request(None, &history, task, mode);
        if estimate_tokens(&request) <= budget {
            return request;
        }

        // One compaction, then at most three 10 % shrinks while the summary
        // itself keeps the request over the budget.
        let mut keep = keep_recent_budget(limit.tokens);
        for _ in 0..=3 {
            match compact(&history, keep, summarizer).await {
                Ok(compaction) => {
                    let candidate = self.chat_request(
                        Some(&compaction.summary),
                        &compaction.history,
                        task,
                        mode,
                    );
                    let after = estimate_tokens(&candidate);
                    if after <= budget {
                        event_sink(&format!(
                            "System: context compacted ({} → {after} tokens est.)",
                            compaction.before
                        ));
                        return candidate;
                    }
                }
                Err(error) => {
                    // A bad summary is worse than none: drop the older turns.
                    let split = window_start(&history, keep);
                    if split > 0 {
                        event_sink(&format!(
                            "System: compaction failed ({error}); older turns dropped"
                        ));
                        return self.chat_request(None, &history[split..], task, mode);
                    }
                    break;
                }
            }
            keep = keep.saturating_sub(keep / 10);
        }

        // Last resort: the newest turn alone, never truncated.
        let split = window_start(&history, 0);
        let request = self.chat_request(None, &history[split..], task, mode);
        let estimate = estimate_tokens(&request);
        if estimate > budget {
            event_sink(&format!(
                "System: {estimate} tokens exceeds the {budget} budget for {}; sending the newest turn anyway",
                self.config.model
            ));
        } else {
            event_sink("System: compaction did not fit the budget; older turns dropped");
        }
        request
    }

    fn chat_request(
        &self,
        summary: Option<&str>,
        history: &[ChatMessage],
        task: &str,
        mode: AgentMode,
    ) -> ChatRequest {
        let system_prompt = self
            .config
            .system_prompt
            .clone()
            .unwrap_or_else(|| load_system_prompt(self.security.root()));
        let system_prompt = with_mode_policy(system_prompt, mode);

        let mut request = ChatRequest::default()
            .with_tools(self.registry.to_genai_tools(mode))
            .append_message(ChatMessage::system(&system_prompt));
        if let Some(summary) = summary {
            request = request.append_message(ChatMessage::user(format!(
                "{}\n{summary}",
                crate::tui::compact::SESSION_CONTEXT_BANNER
            )));
        }
        for message in history {
            request = request.append_message(message.clone());
        }
        request.append_message(ChatMessage::user(task))
    }

    /// The capacity-error retry: compact against 40 % of the budget, keep the
    /// newest turn when even that cannot summarise.
    async fn aggressive_request(
        &self,
        history: &[ChatMessage],
        task: &str,
        mode: AgentMode,
        summarizer: &dyn crate::tui::compact::Summarizer,
    ) -> ChatRequest {
        use crate::agent::budget::{compact, request_budget, window_start};

        let limit = model_context_limit(&self.config.model);
        let aggressive = request_budget(limit.tokens) * 40 / 100;
        match compact(history, aggressive, summarizer).await {
            Ok(compaction) => {
                self.chat_request(Some(&compaction.summary), &compaction.history, task, mode)
            }
            Err(_) => {
                let split = window_start(history, 0);
                self.chat_request(None, &history[split..], task, mode)
            }
        }
    }

    /// One place decides a tool call: the matrix, then the human, then the
    /// answer's persistence. A denial is a tool result, never a turn failure.
    pub(crate) async fn gate(
        &self,
        confirm: &dyn Confirmer,
        kind: ActionKind,
        tool: &str,
        detail: &str,
    ) -> Result<(), String> {
        let denied = || format!("Permission denied: {}", kind.effect(detail));
        match permissions::decide(self.permission_mode(), &self.permitted_kinds(), kind) {
            Verdict::Allow => return Ok(()),
            Verdict::Deny => return Err(denied()),
            Verdict::Ask => {}
        }

        let answer = confirm
            .confirm(PermissionRequest::new(kind, tool, detail))
            .await;
        let path = crate::cli::config::workspace_config_path(self.security.root());
        match answer {
            Answer::Once => Ok(()),
            Answer::Category => match crate::cli::config::append_permitted(&path, kind) {
                Ok(kinds) => {
                    tracing::info!(action = %kind.name(), ?kinds, "permitted action recorded");
                    Ok(())
                }
                // The user answered; a config that cannot record it must not
                // turn an approval into a denial.
                Err(error) => {
                    tracing::warn!(%error, action = %kind.name(), "could not record permitted action");
                    Ok(())
                }
            },
            Answer::Unrestricted => {
                if let Err(error) = crate::cli::config::set_value(
                    &path,
                    crate::cli::config::ConfigKey::ToolPermissions,
                    "allow",
                ) {
                    tracing::warn!(%error, "could not persist tool_permissions");
                }
                Ok(())
            }
            Answer::Deny => Err(denied()),
        }
    }

    /// An invalid mode asks rather than allowing: a broken config must not widen
    /// permissions.
    fn permission_mode(&self) -> PermissionMode {
        permissions::parse_mode(&self.config.tool_permissions).unwrap_or_else(|error| {
            tracing::warn!(%error, "tool_permissions is invalid; asking");
            PermissionMode::Ask
        })
    }

    fn permitted_kinds(&self) -> Vec<ActionKind> {
        permissions::parse_permitted(&self.config.permitted_actions).unwrap_or_default()
    }

    /// The only place tool activity is logged, so redaction cannot be forgotten
    /// by a new call site: the argument is masked here, not by the caller.
    pub(crate) fn log_tool_call(tool: &str, args: &str, id: &str) {
        tracing::info!(tool = %tool, args = %security::redact(args), id = %id, "Executing agent tool");
    }

    pub(crate) fn log_tool_result(tool: &str, is_error: bool, output: &str) {
        tracing::info!(tool = %tool, is_error = is_error, output = %security::redact(output), "Agent tool completed");
    }

    pub(crate) fn log_request_payload(
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
                summary.push_str(&format!(
                    "  msg[{}][{role}]: {}\n",
                    idx,
                    security::redact(&content_summary)
                ));
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

#[cfg(test)]
#[path = "../tests/core/streaming.rs"]
mod streaming_tests;
