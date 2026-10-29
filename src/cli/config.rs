//! Effective configuration reporting and atomic config writes.
//!
//! Shared by `seavx`, the TUI `/config` command and the model picker so every
//! path validates and persists values the same way.

use std::io::Write;
use std::path::{Path, PathBuf};

use crate::agent::engine::{
    AgentConfig, ConfigOrigin, ConfigReport, DEFAULT_BASE_URL, normalize_base_url,
    parse_reasoning_effort,
};

/// Keys `/config set` and `/config unset` accept.
pub const WRITABLE_KEYS: [&str; 5] = [
    "model",
    "base_url",
    "reasoning_effort",
    "max_turns",
    "system_prompt",
];

/// Message for every attempt to write the API key through `/config`.
pub const API_KEY_HINT: &str = "API keys are managed by `seavx setup providers`, not `/config`.";

const DEFAULT_MODEL: &str = "openai/gpt-4o-mini";
const DEFAULT_MAX_TURNS: i64 = 1000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigKey {
    Model,
    BaseUrl,
    ApiKey,
    ReasoningEffort,
    MaxTurns,
    SystemPrompt,
}

impl ConfigKey {
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "model" => Some(Self::Model),
            "base_url" => Some(Self::BaseUrl),
            "api_key" => Some(Self::ApiKey),
            "effort" | "reasoning_effort" => Some(Self::ReasoningEffort),
            "max_turns" => Some(Self::MaxTurns),
            "system_prompt" => Some(Self::SystemPrompt),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Model => "model",
            Self::BaseUrl => "base_url",
            Self::ApiKey => "api_key",
            Self::ReasoningEffort => "reasoning_effort",
            Self::MaxTurns => "max_turns",
            Self::SystemPrompt => "system_prompt",
        }
    }

    pub fn is_writable(self) -> bool {
        self != Self::ApiKey
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigAction {
    Show,
    Path,
    Get {
        key: String,
    },
    Set {
        key: String,
        value: String,
        global: bool,
    },
    Unset {
        key: String,
        global: bool,
    },
}

pub fn workspace_config_path(workspace_root: &Path) -> PathBuf {
    workspace_root.join(".seavx/config.toml")
}

pub fn global_config_path() -> Option<PathBuf> {
    crate::home::dir().map(|home| home.join(".seavx/config.toml"))
}

/// `set (...abcd)` / `set (****)` / `unset`, never the full key.
pub fn mask_key(value: &str) -> String {
    if value.is_empty() {
        return "unset".to_string();
    }
    let characters: Vec<char> = value.chars().collect();
    // Reveal the suffix only when the key is long enough that four characters
    // are a small fraction of it.
    if characters.len() <= 8 {
        return "set (****)".to_string();
    }
    let tail: String = characters[characters.len() - 4..].iter().collect();
    format!("set (...{tail})")
}

/// Strip query strings and userinfo from a URL before printing it.
pub fn redact_url(raw: &str) -> String {
    if let Ok(mut url) = reqwest::Url::parse(raw) {
        let _ = url.set_password(None);
        url.set_query(None);
        return url.to_string();
    }

    let without_fragment = raw.split('#').next().unwrap_or(raw);
    let without_query = without_fragment
        .split('?')
        .next()
        .unwrap_or(without_fragment);
    match (without_query.find("://"), without_query.rfind('@')) {
        (Some(scheme), Some(at)) if at > scheme + 3 => {
            format!(
                "{}://{}",
                &without_query[..scheme],
                &without_query[at + 1..]
            )
        }
        _ => without_query.to_string(),
    }
}

/// Render the effective configuration with the source of every value.
pub fn render_report(config: &AgentConfig, report: &ConfigReport) -> String {
    let mut text = String::from("Seavx configuration\n");

    if let Some(error) = &report.parse_error {
        text.push_str(&format!("\nconfig error      {error}\n"));
    }

    let effort = config
        .reasoning_effort
        .clone()
        .unwrap_or_else(|| "default".to_string());
    let base_url = redact_url(config.base_url.as_deref().unwrap_or(DEFAULT_BASE_URL));
    let api_key = mask_key(config.api_key.as_deref().unwrap_or(""));
    let system_prompt = config
        .system_prompt
        .clone()
        .unwrap_or_else(|| "default".to_string());

    let rows = [
        ("model", config.model.clone(), ConfigKey::Model),
        ("reasoning_effort", effort, ConfigKey::ReasoningEffort),
        ("base_url", base_url, ConfigKey::BaseUrl),
        ("api_key", api_key, ConfigKey::ApiKey),
        (
            "max_turns",
            config.max_turns.to_string(),
            ConfigKey::MaxTurns,
        ),
        ("system_prompt", system_prompt, ConfigKey::SystemPrompt),
    ];

    text.push('\n');
    for (key, value, config_key) in rows {
        text.push_str(&format!(
            "{key:<17} {value:<44} ({})\n",
            origin_label(report.origin(config_key.name()))
        ));
    }

    text.push('\n');
    text.push_str(&render_paths(report));
    text
}

/// Paths view shared by bare `/config` and `/config path`.
pub fn render_paths(report: &ConfigReport) -> String {
    let workspace = display_path(&report.workspace_path);
    let workspace_state = if report.active_origin == ConfigOrigin::Workspace {
        "(active)"
    } else if report.workspace_path.exists() {
        "(present)"
    } else {
        "(not found)"
    };

    let mut text = format!("workspace config  {workspace} {workspace_state}");
    if let Some(global) = &report.global_path {
        let state = if report.active_origin == ConfigOrigin::Global {
            "(active)"
        } else if report.active_origin == ConfigOrigin::Workspace {
            "(shadowed)"
        } else if global.exists() {
            "(present)"
        } else {
            "(not found)"
        };
        text.push_str(&format!(
            "\nglobal config     {} {state}",
            display_path(global)
        ));
    }

    if report.active_origin == ConfigOrigin::Workspace
        && report
            .global_path
            .as_ref()
            .is_some_and(|path| path.exists())
    {
        text.push_str("\nWorkspace config shadows the global config; global values are ignored.");
    }

    text
}

/// One effective value plus its source, for `/config get`.
pub fn render_value(config: &AgentConfig, report: &ConfigReport, key: ConfigKey) -> String {
    let value = match key {
        ConfigKey::Model => config.model.clone(),
        ConfigKey::BaseUrl => redact_url(config.base_url.as_deref().unwrap_or(DEFAULT_BASE_URL)),
        ConfigKey::ApiKey => mask_key(config.api_key.as_deref().unwrap_or("")),
        ConfigKey::ReasoningEffort => config
            .reasoning_effort
            .clone()
            .unwrap_or_else(|| "default".to_string()),
        ConfigKey::MaxTurns => config.max_turns.to_string(),
        ConfigKey::SystemPrompt => config
            .system_prompt
            .clone()
            .unwrap_or_else(|| "default".to_string()),
    };

    format!(
        "{} = {} ({})",
        key.name(),
        value,
        origin_label(report.origin(key.name()))
    )
}

fn origin_label(origin: ConfigOrigin) -> String {
    match origin {
        ConfigOrigin::Default => "default".to_string(),
        ConfigOrigin::Workspace => "workspace".to_string(),
        ConfigOrigin::Global => "global".to_string(),
        ConfigOrigin::Env(name) => format!("env {name}"),
    }
}

fn display_path(path: &Path) -> String {
    let text = path.display().to_string();
    text.strip_prefix("./").unwrap_or(&text).to_string()
}

/// Validate and canonicalise a value before it can touch the file.
fn canonical_value(key: ConfigKey, raw: &str) -> Result<String, String> {
    match key {
        ConfigKey::ApiKey => Err(API_KEY_HINT.to_string()),
        ConfigKey::Model => {
            let value = raw.trim();
            if value.is_empty() {
                Err("model must not be empty".to_string())
            } else {
                Ok(value.to_string())
            }
        }
        ConfigKey::SystemPrompt => {
            let value = raw.trim();
            if value.is_empty() {
                Err("system_prompt must not be empty".to_string())
            } else {
                Ok(value.to_string())
            }
        }
        ConfigKey::BaseUrl => {
            let (normalized, _) = normalize_base_url(raw);
            match reqwest::Url::parse(&normalized) {
                Ok(url) if url.host().is_some() => Ok(normalized),
                _ => Err(format!("invalid base_url: {raw}")),
            }
        }
        ConfigKey::ReasoningEffort => {
            let effort = parse_reasoning_effort(raw)
                .ok_or_else(|| format!("invalid reasoning_effort: {raw}"))?;
            canonical_effort(effort)
                .map(str::to_string)
                .ok_or_else(|| format!("invalid reasoning_effort: {raw}"))
        }
        ConfigKey::MaxTurns => {
            let turns: u64 = raw
                .trim()
                .parse()
                .map_err(|_| format!("max_turns must be an integer, got: {raw}"))?;
            if (1..=10_000).contains(&turns) {
                Ok(turns.to_string())
            } else {
                Err("max_turns must be between 1 and 10000".to_string())
            }
        }
    }
}

fn canonical_effort(effort: genai::chat::ReasoningEffort) -> Option<&'static str> {
    use genai::chat::ReasoningEffort;
    match effort {
        ReasoningEffort::Zero => Some("none"),
        ReasoningEffort::Low | ReasoningEffort::Minimal => Some("minimal"),
        ReasoningEffort::Medium => Some("medium"),
        ReasoningEffort::High => Some("high"),
        ReasoningEffort::XHigh => Some("highx"),
        ReasoningEffort::Max => Some("max"),
        // A token budget has no canonical string form and is never produced by
        // `parse_reasoning_effort`.
        ReasoningEffort::Budget(_) => None,
    }
}

