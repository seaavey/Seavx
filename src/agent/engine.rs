use crate::agent::backend::{BackendError, ChatBackend, GenaiBackend, StreamDelta};
use crate::agent::cancel::{CancelToken, Cleanup, TurnState, TurnStatus};
use crate::agent::events::{
    AgentEvent, FailureKind, SUMMARY_LIMIT, ToolEvent, ToolStatus, bounded,
};
use crate::agent::permissions::{self, ActionKind, Confirmer, PermissionMode, Verdict};
use crate::agent::prompt::{ASK_POLICY, PLAN_POLICY, REVIEW_POLICY, load_system_prompt};
use crate::agent::security::{self, WorkspaceSecurity};
use crate::agent::tools::ToolRegistry;
use crate::session::MessageStatus;
use genai::Client;
use genai::adapter::AdapterKind;
use genai::chat::{
    ChatMessage, ChatOptions, ChatRequest, ContentPart, MessageContent, ReasoningEffort, ToolCall,
    ToolResponse,
};
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
    /// One-shot codebase Q&A. Ephemeral: never persisted in the config and never
    /// the user's standing selection.
    Ask,
    /// One-shot code review of a resolved Git target. Ephemeral too.
    Review,
}

impl AgentMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::Build => "build",
            Self::Plan => "plan",
            Self::Ask => "ask",
            Self::Review => "review",
        }
    }
}

/// A completed task is only a skill candidate in Build Mode.
pub(crate) fn should_auto_distill(mode: AgentMode, step_count: usize) -> bool {
    mode == AgentMode::Build && step_count >= 2
}

/// Append the read-only planning policy after the base prompt has been resolved.
pub(crate) fn with_mode_policy(mut prompt: String, mode: AgentMode) -> String {
    let policy = match mode {
        AgentMode::Build => None,
        AgentMode::Plan => Some(PLAN_POLICY),
        AgentMode::Ask => Some(ASK_POLICY),
        AgentMode::Review => Some(REVIEW_POLICY),
    };
    if let Some(policy) = policy {
        prompt.push_str("\n\n");
        prompt.push_str(policy);
    }
    prompt
}

/// Canonical reasoning-effort options, in menu order. Single source for the
/// TUI modal, the CLI prompts and `parse_reasoning_effort`.
pub const EFFORT_LEVELS: &[&str] = &["none", "minimal", "medium", "high", "highx", "max"];

/// Turn budget a new config starts with. An existing file keeps its own value.
pub const DEFAULT_MAX_TURNS: usize = 100;

/// Model a new config starts with. An existing file keeps its own value.
pub const DEFAULT_MODEL: &str = "openai/gpt-4o-mini";

fn default_model() -> String {
    DEFAULT_MODEL.to_string()
}

fn default_max_turns() -> usize {
    DEFAULT_MAX_TURNS
}

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
    // Every field has a default so a hand-written file that sets only one key
    // is merged with the built-in values instead of failing to parse.
    #[serde(default = "default_model")]
    pub model: String,
    #[serde(default = "default_max_turns")]
    pub max_turns: usize,
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub reasoning_effort: Option<String>,
    #[serde(default)]
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
        let model = env("SEAVX_MODEL").unwrap_or_else(default_model);
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
                        // A key the file omits takes the built-in default: a
                        // partial file must not be discarded, and it must not
                        // silently inherit the fallback API-key variables
                        // either, which would make the file's origin a lie.
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

/// The kind the permission matrix must see. A tool that picks its own
/// destination is judged by where that destination lives: a skill or a memory
/// write lands under the global root, so it is a global write however the tool
/// classifies itself, and the matrix asks for it.
pub(crate) fn effective_kind(
    home: Option<&Path>,
    security: &WorkspaceSecurity,
    declared: ActionKind,
    destination: Option<&Path>,
) -> ActionKind {
    destination
        .and_then(|path| permissions::classify_target_with_home(home, security, path).ok())
        .unwrap_or(declared)
}

/// The replay-rejection retry: keep the newest half of the turns, cut at a user
/// row, so a tool-call pair is never split.
pub(crate) fn halve_turns(request: &ChatRequest) -> ChatRequest {
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
    // The newest half, measured in turn boundaries. A single boundary keeps
    // everything: there is nothing to halve, and indexing past it panicked.
    let keep_from = starts[starts.len() / 2];

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

/// What one turn ended as. `result` is the answer, or the error that failed or
/// cancelled it.
pub struct TurnOutcome {
    pub status: TurnStatus,
    pub cleanup: Option<Cleanup>,
    /// The pid of a killed tree whose cleanup could not be confirmed.
    pub pid: Option<u32>,
    pub result: Result<String, BackendError>,
}

/// How a turn stops inside the loop: cancelled on request, or failed.
enum TurnError {
    Cancelled,
    Failed(BackendError),
}

impl From<BackendError> for TurnError {
    fn from(error: BackendError) -> Self {
        Self::Failed(error)
    }
}

/// Runs `future` until it finishes or the token is set; `None` means the caller
/// must abandon it and keep whatever already left its sink.
async fn until_cancelled<F: std::future::Future>(
    cancel: &CancelToken,
    future: F,
) -> Option<F::Output> {
    tokio::select! {
        biased;
        _ = cancel.cancelled() => None,
        value = future => Some(value),
    }
}

/// The one way a turn reports a cancel: the state machine moves to
/// `Cancelling`, the caller finishes at `Cancelled`.
fn cancelled(turn_state: &mut TurnState) -> TurnError {
    turn_state.transition(TurnStatus::Cancelling);
    TurnError::Cancelled
}

/// A copy of the message with every string the model produced masked, for the
/// transcript and the session. The request keeps the original: a tool result is
/// matched to its call by the arguments that were sent, so masking them there
/// would break the replay.
fn redacted_for_display(message: &ChatMessage) -> ChatMessage {
    let parts: Vec<ContentPart> = message
        .content
        .parts()
        .iter()
        .map(|part| match part {
            ContentPart::Text(text) => ContentPart::Text(security::redact(text)),
            ContentPart::ToolCall(call) => ContentPart::ToolCall(ToolCall {
                fn_arguments: redacted_value(&call.fn_arguments),
                ..call.clone()
            }),
            other => other.clone(),
        })
        .collect();
    ChatMessage {
        role: message.role.clone(),
        content: MessageContent::from_parts(parts),
        options: message.options.clone(),
    }
}

/// Mask every string inside a JSON value, keeping the shape so the transcript
/// still shows which argument was redacted.
fn redacted_value(value: &serde_json::Value) -> serde_json::Value {
    use serde_json::Value;
    match value {
        Value::String(text) => Value::String(security::redact(text)),
        Value::Array(items) => Value::Array(items.iter().map(redacted_value).collect()),
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(key, value)| (key.clone(), redacted_value(value)))
                .collect(),
        ),
        other => other.clone(),
    }
}

/// A loop that stops early still ends with one assistant row, so a resumed
/// session can see why the turn ended instead of a completed turn with no
/// answer. The reason is already redacted by the caller.
fn report_stopped(
    on_message: &mut impl FnMut(&ChatMessage, MessageStatus),
    text: String,
) -> String {
    on_message(&ChatMessage::assistant(&text), MessageStatus::Complete);
    text
}

/// A cancelled turn always leaves one assistant row: `cancelled` with no text,
/// `partial` with what arrived.
fn report_cancelled(on_message: &mut impl FnMut(&ChatMessage, MessageStatus), received: &str) {
    let status = if received.trim().is_empty() {
        MessageStatus::Cancelled
    } else {
        MessageStatus::Partial
    };
    on_message(&ChatMessage::assistant(received), status);
}