/// Write one key, preserving comments, ordering and unknown keys.
pub fn set_value(path: &Path, key: ConfigKey, raw: &str) -> Result<String, String> {
    let value = canonical_value(key, raw)?;
    let mut document = load_document(path)?;

    if key == ConfigKey::MaxTurns {
        let turns: i64 = value
            .parse()
            .map_err(|_| "max_turns must be an integer".to_string())?;
        document[key.name()] = toml_edit::value(turns);
    } else {
        document[key.name()] = toml_edit::value(value.clone());
    }

    write_atomic(path, &document.to_string())?;
    Ok(value)
}

/// Remove one key. Returns `false` when it was not present.
pub fn unset_value(path: &Path, key: ConfigKey) -> Result<bool, String> {
    if !key.is_writable() {
        return Err(API_KEY_HINT.to_string());
    }
    if !path.exists() {
        return Ok(false);
    }

    let mut document = load_document(path)?;
    if document.remove(key.name()).is_none() {
        return Ok(false);
    }

    write_atomic(path, &document.to_string())?;
    Ok(true)
}

fn load_document(path: &Path) -> Result<toml_edit::DocumentMut, String> {
    if !path.exists() {
        // `model` and `max_turns` are required by `AgentConfig`, and a file
        // replaces the built-in defaults wholesale. Seed them so one targeted
        // set still produces a loadable file.
        let mut document = toml_edit::DocumentMut::new();
        document["model"] = toml_edit::value(DEFAULT_MODEL);
        document["max_turns"] = toml_edit::value(DEFAULT_MAX_TURNS);
        return Ok(document);
    }
    let text = std::fs::read_to_string(path)
        .map_err(|error| format!("failed to read {}: {error}", path.display()))?;
    text.parse::<toml_edit::DocumentMut>().map_err(|error| {
        format!(
            "failed to parse {}: {}",
            path.display(),
            first_line(&error.to_string())
        )
    })
}