pub struct AgentRuntime {
    client: Client,
    backend: Box<dyn ChatBackend>,
    summarizer: Option<std::sync::Arc<dyn crate::tui::compact::Summarizer>>,
    registry: ToolRegistry,
    security: WorkspaceSecurity,
    config: AgentConfig,
    /// Who answers `ask_user`. Headless runs keep the default: nobody.
    questioner: std::sync::Arc<dyn crate::agent::questions::Questioner>,
    /// Where a running turn is steered from. Default: nowhere.
    steering: std::sync::Arc<dyn crate::agent::steering::Steering>,
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
            questioner: std::sync::Arc::new(crate::agent::questions::UnavailableQuestioner),
            steering: std::sync::Arc::new(crate::agent::steering::NoSteering),
        }
    }

    /// Swap who answers `ask_user`. The TUI sets a human; `exec` keeps the
    /// default, so a headless run never waits.
    /// Swap where steering comes from. Nothing is delivered mid-call: the loop
    /// reads it at a turn boundary, so a running tool is never interrupted.
    pub fn with_steering(
        mut self,
        steering: std::sync::Arc<dyn crate::agent::steering::Steering>,
    ) -> Self {
        self.steering = steering;
        self
    }

    pub fn with_questioner(
        mut self,
        questioner: std::sync::Arc<dyn crate::agent::questions::Questioner>,
    ) -> Self {
        self.questioner = questioner;
        self
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

    /// One user turn. The outcome carries the terminal status, what the OS said
    /// about a killed process tree, and the answer or the failure.
    // The argument list is the phase plan's interface, and the TUI and `exec`
    // both call it: folding it into a struct would add a shape with one caller.
    #[allow(clippy::too_many_arguments)]
    pub async fn execute_task(
        &self,
        task: &str,
        mode: AgentMode,
        history: Vec<ChatMessage>,
        confirm: &dyn Confirmer,
        cancel: &CancelToken,
        mut event_sink: impl FnMut(AgentEvent) + Send,
        on_message: impl FnMut(&ChatMessage, MessageStatus),
    ) -> TurnOutcome {
        let mut turn = TurnState::new();
        let result = match self
            .run_turn(
                task,
                mode,
                history,
                confirm,
                cancel,
                &mut turn,
                &mut event_sink,
                on_message,
            )
            .await
        {
            Ok(answer) => {
                turn.transition(TurnStatus::Completed);
                Ok(answer)
            }
            Err(TurnError::Cancelled) => {
                // `cancelled()` already moved the state to `Cancelling`; this is
                // the one edge left, and it is reported because the TUI reads it.
                turn.transition(TurnStatus::Cancelled);
                Err(std::io::Error::other("the turn was cancelled").into())
            }
            Err(TurnError::Failed(error)) => {
                turn.transition(TurnStatus::Failed);
                Err(error)
            }
        };

        let cleanup = turn.cleanup();
        let status = turn.status();
        // The provider is the only turn-level failure there is: a tool failure
        // is reported on its own call, not here.
        let failure = match status {
            TurnStatus::Failed => Some(FailureKind::Provider),
            TurnStatus::Cancelled if cleanup == Some(Cleanup::Unconfirmed) => {
                Some(FailureKind::CleanupUnconfirmed)
            }
            _ => None,
        };
        event_sink(AgentEvent::Turn {
            status,
            failure,
            cleanup,
            pid: turn.pid(),
        });

        TurnOutcome {
            status,
            cleanup,
            pid: turn.pid(),
            result,
        }
    }

    // Same seam, plus the state machine this layer owns.
    #[allow(clippy::too_many_arguments)]
    async fn run_turn(
        &self,
        task: &str,
        mode: AgentMode,
        history: Vec<ChatMessage>,
        confirm: &dyn Confirmer,
        cancel: &CancelToken,
        turn_state: &mut TurnState,
        event_sink: &mut (impl FnMut(AgentEvent) + Send),
        mut on_message: impl FnMut(&ChatMessage, MessageStatus),
    ) -> Result<String, TurnError> {
        if cancel.is_cancelled() {
            // The same reporting path every other cancel uses.
            return Err(cancelled(turn_state));
        }
        // What this one Ask turn actually read. Not persisted, not shared with
        // the next turn: an answer may only cite its own turn's reads.
        let mut evidence = Evidence::new();
        let summarizer: std::sync::Arc<dyn crate::tui::compact::Summarizer> = match &self.summarizer
        {
            Some(summarizer) => summarizer.clone(),
            None => std::sync::Arc::new(crate::tui::compact::ModelSummarizer::from_config(
                &self.config,
            )),
        };
        let retry_history = history.clone();
        let mut chat_req = self
            .prepare_request(history, task, mode, &*summarizer, event_sink)
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
            if cancel.is_cancelled() {
                return Err(cancelled(turn_state));
            }
            tracing::debug!(turn = turn, "Executing chat request");

            // A steer lands between turns: after the previous response (tool
            // results included), before this request. Never mid-call.
            if let Some(steer) = self.steering.take() {
                chat_req = chat_req.append_message(genai::chat::ChatMessage::user(steer.clone()));
                // The worker persists this as a user row: a resumed session that
                // held the answer but not the instruction is a broken transcript.
                event_sink(AgentEvent::Steer(steer));
            }

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
            let mut streamed_thinking = false;
            let mut received = String::new();
            let attempt = async {
                if self.config.stream {
                    let mut on_delta = |delta: StreamDelta| {
                        got_delta = true;
                        retry.note_output();
                        match delta {
                            StreamDelta::Token(token) => {
                                received.push_str(&token);
                                // Ask answers are shown once, after their
                                // citations are validated; a delta would show
                                // an unverified reference.
                                if !matches!(mode, AgentMode::Ask | AgentMode::Review) {
                                    event_sink(AgentEvent::Token(token));
                                }
                            }
                            StreamDelta::Thinking(thoughts) => {
                                streamed_thinking = true;
                                event_sink(AgentEvent::Thinking(thoughts));
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
                }
            };

            let first = match until_cancelled(cancel, attempt).await {
                Some(first) => first,
                None => {
                    // The future is dropped, so nothing more is read; the
                    // deltas that already left the sink are kept.
                    report_cancelled(&mut on_message, &received);
                    return Err(cancelled(turn_state));
                }
            };

            let response = match first {
                Ok(response) => response,
                Err(error) if got_delta => {
                    // Output already left the sink: persisted, never resent.
                    if !received.trim().is_empty() {
                        on_message(&ChatMessage::assistant(&received), MessageStatus::Partial);
                    }
                    return Err(TurnError::Failed(error));
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
                        return Err(TurnError::Failed(error));
                    }
                    match trigger {
                        Some(RetryTrigger::ReplayRejection) => chat_req = halve_turns(&chat_req),
                        Some(RetryTrigger::CapacityError) => {
                            chat_req = self
                                .aggressive_request(&retry_history, task, mode, &*summarizer)
                                .await
                        }
                        // The same request, unstreamed: nothing to rebuild.
                        Some(RetryTrigger::StreamUnavailable) | None => {}
                    }
                    // A failure here is the turn's failure: it never buys a
                    // second retry.
                    let retry_send = self.backend.send(
                        &self.config.model,
                        chat_req.clone(),
                        Some(&chat_options),
                    );
                    match until_cancelled(cancel, retry_send).await {
                        Some(result) => result?,
                        None => {
                            report_cancelled(&mut on_message, &received);
                            return Err(cancelled(turn_state));
                        }
                    }
                }
            };

            // Track real usage tokens from provider response
            if let Some(total) = response.usage.total_tokens {
                let prompt = response.usage.prompt_tokens.unwrap_or(0);
                event_sink(AgentEvent::Usage {
                    total_tokens: total,
                    prompt_tokens: prompt,
                });
            }

            // Surface the model's reasoning so the TUI can render it as a gray
            // block. The captured text repeats what the deltas already sent, so
            // a streamed turn is skipped: re-sending it drew the block twice.
            if !streamed_thinking && let Some(ref thoughts) = response.reasoning_content {
                let trimmed = thoughts.trim();
                if !trimmed.is_empty() {
                    event_sink(AgentEvent::Thinking(trimmed.to_string()));
                }
            }

            let tool_calls = response.tool_calls();
            if tool_calls.is_empty() {
                let final_text = response.clone().into_first_text().unwrap_or_default();
                let final_text = if mode == AgentMode::Review {
                    // A review answer is a report or an error; free prose is not
                    // a clean result, so it fails the turn instead.
                    let report = crate::agent::review::parse_report(&final_text)
                        .map_err(|error| TurnError::Failed(std::io::Error::other(error).into()))?;
                    let rendered = crate::agent::review::render_report(&report);
                    event_sink(AgentEvent::Token(rendered.clone()));
                    rendered
                } else if mode == AgentMode::Ask {
                    let validated = sanitize_citations(&final_text, &evidence, &self.security);
                    if !validated.trim().is_empty() {
                        event_sink(AgentEvent::Token(validated.clone()));
                    }
                    validated
                } else {
                    final_text
                };

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
                    self.auto_distill_skill(task, &steps, event_sink).await;
                }

                return Ok(return_text);
            }

            let assistant_msg = response.clone().into_assistant_message_for_tool_use();
            if let Some(msg) = assistant_msg {
                // The model writes the arguments, so they may carry a secret:
                // the transcript and the session get the masked copy.
                on_message(
                    &redacted_for_display(&msg),
                    crate::session::MessageStatus::Complete,
                );
                chat_req = chat_req.append_message(msg);
            }

            let owned_calls = response.into_tool_calls();

            for call in owned_calls {
                let tool_name = &call.fn_name;
                let signature = format!("{tool_name}({})", call.fn_arguments);
                if last_action.as_deref() == Some(signature.as_str()) {
                    // The reason reaches the transcript, so it is masked here
                    // rather than only in the log line.
                    let reason = security::redact(&signature);
                    tracing::warn!(action = %reason, "repeated action stopped the loop");
                    return Ok(report_stopped(
                        &mut on_message,
                        format!("stopped: {reason} repeated twice"),
                    ));
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
                let call_id = call.call_id.clone();
                let summary = bounded(&args_summary, SUMMARY_LIMIT);
                let started_at = crate::util::epoch_secs() as i64;
                let started = std::time::Instant::now();
                let mut lifecycle =
                    |status: ToolStatus,
                     failure: Option<FailureKind>,
                     elapsed: Option<std::time::Duration>| {
                        event_sink(AgentEvent::Tool(ToolEvent {
                            call_id: call_id.clone(),
                            name: tool_name.clone(),
                            summary: summary.clone(),
                            status,
                            failure,
                            started_at,
                            elapsed,
                        }));
                    };
                lifecycle(ToolStatus::Queued, None, None);
                Self::log_tool_call(tool_name, &args_summary, &call_id);
                steps.push(format!("{tool_name}({args_summary})"));

                // How the call ended. Kept apart from the result string because
                // a denial and a failed command must not be the same event.
                enum ToolEnd {
                    Done,
                    Failed(FailureKind),
                    Cancelled,
                }

                lifecycle(ToolStatus::Running, None, None);
                let (result_str, end) = if tool_name == "ask_user" {
                    // The engine asks the human itself: no later tool call runs
                    // until the answer arrives, and no provider request starts.
                    match crate::agent::questions::parse_request(&call_id, &call.fn_arguments) {
                        Ok(request) => {
                            let answer = self.questioner.ask(request).await;
                            (answer.tool_result(), ToolEnd::Done)
                        }
                        Err(error) => (
                            format!("Error executing ask_user: {error}"),
                            ToolEnd::Failed(FailureKind::Tool),
                        ),
                    }
                } else {
                    match self.registry.get_for_mode(tool_name, mode) {
                        Ok(tool) => {
                            let args = call.fn_arguments.clone();
                            let destination = tool.destination(&args, &self.security);
                            let kind = effective_kind(
                                crate::home::dir().as_deref(),
                                &self.security,
                                tool.classify(&args, &self.security),
                                destination.as_deref(),
                            );
                            let detail = destination
                                .map(|path| path.display().to_string())
                                .unwrap_or_else(|| args_summary.clone());
                            match self.gate(confirm, kind, tool_name, &detail).await {
                                Ok(()) => match tool.execute(args, &self.security, cancel).await {
                                    // A tool that returns after the cancel landed did
                                    // not complete: it was cut short, whatever the
                                    // command itself printed.
                                    Ok(res) => {
                                        let end = if cancel.is_cancelled() {
                                            ToolEnd::Cancelled
                                        } else {
                                            ToolEnd::Done
                                        };
                                        (res, end)
                                    }
                                    Err(err) => {
                                        let message = format!("Error executing {tool_name}: {err}");
                                        let end = if cancel.is_cancelled() {
                                            ToolEnd::Cancelled
                                        } else {
                                            ToolEnd::Failed(FailureKind::for_tool_error(&message))
                                        };
                                        (message, end)
                                    }
                                },
                                Err(denied) => {
                                    (denied, ToolEnd::Failed(FailureKind::PermissionDenied))
                                }
                            }
                        }
                        // The tool exists but this mode does not export it: a
                        // dispatch bug, not a tool that failed.
                        Err(err) => (
                            format!("Error executing {tool_name}: {err}"),
                            ToolEnd::Failed(FailureKind::Internal),
                        ),
                    }
                };
                let is_err = !matches!(end, ToolEnd::Done);

                let result_str = security::redact(&result_str);
                // A cancelled command reports what the OS said; the turn carries it.
                if let Some((pid, cleanup)) = cancel.last_cleanup() {
                    turn_state.set_cleanup(pid, cleanup);
                }

                Self::log_tool_result(tool_name, is_err, &result_str);

                if mode == AgentMode::Ask && !is_err {
                    record_evidence(
                        &mut evidence,
                        tool_name,
                        &call.fn_arguments,
                        &result_str,
                        &self.security,
                    );
                }

                if is_err {
                    if last_failure.as_deref() == Some(result_str.as_str()) {
                        return Ok(report_stopped(
                            &mut on_message,
                            format!("stopped: the same error repeated twice: {result_str}"),
                        ));
                    }
                    last_failure = Some(result_str.clone());
                } else {
                    last_failure = None;
                }

                match end {
                    ToolEnd::Done => {
                        lifecycle(ToolStatus::Completed, None, Some(started.elapsed()))
                    }
                    ToolEnd::Failed(failure) => {
                        lifecycle(ToolStatus::Failed, Some(failure), Some(started.elapsed()));
                    }
                    ToolEnd::Cancelled => {
                        lifecycle(ToolStatus::Cancelled, None, Some(started.elapsed()));
                    }
                }

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

        let forced = self
            .backend
            .send(&self.config.model, forced_req, Some(&chat_options));
        let response = match until_cancelled(cancel, forced).await {
            Some(result) => result?,
            None => {
                report_cancelled(&mut on_message, "");
                return Err(cancelled(turn_state));
            }
        };

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
        event_sink: &mut impl FnMut(AgentEvent),
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
                        event_sink(AgentEvent::Notice(format!(
                            "context compacted ({} → {after} tokens est.)",
                            compaction.before
                        )));
                        return candidate;
                    }
                }
                Err(error) => {
                    // A bad summary is worse than none: drop the older turns.
                    let split = window_start(&history, keep);
                    if split > 0 {
                        event_sink(AgentEvent::Notice(format!(
                            "compaction failed ({error}); older turns dropped"
                        )));
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
            event_sink(AgentEvent::Notice(format!(
                "{estimate} tokens exceeds the {budget} budget for {}; sending the newest turn anyway",
                self.config.model
            )));
        } else {
            event_sink(AgentEvent::Notice(
                "compaction did not fit the budget; older turns dropped".to_string(),
            ));
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

        let path = crate::cli::config::workspace_config_path(self.security.root());
        permissions::approve(confirm, kind, tool, detail, &path).await
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
        event_sink: &mut (impl FnMut(AgentEvent) + Send),
    ) {
        event_sink(AgentEvent::Notice(
            "evaluating workflow for automatic skill synthesis...".to_string(),
        ));
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
                event_sink(AgentEvent::Notice(format!(
                    "skill evaluator request failed: {err}"
                )));
                return;
            }
        };

        let text = res.into_first_text().unwrap_or_default();
        let trimmed = text.trim();
        if !(trimmed.starts_with("---") && trimmed.contains("name:")) {
            tracing::info!("Skill synthesis skipped: evaluator returned no skill");
            event_sink(AgentEvent::Notice(
                "no repeatable workflow found, skill not saved".to_string(),
            ));
            return;
        }

        let confidence = parse_confidence(trimmed);
        if !matches!(confidence, Some(c) if c >= SKILL_CONFIDENCE_THRESHOLD) {
            tracing::info!(
                ?confidence,
                "Skill synthesis skipped: confidence below threshold"
            );
            event_sink(AgentEvent::Notice(
                "workflow rated below singularity threshold, skill not saved".to_string(),
            ));
            return;
        }

        let name = crate::util::frontmatter_value(trimmed, "name")
            .unwrap_or_else(|| "auto-workflow".to_string());

        let mgr = crate::agent::skills::SkillManager::new(self.security.root());
        match mgr.save_skill(&name, trimmed) {
            Ok(saved_path) => {
                tracing::info!(skill = %name, "Automatically created high-confidence skill from workflow");
                event_sink(AgentEvent::Notice(format!(
                    "skill synthesized -> {}",
                    saved_path.display()
                )));
            }
            Err(err) => {
                tracing::warn!(error = %err, "Skill synthesis failed: cannot persist skill");
                event_sink(AgentEvent::Notice(format!("skill synthesis failed: {err}")));
            }
        }
    }
}

/// Lines one Ask turn actually observed: canonical path -> 1-based lines.
///
/// Only `read_file` content and `search_files` content hits feed it. A directory
/// listing names files without showing them, so it authorizes nothing.
type Evidence = std::collections::BTreeMap<std::path::PathBuf, std::collections::BTreeSet<usize>>;

/// Record what a successful read or content search returned, keyed by the
/// canonical path the security check produced.
fn record_evidence(
    evidence: &mut Evidence,
    tool: &str,
    args: &serde_json::Value,
    output: &str,
    security: &WorkspaceSecurity,
) {
    match tool {
        "read_file" => {
            let Some(requested) = args.get("path").and_then(|value| value.as_str()) else {
                return;
            };
            let Ok(path) = security.validate_path(requested) else {
                return;
            };
            let lines = evidence.entry(path).or_default();
            for line in output.lines() {
                // `N|text`; only the row number is evidence.
                if let Some((number, _)) = line.split_once('|')
                    && let Ok(number) = number.parse::<usize>()
                {
                    lines.insert(number);
                }
            }
        }
        "search_files" => {
            for line in output.lines() {
                let Some((requested, number)) = split_search_hit(line) else {
                    continue;
                };
                let Ok(path) = security.validate_path(requested) else {
                    continue;
                };
                evidence.entry(path).or_default().insert(number);
            }
        }
        _ => {}
    }
}

/// `path:line: text` as a content search prints it. The path may carry a colon
/// (a Windows drive), so the first `:<digits>: ` run splits it.
fn split_search_hit(line: &str) -> Option<(&str, usize)> {
    let bytes = line.as_bytes();
    for index in 0..bytes.len() {
        if bytes[index] != b':' {
            continue;
        }
        let start = index + 1;
        let mut end = start;
        while end < bytes.len() && bytes[end].is_ascii_digit() {
            end += 1;
        }
        if end > start && line[end..].starts_with(": ") {
            return Some((&line[..index], line[start..end].parse().ok()?));
        }
    }
    None
}

/// Mark every citation whose path or line this turn did not observe. The text is
/// left intact otherwise: no line number is ever rewritten or invented.
fn sanitize_citations(answer: &str, evidence: &Evidence, security: &WorkspaceSecurity) -> String {
    let bytes = answer.as_bytes();
    let mut out = String::with_capacity(answer.len());
    let mut copied = 0;
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b':' {
            index += 1;
            continue;
        }
        let low_start = index + 1;
        let mut low_end = low_start;
        while low_end < bytes.len() && bytes[low_end].is_ascii_digit() {
            low_end += 1;
        }
        if low_end == low_start {
            index += 1;
            continue;
        }
        let Ok(low) = answer[low_start..low_end].parse::<usize>() else {
            index += 1;
            continue;
        };
        // `path:3-9` cites a range; every line in it must have been read.
        let mut high = low;
        let mut after = low_end;
        if bytes.get(low_end) == Some(&b'-') {
            let mut range_end = low_end + 1;
            while range_end < bytes.len() && bytes[range_end].is_ascii_digit() {
                range_end += 1;
            }
            if let Ok(parsed) = answer[low_end + 1..range_end].parse::<usize>() {
                high = parsed;
                after = range_end;
            }
        }
        // The path token runs back to the nearest separator.
        let mut start = index;
        while start > 0 {
            let previous = bytes[start - 1];
            if previous.is_ascii_whitespace()
                || matches!(
                    previous,
                    b'`' | b'(' | b'[' | b'"' | b'\'' | b',' | b'*' | b'<' | b'|'
                )
            {
                break;
            }
            start -= 1;
        }
        if start == index {
            index = after;
            continue;
        }
        let path = &answer[start..index];
        // A clock time, a ratio or a URL port also reads as `x:123`. Only a
        // file-shaped token is a citation; everything else is left alone.
        if !path_like(path) {
            index = after;
            continue;
        }
        let verified = citation_is_verified(path, low, high, evidence, security);
        out.push_str(&answer[copied..after]);
        if !verified {
            out.push_str(" (unverified)");
        }
        copied = after;
        index = after;
    }
    out.push_str(&answer[copied..]);
    out
}