/// Parser errors echo the offending source lines; only the first line is safe
/// to show because the source may be an `api_key`.
fn first_line(text: &str) -> String {
    text.lines().next().unwrap_or_default().trim().to_string()
}

/// Replace the file atomically; the temporary file is owner-only on Unix.
fn write_atomic(path: &Path, contents: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("failed to create {}: {error}", parent.display()))?;
    }

    let file_name = path
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| "config.toml".to_string());
    let temporary = path.with_file_name(format!("{file_name}.tmp-{}", std::process::id()));

    // A crashed previous run may have left the same pid-named file behind.
    let _ = std::fs::remove_file(&temporary);

    let result = (|| -> Result<(), String> {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .map_err(|error| format!("failed to create {}: {error}", temporary.display()))?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(std::fs::Permissions::from_mode(0o600))
                .map_err(|error| format!("failed to secure {}: {error}", temporary.display()))?;
        }

        file.write_all(contents.as_bytes())
            .map_err(|error| format!("failed to write {}: {error}", temporary.display()))?;
        file.sync_all()
            .map_err(|error| format!("failed to flush {}: {error}", temporary.display()))?;
        Ok(())
    })();

    if let Err(error) = result {
        let _ = std::fs::remove_file(&temporary);
        return Err(error);
    }

    match std::fs::rename(&temporary, path) {
        Ok(()) => Ok(()),
        Err(error) => {
            // Windows cannot rename over an existing file; replace it once.
            #[cfg(windows)]
            {
                if path.exists() {
                    let _ = std::fs::remove_file(path);
                    if std::fs::rename(&temporary, path).is_ok() {
                        return Ok(());
                    }
                }
            }
            let _ = std::fs::remove_file(&temporary);
            Err(format!("failed to replace {}: {error}", path.display()))
        }
    }
}

/// Create `.seavx/config.toml` when missing. Returns `(path, created)`.
pub fn init_workspace(workspace_root: &Path) -> Result<(PathBuf, bool), String> {
    let path = workspace_config_path(workspace_root);
    if path.exists() {
        return Ok((path, false));
    }

    let default_toml = format!(
        "model = \"{DEFAULT_MODEL}\"\nmax_turns = {DEFAULT_MAX_TURNS}\nbase_url = \"{DEFAULT_BASE_URL}\"\napi_key = \"\"\n"
    );
    write_atomic(&path, &default_toml)?;
    Ok((path, true))
}

#[cfg(test)]
#[path = "../tests/cli/config.rs"]
mod tests;