/// A citation token names a file: it carries a separator or an extension, and it
/// is not a URL. Without this, `10:30` and `http://host:8080` would be tagged.
fn path_like(token: &str) -> bool {
    !token.contains("://") && (token.contains('/') || token.contains('.'))
}

/// A citation is verified only when its path resolves inside the workspace, is
/// not protected, and every cited line came back from this turn's tools.
fn citation_is_verified(
    path: &str,
    low: usize,
    high: usize,
    evidence: &Evidence,
    security: &WorkspaceSecurity,
) -> bool {
    if low == 0 || high < low {
        return false;
    }
    let Ok(canonical) = security.validate_path(path) else {
        return false;
    };
    let Some(lines) = evidence.get(&canonical) else {
        return false;
    };
    (low..=high).all(|line| lines.contains(&line))
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
    fn review_mode_appends_the_review_policy() {
        let prompt = with_mode_policy("base prompt".to_string(), AgentMode::Review);
        assert!(prompt.starts_with("base prompt"));
        assert!(prompt.contains("# REVIEW MODE ACTIVE"), "{prompt}");
        for part in ["severity", "confirmed", "hypothesis", "remediation"] {
            assert!(prompt.contains(part), "{part} must be named: {prompt}");
        }
        assert!(
            prompt.to_lowercase().contains("never claim"),
            "a review must not claim a test it did not run: {prompt}"
        );
        assert!(!prompt.contains("# PLAN MODE ACTIVE"), "{prompt}");
    }

    #[test]
    fn ask_mode_appends_the_ask_policy() {
        let prompt = with_mode_policy("base prompt".to_string(), AgentMode::Ask);
        assert!(prompt.starts_with("base prompt"));
        assert!(prompt.contains("# ASK MODE ACTIVE"), "{prompt}");
        for part in ["Verified", "Inference", "Unknown"] {
            assert!(prompt.contains(part), "{part} must be named: {prompt}");
        }
        assert!(
            prompt.contains("path:line"),
            "citations carry a path and a line: {prompt}"
        );
        assert!(
            !prompt.contains("# PLAN MODE ACTIVE"),
            "Ask is not Plan: {prompt}"
        );
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
